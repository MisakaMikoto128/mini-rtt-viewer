//! 浏览器管理台服务(`rtt-web`):SerialHub 同构——Rust 数据层 + 内嵌 Web UI,
//! 浏览器访问 `http://127.0.0.1:8080`,Playwright/pytest 可黑盒测试。
//!
//! 与桌面版的关系:共享 lib 数据层(LogPump/ansi/demo/config),**不加载
//! JLinkARM.dll**(真机接入是下一阶段;当前 --demo-log 驱动数据流,UI/主题/
//! 发送/暂停全部可测)。主 exe 不链接 axum/tokio,体积零影响。
//!
//! API 契约(与前端/测试对齐,改这里必同步 ui/web/index.html 与 tests/web):
//! - GET  /            管理台单页
//! - GET  /api/status  → {connected, phase, rxBytes, txBytes, rowsTotal, uptimeSec}
//! - GET  /api/themes  → [{id, name}]
//! - POST /api/send    {text, hex?}  → 回显一行,计数 TX
//! - POST /api/pause   {on}          → 暂停/继续接收
//! - POST /api/clear   → 清空日志
//! - WS   /ws          推 {type:"rows", rows:[{runs:[{text,fg}]}], dropped}
//!                      与 {type:"stats", ...}(500ms 节流)

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
use mini_rtt_viewer::{ansi, demo};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Instant,
};
use tokio::sync::broadcast;

/// 管理台单页(build 时内嵌,零静态文件分发)
const INDEX_HTML: &str = include_str!("../../ui/web/index.html");

/// 服务共享状态
struct Shared {
    pump: Mutex<LogPump>,
    rx_ending: Mutex<i32>,
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
    started: Instant,
    /// 日志行广播(WS 订阅;容量兜底,无订阅者时发送即丢)
    rows_tx: broadcast::Sender<String>,
    connected: AtomicBool,
    /// 行序号:每次广播 rows 递增;clear 记录水位,早于水位的行作废
    seq: AtomicU64,
    clear_seq: AtomicU64,
}

#[derive(Deserialize)]
struct SendReq {
    text: String,
    #[serde(default)]
    hex: bool,
}

#[derive(Deserialize)]
struct PauseReq {
    on: bool,
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

    let (rows_tx, _) = broadcast::channel(256);
    let shared = Arc::new(Shared {
        pump: Mutex::new(LogPump::default()),
        rx_ending: Mutex::new(0),
        tx_bytes: AtomicU64::new(0),
        rx_bytes: AtomicU64::new(0),
        started: Instant::now(),
        rows_tx,
        connected: AtomicBool::new(demo_mode), // demo 视为已连接
        seq: AtomicU64::new(0),
        clear_seq: AtomicU64::new(0),
    });

    // 数据流源:demo 线程复用桌面版同一数据生成器
    let (msg_tx, msg_rx) = mpsc::channel::<mini_rtt_viewer::rtt::WorkerMsg>();
    if demo_mode {
        demo::spawn(msg_tx);
    }

    // tick 线程:消化消息 → pump → 增量行 JSON → 广播(10ms,与桌面版一致)
    {
        let shared = shared.clone();
        std::thread::spawn(move || tick_loop(shared, msg_rx));
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/favicon.png", get(favicon))
        .route("/api/status", get(api_status))
        .route("/api/themes", get(api_themes))
        .route("/api/send", post(api_send))
        .route("/api/pause", post(api_pause))
        .route("/api/clear", post(api_clear))
        .route("/ws", get(ws_handler))
        .with_state(shared);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("[rtt-web] 管理台 http://{addr}  (demo={demo_mode})");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 数据泵:与桌面版 tick 同构(消息消化 → cap → 增量上屏),产出 WS 消息
fn tick_loop(shared: Arc<Shared>, msg_rx: mpsc::Receiver<mini_rtt_viewer::rtt::WorkerMsg>) {
    use mini_rtt_viewer::rtt::WorkerMsg;
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
                Ok(WorkerMsg::State(connected, _)) => {
                    shared.connected.store(connected, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        pump.enforce_line_cap();
        let dropped = pump.take_dropped();
        if let Some(rows) = pump.take_new_rows() {
            let seq = shared.seq.fetch_add(1, Ordering::Relaxed) + 1;
            let payload = rows_payload(seq, &rows, dropped);
            let _ = shared.rows_tx.send(payload);
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
    // 复用桌面版应用图标
    match std::fs::read("assets/app-32.png").or_else(|_| std::fs::read("assets/app.png")) {
        Ok(bytes) => (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn api_status(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let rows_total = shared.pump.lock().unwrap().rows_len();
    Json(serde_json::json!({
        "connected": shared.connected.load(Ordering::Relaxed),
        "phase": if shared.connected.load(Ordering::Relaxed) { "connected" } else { "idle" },
        "port": "demo",
        "rxBytes": shared.rx_bytes.load(Ordering::Relaxed),
        "txBytes": shared.tx_bytes.load(Ordering::Relaxed),
        "rowsTotal": rows_total,
        "uptimeSec": shared.started.elapsed().as_secs(),
    }))
}

async fn api_themes() -> Json<serde_json::Value> {
    // 主题表(前端按 id 取 CSS 变量集;与桌面版四主题对齐)
    Json(serde_json::json!([
        {"id": "dark", "name": "深色"},
        {"id": "light", "name": "浅色"},
        {"id": "oled", "name": "OLED 纯黑"},
        {"id": "sepia", "name": "护眼暖色"},
    ]))
}

async fn api_send(State(shared): State<Arc<Shared>>, Json(req): Json<SendReq>) -> StatusCode {
    if req.text.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let mut pump = shared.pump.lock().unwrap();
    pump.push_colored_line(&format!("» {}", req.text), (0x8f, 0x8f, 0x9a));
    shared
        .tx_bytes
        .fetch_add(req.text.len() as u64, Ordering::Relaxed);
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
        .rows_tx
        .send(format!(r#"{{"type":"cleared","seq":{seq}}}"#));
    StatusCode::OK
}

async fn ws_handler(ws: WebSocketUpgrade, State(shared): State<Arc<Shared>>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, shared))
}

/// WS 会话:订阅行广播 → 逐条转发;断开自动清理
async fn ws_loop(mut socket: WebSocket, shared: Arc<Shared>) {
    let mut rx = shared.rows_tx.subscribe();
    // 初次推送全量快照(页面刷新后重建视图)
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
                // 保活探测:对端无响应则结束会话
                if socket.send(Message::Ping("".into())).await.is_err() {
                    break;
                }
            }
            else => break,
        }
    }
}
