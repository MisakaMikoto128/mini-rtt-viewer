//! 浏览器管理台服务(ADR-11:UI 全面转 Web 形态):**一个 exe,双击启动桌面壳
//! (内嵌同一管理台的 WebView)并在本机起服务**;也可 `--no-window` 纯服务后用
//! 浏览器访问 `http://127.0.0.1:8686`(SerialHub 同构——Rust 数据层 + 内嵌
//! Web UI)。两种形态共用同一套数据层与 worker
//! (rtt::spawn / LogPump / device_db)。
//!
//! API 契约(与前端/测试对齐,改这里必同步 ui/web/index.html 与 tests/web):
//! - GET  /                管理台单页
//! - GET  /api/status      → {connected, rxBytes, txBytes, rowsTotal, paused,
//!   uptimeSec, sessionSec, sessionStatus, device, version, backend};device 8 字段
//!   {firmware,hardware,serial,core,cpu,target,iface,speedKhz};sessionSec=连接
//!   会话秒,断开清零;backend=当前数据采集后端 id(正式程序为 jlink)
//! - GET  /api/backends    → {selected, items:[{id,name,description,isVirtual}]}
//!   正式程序 items 仅 J-Link,独立 QA 进程可注入测试后端。
//! - GET  /api/prefs       → 持久化偏好快照(面板初值恢复)
//! - GET  /api/themes      → [{id, name, custom}];内置 4 套(custom:false)+
//!   自定义项(exe 旁 themes/*.css,id=文件名去扩展,name=文件名,custom:true)
//! - GET  /api/theme-css/{id} → 自定义主题 CSS 文本(text/css);内置主题与未知
//!   id 一律 404(内置主题由前端内嵌表覆盖,不落盘)
//! - GET  /api/devices     → [芯片型号]
//! - GET  /api/jlinks      → [{sn, name}]
//! - GET  /api/history     → [text] 发送历史(最新在前,上限 50)
//! - POST /api/connect     {chip, ifaceIndex, speedIndex, channel, jlinkSn?};
//!   jlinkSn = 选中的序列号(缺省/无效回落列表首台);连接参数写入偏好快照源
//! - POST /api/disconnect  停止当前 worker,Exited 后回到未连接态
//! - POST /api/power       {on};未连接 409
//! - POST /api/reset       {mode?: "in-place"|"reconnect"};缺省 in-place;
//!   reconnect=先走断开逻辑,1 秒后用上次连接参数重新 spawn worker(无上次
//!   参数 409);空请求体容忍为缺省 in-place
//! - POST /api/settings    逐字段可选(camelCase):rxEnding / frameTimeout /
//!   encodingIndex / hexRx / autoFrame / searchRegex / sendEnding / hexSend /
//!   chip / ifaceIndex / speedIndex / channel(连接设置组持久化,F1)/
//!   backend(阶段二切数据源:不在可用列表的 id → 400;连接态判据
//!   shared.connected → 409「断开后才能切换后端」;成功写偏好并切换)
//! - POST /api/send        {text, hex?} → 回显一行,计数 TX,内容记为定时发送源;
//!   成功后记入发送历史(prefs.send_history,去重置顶上限 50,与桌面版同规则)
//! - POST /api/timer       {on, intervalSec?, text?, hex?};定时重发 0.001-999 秒,
//!   连接态才触发,复用 /api/send 发送管线,含回显
//! - POST /api/mark        {text}  → 插入会话标记行
//! - POST /api/pause       {on}
//! - POST /api/clear
//! - GET  /api/export      → text/plain 附件下载(rtt_<时间戳>.log,全部行文本)
//! - POST /api/open-browser 在系统默认浏览器打开管理台(壳内前端按钮用;
//!   URL 由服务端监听端口拼出,不接受客户端传入)
//! - WS   /ws              {type:"rows"/"snapshot"/"cleared"} 与
//!   {type:"state"/"device"/"progress"/"names"/"jlinks"/"stats"}
//!
//! /api/prefs 返回:{chip, ifaceIndex, speedIndex, channel, rxEnding, frameTimeout,
//! encodingIndex, hexRx, autoFrame, searchRegex, timerOn, timerIntervalSec,
//! sendEnding, hexSend, backend}
//!
//! 偏好持久化(与桌面版同模式):`%APPDATA%/MiniRttViewer/prefs.json`(环境变量
//! `RTT_PREFS_FILE` 显式指定路径时优先,测试/便携场景的 prefs 隔离),数据泵
//! tick 内 500ms 快照比对——从 Shared 当前状态构建 StoredPrefs 子集,变化才原子
//! 写(未连接态也保存)。桌面专用字段(window 几何/dark_theme/theme 等)沿用
//! 启动时读到的原值写回,serde(default) 兼容旧文件;主题/字号仍由前端
//! localStorage 管理,不经此通道。
//!
//! 主题插件化(SerialHub ADR-18 同构):内置 4 套由前端内嵌;exe 旁 themes/
//! 目录下每个 *.css 即一套自定义主题(只覆盖 :root 设计令牌),`GET /api/themes`
//! 每次现扫(拖入即生效,零注册),CSS 经 `/api/theme-css/{id}` 服务——路径只
//! 来自本机扫描列表,不拼用户输入,无穿越面。
//!
//! 单实例:端口即互斥——bind AddrInUse 时提示已有实例并以非零码退出。
//!
//! 装配形态(SerialHub 同构整合要点,gui.rs 模块头另有细则):
//! - 纯服务(`--no-window`):`run()` 在调用线程建 tokio runtime 阻塞跑;
//! - 桌面壳:`spawn_gui_service()` 把服务整体搬**后台线程**(tao 的
//!   `EventLoop::run` 占死主线程,Win32 窗口/托盘也必须在主线程),服务就绪
//!   经 ready 通道握手,连接状态变化经 `OnEvent` 回调打回主线程驱动托盘图标。

use crate::ansi;
use crate::backend::{
    self, ConnectParams, DeviceInfo, Host, RttBackend, SessionHandle, WorkerCmd, WorkerMsg,
};
use crate::config::{self, StoredPrefs};
use crate::log_model::{LogPump, DEFAULT_FRAME_TIMEOUT_MS, FLUSH_MS, MAX_LOG_ROWS};
use crate::rtt::SPEEDS_KHZ;
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

mod api;
mod pump;
mod state;
mod theme;
mod ws;

pub(crate) use api::*;
pub(crate) use pump::*;
pub(crate) use state::*;
pub(crate) use theme::*;
pub(crate) use ws::*;

/// 管理台与静态模块(build 时内嵌,运行时不依赖工作目录)
const INDEX_HTML: &str = include_str!("../ui/web/index.html");
const STYLES_CSS: &str = include_str!("../ui/web/styles.css");
const APP_JS: &str = include_str!("../ui/web/app.js");
const API_JS: &str = include_str!("../ui/web/api.js");
const LOG_VIEW_JS: &str = include_str!("../ui/web/log-view.js");
const SEARCH_JS: &str = include_str!("../ui/web/search.js");
const SETTINGS_JS: &str = include_str!("../ui/web/settings.js");
const DIALOGS_JS: &str = include_str!("../ui/web/dialogs.js");
const SOUND_JS: &str = include_str!("../ui/web/sound.js");
/// favicon(build 时内嵌,不依赖进程工作目录;运行时读 assets/ 在 cwd 不对时 404)
const FAVICON: &[u8] = include_bytes!("../assets/app-32.png");
/// 会话标记行颜色(与桌面版 MARK_COLOR 一致)
const MARK_COLOR: (u8, u8, u8) = (0x28, 0xaf, 0xe9);
/// 发送回显行颜色(中性灰,与设备数据一眼区分)
const ECHO_COLOR: (u8, u8, u8) = (0x8f, 0x8f, 0x9a);
/// 定时发送间隔上下界(秒),与桌面版同范围
const TIMER_INTERVAL_RANGE: std::ops::RangeInclusive<f64> = 0.001..=999.0;
/// 复位模式字段值(API 契约):in-place = 现有 Reset 命令(重挂 RTT 续收)
const RESET_MODE_IN_PLACE: &str = "in-place";
/// 复位模式字段值(API 契约):reconnect = 先断开,1 秒后用上次参数重连
const RESET_MODE_RECONNECT: &str = "reconnect";
/// 内置主题 id(与前端 index.html 内嵌主题表一一对应;themes/ 里的同名 .css
/// 不进列表,避免下拉出现两个相同 value)
const BUILTIN_THEME_IDS: [&str; 4] = ["dark", "light", "oled", "sepia"];

/// 服务启动选项(main 解析命令行后传入)
pub struct WebOptions {
    /// --backend <id>:启动时选定数据采集后端;必须命中可用列表
    /// 才生效,非法值忽略(回落偏好/缺省)。缺省 None = 按偏好 backend
    pub backend: Option<String>,
    /// HTTP 监听端口(端口即单实例互斥)
    pub port: u16,
    /// HTTP 监听地址:默认 127.0.0.1 仅本机;0.0.0.0 显式开放局域网/互联网
    /// (管理台无鉴权,SECURITY.md 如实说明)。指定具体网卡 IP 时按该 IP 绑定
    pub listen: std::net::IpAddr,
    /// bind 成功后用系统默认浏览器打开管理台(当前 main 两种形态均传 false:
    /// 窗口模式由 WebView 承载界面、托盘菜单按需开浏览器;纯服务面向自动化;
    /// 环境变量 RTT_WEB_NO_BROWSER=1 可强制跳过,无头/测试场景)
    pub open_browser: bool,
}

/// 服务 → GUI 的事件(经 `OnEvent` 回调打回主线程,SerialHub service.rs 同构)。
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// 服务就绪(管理台已绑定该地址)——仅启动时一次,gui 侧走 ready 握手。
    Ready(SocketAddr),
    /// 连接状态变化(驱动托盘图标刷新)。
    ConnectedChanged(bool),
}

/// 事件回调:在服务线程/数据泵线程触发,**必须非阻塞**
/// (gui 侧只做 `proxy.send_event`,绝不碰窗口/托盘)。
pub type OnEvent = std::sync::Arc<dyn Fn(ServiceEvent) + Send + Sync>;

/// 纯服务模式(`--no-window`):调用线程建 tokio runtime 阻塞跑直到进程退出。
/// bind 失败(端口被占 = 已有实例)返回 Err,由调用方决定退出码。
pub fn run(opts: WebOptions) -> anyhow::Result<()> {
    run_internal(opts, None)
}

/// 用调用方提供的后端运行同一 Web 层，供仓库内 QA 可执行夹具使用。
/// 正式入口仍只通过 `run` 注册 J-Link。
#[doc(hidden)]
pub fn run_with_backend(opts: WebOptions, backend: &'static dyn RttBackend) -> anyhow::Result<()> {
    run_internal(opts, Some(backend))
}

fn run_internal(
    opts: WebOptions,
    backend_override: Option<&'static dyn RttBackend>,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(opts, None, backend_override))
}

/// 桌面壳装配:服务整体搬后台线程(主线程留给 tao 事件循环,见模块头注释)。
/// 返回 ready 通道:`Ok(addr)` = 服务就绪;`Err` = 启动失败(端口被占等)。
/// 就绪后的连接状态变化经 `on_event` 回调转发(gui 侧映射成 EventLoopProxy 事件)。
///
/// 契约(审计登记的一次性语义):ready 通道是**单向一次性**握手,服务线程
/// 没有 cancel 句柄——**调用方在 READY_TIMEOUT 超时后必须退出进程**(main.rs
/// 对 run_gui 的 Err 一律 process::exit),否则后台服务线程仍持有 tokio
/// runtime 与可能已成功的端口绑定,下次启动会被误判「已有实例」。
pub fn spawn_gui_service(
    opts: WebOptions,
    on_event: OnEvent,
) -> Result<std::sync::mpsc::Receiver<Result<SocketAddr, String>>, String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<SocketAddr, String>>();
    std::thread::Builder::new()
        .name("rtt-service".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("tokio runtime 启动失败: {e}")));
                    return;
                }
            };
            // Ready 走 ready 握手(仅一次,窗口创建前);其余事件转发给 gui 回调
            let ready_tx_for_cb = ready_tx.clone();
            let cb: OnEvent = std::sync::Arc::new(move |ev| match ev {
                ServiceEvent::Ready(addr) => {
                    let _ = ready_tx_for_cb.send(Ok(addr));
                }
                other => on_event(other),
            });
            if let Err(e) = rt.block_on(serve(opts, Some(cb), None)) {
                // 就绪前失败(端口被占)必须立即回传主线程;就绪后失败 ready 端
                // 已被 gui 丢弃,发送失败无妨
                let _ = ready_tx.send(Err(format!("{e:#}")));
            }
            // block_on 返回 = 服务退出,runtime 随之析构(进程退出时随进程终止)
        })
        .map_err(|e| format!("服务线程启动失败: {e}"))?;
    Ok(ready_rx)
}

/// 后端选定:CLI --backend → 偏好 backend → 可用列表首个。
fn resolve_backend(
    cli: Option<&str>,
    pref: &str,
    available: &[&'static dyn RttBackend],
) -> &'static dyn RttBackend {
    let pick = |id: &str| available.iter().copied().find(|b| b.id() == id);
    if let Some(id) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(b) = pick(id) {
            return b;
        }
    }
    if let Some(b) = pick(pref.trim()) {
        return b;
    }
    available[0]
}

async fn serve(
    opts: WebOptions,
    on_event: Option<OnEvent>,
    backend_override: Option<&'static dyn RttBackend>,
) -> anyhow::Result<()> {
    // 启动恢复偏好:文件缺失/损坏/字段缺失一律回落默认,绝不阻塞启动。
    // 前端 localStorage 已有主题/字号,不经此通道(保持不动)。
    let saved = config::load();
    let frame_timeout_init = saved
        .frame_timeout
        .trim()
        .parse::<u32>()
        .ok()
        .map(|v| v.clamp(1, 200))
        .unwrap_or(DEFAULT_FRAME_TIMEOUT_MS);
    let timer_interval_init = saved
        .timer_interval
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| TIMER_INTERVAL_RANGE.contains(v))
        .unwrap_or(1.0);

    let (events_tx, _) = broadcast::channel(512);
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    // 正式进程只有 J-Link；QA 夹具可注入独立测试后端，不进入正式 exe。
    let backends_available: Vec<&'static dyn RttBackend> = match backend_override {
        Some(backend) => vec![backend],
        None => vec![backend::backend_by_id(backend::BACKEND_JLINK).expect("jlink 后端未注册")],
    };
    // 选定:CLI --backend → 偏好 backend(serve 内归一化)→ 列表首个
    let backend = resolve_backend(opts.backend.as_deref(), &saved.backend, &backends_available);
    // saved 的字段随后被逐个 move/partial move,先取出共享原子的初值
    let jlink_serial_init = saved.jlink_serial;
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
        session: Mutex::new(None),
        msg_tx: msg_tx.clone(),
        reconnecting: AtomicBool::new(false),
        frame_timeout_ms: Arc::new(AtomicU32::new(frame_timeout_init)),
        encoding_index: Arc::new(AtomicU32::new(saved.encoding_index.clamp(0, 4) as u32)),
        hex_rx: Arc::new(AtomicBool::new(saved.hex_rx)),
        device_names: Mutex::new(Vec::new()),
        jlinks: Mutex::new(Vec::new()),
        jlink_serial: Mutex::new(jlink_serial_init),
        device_info: Mutex::new(None),
        backend: Mutex::new(backend),
        // Vec<&'static> 克隆只是拷几个指针;装配块后还要用原列表找 jlink 启动枚举
        backends_available: backends_available.clone(),
        prefs_base: saved.clone(),
        auto_frame: AtomicBool::new(saved.auto_frame),
        search_regex: AtomicBool::new(saved.search_regex),
        // 上次连接参数从偏好底版恢复(chip 为空 = 从未连接 → reconnect 复位
        // 409);必须在 saved.chip_name 被 move 之前借用
        last_connect: Mutex::new(ConnectParams::from_prefs(&saved)),
        chip_name: Mutex::new(saved.chip_name),
        iface_index: Mutex::new(saved.iface_index.clamp(0, 1)),
        speed_index: Mutex::new(saved.speed_index.clamp(0, SPEEDS_KHZ.len() as i32 - 1)),
        channel: Mutex::new(saved.channel.clamp(0, 15) as u32),
        send_history: Mutex::new(saved.send_history.clone()),
        send_ending: Mutex::new(saved.send_ending.clamp(0, 3)),
        hex_send: AtomicBool::new(saved.hex_send),
        timer_on: AtomicBool::new(saved.timer_send),
        timer_interval: Mutex::new(timer_interval_init),
        timer_text: Mutex::new(String::new()),
        timer_hex: AtomicBool::new(false),
        session_start: Mutex::new(None),
        status_text: Mutex::new("未连接".into()),
        on_state: on_event.clone(),
        port: opts.port,
        listen: opts.listen,
    });

    // 后端装配:J-Link 启动设备库后台枚举,选定后端经 on_selected 初始化候选。
    // QA 后端不会命中 J-Link 枚举分支,因此测试进程不加载 DLL。
    if let Some(jlink) = backends_available
        .iter()
        .copied()
        .find(|b| b.id() == backend::BACKEND_JLINK)
    {
        jlink.start_background_enumeration(&*shared);
    }
    backend.on_selected(&*shared);

    // 数据泵线程:消化 worker 消息 → pump → 事件广播(10ms,与桌面版 tick 同构)
    {
        let shared = shared.clone();
        std::thread::spawn(move || tick_loop(shared, msg_rx));
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/favicon.png", get(favicon))
        .route(
            "/styles.css",
            get(|| async { embedded_asset(STYLES_CSS, "text/css; charset=utf-8") }),
        )
        .route(
            "/app.js",
            get(|| async { embedded_asset(APP_JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/api.js",
            get(|| async { embedded_asset(API_JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/log-view.js",
            get(|| async { embedded_asset(LOG_VIEW_JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/search.js",
            get(|| async { embedded_asset(SEARCH_JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/settings.js",
            get(|| async { embedded_asset(SETTINGS_JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/dialogs.js",
            get(|| async { embedded_asset(DIALOGS_JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/sound.js",
            get(|| async { embedded_asset(SOUND_JS, "text/javascript; charset=utf-8") }),
        )
        .route("/api/status", get(api_status))
        .route("/api/backends", get(api_backends))
        .route("/api/prefs", get(api_prefs))
        .route("/api/themes", get(api_themes))
        .route("/api/theme-css/{id}", get(api_theme_css))
        .route("/api/devices", get(api_devices))
        .route("/api/jlinks", get(api_jlinks))
        .route("/api/history", get(api_history))
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
        .route("/api/open-browser", post(api_open_browser))
        .route("/api/addr", get(api_addr))
        .route("/ws", get(ws_handler))
        .with_state(shared);

    let bind_addr = SocketAddr::new(opts.listen, opts.port);
    // 展示/WebView 加载地址:绑 0.0.0.0 时回环同样可达(全接口含 127.0.0.1),
    // 但 "http://0.0.0.0" 不是可访问 URL,归一成 127.0.0.1;绑具体网卡 IP 时
    // 回环未监听,只能用该 IP 访问
    let addr = SocketAddr::new(
        if opts.listen.is_unspecified() {
            std::net::IpAddr::from([127, 0, 0, 1])
        } else {
            opts.listen
        },
        opts.port,
    );
    // 单实例:端口即互斥。bind 失败(AddrInUse)= 已有实例在跑,提示后交给
    // 调用方以非零码退出,不再用 CreateMutexW 方案
    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            anyhow::bail!("已有实例运行于 http://{addr}(端口即单实例互斥;如需重启请先关闭旧实例,或用 --port 换端口)")
        }
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("监听 {addr} 失败")));
        }
    };
    println!("[mini-rtt-viewer] 管理台 http://{addr}");
    // 就绪握手:gui 壳收到后才创建窗口/WebView(端口被占已在上面报错)
    if let Some(cb) = &on_event {
        cb(ServiceEvent::Ready(addr));
    }
    if opts.open_browser && std::env::var("RTT_WEB_NO_BROWSER").as_deref() != Ok("1") {
        open_in_browser(&format!("http://{addr}"));
    }
    axum::serve(listener, app).await?;
    Ok(())
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
        // FR-11/F8a:每段容忍 0x/0X 前缀("0x41 0x42" ≡ "41 42")
        assert_eq!(parse_hex_bytes("0x41 0x42").unwrap(), vec![0x41, 0x42]);
        assert_eq!(parse_hex_bytes("0X41 0X42").unwrap(), vec![0x41, 0x42]);
        assert_eq!(parse_hex_bytes("0x41:0x42").unwrap(), vec![0x41, 0x42]);
    }

    #[test]
    fn hex_parse_rejects_bad_input() {
        assert!(parse_hex_bytes("abc").is_err()); // 奇数位
        assert!(parse_hex_bytes("zz").is_err()); // 非法字符
        assert!(parse_hex_bytes("  ").is_err()); // 空
        assert!(parse_hex_bytes("0x").is_err()); // 只有前缀 = 空
    }

    #[test]
    fn send_ending_appends_bytes_per_selection() {
        // FR-12/F7:0=CRLF 1=LF 2=CR 3=无;TX 计数按追加后字节数
        assert_eq!(append_send_ending(b"x".to_vec(), 0), b"x\r\n");
        assert_eq!(append_send_ending(b"x".to_vec(), 1), b"x\n");
        assert_eq!(append_send_ending(b"x".to_vec(), 2), b"x\r");
        assert_eq!(append_send_ending(b"x".to_vec(), 3), b"x");
        assert_eq!(append_send_ending(b"x".to_vec(), 99), b"x"); // 越界回落无行尾
                                                                 // HEX 模式同理:解析字节 + 行尾字节
        let hex = parse_hex_bytes("0x41 0x42").unwrap();
        assert_eq!(append_send_ending(hex, 1), vec![0x41, 0x42, b'\n']);
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
        // F1:连接设置组五项经同一 settings 通道(camelCase 命中)
        let req: SettingsReq = serde_json::from_str(
            r#"{"chip":"MYCHIP-TEST1","ifaceIndex":1,"speedIndex":4,"channel":3,"autoFrame":false}"#,
        )
        .unwrap();
        assert_eq!(req.chip.as_deref(), Some("MYCHIP-TEST1"));
        assert_eq!(req.iface_index, Some(1));
        assert_eq!(req.speed_index, Some(4));
        assert_eq!(req.channel, Some(3));
        assert_eq!(req.auto_frame, Some(false));
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

    #[test]
    fn reset_req_tolerates_missing_mode() {
        // 契约:mode 缺省 in-place;显式 reconnect 命中
        let req: ResetReq = serde_json::from_str(r#"{"mode":"reconnect"}"#).unwrap();
        assert_eq!(req.mode.as_deref(), Some("reconnect"));
        let empty: ResetReq = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.mode, None);
    }

    #[test]
    fn reset_mode_defaults_and_falls_back_on_unknown() {
        assert_eq!(parse_reset_mode(None), ResetMode::InPlace); // 缺省
        assert_eq!(parse_reset_mode(Some("in-place")), ResetMode::InPlace);
        assert_eq!(parse_reset_mode(Some("reconnect")), ResetMode::Reconnect);
        assert_eq!(parse_reset_mode(Some("")), ResetMode::InPlace); // 未知值回落
        assert_eq!(parse_reset_mode(Some("garbage")), ResetMode::InPlace);
    }

    #[test]
    fn state_marker_line_uses_manual_mark_style() {
        assert_eq!(marker_line("已连接", "01:02:03"), "── [01:02:03] 已连接 ──");
        assert_eq!(marker_line("已断开", "23:59:59"), "── [23:59:59] 已断开 ──");
        // 自动标记行 = 同款式 + 实时时间戳(时间不定,只验证标签与骨架)
        let up = state_marker_line(true);
        assert!(
            up.starts_with("── [") && up.ends_with("] 已连接 ──"),
            "{up}"
        );
        let down = state_marker_line(false);
        assert!(
            down.starts_with("── [") && down.ends_with("] 已断开 ──"),
            "{down}"
        );
    }

    #[test]
    fn clean_status_text_strips_banner_bullet() {
        assert_eq!(clean_status_text("● 已连接 (J-Link)"), "已连接 (J-Link)");
        assert_eq!(clean_status_text("● 未连接"), "未连接");
        assert_eq!(clean_status_text("plain"), "plain");
        // 双 bullet / 前导空格变体(审计 P3:单次剥离剥不干净)
        assert_eq!(clean_status_text("● ● 已连接"), "已连接");
        assert_eq!(clean_status_text("  ● 已连接"), "已连接");
    }

    #[test]
    fn device_info_json_keys_match_status_contract() {
        // 审计 P3(Device 分支自引用恒真):键名契约在这里显式锁定——
        // WS device 事件与 /api/status device 字段共用 device_info_json 单点
        let info = DeviceInfo {
            firmware: "f".into(),
            hardware: "h".into(),
            serial: "1".into(),
            core_name: "c".into(),
            core_id: "i".into(),
            core_cpu: "cpu".into(),
            target: "t".into(),
            iface: "SWD".into(),
            speed_khz: 4000,
        };
        let v = device_info_json(&info);
        for k in [
            "firmware", "hardware", "serial", "core", "cpu", "target", "iface", "speedKhz",
        ] {
            assert!(v.get(k).is_some(), "device 契约缺字段 {k}");
        }
        assert_eq!(v.get("speedKhz"), Some(&serde_json::json!(4000)));
    }

    #[test]
    fn settings_negative_values_rejected() {
        // 审计 P2:负数必须能在反序列化后识别(api_settings 据此 400),
        // 而不是被 clamp 静默夹成 0
        let req: SettingsReq = serde_json::from_str(r#"{"rxEnding":-1}"#).unwrap();
        assert_eq!(req.rx_ending, Some(-1));
        let req: SettingsReq = serde_json::from_str(r#"{"speedIndex":-5}"#).unwrap();
        assert_eq!(req.speed_index, Some(-5));
    }

    #[test]
    fn settings_req_accepts_backend_key() {
        // backend 可选键(缺省 None = 不切换请求;命中 = 校验并切换)
        let req: SettingsReq = serde_json::from_str(r#"{"backend":"jlink"}"#).unwrap();
        assert_eq!(req.backend.as_deref(), Some("jlink"));
        let req: SettingsReq = serde_json::from_str("{}").unwrap();
        assert_eq!(req.backend, None);
        // 与其它字段共存(逐字段可选语义)
        let req: SettingsReq = serde_json::from_str(r#"{"backend":"jlink","hexRx":true}"#).unwrap();
        assert_eq!(req.backend.as_deref(), Some("jlink"));
        assert_eq!(req.hex_rx, Some(true));
    }

    #[test]
    fn resolve_backend_cli_then_pref_then_jlink() {
        let jlink = backend::backend_by_id(backend::BACKEND_JLINK).unwrap();
        let available = [jlink];
        let id_of = |b: &'static dyn RttBackend| b.id();
        assert_eq!(
            id_of(resolve_backend(Some("jlink"), "", &available)),
            "jlink"
        );
        // 旧偏好或 CLI 中的已删除后端不再可选，统一回落 J-Link。
        for legacy in ["demo", "probe-rs", "", "JLINK"] {
            assert_eq!(
                id_of(resolve_backend(Some(legacy), legacy, &available)),
                "jlink"
            );
            assert_eq!(id_of(resolve_backend(None, legacy, &available)), "jlink");
        }
    }

    #[test]
    fn connect_req_tolerates_missing_jlink_sn() {
        // jlinkSn 可选(缺省 = 自动选首台);命中时反序列化为 Some
        let req: ConnectReq = serde_json::from_str(r#"{"chip":"STM32F103C8"}"#).unwrap();
        assert_eq!(req.jlink_sn, None);
        let req: ConnectReq =
            serde_json::from_str(r#"{"chip":"STM32F103C8","jlinkSn":600788888}"#).unwrap();
        assert_eq!(req.jlink_sn, Some(600788888));
    }

    #[test]
    fn history_push_dedupes_moves_to_front_and_caps() {
        let mut h: Vec<String> = Vec::new();
        history_push(&mut h, "a");
        history_push(&mut h, "b");
        history_push(&mut h, "c");
        assert_eq!(h, vec!["c", "b", "a"]); // 最新在前
        history_push(&mut h, "a"); // 去重 + 置顶
        assert_eq!(h, vec!["a", "c", "b"]);
        for i in 0..60 {
            history_push(&mut h, &format!("x{i}"));
        }
        assert_eq!(h.len(), 50); // 上限 50
        assert_eq!(h[0], "x59");
        history_push(&mut h, "b"); // 超限外的旧项重发 → 置顶,尾部挤掉一条
        assert_eq!(h[0], "b");
        assert_eq!(h.len(), 50);
    }

    #[test]
    fn theme_file_name_whitelist_rules() {
        assert!(valid_theme_file_name("my-theme_1.css"));
        assert!(valid_theme_file_name("T.user.css"));
        // 拒绝:非 css / 分隔符 / 穿越 / 空扩展名 / 超长 / 非 ASCII
        for bad in [
            "plain.txt",
            "a/b.css",
            "a\\b.css",
            "../x.css",
            ".css",
            "",
            &format!("{}.css", "x".repeat(64)),
            "主题.css",
            "ok .css",
        ] {
            assert!(!valid_theme_file_name(bad), "\"{bad}\" 应被拒绝");
        }
    }

    #[test]
    fn ws_event_wire_format_matches_legacy_contract() {
        // 手拼 format! → serde derive 替换的逐字节线格式锁:字段名/顺序/类型与
        // 历史契约一致(改前抓帧基线 target/audit/ws_frames_before.json 同形)。
        // text 含引号/反斜杠/换行/中文,证明转义与 serde_json::to_string 时代一致;
        // rows 用 r## 定界,因期望串里含 `"#` 序列。
        let rows = vec![vec![
            ansi::Run {
                text: "a\"b\\c\n中文".into(),
                fg: Some((0x28, 0xaf, 0xe9)),
            },
            ansi::Run {
                text: "默认色".into(),
                fg: None,
            },
        ]];
        let rows_json =
            r##"[{"runs":[{"text":"a\"b\\c\n中文","fg":"#28afe9"},{"text":"默认色","fg":null}]}]"##;
        assert_eq!(
            serde_json::to_string(&WsEvent::Rows {
                seq: 17,
                dropped: 2,
                rows: &rows_view(&rows),
            })
            .unwrap(),
            format!(r##"{{"type":"rows","seq":17,"dropped":2,"rows":{rows_json}}}"##)
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::Snapshot {
                rows: &rows_view(&rows),
            })
            .unwrap(),
            format!(r##"{{"type":"snapshot","rows":{rows_json}}}"##)
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::Cleared { seq: 9 }).unwrap(),
            r#"{"type":"cleared","seq":9}"#
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::State {
                connected: false,
                status: "● 未连接 (J-Link)".into(),
            })
            .unwrap(),
            r#"{"type":"state","connected":false,"status":"● 未连接 (J-Link)"}"#
        );
        // device:info 载荷与 /api/status 的 device 同源(device_info_json 单点),
        // 断言锁外层包装与历史 format! 逐字节同形;8 字段键名契约由
        // device_info_json_keys_match_status_contract 单独锁定
        let info = serde_json::json!({
            "firmware": "J-Link V11.0", "target": "STM32F103C8", "speedKhz": 4000,
        });
        assert_eq!(
            serde_json::to_string(&WsEvent::Device { info: info.clone() }).unwrap(),
            format!(
                r#"{{"type":"device","info":{}}}"#,
                serde_json::to_string(&info).unwrap()
            )
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::Progress {
                text: "连接中 \"50%\"\r".into(),
            })
            .unwrap(),
            r#"{"type":"progress","text":"连接中 \"50%\"\r"}"#
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::Names {
                names: vec!["STM32F103C8".into(), "GD32".into()],
            })
            .unwrap(),
            r#"{"type":"names","names":["STM32F103C8","GD32"]}"#
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::JLinks {
                list: vec![JLinkEntry {
                    sn: 600788888,
                    name: "J-Link #0".into(),
                }],
            })
            .unwrap(),
            r#"{"type":"jlinks","list":[{"sn":600788888,"name":"J-Link #0"}]}"#
        );
        assert_eq!(
            serde_json::to_string(&WsEvent::Stats {
                rx: 1,
                tx: 2,
                rows: 3,
                session_sec: 4,
                cap: 500
            })
            .unwrap(),
            r#"{"type":"stats","rx":1,"tx":2,"rows":3,"sessionSec":4,"cap":500}"#
        );
    }

    #[test]
    fn scan_custom_themes_sorted_filtered_missing_dir_empty() {
        let d = std::env::temp_dir().join(format!("mini-rtt-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(scan_custom_themes(&d).is_empty()); // 目录不存在 = 空
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("zeta.css"), ":root{}").unwrap();
        std::fs::write(d.join("alpha.css"), ":root{}").unwrap();
        std::fs::write(d.join("notes.txt"), "not a theme").unwrap();
        std::fs::create_dir_all(d.join("fake.css")).unwrap(); // 目录不算
        let got = scan_custom_themes(&d);
        let ids: Vec<_> = got.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "zeta"], "字典序 + 只收 .css 文件");
        assert_eq!(got[0].name, "alpha.css");
        assert!(got[0].path.ends_with("alpha.css"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
