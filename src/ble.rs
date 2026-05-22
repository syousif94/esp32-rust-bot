//! BLE GATT server module for motor and ST3215 bus servo control.
//!
//! Exposes a custom GATT service that a Swift app (CoreBluetooth) can
//! connect to. Provides characteristics for motor power levels, ST3215
//! commands, servo discovery, and WiFi configuration.

use crate::commands::{BATTERY_MV, BATTERY_PCT, Command, MOTOR_COUNT, send_command};
use crate::st3215::{MAX_SERVOS, SHARED_BUS, SHARED_LIST};
use crate::wifi_config::{MAX_PASSWORD_LEN, MAX_SSID_LEN};
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

/// WiFi credentials received via BLE (SSID, password as heapless strings)
pub type WifiCredentialsData = (
    heapless::String<MAX_SSID_LEN>,
    heapless::String<MAX_PASSWORD_LEN>,
);

/// Signal to pass WiFi credentials to main task for flash storage
pub static BLE_WIFI_CREDENTIALS: Signal<CriticalSectionRawMutex, WifiCredentialsData> =
    Signal::new();

// Custom 128-bit UUIDs.
//
// Service UUID bumped after GATT layout changes so iOS / CoreBluetooth
// invalidates its cached GATT table and re-discovers the new layout.
//
// Service:     e3910030-4567-4321-abcd-abcdef012345
// Motors:      e3910003-4567-4321-abcd-abcdef012345
// WiFi Config: e3910004-4567-4321-abcd-abcdef012345
// Battery:     e3910006-4567-4321-abcd-abcdef012345
// ST list:    e3910011-4567-4321-abcd-abcdef012345 (read+notify, 16 IDs)
// ST cmd:     e3910012-4567-4321-abcd-abcdef012345 (write, 6 bytes)
// ST state:   e3910013-4567-4321-abcd-abcdef012345 (read, 8 bytes)

/// GATT Server definition with our motor control service
#[gatt_server]
struct Server {
    motor_control: MotorControlService,
}

/// Custom motor + bus-servo control GATT service.
///
/// Keep the characteristic count small: every characteristic adds 2-3
/// attributes to the `AttributeTable` that `Server::new_with_config` builds
/// on the stack, and the ESP32 main-task stack is tight when the WiFi heap
/// is also active.
#[gatt_service(uuid = "e3910030-4567-4321-abcd-abcdef012345")]
struct MotorControlService {
    /// All motors (4 bytes; only the first MOTOR_COUNT are used).
    #[characteristic(uuid = "e3910003-4567-4321-abcd-abcdef012345", read, write, write_without_response, notify, value = [0, 0, 0, 0])]
    motors: [u8; 4],

    /// WiFi config (write-only): "SSID\0PASSWORD" (null-separated).
    /// 65 bytes = 32 SSID + null + 32 password (WPA2 max).
    #[characteristic(uuid = "e3910004-4567-4321-abcd-abcdef012345", write, value = [0u8; 65])]
    wifi_config: [u8; 65],

    /// Battery level [percentage, voltage_mv_hi, voltage_mv_lo].
    /// Read-only (no notify) to save one CCCD attribute slot.
    #[characteristic(uuid = "e3910006-4567-4321-abcd-abcdef012345", read, value = [0, 0, 0])]
    battery: [u8; 3],

    /// Discovered ST3215 servo IDs, zero-padded to 16 bytes.
    #[characteristic(uuid = "e3910011-4567-4321-abcd-abcdef012345", read, notify, value = [0u8; 16])]
    st_list: [u8; 16],

    /// Command channel for ST3215 servos (6 bytes, see opcodes below).
    ///   [0x01, id, pos_lo, pos_hi, speed_lo, speed_hi]  -> MOVE
    ///   [0x02, id, enable, 0, 0, 0]                     -> TORQUE
    ///   [0x03, current_id, new_id, 0, 0, 0]             -> SET_ID
    ///   [0x04, id, 0, 0, 0, 0]                          -> PING
    ///   [0x05, id, 0, 0, 0, 0]                          -> READ (refreshes st_state)
    ///   [0x06, from, to, 0, 0, 0]                       -> RESCAN
    #[characteristic(uuid = "e3910012-4567-4321-abcd-abcdef012345", write, write_without_response, value = [0u8; 6])]
    st_cmd: [u8; 6],

    /// Last-read ST3215 state: [id, err, pos_lo, pos_hi, load_lo, load_hi, voltage, temp].
    /// Read-only (no notify); refresh on demand via st_cmd op 0x05 then read.
    #[characteristic(uuid = "e3910013-4567-4321-abcd-abcdef012345", read, value = [0u8; 8])]
    st_state: [u8; 8],
}

/// Default move parameters for BLE commands that omit speed/acc.
const BLE_DEFAULT_ACC: u8 = 50;

/// Snapshot the shared servo list into a 16-byte buffer (zero-padded).
async fn snapshot_list() -> [u8; 16] {
    let mut out = [0u8; 16];
    if let Some(list) = SHARED_LIST.try_get() {
        let g = list.lock().await;
        for (i, id) in g.iter().take(MAX_SERVOS).enumerate() {
            out[i] = *id;
        }
    }
    out
}

/// Run the BLE host stack background task
async fn ble_runner<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    let _ = runner.run().await;
}

/// Read state of `id` from the bus and push it into the st_state characteristic.
async fn refresh_st_state<P: PacketPool>(
    server: &Server<'_>,
    _conn: &GattConnection<'_, '_, P>,
    id: u8,
) {
    let Some(bus) = SHARED_BUS.try_get() else {
        return;
    };
    let mut g = bus.lock().await;
    let payload = match g.read_state(id).await {
        Ok(st) => {
            let pos = st.pos as u16;
            let load = st.load as u16;
            [
                id,
                0,
                pos as u8,
                (pos >> 8) as u8,
                load as u8,
                (load >> 8) as u8,
                st.voltage,
                st.temp,
            ]
        }
        Err(_) => [id, 0xFF, 0, 0, 0, 0, 0, 0],
    };
    drop(g);
    let _ = server.motor_control.st_state.set(server, &payload);
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
                        println!("[BLE] Write: handle={:?}, {} bytes", handle, data.len());

                        if handle == server.motor_control.motors.handle {
                            if data.len() >= MOTOR_COUNT {
                                let mut powers = [0i8; MOTOR_COUNT];
                                for i in 0..MOTOR_COUNT {
                                    powers[i] = data[i] as i8;
                                }
                                send_command(Command::MotorsAll(powers));
                            }
                        } else if handle == server.motor_control.wifi_config.handle {
                            if let Some(null_pos) = data.iter().position(|&b| b == 0) {
                                let ssid_bytes = &data[..null_pos];
                                let pass_bytes = &data[null_pos + 1..];
                                if let (Ok(ssid_str), Ok(pass_str)) = (
                                    core::str::from_utf8(ssid_bytes),
                                    core::str::from_utf8(pass_bytes),
                                ) {
                                    if !ssid_str.is_empty()
                                        && ssid_str.len() <= MAX_SSID_LEN
                                        && pass_str.len() <= MAX_PASSWORD_LEN
                                    {
                                        if let (Ok(ssid), Ok(password)) = (
                                            heapless::String::try_from(ssid_str),
                                            heapless::String::try_from(pass_str),
                                        ) {
                                            BLE_WIFI_CREDENTIALS.signal((ssid, password));
                                            println!("[BLE] WiFi credentials queued");
                                        }
                                    }
                                }
                            }
                        } else if handle == server.motor_control.st_cmd.handle {
                            handle_st_cmd(server, conn, data).await;
                        }
                    }
                    GattEvent::Read(read_event) => {
                        let handle = read_event.handle();
                        if handle == server.motor_control.battery.handle {
                            let mv = BATTERY_MV.load(core::sync::atomic::Ordering::Relaxed);
                            let pct = BATTERY_PCT.load(core::sync::atomic::Ordering::Relaxed);
                            let _ = server
                                .motor_control
                                .battery
                                .set(server, &[pct, (mv >> 8) as u8, mv as u8]);
                        } else if handle == server.motor_control.st_list.handle {
                            let snap = snapshot_list().await;
                            let _ = server.motor_control.st_list.set(server, &snap);
                        }
                    }
                    other => {
                        println!(
                            "[BLE] Other GATT event: {:?}",
                            core::any::type_name_of_val(other)
                        );
                    }
                }
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => println!("[BLE] reply error: {:?}", e),
                }
            }
            other => {
                println!(
                    "[BLE] Connection event: {:?}",
                    core::any::type_name_of_val(&other)
                );
            }
        }
    };
    println!("[BLE] disconnected: {:?}", reason);
    let sender = crate::display::DISPLAY_STATE.sender();
    crate::display::set_line1_override(&sender, "BLE: Disconnected");
    Ok(())
}

/// Dispatch a 6-byte ST3215 command frame written to the st_cmd characteristic.
async fn handle_st_cmd<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    data: &[u8],
) {
    if data.len() < 6 {
        println!("[BLE] st_cmd: short ({} bytes)", data.len());
        return;
    }
    let op = data[0];
    match op {
        0x01 => {
            // MOVE { id, pos_lo, pos_hi, speed_lo, speed_hi }
            let id = data[1];
            let pos = u16::from_le_bytes([data[2], data[3]]);
            let speed = u16::from_le_bytes([data[4], data[5]]);
            send_command(Command::St3215Move {
                id,
                pos,
                speed,
                acc: BLE_DEFAULT_ACC,
            });
        }
        0x02 => {
            send_command(Command::St3215Torque {
                id: data[1],
                enable: data[2] != 0,
            });
        }
        0x03 => {
            send_command(Command::St3215SetId {
                current: data[1],
                new: data[2],
            });
            // List will be re-pushed via notify after rescan completes; client
            // can also re-read st_list on its own.
        }
        0x04 => {
            send_command(Command::St3215Ping { id: data[1] });
        }
        0x05 => {
            refresh_st_state(server, conn, data[1]).await;
        }
        0x06 => {
            let from = if data[1] == 0 { 1 } else { data[1] };
            let to = if data[2] == 0 { 20 } else { data[2] };
            send_command(Command::St3215Rescan { from, to });
            // Push a fresh snapshot a moment later — best-effort.
            let snap = snapshot_list().await;
            let _ = server.motor_control.st_list.set(server, &snap);
        }
        _ => println!("[BLE] st_cmd: unknown op 0x{:02X}", op),
    }
}

/// Create an advertiser and wait for a central to connect
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    // UUID e3910030-4567-4321-abcd-abcdef012345 in little-endian byte order
    const SERVICE_UUID: [u8; 16] = [
        0x45, 0x23, 0x01, 0xef, 0xcd, 0xab, 0xcd, 0xab, 0x21, 0x43, 0x67, 0x45, 0x30, 0x00, 0x91,
        0xe3,
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

    static RESOURCES: StaticCell<
        HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>,
    > = StaticCell::new();
    let resources = RESOURCES.init(HostResources::new());

    type MyStack = trouble_host::Stack<
        'static,
        ExternalController<BleConnector<'static>, 20>,
        DefaultPacketPool,
    >;
    static STACK: StaticCell<MyStack> = StaticCell::new();
    let stack: &'static mut MyStack =
        STACK.init(trouble_host::new(controller, resources).set_random_address(address));
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    // Use `init_with` so the (large) Server is constructed in-place inside
    // the StaticCell rather than on the main-task stack — with our
    // 7-characteristic service the attribute table alone is >1 KB and an
    // intermediate stack copy trips the stack guard.
    static SERVER: StaticCell<Server<'static>> = StaticCell::new();
    let server = SERVER.init_with(|| {
        Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
            name: "ESP32 Motor",
            appearance: &appearance::UNKNOWN,
        }))
        .unwrap()
    });

    println!("[BLE] GATT server started (MOTOR_COUNT = {})", MOTOR_COUNT);

    let _ = join(ble_runner(runner), async {
        loop {
            match advertise("ESP32 Motor", &mut peripheral, &server).await {
                Ok(conn) => {
                    // Push the current list at connect time.
                    let snap = snapshot_list().await;
                    let _ = server.motor_control.st_list.set(server, &snap);
                    let _ = gatt_events_task(&server, &conn).await;
                }
                Err(e) => println!("[BLE] advertise error: {:?}", e),
            }
        }
    })
    .await;
}
