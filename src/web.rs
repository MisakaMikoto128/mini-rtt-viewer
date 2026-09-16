//! 浏览器管理台服务(ADR-11:UI 全面转 Web 形态):**一个 exe,双击启动本机
//! 服务并自动打开浏览器管理台**(SerialHub 同构——Rust 数据层 + 内嵌 Web UI),
//! 浏览器访问 `http://127.0.0.1:8686`。与桌面版同一套数据层与 worker
//! (rtt::spawn / LogPump / demo / device_db)。
//!
//! API 契约(与前端/测试对齐,改这里必同步 ui/web/index.html 与 tests/web):
//! - GET  /                管理台单页
//! - GET  /api/status      → {connected, phase, port, rxBytes, txBytes, rowsTotal, uptimeSec, device}
//! - GET  /api/prefs       → 持久化偏好快照(面板初值恢复)
//! - GET  /api/themes      → [{id, name}]
//! - GET  /api/devices     → [芯片型号]
//! - GET  /api/jlinks      → [{sn, name}]
//! - POST /api/connect     {chip, ifaceIndex, speedIndex, channel};连接参数写入偏好
//!   快照源;demo 模式只记录参数,不 spawn 真 worker
//! - POST /api/disconnect
//! - POST /api/power       {on}
//! - POST /api/reset
//! - POST /api/settings    逐字段可选(camelCase):rxEnding / frameTimeout /
//!   encodingIndex / hexRx / autoFrame / searchRegex
//! - POST /api/send        {text, hex?} → 回显一行,计数 TX,内容记为定时发送源
//! - POST /api/timer       {on, intervalSec?, text?, hex?};定时重发 0.001-999 秒,
//!   连接态才触发,复用 /api/send 发送管线,含回显
//! - POST /api/mark        {text}  → 插入会话标记行
//! - POST /api/pause       {on}
//! - POST /api/clear
//! - GET  /api/export      → text/plain 附件下载(rtt_<时间戳>.log,全部行文本)
//! - WS   /ws              {type:"rows"/"snapshot"/"cleared"} 与
//!   {type:"state"/"device"/"progress"/"names"/"jlinks"/"stats"}
//!
//! /api/prefs 返回:{chip, ifaceIndex, speedIndex, channel, rxEnding, frameTimeout,
//! encodingIndex, hexRx, autoFrame, searchRegex, timerOn, timerIntervalSec}
//!
//! 偏好持久化(与桌面版同模式):`%APPDATA%/MiniRttViewer/prefs.json`,数据泵
//! tick 内 500ms 快照比对——从 Shared 当前状态构建 StoredPrefs 子集,变化才原子
//! 写(未连接态也保存)。桌面专用字段(window 几何/dark_theme/log_font_px 等)
//! 沿用启动时读到的原值,serde(default) 兼容旧文件;主题/字号仍由前端
//! localStorage 管理,不经此通道。
//!
//! 单实例:端口即互斥——bind AddrInUse 时提示已有实例并以非零码退出。

use crate::ansi;
use crate::config::{self, StoredPrefs};
use crate::demo;
use crate::device_db;
use crate::log_model::LogPump;
use crate::rtt::{self, WorkerCmd, WorkerConfig, WorkerHandle, WorkerMsg, SPEEDS_KHZ};
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
use serde::Deserialize;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

/// 管理台单页(build 时内嵌,零静态文件分发)
const INDEX_HTML: &str = include_str!("../ui/web/index.html");
/// favicon(build 时内嵌,不依赖进程工作目录;运行时读 assets/ 在 cwd 不对时 404)
const FAVICON: &[u8] = include_bytes!("../assets/app-32.png");
/// 会话标记行颜色(与桌面版 MARK_COLOR 一致)
const MARK_COLOR: (u8, u8, u8) = (0x28, 0xaf, 0xe9);
/// 发送回显行颜色(中性灰,与设备数据一眼区分)
const ECHO_COLOR: (u8, u8, u8) = (0x8f, 0x8f, 0x9a);
/// 定时发送间隔上下界(秒),与桌面版同范围
const TIMER_INTERVAL_RANGE: std::ops::RangeInclusive<f64> = 0.001..=999.0;

/// 服务启动选项(main 解析命令行后传入)
pub struct WebOptions {
    /// --demo-log:内置演示数据源,无设备即可体验/测试
    pub demo: bool,
    /// HTTP 监听端口(仅绑定 127.0.0.1)
    pub port: u16,
    /// bind 成功后用系统默认浏览器打开管理台(--no-open 可关;
    /// 环境变量 RTT_WEB_NO_BROWSER=1 强制跳过,无头/测试场景)
    pub open_browser: bool,
}

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
    // ---- 偏好快照源(tick 内 500ms 比对落盘;未连接态也保存)----
    /// 启动时读到的完整偏好底版:桌面专用字段原样保留写回,不丢失旧值
    prefs_base: StoredPrefs,
    auto_frame: AtomicBool,
    search_regex: AtomicBool,
    chip_name: Mutex<String>,
    iface_index: Mutex<i32>,
    speed_index: Mutex<i32>,
    channel: Mutex<u32>,
    // ---- 定时发送(复用 /api/send 发送管线)----
    timer_on: AtomicBool,
    /// 定时发送间隔(秒)
    timer_interval: Mutex<f64>,
    /// 定时发送内容:最近一次 /api/send 的文本(可经 /api/timer 显式指定)
    timer_text: Mutex<String>,
    timer_hex: AtomicBool,
}

impl Shared {
    /// 发送管线(手动 /api/send 与定时发送共用):HEX/文本 → worker;回显 +
    /// TX 计数 + 记录为定时内容。demo(无 worker)只回显与计数(前端连接态
    /// 才允许发送,语义为虚拟发送)
    fn send_payload(&self, text: &str, hex: bool) -> Result<(), String> {
        if text.is_empty() {
            return Err("空输入".into());
        }
        let payload = if hex {
            parse_hex_bytes(text)?
        } else {
            text.as_bytes().to_vec()
        };
        if let Some(tx) = self.cmd_tx.lock().unwrap().clone() {
            let _ = tx.send(WorkerCmd::Send(payload));
        }
        self.tx_bytes
            .fetch_add(text.len() as u64, Ordering::Relaxed);
        self.pump
            .lock()
            .unwrap()
            .push_colored_line(&format!("» {text}"), ECHO_COLOR);
        *self.timer_text.lock().unwrap() = text.to_string();
        self.timer_hex.store(hex, Ordering::Relaxed);
        Ok(())
    }
}

/// 从 Shared 当前状态构建偏好快照(与桌面版 snapshot_prefs 同构)
fn snapshot_prefs(s: &Shared) -> StoredPrefs {
    let mut p = s.prefs_base.clone();
    p.chip_name = s.chip_name.lock().unwrap().clone();
    p.iface_index = *s.iface_index.lock().unwrap();
    p.speed_index = *s.speed_index.lock().unwrap();
    p.channel = *s.channel.lock().unwrap() as i32;
    p.rx_ending = *s.rx_ending.lock().unwrap();
    p.auto_frame = s.auto_frame.load(Ordering::Relaxed);
    p.frame_timeout = s.frame_timeout_ms.load(Ordering::Relaxed).to_string();
    p.encoding_index = s.encoding_index.load(Ordering::Relaxed) as i32;
    p.hex_rx = s.hex_rx.load(Ordering::Relaxed);
    p.search_regex = s.search_regex.load(Ordering::Relaxed);
    p.timer_send = s.timer_on.load(Ordering::Relaxed);
    p.timer_interval = format!("{}", *s.timer_interval.lock().unwrap());
    p
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
struct PauseReq {
    on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
struct PowerReq {
    on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsReq {
    #[serde(default)]
    rx_ending: Option<i32>,
    #[serde(default)]
    frame_timeout: Option<u32>,
    #[serde(default)]
    encoding_index: Option<i32>,
    #[serde(default)]
    hex_rx: Option<bool>,
    #[serde(default)]
    auto_frame: Option<bool>,
    #[serde(default)]
    search_regex: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimerReq {
    on: bool,
    #[serde(default)]
    interval_sec: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    hex: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkReq {
    #[serde(default)]
    text: String,
}

/// 启动服务(阻塞调用直到服务退出)。bind 失败(端口被占 = 已有实例)返回
/// Err,由调用方决定退出码。
pub fn run(opts: WebOptions) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(opts))
}

async fn serve(opts: WebOptions) -> anyhow::Result<()> {
    // 启动恢复偏好:文件缺失/损坏/字段缺失一律回落默认,绝不阻塞启动。
    // 前端 localStorage 已有主题/字号,不经此通道(保持不动)。
    let saved = config::load();
    let frame_timeout_init = saved
        .frame_timeout
        .trim()
        .parse::<u32>()
        .ok()
        .map(|v| v.clamp(1, 200))
        .unwrap_or(20);
    let timer_interval_init = saved
        .timer_interval
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| TIMER_INTERVAL_RANGE.contains(v))
        .unwrap_or(1.0);

    let (events_tx, _) = broadcast::channel(512);
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    let shared = Arc::new(Shared {
        pump: Mutex::new(LogPump::default()),
        rx_ending: Mutex::new(saved.rx_ending.clamp(0, 4)),
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
        frame_timeout_ms: Arc::new(AtomicU32::new(frame_timeout_init)),
        encoding_index: Arc::new(AtomicU32::new(saved.encoding_index.clamp(0, 4) as u32)),
        hex_rx: Arc::new(AtomicBool::new(saved.hex_rx)),
        device_names: Mutex::new(Vec::new()),
        jlinks: Mutex::new(Vec::new()),
        device_info: Mutex::new(None),
        demo_mode: opts.demo,
        prefs_base: saved.clone(),
        auto_frame: AtomicBool::new(saved.auto_frame),
        search_regex: AtomicBool::new(saved.search_regex),
        chip_name: Mutex::new(saved.chip_name),
        iface_index: Mutex::new(saved.iface_index.clamp(0, 1)),
        speed_index: Mutex::new(saved.speed_index.clamp(0, SPEEDS_KHZ.len() as i32 - 1)),
        channel: Mutex::new(saved.channel.clamp(0, 15) as u32),
        timer_on: AtomicBool::new(saved.timer_send),
        timer_interval: Mutex::new(timer_interval_init),
        timer_text: Mutex::new(String::new()),
        timer_hex: AtomicBool::new(false),
    });

    if opts.demo {
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
        .route("/api/prefs", get(api_prefs))
        .route("/api/themes", get(api_themes))
        .route("/api/devices", get(api_devices))
        .route("/api/jlinks", get(api_jlinks))
        .route("/api/connect", post(api_connect))
        .route("/api/disconnect", post(api_disconnect))
        .route("/api/power", post(api_power))
        .route("/api/reset", post(api_reset))
        .route("/api/settings", post(api_settings))
        .route("/api/send", post(api_send))
        .route("/api/timer", post(api_timer))
        .route("/api/mark", post(api_mark))
        .route("/api/pause", post(api_pause))
        .route("/api/clear", post(api_clear))
        .route("/api/export", get(api_export))
        .route("/ws", get(ws_handler))
        .with_state(shared);

    let addr = SocketAddr::from(([127, 0, 0, 1], opts.port));
    // 单实例:端口即互斥。bind 失败(AddrInUse)= 已有实例在跑,提示后交给
    // 调用方以非零码退出,不再用 CreateMutexW 方案
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            anyhow::bail!("已有实例运行于 http://{addr}(端口即单实例互斥;如需重启请先关闭旧实例,或用 --port 换端口)")
        }
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("监听 {addr} 失败")));
        }
    };
    println!(
        "[mini-rtt-viewer] 管理台 http://{addr}{}",
        if opts.demo { "  (demo)" } else { "" }
    );
    if opts.open_browser && std::env::var("RTT_WEB_NO_BROWSER").as_deref() != Ok("1") {
        open_in_browser(&format!("http://{addr}"));
    }
    axum::serve(listener, app).await?;
    Ok(())
}

/// 用系统默认浏览器打开管理台(Windows;cmd start 的首个引号参数是窗口标题,
/// 补空串占位)。失败静默:用户可手动访问地址栏
fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
}

/// 数据泵:消息消化 → cap → 增量上屏 → 事件广播;状态/设备信息同步进 Shared;
/// 偏好快照比对落盘;定时发送触发
fn tick_loop(shared: Arc<Shared>, msg_rx: mpsc::Receiver<WorkerMsg>) {
    let mut last_stats = Instant::now();
    // 上次落盘的偏好快照(不同才写;首 tick 必写一次,与桌面版一致)
    let mut last_prefs: Option<StoredPrefs> = None;
    // 上次定时发送时刻(启用后等满一个周期再发;未连接时挂起,恢复连接后
    // 因 elapsed 已超时立即发——与桌面版 tick step 6 同语义)
    let mut last_timer = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let rx_ending = *shared.rx_ending.lock().unwrap();
        let auto_frame = shared.auto_frame.load(Ordering::Relaxed);
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
                    if !pump.paused && auto_frame {
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
        if last_stats.elapsed() >= Duration::from_millis(500) {
            last_stats = Instant::now();
            let _ = shared.events_tx.send(format!(
                r#"{{"type":"stats","rx":{},"tx":{},"rows":{}}}"#,
                shared.rx_bytes.load(Ordering::Relaxed),
                shared.tx_bytes.load(Ordering::Relaxed),
                shared.pump.lock().unwrap().rows_len()
            ));
            // 偏好自动保存:与上次落盘快照不同才原子写(单文件几 KB,未连接态也保存)
            let snap = snapshot_prefs(&shared);
            if last_prefs.as_ref() != Some(&snap) {
                config::save(&snap);
                last_prefs = Some(snap);
            }
        }
        // 定时发送:开关 + 连接态 + 间隔合法 → 周期触发(复用 /api/send 发送管线,
        // 含回显/计数)
        if shared.timer_on.load(Ordering::Relaxed) && shared.connected.load(Ordering::Relaxed) {
            let secs = *shared.timer_interval.lock().unwrap();
            if TIMER_INTERVAL_RANGE.contains(&secs)
                && last_timer.elapsed() >= Duration::from_secs_f64(secs)
            {
                last_timer = Instant::now();
                let text = shared.timer_text.lock().unwrap().clone();
                if let Err(e) = shared.send_payload(&text, shared.timer_hex.load(Ordering::Relaxed))
                {
                    // 定时发送失败只写日志,不打断数据泵
                    eprintln!("[mini-rtt-viewer] 定时发送失败:{e}");
                }
            }
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
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/png")],
        FAVICON,
    )
        .into_response()
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

/// 当前偏好快照(面板初值恢复;主题/字号在前端 localStorage,不含在内)
async fn api_prefs(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "chip": shared.chip_name.lock().unwrap().clone(),
        "ifaceIndex": *shared.iface_index.lock().unwrap(),
        "speedIndex": *shared.speed_index.lock().unwrap(),
        "channel": *shared.channel.lock().unwrap(),
        "rxEnding": *shared.rx_ending.lock().unwrap(),
        "frameTimeout": shared.frame_timeout_ms.load(Ordering::Relaxed),
        "encodingIndex": shared.encoding_index.load(Ordering::Relaxed) as i32,
        "hexRx": shared.hex_rx.load(Ordering::Relaxed),
        "autoFrame": shared.auto_frame.load(Ordering::Relaxed),
        "searchRegex": shared.search_regex.load(Ordering::Relaxed),
        "timerOn": shared.timer_on.load(Ordering::Relaxed),
        "timerIntervalSec": *shared.timer_interval.lock().unwrap(),
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
        return Json(demo_device_names());
    }
    Json(shared.device_names.lock().unwrap().clone())
}

/// demo 模式的演示设备库(/api/devices 与 /api/connect 校验同源)
fn demo_device_names() -> Vec<String> {
    vec![
        "STM32F103C8".into(),
        "STM32F030C8".into(),
        "STM32G474VET6".into(),
        "STM32H743ZIT6".into(),
        "GD32F303CCT6".into(),
        "CH32V307VCT6".into(),
    ]
}

async fn api_jlinks(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let list = shared.jlinks.lock().unwrap().clone();
    Json(serde_json::json!(list
        .iter()
        .map(|(sn, name)| serde_json::json!({"sn": sn, "name": name}))
        .collect::<Vec<_>>()))
}

/// 连接:芯片名补全(库内子串匹配首个全称,与桌面版同规则)→ 连接参数写入
/// 偏好快照源 → spawn worker(demo 模式只记录参数,不加载 DLL)
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
    // 校验名单与前端看到的候选同源:demo 用演示设备库,真机用设备库枚举结果
    let full: Vec<String> = if shared.demo_mode {
        demo_device_names()
    } else {
        shared.device_names.lock().unwrap().clone()
    };
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

    // 连接参数进偏好快照源(tick 内 500ms 落盘;重启后面板初值恢复)
    *shared.chip_name.lock().unwrap() = chip.clone();
    *shared.iface_index.lock().unwrap() = req.iface_index.min(1) as i32;
    *shared.speed_index.lock().unwrap() = req.speed_index.min(SPEEDS_KHZ.len() - 1) as i32;
    *shared.channel.lock().unwrap() = req.channel.min(15);

    if shared.demo_mode {
        return StatusCode::OK.into_response();
    }

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
    if let Some(v) = req.auto_frame {
        shared.auto_frame.store(v, Ordering::Relaxed);
    }
    if let Some(v) = req.search_regex {
        shared.search_regex.store(v, Ordering::Relaxed);
    }
    StatusCode::OK
}

async fn api_send(State(shared): State<Arc<Shared>>, Json(req): Json<SendReq>) -> Response {
    if req.text.is_empty() {
        return (StatusCode::BAD_REQUEST, "空输入").into_response();
    }
    // 空输入已在上面拦掉:此处 Err 只可能是 HEX 解析失败
    if let Err(e) = shared.send_payload(&req.text, req.hex) {
        return (StatusCode::BAD_REQUEST, format!("HEX 格式错误:{e}")).into_response();
    }
    StatusCode::OK.into_response()
}

async fn api_timer(State(shared): State<Arc<Shared>>, Json(req): Json<TimerReq>) -> Response {
    if let Some(v) = req.interval_sec {
        if !TIMER_INTERVAL_RANGE.contains(&v) {
            return (StatusCode::BAD_REQUEST, "定时间隔需在 0.001-999 秒").into_response();
        }
        *shared.timer_interval.lock().unwrap() = v;
    }
    if let Some(text) = req.text {
        *shared.timer_text.lock().unwrap() = text;
    }
    if let Some(hex) = req.hex {
        shared.timer_hex.store(hex, Ordering::Relaxed);
    }
    shared.timer_on.store(req.on, Ordering::Relaxed);
    StatusCode::OK.into_response()
}

// 本地时间(Win32 GetLocalTime,零依赖):标记行时间戳与导出文件名用。
// std 不提供本地时区时间,单独为此引 chrono/time 不值当。
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

fn local_time() -> WinSystemTime {
    let mut st = WinSystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    unsafe { GetLocalTime(&mut st) };
    st
}

/// "HH:MM:SS"(标记行内嵌)
fn hms_stamp() -> String {
    let t = local_time();
    format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
}

/// "YYYYMMDD_HHMMSS"(导出文件名)
fn now_stamp() -> String {
    let t = local_time();
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
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

/// 导出当前显示的全部日志(.log 纯文本附件;与桌面版 save_log 同内容形态:
/// 行文本 + \r\n 行尾,时间戳取本地时间)
async fn api_export(State(shared): State<Arc<Shared>>) -> Response {
    let body = {
        let pump = shared.pump.lock().unwrap();
        let mut body = String::new();
        for row in pump.snapshot_rows() {
            for run in row {
                body.push_str(&run.text);
            }
            body.push_str("\r\n");
        }
        body
    };
    let disposition = format!("attachment; filename=\"rtt_{}.log\"", now_stamp());
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
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
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                if socket.send(Message::Ping("".into())).await.is_err() {
                    break;
                }
            }
            else => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parse_tolerates_separators_and_prefix() {
        assert_eq!(parse_hex_bytes("41 42 43").unwrap(), vec![0x41, 0x42, 0x43]);
        assert_eq!(parse_hex_bytes("41:42-43").unwrap(), vec![0x41, 0x42, 0x43]);
        assert_eq!(parse_hex_bytes("0x4142").unwrap(), vec![0x41, 0x42]);
        assert_eq!(parse_hex_bytes("aabb").unwrap(), vec![0xaa, 0xbb]);
    }

    #[test]
    fn hex_parse_rejects_bad_input() {
        assert!(parse_hex_bytes("abc").is_err()); // 奇数位
        assert!(parse_hex_bytes("zz").is_err()); // 非法字符
        assert!(parse_hex_bytes("  ").is_err()); // 空
    }

    #[test]
    fn settings_req_accepts_camel_case_json() {
        // 前端契约:camelCase 字段必须命中(修复前 snake_case 静默丢失为 None)
        let req: SettingsReq =
            serde_json::from_str(r#"{"rxEnding":2,"frameTimeout":55,"encodingIndex":1,"hexRx":true,"autoFrame":false,"searchRegex":true}"#).unwrap();
        assert_eq!(req.rx_ending, Some(2));
        assert_eq!(req.frame_timeout, Some(55));
        assert_eq!(req.encoding_index, Some(1));
        assert_eq!(req.hex_rx, Some(true));
        assert_eq!(req.auto_frame, Some(false));
        assert_eq!(req.search_regex, Some(true));
    }

    #[test]
    fn connect_req_accepts_camel_case_json() {
        let req: ConnectReq = serde_json::from_str(
            r#"{"chip":"STM32F103C8","ifaceIndex":1,"speedIndex":3,"channel":2}"#,
        )
        .unwrap();
        assert_eq!(req.chip, "STM32F103C8");
        assert_eq!(req.iface_index, 1);
        assert_eq!(req.speed_index, 3);
        assert_eq!(req.channel, 2);
    }

    #[test]
    fn timer_req_accepts_camel_case_json() {
        let req: TimerReq =
            serde_json::from_str(r#"{"on":true,"intervalSec":0.5,"text":"led on","hex":false}"#)
                .unwrap();
        assert!(req.on);
        assert_eq!(req.interval_sec, Some(0.5));
        assert_eq!(req.text.as_deref(), Some("led on"));
        assert_eq!(req.hex, Some(false));
    }
}
