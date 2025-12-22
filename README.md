# ESP32 HTTP Servo & Motor Controller

Control an SG90 servo motor and brushless DC motors via HTTP requests or serial commands using an ESP32 microcontroller, written in Rust with `no_std` embedded development.

## Features

- **HTTP Control**: Set servo angle via GET requests (`/servo/90` or `/servo?angle=90`)
- **Motor Control**: Control brushless motors via H-bridge (`/motor/a/50` or `/motor/b/-75`)
- **Serial Control**: Type angle values directly in the serial monitor
- **WiFi Connected**: Connects to your WiFi network and serves HTTP on port 80
- **Async Runtime**: Uses Embassy for efficient async/await embedded programming

## Hardware Requirements

- **ESP32** development board (tested with ESP32-WROOM)
- **SG90 Servo Motor** (or compatible PWM servo)
- **Brushless DC Motors** with H-bridge driver (e.g., L298N, TB6612FNG)

### Wiring

#### Servo

| Servo Wire             | Connection              |
| ---------------------- | ----------------------- |
| Red (VCC)              | 3.3/5V power supply     |
| Brown/Black (GND)      | GND (shared with ESP32) |
| Orange/Yellow (Signal) | GPIO18                  |

#### Brushless Motors (H-Bridge)

| Motor   | Forward Pin | Reverse Pin | Description                  |
| ------- | ----------- | ----------- | ---------------------------- |
| Motor A | GPIO32      | GPIO33      | First motor H-bridge inputs  |
| Motor B | GPIO25      | GPIO26      | Second motor H-bridge inputs |

Connect your H-bridge driver's input pins to the ESP32 GPIOs, and motor outputs to your brushless motors. Ensure proper power supply for the motors through the H-bridge.

## Software Requirements

### Install Rust and ESP32 Toolchain

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install espup (ESP32 Rust toolchain installer)
cargo install espup

# Install the ESP32 toolchain
espup install

# Source the environment (add to your shell profile)
source ~/export-esp.sh

# Install cargo-espflash for flashing
cargo install cargo-espflash
```

### Configure WiFi

Edit `cfg.toml` with your WiFi credentials:

```toml
wifi_ssid = "YourNetworkName"
wifi_password = "YourPassword"
```

## Building and Flashing

```bash
cargo espflash flash --monitor
```

## Usage

### HTTP Control

Once connected, the ESP32 will print its IP address. Use curl or a browser:

```bash
# Servo Control
# Move to 90 degrees (center)
curl http://192.168.x.x/servo/90

# Move to 0 degrees
curl http://192.168.x.x/servo/0

# Move to 180 degrees
curl http://192.168.x.x/servo/180

# Alternative query string format
curl http://192.168.x.x/servo?angle=45

# Motor Control (H-Bridge)
# Motor A forward at 75% power
curl http://192.168.x.x/motor/a/75

# Motor A reverse at 50% power
curl http://192.168.x.x/motor/a/-50

# Motor B forward at 100% power
curl http://192.168.x.x/motor/b/100

# Stop Motor A
curl http://192.168.x.x/motor/a/0

# Alternative query string format
curl http://192.168.x.x/motor/a?power=80

# Check server status
curl http://192.168.x.x/
```

**Response format** (JSON):

```json
{ "angle": 90 }
{ "motor": "a", "power": 75 }
```

### Serial Control

While connected via `cargo espflash flash --monitor`, type commands directly:

```
# Servo commands
90           # Move servo to 90 degrees
0            # Move servo to 0 degrees
180          # Move servo to 180 degrees
servo 45     # Also works

# Motor commands
ma 50        # Motor A forward at 50%
ma -75       # Motor A reverse at 75%
mb 100       # Motor B forward at 100%
mb 0         # Stop Motor B
motor a 80   # Alternative format
motor b -50  # Alternative format
```

## Project Structure

```
src/
├── bin/
│   └── main.rs        # Entry point, WiFi setup, main loop
├── lib.rs             # Library root
├── brushless.rs       # H-bridge brushless motor control using LEDC
├── http_server.rs     # HTTP server and request handling
├── serial_cmd.rs      # Serial command parsing
└── servo.rs           # PWM servo control using LEDC
```

## How It Works

### Servo Control (`servo.rs`)

The servo is controlled using the ESP32's **LEDC (LED Control)** peripheral, which generates PWM signals:

- **Frequency**: 50 Hz (20ms period) - standard for hobby servos
- **Duty Cycle**:
  - 0° → 0.5ms pulse (2.5% duty)
  - 90° → 1.5ms pulse (7.5% duty)
  - 180° → 2.5ms pulse (12.5% duty)
- **Resolution**: 14-bit for precise angle control
- **Timer**: HighSpeed LEDC Timer0 with 80MHz APB clock

### Brushless Motor Control (`brushless.rs`)

Brushless motors are controlled via an H-bridge driver using two PWM channels per motor:

- **Frequency**: 1 kHz - good for responsive H-bridge motor control
- **Direction Control**:
  - Forward: Pin A = PWM duty, Pin B = 0%
  - Reverse: Pin A = 0%, Pin B = PWM duty
  - Brake: Both pins = 0%
- **Power Range**: -100% (full reverse) to +100% (full forward)
- **Resolution**: 14-bit for smooth speed control
- **Timer**: HighSpeed LEDC Timer1 (separate from servo)

| Motor   | LEDC Channels | GPIO Pins  |
| ------- | ------------- | ---------- |
| Motor A | Channel 1, 2  | GPIO32, 33 |
| Motor B | Channel 3, 4  | GPIO25, 26 |

### HTTP Server (`http_server.rs`)

A simple async TCP server running on port 80:

1. Accepts TCP connections
2. Parses HTTP GET requests
3. Extracts angle from URL path or query string
4. Signals the main loop via `embassy_sync::Signal`
5. Returns JSON response

**Endpoints**:

- `GET /` - Server status
- `GET /health` - Health check
- `GET /servo/<angle>` - Set servo angle (0-180)
- `GET /servo?angle=<angle>` - Alternative format
- `GET /motor/a/<power>` - Set Motor A power (-100 to 100)
- `GET /motor/b/<power>` - Set Motor B power (-100 to 100)
- `GET /motor/a?power=<power>` - Alternative format
- `GET /motor/b?power=<power>` - Alternative format

### Serial Commands (`serial_cmd.rs`)

Polls UART0 for input and parses servo and motor commands:

- Runs as an Embassy task
- Non-blocking read with `read_ready()` check
- Echoes characters back to terminal

**Servo formats**: `<angle>`, `servo <angle>`, `s <angle>`

**Motor formats**: `ma <power>`, `mb <power>`, `motor a <power>`, `motor b <power>`

### Main Loop (`main.rs`)

1. Initializes peripherals (LEDC, UART, WiFi)
2. Sets up servo on GPIO18 (LEDC Timer0, Channel0)
3. Sets up Motor A on GPIO32/33 (LEDC Timer1, Channel1/2)
4. Sets up Motor B on GPIO25/26 (LEDC Timer1, Channel3/4)
5. Connects to WiFi network
6. Spawns background tasks:
   - WiFi connection manager
   - Network stack runner
   - HTTP server
   - Serial command handler
7. Main loop uses nested `select` combinators to wait for updates from HTTP or serial (6 signal sources), then controls servo/motors

## Async Execution Model

### Is the main loop executing every tick?

**No.** The main loop is _not_ polling or running on a fixed tick. It **sleeps** until woken by an event.

```rust
loop {
    match select(
        select4(
            SERVO_ANGLE.wait(),
            SERIAL_SERVO_ANGLE.wait(),
            MOTOR_A_POWER.wait(),
            MOTOR_B_POWER.wait(),
        ),
        select(
            SERIAL_MOTOR_A_POWER.wait(),
            SERIAL_MOTOR_B_POWER.wait(),
        ),
    ).await {
        Either::First(Either4::First(angle)) => servo.set_angle(angle),
        Either::First(Either4::Second(angle)) => servo.set_angle(angle),
        Either::First(Either4::Third(power)) => motor_a.set_power(power),
        Either::First(Either4::Fourth(power)) => motor_b.set_power(power),
        Either::Second(Either::First(power)) => motor_a.set_power(power),
        Either::Second(Either::Second(power)) => motor_b.set_power(power),
    }
}
```

When execution hits `.await`, the task **yields** and the CPU can sleep or run other tasks. The main task only wakes when:

- The HTTP server signals a new servo angle via `SERVO_ANGLE.signal(angle)`
- The serial handler signals servo via `SERIAL_SERVO_ANGLE.signal(angle)`
- The HTTP server signals motor power via `MOTOR_A_POWER.signal(power)` or `MOTOR_B_POWER.signal(power)`
- The serial handler signals motor power via `SERIAL_MOTOR_A_POWER.signal(power)` or `SERIAL_MOTOR_B_POWER.signal(power)`

### How Nested `select()` Works

We use nested `select` combinators to handle 6 signal sources (more than `select4` supports):

1. All six signal `.wait()` futures are polled concurrently via nested selects
2. When **any one** completes, the nested select returns immediately with that result
3. The other futures are dropped (but their signals remain for next iteration)

This is **not busy-polling**. If no signal is ready, the executor puts the task to sleep.

### Embassy Executor Model

Embassy uses a **single-threaded, cooperative, interrupt-driven** scheduler:

| Concept              | Description                                           |
| -------------------- | ----------------------------------------------------- |
| **Cooperative**      | Tasks voluntarily yield at `.await` points            |
| **Single-threaded**  | One task runs at a time (no preemption between tasks) |
| **Interrupt-driven** | Hardware interrupts wake sleeping tasks               |
| **No fixed tick**    | Wakeups happen on-demand, not periodically            |

**Task lifecycle:**

1. Task runs until it hits `.await` on a pending future
2. Task yields to executor and goes to sleep
3. Hardware interrupt fires (timer, UART RX, WiFi packet, etc.)
4. Interrupt handler calls `Waker::wake()` to mark task as ready
5. Executor polls the task again

### Example Flow: HTTP Request → Servo Movement

```
1. WiFi packet arrives       → Hardware interrupt
2. Interrupt wakes net_task  → Executor runs net_task
3. net_task processes packet → Wakes http_server_task
4. HTTP server parses URL    → Calls SERVO_ANGLE.signal(90)
5. signal() wakes main task  → Executor runs main task
6. select() returns angle    → servo.set_angle(90)
7. Main task loops, awaits   → Goes back to sleep
```

The CPU spends most of its time **sleeping**. It wakes only for interrupts, does minimal work, then sleeps again. This is extremely power-efficient.

### RTOS Timing

There is no fixed "tick rate" like traditional RTOS (e.g., FreeRTOS 1ms tick). Instead:

- **Timers**: `Timer::after(Duration::from_millis(500))` sets a hardware timer interrupt; the executor sleeps until it fires
- **Signals**: Wake immediately when `signal()` is called from another task
- **I/O**: UART/WiFi interrupts wake tasks when data arrives

The ESP32's timer peripheral runs at **80 MHz APB clock**, providing microsecond-level precision for timing operations.

## Dependencies

Key crates used:

| Crate              | Purpose                              |
| ------------------ | ------------------------------------ |
| `esp-hal`          | Hardware abstraction layer for ESP32 |
| `esp-radio`        | WiFi driver                          |
| `esp-rtos`         | Embassy integration for ESP32        |
| `embassy-net`      | Async TCP/IP networking              |
| `embassy-executor` | Async task executor                  |
| `embassy-sync`     | Async synchronization primitives     |
| `embassy-time`     | Async timers and delays              |
