//! 浏览器管理台服务(`rtt-web`):SerialHub 同构——Rust 数据层 + 内嵌 Web UI,
//! 浏览器访问 `http://127.0.0.1:8080`,Playwright/pytest 可黑盒测试。
//!
//! 与桌面版**同一套数据层与 worker**(rtt::spawn / LogPump / demo / device_db),
//! 界面布局与桌面版一致(左配置面板 + 右日志区)。主 exe 不链接 axum/tokio,
//! 体积零影响。
//!
//! API 契约(与前端/测试对齐,改这里必同步 ui/web/index.html 与 tests/web):
//! - GET  /                管理台单页
//! - GET  /api/status      → {connected, phase, port, rxBytes, txBytes, rowsTotal, uptimeSec, device}
//! - GET  /api/themes      → [{id, name}]
//! - GET  /api/devices     → [芯片型号]
//! - GET  /api/jlinks      → [{sn, name}]
//! - POST /api/connect     {chip, ifaceIndex, speedIndex, channel}
//! - POST /api/disconnect
//! - POST /api/power       {on}
//! - POST /api/reset
//! - POST /api/settings    {rxEnding?, frameTimeout?, encodingIndex?, hexRx?}(逐字段可选)
//! - POST /api/send        {text}  → 回显一行,计数 TX
//! - POST /api/mark        {text}  → 插入会话标记行
//! - POST /api/pause       {on}
//! - POST /api/clear
//! - WS   /ws              {type:"rows"/"snapshot"/"cleared"} 与
//!   {type:"state"/"device"/"progress"/"names"/"jlinks"/"stats"}

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use mini_rtt_viewer::log_model::LogPump;
use mini_rtt_viewer::rtt::SPEEDS_KHZ;
use mini_rtt_viewer::{
    ansi, demo, device_db,
    rtt::{self, WorkerCmd, WorkerConfig, WorkerHandle, WorkerMsg},
};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Instant,
};
use tokio::sync::broadcast;

/// 管理台单页(build 时内嵌,零静态文件分发)
const INDEX_HTML: &str = include_str!("../../ui/web/index.html");
/// 会话标记行颜色(与桌面版 MARK_COLOR 一致)
const MARK_COLOR: (u8, u8, u8) = (0x28, 0xaf, 0xe9);

/// 服务共享状态
struct Shared {
    pump: Mutex<LogPump>,
    rx_ending: Mutex<i32>,
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
    started: Instant,
    /// 事件流:rows/snapshot/cleared/state/device/progress/names/jlinks/stats
    events_tx: broadcast::Sender<String>,
    connected: AtomicBool,
    seq: AtomicU64,
    clear_seq: AtomicU64,
    // ---- 真机连接(与桌面版同一 worker 协议)----
    worker: Mutex<Option<Arc<WorkerHandle>>>,
    cmd_tx: Mutex<Option<mpsc::Sender<WorkerCmd>>>,
    msg_tx: mpsc::Sender<WorkerMsg>,
    frame_timeout_ms: Arc<AtomicU32>,
    encoding_index: Arc<AtomicU32>,
    hex_rx: Arc<AtomicBool>,
    device_names: Mutex<Vec<String>>,
    jlinks: Mutex<Vec<(u32, String)>>,
    device_info: Mutex<Option<rtt::DeviceInfo>>,
    demo_mode: bool,
}

#[derive(Deserialize)]
struct SendReq {
    text: String,
    /// HEX 发送模式:输入按十六进制字节解析(与桌面版 parse_hex_bytes 同规则)
    #[serde(default)]
    hex: bool,
}

/// hex 文本 → 字节:容忍空格/冒号/连字符与 0x 前缀;空/奇数位/非法字符报错
fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s
        .trim()
        .trim_start_matches("0x")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '-')
        .collect();
    if cleaned.is_empty() {
        return Err("空输入".into());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err("十六进制位数为奇数".into());
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for pair in cleaned.as_bytes().as_chunks::<2>().0 {
        let hi = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| format!("非法字符 '{}'", pair[0] as char))?;
        let lo = (pair[1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("非法字符 '{}'", pair[1] as char))?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

#[derive(Deserialize)]
struct PauseReq {
    on: bool,
}

#[derive(Deserialize)]
struct ConnectReq {
    chip: String,
    #[serde(default)]
    iface_index: usize,
    #[serde(default)]
    speed_index: usize,
    #[serde(default)]
    channel: u32,
}

#[derive(Deserialize)]
struct PowerReq {
    on: bool,
}

#[derive(Deserialize)]
struct SettingsReq {
    #[serde(default)]
    rx_ending: Option<i32>,
    #[serde(default)]
    frame_timeout: Option<u32>,
    #[serde(default)]
    encoding_index: Option<i32>,
    #[serde(default)]
    hex_rx: Option<bool>,
}

#[derive(Deserialize)]
struct MarkReq {
    #[serde(default)]
    text: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let demo_mode = args.iter().any(|a| a == "--demo-log");
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let (events_tx, _) = broadcast::channel(512);
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    let shared = Arc::new(Shared {
        pump: Mutex::new(LogPump::default()),
        rx_ending: Mutex::new(0),
        tx_bytes: AtomicU64::new(0),
        rx_bytes: AtomicU64::new(0),
        started: Instant::now(),
        events_tx,
        connected: AtomicBool::new(false),
        seq: AtomicU64::new(0),
        clear_seq: AtomicU64::new(0),
        worker: Mutex::new(None),
        cmd_tx: Mutex::new(None),
        msg_tx: msg_tx.clone(),
        frame_timeout_ms: Arc::new(AtomicU32::new(20)),
        encoding_index: Arc::new(AtomicU32::new(0)),
        hex_rx: Arc::new(AtomicBool::new(false)),
        device_names: Mutex::new(Vec::new()),
        jlinks: Mutex::new(Vec::new()),
        device_info: Mutex::new(None),
        demo_mode,
    });

    if demo_mode {
        demo::spawn(msg_tx);
        shared.connected.store(true, Ordering::Relaxed);
    } else {
        // 设备库后台枚举(芯片型号 + 本机 J-Link),与桌面版同一模块
        let (db_tx, db_rx) = mpsc::channel::<device_db::DbResult>();
        device_db::spawn_background(db_tx);
        let tx = msg_tx.clone();
        std::thread::spawn(move || {
            while let Ok(r) = db_rx.recv() {
                let msg = match r {
                    device_db::DbResult::DeviceNames(names) => WorkerMsg::DeviceNames(names),
                    device_db::DbResult::Emulators(list) => WorkerMsg::JLinks(list),
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }

    // 数据泵线程:消化 worker 消息 → pump → 事件广播(10ms,与桌面版 tick 同构)
    {
        let shared = shared.clone();
        std::thread::spawn(move || tick_loop(shared, msg_rx));
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/favicon.png", get(favicon))
        .route("/api/status", get(api_status))
        .route("/api/themes", get(api_themes))
        .route("/api/devices", get(api_devices))
        .route("/api/jlinks", get(api_jlinks))
        .route("/api/connect", post(api_connect))
        .route("/api/disconnect", post(api_disconnect))
        .route("/api/power", post(api_power))
        .route("/api/reset", post(api_reset))
        .route("/api/settings", post(api_settings))
        .route("/api/send", post(api_send))
        .route("/api/mark", post(api_mark))
        .route("/api/pause", post(api_pause))
        .route("/api/clear", post(api_clear))
        .route("/ws", get(ws_handler))
        .with_state(shared);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("[rtt-web] 管理台 http://{addr}  (demo={demo_mode})");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 数据泵:消息消化 → cap → 增量上屏 → 事件广播;状态/设备信息同步进 Shared
fn tick_loop(shared: Arc<Shared>, msg_rx: mpsc::Receiver<WorkerMsg>) {
    let mut last_stats = Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let rx_ending = *shared.rx_ending.lock().unwrap();
        let mut pump = shared.pump.lock().unwrap();
        loop {
            match msg_rx.try_recv() {
                Ok(WorkerMsg::Block(text)) => {
                    if !pump.paused {
                        shared
                            .rx_bytes
                            .fetch_add(text.len() as u64, Ordering::Relaxed);
                        pump.absorb_text(&text, rx_ending);
                    }
                }
                Ok(WorkerMsg::Log(text)) => {
                    if !pump.paused {
                        pump.absorb_text(&text, rx_ending);
                    }
                }
                Ok(WorkerMsg::FrameEnd) => {
                    if !pump.paused {
                        pump.absorb_frame_end(rx_ending);
                    }
                }
                Ok(WorkerMsg::Progress(text)) => {
                    let _ = shared.events_tx.send(format!(
                        r#"{{"type":"progress","text":{}}}"#,
                        serde_json::to_string(&text).unwrap()
                    ));
                }
                Ok(WorkerMsg::State(connected, status)) => {
                    shared.connected.store(connected, Ordering::Relaxed);
                    let _ = shared.events_tx.send(format!(
                        r#"{{"type":"state","connected":{},"status":{}}}"#,
                        connected,
                        serde_json::to_string(&status).unwrap()
                    ));
                }
                Ok(WorkerMsg::DeviceInfo(info)) => {
                    let json = serde_json::json!({
                        "firmware": info.firmware, "hardware": info.hardware,
                        "serial": info.serial, "core": info.core_name,
                        "cpu": info.core_cpu, "target": info.target,
                        "iface": info.iface, "speedKhz": info.speed_khz,
                    });
                    *shared.device_info.lock().unwrap() = Some(info);
                    let _ = shared
                        .events_tx
                        .send(format!(r#"{{"type":"device","info":{json}}}"#));
                }
                Ok(WorkerMsg::DeviceNames(names)) => {
                    *shared.device_names.lock().unwrap() = names.clone();
                    let _ = shared.events_tx.send(format!(
                        r#"{{"type":"names","names":{}}}"#,
                        serde_json::to_string(&names).unwrap()
                    ));
                }
                Ok(WorkerMsg::JLinks(list)) => {
                    *shared.jlinks.lock().unwrap() = list.clone();
                    let arr: Vec<serde_json::Value> = list
                        .iter()
                        .map(|(sn, name)| serde_json::json!({"sn": sn, "name": name}))
                        .collect();
                    let _ = shared.events_tx.send(format!(
                        r#"{{"type":"jlinks","list":{}}}"#,
                        serde_json::Value::Array(arr)
                    ));
                }
                Ok(WorkerMsg::Exited) => {
                    *shared.worker.lock().unwrap() = None;
                    *shared.cmd_tx.lock().unwrap() = None;
                    shared.connected.store(false, Ordering::Relaxed);
                }
                Err(_) => break,
            }
        }
        pump.enforce_line_cap();
        let dropped = pump.take_dropped();
        if let Some(rows) = pump.take_new_rows() {
            let seq = shared.seq.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = shared.events_tx.send(rows_payload(seq, &rows, dropped));
        }
        drop(pump);
        if last_stats.elapsed() >= std::time::Duration::from_millis(500) {
            last_stats = Instant::now();
            let _ = shared.events_tx.send(format!(
                r#"{{"type":"stats","rx":{},"tx":{},"rows":{}}}"#,
                shared.rx_bytes.load(Ordering::Relaxed),
                shared.tx_bytes.load(Ordering::Relaxed),
                shared.pump.lock().unwrap().rows_len()
            ));
        }
    }
}

/// 行数组 → JSON(runs: [{text, fg}] 逐行;fg 为 null 表示主题默认色)
fn rows_json_array(rows: &[Vec<ansi::Run>]) -> String {
    let mut out = String::from("[");
    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 {
            out.push(',');
        }
        out.push_str(r#"{"runs":["#);
        for (si, run) in row.iter().enumerate() {
            if si > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                r#"{{"text":{},"fg":{}}}"#,
                serde_json::to_string(&run.text).unwrap(),
                match run.fg {
                    Some((r, g, b)) => format!("\"#{:02x}{:02x}{:02x}\"", r, g, b),
                    None => "null".to_string(),
                }
            ));
        }
        out.push_str("]}");
    }
    out.push(']');
    out
}

/// 增量行消息(dropped>0 时前端同步丢弃头部同量行)
fn rows_payload(seq: u64, rows: &[Vec<ansi::Run>], dropped: usize) -> String {
    format!(
        r#"{{"type":"rows","seq":{seq},"dropped":{dropped},"rows":{}}}"#,
        rows_json_array(rows)
    )
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn favicon() -> Response {
    match std::fs::read("assets/app-32.png").or_else(|_| std::fs::read("assets/app.png")) {
        Ok(bytes) => (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn api_status(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let device = shared.device_info.lock().unwrap().as_ref().map(|d| {
        serde_json::json!({
            "firmware": d.firmware, "target": d.target, "iface": d.iface,
            "speedKhz": d.speed_khz, "serial": d.serial,
        })
    });
    Json(serde_json::json!({
        "connected": shared.connected.load(Ordering::Relaxed),
        "phase": if shared.connected.load(Ordering::Relaxed) { "connected" } else { "idle" },
        "port": if shared.connected.load(Ordering::Relaxed) { "jlink" } else { "demo" },
        "rxBytes": shared.rx_bytes.load(Ordering::Relaxed),
        "txBytes": shared.tx_bytes.load(Ordering::Relaxed),
        "rowsTotal": shared.pump.lock().unwrap().rows_len(),
        "uptimeSec": shared.started.elapsed().as_secs(),
        "device": device,
    }))
}

async fn api_themes() -> Json<serde_json::Value> {
    Json(serde_json::json!([
        {"id": "dark", "name": "深色"},
        {"id": "light", "name": "浅色"},
        {"id": "oled", "name": "OLED 纯黑"},
        {"id": "sepia", "name": "护眼暖色"},
    ]))
}

async fn api_devices(State(shared): State<Arc<Shared>>) -> Json<Vec<String>> {
    // demo 模式返回演示设备库:无 J-Link 环境也能体验/测试补全交互
    if shared.demo_mode {
        return Json(vec![
            "STM32F103C8".into(),
            "STM32F030C8".into(),
            "STM32G474VET6".into(),
            "STM32H743ZIT6".into(),
            "GD32F303CCT6".into(),
            "CH32V307VCT6".into(),
        ]);
    }
    Json(shared.device_names.lock().unwrap().clone())
}

async fn api_jlinks(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let list = shared.jlinks.lock().unwrap().clone();
    Json(serde_json::json!(list
        .iter()
        .map(|(sn, name)| serde_json::json!({"sn": sn, "name": name}))
        .collect::<Vec<_>>()))
}

/// 连接:芯片名补全(库内子串匹配首个全称,与桌面版同规则)→ spawn worker
async fn api_connect(State(shared): State<Arc<Shared>>, Json(req): Json<ConnectReq>) -> Response {
    if shared
        .worker
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|h| h.alive.load(Ordering::Relaxed))
    {
        return (StatusCode::CONFLICT, "已有连接").into_response();
    }
    let chip_raw = req.chip.trim().to_string();
    if chip_raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "请先填写目标芯片型号").into_response();
    }
    let full = shared.device_names.lock().unwrap();
    let exact = full.iter().any(|n| n.eq_ignore_ascii_case(&chip_raw));
    let chip = if exact {
        chip_raw
    } else {
        let needle = chip_raw.to_uppercase();
        match full.iter().find(|s| s.to_uppercase().contains(&needle)) {
            Some(c) => c.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("设备库无匹配型号:{chip_raw}"),
                )
                    .into_response()
            }
        }
    };
    drop(full);

    let selected_sn = shared.jlinks.lock().unwrap().first().map(|(sn, _)| *sn);
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
    let handle = rtt::spawn(
        WorkerConfig {
            chip,
            iface_index: req.iface_index.min(1),
            speed_khz: SPEEDS_KHZ[req.speed_index.min(SPEEDS_KHZ.len() - 1)],
            channel: req.channel.min(15),
            frame_timeout_ms: shared.frame_timeout_ms.clone(),
            selected_sn,
            encoding_index: shared.encoding_index.clone(),
            hex_rx: shared.hex_rx.clone(),
        },
        shared.msg_tx.clone(),
        cmd_rx,
    );
    *shared.worker.lock().unwrap() = Some(handle);
    *shared.cmd_tx.lock().unwrap() = Some(cmd_tx);
    StatusCode::OK.into_response()
}

/// 断开:置停止标志,worker 的 Exited 消息回到未连接态(与桌面版同协议)
async fn api_disconnect(State(shared): State<Arc<Shared>>) -> StatusCode {
    if let Some(h) = shared.worker.lock().unwrap().as_ref() {
        h.stop.store(true, Ordering::Relaxed);
    }
    if let Some(tx) = shared.cmd_tx.lock().unwrap().as_ref() {
        let _ = tx.send(WorkerCmd::Send(Vec::new())); // 唤醒阻塞中的 worker 尽快退出
    }
    StatusCode::OK
}

async fn api_power(State(shared): State<Arc<Shared>>, Json(req): Json<PowerReq>) -> Response {
    let Some(tx) = shared.cmd_tx.lock().unwrap().clone() else {
        return (StatusCode::CONFLICT, "未连接").into_response();
    };
    let _ = tx.send(WorkerCmd::Power(req.on));
    StatusCode::OK.into_response()
}

async fn api_reset(State(shared): State<Arc<Shared>>) -> Response {
    let Some(tx) = shared.cmd_tx.lock().unwrap().clone() else {
        return (StatusCode::CONFLICT, "未连接").into_response();
    };
    let _ = tx.send(WorkerCmd::Reset);
    StatusCode::OK.into_response()
}

/// 运行时参数:逐字段可选,worker 共享原子热切换(与桌面版同语义)
async fn api_settings(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<SettingsReq>,
) -> StatusCode {
    if let Some(v) = req.rx_ending {
        *shared.rx_ending.lock().unwrap() = v.clamp(0, 4);
    }
    if let Some(v) = req.frame_timeout {
        shared
            .frame_timeout_ms
            .store(v.clamp(1, 200), Ordering::Relaxed);
    }
    if let Some(v) = req.encoding_index {
        shared
            .encoding_index
            .store(v.clamp(0, 4) as u32, Ordering::Relaxed);
    }
    if let Some(v) = req.hex_rx {
        shared.hex_rx.store(v, Ordering::Relaxed);
    }
    StatusCode::OK
}

async fn api_send(State(shared): State<Arc<Shared>>, Json(req): Json<SendReq>) -> Response {
    if req.text.is_empty() {
        return (StatusCode::BAD_REQUEST, "空输入").into_response();
    }
    let payload = if req.hex {
        match parse_hex_bytes(&req.text) {
            Ok(b) => b,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("HEX 格式错误:{e}")).into_response()
            }
        }
    } else {
        req.text.clone().into_bytes()
    };
    // demo 模式无 cmd_tx:仍回显与计数(前端连接态才允许发送,语义为虚拟发送)
    if let Some(tx) = shared.cmd_tx.lock().unwrap().clone() {
        let _ = tx.send(WorkerCmd::Send(payload));
    }
    shared
        .tx_bytes
        .fetch_add(req.text.len() as u64, Ordering::Relaxed);
    let mut pump = shared.pump.lock().unwrap();
    pump.push_colored_line(&format!("» {}", req.text), (0x8f, 0x8f, 0x9a));
    StatusCode::OK.into_response()
}

/// 本地时间戳(HH:MM:SS):零依赖 Win32 GetLocalTime,与桌面版同款
fn hms_stamp() -> String {
    #[repr(C)]
    struct WinSystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(out: *mut WinSystemTime);
    }
    let mut t = WinSystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    unsafe { GetLocalTime(&mut t) };
    format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
}

async fn api_mark(State(shared): State<Arc<Shared>>, Json(req): Json<MarkReq>) -> StatusCode {
    let label = if req.text.is_empty() {
        "标记".to_string()
    } else {
        req.text
    };
    let mut pump = shared.pump.lock().unwrap();
    pump.push_colored_line(&format!("── [{}] {label} ──", hms_stamp()), MARK_COLOR);
    StatusCode::OK
}

async fn api_pause(State(shared): State<Arc<Shared>>, Json(req): Json<PauseReq>) -> StatusCode {
    shared.pump.lock().unwrap().paused = req.on;
    StatusCode::OK
}

async fn api_clear(State(shared): State<Arc<Shared>>) -> StatusCode {
    shared.pump.lock().unwrap().clear();
    // 水位之后的 rows 才有效;清空前的滞留行由前端按 seq 丢弃
    let seq = shared.seq.fetch_add(1, Ordering::Relaxed) + 1;
    shared.clear_seq.store(seq, Ordering::Relaxed);
    let _ = shared
        .events_tx
        .send(format!(r#"{{"type":"cleared","seq":{seq}}}"#));
    StatusCode::OK
}

async fn ws_handler(ws: WebSocketUpgrade, State(shared): State<Arc<Shared>>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, shared))
}

/// WS 会话:初次全量快照 + 订阅事件流;Lagged 自动重同步
async fn ws_loop(mut socket: WebSocket, shared: Arc<Shared>) {
    let mut rx = shared.events_tx.subscribe();
    {
        let snapshot = shared.pump.lock().unwrap().snapshot_rows();
        let msg = format!(
            r#"{{"type":"snapshot","rows":{}}}"#,
            rows_json_array(&snapshot)
        );
        if socket.send(Message::Text(msg.into())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            res = rx.recv() => {
                match res {
                    Ok(payload) => {
                        if socket.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // 订阅滞后:丢弃的行不可恢复,重发全量快照让前端重同步
                        let snapshot = shared.pump.lock().unwrap().snapshot_rows();
                        let msg = format!(
                            r#"{{"type":"snapshot","rows":{}}}"#,
                            rows_json_array(&snapshot)
                        );
                        if socket.send(Message::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                if socket.send(Message::Ping("".into())).await.is_err() {
                    break;
                }
            }
            else => break,
        }
    }
}
