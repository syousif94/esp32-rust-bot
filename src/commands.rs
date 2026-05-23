//! Unified command channel for actuator control.
//!
//! All input sources (HTTP, serial, BLE) send commands through a single
//! channel, which the main loop consumes to drive servos and motors.

use core::sync::atomic::{AtomicU8, AtomicU16, Ordering};
use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};

use crate::st3215::MAX_SERVOS;

/// Battery voltage in millivolts (set by INA219 task, read by HTTP/BLE)
pub static BATTERY_MV: AtomicU16 = AtomicU16::new(0);
/// Battery percentage 0-100 (set by INA219 task, read by HTTP/BLE)
pub static BATTERY_PCT: AtomicU8 = AtomicU8::new(0);
static BATTERY_SAMPLE_REQUEST: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static BATTERY_SAMPLE_REQUESTS: AtomicU16 = AtomicU16::new(0);
static BATTERY_SAMPLE_COMPLETIONS: AtomicU16 = AtomicU16::new(0);

fn generation_reached(current: u16, target: u16) -> bool {
    current == target || current.wrapping_sub(target) < 0x8000
}

async fn wait_battery_sample(target: u16) {
    while !generation_reached(BATTERY_SAMPLE_COMPLETIONS.load(Ordering::Relaxed), target) {
        Timer::after(Duration::from_millis(5)).await;
    }
}

/// Request a fresh battery sample and wait briefly for the INA219 task to update the cache.
pub async fn request_battery_sample(timeout: Duration) {
    let target = BATTERY_SAMPLE_REQUESTS
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    BATTERY_SAMPLE_REQUEST.signal(());
    let _ = select(wait_battery_sample(target), Timer::after(timeout)).await;
}

/// Wait until a battery sample is requested by HTTP or BLE.
pub async fn wait_for_battery_sample_request() {
    BATTERY_SAMPLE_REQUEST.wait().await;
}

/// Mark all currently requested battery samples complete.
pub fn complete_battery_sample_request() {
    let requested = BATTERY_SAMPLE_REQUESTS.load(Ordering::Relaxed);
    BATTERY_SAMPLE_COMPLETIONS.store(requested, Ordering::Relaxed);
}

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
