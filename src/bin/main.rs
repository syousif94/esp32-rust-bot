#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::StackResources;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    ledc::Ledc,
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
use heapless::Vec as HVec;
use static_cell::StaticCell;

use esp32_http_servo::ble::ble_task;
#[cfg(feature = "four_motor")]
use esp32_http_servo::brushless::BrushlessMotor;
#[cfg(feature = "two_motor")]
use esp32_http_servo::brushless::TB6612Motor;
use esp32_http_servo::brushless::{MotorControl, init_motor_timer};
use esp32_http_servo::commands::{
    COMMANDS, Command, MOTOR_COUNT, complete_battery_sample_request,
    wait_for_battery_sample_request,
};
#[cfg(feature = "four_motor")]
use esp32_http_servo::display::display_task;
use esp32_http_servo::display::{DisplaySender, init_display_state, update_motor};
use esp32_http_servo::st3215::{
    MAX_SERVOS, SHARED_BUS, SHARED_LIST, ServoList, SharedBus, SharedList, St3215Bus,
};

use esp32_http_servo::serial_cmd::serial_input_task;
use esp32_http_servo::wifi::{
    net_task, request_ble_mode, request_wifi_mode, wifi_config_task, wifi_connection_task,
    wifi_ready_task,
};
use esp32_http_servo::wifi_config::{RadioMode, read_radio_mode};

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

    // Heap + RTOS timer.
    //
    // Keep some stack headroom below the stack-breaking 64K setting, but give
    // WiFi enough normal heap that ARP/TCP stay responsive after DHCP.
    esp_alloc::heap_allocator!(#[unsafe(link_section = ".dram2_uninit")] size: 60 * 1024);
    println!(
        "[Heap] after heap init: {} bytes free",
        esp_alloc::HEAP.free()
    );
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // -- UART0 (serial commands console) --------------------------------
    let uart0 = Uart::new(peripherals.UART0, UartConfig::default()).unwrap();
    spawner.spawn(serial_input_task(uart0)).ok();

    // -- UART1 (ST3215 bus servo @ 1 Mbps) -----------------------------
    // Waveshare General Driver for Robots: RX=GPIO18, TX=GPIO19.
    let bus_uart = Uart::new(
        peripherals.UART1,
        UartConfig::default().with_baudrate(1_000_000),
    )
    .unwrap()
    .with_rx(peripherals.GPIO18)
    .with_tx(peripherals.GPIO19);

    let shared_bus: &'static SharedBus =
        mk_static!(SharedBus, Mutex::new(St3215Bus::new(bus_uart)));
    SHARED_BUS.init(shared_bus).ok();

    let shared_list: &'static SharedList = mk_static!(SharedList, Mutex::new(HVec::new()));
    SHARED_LIST.init(shared_list).ok();

    println!("ST3215 bus initialized on UART1 (RX=GPIO18, TX=GPIO19 @ 1Mbps)");

    // -- LEDC / motors --------------------------------------------------
    let ledc = mk_static!(Ledc<'static>, Ledc::new(peripherals.LEDC));

    let motor_timer = mk_static!(
        esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::HighSpeed>,
        init_motor_timer(ledc)
    );

    // -- Motor pin setup (conditional on feature) ----------------------
    #[cfg(feature = "four_motor")]
    let mut motors = {
        // NOTE: this branch is gated by a compile_error! in lib.rs because
        // GPIO19 conflicts with the ST3215 TX line. Kept here for symmetry.
        let gpio32 = Output::new(peripherals.GPIO32, Level::Low, OutputConfig::default());
        let gpio33 = Output::new(peripherals.GPIO33, Level::Low, OutputConfig::default());
        let gpio25 = Output::new(peripherals.GPIO25, Level::Low, OutputConfig::default());
        let gpio26 = Output::new(peripherals.GPIO26, Level::Low, OutputConfig::default());
        let gpio19 = Output::new(peripherals.GPIO19, Level::Low, OutputConfig::default());
        let gpio21 = Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default());
        let gpio22 = Output::new(peripherals.GPIO22, Level::Low, OutputConfig::default());
        let gpio23 = Output::new(peripherals.GPIO23, Level::Low, OutputConfig::default());

        let mut m: [BrushlessMotor; 4] = [
            BrushlessMotor::new(
                motor_timer,
                gpio32,
                gpio33,
                esp_hal::ledc::channel::Number::Channel1,
                esp_hal::ledc::channel::Number::Channel2,
                "Motor A",
            ),
            BrushlessMotor::new(
                motor_timer,
                gpio25,
                gpio26,
                esp_hal::ledc::channel::Number::Channel3,
                esp_hal::ledc::channel::Number::Channel4,
                "Motor B",
            ),
            BrushlessMotor::new(
                motor_timer,
                gpio19,
                gpio21,
                esp_hal::ledc::channel::Number::Channel5,
                esp_hal::ledc::channel::Number::Channel6,
                "Motor C",
            ),
            BrushlessMotor::new(
                motor_timer,
                gpio22,
                gpio23,
                esp_hal::ledc::channel::Number::Channel7,
                esp_hal::ledc::channel::Number::Channel0,
                "Motor D",
            ),
        ];
        for motor in m.iter_mut() {
            motor.set_power(0);
        }
        m
    };

    #[cfg(feature = "two_motor")]
    let mut motors = {
        // TB6612 2-motor configuration (STBY hardwired to 3.3V)
        let ain1 = Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default());
        let ain2 = Output::new(peripherals.GPIO17, Level::Low, OutputConfig::default());
        let bin1 = Output::new(peripherals.GPIO22, Level::Low, OutputConfig::default());
        let bin2 = Output::new(peripherals.GPIO23, Level::Low, OutputConfig::default());

        let mut m: [TB6612Motor; 2] = [
            TB6612Motor::new(
                motor_timer,
                ain1,
                ain2,
                peripherals.GPIO25,
                esp_hal::ledc::channel::Number::Channel1,
                "Motor A",
            ),
            TB6612Motor::new(
                motor_timer,
                bin1,
                bin2,
                peripherals.GPIO26,
                esp_hal::ledc::channel::Number::Channel2,
                "Motor B",
            ),
        ];
        for motor in m.iter_mut() {
            motor.set_power(0);
        }
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

    let display_sender = init_display_state("BLE Ready");
    println!("Boot: display state initialized");

    #[cfg(feature = "four_motor")]
    {
        spawner.spawn(display_task(i2c)).ok();
        println!("OLED display task spawned");
    }

    // -- Radio (BLE by default, persisted WiFi/BLE selected by serial) ----
    println!("Boot: initializing radio controller");
    let esp_radio_controller =
        mk_static!(esp_radio::Controller<'static>, esp_radio::init().unwrap());
    println!("Boot: radio controller initialized");
    println!(
        "[Heap] after radio init: {} bytes free",
        esp_alloc::HEAP.free()
    );

    let flash_storage = mk_static!(FlashStorage<'static>, FlashStorage::new(peripherals.FLASH));
    let radio_mode = read_radio_mode(flash_storage);
    println!("Boot: selected radio mode: {:?}", radio_mode);

    spawner
        .spawn(wifi_config_task(flash_storage, display_sender.clone()))
        .ok();

    if radio_mode == RadioMode::Ble {
        request_ble_mode();
        println!("Boot: initializing BLE connector");
        let connector =
            BleConnector::new(esp_radio_controller, peripherals.BT, Default::default()).unwrap();
        println!("BLE connector initialized");
        println!(
            "[Heap] after BLE connector init: {} bytes free",
            esp_alloc::HEAP.free()
        );

        spawner.spawn(ble_task(connector)).ok();
        println!("Boot: BLE task spawned");
    } else {
        request_wifi_mode();
        println!("Boot: BLE disabled for WiFi mode");
    }

    let wifi_cfg = esp_radio::wifi::Config::default()
        .with_rx_queue_size(2)
        .with_tx_queue_size(2)
        .with_static_rx_buf_num(2)
        .with_dynamic_rx_buf_num(4)
        .with_dynamic_tx_buf_num(4)
        .with_ampdu_rx_enable(false)
        .with_ampdu_tx_enable(false)
        .with_rx_ba_win(2);
    let (controller, interfaces) =
        esp_radio::wifi::new(esp_radio_controller, peripherals.WIFI, wifi_cfg).unwrap();
    println!("Boot: WiFi interface initialized");
    println!(
        "[Heap] after WiFi interface init: {} bytes free",
        esp_alloc::HEAP.free()
    );

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let seed = 0x5eed_cafe_1f0c_0068;

    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );
    println!("Boot: network stack initialized");

    spawner
        .spawn(wifi_connection_task(controller, display_sender.clone()))
        .ok();
    spawner.spawn(net_task(runner)).ok();
    spawner
        .spawn(wifi_ready_task(spawner, stack, display_sender.clone()))
        .ok();
    println!("Boot: WiFi tasks spawned (idle until serial `wi`)");

    // -- Boot scan for ST3215 servos -----------------------------------
    // Give peripherals a moment to settle.
    Timer::after(Duration::from_millis(50)).await;
    st_rescan(1, 20).await;

    // -- Command loop ---------------------------------------------------
    command_loop(&mut motors, &display_sender).await
}

/// Re-scan the ST3215 bus and update the shared list.
async fn st_rescan(from: u8, to: u8) {
    let Some(bus) = SHARED_BUS.try_get() else {
        return;
    };
    let Some(list) = SHARED_LIST.try_get() else {
        return;
    };
    let mut bus_guard = bus.lock().await;
    let mut list_guard = list.lock().await;
    bus_guard.scan(from, to, &mut list_guard).await;
    if list_guard.is_empty() {
        println!("ST3215 scan ({}..={}): no servos found", from, to);
    } else {
        print_servo_list(&list_guard);
    }
}

fn print_servo_list(list: &ServoList) {
    use core::fmt::Write;
    let mut s: heapless::String<128> = heapless::String::new();
    for (i, id) in list.iter().enumerate() {
        if i > 0 {
            let _ = s.push_str(", ");
        }
        let _ = write!(s, "{}", id);
    }
    println!("ST3215 servos found ({}): [{}]", list.len(), s);
}

// ---------------------------------------------------------------------------
// INA219 voltage monitor (two_motor only)
// ---------------------------------------------------------------------------

const INA219_ADDR: u8 = 0x42;
const INA219_REG_BUS_VOLTAGE: u8 = 0x02;

fn battery_percentage_3s(mv: u16) -> u8 {
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
    let mut i = 0;
    while i < TABLE.len() - 1 {
        let (v_hi, p_hi) = TABLE[i];
        let (v_lo, p_lo) = TABLE[i + 1];
        if mv >= v_lo {
            let pct =
                p_lo + ((mv - v_lo) as u32 * (p_hi - p_lo) as u32 / (v_hi - v_lo) as u32) as u16;
            return pct as u8;
        }
        i += 1;
    }
    0
}

#[embassy_executor::task]
async fn ina219_task(mut i2c: I2c<'static, esp_hal::Blocking>) {
    Timer::after(Duration::from_millis(200)).await;
    println!("INA219 task started (addr=0x{:02X})", INA219_ADDR);

    let mut check = [0u8; 2];
    match i2c.write_read(INA219_ADDR, &[0x00], &mut check) {
        Ok(()) => println!(
            "INA219 config register: 0x{:04X}",
            u16::from_be_bytes(check)
        ),
        Err(e) => println!("INA219 not responding: {:?}", e),
    }

    loop {
        wait_for_battery_sample_request().await;
        let mut buf = [0u8; 2];
        match i2c.write_read(INA219_ADDR, &[INA219_REG_BUS_VOLTAGE], &mut buf) {
            Ok(()) => {
                let raw = u16::from_be_bytes(buf);
                let voltage_mv = (raw >> 3) * 4;
                let pct = battery_percentage_3s(voltage_mv);
                esp32_http_servo::commands::BATTERY_MV
                    .store(voltage_mv, core::sync::atomic::Ordering::Relaxed);
                esp32_http_servo::commands::BATTERY_PCT
                    .store(pct, core::sync::atomic::Ordering::Relaxed);
                let volts = voltage_mv / 1000;
                let frac = (voltage_mv % 1000) / 10;
                println!("Battery: {}.{:02}V  {}%", volts, frac, pct);
            }
            Err(e) => {
                println!("INA219 read error: {:?}", e);
            }
        }
        complete_battery_sample_request();
    }
}

/// Command loop: receives commands and drives motors + ST3215 bus.
///
/// Motors are watchdogged at 500ms (auto-stop if no motor command). ST3215
/// servos hold position by themselves and are exempt from the watchdog.
async fn command_loop<M: MotorControl>(
    motors: &mut [M; MOTOR_COUNT],
    display_sender: &DisplaySender,
) -> ! {
    let mut last_powers = [0i8; MOTOR_COUNT];
    let mut last_motor_cmd = Instant::now();

    loop {
        match select(COMMANDS.receive(), Timer::after(Duration::from_millis(50))).await {
            Either::First(cmd) => match cmd {
                Command::Motor(id, power) => {
                    last_motor_cmd = Instant::now();
                    let idx = id as usize;
                    motors[idx].set_power(power);
                    update_motor(display_sender, idx, power);
                    last_powers[idx] = power;
                    println!("Motor {:?} set to {}%", id, power);
                }
                Command::MotorsAll(powers) => {
                    last_motor_cmd = Instant::now();
                    for (i, &p) in powers.iter().enumerate() {
                        motors[i].set_power(p);
                        update_motor(display_sender, i, p);
                    }
                    last_powers = powers;
                    #[cfg(feature = "four_motor")]
                    println!(
                        "Motors set to A={}% B={}% C={}% D={}%",
                        powers[0], powers[1], powers[2], powers[3]
                    );
                    #[cfg(feature = "two_motor")]
                    println!("Motors set to A={}% B={}%", powers[0], powers[1]);
                }
                Command::St3215Move {
                    id,
                    pos,
                    speed,
                    acc,
                } => {
                    if let Some(bus) = SHARED_BUS.try_get() {
                        let mut g = bus.lock().await;
                        match g.write_pos(id, pos, speed, acc).await {
                            Ok(()) => println!(
                                "ST3215[{}] -> pos={} speed={} acc={}",
                                id, pos, speed, acc
                            ),
                            Err(e) => println!("ST3215[{}] move error: {:?}", id, e),
                        }
                    }
                }
                Command::St3215MoveAll {
                    count,
                    moves,
                    speed,
                    acc,
                } => {
                    if let Some(bus) = SHARED_BUS.try_get() {
                        // Build the (id, pos, speed, acc) array for sync_write.
                        let mut buf: HVec<(u8, u16, u16, u8), MAX_SERVOS> = HVec::new();
                        for i in 0..count.min(MAX_SERVOS as u8) as usize {
                            let (id, pos) = moves[i];
                            let _ = buf.push((id, pos, speed, acc));
                        }
                        let mut g = bus.lock().await;
                        match g.sync_write_pos(&buf).await {
                            Ok(()) => println!(
                                "ST3215 sync_write {} servos @ speed={} acc={}",
                                buf.len(),
                                speed,
                                acc
                            ),
                            Err(e) => println!("ST3215 sync_write error: {:?}", e),
                        }
                    }
                }
                Command::St3215Torque { id, enable } => {
                    if let Some(bus) = SHARED_BUS.try_get() {
                        let mut g = bus.lock().await;
                        match g.set_torque(id, enable).await {
                            Ok(()) => println!("ST3215[{}] torque={}", id, enable),
                            Err(e) => println!("ST3215[{}] torque error: {:?}", id, e),
                        }
                    }
                }
                Command::St3215SetId { current, new } => {
                    if let Some(bus) = SHARED_BUS.try_get() {
                        let mut g = bus.lock().await;
                        match g.write_id(current, new).await {
                            Ok(()) => {
                                println!("ST3215 id {} -> {}", current, new);
                                drop(g);
                                st_rescan(1, 20).await;
                            }
                            Err(e) => println!("ST3215 set_id error: {:?}", e),
                        }
                    }
                }
                Command::St3215Ping { id } => {
                    if let Some(bus) = SHARED_BUS.try_get() {
                        let mut g = bus.lock().await;
                        match g.ping(id).await {
                            Ok(()) => println!("ST3215[{}] ping OK", id),
                            Err(e) => println!("ST3215[{}] ping error: {:?}", id, e),
                        }
                    }
                }
                Command::St3215Rescan { from, to } => {
                    st_rescan(from, to).await;
                }
            },
            Either::Second(_) => {
                if last_powers.iter().any(|&p| p != 0)
                    && last_motor_cmd.elapsed() >= Duration::from_millis(500)
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
