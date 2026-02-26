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
    i2c::master::{Config as I2cConfig, I2c},
    ledc::Ledc,
    rng::Rng,
    time::Rate,
    timer::timg::TimerGroup,
    uart::{Config as UartConfig, Uart},
};
use esp_println::println;
use esp_radio::ble::controller::BleConnector;
use esp_storage::FlashStorage;
use static_cell::StaticCell;

use esp32_http_servo::brushless::{init_motor_timer, BrushlessMotor};
use esp32_http_servo::commands::{Command, COMMANDS};
use esp32_http_servo::display::{display_task, init_display_state, update_motor, DisplaySender};
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
    // Pre-configure all motor pins as output low to prevent them being high on startup
    let gpio32 = Output::new(peripherals.GPIO32, Level::Low, OutputConfig::default());
    let gpio33 = Output::new(peripherals.GPIO33, Level::Low, OutputConfig::default());
    let gpio25 = Output::new(peripherals.GPIO25, Level::Low, OutputConfig::default());
    let gpio26 = Output::new(peripherals.GPIO26, Level::Low, OutputConfig::default());
    let gpio19 = Output::new(peripherals.GPIO19, Level::Low, OutputConfig::default());
    let gpio21 = Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default());
    let gpio22 = Output::new(peripherals.GPIO22, Level::Low, OutputConfig::default());
    let gpio23 = Output::new(peripherals.GPIO23, Level::Low, OutputConfig::default());

    let mut motors = [
        BrushlessMotor::new(motor_timer, gpio32, gpio33,
            esp_hal::ledc::channel::Number::Channel1, esp_hal::ledc::channel::Number::Channel2, "Motor A"),
        BrushlessMotor::new(motor_timer, gpio25, gpio26,
            esp_hal::ledc::channel::Number::Channel3, esp_hal::ledc::channel::Number::Channel4, "Motor B"),
        BrushlessMotor::new(motor_timer, gpio19, gpio21,
            esp_hal::ledc::channel::Number::Channel5, esp_hal::ledc::channel::Number::Channel6, "Motor C"),
        BrushlessMotor::new(motor_timer, gpio22, gpio23,
            esp_hal::ledc::channel::Number::Channel7, esp_hal::ledc::channel::Number::Channel0, "Motor D"),
    ];

    // Ensure all motors are stopped after initialization
    for motor in motors.iter_mut() {
        motor.set_power(0);
    }

    // -- I2C / OLED display ---------------------------------------------
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(100)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO4)
    .with_scl(peripherals.GPIO5);
    println!("I2C initialized on GPIO5 (SDA) / GPIO4 (SCL) at 100kHz");

    let display_sender = init_display_state(default_ssid());
    spawner.spawn(display_task(i2c)).ok();
    println!("OLED display task spawned");

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
    command_loop(&mut servo, &mut motors, &display_sender).await
}

/// Receive commands from any source and drive the actuators.
///
/// The `select(receive, yield_now)` pattern keeps the executor cycling so
/// radio coex events (WiFi / BLE) are serviced promptly.
///
/// A 500ms watchdog is implemented via `Instant`: if no command is received
/// within 500ms, all motors are automatically stopped. This prevents motors
/// from being stuck at a power level when the controller loses connectivity.
async fn command_loop(
    servo: &mut ServoController<'_>,
    motors: &mut [BrushlessMotor<'_>; 4],
    display_sender: &DisplaySender,
) -> ! {
    let mut last_powers = [0i8; 4];
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
                    Command::MotorsAll([a, b, c, d]) => {
                        motors[0].set_power(a);
                        motors[1].set_power(b);
                        motors[2].set_power(c);
                        motors[3].set_power(d);
                        update_motor(display_sender, 0, a);
                        update_motor(display_sender, 1, b);
                        update_motor(display_sender, 2, c);
                        update_motor(display_sender, 3, d);
                        last_powers = [a, b, c, d];
                        println!("Motors set to A={}% B={}% C={}% D={}%", a, b, c, d);
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
                    last_powers = [0; 4];
                }
            }
        }
    }
}
