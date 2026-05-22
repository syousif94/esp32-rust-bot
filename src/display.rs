//! OLED Display module for 128x64 I2C SSD1306 display
//!
//! Displays WiFi network, IP address, and motor power levels.

use core::fmt::Write;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Sender, Watch};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    geometry::Size,
    mono_font::{MonoTextStyleBuilder, ascii::FONT_6X10},
    pixelcolor::BinaryColor,
    prelude::*,
    text::Text,
};
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use esp_println::println;
use ssd1306::{I2CDisplayInterface, Ssd1306, prelude::*};

/// Number of receivers for Watch
const WATCH_RECEIVERS: usize = 2;

/// Type alias for the display state sender
pub type DisplaySender = Sender<'static, CriticalSectionRawMutex, DisplayState, WATCH_RECEIVERS>;

/// Watch for sharing display state across tasks
pub static DISPLAY_STATE: Watch<CriticalSectionRawMutex, DisplayState, WATCH_RECEIVERS> =
    Watch::new();

/// WiFi connection status for display
#[derive(Clone, Copy, Default)]
pub enum WifiStatus {
    #[default]
    Connecting,
    GettingIP,
    Connected,
}

/// Display state containing motor powers and IP address
#[derive(Clone, Copy, Default)]
pub struct DisplayState {
    pub motor_a: i8,
    pub motor_b: i8,
    #[cfg(feature = "four_motor")]
    pub motor_c: i8,
    #[cfg(feature = "four_motor")]
    pub motor_d: i8,
    pub ip: [u8; 4],
    pub status: WifiStatus,
    pub dots: u8,
    /// Current WiFi SSID to display
    pub ssid: [u8; 32],
    pub ssid_len: u8,
    /// Temporary line 1 override (e.g. "BLE: Device" or "WiFi Saved")
    pub line1_override: [u8; 21],
    pub line1_override_len: u8,
    /// Remaining display cycles for line1 override (counts down to 0)
    pub line1_ticks: u8,
    /// Temporary flash message (e.g. "BLE Connected") - DEPRECATED, use line1_override
    pub flash_message: [u8; 20],
    pub flash_message_len: u8,
    /// Remaining display cycles for the flash message (counts down to 0)
    pub flash_ticks: u8,
}

/// Initialize the display state sender with initial SSID
pub fn init_display_state(initial_ssid: &str) -> DisplaySender {
    let sender = DISPLAY_STATE.sender();
    // Initialize with default state and SSID
    let mut state = DisplayState::default();
    let bytes = initial_ssid.as_bytes();
    let len = bytes.len().min(state.ssid.len());
    state.ssid[..len].copy_from_slice(&bytes[..len]);
    state.ssid_len = len as u8;
    sender.send(state);
    sender
}

/// Update the WiFi SSID in display state
pub fn update_ssid(sender: &DisplaySender, ssid: &str) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            let bytes = ssid.as_bytes();
            let len = bytes.len().min(s.ssid.len());
            s.ssid[..len].copy_from_slice(&bytes[..len]);
            s.ssid_len = len as u8;
        }
    });
}

/// Update motor A power in display state
pub fn update_motor_a(sender: &DisplaySender, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.motor_a = power;
        }
    });
}

/// Update motor B power in display state
pub fn update_motor_b(sender: &DisplaySender, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.motor_b = power;
        }
    });
}

/// Update motor C power in display state
#[cfg(feature = "four_motor")]
pub fn update_motor_c(sender: &DisplaySender, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.motor_c = power;
        }
    });
}

/// Update motor D power in display state
#[cfg(feature = "four_motor")]
pub fn update_motor_d(sender: &DisplaySender, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.motor_d = power;
        }
    });
}

/// Update a motor's power by index (0=A, 1=B, 2=C, 3=D)
pub fn update_motor(sender: &DisplaySender, index: usize, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            match index {
                0 => s.motor_a = power,
                1 => s.motor_b = power,
                #[cfg(feature = "four_motor")]
                2 => s.motor_c = power,
                #[cfg(feature = "four_motor")]
                3 => s.motor_d = power,
                _ => {}
            }
        }
    });
}

/// Update IP address in display state
pub fn update_ip(sender: &DisplaySender, ip: [u8; 4]) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.ip = ip;
            s.status = WifiStatus::Connected;
        }
    });
}

/// Update the WiFi status in display state
pub fn update_status(sender: &DisplaySender, status: WifiStatus) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.status = status;
        }
    });
}

/// Update the animated dots count (0-3) in display state
pub fn update_dots(sender: &DisplaySender, dots: u8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.dots = dots;
        }
    });
}

/// Show a temporary flash message on the display for ~3 seconds
pub fn flash_message(sender: &DisplaySender, msg: &str) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            let bytes = msg.as_bytes();
            let len = bytes.len().min(s.flash_message.len());
            s.flash_message[..len].copy_from_slice(&bytes[..len]);
            s.flash_message_len = len as u8;
            // ~6 ticks at 500ms each = ~3 seconds
            s.flash_ticks = 6;
        }
    });
}

/// Override line 1 temporarily (replaces WiFi SSID line for ~3 seconds)
pub fn set_line1_override(sender: &DisplaySender, msg: &str) {
    println!("[Display] Setting line1 override: '{}'", msg);
    sender.send_modify(|state| {
        if let Some(s) = state {
            let bytes = msg.as_bytes();
            let len = bytes.len().min(s.line1_override.len());
            s.line1_override[..len].copy_from_slice(&bytes[..len]);
            s.line1_override_len = len as u8;
            // ~6 ticks at 500ms each = ~3 seconds
            s.line1_ticks = 6;
        }
    });
}

/// OLED display task - updates the display periodically
#[embassy_executor::task]
pub async fn display_task(i2c: I2c<'static, Blocking>) {
    println!("Starting OLED display task...");

    // Create display interface
    let interface = I2CDisplayInterface::new(i2c);

    // Initialize display (128x64, I2C address 0x3C is default)
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();

    if let Err(e) = display.init() {
        println!("Failed to initialize display: {:?}", e);
        return;
    }

    // Turn on the display explicitly
    if let Err(e) = display.set_display_on(true) {
        println!("Failed to turn on display: {:?}", e);
    }

    // Clear the display
    display.clear_buffer();
    if let Err(e) = display.flush() {
        println!("Failed to flush display: {:?}", e);
        return;
    }

    println!("OLED display initialized and cleared");

    // Create text style
    let text_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    // Get a receiver for display state updates
    let mut receiver = DISPLAY_STATE.receiver().unwrap();

    // Buffer for formatting text
    let mut line_buf: heapless::String<32> = heapless::String::new();

    // Wait for initial state to be available
    let mut state = receiver.changed().await;

    loop {
        // Clear display buffer
        display.clear_buffer();

        // Line 1: WiFi SSID or temporary override (BLE connection, etc.)
        line_buf.clear();
        if state.line1_ticks > 0 {
            let override_msg =
                core::str::from_utf8(&state.line1_override[..state.line1_override_len as usize])
                    .unwrap_or("");
            let _ = write!(line_buf, "{}", override_msg);
            // Decrement line1 ticks
            DISPLAY_STATE.sender().send_modify(|s| {
                if let Some(s) = s {
                    s.line1_ticks = s.line1_ticks.saturating_sub(1);
                }
            });
        } else {
            let ssid = core::str::from_utf8(&state.ssid[..state.ssid_len as usize]).unwrap_or("");
            let _ = write!(
                line_buf,
                "WiFi: {}",
                if ssid.len() > 15 { &ssid[..15] } else { ssid }
            );
        }
        let _ = Text::new(&line_buf, Point::new(0, 10), text_style).draw(&mut display);

        // Line 2: IP Address
        line_buf.clear();
        match state.status {
            WifiStatus::Connected => {
                let _ = write!(
                    line_buf,
                    "IP: {}.{}.{}.{}",
                    state.ip[0], state.ip[1], state.ip[2], state.ip[3]
                );
            }
            _ => {
                let dots = match state.dots % 4 {
                    1 => ".",
                    2 => "..",
                    3 => "...",
                    _ => "",
                };
                let label = match state.status {
                    WifiStatus::Connecting => "Connecting",
                    WifiStatus::GettingIP => "Getting IP",
                    _ => unreachable!(),
                };
                let _ = write!(line_buf, "{}{}", label, dots);
            }
        }
        let _ = Text::new(&line_buf, Point::new(0, 24), text_style).draw(&mut display);

        // Line 3: Motors A & B
        line_buf.clear();
        let _ = write!(line_buf, "A:{:+4}% B:{:+4}%", state.motor_a, state.motor_b);
        let _ = Text::new(&line_buf, Point::new(0, 42), text_style).draw(&mut display);

        // Line 4: Motors C & D (four_motor only)
        #[cfg(feature = "four_motor")]
        {
            line_buf.clear();
            let _ = write!(line_buf, "C:{:+4}% D:{:+4}%", state.motor_c, state.motor_d);
            let _ = Text::new(&line_buf, Point::new(0, 56), text_style).draw(&mut display);
        }

        // Flash message overlay (centered banner)
        if state.flash_ticks > 0 {
            let msg =
                core::str::from_utf8(&state.flash_message[..state.flash_message_len as usize])
                    .unwrap_or("");
            // Center the message horizontally (6px per char on 128px wide display)
            let x = ((128i32 - (msg.len() as i32) * 6) / 2).max(0);
            // Draw a blank band in the middle of the screen then the text
            use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
            let _ = Rectangle::new(Point::new(0, 24), Size::new(128, 16))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(&mut display);
            let _ = Rectangle::new(
                Point::new(x - 2, 24),
                Size::new((msg.len() as u32) * 6 + 4, 16),
            )
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(&mut display);
            let inverted_style = MonoTextStyleBuilder::new()
                .font(&FONT_6X10)
                .text_color(BinaryColor::Off)
                .build();
            let _ = Text::new(msg, Point::new(x, 34), inverted_style).draw(&mut display);

            // Decrement flash ticks
            DISPLAY_STATE.sender().send_modify(|s| {
                if let Some(s) = s {
                    s.flash_ticks = s.flash_ticks.saturating_sub(1);
                }
            });
        }

        // Flush buffer to display
        let _ = display.flush();

        // Wait for state change or timeout (update at least every 500ms)
        match embassy_futures::select::select(
            receiver.changed(),
            Timer::after(Duration::from_millis(500)),
        )
        .await
        {
            embassy_futures::select::Either::First(new_state) => {
                state = new_state;
            }
            embassy_futures::select::Either::Second(_) => {
                // Timeout - re-read current state for tick countdown
                if let Some(s) = receiver.try_get() {
                    state = s;
                }
            }
        }
    }
}
