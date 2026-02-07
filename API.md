# API Reference

This document covers the two control interfaces exposed by the ESP32: a **REST API** over WiFi and a **BLE GATT** service. Both interfaces control the same servo and motors — commands from either source are handled identically.

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

#### `GET /motors?a=<power>&b=<power>&c=<power>&d=<power>`

Set power for one or more motors in a single request. All parameters are optional — only the motors included in the query string are updated.

| Parameter | Type | Range    | Description                        |
| --------- | ---- | -------- | ---------------------------------- |
| `a`       | i8   | -100–100 | Motor A power (negative = reverse) |
| `b`       | i8   | -100–100 | Motor B power (negative = reverse) |
| `c`       | i8   | -100–100 | Motor C power (negative = reverse) |
| `d`       | i8   | -100–100 | Motor D power (negative = reverse) |

**Examples**

```bash
# Set all four motors
curl http://192.168.1.100/motors?a=75&b=75&c=-50&d=-50

# Set only left side (A & C)
curl http://192.168.1.100/motors?a=60&c=60

# Stop all motors
curl http://192.168.1.100/motors?a=0&b=0&c=0&d=0
```

**Response** `200 OK`

```json
{ "a": 75, "b": 75, "c": -50, "d": -50 }
```

Motors not included in the request are returned as `null`:

```json
{ "a": 60, "b": null, "c": 60, "d": null }
```

#### `GET /motor/<id>/<power>`

Set a single motor's power using a path parameter.

| Parameter | Type | Values             | Description                      |
| --------- | ---- | ------------------ | -------------------------------- |
| `id`      | char | `a`, `b`, `c`, `d` | Motor identifier                 |
| `power`   | i8   | -100 to 100        | Power level (negative = reverse) |

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

The ESP32 advertises as a BLE peripheral named **"ESP32 Motor"**. It exposes a single custom GATT service with characteristics for each controllable output. WiFi and BLE run simultaneously via radio coexistence.

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
| Service UUID | `E3910001-4567-4321-ABCD-ABCDEF012345` |

### Characteristics

Both characteristics support **Read**, **Write**, and **Notify**.

| Name        | UUID                                   | Type      | Length  | Range         | Default        | Description                        |
| ----------- | -------------------------------------- | --------- | ------- | ------------- | -------------- | ---------------------------------- |
| Servo Angle | `E3910002-4567-4321-ABCD-ABCDEF012345` | u8        | 1 byte  | 0–180         | 90             | Servo position in degrees          |
| Motors      | `E3910003-4567-4321-ABCD-ABCDEF012345` | `[i8; 4]` | 4 bytes | -100–100 each | `[0, 0, 0, 0]` | Motor A, B, C, D powers (in order) |

#### Motors Byte Layout

| Byte | Motor | Type | Range    |
| ---- | ----- | ---- | -------- |
| 0    | A     | i8   | -100–100 |
| 1    | B     | i8   | -100–100 |
| 2    | C     | i8   | -100–100 |
| 3    | D     | i8   | -100–100 |

Writes with fewer than 4 bytes are ignored. All four motor powers are updated atomically in a single BLE write.

### Data Format

- **Servo Angle**: unsigned byte (`0x00`–`0xB4`). Values above 180 are rejected.
- **Motors**: 4 signed bytes `[A, B, C, D]`. Each byte is an i8 in the range -100 to +100. Negative values = reverse. Writes with fewer than 4 bytes are rejected.

### Write Behavior

Writing a value to a characteristic immediately applies it. Out-of-range or malformed values are rejected — the write succeeds at the GATT level but the motor/servo state does not change, and a diagnostic message is printed to the serial log.

All write events are logged to the serial console with the handle, raw data bytes, and length for debugging.

### Read Behavior

Reading a characteristic returns the last value set by any interface (BLE, HTTP, or serial).

---

### Swift (CoreBluetooth) Integration

```swift
import CoreBluetooth

// UUIDs
let serviceUUID    = CBUUID(string: "E3910001-4567-4321-ABCD-ABCDEF012345")
let servoAngleUUID = CBUUID(string: "E3910002-4567-4321-ABCD-ABCDEF012345")
let motorsUUID     = CBUUID(string: "E3910003-4567-4321-ABCD-ABCDEF012345")

class MotorController: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    var centralManager: CBCentralManager!
    var peripheral: CBPeripheral?
    var servoChar: CBCharacteristic?
    var motorsChar: CBCharacteristic?

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

    /// Set all motors at once (4 bytes: A, B, C, D)
    func setMotors(a: Int8, b: Int8, c: Int8, d: Int8) {
        guard let char = motorsChar, let p = peripheral else { return }
        let data = Data([
            UInt8(bitPattern: a),
            UInt8(bitPattern: b),
            UInt8(bitPattern: c),
            UInt8(bitPattern: d)
        ])
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
            if data.count >= 4 {
                let powers = data.prefix(4).map { Int8(bitPattern: $0) }
                print("Motors: A=\(powers[0]) B=\(powers[1]) C=\(powers[2]) D=\(powers[3])")
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

val SERVICE_UUID     = UUID.fromString("E3910001-4567-4321-ABCD-ABCDEF012345")
val SERVO_ANGLE_UUID = UUID.fromString("E3910002-4567-4321-ABCD-ABCDEF012345")
val MOTORS_UUID      = UUID.fromString("E3910003-4567-4321-ABCD-ABCDEF012345")

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

        // Write servo angle
        val servoChar = service.getCharacteristic(SERVO_ANGLE_UUID)
        servoChar.value = byteArrayOf(90.toByte())
        gatt.writeCharacteristic(servoChar)

        // Write all motors at once
        val motorsChar = service.getCharacteristic(MOTORS_UUID)
        motorsChar.value = byteArrayOf(
            75.toByte(),    // Motor A: forward 75%
            75.toByte(),    // Motor B: forward 75%
            (-50).toByte(), // Motor C: reverse 50%
            (-50).toByte()  // Motor D: reverse 50%
        )
        gatt.writeCharacteristic(motorsChar)
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
