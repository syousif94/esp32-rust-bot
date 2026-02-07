#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};
use embassy_futures::select::{select, select4, Either, Either4};
use core::pin::pin;
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    i2c::master::{Config as I2cConfig, I2c},
    ledc::Ledc,
    rng::Rng,
    time::Rate,
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
use esp_radio::ble::controller::BleConnector;
use esp32_http_servo::ble::{ble_task, BLE_SERVO_ANGLE, BLE_MOTORS_ALL};
use esp32_http_servo::brushless::{BrushlessMotor, init_motor_timer};
use esp32_http_servo::display::{display_task, init_display_state, update_motor_a, update_motor_b, update_motor_c, update_motor_d, update_ip, update_dots, update_status, WifiStatus, DisplaySender};
use esp32_http_servo::http_server::{http_server_task, SERVO_ANGLE, MOTOR_A_POWER, MOTOR_B_POWER, MOTOR_C_POWER, MOTOR_D_POWER};
use esp32_http_servo::serial_cmd::{serial_input_task, SERIAL_SERVO_ANGLE, SERIAL_MOTOR_A_POWER, SERIAL_MOTOR_B_POWER, SERIAL_MOTOR_C_POWER, SERIAL_MOTOR_D_POWER};
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
    // NOTE: 64KB heap balances WiFi/radio memory needs with stack space.
    // BLE large objects are in static cells, but the Server struct is temporarily
    // constructed on the stack before being moved, requiring extra headroom.
    esp_alloc::heap_allocator!(size: 64 * 1024);

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
    
    // Initialize Motor C on GPIO19 (forward) and GPIO21 (reverse)
    let mut motor_c = BrushlessMotor::new(
        motor_timer,
        peripherals.GPIO19,
        peripherals.GPIO21,
        esp_hal::ledc::channel::Number::Channel5,
        esp_hal::ledc::channel::Number::Channel6,
        "Motor C",
    );
    println!("Motor C initialized on GPIO19/GPIO21");
    
    // Initialize Motor D on GPIO22 (forward) and GPIO23 (reverse)
    let mut motor_d = BrushlessMotor::new(
        motor_timer,
        peripherals.GPIO22,
        peripherals.GPIO23,
        esp_hal::ledc::channel::Number::Channel7,
        esp_hal::ledc::channel::Number::Channel0,
        "Motor D",
    );
    println!("Motor D initialized on GPIO22/GPIO23");

    // Initialize I2C for OLED display (GPIO5 = SDA, GPIO4 = SCL)
    let i2c_config = I2cConfig::default().with_frequency(Rate::from_khz(100));
    let i2c = I2c::new(peripherals.I2C0, i2c_config)
        .unwrap()
        .with_sda(peripherals.GPIO4)
        .with_scl(peripherals.GPIO5);
    println!("I2C initialized on GPIO5 (SDA) / GPIO4 (SCL) at 100kHz");

    // Initialize display state and spawn display task
    let display_sender = init_display_state();
    spawner.spawn(display_task(i2c, SSID)).ok();
    println!("OLED display task spawned");

    // Initialize esp-radio controller (shared by WiFi and BLE)
    let esp_radio_controller = mk_static!(esp_radio::Controller<'static>, esp_radio::init().unwrap());

    // Initialize BLE connector (must be created alongside WiFi for coex)
    let connector = BleConnector::new(
        esp_radio_controller,
        peripherals.BT,
        Default::default(),
    ).unwrap();
    println!("BLE connector initialized");

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
    spawner.spawn(ble_task(connector)).ok();
    println!("BLE task spawned");
    spawner.spawn(connection(controller, display_sender.clone())).ok();
    spawner.spawn(net_task(runner)).ok();
    spawner.spawn(wifi_ready_task(spawner, stack, display_sender.clone())).ok();

    // Main loop - handle servo and motor updates from HTTP, serial, or BLE
    // This runs immediately, allowing serial/BLE control before WiFi connects
    loop {
        // Wait for signal from any source: servo or motors A-D (HTTP/serial/BLE)
        match select(
            select(
                select4(
                    SERVO_ANGLE.wait(),
                    SERIAL_SERVO_ANGLE.wait(),
                    BLE_SERVO_ANGLE.wait(),
                    MOTOR_A_POWER.wait(),
                ),
                select4(
                    MOTOR_B_POWER.wait(),
                    MOTOR_C_POWER.wait(),
                    MOTOR_D_POWER.wait(),
                    SERIAL_MOTOR_A_POWER.wait(),
                ),
            ),
            select(
                select4(
                    SERIAL_MOTOR_B_POWER.wait(),
                    SERIAL_MOTOR_C_POWER.wait(),
                    SERIAL_MOTOR_D_POWER.wait(),
                    BLE_MOTORS_ALL.wait(),
                ),
                embassy_futures::yield_now(),
            ),
        ).await {
            // HTTP servo
            Either::First(Either::First(Either4::First(angle))) => {
                servo.set_angle(angle);
                println!("Servo moved to {} degrees", angle);
            }
            // Serial servo
            Either::First(Either::First(Either4::Second(angle))) => {
                servo.set_angle(angle);
                println!("Servo moved to {} degrees (serial)", angle);
            }
            // BLE servo
            Either::First(Either::First(Either4::Third(angle))) => {
                servo.set_angle(angle);
                println!("Servo moved to {} degrees (BLE)", angle);
            }
            // HTTP Motor A
            Either::First(Either::First(Either4::Fourth(power))) => {
                motor_a.set_power(power);
                update_motor_a(&display_sender, power);
                println!("Motor A set to {}%", power);
            }
            // HTTP Motor B
            Either::First(Either::Second(Either4::First(power))) => {
                motor_b.set_power(power);
                update_motor_b(&display_sender, power);
                println!("Motor B set to {}%", power);
            }
            // HTTP Motor C
            Either::First(Either::Second(Either4::Second(power))) => {
                motor_c.set_power(power);
                update_motor_c(&display_sender, power);
                println!("Motor C set to {}%", power);
            }
            // HTTP Motor D
            Either::First(Either::Second(Either4::Third(power))) => {
                motor_d.set_power(power);
                update_motor_d(&display_sender, power);
                println!("Motor D set to {}%", power);
            }
            // Serial Motor A
            Either::First(Either::Second(Either4::Fourth(power))) => {
                motor_a.set_power(power);
                update_motor_a(&display_sender, power);
                println!("Motor A set to {}% (serial)", power);
            }
            // Serial Motor B
            Either::Second(Either::First(Either4::First(power))) => {
                motor_b.set_power(power);
                update_motor_b(&display_sender, power);
                println!("Motor B set to {}% (serial)", power);
            }
            // Serial Motor C
            Either::Second(Either::First(Either4::Second(power))) => {
                motor_c.set_power(power);
                update_motor_c(&display_sender, power);
                println!("Motor C set to {}% (serial)", power);
            }
            // Serial Motor D
            Either::Second(Either::First(Either4::Third(power))) => {
                motor_d.set_power(power);
                update_motor_d(&display_sender, power);
                println!("Motor D set to {}% (serial)", power);
            }
            // BLE Motors (all 4 at once)
            Either::Second(Either::First(Either4::Fourth(motors))) => {
                let [a, b, c, d] = motors;
                motor_a.set_power(a);
                motor_b.set_power(b);
                motor_c.set_power(c);
                motor_d.set_power(d);
                update_motor_a(&display_sender, a);
                update_motor_b(&display_sender, b);
                update_motor_c(&display_sender, c);
                update_motor_d(&display_sender, d);
                println!("Motors set to A={}% B={}% C={}% D={}% (BLE)", a, b, c, d);
            }
            // Unused slot (yield padding)
            Either::Second(Either::Second(_)) => {}
        }
    }
}

#[embassy_executor::task]
async fn wifi_ready_task(
    spawner: Spawner, 
    stack: embassy_net::Stack<'static>,
    display_sender: DisplaySender,
) {
    // Wait for link to be up
    let mut period_count: u8 = 0;
    loop {
        if stack.is_link_up() {
            break;
        }
        period_count = (period_count + 1) % 4;
        update_dots(&display_sender, period_count);
        match period_count {
            0 => println!("Waiting for link"),
            1 => println!("Waiting for link."),
            2 => println!("Waiting for link.."),
            _ => println!("Waiting for link..."),
        }
        Timer::after(Duration::from_millis(1000)).await;
    }

    println!("Waiting to get IP address...");
    update_status(&display_sender, WifiStatus::GettingIP);
    period_count = 0;
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            // Update display with IP address
            let ip = config.address.address();
            update_ip(&display_sender, ip.octets());
            break;
        }
        period_count = (period_count + 1) % 4;
        update_dots(&display_sender, period_count);
        match period_count {
            0 => println!("Getting IP"),
            1 => println!("Getting IP."),
            2 => println!("Getting IP.."),
            _ => println!("Getting IP..."),
        }
        Timer::after(Duration::from_millis(1000)).await;
    }

    println!("WiFi connected successfully!");

    // Spawn HTTP server once WiFi is ready
    spawner.spawn(http_server_task(stack)).ok();
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>, display_sender: DisplaySender) {
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
        
        // Animate "Connecting" message with 0-3 periods while waiting
        let mut connect_future = pin!(controller.connect_async());
        let mut period_count: u8 = 0;
        
        let result = loop {
            match select(
                &mut connect_future,
                Timer::after(Duration::from_millis(1000)),
            ).await {
                Either::First(result) => break result,
                Either::Second(_) => {
                    period_count = (period_count + 1) % 4;
                    update_dots(&display_sender, period_count);
                    match period_count {
                        0 => println!("Connecting"),
                        1 => println!("Connecting."),
                        2 => println!("Connecting.."),
                        _ => println!("Connecting..."),
                    }
                }
            }
        };
        
        match result {
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
