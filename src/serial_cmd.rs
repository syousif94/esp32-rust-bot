use esp_println::println;
use esp_hal::uart::Uart;
use esp_hal::Blocking;
use embassy_time::{Duration, Timer};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

/// Signal for servo angle updates from serial
pub static SERIAL_SERVO_ANGLE: Signal<CriticalSectionRawMutex, u8> = Signal::new();

/// Signal for motor A power updates from serial
pub static SERIAL_MOTOR_A_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor B power updates from serial
pub static SERIAL_MOTOR_B_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor C power updates from serial
pub static SERIAL_MOTOR_C_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor D power updates from serial
pub static SERIAL_MOTOR_D_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Parsed command from serial input
enum SerialCommand {
    Servo(u8),
    MotorA(i8),
    MotorB(i8),
    MotorC(i8),
    MotorD(i8),
    MotorAll(i8),
}

/// Parse a servo command from input
/// Accepts formats like: "90", "servo 90", "angle 90", "s90", "a90"
fn parse_servo_command(input: &str) -> Option<u8> {
    let input = input.trim();
    
    // Try direct number (only positive, for servo)
    if let Ok(angle) = input.parse::<u8>() {
        if angle <= 180 {
            return Some(angle);
        }
    }
    
    // Try "servo X" or "s X" or "sX"
    for prefix in ["servo ", "angle ", "s ", "a ", "s", "a"] {
        if let Some(rest) = input.strip_prefix(prefix) {
            if let Ok(angle) = rest.trim().parse::<u8>() {
                if angle <= 180 {
                    return Some(angle);
                }
            }
        }
    }
    
    None
}

/// Parse a motor command from input
/// Accepts formats like: "ma 50", "mb -75", "mc 100", "md -50", "motor a 100", "motor b -50", "m 50" (all motors)
fn parse_motor_command(input: &str) -> Option<(char, i8)> {
    let input = input.trim().to_lowercase();
    
    // Try "m X" format for all motors (must check before "ma"/"mb"/"mc"/"md")
    // Make sure it's not "ma", "mb", "mc", or "md"
    if let Some(rest) = input.strip_prefix("m ") {
        if let Ok(power) = rest.trim().parse::<i8>() {
            if power >= -100 && power <= 100 {
                return Some(('*', power)); // '*' means all motors
            }
        }
    }
    
    // Try "ma X", "mb X", "mc X", "md X" format
    for (prefix, motor_id) in [("ma ", 'a'), ("mb ", 'b'), ("mc ", 'c'), ("md ", 'd')] {
        if let Some(rest) = input.strip_prefix(prefix) {
            if let Ok(power) = rest.trim().parse::<i8>() {
                if power >= -100 && power <= 100 {
                    return Some((motor_id, power));
                }
            }
        }
    }
    
    // Try "motor a X", "motor b X", "motor c X", "motor d X" format
    if let Some(rest) = input.strip_prefix("motor ") {
        let rest = rest.trim();
        for (prefix, motor_id) in [("a ", 'a'), ("b ", 'b'), ("c ", 'c'), ("d ", 'd')] {
            if let Some(power_str) = rest.strip_prefix(prefix) {
                if let Ok(power) = power_str.trim().parse::<i8>() {
                    if power >= -100 && power <= 100 {
                        return Some((motor_id, power));
                    }
                }
            }
        }
    }
    
    None
}

/// Parse any serial command
fn parse_command(input: &str) -> Option<SerialCommand> {
    // Try motor command first (to avoid "ma" being parsed as servo)
    if let Some((motor, power)) = parse_motor_command(input) {
        return match motor {
            'a' => Some(SerialCommand::MotorA(power)),
            'b' => Some(SerialCommand::MotorB(power)),
            'c' => Some(SerialCommand::MotorC(power)),
            'd' => Some(SerialCommand::MotorD(power)),
            '*' => Some(SerialCommand::MotorAll(power)),
            _ => None,
        };
    }
    
    // Try servo command
    if let Some(angle) = parse_servo_command(input) {
        return Some(SerialCommand::Servo(angle));
    }
    
    None
}

/// Task to read serial input and parse servo/motor commands
#[embassy_executor::task]
pub async fn serial_input_task(mut uart: Uart<'static, Blocking>) {
    println!("Serial command interface ready");
    println!("  Servo:  <angle> or 'servo <angle>' (0-180)");
    println!("  Motor:  'm <power>' (all), 'ma/mb/mc/md <power>' (-100 to 100)");
    println!("  Examples: 90, m 50, ma 75, mb -50, mc 100, md -25");
    
    let mut buffer = [0u8; 64];
    let mut pos = 0usize;
    let mut read_buf = [0u8; 1];
    
    loop {
        // Check if data is available (non-blocking check)
        if uart.read_ready() {
            // Try to read a byte
            match uart.read(&mut read_buf) {
                Ok(1) => {
                    let byte = read_buf[0];
                    
                    // Echo the character back
                    let _ = uart.write(&[byte]);
                    
                    if byte == b'\r' || byte == b'\n' {
                        if pos > 0 {
                            // Try to parse the command
                            if let Ok(cmd) = core::str::from_utf8(&buffer[..pos]) {
                                match parse_command(cmd) {
                                    Some(SerialCommand::Servo(angle)) => {
                                        println!("\nSerial: Setting servo to {} degrees", angle);
                                        SERIAL_SERVO_ANGLE.signal(angle);
                                    }
                                    Some(SerialCommand::MotorA(power)) => {
                                        println!("\nSerial: Setting motor A to {}%", power);
                                        SERIAL_MOTOR_A_POWER.signal(power);
                                    }
                                    Some(SerialCommand::MotorB(power)) => {
                                        println!("\nSerial: Setting motor B to {}%", power);
                                        SERIAL_MOTOR_B_POWER.signal(power);
                                    }
                                    Some(SerialCommand::MotorC(power)) => {
                                        println!("\nSerial: Setting motor C to {}%", power);
                                        SERIAL_MOTOR_C_POWER.signal(power);
                                    }
                                    Some(SerialCommand::MotorD(power)) => {
                                        println!("\nSerial: Setting motor D to {}%", power);
                                        SERIAL_MOTOR_D_POWER.signal(power);
                                    }
                                    Some(SerialCommand::MotorAll(power)) => {
                                        println!("\nSerial: Setting all motors to {}%", power);
                                        SERIAL_MOTOR_A_POWER.signal(power);
                                        SERIAL_MOTOR_B_POWER.signal(power);
                                        SERIAL_MOTOR_C_POWER.signal(power);
                                        SERIAL_MOTOR_D_POWER.signal(power);
                                    }
                                    None => {
                                        if !cmd.trim().is_empty() {
                                            println!("\nUnknown command: '{}'. Use 0-180 for servo, 'm/ma/mb/mc/md <-100 to 100>' for motors.", cmd);
                                        }
                                    }
                                }
                            }
                            pos = 0;
                        }
                        println!("");
                    } else if pos < buffer.len() - 1 {
                        buffer[pos] = byte;
                        pos += 1;
                    }
                }
                _ => {}
            }
        } else {
            // No data available, yield to other tasks
            Timer::after(Duration::from_millis(10)).await;
        }
    }
}
