# API Reference

This document covers the two control interfaces exposed by the ESP32: a **REST API** over WiFi and a **BLE GATT** service. Both interfaces control the same servo and motors — commands from either source are handled identically.

The firmware supports two build configurations selected at compile time via Cargo features:

| Feature      | Motors | Motor IDs  | Description                     |
| ------------ | ------ | ---------- | ------------------------------- |
| `four_motor` | 4      | A, B, C, D | Default — 4 H-bridge channels   |
| `two_motor`  | 2      | A, B       | TB6612 driver with STBY standby |

Clients can query the active configuration at runtime via **`GET /config`** (HTTP) or reading the **Motor Count** BLE characteristic.

---

## REST API

The ESP32 runs an HTTP server on **port 80** after connecting to WiFi. All endpoints accept **GET** requests and return **JSON** responses.

### Base URL

```
http://<ESP32_IP>/
```

The IP address is printed to the serial monitor on boot.

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

The ESP32 advertises as a BLE peripheral named **"ESP32 Motor"**. It exposes a single custom GATT service with characteristics for each controllable output, plus WiFi configuration. WiFi and BLE run simultaneously via radio coexistence.

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
| Service UUID | `E3910010-4567-4321-ABCD-ABCDEF012345` |

> **Note:** Service UUID changed from `E3910001-...` to `E3910010-...` to force GATT cache refresh on clients after adding WiFi config characteristic.

### Characteristics

| Name        | UUID                                   | Type      | Length  | Access | Range/Format                 | Default        | Description                                     |
| ----------- | -------------------------------------- | --------- | ------- | ------ | ---------------------------- | -------------- | ----------------------------------------------- |
| Servo Angle | `E3910002-4567-4321-ABCD-ABCDEF012345` | u8        | 1 byte  | R/W/N  | 0–180                        | 90             | Servo position in degrees                       |
| Motors      | `E3910003-4567-4321-ABCD-ABCDEF012345` | `[i8; 4]` | 4 bytes | R/W/N  | -100–100 each                | `[0, 0, 0, 0]` | Motor powers (see Motor Count for active count) |
| WiFi Config | `E3910004-4567-4321-ABCD-ABCDEF012345` | bytes     | 1-97    | W      | `SSID\0PASSWORD` (see below) | —              | Set WiFi credentials, stored in flash           |
| Motor Count | `E3910005-4567-4321-ABCD-ABCDEF012345` | u8        | 1 byte  | R      | 2 or 4                       | (build-time)   | Number of active motors in this firmware build  |

#### Motor Count

Read this characteristic on connect to determine the motor configuration:

- **2** — two-motor build (TB6612): only bytes 0–1 of the Motors characteristic are used (A, B)
- **4** — four-motor build: all 4 bytes are used (A, B, C, D)

The Motors characteristic is always 4 bytes, but in two-motor mode bytes 2–3 are ignored on write and always read as 0.

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

Credentials are stored in flash and persist across reboots. After writing new credentials, the device will use them on next boot. A "WiFi Saved!" message flashes on the OLED display upon successful write.

### Data Format

- **Servo Angle**: unsigned byte (`0x00`–`0xB4`). Values above 180 are rejected.
- **Motors**: 4 signed bytes `[A, B, C, D]`. Each byte is an i8 in the range -100 to +100. Negative values = reverse. In two_motor mode only the first 2 bytes are used; writes with fewer than 2 bytes are rejected. In four_motor mode all 4 bytes are used; writes with fewer than 4 bytes are rejected.
- **Motor Count**: unsigned byte, read-only. Returns `2` or `4` depending on the firmware build.

### Write Behavior

Writing a value to a characteristic immediately applies it. Out-of-range or malformed values are rejected — the write succeeds at the GATT level but the motor/servo state does not change, and a diagnostic message is printed to the serial log.

All write events are logged to the serial console with the handle, raw data bytes, and length for debugging.

### Read Behavior

Reading a characteristic returns the last value set by any interface (BLE, HTTP, or serial).

---

### Swift (CoreBluetooth) Integration

```swift
import CoreBluetooth

// UUIDs (Service UUID updated to E3910010 for GATT cache refresh)
let serviceUUID    = CBUUID(string: "E3910010-4567-4321-ABCD-ABCDEF012345")
let servoAngleUUID = CBUUID(string: "E3910002-4567-4321-ABCD-ABCDEF012345")
let motorsUUID     = CBUUID(string: "E3910003-4567-4321-ABCD-ABCDEF012345")
let wifiConfigUUID = CBUUID(string: "E3910004-4567-4321-ABCD-ABCDEF012345")
let motorCountUUID = CBUUID(string: "E3910005-4567-4321-ABCD-ABCDEF012345")

class MotorController: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    var centralManager: CBCentralManager!
    var peripheral: CBPeripheral?
    var servoChar: CBCharacteristic?
    var motorsChar: CBCharacteristic?
    var wifiConfigChar: CBCharacteristic?
    var motorCountChar: CBCharacteristic?
    var motorCount: Int = 4  // default, updated on connect

    override init() {
        super.init()
        centralManager = CBCentralManager(delegate: self, queue: nil)
    }

    // Start scanning when Bluetooth is ready
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state == .poweredOn {
            central.scanForPeripherals(withServices: [serviceUUID])
        }
    }

    // Connect when the ESP32 is found
    func centralManager(_ central: CBCentralManager,
                        didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any],
                        rssi RSSI: NSNumber) {
        self.peripheral = peripheral
        central.stopScan()
        central.connect(peripheral)
    }

    // Discover services after connecting
    func centralManager(_ central: CBCentralManager,
                        didConnect peripheral: CBPeripheral) {
        peripheral.delegate = self
        peripheral.discoverServices([serviceUUID])
    }

    // Discover characteristics
    func peripheral(_ peripheral: CBPeripheral,
                    didDiscoverServices error: Error?) {
        guard let service = peripheral.services?.first(where: {
            $0.uuid == serviceUUID
        }) else { return }
        peripheral.discoverCharacteristics(nil, for: service)
    }

    // Store characteristic references
    func peripheral(_ peripheral: CBPeripheral,
                    didDiscoverCharacteristicsFor service: CBService,
                    error: Error?) {
        for char in service.characteristics ?? [] {
            switch char.uuid {
            case servoAngleUUID: servoChar = char
            case motorsUUID:     motorsChar = char
            case wifiConfigUUID: wifiConfigChar = char
            case motorCountUUID:
                motorCountChar = char
                // Read motor count on discovery
                peripheral.readValue(for: char)
            default: break
            }
            // Enable notifications
            if char.properties.contains(.notify) {
                peripheral.setNotifyValue(true, for: char)
            }
        }
    }

    // --- Control Methods ---

    /// Set servo angle (0-180)
    func setServoAngle(_ angle: UInt8) {
        guard let char = servoChar, let p = peripheral else { return }
        p.writeValue(Data([angle]), for: char, type: .withResponse)
    }

    /// Set all motors at once (sends motorCount bytes)
    func setMotors(a: Int8, b: Int8, c: Int8 = 0, d: Int8 = 0) {
        guard let char = motorsChar, let p = peripheral else { return }
        let data: Data
        if motorCount == 2 {
            data = Data([UInt8(bitPattern: a), UInt8(bitPattern: b)])
        } else {
            data = Data([
                UInt8(bitPattern: a),
                UInt8(bitPattern: b),
                UInt8(bitPattern: c),
                UInt8(bitPattern: d)
            ])
        }
        p.writeValue(data, for: char, type: .withResponse)
    }

    /// Configure WiFi credentials (stored in flash, used on next boot)
    func setWiFiCredentials(ssid: String, password: String) {
        guard let char = wifiConfigChar, let p = peripheral else { return }
        // Format: "SSID\0PASSWORD"
        var data = Data(ssid.utf8)
        data.append(0) // null separator
        data.append(contentsOf: password.utf8)
        p.writeValue(data, for: char, type: .withResponse)
    }

    // Handle notifications
    func peripheral(_ peripheral: CBPeripheral,
                    didUpdateValueFor characteristic: CBCharacteristic,
                    error: Error?) {
        guard let data = characteristic.value else { return }
        switch characteristic.uuid {
        case servoAngleUUID:
            if let byte = data.first {
                print("Servo angle: \(byte)")
            }
        case motorsUUID:
            let powers = data.prefix(motorCount).map { Int8(bitPattern: $0) }
            print("Motors: \(powers)")
        case motorCountUUID:
            if let byte = data.first {
                motorCount = Int(byte)
                print("Motor count: \(motorCount)")
            }
        default: break
        }
    }
}
```

### Kotlin (Android BLE) Integration

```kotlin
import android.bluetooth.*
import android.bluetooth.le.*
import java.util.UUID

// UUIDs (Service UUID updated to E3910010 for GATT cache refresh)
val SERVICE_UUID     = UUID.fromString("E3910010-4567-4321-ABCD-ABCDEF012345")
val SERVO_ANGLE_UUID = UUID.fromString("E3910002-4567-4321-ABCD-ABCDEF012345")
val MOTORS_UUID      = UUID.fromString("E3910003-4567-4321-ABCD-ABCDEF012345")
val WIFI_CONFIG_UUID = UUID.fromString("E3910004-4567-4321-ABCD-ABCDEF012345")
val MOTOR_COUNT_UUID = UUID.fromString("E3910005-4567-4321-ABCD-ABCDEF012345")

// Scan for the ESP32
val scanner = bluetoothAdapter.bluetoothLeScanner
val filter  = ScanFilter.Builder().setServiceUuid(ParcelUuid(SERVICE_UUID)).build()
val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build()

scanner.startScan(listOf(filter), settings, object : ScanCallback() {
    override fun onScanResult(callbackType: Int, result: ScanResult) {
        val device = result.device
        device.connectGatt(context, false, gattCallback)
    }
})

// GATT callback
val gattCallback = object : BluetoothGattCallback() {
    override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
        if (newState == BluetoothProfile.STATE_CONNECTED) {
            gatt.discoverServices()
        }
    }

    override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
        val service = gatt.getService(SERVICE_UUID)

        // Read motor count first to determine configuration
        val motorCountChar = service.getCharacteristic(MOTOR_COUNT_UUID)
        gatt.readCharacteristic(motorCountChar)
        // motorCount will be available in onCharacteristicRead callback

        // Write servo angle
        val servoChar = service.getCharacteristic(SERVO_ANGLE_UUID)
        servoChar.value = byteArrayOf(90.toByte())
        gatt.writeCharacteristic(servoChar)

        // Write motors (2-motor: send 2 bytes, 4-motor: send 4 bytes)
        val motorsChar = service.getCharacteristic(MOTORS_UUID)
        // Example for 2-motor mode:
        motorsChar.value = byteArrayOf(
            75.toByte(),    // Motor A: forward 75%
            (-50).toByte()  // Motor B: reverse 50%
        )
        // Example for 4-motor mode:
        // motorsChar.value = byteArrayOf(
        //     75.toByte(), 75.toByte(), (-50).toByte(), (-50).toByte()
        // )
        gatt.writeCharacteristic(motorsChar)

        // Configure WiFi credentials (stored in flash, used on next boot)
        val wifiChar = service.getCharacteristic(WIFI_CONFIG_UUID)
        val ssid = "MyNetwork"
        val password = "secret123"
        wifiChar.value = (ssid + "\u0000" + password).toByteArray(Charsets.UTF_8)
        gatt.writeCharacteristic(wifiChar)
    }
}
```

---

## Value Ranges Summary

| Control     | Type | Min  | Max | Unit    | Notes              |
| ----------- | ---- | ---- | --- | ------- | ------------------ |
| Servo Angle | u8   | 0    | 180 | degrees | Unsigned byte      |
| Motor Power | i8   | -100 | 100 | percent | Negative = reverse |

Both interfaces enforce the same validation. Out-of-range values return a `400` error over HTTP and are rejected (with a serial log message) over BLE.
