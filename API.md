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

### Service

| Field        | Value                                  |
| ------------ | -------------------------------------- |
| Service UUID | `E3910001-4567-4321-ABCD-ABCDEF012345` |

### Characteristics

All characteristics support **Read**, **Write**, and **Notify**.

| Name        | UUID                                   | Type | Range    | Default | Description               |
| ----------- | -------------------------------------- | ---- | -------- | ------- | ------------------------- |
| Servo Angle | `E3910002-4567-4321-ABCD-ABCDEF012345` | u8   | 0–180    | 90      | Servo position in degrees |
| Motor A     | `E3910003-4567-4321-ABCD-ABCDEF012345` | i8   | -100–100 | 0       | Motor A power (signed)    |
| Motor B     | `E3910004-4567-4321-ABCD-ABCDEF012345` | i8   | -100–100 | 0       | Motor B power (signed)    |
| Motor C     | `E3910005-4567-4321-ABCD-ABCDEF012345` | i8   | -100–100 | 0       | Motor C power (signed)    |
| Motor D     | `E3910006-4567-4321-ABCD-ABCDEF012345` | i8   | -100–100 | 0       | Motor D power (signed)    |

### Data Format

Each characteristic holds a single byte:

- **Servo Angle**: unsigned byte (`0x00`–`0xB4`). Values above 180 are ignored.
- **Motor Power**: signed byte (`0x9C`–`0x64`, i.e. -100 to +100). Negative values = reverse. Values outside the range are ignored.

### Write Behavior

Writing a value to a characteristic immediately applies it. Out-of-range values are silently ignored (the write succeeds at the GATT level but the motor/servo state does not change).

### Read Behavior

Reading a characteristic returns the last value set by any interface (BLE, HTTP, or serial).

---

### Swift (CoreBluetooth) Integration

```swift
import CoreBluetooth

// UUIDs
let serviceUUID        = CBUUID(string: "E3910001-4567-4321-ABCD-ABCDEF012345")
let servoAngleUUID     = CBUUID(string: "E3910002-4567-4321-ABCD-ABCDEF012345")
let motorAUUID         = CBUUID(string: "E3910003-4567-4321-ABCD-ABCDEF012345")
let motorBUUID         = CBUUID(string: "E3910004-4567-4321-ABCD-ABCDEF012345")
let motorCUUID         = CBUUID(string: "E3910005-4567-4321-ABCD-ABCDEF012345")
let motorDUUID         = CBUUID(string: "E3910006-4567-4321-ABCD-ABCDEF012345")

class MotorController: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    var centralManager: CBCentralManager!
    var peripheral: CBPeripheral?
    var servoChar: CBCharacteristic?
    var motorAChar: CBCharacteristic?

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
            case motorAUUID:     motorAChar = char
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

    /// Set motor power (-100 to 100)
    func setMotorAPower(_ power: Int8) {
        guard let char = motorAChar, let p = peripheral else { return }
        p.writeValue(Data([UInt8(bitPattern: power)]), for: char, type: .withResponse)
    }

    // Handle notifications
    func peripheral(_ peripheral: CBPeripheral,
                    didUpdateValueFor characteristic: CBCharacteristic,
                    error: Error?) {
        guard let data = characteristic.value, let byte = data.first else { return }
        switch characteristic.uuid {
        case servoAngleUUID:
            print("Servo angle: \(byte)")
        case motorAUUID:
            let power = Int8(bitPattern: byte)
            print("Motor A power: \(power)")
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
val MOTOR_A_UUID     = UUID.fromString("E3910003-4567-4321-ABCD-ABCDEF012345")

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
        val servoChar = service.getCharacteristic(SERVO_ANGLE_UUID)

        // Write servo angle
        servoChar.value = byteArrayOf(90.toByte())
        gatt.writeCharacteristic(servoChar)

        // Write motor power (signed byte)
        val motorChar = service.getCharacteristic(MOTOR_A_UUID)
        motorChar.value = byteArrayOf(75.toByte()) // forward 75%
        gatt.writeCharacteristic(motorChar)
    }
}
```

---

## Value Ranges Summary

| Control     | Type | Min  | Max | Unit    | Notes              |
| ----------- | ---- | ---- | --- | ------- | ------------------ |
| Servo Angle | u8   | 0    | 180 | degrees | Unsigned byte      |
| Motor Power | i8   | -100 | 100 | percent | Negative = reverse |

Both interfaces enforce the same validation. Out-of-range values return a `400` error over HTTP and are silently ignored over BLE.
