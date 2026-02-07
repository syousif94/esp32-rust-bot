use esp_hal::ledc::{
    channel::{self, ChannelIFace, ChannelHW},
    timer::{self, TimerIFace, config::Duty},
    Ledc, HighSpeed,
};
use esp_hal::gpio::{DriveMode, interconnect::PeripheralOutput};
use esp_println::println;

/// PWM frequency for brushless motor control via H-bridge
/// 1kHz provides fast, responsive motor control
const MOTOR_FREQ_HZ: u32 = 1000;

/// Duty resolution (14-bit = 16384 steps)
const DUTY_RESOLUTION: u32 = 16384;

/// H-Bridge brushless motor controller using LEDC PWM
/// Controls motor direction and speed using two PWM channels
/// - Forward: channel_a = duty, channel_b = 0
/// - Reverse: channel_a = 0, channel_b = duty
/// - Brake: both channels = 0
pub struct BrushlessMotor<'d> {
    channel_a: channel::Channel<'d, HighSpeed>,
    channel_b: channel::Channel<'d, HighSpeed>,
    name: &'static str,
}

impl<'d> BrushlessMotor<'d> {
    /// Create a new H-bridge motor controller
    /// 
    /// # Arguments
    /// * `timer` - LEDC timer configured for motor PWM frequency
    /// * `pin_a` - GPIO pin for forward direction (e.g., GPIO32)
    /// * `pin_b` - GPIO pin for reverse direction (e.g., GPIO33)
    /// * `channel_num_a` - LEDC channel number for pin_a
    /// * `channel_num_b` - LEDC channel number for pin_b
    /// * `name` - Name for logging (e.g., "Motor A")
    pub fn new<PA: PeripheralOutput<'d>, PB: PeripheralOutput<'d>>(
        timer: &'d timer::Timer<'d, HighSpeed>,
        pin_a: PA,
        pin_b: PB,
        channel_num_a: channel::Number,
        channel_num_b: channel::Number,
        name: &'static str,
    ) -> Self {
        println!("Initializing {} (H-Bridge LEDC)", name);
        println!("  PWM frequency: {} Hz", MOTOR_FREQ_HZ);
        
        let mut channel_a = channel::Channel::new(channel_num_a, pin_a);
        channel_a.configure(channel::config::Config {
            timer,
            duty_pct: 0,
            drive_mode: DriveMode::PushPull,
        }).unwrap();
        channel_a.set_duty_hw(0);
        
        let mut channel_b = channel::Channel::new(channel_num_b, pin_b);
        channel_b.configure(channel::config::Config {
            timer,
            duty_pct: 0,
            drive_mode: DriveMode::PushPull,
        }).unwrap();
        channel_b.set_duty_hw(0);
        
        Self { channel_a, channel_b, name }
    }

    /// Set motor power and direction
    /// 
    /// # Arguments
    /// * `power` - Power level from -100 (full reverse) to +100 (full forward)
    ///            0 = brake/stop
    pub fn set_power(&mut self, power: i8) {
        let power = power.clamp(-100, 100);
        
        if power > 0 {
            // Forward: channel_a = duty, channel_b = 0
            let duty_raw = (power as u32 * DUTY_RESOLUTION) / 100;
            self.channel_a.set_duty_hw(duty_raw);
            self.channel_b.set_duty_hw(0);
            println!("{}: forward {}% (duty={})", self.name, power, duty_raw);
        } else if power < 0 {
            // Reverse: channel_a = 0, channel_b = duty
            let duty_raw = ((-power) as u32 * DUTY_RESOLUTION) / 100;
            self.channel_a.set_duty_hw(0);
            self.channel_b.set_duty_hw(duty_raw);
            println!("{}: reverse {}% (duty={})", self.name, -power, duty_raw);
        } else {
            // Brake: both channels = 0
            self.channel_a.set_duty_hw(0);
            self.channel_b.set_duty_hw(0);
            println!("{}: stopped", self.name);
        }
    }

    /// Emergency stop - immediately sets both channels to 0
    pub fn stop(&mut self) {
        self.channel_a.set_duty_hw(0);
        self.channel_b.set_duty_hw(0);
        println!("{}: emergency stop", self.name);
    }
}

/// Initialize LEDC timer for brushless motor control
/// Uses Timer1 (separate from servo Timer0) for 1kHz PWM frequency
pub fn init_motor_timer<'d>(ledc: &'d Ledc<'d>) -> timer::Timer<'d, HighSpeed> {
    let mut timer = ledc.timer::<HighSpeed>(timer::Number::Timer1);
    timer.configure(timer::config::Config {
        duty: Duty::Duty14Bit,
        clock_source: timer::HSClockSource::APBClk,
        frequency: esp_hal::time::Rate::from_hz(MOTOR_FREQ_HZ),
    }).unwrap();
    timer
}
