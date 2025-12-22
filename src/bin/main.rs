#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};
use embassy_futures::select::{select, select4, Either, Either4};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    ledc::Ledc,
    rng::Rng,
    timer::timg::TimerGroup,
    uart::{Uart, Config as UartConfig},
};
use esp_println::println;
use esp_radio::wifi::{
    ClientConfig,
    ModeConfig,
    WifiController,
    WifiDevice,
    WifiEvent,
    WifiStaState,
    sta_state,
};
use static_cell::StaticCell;
use esp32_http_servo::brushless::{BrushlessMotor, init_motor_timer};
use esp32_http_servo::http_server::{http_server_task, SERVO_ANGLE, MOTOR_A_POWER, MOTOR_B_POWER};
use esp32_http_servo::serial_cmd::{serial_input_task, SERIAL_SERVO_ANGLE, SERIAL_MOTOR_A_POWER, SERIAL_MOTOR_B_POWER};
use esp32_http_servo::servo::{ServoController, init_servo_timer};

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Initialize heap allocator
    esp_alloc::heap_allocator!(size: 72 * 1024);

    // Initialize timer and software interrupt for esp-rtos
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // Initialize UART for serial commands (uses USB-serial on most dev boards)
    let uart0 = Uart::new(
        peripherals.UART0,
        UartConfig::default(),
    ).unwrap();
    
    // Spawn serial command task
    spawner.spawn(serial_input_task(uart0)).ok();

    // Initialize LEDC peripheral
    let ledc = mk_static!(Ledc<'static>, Ledc::new(peripherals.LEDC));
    
    // Initialize servo timer (50Hz) and servo on GPIO18
    let servo_timer = mk_static!(
        esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::HighSpeed>,
        init_servo_timer(ledc)
    );
    let mut servo = ServoController::new(servo_timer, peripherals.GPIO18);
    servo.set_angle(90);
    println!("Servo initialized on GPIO18 at 90 degrees");
    
    // Initialize motor timer (1kHz for responsive H-bridge control)
    let motor_timer = mk_static!(
        esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::HighSpeed>,
        init_motor_timer(ledc)
    );
    
    // Initialize Motor A on GPIO32 (forward) and GPIO33 (reverse)
    let mut motor_a = BrushlessMotor::new(
        motor_timer,
        peripherals.GPIO32,
        peripherals.GPIO33,
        esp_hal::ledc::channel::Number::Channel1,
        esp_hal::ledc::channel::Number::Channel2,
        "Motor A",
    );
    println!("Motor A initialized on GPIO32/GPIO33");
    
    // Initialize Motor B on GPIO25 (forward) and GPIO26 (reverse)
    let mut motor_b = BrushlessMotor::new(
        motor_timer,
        peripherals.GPIO25,
        peripherals.GPIO26,
        esp_hal::ledc::channel::Number::Channel3,
        esp_hal::ledc::channel::Number::Channel4,
        "Motor B",
    );
    println!("Motor B initialized on GPIO25/GPIO26");

    // Initialize esp-radio controller
    let esp_radio_controller = mk_static!(esp_radio::Controller<'static>, esp_radio::init().unwrap());

    // Initialize WiFi
    let (controller, interfaces) = esp_radio::wifi::new(
        esp_radio_controller,
        peripherals.WIFI,
        esp_radio::wifi::Config::default(),
    )
    .unwrap();

    let wifi_interface = interfaces.sta;

    // Configure DHCP
    let net_config = embassy_net::Config::dhcpv4(Default::default());

    // Generate random seed for network stack
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    // Initialize network stack
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        net_config,
        mk_static!(StackResources<5>, StackResources::<5>::new()),
        seed,
    );

    // Spawn background tasks
    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(runner)).ok();
    spawner.spawn(wifi_ready_task(spawner, stack)).ok();

    // Main loop - handle servo and motor updates from HTTP or serial
    // This runs immediately, allowing serial control before WiFi connects
    loop {
        // Wait for signal from any source: servo (HTTP/serial) or motors (HTTP/serial)
        // Use nested select to handle 6 signals (select4 + select for remaining 2)
        match select(
            select4(
                SERVO_ANGLE.wait(),
                SERIAL_SERVO_ANGLE.wait(),
                MOTOR_A_POWER.wait(),
                MOTOR_B_POWER.wait(),
            ),
            select(
                SERIAL_MOTOR_A_POWER.wait(),
                SERIAL_MOTOR_B_POWER.wait(),
            ),
        ).await {
            Either::First(Either4::First(angle)) => {
                servo.set_angle(angle);
                println!("Servo moved to {} degrees", angle);
            }
            Either::First(Either4::Second(angle)) => {
                servo.set_angle(angle);
                println!("Servo moved to {} degrees (serial)", angle);
            }
            Either::First(Either4::Third(power)) => {
                motor_a.set_power(power);
                println!("Motor A set to {}%", power);
            }
            Either::First(Either4::Fourth(power)) => {
                motor_b.set_power(power);
                println!("Motor B set to {}%", power);
            }
            Either::Second(Either::First(power)) => {
                motor_a.set_power(power);
                println!("Motor A set to {}% (serial)", power);
            }
            Either::Second(Either::Second(power)) => {
                motor_b.set_power(power);
                println!("Motor B set to {}% (serial)", power);
            }
        }
    }
}

#[embassy_executor::task]
async fn wifi_ready_task(spawner: Spawner, stack: embassy_net::Stack<'static>) {
    // Wait for link to be up
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("WiFi connected successfully!");

    // Spawn HTTP server once WiFi is ready
    spawner.spawn(http_server_task(stack)).ok();
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("Start connection task");
    println!("Device capabilities: {:?}", controller.capabilities());
    
    loop {
        match sta_state() {
            WifiStaState::Connected => {
                // Wait until we're no longer connected
                controller
                    .wait_for_event(WifiEvent::StaDisconnected)
                    .await;
                Timer::after(Duration::from_millis(5000)).await
            }
            _ => {}
        }
        
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = ModeConfig::Client(
                ClientConfig::default()
                    .with_ssid(SSID.try_into().unwrap())
                    .with_password(PASSWORD.try_into().unwrap()),
            );
            controller.set_config(&client_config).unwrap();
            println!("Starting WiFi...");
            controller.start_async().await.unwrap();
            println!("WiFi started!");
        }
        
        println!("Connecting to WiFi network: {}", SSID);
        
        match controller.connect_async().await {
            Ok(_) => println!("WiFi connected!"),
            Err(e) => {
                println!("Failed to connect to WiFi: {:?}", e);
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
