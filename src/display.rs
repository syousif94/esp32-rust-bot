//! OLED Display module for 128x64 I2C SSD1306 display
//! 
//! Displays WiFi network, IP address, and motor power levels.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Watch, Sender};
use embassy_time::{Duration, Timer};
use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;
use esp_println::println;
use ssd1306::{
    prelude::*,
    I2CDisplayInterface,
    Ssd1306,
};
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::Text,
};
use core::fmt::Write;

/// Number of receivers for Watch
const WATCH_RECEIVERS: usize = 2;

/// Type alias for the display state sender
pub type DisplaySender = Sender<'static, CriticalSectionRawMutex, DisplayState, WATCH_RECEIVERS>;

/// Watch for sharing display state across tasks
pub static DISPLAY_STATE: Watch<CriticalSectionRawMutex, DisplayState, WATCH_RECEIVERS> = Watch::new();

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
    pub motor_c: i8,
    pub motor_d: i8,
    pub ip: [u8; 4],
    pub status: WifiStatus,
    pub dots: u8,
}

/// Initialize the display state sender
pub fn init_display_state() -> DisplaySender {
    let sender = DISPLAY_STATE.sender();
    // Initialize with default state
    sender.send(DisplayState::default());
    sender
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
pub fn update_motor_c(sender: &DisplaySender, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.motor_c = power;
        }
    });
}

/// Update motor D power in display state
pub fn update_motor_d(sender: &DisplaySender, power: i8) {
    sender.send_modify(|state| {
        if let Some(s) = state {
            s.motor_d = power;
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

/// OLED display task - updates the display periodically
#[embassy_executor::task]
pub async fn display_task(i2c: I2c<'static, Blocking>, ssid: &'static str) {
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
    
    loop {
        // Get current state (wait for it to be available)
        let state = receiver.get().await;
        
        // Clear display buffer
        display.clear_buffer();
        
        // Line 1: WiFi SSID (truncate if needed)
        line_buf.clear();
        let _ = write!(line_buf, "WiFi: {}", if ssid.len() > 15 { &ssid[..15] } else { ssid });
        let _ = Text::new(&line_buf, Point::new(0, 10), text_style).draw(&mut display);
        
        // Line 2: IP Address
        line_buf.clear();
        match state.status {
            WifiStatus::Connected => {
                let _ = write!(line_buf, "IP: {}.{}.{}.{}", 
                    state.ip[0], state.ip[1], state.ip[2], state.ip[3]);
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
        
        // Line 4: Motors C & D
        line_buf.clear();
        let _ = write!(line_buf, "C:{:+4}% D:{:+4}%", state.motor_c, state.motor_d);
        let _ = Text::new(&line_buf, Point::new(0, 56), text_style).draw(&mut display);
        
        // Flush buffer to display
        let _ = display.flush();
        
        // Wait for state change or timeout (update at least every 500ms)
        let _ = embassy_futures::select::select(
            receiver.changed(),
            Timer::after(Duration::from_millis(500)),
        ).await;
    }
}
