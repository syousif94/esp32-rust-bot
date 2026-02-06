//! BLE GATT server module for motor and servo control.
//!
//! Exposes a custom GATT service that a Swift app (CoreBluetooth) can connect to.
//! Provides read/write characteristics for servo angle and motor power levels,
//! using the same signals as the HTTP server and serial command modules.

use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use esp_println::println;
use esp_radio::ble::controller::BleConnector;
use static_cell::StaticCell;
use trouble_host::prelude::*;

/// Max number of BLE connections (1 is typical for a peripheral)
const CONNECTIONS_MAX: usize = 1;

/// Max number of L2CAP channels (Signal + ATT)
const L2CAP_CHANNELS_MAX: usize = 2;

/// Signal for servo angle updates from BLE
pub static BLE_SERVO_ANGLE: Signal<CriticalSectionRawMutex, u8> = Signal::new();

/// Signal for motor A power updates from BLE (-100 to +100)
pub static BLE_MOTOR_A_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor B power updates from BLE (-100 to +100)
pub static BLE_MOTOR_B_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor C power updates from BLE (-100 to +100)
pub static BLE_MOTOR_C_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor D power updates from BLE (-100 to +100)
pub static BLE_MOTOR_D_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

// Custom 128-bit UUIDs for our service and characteristics.
// These are random UUIDs — the Swift app will scan for the service UUID.
//
// Service:  e3910001-4567-4321-abcd-abcdef012345
// Servo:    e3910002-4567-4321-abcd-abcdef012345
// Motor A:  e3910003-4567-4321-abcd-abcdef012345
// Motor B:  e3910004-4567-4321-abcd-abcdef012345
// Motor C:  e3910005-4567-4321-abcd-abcdef012345
// Motor D:  e3910006-4567-4321-abcd-abcdef012345

/// GATT Server definition with our motor control service
#[gatt_server]
struct Server {
    motor_control: MotorControlService,
}

/// Custom motor control GATT service
#[gatt_service(uuid = "e3910001-4567-4321-abcd-abcdef012345")]
struct MotorControlService {
    /// Servo angle (0-180), read + write + notify
    #[characteristic(uuid = "e3910002-4567-4321-abcd-abcdef012345", read, write, notify, value = 90)]
    servo_angle: u8,

    /// Motor A power (-100 to 100), read + write + notify
    #[characteristic(uuid = "e3910003-4567-4321-abcd-abcdef012345", read, write, notify, value = 0)]
    motor_a: i8,

    /// Motor B power (-100 to 100), read + write + notify
    #[characteristic(uuid = "e3910004-4567-4321-abcd-abcdef012345", read, write, notify, value = 0)]
    motor_b: i8,

    /// Motor C power (-100 to 100), read + write + notify
    #[characteristic(uuid = "e3910005-4567-4321-abcd-abcdef012345", read, write, notify, value = 0)]
    motor_c: i8,

    /// Motor D power (-100 to 100), read + write + notify
    #[characteristic(uuid = "e3910006-4567-4321-abcd-abcdef012345", read, write, notify, value = 0)]
    motor_d: i8,
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

                        // Check which characteristic was written and signal accordingly
                        if handle == server.motor_control.servo_angle.handle {
                            if let Some(&val) = data.first() {
                                if val <= 180 {
                                    println!("[BLE] Servo angle set to {}", val);
                                    BLE_SERVO_ANGLE.signal(val);
                                }
                            }
                        } else if handle == server.motor_control.motor_a.handle {
                            if let Some(&val) = data.first() {
                                let power = val as i8;
                                if power >= -100 && power <= 100 {
                                    println!("[BLE] Motor A set to {}%", power);
                                    BLE_MOTOR_A_POWER.signal(power);
                                }
                            }
                        } else if handle == server.motor_control.motor_b.handle {
                            if let Some(&val) = data.first() {
                                let power = val as i8;
                                if power >= -100 && power <= 100 {
                                    println!("[BLE] Motor B set to {}%", power);
                                    BLE_MOTOR_B_POWER.signal(power);
                                }
                            }
                        } else if handle == server.motor_control.motor_c.handle {
                            if let Some(&val) = data.first() {
                                let power = val as i8;
                                if power >= -100 && power <= 100 {
                                    println!("[BLE] Motor C set to {}%", power);
                                    BLE_MOTOR_C_POWER.signal(power);
                                }
                            }
                        } else if handle == server.motor_control.motor_d.handle {
                            if let Some(&val) = data.first() {
                                let power = val as i8;
                                if power >= -100 && power <= 100 {
                                    println!("[BLE] Motor D set to {}%", power);
                                    BLE_MOTOR_D_POWER.signal(power);
                                }
                            }
                        }
                    }
                    GattEvent::Read(_) => {
                        // Reads are handled automatically by the GATT server
                    }
                    _ => {}
                }
                // Accept and send the reply
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => println!("[BLE] error sending GATT response: {:?}", e),
                }
            }
            _ => {} // ignore other events
        }
    };
    println!("[BLE] disconnected: {:?}", reason);
    Ok(())
}

/// Create an advertiser and wait for a central to connect
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut advertiser_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut advertiser_data[..],
    )?;
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..len],
                scan_data: &[],
            },
        )
        .await?;
    println!("[BLE] advertising as '{}'...", name);
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    println!("[BLE] connection established");
    Ok(conn)
}

/// Main BLE task — runs advertising + GATT server forever
#[embassy_executor::task]
pub async fn ble_task(connector: BleConnector<'static>) {
    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    let address = Address::random([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xff]);
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
