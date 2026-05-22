//! WiFi connection management tasks.
//!
//! Contains the WiFi connection state machine, IP-readiness monitor,
//! credential persistence task, and the network stack runner.

use core::pin::pin;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::Runner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_println::println;
use esp_radio::wifi::{
    ClientConfig, ModeConfig, WifiController, WifiDevice, WifiEvent, WifiStaState, sta_state,
};
use esp_storage::FlashStorage;

use crate::ble::BLE_WIFI_CREDENTIALS;
use crate::display::{
    DisplaySender, WifiStatus, set_line1_override, update_dots, update_ip, update_ssid,
    update_status,
};
use crate::http_server::http_server_task;
use crate::wifi_config::{
    MAX_PASSWORD_LEN, MAX_SSID_LEN, read_wifi_credentials, write_wifi_credentials,
};

/// Compile-time default WiFi credentials (from cfg.toml / environment)
const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

/// Signal to trigger WiFi reconnection with new credentials.
/// Contains (ssid, password) as heapless strings.
pub static WIFI_RECONNECT: Signal<
    CriticalSectionRawMutex,
    (
        heapless::String<MAX_SSID_LEN>,
        heapless::String<MAX_PASSWORD_LEN>,
    ),
> = Signal::new();

/// Return the compile-time default SSID (for display init, etc.)
pub fn default_ssid() -> &'static str {
    SSID
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// Persist WiFi credentials received via BLE to flash.
///
/// On startup, reads any previously-stored credentials and signals the
/// connection task to use them.
#[embassy_executor::task]
pub async fn wifi_config_task(
    flash_storage: &'static mut FlashStorage<'static>,
    display_sender: DisplaySender,
) {
    // Check for stored credentials at startup and signal connection task
    if let Some(creds) = read_wifi_credentials(flash_storage) {
        println!(
            "[WiFi Config] Found stored credentials: SSID='{}'",
            creds.ssid.as_str()
        );
        update_ssid(&display_sender, creds.ssid.as_str());
        WIFI_RECONNECT.signal((creds.ssid, creds.password));
    }

    loop {
        // Wait for WiFi credentials from BLE
        let (ssid, password) = BLE_WIFI_CREDENTIALS.wait().await;
        println!(
            "[WiFi Config] Received WiFi credentials via BLE: SSID='{}'",
            ssid.as_str()
        );

        match write_wifi_credentials(flash_storage, ssid.as_str(), password.as_str()) {
            Ok(()) => {
                println!(
                    "[WiFi Config] Credentials saved successfully: SSID='{}'",
                    ssid.as_str()
                );
                update_ssid(&display_sender, ssid.as_str());
                set_line1_override(&display_sender, "WiFi Saved!");
                WIFI_RECONNECT.signal((ssid, password));
            }
            Err(e) => {
                println!(
                    "[WiFi Config] Failed to save credentials for SSID='{}': {}",
                    ssid.as_str(),
                    e
                );
                set_line1_override(&display_sender, "Save Failed!");
            }
        }
    }
}

/// Wait for WiFi link + DHCP, then spawn the HTTP server.
#[embassy_executor::task]
pub async fn wifi_ready_task(
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
    spawner.spawn(http_server_task(stack)).ok();
}

/// WiFi connect / reconnect state machine.
///
/// Starts with compile-time defaults, listens for credential changes from
/// [`WIFI_RECONNECT`], and handles disconnect + reconfigure automatically.
#[embassy_executor::task]
pub async fn wifi_connection_task(
    mut controller: WifiController<'static>,
    display_sender: DisplaySender,
) {
    println!("Start connection task");
    println!("Device capabilities: {:?}", controller.capabilities());

    let mut current_ssid: heapless::String<MAX_SSID_LEN> =
        heapless::String::try_from(SSID).unwrap_or_default();
    let mut current_password: heapless::String<MAX_PASSWORD_LEN> =
        heapless::String::try_from(PASSWORD).unwrap_or_default();

    match select(WIFI_RECONNECT.wait(), Timer::after(Duration::from_millis(500))).await {
        Either::First((ssid, password)) => {
            println!(
                "[WiFi] Using stored credentials from boot: SSID='{}'",
                ssid.as_str()
            );
            current_ssid = ssid;
            current_password = password;
        }
        Either::Second(_) => {
            println!(
                "[WiFi] No stored credentials, using defaults: SSID='{}'",
                current_ssid.as_str()
            );
        }
    }

    let mut needs_reconfigure = true;

    loop {
        // Check if new credentials are available (non-blocking check)
        if WIFI_RECONNECT.signaled() {
            let (new_ssid, new_password) = WIFI_RECONNECT.wait().await;
            println!(
                "[WiFi] New credentials received: SSID='{}'",
                new_ssid.as_str()
            );

            if new_ssid.as_str() != current_ssid.as_str()
                || new_password.as_str() != current_password.as_str()
            {
                current_ssid = new_ssid;
                current_password = new_password;
                needs_reconfigure = true;

                if matches!(sta_state(), WifiStaState::Connected) {
                    println!("[WiFi] Disconnecting to switch networks...");
                    let _ = controller.disconnect_async().await;
                    Timer::after(Duration::from_millis(1000)).await;
                }
                if matches!(controller.is_started(), Ok(true)) {
                    println!("[WiFi] Stopping WiFi for reconfiguration...");
                    let _ = controller.stop_async().await;
                    Timer::after(Duration::from_millis(500)).await;
                }
            }
        }

        match sta_state() {
            WifiStaState::Connected => {
                match select(
                    controller.wait_for_event(WifiEvent::StaDisconnected),
                    WIFI_RECONNECT.wait(),
                )
                .await
                {
                    Either::First(_) => {
                        println!("[WiFi] Disconnected from network");
                        Timer::after(Duration::from_millis(5000)).await;
                    }
                    Either::Second((new_ssid, new_password)) => {
                        println!(
                            "[WiFi] New credentials while connected: SSID='{}'",
                            new_ssid.as_str()
                        );
                        if new_ssid.as_str() != current_ssid.as_str()
                            || new_password.as_str() != current_password.as_str()
                        {
                            current_ssid = new_ssid;
                            current_password = new_password;
                            needs_reconfigure = true;

                            println!("[WiFi] Disconnecting to switch networks...");
                            let _ = controller.disconnect_async().await;
                            Timer::after(Duration::from_millis(1000)).await;

                            if matches!(controller.is_started(), Ok(true)) {
                                let _ = controller.stop_async().await;
                                Timer::after(Duration::from_millis(500)).await;
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        if needs_reconfigure || !matches!(controller.is_started(), Ok(true)) {
            let client_config = ModeConfig::Client(
                ClientConfig::default()
                    .with_ssid(current_ssid.as_str().try_into().unwrap())
                    .with_password(current_password.as_str().try_into().unwrap()),
            );
            controller.set_config(&client_config).unwrap();
            needs_reconfigure = false;
            println!("Starting WiFi...");
            controller.start_async().await.unwrap();
            println!("WiFi started!");
        }

        println!("Connecting to WiFi network: {}", current_ssid.as_str());

        let mut period_count: u8 = 0;
        let mut new_creds_arrived = false;
        let mut connect_future = pin!(controller.connect_async());

        let result = loop {
            match select(
                &mut connect_future,
                Timer::after(Duration::from_millis(1000)),
            )
            .await
            {
                Either::First(result) => break Some(result),
                Either::Second(_) => {
                    // Check for new credentials during connection attempt
                    if WIFI_RECONNECT.signaled() {
                        println!("[WiFi] New credentials during connection, will restart");
                        new_creds_arrived = true;
                        break None;
                    }

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

        if new_creds_arrived {
            continue;
        }

        match result {
            Some(Ok(_)) => println!("WiFi connected!"),
            Some(Err(e)) => {
                println!("Failed to connect to WiFi: {:?}", e);
                Timer::after(Duration::from_millis(5000)).await;
            }
            None => {
                // Connection aborted due to new credentials
            }
        }
    }
}

/// Trivial embassy-net runner task.
#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
