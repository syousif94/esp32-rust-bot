//! Unified command channel for actuator control.
//!
//! All input sources (HTTP, serial, BLE) send commands through a single
//! channel, which the main loop consumes to drive servos and motors.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

/// Number of motors in this build configuration
#[cfg(feature = "four_motor")]
pub const MOTOR_COUNT: usize = 4;
#[cfg(feature = "two_motor")]
pub const MOTOR_COUNT: usize = 2;

/// Motor identifier
#[derive(Debug, Clone, Copy)]
pub enum MotorId {
    A = 0,
    B = 1,
    #[cfg(feature = "four_motor")]
    C = 2,
    #[cfg(feature = "four_motor")]
    D = 3,
}

/// A command sent from any input source to control an actuator
#[derive(Debug, Clone, Copy)]
pub enum Command {
    /// Set servo angle (0–180 degrees)
    Servo(u8),
    /// Set a single motor's power (–100 to +100)
    Motor(MotorId, i8),
    /// Set all motors at once
    MotorsAll([i8; MOTOR_COUNT]),
}

/// Global command channel — all input tasks send here, main loop receives.
/// Depth of 8 gives plenty of headroom without wasting RAM.
pub static COMMANDS: Channel<CriticalSectionRawMutex, Command, 8> = Channel::new();

/// Send a command, draining any stale queued commands first so only the
/// latest value is delivered.
pub fn send_command(cmd: Command) {
    while COMMANDS.try_receive().is_ok() {}
    let _ = COMMANDS.try_send(cmd);
}
