use crate::commands::{
    BATTERY_MV, BATTERY_PCT, Command, MOTOR_COUNT, MotorId, request_battery_sample, send_command,
};
use crate::st3215::{MAX_SERVOS, SHARED_BUS, SHARED_LIST};
use crate::wifi_config::{RADIO_MODE_REQUEST, RadioMode};
use alloc::string::String;
use core::fmt::Write as FmtWrite;
use embassy_net::Stack;
use embassy_time::Duration;
use esp_println::println;
use heapless::Vec as HVec;
use picoserve::io::{Read, Write};
use picoserve::request::Request;
use picoserve::response::{Content, File, IntoResponse, NoContent, ResponseWriter, StatusCode};
use picoserve::routing::{self, RequestHandlerService, parse_path_segment};
use picoserve::{AppBuilder, AppRouter, ResponseSent, Router};

pub const WEB_TASK_POOL_SIZE: usize = 3;

const TCP_RX_BUFFER_SIZE: usize = 1024;
const TCP_TX_BUFFER_SIZE: usize = 1024;
const HTTP_BUFFER_SIZE: usize = 2048;
const CONTROLLER_HTML: &str = include_str!("../controller.html");

type Query<'a> = Option<picoserve::url_encoded::UrlEncodedString<'a>>;
type JsonResponse = (StatusCode, (&'static str, &'static str), JsonBody);

pub struct Application;

impl AppBuilder for Application {
    type PathRouter = impl routing::PathRouter;

    fn build_app(self) -> Router<Self::PathRouter> {
        Router::new()
            .route("/", routing::get_service(File::html(CONTROLLER_HTML)))
            .route(
                "/favicon.ico",
                routing::get(async || (StatusCode::NO_CONTENT, NoContent)),
            )
            .route("/health", routing::get(health_response))
            .route("/battery", routing::get(battery_response))
            .route("/config", routing::get(config_response))
            .route("/radio/ble", routing::get(radio_ble_response))
            .route("/motors", routing::get_service(MotorsService))
            .route(
                ("/motor", parse_path_segment::<char>()),
                routing::get_service(MotorQueryService),
            )
            .route(
                (
                    "/motor",
                    parse_path_segment::<char>(),
                    parse_path_segment::<i8>(),
                ),
                motor_power_handler(),
            )
            .route("/servo", routing::get_service(ServoQueryService))
            .route(
                ("/servo", parse_path_segment::<u8>()),
                servo_angle_handler(),
            )
            .route("/st/list", routing::get(st_list_response))
            .route("/st/scan", routing::get_service(StScanService))
            .route("/st/all", routing::get_service(StAllService))
            .route("/st/state", routing::get(st_state_response))
            .route(
                (
                    "/st",
                    parse_path_segment::<u8>(),
                    "/pos",
                    parse_path_segment::<u16>(),
                ),
                routing::get_service(StMoveService),
            )
            .route(
                (
                    "/st",
                    parse_path_segment::<u8>(),
                    "/torque",
                    parse_path_segment::<u8>(),
                ),
                st_torque_handler(),
            )
            .route(("/st", parse_path_segment::<u8>(), "/zero"), st_zero_handler())
            .route(
                (
                    "/st",
                    parse_path_segment::<u8>(),
                    "/wheel",
                    parse_path_segment::<i16>(),
                ),
                routing::get_service(StWheelService),
            )
            .route(
                ("/st", parse_path_segment::<u8>(), "/mode", "/servo"),
                st_servo_mode_handler(),
            )
            .route(
                ("/st", parse_path_segment::<u8>(), "/ping"),
                st_ping_handler(),
            )
            .route(
                (
                    "/st",
                    parse_path_segment::<u8>(),
                    "/id",
                    parse_path_segment::<u8>(),
                ),
                st_set_id_handler(),
            )
    }
}

pub struct WebApp {
    pub router: &'static AppRouter<Application>,
    pub config: &'static picoserve::Config,
}

impl Default for WebApp {
    fn default() -> Self {
        let router = picoserve::make_static!(AppRouter<Application>, Application.build_app());
        let config = picoserve::make_static!(
            picoserve::Config,
            picoserve::Config::new(picoserve::Timeouts {
                start_read_request: Duration::from_secs(5),
                persistent_start_read_request: Duration::from_secs(1),
                read_request: Duration::from_secs(2),
                write: Duration::from_secs(2),
            })
            .keep_connection_alive()
        );

        Self { router, config }
    }
}

struct JsonBody(String);

impl Content for JsonBody {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    fn content_length(&self) -> usize {
        self.0.len()
    }

    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0.as_bytes()).await
    }
}

fn json_response(status: StatusCode, body: String) -> JsonResponse {
    (status, ("Access-Control-Allow-Origin", "*"), JsonBody(body))
}

fn json_ok(body: impl Into<String>) -> JsonResponse {
    json_response(StatusCode::OK, body.into())
}

fn json_error(status: StatusCode, message: &str) -> JsonResponse {
    json_response(
        status,
        alloc::format!(r#"{{"ok":false,"error":"{}"}}"#, message),
    )
}

fn query_value<'a>(query: Query<'a>, name: &str) -> Option<&'a str> {
    query?.0.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn query_u8(query: Query<'_>, name: &str) -> Option<u8> {
    query_value(query, name)?.parse().ok()
}

fn query_u16(query: Query<'_>, name: &str) -> Option<u16> {
    query_value(query, name)?.parse().ok()
}

const ST_MAX_WHEEL_SPEED: i16 = 4095;

fn is_valid_motor_id(c: char) -> bool {
    match c {
        'a' | 'b' => true,
        #[cfg(feature = "four_motor")]
        'c' | 'd' => true,
        _ => false,
    }
}

fn motor_id_from_char(motor_id: char) -> Option<MotorId> {
    match motor_id {
        'a' => Some(MotorId::A),
        'b' => Some(MotorId::B),
        #[cfg(feature = "four_motor")]
        'c' => Some(MotorId::C),
        #[cfg(feature = "four_motor")]
        'd' => Some(MotorId::D),
        _ => None,
    }
}

fn parse_motors_batch(query: Query<'_>) -> Option<[Option<i8>; MOTOR_COUNT]> {
    let mut result = [None::<i8>; MOTOR_COUNT];
    let mut found_any = false;

    for part in query?.0.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let Ok(power) = value.parse::<i8>() else {
            continue;
        };
        if !(-100..=100).contains(&power) {
            continue;
        }

        match key {
            "a" => {
                result[0] = Some(power);
                found_any = true;
            }
            "b" => {
                result[1] = Some(power);
                found_any = true;
            }
            #[cfg(feature = "four_motor")]
            "c" => {
                result[2] = Some(power);
                found_any = true;
            }
            #[cfg(feature = "four_motor")]
            "d" => {
                result[3] = Some(power);
                found_any = true;
            }
            _ => {}
        }
    }

    found_any.then_some(result)
}

fn validate_st_id(id: u8) -> bool {
    (1..=253).contains(&id)
}

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

async fn health_response() -> impl IntoResponse {
    println!("HTTP GET /health");
    json_ok(r#"{"healthy": true}"#)
}

async fn battery_response() -> impl IntoResponse {
    println!("HTTP GET /battery");
    request_battery_sample(Duration::from_millis(100)).await;
    let mv = BATTERY_MV.load(core::sync::atomic::Ordering::Relaxed);
    let pct = BATTERY_PCT.load(core::sync::atomic::Ordering::Relaxed);
    let volts = mv / 1000;
    let frac = (mv % 1000) / 10;
    json_ok(alloc::format!(
        r#"{{"voltage":"{}.{:02}","voltage_mv":{},"percentage":{}}}"#,
        volts,
        frac,
        mv,
        pct
    ))
}

async fn config_response() -> impl IntoResponse {
    println!("HTTP GET /config");
    #[cfg(feature = "four_motor")]
    let body = r#"{"motor_mode":"four_motor","motor_count":4,"motors":["a","b","c","d"]}"#;
    #[cfg(feature = "two_motor")]
    let body = r#"{"motor_mode":"two_motor","motor_count":2,"motors":["a","b"]}"#;
    json_ok(body)
}

async fn radio_ble_response() -> impl IntoResponse {
    println!("HTTP GET /radio/ble");
    RADIO_MODE_REQUEST.signal(RadioMode::Ble);
    json_ok(r#"{"ok":true,"mode":"ble","rebooting":true}"#)
}

fn servo_angle_handler() -> impl routing::MethodHandler<(), (u8,)> {
    routing::get(async |angle: u8| {
        println!("HTTP GET /servo/{}", angle);
        set_servo_angle(angle)
    })
}

fn set_servo_angle(angle: u8) -> JsonResponse {
    if angle <= 180 {
        let pos = (angle as u16 * 4095) / 180;
        send_command(Command::St3215Move {
            id: 1,
            pos,
            speed: 1000,
            acc: 50,
        });
        json_ok(alloc::format!(
            r#"{{"angle": {}, "id": 1, "pos": {}}}"#,
            angle,
            pos
        ))
    } else {
        json_error(StatusCode::BAD_REQUEST, "Angle must be between 0 and 180")
    }
}

struct ServoQueryService;

impl RequestHandlerService for ServoQueryService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _path_parameters: (),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /servo");
        let response = match query_u8(request.parts.query(), "angle") {
            Some(angle) => set_servo_angle(angle),
            None => json_error(
                StatusCode::BAD_REQUEST,
                "Missing or invalid angle parameter. Use /servo/90 or /servo?angle=90",
            ),
        };
        response
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

fn motor_power_handler() -> impl routing::MethodHandler<(), (char, i8)> {
    routing::get(async |(motor_id, power): (char, i8)| {
        println!("HTTP GET /motor/{}/{}", motor_id, power);
        set_motor_power(motor_id, power)
    })
}

fn set_motor_power(motor_id: char, power: i8) -> JsonResponse {
    if !is_valid_motor_id(motor_id) {
        return json_error(StatusCode::NOT_FOUND, "Unknown motor id");
    }
    if !(-100..=100).contains(&power) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Power must be between -100 and 100",
        );
    }

    let Some(id) = motor_id_from_char(motor_id) else {
        return json_error(StatusCode::NOT_FOUND, "Unknown motor id");
    };

    send_command(Command::Motor(id, power));
    json_ok(alloc::format!(
        r#"{{"motor": "{}", "power": {}}}"#,
        motor_id,
        power
    ))
}

struct MotorQueryService;

impl RequestHandlerService<(), (char,)> for MotorQueryService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        (motor_id,): (char,),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /motor/{}", motor_id);
        let response = match query_value(request.parts.query(), "power")
            .and_then(|value| value.parse::<i8>().ok())
        {
            Some(power) => set_motor_power(motor_id, power),
            None => json_error(
                StatusCode::BAD_REQUEST,
                "Missing or invalid power parameter. Use /motor/a/50 or /motor/a?power=50",
            ),
        };
        response
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

struct MotorsService;

impl RequestHandlerService for MotorsService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _path_parameters: (),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /motors");
        let response = if let Some(parsed) = parse_motors_batch(request.parts.query()) {
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
            json_ok(body)
        } else {
            json_error(
                StatusCode::BAD_REQUEST,
                "Provide at least one motor param: /motors?a=50&b=-30",
            )
        };
        response
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

async fn st_list_response() -> impl IntoResponse {
    println!("HTTP GET /st/list");
    let Some(list) = SHARED_LIST.try_get() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            String::from(r#"{"error":"ST3215 list not initialized"}"#),
        );
    };
    let list_guard = list.lock().await;
    json_ok(st_list_body(list_guard.iter().copied()))
}

async fn st_scan_response(query: Query<'_>) -> impl IntoResponse {
    let from = query_u8(query, "from").unwrap_or(1);
    let to = query_u8(query, "to").unwrap_or(20);
    if from == 0 || to == 0 || from > to || to > 253 {
        return json_response(
            StatusCode::BAD_REQUEST,
            String::from(r#"{"error":"Scan range must satisfy 1 <= from <= to <= 253"}"#),
        );
    }

    let Some(bus) = SHARED_BUS.try_get() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            String::from(r#"{"error":"ST3215 bus not initialized"}"#),
        );
    };
    let Some(list) = SHARED_LIST.try_get() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            String::from(r#"{"error":"ST3215 list not initialized"}"#),
        );
    };

    let mut bus_guard = bus.lock().await;
    let mut list_guard = list.lock().await;
    bus_guard.scan(from, to, &mut list_guard).await;
    json_ok(st_list_body(list_guard.iter().copied()))
}

struct StScanService;

impl RequestHandlerService for StScanService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _path_parameters: (),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /st/scan");
        st_scan_response(request.parts.query())
            .await
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

struct StMoveService;

impl RequestHandlerService<(), (u8, u16)> for StMoveService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        (id, pos): (u8, u16),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /st/{}/pos/{}", id, pos);
        st_move_response(id, pos, request.parts.query())
            .await
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

async fn st_move_response(id: u8, pos: u16, query: Query<'_>) -> impl IntoResponse {
    if !validate_st_id(id) || pos > 4095 {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Servo ID must be 1-253 and position 0-4095",
        );
    }
    let speed = query_u16(query, "speed").unwrap_or(2000).min(4095);
    let acc = query_u8(query, "acc").unwrap_or(50);
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.write_pos(id, pos, speed, acc).await {
        Ok(()) => json_ok(alloc::format!(
            r#"{{"ok":true,"id":{},"pos":{},"speed":{},"acc":{}}}"#,
            id,
            pos,
            speed,
            acc
        )),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
        ),
    }
}

fn st_torque_handler() -> impl routing::MethodHandler<(), (u8, u8)> {
    routing::get(async |(id, enable): (u8, u8)| {
        println!("HTTP GET /st/{}/torque/{}", id, enable);
        match enable {
            0 => st_torque_response(id, false).await,
            1 => st_torque_response(id, true).await,
            _ => json_error(StatusCode::BAD_REQUEST, "Torque value must be 0 or 1"),
        }
    })
}

async fn st_torque_response(id: u8, enable: bool) -> JsonResponse {
    if !validate_st_id(id) {
        return json_error(StatusCode::BAD_REQUEST, "Servo ID must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.set_torque(id, enable).await {
        Ok(()) => json_ok(alloc::format!(
            r#"{{"ok":true,"id":{},"torque":{}}}"#,
            id,
            enable
        )),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
        ),
    }
}

fn st_zero_handler() -> impl routing::MethodHandler<(), (u8,)> {
    routing::get(async |id: u8| {
        println!("HTTP GET /st/{}/zero", id);
        st_zero_response(id).await
    })
}

async fn st_zero_response(id: u8) -> JsonResponse {
    if !validate_st_id(id) {
        return json_error(StatusCode::BAD_REQUEST, "Servo ID must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.calibrate_zero(id).await {
        Ok(()) => json_ok(alloc::format!(r#"{{"ok":true,"id":{},"zero":true}}"#, id)),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
        ),
    }
}

struct StWheelService;

impl RequestHandlerService<(), (u8, i16)> for StWheelService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        (id, speed): (u8, i16),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /st/{}/wheel/{}", id, speed);
        st_wheel_response(id, speed, request.parts.query())
            .await
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

async fn st_wheel_response(id: u8, speed: i16, query: Query<'_>) -> JsonResponse {
    if !validate_st_id(id) || !(-ST_MAX_WHEEL_SPEED..=ST_MAX_WHEEL_SPEED).contains(&speed) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Servo ID must be 1-253 and wheel speed -4095..=4095",
        );
    }
    let acc = query_u8(query, "acc").unwrap_or(50);
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.write_wheel_speed(id, speed, acc).await {
        Ok(()) => json_ok(alloc::format!(
            r#"{{"ok":true,"id":{},"wheel_speed":{},"acc":{}}}"#,
            id,
            speed,
            acc
        )),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
        ),
    }
}

fn st_servo_mode_handler() -> impl routing::MethodHandler<(), (u8,)> {
    routing::get(async |id: u8| {
        println!("HTTP GET /st/{}/mode/servo", id);
        st_servo_mode_response(id).await
    })
}

async fn st_servo_mode_response(id: u8) -> JsonResponse {
    if !validate_st_id(id) {
        return json_error(StatusCode::BAD_REQUEST, "Servo ID must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.set_servo_mode(id).await {
        Ok(()) => json_ok(alloc::format!(r#"{{"ok":true,"id":{},"mode":"servo"}}"#, id)),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
        ),
    }
}

fn st_ping_handler() -> impl routing::MethodHandler<(), (u8,)> {
    routing::get(async |id: u8| {
        println!("HTTP GET /st/{}/ping", id);
        st_ping_response(id).await
    })
}

async fn st_ping_response(id: u8) -> impl IntoResponse {
    if !validate_st_id(id) {
        return json_error(StatusCode::BAD_REQUEST, "Servo ID must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    let body = match bus_guard.ping(id).await {
        Ok(()) => alloc::format!(r#"{{"ok":true,"id":{}}}"#, id),
        Err(e) => alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, id, e),
    };
    json_ok(body)
}

fn st_set_id_handler() -> impl routing::MethodHandler<(), (u8, u8)> {
    routing::get(async |(current, new): (u8, u8)| {
        println!("HTTP GET /st/{}/id/{}", current, new);
        st_set_id_response(current, new).await
    })
}

async fn st_set_id_response(current: u8, new: u8) -> impl IntoResponse {
    if !validate_st_id(current) || !validate_st_id(new) {
        return json_error(StatusCode::BAD_REQUEST, "Servo IDs must be 1-253");
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };
    let Some(list) = SHARED_LIST.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 list not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.write_id(current, new).await {
        Ok(()) => {
            let mut list_guard = list.lock().await;
            bus_guard.scan(1, 20, &mut list_guard).await;
            json_ok(alloc::format!(
                r#"{{"ok":true,"current":{},"new":{}}}"#,
                current,
                new
            ))
        }
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"id":{},"error":"{:?}"}}"#, current, e),
        ),
    }
}

struct StAllService;

impl RequestHandlerService for StAllService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _path_parameters: (),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        println!("HTTP GET /st/all");
        st_all_response(request.parts.query())
            .await
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

async fn st_all_response(query: Query<'_>) -> impl IntoResponse {
    let speed = query_u16(query, "speed").unwrap_or(2000).min(4095);
    let acc = query_u8(query, "acc").unwrap_or(50);
    let mut moves: HVec<(u8, u16, u16, u8), MAX_SERVOS> = HVec::new();

    if let Some(query) = query {
        for part in query.0.split('&') {
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
            StatusCode::BAD_REQUEST,
            "Provide at least one servo position, e.g. /st/all?1=2048",
        );
    }
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };

    let mut bus_guard = bus.lock().await;
    match bus_guard.sync_write_pos(&moves).await {
        Ok(()) => json_ok(alloc::format!(r#"{{"ok":true,"count":{}}}"#, moves.len())),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            alloc::format!(r#"{{"ok":false,"error":"{:?}"}}"#, e),
        ),
    }
}

async fn st_state_response() -> impl IntoResponse {
    println!("HTTP GET /st/state");
    let Some(bus) = SHARED_BUS.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 bus not initialized",
        );
    };
    let Some(list) = SHARED_LIST.try_get() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ST3215 list not initialized",
        );
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
    json_ok(body)
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
pub async fn web_task(
    task_id: usize,
    stack: Stack<'static>,
    router: &'static AppRouter<Application>,
    config: &'static picoserve::Config,
) -> ! {
    let mut tcp_rx_buffer = [0; TCP_RX_BUFFER_SIZE];
    let mut tcp_tx_buffer = [0; TCP_TX_BUFFER_SIZE];
    let mut http_buffer = [0; HTTP_BUFFER_SIZE];

    println!(
        "picoserve web task {} started ([Heap] {} bytes free)",
        task_id,
        esp_alloc::HEAP.free()
    );

    picoserve::Server::new(router, config, &mut http_buffer)
        .listen_and_serve(task_id, stack, 80, &mut tcp_rx_buffer, &mut tcp_tx_buffer)
        .await
        .into_never()
}
