#[cfg(feature = "two_motor")]
use esp_hal::gpio::Output;
use esp_hal::gpio::{DriveMode, interconnect::PeripheralOutput};
use esp_hal::ledc::{
    HighSpeed, Ledc,
    channel::{self, ChannelHW, ChannelIFace},
    timer::{self, TimerIFace, config::Duty},
};
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
        channel_a
            .configure(channel::config::Config {
                timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .unwrap();
        channel_a.set_duty_hw(0);

        let mut channel_b = channel::Channel::new(channel_num_b, pin_b);
        channel_b
            .configure(channel::config::Config {
                timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .unwrap();
        channel_b.set_duty_hw(0);

        Self {
            channel_a,
            channel_b,
            name,
        }
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
    timer
        .configure(timer::config::Config {
            duty: Duty::Duty14Bit,
            clock_source: timer::HSClockSource::APBClk,
            frequency: esp_hal::time::Rate::from_hz(MOTOR_FREQ_HZ),
        })
        .unwrap();
    timer
}

/// Common trait for motor control, implemented by both BrushlessMotor and TB6612Motor
pub trait MotorControl {
    fn set_power(&mut self, power: i8);
    fn stop(&mut self);
}

impl MotorControl for BrushlessMotor<'_> {
    fn set_power(&mut self, power: i8) {
        self.set_power(power);
    }
    fn stop(&mut self) {
        self.stop();
    }
}

/// TB6612FNG motor driver controller
///
/// Uses the dedicated PWM pin for speed control (LEDC) and IN1/IN2 as
/// digital direction pins. This matches the TB6612 truth table:
/// - Forward: IN1=HIGH, IN2=LOW, PWM=duty
/// - Reverse: IN1=LOW,  IN2=HIGH, PWM=duty
/// - Brake:   IN1=HIGH, IN2=HIGH, PWM=HIGH
/// - Stop:    IN1=LOW,  IN2=LOW,  PWM=0
#[cfg(feature = "two_motor")]
pub struct TB6612Motor<'d> {
    in1: Output<'d>,
    in2: Output<'d>,
    pwm_channel: channel::Channel<'d, HighSpeed>,
    name: &'static str,
}

#[cfg(feature = "two_motor")]
impl<'d> TB6612Motor<'d> {
    /// Create a new TB6612 motor controller
    ///
    /// # Arguments
    /// * `timer` - LEDC timer configured for motor PWM frequency
    /// * `in1` - GPIO output for forward direction (e.g., AIN1)
    /// * `in2` - GPIO output for reverse direction (e.g., AIN2)
    /// * `pwm_pin` - GPIO pin connected to TB6612 PWM input (PWMA/PWMB)
    /// * `channel_num` - LEDC channel number for PWM pin
    /// * `name` - Name for logging (e.g., "Motor A")
    pub fn new<P: PeripheralOutput<'d>>(
        timer: &'d timer::Timer<'d, HighSpeed>,
        in1: Output<'d>,
        in2: Output<'d>,
        pwm_pin: P,
        channel_num: channel::Number,
        name: &'static str,
    ) -> Self {
        println!("Initializing {} (TB6612 LEDC)", name);
        println!("  PWM frequency: {} Hz", MOTOR_FREQ_HZ);

        let mut pwm_channel = channel::Channel::new(channel_num, pwm_pin);
        pwm_channel
            .configure(channel::config::Config {
                timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .unwrap();
        pwm_channel.set_duty_hw(0);

        Self {
            in1,
            in2,
            pwm_channel,
            name,
        }
    }

    /// Set motor power and direction
    ///
    /// # Arguments
    /// * `power` - Power level from -100 (full reverse) to +100 (full forward)
    ///            0 = stop
    pub fn set_power(&mut self, power: i8) {
        let power = power.clamp(-100, 100);

        if power > 0 {
            // Forward: IN1=HIGH, IN2=LOW, PWM=duty
            self.in1.set_high();
            self.in2.set_low();
            let duty_raw = (power as u32 * DUTY_RESOLUTION) / 100;
            self.pwm_channel.set_duty_hw(duty_raw);
            println!("{}: forward {}% (duty={})", self.name, power, duty_raw);
        } else if power < 0 {
            // Reverse: IN1=LOW, IN2=HIGH, PWM=duty
            self.in1.set_low();
            self.in2.set_high();
            let duty_raw = ((-power) as u32 * DUTY_RESOLUTION) / 100;
            self.pwm_channel.set_duty_hw(duty_raw);
            println!("{}: reverse {}% (duty={})", self.name, -power, duty_raw);
        } else {
            // Stop: IN1=LOW, IN2=LOW, PWM=0
            self.in1.set_low();
            self.in2.set_low();
            self.pwm_channel.set_duty_hw(0);
            println!("{}: stopped", self.name);
        }
    }

    /// Emergency stop
    pub fn stop(&mut self) {
        self.in1.set_low();
        self.in2.set_low();
        self.pwm_channel.set_duty_hw(0);
        println!("{}: emergency stop", self.name);
    }
}

#[cfg(feature = "two_motor")]
impl MotorControl for TB6612Motor<'_> {
    fn set_power(&mut self, power: i8) {
        self.set_power(power);
    }
    fn stop(&mut self) {
        self.stop();
    }
}
