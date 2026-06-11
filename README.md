# ESP32 ST3215 Motor Controller

Rust `no_std` firmware for an ESP32 robot controller. It drives ST3215 serial-bus servos, H-bridge/TB6612 DC motors, optional battery telemetry, a serial console, a WiFi REST API, and a BLE GATT control service.

Bluetooth is the default boot mode. WiFi credentials and the preferred radio mode are stored in flash, so the controller can be configured once and then boot directly into WiFi when requested.

For the full endpoint and BLE characteristic reference, see [API.md](API.md).

## Features

- ST3215 bus servo control over UART1 at 1 Mbps.
- Raw position moves, multi-servo sync-write moves, torque enable/disable, ID changes, ping, scan, state reads, zero calibration, wheel mode, and servo mode restore.
- Motor control through a unified command channel shared by serial, HTTP, and BLE.
- Default `two_motor` build for a TB6612 driver with optional INA219 battery telemetry.
- Optional `four_motor` build for four H-bridge channels. This build is currently guarded in code because one motor pin conflicts with the ST3215 TX line.
- BLE GATT service for motor writes, ST3215 commands, servo discovery, battery reads, and WiFi provisioning.
- WiFi REST API and browser controller page when WiFi mode is selected.
- 500 ms motor watchdog that stops DC motors if command updates stop. ST3215 servos are not watchdog-stopped because they hold position on the bus.

## Hardware

- ESP32 development board.
- ST3215 / SMS_STS-compatible serial-bus servos.
- ST3215 half-duplex bus adapter, such as the Waveshare General Driver for Robots path used by this firmware.
- TB6612 driver for the default two-motor build, or H-bridge drivers for the four-motor build.
- Optional INA219 voltage monitor for the `two_motor` build.
- Optional SSD1306 OLED display for the `four_motor` build.

### ST3215 Bus Wiring

UART1 is configured for the ST3215 bus at 1 Mbps, 8N1.

| Signal | ESP32 GPIO |
| ------ | ---------- |
| ST RX  | GPIO18     |
| ST TX  | GPIO19     |
| GND    | Shared GND |

Power ST3215 servos from an external supply sized for the servo load. Keep grounds common between the ESP32, servo supply, and motor drivers.

### Default Two-Motor Wiring

The default Cargo feature is `two_motor`, using a TB6612-style driver with STBY hardwired high.

| Motor | Direction Pins | PWM Pin |
| ----- | -------------- | ------- |
| A     | GPIO21, GPIO17 | GPIO25  |
| B     | GPIO22, GPIO23 | GPIO26  |

Optional INA219 battery telemetry uses I2C0:

| Signal | ESP32 GPIO |
| ------ | ---------- |
| SDA    | GPIO32     |
| SCL    | GPIO33     |

## Software Setup

Install the ESP Rust toolchain and flashing tool:

```bash
cargo install espup
espup install
source ~/export-esp.sh
cargo install cargo-espflash
```

## Build And Flash

Default build, with two motors and BLE-first boot behavior:

```bash
cargo espflash flash --monitor
```

Explicit two-motor build:

```bash
cargo espflash flash --features two_motor --monitor
```

WiFi support is compiled in by the firmware, but the device starts BLE by default unless persisted radio mode is set to WiFi. Use the serial `wi` command or BLE WiFi provisioning to switch.

## Radio Modes And WiFi Provisioning

The firmware stores radio preference and WiFi credentials in flash.

Serial commands:

```text
wifi <ssid> <password>    # store WiFi credentials
wi                        # store WiFi boot mode and reboot
ble                       # store BLE boot mode and reboot
```

BLE provisioning writes credentials as `SSID\0PASSWORD` to the WiFi Config characteristic. This is the better path for SSIDs or passwords containing spaces.

When WiFi mode is persisted, the ESP32 tries WiFi first on each boot. If it does not obtain an IP address within 10 seconds, BLE advertising starts as a fallback for that boot without changing the persisted mode.

## Quick REST Examples

After WiFi connects, the ESP32 serves HTTP on port 80. The root route returns the embedded controller page.

```bash
# Health and build configuration
curl http://192.168.1.100/health
curl http://192.168.1.100/config

# Motors
curl 'http://192.168.1.100/motors?a=60&b=-60'
curl http://192.168.1.100/motor/a/0

# Legacy angle route: maps 0-180 degrees to ST3215 ID 1 position 0-4095
curl http://192.168.1.100/servo/90

# ST3215 bus servos
curl http://192.168.1.100/st/list
curl 'http://192.168.1.100/st/scan?from=1&to=20'
curl 'http://192.168.1.100/st/1/pos/2048?speed=1500&acc=50'
curl 'http://192.168.1.100/st/all?1=2048&2=1024&speed=1500&acc=50'
curl http://192.168.1.100/st/1/torque/1
curl http://192.168.1.100/st/1/zero
curl 'http://192.168.1.100/st/1/wheel/-1200?acc=50'
curl http://192.168.1.100/st/1/mode/servo
curl http://192.168.1.100/st/1/ping
curl http://192.168.1.100/st/state
```

Common REST responses are JSON. Successful ST3215 commands return `ok: true`; bus failures return an error string and a non-OK status where the operation needs a bus transaction.

## Serial Console

UART0 accepts line-oriented commands while monitoring with `cargo espflash flash --monitor`.

```text
# Motors
m 0                         # set all motors
ma 50                       # Motor A forward at 50%
mb -75                      # Motor B reverse at 75%
motor a 80                  # verbose motor form

# ST3215 discovery and movement
st list                     # scan 1..=20 and print discovered IDs
st scan 1 40                # scan a custom ID range
st 1 pos 2048 speed 1500 acc 50
st all 1=2048 2=1024 speed 1500 acc 50

# ST3215 maintenance and modes
st 1 torque 1
st 1 zero                   # calibrate current physical position as zero/home
st 1 wheel -1200 acc 50     # continuous rotation, -4095..=4095
st 1 servo                  # return to position-control mode
st 1 setid 2
st 1 ping
st 1 state                  # serial pings; use HTTP /st/state for full state
```

## BLE GATT API

The BLE peripheral advertises as `ESP32 Motor` and exposes service UUID `e3910040-4567-4321-abcd-abcdef012345`.

| Characteristic | UUID                                   | Access            | Value                                                        |
| -------------- | -------------------------------------- | ----------------- | ------------------------------------------------------------ |
| Motors         | `e3910003-4567-4321-abcd-abcdef012345` | read/write/notify | 4 bytes, first `MOTOR_COUNT` bytes are signed motor powers   |
| WiFi Config    | `e3910004-4567-4321-abcd-abcdef012345` | write             | `SSID\0PASSWORD`                                             |
| Battery        | `e3910006-4567-4321-abcd-abcdef012345` | read              | `[percentage, voltage_mv_hi, voltage_mv_lo]`                 |
| ST List        | `e3910011-4567-4321-abcd-abcdef012345` | read/notify       | 16 zero-padded discovered servo IDs                          |
| ST Cmd         | `e3910012-4567-4321-abcd-abcdef012345` | write             | 6-byte command frames                                        |
| ST State       | `e3910013-4567-4321-abcd-abcdef012345` | read              | `[id, err, pos_lo, pos_hi, load_lo, load_hi, voltage, temp]` |

ST Cmd opcodes:

| Opcode | Frame                                           | Description                |
| ------ | ----------------------------------------------- | -------------------------- |
| `0x01` | `[op, id, pos_lo, pos_hi, speed_lo, speed_hi]`  | Move one servo             |
| `0x02` | `[op, id, enable, 0, 0, 0]`                     | Torque on/off              |
| `0x03` | `[op, current_id, new_id, 0, 0, 0]`             | Change servo ID            |
| `0x04` | `[op, id, 0, 0, 0, 0]`                          | Ping                       |
| `0x05` | `[op, id, 0, 0, 0, 0]`                          | Refresh ST State           |
| `0x06` | `[op, from, to, 0, 0, 0]`                       | Rescan bus                 |
| `0x07` | `[op, pos_lo, pos_hi, speed_lo, speed_hi, acc]` | Move all discovered servos |
| `0x08` | `[op, id, 0, 0, 0, 0]`                          | Zero calibration           |
| `0x09` | `[op, id, speed_lo, speed_hi, acc, 0]`          | Wheel mode signed speed    |
| `0x0A` | `[op, id, 0, 0, 0, 0]`                          | Servo position mode        |

## ST3215 Notes

- Positions are raw ST3215 units, `0..=4095`.
- Wheel speed is signed, `-4095..=4095`; `0` stops continuous rotation.
- Zero calibration persists in the servo by writing the STS calibration command.
- `GET /st/state` reads current position, speed, load, voltage, and temperature for discovered IDs.
- Servo ID changes write EEPROM, then the firmware rescans IDs `1..=20`.

## Project Structure

```text
src/
|-- bin/main.rs       # boot, peripherals, radio setup, ST3215 scan, command loop
|-- ble.rs            # BLE GATT service and ST3215 command frames
|-- brushless.rs      # motor driver abstractions for two/four motor builds
|-- commands.rs       # unified command channel and shared telemetry state
|-- display.rs        # optional OLED display state/task
|-- http_server.rs    # picoserve REST API and embedded controller page
|-- serial_cmd.rs     # UART0 command parser
|-- st3215.rs         # ST3215/SMS_STS bus driver
|-- wifi.rs           # WiFi tasks, web task startup, BLE fallback
`-- wifi_config.rs    # persisted credentials and radio mode
```

## Execution Model

Input tasks do not drive hardware directly. HTTP, BLE, and serial all enqueue `Command` values into the global command channel. The main command loop owns actuator updates, serializes access to the ST3215 bus mutex, and applies the motor watchdog.

```text
Serial / HTTP / BLE
        |
        v
   COMMANDS channel
        |
        v
 main command loop
        |
        +--> TB6612 / H-bridge motors
        +--> ST3215 bus driver on UART1
```

Embassy tasks sleep at `.await` points and wake from UART, WiFi, BLE, timer, or channel events. There is no fixed polling tick in the main loop beyond the 50 ms watchdog check.

## Dependencies

Key crates used:

| Crate              | Purpose                            |
| ------------------ | ---------------------------------- |
| `esp-hal`          | ESP32 hardware abstraction layer   |
| `esp-radio`        | WiFi/BLE radio support             |
| `esp-rtos`         | Embassy integration for ESP32      |
| `embassy-net`      | Async TCP/IP networking            |
| `embassy-executor` | Async task executor                |
| `embassy-sync`     | Channels, mutexes, and signals     |
| `picoserve`        | Embedded HTTP server               |
| `trouble-host`     | BLE host and GATT server           |
| `esp-storage`      | Flash-backed configuration storage |
