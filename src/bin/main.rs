#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::StackResources;
use embassy_time::{Duration, Instant};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    ledc::Ledc,
    rng::Rng,
    timer::timg::TimerGroup,
    uart::{Config as UartConfig, Uart},
};
use esp_hal::{
    i2c::master::{Config as I2cConfig, I2c},
    time::Rate,
};
use esp_println::println;
use esp_radio::ble::controller::BleConnector;
use esp_storage::FlashStorage;
use static_cell::StaticCell;

use esp32_http_servo::brushless::{init_motor_timer, MotorControl};
#[cfg(feature = "four_motor")]
use esp32_http_servo::brushless::BrushlessMotor;
#[cfg(feature = "two_motor")]
use esp32_http_servo::brushless::TB6612Motor;
use esp32_http_servo::commands::{Command, COMMANDS, MOTOR_COUNT};
#[cfg(feature = "four_motor")]
use esp32_http_servo::display::display_task;
use esp32_http_servo::display::{init_display_state, update_motor, DisplaySender};
use esp32_http_servo::servo::{init_servo_timer, ServoController};

use esp32_http_servo::ble::ble_task;
use esp32_http_servo::serial_cmd::serial_input_task;
use esp32_http_servo::wifi::{
    default_ssid, net_task, wifi_config_task, wifi_connection_task, wifi_ready_task,
};

// Required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Heap + RTOS timer
    esp_alloc::heap_allocator!(size: 64 * 1024);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // -- UART (serial commands) ----------------------------------------
    let uart0 = Uart::new(peripherals.UART0, UartConfig::default()).unwrap();
    spawner.spawn(serial_input_task(uart0)).ok();

    // -- LEDC / servo / motors ------------------------------------------
    let ledc = mk_static!(Ledc<'static>, Ledc::new(peripherals.LEDC));

    let servo_timer = mk_static!(
        esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::HighSpeed>,
        init_servo_timer(ledc)
    );
    let mut servo = ServoController::new(servo_timer, peripherals.GPIO18);
    servo.set_angle(90);
    println!("Servo initialized on GPIO18 at 90 degrees");

    let motor_timer = mk_static!(
        esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::HighSpeed>,
        init_motor_timer(ledc)
    );
    // -- Motor pin setup (conditional on feature) ----------------------
    #[cfg(feature = "four_motor")]
    let mut motors = {
        // Pre-configure all motor pins as output low to prevent them being high on startup
        let gpio32 = Output::new(peripherals.GPIO32, Level::Low, OutputConfig::default());
        let gpio33 = Output::new(peripherals.GPIO33, Level::Low, OutputConfig::default());
        let gpio25 = Output::new(peripherals.GPIO25, Level::Low, OutputConfig::default());
        let gpio26 = Output::new(peripherals.GPIO26, Level::Low, OutputConfig::default());
        let gpio19 = Output::new(peripherals.GPIO19, Level::Low, OutputConfig::default());
        let gpio21 = Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default());
        let gpio22 = Output::new(peripherals.GPIO22, Level::Low, OutputConfig::default());
        let gpio23 = Output::new(peripherals.GPIO23, Level::Low, OutputConfig::default());

        let mut m: [BrushlessMotor; 4] = [
            BrushlessMotor::new(motor_timer, gpio32, gpio33,
                esp_hal::ledc::channel::Number::Channel1, esp_hal::ledc::channel::Number::Channel2, "Motor A"),
            BrushlessMotor::new(motor_timer, gpio25, gpio26,
                esp_hal::ledc::channel::Number::Channel3, esp_hal::ledc::channel::Number::Channel4, "Motor B"),
            BrushlessMotor::new(motor_timer, gpio19, gpio21,
                esp_hal::ledc::channel::Number::Channel5, esp_hal::ledc::channel::Number::Channel6, "Motor C"),
            BrushlessMotor::new(motor_timer, gpio22, gpio23,
                esp_hal::ledc::channel::Number::Channel7, esp_hal::ledc::channel::Number::Channel0, "Motor D"),
        ];
        for motor in m.iter_mut() { motor.set_power(0); }
        m
    };

    #[cfg(feature = "two_motor")]
    let mut motors = {
        // TB6612 2-motor configuration (STBY is hardwired to 3.3V)
        // Pin mapping from schematic: S0=GPIO25(PWMA), S1=GPIO17(AIN2),
        // S2=GPIO21(AIN1), S3=GPIO22(BIN1), S4=GPIO23(BIN2), S5=GPIO26(PWMB)

        // Motor A: AIN1 = GPIO21, AIN2 = GPIO17, PWMA = GPIO25
        let ain1 = Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default());
        let ain2 = Output::new(peripherals.GPIO17, Level::Low, OutputConfig::default());
        // Motor B: BIN1 = GPIO22, BIN2 = GPIO23, PWMB = GPIO26
        let bin1 = Output::new(peripherals.GPIO22, Level::Low, OutputConfig::default());
        let bin2 = Output::new(peripherals.GPIO23, Level::Low, OutputConfig::default());

        let mut m: [TB6612Motor; 2] = [
            TB6612Motor::new(motor_timer, ain1, ain2, peripherals.GPIO25,
                esp_hal::ledc::channel::Number::Channel1, "Motor A"),
            TB6612Motor::new(motor_timer, bin1, bin2, peripherals.GPIO26,
                esp_hal::ledc::channel::Number::Channel2, "Motor B"),
        ];
        for motor in m.iter_mut() { motor.set_power(0); }
        println!("TB6612 2-motor config: STBY=3.3V (always on), PWMA=GPIO25, PWMB=GPIO26");
        m
    };

    // -- I2C / OLED display ---------------------------------------------
    #[cfg(feature = "four_motor")]
    let i2c = {
        let bus = I2c::new(
            peripherals.I2C0,
            I2cConfig::default().with_frequency(Rate::from_khz(100)),
        )
        .unwrap()
        .with_sda(peripherals.GPIO4)
        .with_scl(peripherals.GPIO5);
        println!("I2C initialized on GPIO4 (SDA) / GPIO5 (SCL) at 100kHz");
        bus
    };

    // -- INA219 voltage monitor (two_motor only) -----------------------
    #[cfg(feature = "two_motor")]
    {
        let ina_i2c = I2c::new(
            peripherals.I2C0,
            I2cConfig::default().with_frequency(Rate::from_khz(100)),
        )
        .unwrap()
        .with_sda(peripherals.GPIO32)
        .with_scl(peripherals.GPIO33);
        println!("INA219 I2C initialized on GPIO32 (SDA) / GPIO33 (SCL)");
        spawner.spawn(ina219_task(ina_i2c)).ok();
    }

    let display_sender = init_display_state(default_ssid());

    #[cfg(feature = "four_motor")]
    {
        spawner.spawn(display_task(i2c)).ok();
        println!("OLED display task spawned");
    }

    // -- Radio (WiFi + BLE) ---------------------------------------------
    let esp_radio_controller =
        mk_static!(esp_radio::Controller<'static>, esp_radio::init().unwrap());

    let connector = BleConnector::new(
        esp_radio_controller,
        peripherals.BT,
        Default::default(),
    )
    .unwrap();
    println!("BLE connector initialized");

    let (controller, interfaces) = esp_radio::wifi::new(
        esp_radio_controller,
        peripherals.WIFI,
        esp_radio::wifi::Config::default(),
    )
    .unwrap();

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        net_config,
        mk_static!(StackResources<5>, StackResources::<5>::new()),
        seed,
    );

    let flash_storage = mk_static!(
        FlashStorage<'static>,
        FlashStorage::new(peripherals.FLASH)
    );

    // -- Spawn background tasks -----------------------------------------
    spawner.spawn(ble_task(connector)).ok();
    spawner.spawn(wifi_config_task(flash_storage, display_sender.clone())).ok();
    spawner.spawn(wifi_connection_task(controller, display_sender.clone())).ok();
    spawner.spawn(net_task(runner)).ok();
    spawner.spawn(wifi_ready_task(spawner, stack, display_sender.clone())).ok();

    // -- Command loop ---------------------------------------------------
    #[cfg(feature = "four_motor")]
    {
        command_loop(&mut servo, &mut motors, &display_sender).await
    }
    #[cfg(feature = "two_motor")]
    {
        command_loop(&mut servo, &mut motors, &display_sender).await
    }
}

// ---------------------------------------------------------------------------
// INA219 voltage monitor (two_motor only)
// ---------------------------------------------------------------------------

/// INA219 default I2C address
const INA219_ADDR: u8 = 0x42;
/// Bus Voltage register
const INA219_REG_BUS_VOLTAGE: u8 = 0x02;

/// Estimate 3S LiPo battery percentage from voltage (in mV).
///
/// Piecewise-linear approximation of the discharge curve:
///   12600 mV = 100%,  12000 mV = 80%,  11400 mV = 50%,
///   10800 mV = 20%,   10200 mV = 5%,    9000 mV = 0%
fn battery_percentage_3s(mv: u16) -> u8 {
    // Table of (millivolts, percentage) breakpoints, descending
    const TABLE: [(u16, u16); 6] = [
        (12600, 100),
        (12000, 80),
        (11400, 50),
        (10800, 20),
        (10200, 5),
        (9000, 0),
    ];
    if mv >= TABLE[0].0 {
        return 100;
    }
    if mv <= TABLE[TABLE.len() - 1].0 {
        return 0;
    }
    // Find the segment and linearly interpolate
    let mut i = 0;
    while i < TABLE.len() - 1 {
        let (v_hi, p_hi) = TABLE[i];
        let (v_lo, p_lo) = TABLE[i + 1];
        if mv >= v_lo {
            // Linear interpolation between the two breakpoints
            let pct = p_lo + ((mv - v_lo) as u32 * (p_hi - p_lo) as u32
                / (v_hi - v_lo) as u32) as u16;
            return pct as u8;
        }
        i += 1;
    }
    0
}

/// Periodically reads the INA219 bus voltage and logs it with battery %.
#[embassy_executor::task]
async fn ina219_task(mut i2c: I2c<'static, esp_hal::Blocking>) {
    // Wait a moment for the INA219 to power up
    embassy_time::Timer::after(Duration::from_millis(200)).await;

    println!("INA219 task started (addr=0x{:02X})", INA219_ADDR);

    // Verify the device is present with a simple read
    let mut check = [0u8; 2];
    match i2c.write_read(INA219_ADDR, &[0x00], &mut check) {
        Ok(()) => println!("INA219 config register: 0x{:04X}", u16::from_be_bytes(check)),
        Err(e) => println!("INA219 not responding: {:?}", e),
    }

    let mut buf = [0u8; 2];
    loop {
        // Read bus voltage register: write register address, then read 2 bytes
        match i2c.write_read(INA219_ADDR, &[INA219_REG_BUS_VOLTAGE], &mut buf) {
            Ok(()) => {
                let raw = u16::from_be_bytes(buf);
                // Bits [15:3] contain the voltage value, LSB = 4mV
                let voltage_mv = (raw >> 3) * 4;
                let pct = battery_percentage_3s(voltage_mv);
                // Publish to global state for HTTP/BLE access
                esp32_http_servo::commands::BATTERY_MV.store(voltage_mv, core::sync::atomic::Ordering::Relaxed);
                esp32_http_servo::commands::BATTERY_PCT.store(pct, core::sync::atomic::Ordering::Relaxed);
                let volts = voltage_mv / 1000;
                let frac = (voltage_mv % 1000) / 10; // two decimal places
                println!("Battery: {}.{:02}V  {}%", volts, frac, pct);
            }
            Err(e) => {
                println!("INA219 read error: {:?}", e);
            }
        }
        embassy_time::Timer::after(Duration::from_secs(2)).await;
    }
}

/// Receive commands from any source and drive the actuators.
///
/// The `select(receive, yield_now)` pattern keeps the executor cycling so
/// radio coex events (WiFi / BLE) are serviced promptly.
///
/// A 500ms watchdog is implemented via `Instant`: if no command is received
/// within 500ms, all motors are automatically stopped. This prevents motors
/// from being stuck at a power level when the controller loses connectivity.
async fn command_loop<M: MotorControl>(
    servo: &mut ServoController<'_>,
    motors: &mut [M; MOTOR_COUNT],
    display_sender: &DisplaySender,
) -> ! {
    let mut last_powers = [0i8; MOTOR_COUNT];
    let mut last_cmd_time = Instant::now();

    loop {
        match select(COMMANDS.receive(), embassy_futures::yield_now()).await {
            Either::First(cmd) => {
                last_cmd_time = Instant::now();
                match cmd {
                    Command::Servo(angle) => {
                        servo.set_angle(angle);
                        println!("Servo moved to {} degrees", angle);
                    }
                    Command::Motor(id, power) => {
                        let idx = id as usize;
                        motors[idx].set_power(power);
                        update_motor(display_sender, idx, power);
                        last_powers[idx] = power;
                        println!("Motor {:?} set to {}%", id, power);
                    }
                    Command::MotorsAll(powers) => {
                        for (i, &p) in powers.iter().enumerate() {
                            motors[i].set_power(p);
                            update_motor(display_sender, i, p);
                        }
                        last_powers = powers;
                        #[cfg(feature = "four_motor")]
                        println!("Motors set to A={}% B={}% C={}% D={}%", powers[0], powers[1], powers[2], powers[3]);
                        #[cfg(feature = "two_motor")]
                        println!("Motors set to A={}% B={}%", powers[0], powers[1]);
                    }
                }
            }
            Either::Second(_) => {
                // Watchdog: no command for 500ms — stop motors if any are active
                if last_powers.iter().any(|&p| p != 0)
                    && last_cmd_time.elapsed() >= Duration::from_millis(500)
                {
                    println!("Watchdog: stopping all motors (no command for 500ms)");
                    for (i, m) in motors.iter_mut().enumerate() {
                        m.set_power(0);
                        update_motor(display_sender, i, 0);
                    }
                    last_powers = [0; MOTOR_COUNT];
                }
            }
        }
    }
}
