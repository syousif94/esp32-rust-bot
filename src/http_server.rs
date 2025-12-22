use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_time::Duration;
use esp_println::println;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

/// Buffer sizes for HTTP server
const RX_BUFFER_SIZE: usize = 1024;
const TX_BUFFER_SIZE: usize = 1024;

/// Signal for servo angle updates
pub static SERVO_ANGLE: Signal<CriticalSectionRawMutex, u8> = Signal::new();

/// Signal for motor A power updates (-100 to +100)
pub static MOTOR_A_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor B power updates (-100 to +100)
pub static MOTOR_B_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor C power updates (-100 to +100)
pub static MOTOR_C_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Signal for motor D power updates (-100 to +100)
pub static MOTOR_D_POWER: Signal<CriticalSectionRawMutex, i8> = Signal::new();

/// Simple HTTP response builder
fn build_response(status: &str, content_type: &str, body: &str) -> alloc::string::String {
    alloc::format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
        if motor_id != 'a' && motor_id != 'b' && motor_id != 'c' && motor_id != 'd' {
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
        if motor_id != 'a' && motor_id != 'b' && motor_id != 'c' && motor_id != 'd' {
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

/// Handle an incoming HTTP request and return a response
fn handle_request(request: &str) -> alloc::string::String {
    let Some((method, path)) = parse_request(request) else {
        return build_response("400 Bad Request", "text/plain", "Bad Request");
    };

    println!("HTTP {} {}", method, path);

    match method {
        "GET" => {
            if path == "/" {
                let body = r#"{"status": "ok", "message": "ESP32 Motor & Servo Controller", "endpoints": ["/servo/<angle>", "/servo?angle=<0-180>", "/motor/a/<power>", "/motor/b/<power>", "/motor/c/<power>", "/motor/d/<power>", "/motor/<a|b|c|d>?power=<-100 to 100>"]}"#;
                build_response("200 OK", "application/json", body)
            } else if path == "/health" {
                let body = r#"{"healthy": true}"#;
                build_response("200 OK", "application/json", body)
            } else if path.starts_with("/motor/") {
                if let Some((motor_id, power)) = parse_motor_power(path) {
                    if power >= -100 && power <= 100 {
                        match motor_id {
                            'a' => MOTOR_A_POWER.signal(power),
                            'b' => MOTOR_B_POWER.signal(power),
                            'c' => MOTOR_C_POWER.signal(power),
                            'd' => MOTOR_D_POWER.signal(power),
                            _ => unreachable!(),
                        }
                        let body = alloc::format!(r#"{{"motor": "{}", "power": {}}}"#, motor_id, power);
                        build_response("200 OK", "application/json", &body)
                    } else {
                        let body = r#"{"error": "Power must be between -100 and 100"}"#;
                        build_response("400 Bad Request", "application/json", body)
                    }
                } else {
                    let body = r#"{"error": "Missing or invalid power parameter. Use /motor/a/50 or /motor/a?power=50"}"#;
                    build_response("400 Bad Request", "application/json", body)
                }
            } else if path.starts_with("/servo") {
                if let Some(angle) = parse_servo_angle(path) {
                    if angle <= 180 {
                        SERVO_ANGLE.signal(angle);
                        let body = alloc::format!(r#"{{"angle": {}}}"#, angle);
                        build_response("200 OK", "application/json", &body)
                    } else {
                        let body = r#"{"error": "Angle must be between 0 and 180"}"#;
                        build_response("400 Bad Request", "application/json", body)
                    }
                } else {
                    let body = r#"{"error": "Missing or invalid angle parameter. Use /servo/90 or /servo?angle=90"}"#;
                    build_response("400 Bad Request", "application/json", body)
                }
            } else {
                let body = r#"{"error": "Not Found"}"#;
                build_response("404 Not Found", "application/json", body)
            }
        }
        _ => {
            let body = r#"{"error": "Method Not Allowed"}"#;
            build_response("405 Method Not Allowed", "application/json", body)
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
        socket.set_timeout(Some(Duration::from_secs(10)));

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
                    let response = handle_request(request);
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
            }
            Err(e) => {
                println!("Read error: {:?}", e);
            }
        }

        socket.close();
        // Small delay before accepting next connection
        embassy_time::Timer::after(Duration::from_millis(100)).await;
    }
}
