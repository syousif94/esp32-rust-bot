//! Unified command channel for actuator control.
//!
//! All input sources (HTTP, serial, BLE) send commands through a single
//! channel, which the main loop consumes to drive servos and motors.

use core::sync::atomic::{AtomicU8, AtomicU16};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use crate::st3215::MAX_SERVOS;

/// Battery voltage in millivolts (set by INA219 task, read by HTTP/BLE)
pub static BATTERY_MV: AtomicU16 = AtomicU16::new(0);
/// Battery percentage 0-100 (set by INA219 task, read by HTTP/BLE)
pub static BATTERY_PCT: AtomicU8 = AtomicU8::new(0);

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
    /// Set a single motor's power (–100 to +100)
    Motor(MotorId, i8),
    /// Set all motors at once
    MotorsAll([i8; MOTOR_COUNT]),
    /// Move a single ST3215 bus servo
    St3215Move {
        id: u8,
        pos: u16,
        speed: u16,
        acc: u8,
    },
    /// Atomic SYNC_WRITE multi-servo move.
    /// `count` valid entries from `moves`. Speed/acc shared across all.
    St3215MoveAll {
        count: u8,
        moves: [(u8, u16); MAX_SERVOS],
        speed: u16,
        acc: u8,
    },
    /// Enable/disable torque on a single servo
    St3215Torque { id: u8, enable: bool },
    /// Change a servo's ID (EEPROM write). Triggers an auto-rescan on success.
    St3215SetId { current: u8, new: u8 },
    /// Ping a single servo and log the result.
    St3215Ping { id: u8 },
    /// Re-scan the bus and refresh the shared servo list.
    St3215Rescan { from: u8, to: u8 },
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
