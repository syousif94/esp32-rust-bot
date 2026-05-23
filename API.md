# API Reference

This document covers the control interfaces exposed by the ESP32: a **serial console**, a **REST API** over WiFi, and a **BLE GATT** service. All interfaces control the same motors and ST3215 bus servos; commands from any source are handled identically.

Bluetooth is the default radio mode after boot. WiFi credentials can be stored over serial or BLE. The serial `wi` and `ble` commands store the requested boot mode in flash and reboot into WiFi or BLE; this does not require separate BLE and WiFi firmware images.

The firmware supports two motor configurations selected at compile time via Cargo features:

| Feature      | Motors | Motor IDs  | Description                     |
| ------------ | ------ | ---------- | ------------------------------- |
| `four_motor` | 4      | A, B, C, D | Default — 4 H-bridge channels   |
| `two_motor`  | 2      | A, B       | TB6612 driver with STBY standby |

Clients can query the active HTTP motor configuration at runtime via **`GET /config`**. BLE motor writes use the first `MOTOR_COUNT` bytes of the Motors characteristic.

---

## REST API

The ESP32 runs an HTTP server on **port 80** after WiFi mode is requested and the station connects. All endpoints accept **GET** requests and return **JSON** responses.

### Base URL

```
http://<ESP32_IP>/
```

The IP address is printed to the serial monitor on boot.

---

## Serial Console

UART0 accepts line-oriented commands. Press Enter after each command.

### Radio Mode

| Command                  | Description                                      |
| ------------------------ | ------------------------------------------------ |
| `wifi <ssid> <password>` | Store WiFi credentials in flash                  |
| `wi`                     | Store WiFi boot mode and reboot using stored credentials |
| `ble`                    | Store BLE boot mode and reboot into BLE advertising |

Serial WiFi credentials use whitespace-separated arguments, so SSIDs and passwords containing spaces are not supported by the serial command. Use the BLE WiFi Config characteristic if you need spaces. Radio mode persists until changed with `wi` or `ble`.

### Motors And Servos

| Command                              | Description                    |
| ------------------------------------ | ------------------------------ |
| `m <power>`                          | Set all motors                 |
| `ma <power>` / `mb <power>`          | Set Motor A or B               |
| `st list`                            | Rescan and print servo IDs     |
| `st scan [from to]`                  | Rescan a servo ID range        |
| `st all <id>=<pos> ... [speed <s>] [acc <a>]` | Move discovered servos atomically |
| `st <id> pos <v> [speed <s>] [acc <a>]` | Move one ST3215 servo          |
| `st <id> torque <0|1>`               | Disable or enable torque       |
| `st <id> setid <new>`                | Change servo ID and rescan     |
| `st <id> ping`                       | Ping one servo                 |

### Endpoints

#### `GET /`

Returns server status and lists available endpoints.

**Response** `200 OK`

```json
{
  "status": "ok",
  "message": "ESP32 Motor & Servo Controller",
  "endpoints": [
    "/servo/<angle>",
    "/servo?angle=<0-180>",
    "/motor/a/<power>",
    "/motor/b/<power>",
    "/motor/c/<power>",
    "/motor/d/<power>",
    "/motor/<a|b|c|d>?power=<-100 to 100>"
  ]
}
```

#### `GET /health`

Health check endpoint.

**Response** `200 OK`

```json
{ "healthy": true }
```

#### `GET /config`

Returns the motor configuration for this firmware build.

**Response** `200 OK` (two_motor build)

```json
{ "motor_mode": "two_motor", "motor_count": 2, "motors": ["a", "b"] }
```

**Response** `200 OK` (four_motor build)

```json
{ "motor_mode": "four_motor", "motor_count": 4, "motors": ["a", "b", "c", "d"] }
```

Use this endpoint on connect to discover how many motors are available and adapt your UI accordingly.

#### `GET /battery`

Returns the current battery voltage and estimated percentage (3S LiPo). Available only in `two_motor` builds with an INA219 sensor connected via I2C (SDA=GPIO32, SCL=GPIO33). In `four_motor` builds this endpoint returns zeroes.

**Response** `200 OK`

```json
{ "voltage": "11.72", "voltage_mv": 11720, "percentage": 57 }
```

| Field        | Type   | Description                                   |
| ------------ | ------ | --------------------------------------------- |
| `voltage`    | string | Bus voltage formatted as `"X.XX"` (volts)     |
| `voltage_mv` | u16    | Bus voltage in millivolts                     |
| `percentage` | u8     | Estimated battery percentage (0–100, 3S LiPo) |

The controller HTML page polls this endpoint every 5 seconds to display battery status.

---

### Servo

#### `GET /servo/<angle>`

Set the servo angle using a path parameter.

| Parameter | Type | Range | Description             |
| --------- | ---- | ----- | ----------------------- |
| `angle`   | u8   | 0–180 | Target angle in degrees |

**Example**

```bash
curl http://192.168.1.100/servo/90
```

**Response** `200 OK`

```json
{ "angle": 90 }
```

#### `GET /servo?angle=<angle>`

Set the servo angle using a query parameter.

**Example**

```bash
curl http://192.168.1.100/servo?angle=45
```

**Response** `200 OK`

```json
{ "angle": 45 }
```

#### Errors

| Status | Condition                                |
| ------ | ---------------------------------------- |
| `400`  | Angle > 180 or missing/non-numeric value |

```json
{ "error": "Angle must be between 0 and 180" }
```

---

### Motors

#### `GET /motors?a=<power>&b=<power>` (two_motor) / `GET /motors?a=<power>&b=<power>&c=<power>&d=<power>` (four_motor)

Set power for one or more motors in a single request. All parameters are optional — only the motors included in the query string are updated. Omitted motors default to 0.

| Parameter | Type | Range    | Description                        | Availability |
| --------- | ---- | -------- | ---------------------------------- | ------------ |
| `a`       | i8   | -100–100 | Motor A power (negative = reverse) | Both         |
| `b`       | i8   | -100–100 | Motor B power (negative = reverse) | Both         |
| `c`       | i8   | -100–100 | Motor C power (negative = reverse) | four_motor   |
| `d`       | i8   | -100–100 | Motor D power (negative = reverse) | four_motor   |

**Examples**

```bash
# Two-motor: set both motors
curl http://192.168.1.100/motors?a=75&b=-50

# Four-motor: set all four motors
curl http://192.168.1.100/motors?a=75&b=75&c=-50&d=-50

# Set only motor A
curl http://192.168.1.100/motors?a=60

# Stop all motors
curl http://192.168.1.100/motors?a=0&b=0
```

**Response** `200 OK` (two_motor)

```json
{ "a": 75, "b": -50 }
```

**Response** `200 OK` (four_motor)

```json
{ "a": 75, "b": 75, "c": -50, "d": -50 }
```

Motors not included in the request are returned as `null`:

```json
{ "a": 60, "b": null }
```

#### `GET /motor/<id>/<power>`

Set a single motor's power using a path parameter.

| Parameter | Type | Values                                        | Description                      |
| --------- | ---- | --------------------------------------------- | -------------------------------- |
| `id`      | char | `a`, `b` (both) or `c`, `d` (four_motor only) | Motor identifier                 |
| `power`   | i8   | -100 to 100                                   | Power level (negative = reverse) |

**Examples**

```bash
# Motor A forward at 75%
curl http://192.168.1.100/motor/a/75

# Motor B reverse at 50%
curl http://192.168.1.100/motor/b/-50

# Stop Motor A
curl http://192.168.1.100/motor/a/0
```

**Response** `200 OK`

```json
{ "motor": "a", "power": 75 }
```

#### `GET /motor/<id>?power=<power>`

Set motor power using a query parameter.

**Example**

```bash
curl http://192.168.1.100/motor/a?power=80
```

#### Errors

| Status | Condition                                                   |
| ------ | ----------------------------------------------------------- |
| `400`  | Power outside -100..100, missing value, or invalid motor id |
| `404`  | Unknown path                                                |
| `405`  | Non-GET method                                              |

---

## BLE GATT API

The ESP32 advertises as a BLE peripheral named **"ESP32 Motor"** in BLE boot mode. It exposes a single custom GATT service with characteristics for each controllable output, plus WiFi configuration. Serial `wi` stores WiFi boot mode and reboots; serial `ble` stores BLE boot mode and reboots.

### Scanning

| Property         | Value                              |
| ---------------- | ---------------------------------- |
| Device Name      | `ESP32 Motor`                      |
| Advertising Type | Connectable, Scannable, Undirected |
| Discoverable     | LE General Discoverable            |
| BR/EDR           | Not supported (BLE only)           |
| Scan Response    | 128-bit service UUID               |

CoreBluetooth and Android can discover the device by scanning for the service UUID directly.

### Service

| Field        | Value                                  |
| ------------ | -------------------------------------- |
| Service UUID | `E3910040-4567-4321-ABCD-ABCDEF012345` |

> **Note:** Service UUID changed from `E3910030-...` to `E3910040-...` to force GATT cache refresh on clients after GATT layout changes.

### Characteristics

| Name        | UUID                                   | Type       | Length  | Access | Range/Format                 | Default        | Description                                    |
| ----------- | -------------------------------------- | ---------- | ------- | ------ | ---------------------------- | -------------- | ---------------------------------------------- |
| Motors      | `E3910003-4567-4321-ABCD-ABCDEF012345` | `[i8; 4]`  | 4 bytes | R/W/N  | -100-100 each                | `[0, 0, 0, 0]` | Motor powers; first `MOTOR_COUNT` bytes apply  |
| WiFi Config | `E3910004-4567-4321-ABCD-ABCDEF012345` | bytes      | 1-65    | W      | `SSID\0PASSWORD` (see below) | zeroed         | Set WiFi credentials, stored in flash          |
| Battery     | `E3910006-4567-4321-ABCD-ABCDEF012345` | `[u8; 3]`  | 3 bytes | R      | see below                    | `[0, 0, 0]`    | Battery percentage + voltage (two_motor only)  |
| ST List     | `E3910011-4567-4321-ABCD-ABCDEF012345` | `[u8; 16]` | 16 bytes | R/N    | zero-padded servo IDs        | all zeroes     | Discovered ST3215 servo IDs                    |
| ST Cmd      | `E3910012-4567-4321-ABCD-ABCDEF012345` | `[u8; 6]`  | 6 bytes | W      | opcode frame (see below)     | all zeroes     | ST3215 command channel                         |
| ST State    | `E3910013-4567-4321-ABCD-ABCDEF012345` | `[u8; 8]`  | 8 bytes | R      | state frame (see below)      | all zeroes     | Last ST3215 state read via ST Cmd opcode `0x05` |

#### Motors Byte Layout

| Byte | Motor | Type | Range    | Notes                                  |
| ---- | ----- | ---- | -------- | -------------------------------------- |
| 0    | A     | i8   | -100–100 |                                        |
| 1    | B     | i8   | -100–100 |                                        |
| 2    | C     | i8   | -100–100 | four_motor only (ignored in two_motor) |
| 3    | D     | i8   | -100–100 | four_motor only (ignored in two_motor) |

In **two_motor** mode, writes with at least 2 bytes are accepted. In **four_motor** mode, writes with at least 4 bytes are required. All motor powers are updated atomically in a single BLE write.

#### WiFi Config Format

The WiFi Config characteristic accepts a null-separated string containing the SSID and password:

```
SSID\0PASSWORD
```

| Field    | Max Length | Description                         |
| -------- | ---------- | ----------------------------------- |
| SSID     | 32 bytes   | WiFi network name (UTF-8)           |
| `\0`     | 1 byte     | Null separator (0x00)               |
| Password | 64 bytes   | WiFi password (UTF-8), can be empty |

**Total max length:** 97 bytes (32 + 1 + 64)

**Example (hex):** `MyNetwork` with password `secret123`

```
4D794E6574776F726B 00 736563726574313233
│                  │  └── "secret123"
│                  └── null separator
└── "MyNetwork"
```

Credentials are stored in flash and persist across reboots. After writing new credentials, the device will use them the next time WiFi mode is requested with serial `wi`. A "WiFi Saved!" message flashes on the OLED display upon successful write.

### Data Format

- **Motors**: 4 signed bytes `[A, B, C, D]`. Each byte is an i8 in the range -100 to +100. Negative values = reverse. In two_motor mode only the first 2 bytes are used; writes with fewer than 2 bytes are rejected. In four_motor mode all 4 bytes are used; writes with fewer than 4 bytes are rejected.
- **Battery**: 3 bytes, read-only. Read on demand from the INA219 sensor values mirrored by the firmware. See Battery Byte Layout below.
- **ST List**: 16 unsigned bytes containing discovered ST3215 servo IDs, zero-padded after the last discovered ID. Subscribe for notifications or read after `ST Cmd` opcode `0x06`.
- **ST Cmd**: 6-byte write-only opcode frame. See ST3215 Command Frames below.
- **ST State**: 8-byte read-only state frame refreshed by `ST Cmd` opcode `0x05`. See ST3215 State Frame below.

#### Battery Byte Layout

| Byte | Field          | Type | Range | Notes                        |
| ---- | -------------- | ---- | ----- | ---------------------------- |
| 0    | Percentage     | u8   | 0–100 | Estimated 3S LiPo percentage |
| 1    | Voltage (high) | u8   | —     | `voltage_mv >> 8`            |
| 2    | Voltage (low)  | u8   | —     | `voltage_mv & 0xFF`          |

To reconstruct the voltage in millivolts: `voltage_mv = (byte[1] << 8) | byte[2]`.

The percentage is a piecewise-linear approximation of a 3S LiPo discharge curve (9.0V = 0%, 12.6V = 100%). In `four_motor` builds the battery characteristic always reads `[0, 0, 0]`.

#### ST3215 Command Frames

Write exactly 6 bytes to the ST Cmd characteristic.

| Opcode | Frame                                           | Description                                      |
| ------ | ----------------------------------------------- | ------------------------------------------------ |
| `0x01` | `[0x01, id, pos_lo, pos_hi, speed_lo, speed_hi]` | Move one servo to `pos` at `speed`               |
| `0x02` | `[0x02, id, enable, 0, 0, 0]`                   | Enable torque when `enable != 0`; disable at `0` |
| `0x03` | `[0x03, current_id, new_id, 0, 0, 0]`           | Change a servo ID                                |
| `0x04` | `[0x04, id, 0, 0, 0, 0]`                        | Ping one servo                                   |
| `0x05` | `[0x05, id, 0, 0, 0, 0]`                        | Refresh ST State for one servo                   |
| `0x06` | `[0x06, from, to, 0, 0, 0]`                     | Rescan IDs from `from` to `to`                   |
| `0x07` | `[0x07, pos_lo, pos_hi, speed_lo, speed_hi, acc]` | Move all currently discovered servos             |

For opcode `0x06`, `from = 0` defaults to `1` and `to = 0` defaults to `20`. The rescan updates ST List before the write handler returns.

#### ST3215 State Frame

After writing `ST Cmd` opcode `0x05`, read ST State.

| Byte | Field          | Type | Notes                                  |
| ---- | -------------- | ---- | -------------------------------------- |
| 0    | ID             | u8   | Requested servo ID                     |
| 1    | Error          | u8   | `0` on success, `0xFF` on read failure |
| 2    | Position low   | u8   | `position & 0xFF`                      |
| 3    | Position high  | u8   | `position >> 8`                        |
| 4    | Load low       | u8   | `load & 0xFF`                          |
| 5    | Load high      | u8   | `load >> 8`                            |
| 6    | Voltage        | u8   | Servo-reported voltage                 |
| 7    | Temperature    | u8   | Servo-reported temperature             |

### Write Behavior

Writing a value to a characteristic immediately applies it. Out-of-range or malformed values are rejected — the write succeeds at the GATT level but the motor/servo state does not change, and a diagnostic message is printed to the serial log.

All write events are logged to the serial console with the handle, raw data bytes, and length for debugging.

### Read Behavior

Reading a characteristic returns the last value set by any interface (BLE, HTTP, or serial).

---

### Swift (CoreBluetooth) Integration

```swift
import CoreBluetooth

let serviceUUID    = CBUUID(string: "E3910040-4567-4321-ABCD-ABCDEF012345")
let motorsUUID     = CBUUID(string: "E3910003-4567-4321-ABCD-ABCDEF012345")
let wifiConfigUUID = CBUUID(string: "E3910004-4567-4321-ABCD-ABCDEF012345")
let batteryUUID    = CBUUID(string: "E3910006-4567-4321-ABCD-ABCDEF012345")
let stListUUID     = CBUUID(string: "E3910011-4567-4321-ABCD-ABCDEF012345")
let stCmdUUID      = CBUUID(string: "E3910012-4567-4321-ABCD-ABCDEF012345")
let stStateUUID    = CBUUID(string: "E3910013-4567-4321-ABCD-ABCDEF012345")

func setMotors(_ char: CBCharacteristic, peripheral: CBPeripheral, a: Int8, b: Int8, c: Int8 = 0, d: Int8 = 0) {
    let data = Data([UInt8(bitPattern: a), UInt8(bitPattern: b), UInt8(bitPattern: c), UInt8(bitPattern: d)])
    peripheral.writeValue(data, for: char, type: .withResponse)
}

func rescanServos(_ char: CBCharacteristic, peripheral: CBPeripheral, from: UInt8 = 1, to: UInt8 = 20) {
    peripheral.writeValue(Data([0x06, from, to, 0, 0, 0]), for: char, type: .withResponse)
}

func moveServo(_ char: CBCharacteristic, peripheral: CBPeripheral, id: UInt8, pos: UInt16, speed: UInt16) {
    let data = Data([0x01, id, UInt8(pos & 0xFF), UInt8(pos >> 8), UInt8(speed & 0xFF), UInt8(speed >> 8)])
    peripheral.writeValue(data, for: char, type: .withResponse)
}

func moveDiscoveredServos(_ char: CBCharacteristic, peripheral: CBPeripheral, pos: UInt16, speed: UInt16, acc: UInt8 = 50) {
    let data = Data([0x07, UInt8(pos & 0xFF), UInt8(pos >> 8), UInt8(speed & 0xFF), UInt8(speed >> 8), acc])
    peripheral.writeValue(data, for: char, type: .withResponse)
}
```

### Kotlin (Android BLE) Integration

```kotlin
import java.util.UUID

val SERVICE_UUID     = UUID.fromString("E3910040-4567-4321-ABCD-ABCDEF012345")
val MOTORS_UUID      = UUID.fromString("E3910003-4567-4321-ABCD-ABCDEF012345")
val WIFI_CONFIG_UUID = UUID.fromString("E3910004-4567-4321-ABCD-ABCDEF012345")
val BATTERY_UUID     = UUID.fromString("E3910006-4567-4321-ABCD-ABCDEF012345")
val ST_LIST_UUID     = UUID.fromString("E3910011-4567-4321-ABCD-ABCDEF012345")
val ST_CMD_UUID      = UUID.fromString("E3910012-4567-4321-ABCD-ABCDEF012345")
val ST_STATE_UUID    = UUID.fromString("E3910013-4567-4321-ABCD-ABCDEF012345")

fun setMotors(gatt: BluetoothGatt, char: BluetoothGattCharacteristic, a: Byte, b: Byte, c: Byte = 0, d: Byte = 0) {
    char.value = byteArrayOf(a, b, c, d)
    gatt.writeCharacteristic(char)
}

fun rescanServos(gatt: BluetoothGatt, char: BluetoothGattCharacteristic, from: Byte = 1, to: Byte = 20) {
    char.value = byteArrayOf(0x06, from, to, 0, 0, 0)
    gatt.writeCharacteristic(char)
}

fun moveServo(gatt: BluetoothGatt, char: BluetoothGattCharacteristic, id: Byte, pos: Int, speed: Int) {
    char.value = byteArrayOf(0x01, id, pos.toByte(), (pos shr 8).toByte(), speed.toByte(), (speed shr 8).toByte())
    gatt.writeCharacteristic(char)
}

fun moveDiscoveredServos(gatt: BluetoothGatt, char: BluetoothGattCharacteristic, pos: Int, speed: Int, acc: Byte = 50) {
    char.value = byteArrayOf(0x07, pos.toByte(), (pos shr 8).toByte(), speed.toByte(), (speed shr 8).toByte(), acc)
    gatt.writeCharacteristic(char)
}
```

---

## Value Ranges Summary

| Control             | Type | Min  | Max | Unit    | Notes               |
| ------------------- | ---- | ---- | --- | ------- | ------------------- |
| Motor Power         | i8   | -100 | 100 | percent | Negative = reverse  |
| ST3215 ID           | u8   | 1    | 253 | id      | Scan defaults 1-20  |
| ST3215 Position     | u16  | 0    | 4095 | ticks  | 12-bit register     |
| ST3215 Speed        | u16  | 0    | 4095 | ticks/s | 12-bit register     |
| Battery Pct         | u8   | 0    | 100 | percent | 3S LiPo estimate    |
| Battery Voltage     | u16  | 0    | -   | mV      | Big-endian in BLE   |

Both interfaces enforce the same validation. Out-of-range values return a `400` error over HTTP and are rejected or ignored over BLE with a serial log message.
