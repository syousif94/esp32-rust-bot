use crate::commands::{
    BATTERY_MV, BATTERY_PCT, Command, MOTOR_COUNT, MotorId, request_battery_sample, send_command,
};
use crate::st3215::{MAX_SERVOS, SHARED_BUS, SHARED_LIST};
use alloc::string::String;
use core::fmt::Write;
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::Duration;
use esp_println::println;
use heapless::Vec as HVec;
use static_cell::StaticCell;

/// Buffer sizes for HTTP server
const RX_BUFFER_SIZE: usize = 1024;
const TX_BUFFER_SIZE: usize = 1024;

/// The controller HTML page, included at compile time
const CONTROLLER_HTML: &str = include_str!("../controller.html");

/// Simple HTTP response builder
fn build_response(status: &str, content_type: &str, body: &str) -> alloc::string::String {
    alloc::format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    )
}

/// Parse the HTTP request and extract the method and path
fn parse_request(request: &str) -> Option<(&str, &str)> {
    let first_line = request.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path))
}

/// Parse angle from path like /servo/90 or /servo?angle=90
fn parse_servo_angle(path: &str) -> Option<u8> {
    // Try path format: /servo/90
    if let Some(angle_str) = path.strip_prefix("/servo/") {
        return angle_str.parse().ok();
    }

    // Try query format: /servo?angle=90
    if path.starts_with("/servo?") || path.starts_with("/servo?") {
        for part in path.split('?').nth(1)?.split('&') {
            if let Some(value) = part.strip_prefix("angle=") {
                return value.parse().ok();
            }
        }
    }

    None
}

/// Parse motor power from path like /motor/a/50 or /motor/a?power=50
/// Returns (motor_id, power) where motor_id is 'a', 'b', 'c', or 'd' and power is -100 to 100
fn parse_motor_power(path: &str) -> Option<(char, i8)> {
    // Try path format: /motor/a/50 or /motor/b/-50
    if let Some(rest) = path.strip_prefix("/motor/") {
        let mut parts = rest.split('/');
        let motor_id = parts.next()?.chars().next()?;
        if !is_valid_motor_id(motor_id) {
            return None;
        }
        if let Some(power_str) = parts.next() {
            let power: i8 = power_str.parse().ok()?;
            return Some((motor_id, power));
        }
    }

    // Try query format: /motor/a?power=50
    if path.starts_with("/motor/") {
        let rest = path.strip_prefix("/motor/")?;
        let motor_id = rest.chars().next()?;
        if !is_valid_motor_id(motor_id) {
            return None;
        }
        if rest.contains('?') {
            for part in rest.split('?').nth(1)?.split('&') {
                if let Some(value) = part.strip_prefix("power=") {
                    let power: i8 = value.parse().ok()?;
                    return Some((motor_id, power));
                }
            }
        }
    }

    None
}

/// Check whether a motor id char is valid for this build
fn is_valid_motor_id(c: char) -> bool {
    match c {
        'a' | 'b' => true,
        #[cfg(feature = "four_motor")]
        'c' | 'd' => true,
        _ => false,
    }
}

/// Parse batch motor powers from query string like /motors?a=50&b=-30&c=50&d=-30
/// Returns (Option<i8>, Option<i8>, Option<i8>, Option<i8>) for motors A, B, C, D
fn parse_motors_batch(path: &str) -> Option<[Option<i8>; MOTOR_COUNT]> {
    let query = path.split('?').nth(1)?;
    let mut result = [None::<i8>; MOTOR_COUNT];
    let mut found_any = false;
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix("a=") {
            if let Ok(p) = val.parse::<i8>() {
                if p >= -100 && p <= 100 {
                    result[0] = Some(p);
                    found_any = true;
                }
            }
        } else if let Some(val) = part.strip_prefix("b=") {
            if let Ok(p) = val.parse::<i8>() {
                if p >= -100 && p <= 100 {
                    result[1] = Some(p);
                    found_any = true;
                }
            }
        }
        #[cfg(feature = "four_motor")]
        {
            if let Some(val) = part.strip_prefix("c=") {
                if let Ok(p) = val.parse::<i8>() {
                    if p >= -100 && p <= 100 {
                        result[2] = Some(p);
                        found_any = true;
                    }
                }
            } else if let Some(val) = part.strip_prefix("d=") {
                if let Ok(p) = val.parse::<i8>() {
                    if p >= -100 && p <= 100 {
                        result[3] = Some(p);
                        found_any = true;
                    }
                }
            }
        }
    }
    if found_any { Some(result) } else { None }
}

/// Parse a u8 query parameter from a path.
fn query_u8(path: &str, name: &str) -> Option<u8> {
    let query = path.split('?').nth(1)?;
    for part in query.split('&') {
        if let Some((key, value)) = part.split_once('=')
            && key == name
        {
            return value.parse().ok();
        }
    }
    None
}

/// Parse a u16 query parameter from a path.
fn query_u16(path: &str, name: &str) -> Option<u16> {
    let query = path.split('?').nth(1)?;
    for part in query.split('&') {
        if let Some((key, value)) = part.split_once('=')
            && key == name
        {
            return value.parse().ok();
        }
    }
    None
}

fn parse_st_segments(path: &str) -> Option<[&str; 4]> {
    let route = path.split('?').next().unwrap_or(path);
    let rest = route.strip_prefix("/st/")?;
    let mut parts = rest.split('/');
    Some([
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    ])
}

fn validate_st_id(id: u8) -> bool {
    (1..=253).contains(&id)
}

fn json_error(status: &str, message: &str) -> HttpResponse {
    let body = alloc::format!(r#"{{"ok":false,"error":"{}"}}"#, message);
    HttpResponse::Small(build_response(status, "application/json", &body))
}

/// Build the JSON body used by /st/list and /st/scan.
fn st_list_body<I>(ids: I) -> String
where
    I: IntoIterator<Item = u8>,
{
    let mut body = String::from(r#"{"ids":["#);
    let mut first = true;
    for id in ids {
        if !first {
            let _ = body.push(',');
        }
        first = false;
        let _ = write!(body, "{}", id);
    }
    let _ = body.push_str("]}");
    body
}

/// Return the current shared ST3215 discovery list.
async fn st_list_response() -> HttpResponse {
    let Some(list) = SHARED_LIST.try_get() else {
        let body = r#"{"error":"ST3215 list not initialized"}"#;
        return HttpResponse::Small(build_response(
            "503 Service Unavailable",
            "application/json",
            body,
        ));
    };
    let list_guard = list.lock().await;
    let body = st_list_body(list_guard.iter().copied());
    HttpResponse::Small(build_response("200 OK", "application/json", &body))
}

/// Rescan the ST3215 bus and return the refreshed shared discovery list.
async fn st_scan_response(path: &str) -> HttpResponse {
    let from = query_u8(path, "from").unwrap_or(1);
    let to = query_u8(path, "to").unwrap_or(20);
    if from == 0 || to == 0 || from > to || to > 253 {
        let body = r#"{"error":"Scan range must satisfy 1 <= from <= to <= 253"}"#;
        return HttpResponse::Small(build_response("400 Bad Request", "application/json", body));
    }

    let Some(bus) = SHARED_BUS.try_get() else {
        let body = r#"{"error":"ST3215 bus not initialized"}"#;
        return HttpResponse::Small(build_response(
            "503 Service Unavailable",
            "application/json",
            body,
        ));
    };
    let Some(list) = SHARED_LIST.try_get() else {
        let body = r#"{"error":"ST3215 list not initialized"}"#;
        return HttpResponse::Small(build_response(
            "503 Service Unavailable",
            "application/json",
            body,
        ));
    };

    let mut bus_guard = bus.lock().await;
    let mut list_guard = list.lock().await;
    bus_guard.scan(from, to, &mut list_guard).await;
    let body = st_list_body(list_guard.iter().copied());
    HttpResponse::Small(build_response("200 OK", "application/json", &body))
}

async fn st_move_response(id: u8, pos: u16, path: &str) -> HttpResponse {
    if !validate_st_id(id) || pos > 4095 {
        return json_error(
            "400 Bad Request",
            "Servo ID must be 1-253 and position 0-4095",
        );
    }
    let speed = query_u16(path, "speed").unwrap_or(2000).min(4095);
    let acc = query_u8(path, "acc").unwrap_or(50);
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 bus not initialized");
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.write_pos(id, pos, speed, acc).await {
        Ok(()) => {
            let body = alloc::format!(
                r#"{{"ok":true,"id":{},"pos":{},"speed":{},"acc":{}}}"#,
                id,
                pos,
                speed,
                acc
            );
            HttpResponse::Small(build_response("200 OK", "application/json", &body))
        }
        Err(e) => {
            let body = alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e);
            HttpResponse::Small(build_response("502 Bad Gateway", "application/json", &body))
        }
    }
}

async fn st_torque_response(id: u8, enable: bool) -> HttpResponse {
    if !validate_st_id(id) {
        return json_error("400 Bad Request", "Servo ID must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 bus not initialized");
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.set_torque(id, enable).await {
        Ok(()) => {
            let body = alloc::format!(r#"{{"ok":true,"id":{},"torque":{}}}"#, id, enable);
            HttpResponse::Small(build_response("200 OK", "application/json", &body))
        }
        Err(e) => {
            let body = alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e);
            HttpResponse::Small(build_response("502 Bad Gateway", "application/json", &body))
        }
    }
}

async fn st_ping_response(id: u8) -> HttpResponse {
    if !validate_st_id(id) {
        return json_error("400 Bad Request", "Servo ID must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 bus not initialized");
    };

    let mut bus_guard = bus.lock().await;
    let body = match bus_guard.ping(id).await {
        Ok(()) => alloc::format!(r#"{{"ok":true,"id":{}}}"#, id),
        Err(e) => alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
    };
    HttpResponse::Small(build_response("200 OK", "application/json", &body))
}

async fn st_set_id_response(current: u8, new: u8) -> HttpResponse {
    if !validate_st_id(current) || !validate_st_id(new) {
        return json_error("400 Bad Request", "Servo IDs must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 bus not initialized");
    };
    let Some(list) = SHARED_LIST.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 list not initialized");
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.write_id(current, new).await {
        Ok(()) => {
            let mut list_guard = list.lock().await;
            bus_guard.scan(1, 20, &mut list_guard).await;
            let body = alloc::format!(r#"{{"ok":true,"current":{},"new":{}}}"#, current, new);
            HttpResponse::Small(build_response("200 OK", "application/json", &body))
        }
        Err(e) => {
            let body = alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, current, e);
            HttpResponse::Small(build_response("502 Bad Gateway", "application/json", &body))
        }
    }
}

async fn st_all_response(path: &str) -> HttpResponse {
    let speed = query_u16(path, "speed").unwrap_or(2000).min(4095);
    let acc = query_u8(path, "acc").unwrap_or(50);
    let mut moves: HVec<(u8, u16, u16, u8), MAX_SERVOS> = HVec::new();
    if let Some(query) = path.split('?').nth(1) {
        for part in query.split('&') {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            if key == "speed" || key == "acc" {
                continue;
            }
            let Ok(id) = key.parse::<u8>() else {
                continue;
            };
            let Ok(pos) = value.parse::<u16>() else {
                continue;
            };
            if validate_st_id(id) && pos <= 4095 {
                let _ = moves.push((id, pos, speed, acc));
            }
        }
    }
    if moves.is_empty() {
        return json_error(
            "400 Bad Request",
            "Provide at least one servo position, e.g. /st/all?1=2048",
        );
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 bus not initialized");
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.sync_write_pos(&moves).await {
        Ok(()) => {
            let body = alloc::format!(r#"{{"ok":true,"count":{}}}"#, moves.len());
            HttpResponse::Small(build_response("200 OK", "application/json", &body))
        }
        Err(e) => {
            let body = alloc::format!(r#"{{"ok":false,"error":"{:?}"}}"#, e);
            HttpResponse::Small(build_response("502 Bad Gateway", "application/json", &body))
        }
    }
}

async fn st_state_response() -> HttpResponse {
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 bus not initialized");
    };
    let Some(list) = SHARED_LIST.try_get() else {
        return json_error("503 Service Unavailable", "ST3215 list not initialized");
    };

    let mut ids: HVec<u8, MAX_SERVOS> = HVec::new();
    {
        let list_guard = list.lock().await;
        for id in list_guard.iter().copied() {
            let _ = ids.push(id);
        }
    }

    let mut body = String::from(r#"{"servos":["#);
    let mut first = true;
    let mut bus_guard = bus.lock().await;
    for id in ids {
        if !first {
            let _ = body.push(',');
        }
        first = false;
        match bus_guard.read_state(id).await {
            Ok(state) => {
                let _ = write!(
                    body,
                    r#"{{"id":{},"pos":{},"speed":{},"load":{},"voltage":{},"temp":{}}}"#,
                    id, state.pos, state.speed, state.load, state.voltage, state.temp
                );
            }
            Err(e) => {
                let _ = write!(body, r#"{{"id":{},"error":"{:?}"}}"#, id, e);
            }
        }
    }
    let _ = body.push_str("]}");
    HttpResponse::Small(build_response("200 OK", "application/json", &body))
}

async fn st_route_response(path: &str) -> HttpResponse {
    if path.starts_with("/st/all?") {
        return st_all_response(path).await;
    }
    if path == "/st/state" {
        return st_state_response().await;
    }

    let Some([id_s, action, value_s, _]) = parse_st_segments(path) else {
        return json_error("404 Not Found", "Unknown ST3215 route");
    };
    let Ok(id) = id_s.parse::<u8>() else {
        return json_error("400 Bad Request", "Servo ID must be 1-253");
    };
    match action {
        "pos" => match value_s.parse::<u16>() {
            Ok(pos) => st_move_response(id, pos, path).await,
            Err(_) => json_error("400 Bad Request", "Position must be 0-4095"),
        },
        "torque" => match value_s {
            "0" => st_torque_response(id, false).await,
            "1" => st_torque_response(id, true).await,
            _ => json_error("400 Bad Request", "Torque value must be 0 or 1"),
        },
        "ping" => st_ping_response(id).await,
        "id" => match value_s.parse::<u8>() {
            Ok(new) => st_set_id_response(id, new).await,
            Err(_) => json_error("400 Bad Request", "New servo ID must be 1-253"),
        },
        _ => json_error("404 Not Found", "Unknown ST3215 route"),
    }
}

/// Response type to avoid allocating the large HTML page on heap
enum HttpResponse {
    /// Small JSON/text response (heap allocated, fine for small bodies)
    Small(alloc::string::String),
    /// Serve the static HTML page - only headers are allocated, body streamed from flash
    StaticHtml,
}

/// Build just the HTTP headers for the static HTML page
fn build_html_headers() -> alloc::string::String {
    alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        CONTROLLER_HTML.len()
    )
}

/// Handle an incoming HTTP request and return a response
async fn handle_request(request: &str) -> HttpResponse {
    let Some((method, path)) = parse_request(request) else {
        return HttpResponse::Small(build_response(
            "400 Bad Request",
            "text/plain",
            "Bad Request",
        ));
    };

    println!("HTTP {} {}", method, path);

    match method {
        "GET" => {
            if path == "/" {
                return HttpResponse::StaticHtml;
            } else if path == "/favicon.ico" {
                HttpResponse::Small("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into())
            } else if path == "/health" {
                let body = r#"{"healthy": true}"#;
                HttpResponse::Small(build_response("200 OK", "application/json", body))
            } else if path == "/battery" {
                request_battery_sample(Duration::from_millis(100)).await;
                let mv = BATTERY_MV.load(core::sync::atomic::Ordering::Relaxed);
                let pct = BATTERY_PCT.load(core::sync::atomic::Ordering::Relaxed);
                let volts = mv / 1000;
                let frac = (mv % 1000) / 10;
                let body = alloc::format!(
                    r#"{{"voltage":"{}.{:02}","voltage_mv":{},"percentage":{}}}"#,
                    volts,
                    frac,
                    mv,
                    pct
                );
                HttpResponse::Small(build_response("200 OK", "application/json", &body))
            } else if path == "/config" {
                #[cfg(feature = "four_motor")]
                let body =
                    r#"{"motor_mode":"four_motor","motor_count":4,"motors":["a","b","c","d"]}"#;
                #[cfg(feature = "two_motor")]
                let body = r#"{"motor_mode":"two_motor","motor_count":2,"motors":["a","b"]}"#;
                HttpResponse::Small(build_response("200 OK", "application/json", body))
            } else if path.starts_with("/motors?") {
                if let Some(parsed) = parse_motors_batch(path) {
                    let mut powers = [0i8; MOTOR_COUNT];
                    for (i, opt) in parsed.iter().enumerate() {
                        powers[i] = opt.unwrap_or(0);
                    }
                    send_command(Command::MotorsAll(powers));
                    #[cfg(feature = "four_motor")]
                    let body = alloc::format!(
                        r#"{{"a":{},"b":{},"c":{},"d":{}}}"#,
                        parsed[0].map_or("null".into(), |v| alloc::format!("{}", v)),
                        parsed[1].map_or("null".into(), |v| alloc::format!("{}", v)),
                        parsed[2].map_or("null".into(), |v| alloc::format!("{}", v)),
                        parsed[3].map_or("null".into(), |v| alloc::format!("{}", v)),
                    );
                    #[cfg(feature = "two_motor")]
                    let body = alloc::format!(
                        r#"{{"a":{},"b":{}}}"#,
                        parsed[0].map_or("null".into(), |v| alloc::format!("{}", v)),
                        parsed[1].map_or("null".into(), |v| alloc::format!("{}", v)),
                    );
                    HttpResponse::Small(build_response("200 OK", "application/json", &body))
                } else {
                    let body =
                        r#"{"error": "Provide at least one motor param: /motors?a=50&b=-30"}"#;
                    HttpResponse::Small(build_response("400 Bad Request", "application/json", body))
                }
            } else if path.starts_with("/motor/") {
                if let Some((motor_id, power)) = parse_motor_power(path) {
                    if power >= -100 && power <= 100 {
                        let id = match motor_id {
                            'a' => MotorId::A,
                            'b' => MotorId::B,
                            #[cfg(feature = "four_motor")]
                            'c' => MotorId::C,
                            #[cfg(feature = "four_motor")]
                            'd' => MotorId::D,
                            _ => unreachable!(),
                        };
                        send_command(Command::Motor(id, power));
                        let body =
                            alloc::format!(r#"{{"motor": "{}", "power": {}}}"#, motor_id, power);
                        HttpResponse::Small(build_response("200 OK", "application/json", &body))
                    } else {
                        let body = r#"{"error": "Power must be between -100 and 100"}"#;
                        HttpResponse::Small(build_response(
                            "400 Bad Request",
                            "application/json",
                            body,
                        ))
                    }
                } else {
                    let body = r#"{"error": "Missing or invalid power parameter. Use /motor/a/50 or /motor/a?power=50"}"#;
                    HttpResponse::Small(build_response("400 Bad Request", "application/json", body))
                }
            } else if path == "/st/list" {
                st_list_response().await
            } else if path.starts_with("/st/scan") {
                st_scan_response(path).await
            } else if path.starts_with("/st/") {
                st_route_response(path).await
            } else if path.starts_with("/servo") {
                if let Some(angle) = parse_servo_angle(path) {
                    if angle <= 180 {
                        let pos = (angle as u16 * 4095) / 180;
                        send_command(Command::St3215Move {
                            id: 1,
                            pos,
                            speed: 1000,
                            acc: 50,
                        });
                        let body =
                            alloc::format!(r#"{{"angle": {}, "id": 1, "pos": {}}}"#, angle, pos);
                        HttpResponse::Small(build_response("200 OK", "application/json", &body))
                    } else {
                        let body = r#"{"error": "Angle must be between 0 and 180"}"#;
                        HttpResponse::Small(build_response(
                            "400 Bad Request",
                            "application/json",
                            body,
                        ))
                    }
                } else {
                    let body = r#"{"error": "Missing or invalid angle parameter. Use /servo/90 or /servo?angle=90"}"#;
                    HttpResponse::Small(build_response("400 Bad Request", "application/json", body))
                }
            } else {
                let body = r#"{"error": "Not Found"}"#;
                HttpResponse::Small(build_response("404 Not Found", "application/json", body))
            }
        }
        _ => {
            let body = r#"{"error": "Method Not Allowed"}"#;
            HttpResponse::Small(build_response(
                "405 Method Not Allowed",
                "application/json",
                body,
            ))
        }
    }
}

/// Run the HTTP server on port 80
#[embassy_executor::task]
pub async fn http_server_task(stack: Stack<'static>) {
    static RX_BUFFER: StaticCell<[u8; RX_BUFFER_SIZE]> = StaticCell::new();
    static TX_BUFFER: StaticCell<[u8; TX_BUFFER_SIZE]> = StaticCell::new();
    static REQUEST_BUFFER: StaticCell<[u8; RX_BUFFER_SIZE]> = StaticCell::new();

    let rx_buffer = RX_BUFFER.init([0u8; RX_BUFFER_SIZE]);
    let tx_buffer = TX_BUFFER.init([0u8; TX_BUFFER_SIZE]);
    let request_buffer = REQUEST_BUFFER.init([0u8; RX_BUFFER_SIZE]);

    println!(
        "HTTP server task started ([Heap] {} bytes free)",
        esp_alloc::HEAP.free()
    );

    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer[..], &mut tx_buffer[..]);
        socket.set_timeout(Some(Duration::from_secs(2)));

        println!(
            "HTTP server listening on port 80 ([Heap] {} bytes free)...",
            esp_alloc::HEAP.free()
        );

        if let Err(e) = socket.accept(80).await {
            println!("Accept error: {:?}", e);
            continue;
        }

        println!("Client connected");

        match socket.read(request_buffer).await {
            Ok(0) => {
                println!("Client disconnected");
            }
            Ok(n) => {
                if let Ok(request) = core::str::from_utf8(&request_buffer[..n]) {
                    match handle_request(request).await {
                        HttpResponse::Small(response) => {
                            let mut offset = 0;
                            let bytes = response.as_bytes();
                            while offset < bytes.len() {
                                match socket.write(&bytes[offset..]).await {
                                    Ok(written) => offset += written,
                                    Err(e) => {
                                        println!("Write error: {:?}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        HttpResponse::StaticHtml => {
                            // Write headers (small heap alloc, ~150 bytes)
                            let headers = build_html_headers();
                            let mut offset = 0;
                            let bytes = headers.as_bytes();
                            while offset < bytes.len() {
                                match socket.write(&bytes[offset..]).await {
                                    Ok(written) => offset += written,
                                    Err(e) => {
                                        println!("Write error: {:?}", e);
                                        break;
                                    }
                                }
                            }
                            // Stream HTML body directly from flash, no heap alloc
                            let mut offset = 0;
                            let bytes = CONTROLLER_HTML.as_bytes();
                            while offset < bytes.len() {
                                match socket.write(&bytes[offset..]).await {
                                    Ok(written) => offset += written,
                                    Err(e) => {
                                        println!("Write error: {:?}", e);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("Read error: {:?}", e);
            }
        }

        socket.flush().await.ok();
        socket.close();
    }
}
