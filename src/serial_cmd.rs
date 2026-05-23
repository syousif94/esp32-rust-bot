//! Serial console command interface.
//!
//! Grammar:
//!   m <power>                       — set ALL motors to power
//!   ma <power> | mb <power>         — set Motor A / B (-100..=100)
//!   mc <power> | md <power>         — Motor C / D (four_motor only)
//!   motor <a|b|c|d> <power>         — verbose form
//!   st list                        — print discovered servo IDs
//!   st scan [from to]              — re-scan the bus
//!   st all <id>=<pos> ...          — atomic sync_write move
//!     [speed <s>] [acc <a>]
//!   st <id> pos <v>                — move servo to pos 0..=4095
//!     [speed <s>] [acc <a>]         (alias: st <id> p <v>)
//!   st <id> torque <0|1>           — enable/disable torque (alias t)
//!   st <id> setid <new>            — change servo ID (auto-rescans)
//!   st <id> ping                   — ping a single servo
//!   st <id> state                  — read pos/speed/load/V/T
//!   wifi <ssid> <password>         — save WiFi credentials
//!   wi | ble                       — switch radio preference / WiFi link

use embassy_time::{Duration, Timer};
use esp_hal::Blocking;
use esp_hal::uart::Uart;
use esp_println::println;

use crate::ble::BLE_WIFI_CREDENTIALS;
use crate::commands::{Command, MOTOR_COUNT, MotorId, send_command};
use crate::st3215::MAX_SERVOS;
use crate::wifi_config::{MAX_PASSWORD_LEN, MAX_SSID_LEN, RADIO_MODE_REQUEST, RadioMode};

const ST_DEFAULT_SPEED: u16 = 1500;
const ST_DEFAULT_ACC: u8 = 50;
const ST_MAX_POS: u16 = 4095;

fn parse_motor_token(input: &str) -> Option<(char, i8)> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("m ") {
        if let Ok(power) = rest.trim().parse::<i8>() {
            if (-100..=100).contains(&power) {
                return Some(('*', power));
            }
        }
    }
    #[cfg(feature = "four_motor")]
    let motor_prefixes: &[(&str, char)] = &[("ma ", 'a'), ("mb ", 'b'), ("mc ", 'c'), ("md ", 'd')];
    #[cfg(feature = "two_motor")]
    let motor_prefixes: &[(&str, char)] = &[("ma ", 'a'), ("mb ", 'b')];
    for &(prefix, motor_id) in motor_prefixes {
        if let Some(rest) = input.strip_prefix(prefix) {
            if let Ok(power) = rest.trim().parse::<i8>() {
                if (-100..=100).contains(&power) {
                    return Some((motor_id, power));
                }
            }
        }
    }
    if let Some(rest) = input.strip_prefix("motor ") {
        let rest = rest.trim();
        #[cfg(feature = "four_motor")]
        let inner: &[(&str, char)] = &[("a ", 'a'), ("b ", 'b'), ("c ", 'c'), ("d ", 'd')];
        #[cfg(feature = "two_motor")]
        let inner: &[(&str, char)] = &[("a ", 'a'), ("b ", 'b')];
        for &(prefix, motor_id) in inner {
            if let Some(power_str) = rest.strip_prefix(prefix) {
                if let Ok(power) = power_str.trim().parse::<i8>() {
                    if (-100..=100).contains(&power) {
                        return Some((motor_id, power));
                    }
                }
            }
        }
    }
    None
}

/// Tokenize on whitespace, max 32 tokens.
fn tokens(input: &str) -> heapless::Vec<&str, 32> {
    let mut v: heapless::Vec<&str, 32> = heapless::Vec::new();
    for tok in input.split_whitespace() {
        if v.push(tok).is_err() {
            break;
        }
    }
    v
}

/// Parse and dispatch an `st ...` subcommand. Returns `true` if recognized.
fn parse_st(input: &str) -> bool {
    let Some(rest) = input.trim().strip_prefix("st") else {
        return false;
    };
    let rest = rest.trim();
    let toks = tokens(rest);
    let tcount = toks.len();
    if tcount == 0 {
        println!("usage: st list | scan [from to] | all <id>=<pos> ... | <id> <op> ...");
        return true;
    }

    match toks[0] {
        "list" => {
            send_command(Command::St3215Rescan { from: 1, to: 20 });
            println!("Serial: rescanning bus (list will print in main loop)");
            return true;
        }
        "scan" => {
            let from = toks.get(1).and_then(|s| s.parse().ok()).unwrap_or(1u8);
            let to = toks.get(2).and_then(|s| s.parse().ok()).unwrap_or(20u8);
            send_command(Command::St3215Rescan { from, to });
            println!("Serial: scan {}..={}", from, to);
            return true;
        }
        "all" => {
            // st all 1=2048 2=1024 [speed 1500] [acc 50]
            let mut moves: [(u8, u16); MAX_SERVOS] = [(0, 0); MAX_SERVOS];
            let mut count = 0u8;
            let mut speed = ST_DEFAULT_SPEED;
            let mut acc = ST_DEFAULT_ACC;
            let mut i = 1;
            while i < tcount {
                let tok = toks[i];
                if tok == "speed" {
                    if let Some(v) = toks.get(i + 1).and_then(|s| s.parse().ok()) {
                        speed = v;
                        i += 2;
                        continue;
                    }
                } else if tok == "acc" {
                    if let Some(v) = toks.get(i + 1).and_then(|s| s.parse().ok()) {
                        acc = v;
                        i += 2;
                        continue;
                    }
                } else if let Some((id_s, pos_s)) = tok.split_once('=') {
                    if let (Ok(id), Ok(pos)) = (id_s.parse::<u8>(), pos_s.parse::<u16>()) {
                        if pos <= ST_MAX_POS && (count as usize) < MAX_SERVOS {
                            moves[count as usize] = (id, pos);
                            count += 1;
                        }
                    }
                }
                i += 1;
            }
            if count == 0 {
                println!("usage: st all <id>=<pos> ... [speed <s>] [acc <a>]");
                return true;
            }
            send_command(Command::St3215MoveAll {
                count,
                moves,
                speed,
                acc,
            });
            println!(
                "Serial: st all {} moves @ speed={} acc={}",
                count, speed, acc
            );
            return true;
        }
        _ => {}
    }

    // st <id> <op> ...
    let Ok(id) = toks[0].parse::<u8>() else {
        println!("Unknown st subcommand: '{}'", toks[0]);
        return true;
    };
    if tcount < 2 {
        println!("usage: st <id> <pos|p|torque|t|setid|ping|state> ...");
        return true;
    }
    match toks[1] {
        "pos" | "p" => {
            let Some(pos) = toks.get(2).and_then(|s| s.parse::<u16>().ok()) else {
                println!("usage: st <id> pos <0..=4095> [speed <s>] [acc <a>]");
                return true;
            };
            if pos > ST_MAX_POS {
                println!("pos must be 0..=4095");
                return true;
            }
            let mut speed = ST_DEFAULT_SPEED;
            let mut acc = ST_DEFAULT_ACC;
            let mut i = 3;
            while i < tcount {
                match toks[i] {
                    "speed" => {
                        if let Some(v) = toks.get(i + 1).and_then(|s| s.parse().ok()) {
                            speed = v;
                        }
                        i += 2;
                    }
                    "acc" => {
                        if let Some(v) = toks.get(i + 1).and_then(|s| s.parse().ok()) {
                            acc = v;
                        }
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            send_command(Command::St3215Move {
                id,
                pos,
                speed,
                acc,
            });
            println!("Serial: st {} pos={} speed={} acc={}", id, pos, speed, acc);
        }
        "torque" | "t" => {
            let Some(v) = toks.get(2) else {
                println!("usage: st <id> torque <0|1>");
                return true;
            };
            let enable = matches!(*v, "1" | "on" | "true");
            send_command(Command::St3215Torque { id, enable });
            println!("Serial: st {} torque={}", id, enable);
        }
        "setid" => {
            let Some(new) = toks.get(2).and_then(|s| s.parse::<u8>().ok()) else {
                println!("usage: st <id> setid <new>");
                return true;
            };
            if !(1..=253).contains(&new) {
                println!("new id must be 1..=253");
                return true;
            }
            send_command(Command::St3215SetId { current: id, new });
            println!("Serial: st {} setid {} (rescan will follow)", id, new);
        }
        "ping" => {
            send_command(Command::St3215Ping { id });
            println!("Serial: st {} ping", id);
        }
        "state" => {
            // Routed via HTTP/BLE for sync reads; serial just pings to log presence.
            send_command(Command::St3215Ping { id });
            println!(
                "Serial: st {} ping (use HTTP /st/{}/state for full state)",
                id, id
            );
        }
        other => {
            println!("Unknown st op: '{}'", other);
        }
    }
    true
}

fn dispatch(cmd: &str) {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }

    if cmd == "wi" {
        RADIO_MODE_REQUEST.signal(RadioMode::Wifi);
        println!("\nSerial: saving WiFi mode and rebooting");
        return;
    }

    if cmd == "ble" {
        RADIO_MODE_REQUEST.signal(RadioMode::Ble);
        println!("\nSerial: saving BLE mode and rebooting");
        return;
    }

    if let Some(rest) = cmd.strip_prefix("wifi ") {
        let mut parts = rest.split_whitespace();
        let Some(ssid_part) = parts.next() else {
            println!("usage: wifi <ssid> <password>");
            return;
        };
        let Some(password_part) = parts.next() else {
            println!("usage: wifi <ssid> <password>");
            return;
        };
        if parts.next().is_some() {
            println!("usage: wifi <ssid> <password>");
            return;
        }

        let Ok(ssid) = heapless::String::<MAX_SSID_LEN>::try_from(ssid_part) else {
            println!("WiFi SSID too long (max {} bytes)", MAX_SSID_LEN);
            return;
        };
        let Ok(password) = heapless::String::<MAX_PASSWORD_LEN>::try_from(password_part) else {
            println!("WiFi password too long (max {} bytes)", MAX_PASSWORD_LEN);
            return;
        };

        BLE_WIFI_CREDENTIALS.signal((ssid, password));
        println!("\nSerial: WiFi credentials queued");
        return;
    }

    // Try motor commands first
    if let Some((motor, power)) = parse_motor_token(cmd) {
        match motor {
            'a' => {
                println!("\nSerial: Motor A = {}%", power);
                send_command(Command::Motor(MotorId::A, power));
            }
            'b' => {
                println!("\nSerial: Motor B = {}%", power);
                send_command(Command::Motor(MotorId::B, power));
            }
            #[cfg(feature = "four_motor")]
            'c' => {
                println!("\nSerial: Motor C = {}%", power);
                send_command(Command::Motor(MotorId::C, power));
            }
            #[cfg(feature = "four_motor")]
            'd' => {
                println!("\nSerial: Motor D = {}%", power);
                send_command(Command::Motor(MotorId::D, power));
            }
            '*' => {
                println!("\nSerial: All motors = {}%", power);
                send_command(Command::MotorsAll([power; MOTOR_COUNT]));
            }
            _ => {}
        }
        return;
    }

    if parse_st(cmd) {
        return;
    }

    println!("\nUnknown command: '{}'", cmd);
    println!(
        "  Motors: m <p>, ma/mb{} <p>",
        if cfg!(feature = "four_motor") {
            "/mc/md"
        } else {
            ""
        }
    );
    println!(
        "  Servos: st list | st scan | st all <id>=<pos> ... | st <id> pos <v> | st <id> torque <0|1> | st <id> setid <new> | st <id> ping"
    );
    println!("  Radio: wifi <ssid> <password> | wi | ble");
}

#[embassy_executor::task]
pub async fn serial_input_task(mut uart: Uart<'static, Blocking>) {
    println!("Serial command interface ready");
    println!(
        "  Motors: m <p>, ma/mb{} <p>",
        if cfg!(feature = "four_motor") {
            "/mc/md"
        } else {
            ""
        }
    );
    println!(
        "  Servos: st list | st scan | st all <id>=<pos> ... | st <id> pos <v> | st <id> torque <0|1> | st <id> setid <new> | st <id> ping"
    );
    println!("  Radio: wifi <ssid> <password> | wi | ble");

    let mut buffer = [0u8; 128];
    let mut pos = 0usize;
    let mut read_buf = [0u8; 1];

    loop {
        if uart.read_ready() {
            match uart.read(&mut read_buf) {
                Ok(1) => {
                    let byte = read_buf[0];
                    let _ = uart.write(&[byte]);
                    if byte == b'\r' || byte == b'\n' {
                        if pos > 0 {
                            if let Ok(cmd) = core::str::from_utf8(&buffer[..pos]) {
                                dispatch(cmd);
                            }
                            pos = 0;
                        }
                        println!("");
                    } else if pos < buffer.len() - 1 {
                        buffer[pos] = byte;
                        pos += 1;
                    }
                }
                _ => {}
            }
        } else {
            Timer::after(Duration::from_millis(10)).await;
        }
    }
}
