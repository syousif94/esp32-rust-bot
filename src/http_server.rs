use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_time::Duration;
use esp_println::println;
use crate::commands::{Command, MotorId, send_command, MOTOR_COUNT};

/// Buffer sizes for HTTP server
const RX_BUFFER_SIZE: usize = 1024;
const TX_BUFFER_SIZE: usize = 4096;

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
                if p >= -100 && p <= 100 { result[0] = Some(p); found_any = true; }
            }
        } else if let Some(val) = part.strip_prefix("b=") {
            if let Ok(p) = val.parse::<i8>() {
                if p >= -100 && p <= 100 { result[1] = Some(p); found_any = true; }
            }
        }
        #[cfg(feature = "four_motor")]
        {
            if let Some(val) = part.strip_prefix("c=") {
                if let Ok(p) = val.parse::<i8>() {
                    if p >= -100 && p <= 100 { result[2] = Some(p); found_any = true; }
                }
            } else if let Some(val) = part.strip_prefix("d=") {
                if let Ok(p) = val.parse::<i8>() {
                    if p >= -100 && p <= 100 { result[3] = Some(p); found_any = true; }
                }
            }
        }
    }
    if found_any { Some(result) } else { None }
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
fn handle_request(request: &str) -> HttpResponse {
    let Some((method, path)) = parse_request(request) else {
        return HttpResponse::Small(build_response("400 Bad Request", "text/plain", "Bad Request"));
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
            } else if path == "/config" {
                #[cfg(feature = "four_motor")]
                let body = r#"{"motor_mode":"four_motor","motor_count":4,"motors":["a","b","c","d"]}"#;
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
                    let body = r#"{"error": "Provide at least one motor param: /motors?a=50&b=-30"}"#;
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
                        let body = alloc::format!(r#"{{"motor": "{}", "power": {}}}"#, motor_id, power);
                        HttpResponse::Small(build_response("200 OK", "application/json", &body))
                    } else {
                        let body = r#"{"error": "Power must be between -100 and 100"}"#;
                        HttpResponse::Small(build_response("400 Bad Request", "application/json", body))
                    }
                } else {
                    let body = r#"{"error": "Missing or invalid power parameter. Use /motor/a/50 or /motor/a?power=50"}"#;
                    HttpResponse::Small(build_response("400 Bad Request", "application/json", body))
                }
            } else if path.starts_with("/servo") {
                if let Some(angle) = parse_servo_angle(path) {
                    if angle <= 180 {
                        send_command(Command::Servo(angle));
                        let body = alloc::format!(r#"{{"angle": {}}}"#, angle);
                        HttpResponse::Small(build_response("200 OK", "application/json", &body))
                    } else {
                        let body = r#"{"error": "Angle must be between 0 and 180"}"#;
                        HttpResponse::Small(build_response("400 Bad Request", "application/json", body))
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
            HttpResponse::Small(build_response("405 Method Not Allowed", "application/json", body))
        }
    }
}

/// Run the HTTP server on port 80
#[embassy_executor::task]
pub async fn http_server_task(stack: Stack<'static>) {
    let mut rx_buffer = [0u8; RX_BUFFER_SIZE];
    let mut tx_buffer = [0u8; TX_BUFFER_SIZE];

    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(2)));

        println!("HTTP server listening on port 80...");

        if let Err(e) = socket.accept(80).await {
            println!("Accept error: {:?}", e);
            continue;
        }

        println!("Client connected");

        let mut buf = [0u8; RX_BUFFER_SIZE];
        match socket.read(&mut buf).await {
            Ok(0) => {
                println!("Client disconnected");
            }
            Ok(n) => {
                if let Ok(request) = core::str::from_utf8(&buf[..n]) {
                    match handle_request(request) {
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
