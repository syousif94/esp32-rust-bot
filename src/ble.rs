//! BLE GATT server module for motor and servo control.
//!
//! Exposes a custom GATT service that a Swift app (CoreBluetooth) can connect to.
//! Provides read/write characteristics for servo angle and motor power levels,
//! using the same signals as the HTTP server and serial command modules.
//! Also provides WiFi configuration characteristic for setting SSID/password via BLE.

use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use esp_println::println;
use esp_radio::ble::controller::BleConnector;
use static_cell::StaticCell;
use trouble_host::prelude::*;
use crate::wifi_config::{MAX_SSID_LEN, MAX_PASSWORD_LEN};
use crate::commands::{Command, send_command, MOTOR_COUNT, BATTERY_MV, BATTERY_PCT};

/// Max number of BLE connections (1 is typical for a peripheral)
const CONNECTIONS_MAX: usize = 1;

/// Max number of L2CAP channels (Signal + ATT)
const L2CAP_CHANNELS_MAX: usize = 2;

/// WiFi credentials received via BLE (SSID, password as heapless strings)
pub type WifiCredentialsData = (heapless::String<MAX_SSID_LEN>, heapless::String<MAX_PASSWORD_LEN>);

/// Signal to pass WiFi credentials to main task for flash storage
pub static BLE_WIFI_CREDENTIALS: Signal<CriticalSectionRawMutex, WifiCredentialsData> = Signal::new();

// Custom 128-bit UUIDs for our service and characteristics.
// These are random UUIDs — the Swift app will scan for the service UUID.
//
// Service:     e3910010-4567-4321-abcd-abcdef012345 (updated for GATT cache refresh)
// Servo:       e3910002-4567-4321-abcd-abcdef012345
// Motors (4B): e3910003-4567-4321-abcd-abcdef012345
// WiFi Config: e3910004-4567-4321-abcd-abcdef012345 (write SSID + password)
// Motor Count: e3910005-4567-4321-abcd-abcdef012345 (read-only, 1 byte: 2 or 4)

/// GATT Server definition with our motor control service
#[gatt_server]
struct Server {
    motor_control: MotorControlService,
}

/// Custom motor control GATT service
/// Note: Service UUID incremented to e3910010 to force GATT cache refresh on clients
#[gatt_service(uuid = "e3910010-4567-4321-abcd-abcdef012345")]
struct MotorControlService {
    /// Servo angle (0-180), read + write + notify
    #[characteristic(uuid = "e3910002-4567-4321-abcd-abcdef012345", read, write, write_without_response, notify, value = 90)]
    servo_angle: u8,

    /// All motors (4 bytes: A, B, C, D each -100..100), read + write + write_without_response + notify
    /// For two_motor builds only bytes 0-1 are used.
    #[characteristic(uuid = "e3910003-4567-4321-abcd-abcdef012345", read, write, write_without_response, notify, value = [0, 0, 0, 0])]
    motors: [u8; 4],

    /// WiFi config (write-only): format is "SSID\0PASSWORD" (null-separated)
    /// Max 97 bytes: 32 (SSID) + 1 (null) + 64 (password)
    #[characteristic(uuid = "e3910004-4567-4321-abcd-abcdef012345", write, value = [0u8; 97])]
    wifi_config: [u8; 97],

    /// Motor count (read-only): returns the number of motors in this firmware build (2 or 4)
    #[characteristic(uuid = "e3910005-4567-4321-abcd-abcdef012345", read, value = 0)]
    motor_count: u8,

    /// Battery level: 3 bytes [percentage, voltage_mv_hi, voltage_mv_lo], read + notify
    #[characteristic(uuid = "e3910006-4567-4321-abcd-abcdef012345", read, notify, value = [0, 0, 0])]
    battery: [u8; 3],
}

/// Run the BLE host stack background task
async fn ble_runner<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    let _ = runner.run().await;
}

/// Handle GATT events (reads/writes) for a single connection
async fn gatt_events_task<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
) -> Result<(), Error> {
    let reason = loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => break reason,
            GattConnectionEvent::Gatt { event } => {
                match &event {
                    GattEvent::Write(write_event) => {
                        let handle = write_event.handle();
                        let data = write_event.data();

                        println!("[BLE] Write event: handle={:?}, data={:?} ({} bytes)", handle, data, data.len());

                        // Check which characteristic was written and signal accordingly
                        if handle == server.motor_control.servo_angle.handle {
                            if let Some(&val) = data.first() {
                                if val <= 180 {
                                    println!("[BLE] Servo angle set to {}", val);
                                    send_command(Command::Servo(val));
                                } else {
                                    println!("[BLE] Servo angle {} out of range (0-180)", val);
                                }
                            }
                        } else if handle == server.motor_control.motors.handle {
                            if data.len() >= MOTOR_COUNT {
                                let mut powers = [0i8; MOTOR_COUNT];
                                for i in 0..MOTOR_COUNT {
                                    powers[i] = data[i] as i8;
                                }
                                #[cfg(feature = "four_motor")]
                                println!("[BLE] Motors: A={}% B={}% C={}% D={}%", powers[0], powers[1], powers[2], powers[3]);
                                #[cfg(feature = "two_motor")]
                                println!("[BLE] Motors: A={}% B={}%", powers[0], powers[1]);
                                send_command(Command::MotorsAll(powers));
                            } else {
                                println!("[BLE] motors: expected {} bytes, got {}", MOTOR_COUNT, data.len());
                            }
                        } else if handle == server.motor_control.wifi_config.handle {
                            // WiFi config format: "SSID\0PASSWORD" (null-separated)
                            if let Some(null_pos) = data.iter().position(|&b| b == 0) {
                                let ssid_bytes = &data[..null_pos];
                                let pass_bytes = &data[null_pos + 1..];
                                
                                if let (Ok(ssid_str), Ok(pass_str)) = (
                                    core::str::from_utf8(ssid_bytes),
                                    core::str::from_utf8(pass_bytes),
                                ) {
                                    if ssid_str.is_empty() || ssid_str.len() > MAX_SSID_LEN {
                                        println!("[BLE] WiFi config: invalid SSID length (1-{} bytes)", MAX_SSID_LEN);
                                    } else if pass_str.len() > MAX_PASSWORD_LEN {
                                        println!("[BLE] WiFi config: password too long (max {} bytes)", MAX_PASSWORD_LEN);
                                    } else {
                                        println!("[BLE] WiFi config: SSID='{}' (password {} chars)", ssid_str, pass_str.len());
                                        // Create heapless strings to pass to main task
                                        if let (Ok(ssid), Ok(password)) = (
                                            heapless::String::try_from(ssid_str),
                                            heapless::String::try_from(pass_str),
                                        ) {
                                            // Signal main task to save credentials
                                            BLE_WIFI_CREDENTIALS.signal((ssid, password));
                                            println!("[BLE] WiFi credentials queued for saving");
                                        }
                                    }
                                } else {
                                    println!("[BLE] WiFi config: invalid UTF-8 in SSID or password");
                                }
                            } else {
                                println!("[BLE] WiFi config: expected null-separated SSID and password");
                            }
                        } else if handle == server.motor_control.servo_angle.cccd_handle.unwrap()
                            || handle == server.motor_control.motors.cccd_handle.unwrap()
                        {
                            let enabled = data.len() >= 2 && data[0] == 1;
                            println!("[BLE] Notifications {} for handle {:?}", if enabled { "enabled" } else { "disabled" }, handle);
                        } else {
                            println!("[BLE] Unknown handle {:?}, data={:?}", handle, data);
                        }
                    }
                    GattEvent::Read(read_event) => {
                        // Update battery characteristic on every read so clients get fresh data
                        let mv = BATTERY_MV.load(core::sync::atomic::Ordering::Relaxed);
                        let pct = BATTERY_PCT.load(core::sync::atomic::Ordering::Relaxed);
                        let _ = server.motor_control.battery.set(server, &[pct, (mv >> 8) as u8, mv as u8]);
                        let _ = read_event;
                    }
                    other => {
                        println!("[BLE] Other GATT event: {:?}", core::any::type_name_of_val(other));
                    }
                }
                // Accept and send the reply
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => println!("[BLE] error sending GATT response: {:?}", e),
                }
            }
            other => {
                println!("[BLE] Connection event: {:?}", core::any::type_name_of_val(&other));
            }
        }
    };
    println!("[BLE] disconnected: {:?}", reason);
    // Show disconnection on line 1 of the OLED display
    let sender = crate::display::DISPLAY_STATE.sender();
    crate::display::set_line1_override(&sender, "BLE: Disconnected");
    Ok(())
}

/// Create an advertiser and wait for a central to connect
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    // Service UUID for scan_data so CoreBluetooth can discover by service UUID
    // UUID e3910010-4567-4321-abcd-abcdef012345 in little-endian byte order
    // (updated from e3910001 to force GATT cache refresh on clients)
    const SERVICE_UUID: [u8; 16] = [
        0x45, 0x23, 0x01, 0xef, 0xcd, 0xab, 0xcd, 0xab,
        0x21, 0x43, 0x67, 0x45, 0x10, 0x00, 0x91, 0xe3,
    ];
    let mut advertiser_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut advertiser_data[..],
    )?;
    let mut scan_response = [0; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::ServiceUuids128(&[SERVICE_UUID])],
        &mut scan_response[..],
    )?;
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..len],
                scan_data: &scan_response[..scan_len],
            },
        )
        .await?;
    println!("[BLE] advertising as '{}'...", name);
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    println!("[BLE] connection established");
    // Show BLE device name on line 1 of the OLED display
    let sender = crate::display::DISPLAY_STATE.sender();
    crate::display::set_line1_override(&sender, "BLE: Connected");
    Ok(conn)
}

/// Main BLE task — runs advertising + GATT server forever
#[embassy_executor::task]
pub async fn ble_task(connector: BleConnector<'static>) {
    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    let address = Address::random([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xfe]);
    println!("[BLE] address = {:?}", address);

    // Place HostResources in a static cell to avoid blowing the stack.
    // These structs are large and embassy tasks all share the main thread stack.
    static RESOURCES: StaticCell<HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>> =
        StaticCell::new();
    let resources = RESOURCES.init(HostResources::new());

    // The Stack must also live in a static cell because build() borrows &'stack self,
    // and the resulting Host/Peripheral/Runner need 'static lifetimes for the embassy task.
    type MyStack = trouble_host::Stack<'static, ExternalController<BleConnector<'static>, 20>, DefaultPacketPool>;
    static STACK: StaticCell<MyStack> = StaticCell::new();
    let stack: &'static mut MyStack =
        STACK.init(trouble_host::new(controller, resources).set_random_address(address));
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    // Place Server in a static cell as well — it contains the GATT attribute table.
    static SERVER: StaticCell<Server<'static>> = StaticCell::new();
    let server = SERVER.init(
        Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
            name: "ESP32 Motor",
            appearance: &appearance::UNKNOWN,
        }))
        .unwrap(),
    );

    println!("[BLE] GATT server started");

    // Set motor_count characteristic to the actual MOTOR_COUNT for this build
    server.motor_control.motor_count.set(server, &(MOTOR_COUNT as u8)).ok();
    println!("[BLE] motor_count characteristic set to {}", MOTOR_COUNT);

    // Update battery characteristic periodically in the background
    // (We'll do it in the advertising loop below since we have access to server)

    // Run the BLE runner and the advertising/connection loop concurrently
    let _ = join(ble_runner(runner), async {
        loop {
            match advertise("ESP32 Motor", &mut peripheral, &server).await {
                Ok(conn) => {
                    // Handle GATT events until disconnection
                    let _ = gatt_events_task(&server, &conn).await;
                }
                Err(e) => {
                    println!("[BLE] advertise error: {:?}", e);
                }
            }
        }
    })
    .await;
}
