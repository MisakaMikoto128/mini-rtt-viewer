//! 浏览器管理台服务(ADR-11:UI 全面转 Web 形态):**一个 exe,双击启动本机
//! 服务并自动打开浏览器管理台**(SerialHub 同构——Rust 数据层 + 内嵌 Web UI),
//! 浏览器访问 `http://127.0.0.1:8686`。与桌面版同一套数据层与 worker
//! (rtt::spawn / LogPump / demo / device_db)。
//!
//! API 契约(与前端/测试对齐,改这里必同步 ui/web/index.html 与 tests/web):
//! - GET  /                管理台单页
//! - GET  /api/status      → {connected, phase, port, rxBytes, txBytes, rowsTotal,
//!   uptimeSec, sessionSec, sessionStatus, device, version};device 8 字段
//!   {firmware,hardware,serial,core,cpu,target,iface,speedKhz}(demo 模式为演示
//!   数据);sessionSec=连接会话秒(State 连接迁移记起点,断开清零,demo 启动即
//!   计);sessionStatus=当前状态文案(如「已连接 (demo)」)
//! - GET  /api/prefs       → 持久化偏好快照(面板初值恢复)
//! - GET  /api/themes      → [{id, name, custom}];内置 4 套(custom:false)+
//!   自定义项(exe 旁 themes/*.css,id=文件名去扩展,name=文件名,custom:true)
//! - GET  /api/theme-css/{id} → 自定义主题 CSS 文本(text/css);内置主题与未知
//!   id 一律 404(内置主题由前端内嵌表覆盖,不落盘)
//! - GET  /api/devices     → [芯片型号]
//! - GET  /api/jlinks      → [{sn, name}]
//! - GET  /api/history     → [text] 发送历史(最新在前,上限 50)
//! - POST /api/connect     {chip, ifaceIndex, speedIndex, channel};连接参数写入偏好
//!   快照源;demo 模式命令进 demo 线程模拟连接(demo 状态机),不加载 DLL
//! - POST /api/disconnect  demo 模式命令进 demo 线程(手动断开后不自动重连)
//! - POST /api/power       {on};demo 未连接 409,连接态日志行反馈(demo 无法真供电)
//! - POST /api/reset       {mode?: "in-place"|"reconnect"};缺省 in-place(现有
//!   Reset 命令,复位目标并重挂 RTT 续收);reconnect=先走断开逻辑,1 秒后用
//!   上次连接参数重新 spawn worker(无上次参数 409;demo 未连接 409,连接态
//!   由 demo 线程模拟日志行 + 短暂 State(false→true));空请求体容忍为缺省
//!   in-place(旧前端兼容)
//! - POST /api/settings    逐字段可选(camelCase):rxEnding / frameTimeout /
//!   encodingIndex / hexRx / autoFrame / searchRegex / sendEnding / hexSend /
//!   chip / ifaceIndex / speedIndex / channel(连接设置组持久化,F1)
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
//! sendEnding, hexSend}
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
use crate::config::{self, StoredPrefs};
use crate::demo;
use crate::device_db;
use crate::log_model::{LogPump, MAX_LOG_ROWS};
use crate::rtt::{self, WorkerCmd, WorkerConfig, WorkerHandle, WorkerMsg, SPEEDS_KHZ};
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
/// 复位模式字段值(API 契约):in-place = 现有 Reset 命令(重挂 RTT 续收)
const RESET_MODE_IN_PLACE: &str = "in-place";
/// 复位模式字段值(API 契约):reconnect = 先断开,1 秒后用上次参数重连
const RESET_MODE_RECONNECT: &str = "reconnect";
/// 内置主题 id(与前端 index.html 内嵌主题表一一对应;themes/ 里的同名 .css
/// 不进列表,避免下拉出现两个相同 value)
const BUILTIN_THEME_IDS: [&str; 4] = ["dark", "light", "oled", "sepia"];

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

/// 服务 → GUI 的事件(经 `OnEvent` 回调打回主线程,SerialHub service.rs 同构)。
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// 服务就绪(管理台已绑定该地址)——仅启动时一次,gui 侧走 ready 握手。
    Ready(SocketAddr),
    /// 连接状态变化(驱动托盘图标刷新)。demo 模式启动即发出 true。
    ConnectedChanged(bool),
}

/// 事件回调:在服务线程/数据泵线程触发,**必须非阻塞**
/// (gui 侧只做 `proxy.send_event`,绝不碰窗口/托盘)。
pub type OnEvent = std::sync::Arc<dyn Fn(ServiceEvent) + Send + Sync>;

/// 纯服务模式(`--no-window`):调用线程建 tokio runtime 阻塞跑直到进程退出。
/// bind 失败(端口被占 = 已有实例)返回 Err,由调用方决定退出码。
pub fn run(opts: WebOptions) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(opts, None))
}

/// 桌面壳装配:服务整体搬后台线程(主线程留给 tao 事件循环,见模块头注释)。
/// 返回 ready 通道:`Ok(addr)` = 服务就绪;`Err` = 启动失败(端口被占等)。
/// 就绪后的连接状态变化经 `on_event` 回调转发(gui 侧映射成 EventLoopProxy 事件)。
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
            if let Err(e) = rt.block_on(serve(opts, Some(cb))) {
                // 就绪前失败(端口被占)必须立即回传主线程;就绪后失败 ready 端
                // 已被 gui 丢弃,发送失败无妨
                let _ = ready_tx.send(Err(format!("{e:#}")));
            }
            // block_on 返回 = 服务退出,runtime 随之析构(进程退出时随进程终止)
        })
        .map_err(|e| format!("服务线程启动失败: {e}"))?;
    Ok(ready_rx)
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
    /// 连接状态变化回调(gui 壳注入,驱动托盘图标;纯服务模式 None)
    on_state: Option<OnEvent>,
    // ---- 偏好快照源(tick 内 500ms 比对落盘;未连接态也保存)----
    /// 启动时读到的完整偏好底版:桌面专用字段原样保留写回,不丢失旧值
    prefs_base: StoredPrefs,
    auto_frame: AtomicBool,
    search_regex: AtomicBool,
    chip_name: Mutex<String>,
    iface_index: Mutex<i32>,
    speed_index: Mutex<i32>,
    channel: Mutex<u32>,
    /// 发送历史(最新在前,去重,上限 50;/api/history 读,快照落盘)
    send_history: Mutex<Vec<String>>,
    /// 发送行尾 0=CRLF 1=LF 2=CR 3=无(纯 UI 习惯记忆,不改发送内容)
    send_ending: Mutex<i32>,
    /// HEX 发送模式(前端发送框按十六进制字节解析)
    hex_send: AtomicBool,
    // ---- 定时发送(复用 /api/send 发送管线)----
    timer_on: AtomicBool,
    /// 定时发送间隔(秒)
    timer_interval: Mutex<f64>,
    /// 定时发送内容:最近一次 /api/send 的文本(可经 /api/timer 显式指定)
    timer_text: Mutex<String>,
    timer_hex: AtomicBool,
    // ---- /api/status 会话扩展 + reset reconnect 模式 ----
    /// 连接会话计时起点:State 迁移到已连接时记,断开清零;demo 启动即记
    session_start: Mutex<Option<Instant>>,
    /// 当前状态文案(State 消息携带,去横幅圆点前缀;初始"未连接")
    status_text: Mutex<String>,
    /// 上次成功连接的参数(reset reconnect 的重连来源;/api/connect 成功时
    /// 刷新,启动时从偏好底版恢复)
    last_connect: Mutex<Option<SavedConnect>>,
    /// 服务监听端口(壳内前端「浏览器打开」按钮经 /api/open-browser 复用)
    port: u16,
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
        // 发送行尾(FR-12/F7):按 UI 选择追加行尾字节,文本/HEX 同规则
        let payload = append_send_ending(payload, *self.send_ending.lock().unwrap());
        // TX 计数按实际发送字节数(HEX 模式 = 解析后字节 + 行尾,非原文长度)
        let n = payload.len();
        if let Some(tx) = self.cmd_tx.lock().unwrap().clone() {
            let _ = tx.send(WorkerCmd::Send(payload));
        }
        self.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
        self.pump
            .lock()
            .unwrap()
            .push_colored_line(&format!("» {text}"), ECHO_COLOR);
        *self.timer_text.lock().unwrap() = text.to_string();
        self.timer_hex.store(hex, Ordering::Relaxed);
        history_push(&mut self.send_history.lock().unwrap(), text);
        Ok(())
    }
}

/// 发送历史入库:去重置顶(最新在前),上限 50 条(与桌面版同规则)。
/// 自由函数便于单测锁定规则;send_payload 与快照源共用。
fn history_push(history: &mut Vec<String>, text: &str) {
    const SEND_HISTORY_CAP: usize = 50;
    if let Some(i) = history.iter().position(|t| t == text) {
        history.remove(i);
    }
    history.insert(0, text.to_string());
    history.truncate(SEND_HISTORY_CAP);
}

/// 扫描到的自定义主题(SerialHub ADR-18 同构)
struct CustomTheme {
    /// 主题 id = 文件名去 .css 后缀(/api/theme-css/{id} 的 {id})
    id: String,
    /// 显示名 = 文件名(含扩展名,与 id 区分)
    name: String,
    /// CSS 文件路径(来自本机 read_dir,非用户输入拼装)
    path: PathBuf,
}

/// 自定义主题目录:exe 旁 themes/(exe 定位失败 → 相对路径 themes/)。
/// 目录不存在 = 无自定义主题(扫描静默返回空)。
fn themes_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("themes")))
        .unwrap_or_else(|| PathBuf::from("themes"))
}

/// 主题文件名白名单:ASCII 字母/数字/点/短横线/下划线、≤64 字节、.css 结尾且
/// 去后缀后 id 非空(排除分隔符、控制字符、全角/Unicode 混淆名;
/// 与 SerialHub themes.rs 同规则)
fn valid_theme_file_name(file: &str) -> bool {
    file.len() > 4
        && file.len() <= 64
        && file.ends_with(".css")
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// 扫描自定义主题:目录下每个 *.css = 一套主题,id 字典序;目录不存在/不可读
/// = 空列表。每次请求现扫(拖入 .css 即新主题,零注册,无需重启)。
fn scan_custom_themes(dir: &std::path::Path) -> Vec<CustomTheme> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<CustomTheme> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !valid_theme_file_name(&name) {
                return None;
            }
            Some(CustomTheme {
                id: name[..name.len() - 4].to_string(),
                name,
                path: e.path(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
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
    p.send_history = s.send_history.lock().unwrap().clone();
    p.send_ending = *s.send_ending.lock().unwrap();
    p.hex_send = s.hex_send.load(Ordering::Relaxed);
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

/// hex 文本 → 字节:容忍空格/冒号/连字符分隔与**每段** 0x/0X 前缀
/// ("0x41 0x42" ≡ "41 42",FR-11);空/奇数位/非法字符报错(错误信息经
/// /api/send 400 传回前端 toast 展示,F8b)
fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s
        .split(|c: char| c.is_whitespace() || c == ':' || c == '-')
        .filter(|t| !t.is_empty())
        .map(|tok| {
            tok.strip_prefix("0x")
                .or_else(|| tok.strip_prefix("0X"))
                .unwrap_or(tok)
        })
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

/// 按发送行尾选择给 payload 追加行尾字节(FR-12,文本/HEX 同规则):
/// 0=CRLF 1=LF 2=CR 3=无;TX 计数按追加后的实际字节数
fn append_send_ending(mut payload: Vec<u8>, ending: i32) -> Vec<u8> {
    match ending {
        0 => payload.extend_from_slice(b"\r\n"),
        1 => payload.extend_from_slice(b"\n"),
        2 => payload.extend_from_slice(b"\r"),
        _ => {}
    }
    payload
}

/// 文本 → 十六进制大写文本(UTF-8 字节,每字节两位、空格分隔,与真机 worker
/// 的 HEX 接收输出同格式,如 "[ de" → "5B 20 64 65 ")
fn text_to_hex_upper(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let _ = write!(out, "{b:02X} ");
    }
    out
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

/// 上次成功连接的参数(reset reconnect 模式 1 秒后用它重连;字段均已夹取)
#[derive(Debug, Clone, PartialEq, Eq)]
struct SavedConnect {
    chip: String,
    iface_index: usize,
    speed_index: usize,
    channel: u32,
}

impl SavedConnect {
    /// 偏好底版 → 上次连接参数(启动恢复;chip 为空 = 从未连接过 → None,
    /// reset reconnect 对此返回 409)
    fn from_prefs(p: &StoredPrefs) -> Option<Self> {
        let chip = p.chip_name.trim().to_string();
        if chip.is_empty() {
            return None;
        }
        Some(Self {
            iface_index: p.iface_index.clamp(0, 1) as usize,
            speed_index: p.speed_index.clamp(0, SPEEDS_KHZ.len() as i32 - 1) as usize,
            channel: p.channel.clamp(0, 15) as u32,
            chip,
        })
    }
}

/// 复位模式:in-place = 现有 Reset 命令(worker 复位目标并重挂 RTT 续收);
/// reconnect = 先走断开逻辑,1 秒后用上次连接参数重新 spawn worker
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetMode {
    InPlace,
    Reconnect,
}

/// 解析复位模式:缺省/未知值一律回落 in-place(容忍旧前端空请求体与拼写
/// 失误,复位不至于整体失效)
fn parse_reset_mode(mode: Option<&str>) -> ResetMode {
    match mode.map(str::trim) {
        Some(m) if m.eq_ignore_ascii_case(RESET_MODE_RECONNECT) => ResetMode::Reconnect,
        Some(m) if m.eq_ignore_ascii_case(RESET_MODE_IN_PLACE) => ResetMode::InPlace,
        _ => ResetMode::InPlace,
    }
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
    /// 发送行尾 0=CRLF 1=LF 2=CR 3=无(UI 习惯记忆)
    #[serde(default)]
    send_ending: Option<i32>,
    /// HEX 发送模式
    #[serde(default)]
    hex_send: Option<bool>,
    // ---- 连接设置组(F1:web→服务端保存链路;此前这五项只写 localStorage,
    // reload 被 /api/prefs 服务端值回填覆盖,改动全部丢失)----
    /// 目标设备名(存原始输入;连接时才做设备库匹配)
    #[serde(default)]
    chip: Option<String>,
    /// 接口 0=SWD 1=JTAG
    #[serde(default)]
    iface_index: Option<i32>,
    /// 速度下拉索引
    #[serde(default)]
    speed_index: Option<i32>,
    /// RTT 通道 0-15
    #[serde(default)]
    channel: Option<u32>,
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

async fn serve(opts: WebOptions, on_event: Option<OnEvent>) -> anyhow::Result<()> {
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
        // 上次连接参数从偏好底版恢复(chip 为空 = 从未连接 → reconnect 复位
        // 409);必须在 saved.chip_name 被 move 之前借用
        last_connect: Mutex::new(SavedConnect::from_prefs(&saved)),
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
    });

    if opts.demo {
        // demo 虚拟 worker 的命令通道:连接/断开/重置/电源命令由 demo 线程消费
        // (F2:此前 demo 无命令消费者,cmd_tx 发出的命令全部无人接收,
        // 按钮「连接/断开/重置目标/电源输出」在 demo 下全部无效)
        let (demo_cmd_tx, demo_cmd_rx) = mpsc::channel::<WorkerCmd>();
        *shared.cmd_tx.lock().unwrap() = Some(demo_cmd_tx);
        demo::spawn(msg_tx, demo_cmd_rx, shared.frame_timeout_ms.clone());
        shared.connected.store(true, Ordering::Relaxed);
        // demo 虚拟会话启动即"已连接":会话计时起点 + 状态文案 + 初始连接
        // 标记行(与 State 迁移标记同款式;demo 的 State(true) 5 秒后才到,
        // 期间 status/会话口径保持一致)
        *shared.session_start.lock().unwrap() = Some(Instant::now());
        *shared.status_text.lock().unwrap() = "已连接 (demo)".into();
        shared
            .pump
            .lock()
            .unwrap()
            .push_colored_line(&state_marker_line(true), MARK_COLOR);
        // gui 壳在途:托盘图标立即反映"已连接"(事件在事件循环启动前排队的语义,
        // 见 gui.rs 模块头)
        if let Some(cb) = &shared.on_state {
            cb(ServiceEvent::ConnectedChanged(true));
        }
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

/// 用系统默认浏览器打开管理台(Windows;cmd start 的首个引号参数是窗口标题,
/// 补空串占位)。失败静默:用户可手动访问地址栏。
/// gui 壳的托盘菜单「在浏览器打开」与页内 /api/open-browser 都走这里。
pub(crate) fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
}

/// 在系统默认浏览器打开管理台(壳内前端按钮用;SerialHub Sprint7 实测壳内
/// window.open 被原生层吞掉,浏览器访问用不到此端点)。URL 由服务端监听端口
/// 拼出、不接受客户端传入,无任意 URL 打开面。
async fn api_open_browser(State(shared): State<Arc<Shared>>) -> StatusCode {
    open_in_browser(&format!("http://127.0.0.1:{}", shared.port));
    StatusCode::OK
}

/// WS 事件统一序列化(替换历史手拼 `format!(r#"{{"type":…}}"#)`,serde derive
/// 消掉拼串笔误面):`tag = "type"` + 变体名 camelCase(单词名即小写)产出
/// `{"type":"rows|snapshot|cleared|state|device|progress|names|jlinks|stats",…}`。
/// 字段名/顺序/类型与历史契约逐项对齐(前端 index.html 与 tests/web 黑盒只认
/// 这套);线格式由单测 `ws_event_wire_format_matches_legacy_contract` 逐字节
/// 锁死——camelCase 契约坑(见 SettingsReq 注释)自此有编译期 + 测试双保险。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WsEvent<'a> {
    /// 增量行 {"type":"rows","seq":N,"dropped":N,"rows":[{runs:[{text,fg}]}]}
    Rows {
        seq: u64,
        dropped: usize,
        rows: &'a [RunsRow<'a>],
    },
    /// 重同步全量快照 {"type":"snapshot","rows":[…]}
    Snapshot { rows: &'a [RunsRow<'a>] },
    /// 清空水位 {"type":"cleared","seq":N}(水位之后的 rows 才有效)
    Cleared { seq: u64 },
    /// 连接状态 {"type":"state","connected":B,"status":"…"}
    State { connected: bool, status: String },
    /// 设备信息 {"type":"device","info":{8 字段}}——info 与 /api/status 的 device
    /// 同形,继续由 serde_json::json! 构造(与 demo_device_json 单点同源)
    Device { info: serde_json::Value },
    /// 进度文案 {"type":"progress","text":"…"}
    Progress { text: String },
    /// 芯片型号名单 {"type":"names","names":[…]}
    Names { names: Vec<String> },
    /// J-Link 列表 {"type":"jlinks","list":[{sn,name}]}——camelCase 会把变体名
    /// 转成 "jLinks",与历史契约不符 → 显式 rename
    #[serde(rename = "jlinks")]
    JLinks { list: Vec<JLinkEntry> },
    /// 统计 {"type":"stats","rx":N,"tx":N,"rows":N,"sessionSec":N,"cap":N}——
    /// sessionSec=当前连接会话秒(断开为 0);cap=服务端行数上限(MAX_LOG_ROWS,
    /// 底栏「n / 500 行」口径单点:前端不硬编码,以本字段为准)。
    /// 注意:容器 rename_all="camelCase" 只改写**变体名**,不变体字段——首个
    /// 多词字段必须显式 rename(线格式测试已锁),别再假设字段名自动转驼峰
    Stats {
        rx: u64,
        tx: u64,
        rows: usize,
        #[serde(rename = "sessionSec")]
        session_sec: u64,
        cap: usize,
    },
}

/// WS jlinks 事件的列表项 {sn, name}(与 /api/jlinks 输出同形)
#[derive(Serialize)]
struct JLinkEntry {
    sn: u32,
    name: String,
}

/// 单行 runs 包装:历史契约每行是 `{"runs":[{text,fg},…]}` **对象**而非裸数组
/// (Vec<Run> 直接序列化会丢掉 runs 键,这是替换时唯一需要显式建模的层级)
#[derive(Serialize)]
struct RunsRow<'a> {
    runs: &'a [ansi::Run],
}

/// 行数组 → runs 包装视图(借用,零文本拷贝;rows/snapshot 事件共用)
fn rows_view(rows: &[Vec<ansi::Run>]) -> Vec<RunsRow<'_>> {
    rows.iter().map(|r| RunsRow { runs: r }).collect()
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
                        // RX 统计按原始块长计(hex 转换只是 demo 的显示变换)
                        let raw_len = text.len();
                        // demo 流是文本:hexRx 开启时把块文本按 UTF-8 字节转
                        // 十六进制大写文本再上屏,演示 HEX 接收效果。仅 demo
                        // 生效——真机路径已在 worker 内转换,这里再转就是二次
                        // 转换(真机 hex 文本会被再 hex 一次)
                        let shown = if shared.demo_mode && shared.hex_rx.load(Ordering::Relaxed) {
                            text_to_hex_upper(&text)
                        } else {
                            text
                        };
                        shared.rx_bytes.fetch_add(raw_len as u64, Ordering::Relaxed);
                        pump.absorb_text(&shown, rx_ending);
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
                    // 进度文案同步进 status_text(F3):2s 轮询的 sessionStatus
                    // 与 WS progress 同源,轮询回填不会倒退/打架
                    *shared.status_text.lock().unwrap() = clean_status_text(&text);
                    let _ = shared
                        .events_tx
                        .send(serde_json::to_string(&WsEvent::Progress { text }).unwrap());
                }
                Ok(WorkerMsg::State(connected, status)) => {
                    // 翻变才回调(gui 壳驱动托盘图标;数据泵线程非阻塞,只 send_event)
                    let prev = shared.connected.swap(connected, Ordering::Relaxed);
                    if prev != connected {
                        // 自动连接/断开标记行(与手动标记同款式;直接用已持有的
                        // pump,严禁再借——见 AGENTS「tick 持 RefCell borrow」条)
                        pump.push_colored_line(&state_marker_line(connected), MARK_COLOR);
                        // 会话计时:连接记起点,断开清零
                        *shared.session_start.lock().unwrap() = if connected {
                            Some(Instant::now())
                        } else {
                            None
                        };
                        if let Some(cb) = &shared.on_state {
                            cb(ServiceEvent::ConnectedChanged(connected));
                        }
                    }
                    if !connected {
                        // 断开统一口径(F2):会话统计全清 + 设备信息清空,
                        // 消除「sessionSec 归零而 rxBytes 残留」的口径矛盾
                        // (demo 周期断开与真机断开走同一分支)
                        shared.rx_bytes.store(0, Ordering::Relaxed);
                        shared.tx_bytes.store(0, Ordering::Relaxed);
                        *shared.session_start.lock().unwrap() = None;
                        *shared.device_info.lock().unwrap() = None;
                    }
                    // 状态文案每条 State 都刷新(连接失败的错误详情也随 false 态带出)
                    *shared.status_text.lock().unwrap() = clean_status_text(&status);
                    let _ = shared.events_tx.send(
                        serde_json::to_string(&WsEvent::State { connected, status }).unwrap(),
                    );
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
                        .send(serde_json::to_string(&WsEvent::Device { info: json }).unwrap());
                }
                Ok(WorkerMsg::DeviceNames(names)) => {
                    *shared.device_names.lock().unwrap() = names.clone();
                    let _ = shared
                        .events_tx
                        .send(serde_json::to_string(&WsEvent::Names { names }).unwrap());
                }
                Ok(WorkerMsg::JLinks(list)) => {
                    *shared.jlinks.lock().unwrap() = list.clone();
                    let entries = list
                        .into_iter()
                        .map(|(sn, name)| JLinkEntry { sn, name })
                        .collect();
                    let _ = shared
                        .events_tx
                        .send(serde_json::to_string(&WsEvent::JLinks { list: entries }).unwrap());
                }
                Ok(WorkerMsg::Exited) => {
                    // 只清"确实已停"的登记句柄:reset reconnect 场景下旧 worker
                    // 的 Exited 可能晚于新 worker 登记到达,不能误清新句柄
                    // (alive=false 由 worker 线程在发出 Exited 前置位)
                    let mine = {
                        let mut w = shared.worker.lock().unwrap();
                        let mine = match w.as_ref() {
                            // 句柄已被 reset reconnect 提前取走:按幂等清理处理
                            None => true,
                            Some(h) => !h.alive.load(Ordering::Relaxed),
                        };
                        if mine {
                            *w = None;
                        }
                        mine
                    };
                    if mine {
                        *shared.cmd_tx.lock().unwrap() = None;
                        let prev = shared.connected.swap(false, Ordering::Relaxed);
                        if prev {
                            if let Some(cb) = &shared.on_state {
                                cb(ServiceEvent::ConnectedChanged(false));
                            }
                        }
                        // worker 线程已退:会话计时清零(State(false) 通常已先行
                        // 处理,这里兜底)
                        *shared.session_start.lock().unwrap() = None;
                    }
                }
                Err(_) => break,
            }
        }
        pump.enforce_line_cap();
        let dropped = pump.take_dropped();
        if let Some(rows) = pump.take_new_rows() {
            let seq = shared.seq.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = shared.events_tx.send(
                serde_json::to_string(&WsEvent::Rows {
                    seq,
                    dropped,
                    rows: &rows_view(&rows),
                })
                .unwrap(),
            );
        }
        drop(pump);
        if last_stats.elapsed() >= Duration::from_millis(500) {
            last_stats = Instant::now();
            let session_sec = shared
                .session_start
                .lock()
                .unwrap()
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            let _ = shared.events_tx.send(
                serde_json::to_string(&WsEvent::Stats {
                    rx: shared.rx_bytes.load(Ordering::Relaxed),
                    tx: shared.tx_bytes.load(Ordering::Relaxed),
                    rows: shared.pump.lock().unwrap().rows_len(),
                    session_sec,
                    cap: MAX_LOG_ROWS,
                })
                .unwrap(),
            );
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

/// demo 模式的演示设备信息(/api/status 的 device 字段;8 字段与真机同形,
/// WS device 事件同字段名)
fn demo_device_json() -> serde_json::Value {
    serde_json::json!({
        "firmware": "J-Link V11 demo", "hardware": "V11",
        "serial": "600788888", "core": "Cortex-M4",
        "cpu": "ARM 32-Bit", "target": "STM32F103C8",
        "iface": "SWD", "speedKhz": 4000,
    })
}

async fn api_status(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let connected = shared.connected.load(Ordering::Relaxed);
    // device 8 字段(与 WS device 事件同形):demo 为演示数据且**仅连接态提供**
    // (断开清空,与统计清零同口径;F2 前断开态仍回演示数据,前端设备信息
    // 断开后看起来"仍连接"),真机取最近一次 DeviceInfo(State(false) 已清空,
    // 断开后为 None)
    let device = if shared.demo_mode && connected {
        Some(demo_device_json())
    } else {
        shared.device_info.lock().unwrap().as_ref().map(|d| {
            serde_json::json!({
                "firmware": d.firmware, "hardware": d.hardware,
                "serial": d.serial, "core": d.core_name,
                "cpu": d.core_cpu, "target": d.target,
                "iface": d.iface, "speedKhz": d.speed_khz,
            })
        })
    };
    let session_sec = shared
        .session_start
        .lock()
        .unwrap()
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    Json(serde_json::json!({
        "connected": connected,
        "phase": if connected { "connected" } else { "idle" },
        "port": if connected { "jlink" } else { "demo" },
        "rxBytes": shared.rx_bytes.load(Ordering::Relaxed),
        "txBytes": shared.tx_bytes.load(Ordering::Relaxed),
        "rowsTotal": shared.pump.lock().unwrap().rows_len(),
        "uptimeSec": shared.started.elapsed().as_secs(),
        "sessionSec": session_sec,
        "sessionStatus": shared.status_text.lock().unwrap().clone(),
        "device": device,
        "version": env!("CARGO_PKG_VERSION"),
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
        "sendEnding": *shared.send_ending.lock().unwrap(),
        "hexSend": shared.hex_send.load(Ordering::Relaxed),
    }))
}

/// 内置主题(前端 index.html 内嵌同款主题表,前端按 id 覆盖,不落盘)+
/// 自定义主题(exe 旁 themes/*.css,每次现扫:拖入即生效,零注册)
async fn api_themes() -> Json<serde_json::Value> {
    let mut items = serde_json::json!([
        {"id": "dark", "name": "深色", "custom": false},
        {"id": "light", "name": "浅色", "custom": false},
        {"id": "oled", "name": "OLED 纯黑", "custom": false},
        {"id": "sepia", "name": "护眼暖色", "custom": false},
    ]);
    let arr = items.as_array_mut().unwrap();
    for t in scan_custom_themes(&themes_dir()) {
        // 与内置同名的 .css 不进列表(前端内置表覆盖,列出来只会出现两个相同 value)
        if BUILTIN_THEME_IDS.contains(&t.id.as_str()) {
            continue;
        }
        arr.push(serde_json::json!({"id": t.id, "name": t.name, "custom": true}));
    }
    Json(items)
}

/// 自定义主题 CSS 文本(text/css);内置主题与未知 id 一律 404(前端内置表
/// 覆盖)。路径安全:只在「本机扫描列表」里按 id 精确匹配,拿到的路径来自
/// read_dir 而非用户输入,天然无目录穿越面。
async fn api_theme_css(Path(id): Path<String>) -> Response {
    let hit = scan_custom_themes(&themes_dir())
        .into_iter()
        .find(|t| t.id == id);
    match hit {
        Some(t) => match std::fs::read(&t.path) {
            Ok(bytes) => {
                ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], bytes).into_response()
            }
            // 扫描后文件被删等竞态:按不存在处理
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 发送历史(最新在前,上限 50;前端 ↑↓ 浏览的数据源)
async fn api_history(State(shared): State<Arc<Shared>>) -> Json<Vec<String>> {
    Json(shared.send_history.lock().unwrap().clone())
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

/// spawn 真 worker 并登记句柄/命令通道(api_connect 与 reset reconnect 共用;
/// selected_sn 取 J-Link 列表首台,与原逻辑一致)
fn spawn_worker(shared: &Shared, params: &SavedConnect) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
    let handle = rtt::spawn(
        WorkerConfig {
            chip: params.chip.clone(),
            iface_index: params.iface_index,
            speed_khz: SPEEDS_KHZ[params.speed_index.min(SPEEDS_KHZ.len() - 1)],
            channel: params.channel.min(15),
            frame_timeout_ms: shared.frame_timeout_ms.clone(),
            selected_sn: shared.jlinks.lock().unwrap().first().map(|(sn, _)| *sn),
            encoding_index: shared.encoding_index.clone(),
            hex_rx: shared.hex_rx.clone(),
        },
        shared.msg_tx.clone(),
        cmd_rx,
    );
    *shared.worker.lock().unwrap() = Some(handle);
    *shared.cmd_tx.lock().unwrap() = Some(cmd_tx);
}

/// 连接:芯片名补全(库内子串匹配首个全称,与桌面版同规则)→ 连接参数写入
/// 偏好快照源 → spawn worker(demo 模式命令进 demo 线程模拟连接,不加载 DLL)
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

    // 连接参数夹取一次,偏好快照源 / 上次连接参数 / worker 配置三者同源
    let iface_index = req.iface_index.min(1);
    let speed_index = req.speed_index.min(SPEEDS_KHZ.len() - 1);
    let channel = req.channel.min(15);

    // 连接参数进偏好快照源(tick 内 500ms 落盘;重启后面板初值恢复)
    *shared.chip_name.lock().unwrap() = chip.clone();
    *shared.iface_index.lock().unwrap() = iface_index as i32;
    *shared.speed_index.lock().unwrap() = speed_index as i32;
    *shared.channel.lock().unwrap() = channel;
    // 上次连接参数:reset reconnect 模式的重连来源(demo 也记录,语义一致)
    let params = SavedConnect {
        chip: chip.clone(),
        iface_index,
        speed_index,
        channel,
    };
    *shared.last_connect.lock().unwrap() = Some(params.clone());

    if shared.demo_mode {
        // demo 虚拟 worker:命令进 demo 线程,立即 State(true)(F2 前此处
        // 只记录参数,「连接」按钮在 demo 下完全无效)
        if let Some(tx) = shared.cmd_tx.lock().unwrap().as_ref() {
            let _ = tx.send(WorkerCmd::DemoConnect);
        }
        return StatusCode::OK.into_response();
    }

    spawn_worker(&shared, &params);
    StatusCode::OK.into_response()
}

/// 断开:置停止标志,worker 的 Exited 消息回到未连接态(与桌面版同协议);
/// demo 模式命令进 demo 线程(手动断开后不自动重连,由 demo 状态机保证)
async fn api_disconnect(State(shared): State<Arc<Shared>>) -> StatusCode {
    if shared.demo_mode {
        if let Some(tx) = shared.cmd_tx.lock().unwrap().as_ref() {
            let _ = tx.send(WorkerCmd::DemoDisconnect);
        }
        return StatusCode::OK;
    }
    if let Some(h) = shared.worker.lock().unwrap().as_ref() {
        h.stop.store(true, Ordering::Relaxed);
    }
    if let Some(tx) = shared.cmd_tx.lock().unwrap().as_ref() {
        let _ = tx.send(WorkerCmd::Send(Vec::new())); // 唤醒阻塞中的 worker 尽快退出
    }
    StatusCode::OK
}

async fn api_power(State(shared): State<Arc<Shared>>, Json(req): Json<PowerReq>) -> Response {
    // demo 与真机同语义:未连接拒绝(前端此时也禁用勾选框)
    if shared.demo_mode && !shared.connected.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "未连接").into_response();
    }
    let Some(tx) = shared.cmd_tx.lock().unwrap().clone() else {
        return (StatusCode::CONFLICT, "未连接").into_response();
    };
    let _ = tx.send(WorkerCmd::Power(req.on));
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetReq {
    /// 复位模式:"in-place"(缺省)| "reconnect";缺省/未知值一律 in-place
    #[serde(default)]
    mode: Option<String>,
}

/// 断开 worker 供重连(reset reconnect 专用):置停止标志 + 空发送唤醒阻塞中
/// 的读循环,引用立即清空;旧句柄返回给重连线程,等线程真正退出后才重连。
/// 与 /api/disconnect 有意不同:那边保留句柄到 Exited 到达,维持"worker 存活
/// 期间拒绝新连接"的门闩;这边等待职责由重连线程接管。
fn take_worker_down(shared: &Shared) -> Option<Arc<WorkerHandle>> {
    let old = shared.worker.lock().unwrap().take();
    if let Some(h) = &old {
        h.stop.store(true, Ordering::Relaxed);
    }
    if let Some(tx) = shared.cmd_tx.lock().unwrap().take() {
        let _ = tx.send(WorkerCmd::Send(Vec::new())); // 唤醒阻塞中的 worker 尽快退出
    }
    old
}

/// 等旧 worker 线程退出(alive=false 由线程在发出 Exited 前置位);超时返回
/// false——此时严禁 spawn 新 worker,见 rtt.rs「worker 生命周期铁律」
fn wait_worker_exit(h: Option<&Arc<WorkerHandle>>, timeout: Duration) -> bool {
    let Some(h) = h else {
        return true;
    };
    let deadline = Instant::now() + timeout;
    while h.alive.load(Ordering::Relaxed) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// 复位两模式:in-place = 现有 Reset 命令(worker 复位目标并重挂 RTT 续收);
/// reconnect = 先走断开逻辑,1 秒后用上次连接参数重新 spawn worker(重启服务
/// 且无上次参数 → 409)。demo 无真实目标:命令进 demo 线程模拟(日志行 +
/// 短暂 State(false→true)),未连接态与真机同语义 409。
async fn api_reset(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    // 兼容旧前端:POST 无请求体 = 缺省 in-place;非空则必须是合法 JSON
    let req: ResetReq = if body.is_empty() {
        ResetReq { mode: None }
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("请求体 JSON 解析失败:{e}"))
                    .into_response()
            }
        }
    };
    if shared.demo_mode {
        if !shared.connected.load(Ordering::Relaxed) {
            return (StatusCode::CONFLICT, "未连接").into_response();
        }
        if let Some(tx) = shared.cmd_tx.lock().unwrap().as_ref() {
            let _ = tx.send(WorkerCmd::Reset);
        }
        return StatusCode::OK.into_response();
    }
    match parse_reset_mode(req.mode.as_deref()) {
        ResetMode::InPlace => {
            let Some(tx) = shared.cmd_tx.lock().unwrap().clone() else {
                return (StatusCode::CONFLICT, "未连接").into_response();
            };
            let _ = tx.send(WorkerCmd::Reset);
            StatusCode::OK.into_response()
        }
        ResetMode::Reconnect => {
            let Some(params) = shared.last_connect.lock().unwrap().clone() else {
                return (StatusCode::CONFLICT, "无上次连接参数,无法重连").into_response();
            };
            // 断开 → 1 秒后用上次参数重连(独立线程,HTTP 立即返回 200)
            let old = take_worker_down(&shared);
            let shared2 = Arc::clone(&shared);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(1));
                // 铁律:旧 worker 线程存活期间严禁 spawn 新 worker(双 worker
                // 抢 J-Link 是数据损坏/状态错乱的根源)——先等旧线程真正退出
                if !wait_worker_exit(old.as_ref(), Duration::from_secs(3)) {
                    let _ = shared2.msg_tx.send(WorkerMsg::Log(
                        "[自动重连] 上一连接未退出,已放弃本次重连\r\n".into(),
                    ));
                    return;
                }
                // 等待期内用户已手动连接则让位;进程退出中不再拉起 worker
                if shared2.worker.lock().unwrap().is_some()
                    || rtt::APP_SHUTDOWN.load(Ordering::Relaxed)
                {
                    return;
                }
                spawn_worker(&shared2, &params);
            });
            StatusCode::OK.into_response()
        }
    }
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
    if let Some(v) = req.send_ending {
        *shared.send_ending.lock().unwrap() = v.clamp(0, 3);
    }
    if let Some(v) = req.hex_send {
        shared.hex_send.store(v, Ordering::Relaxed);
    }
    // 连接设置组(F1):进偏好快照源,与其它设置同一条「tick 500ms 快照比对
    // 落盘 → 重启 /api/prefs 回填」链路;夹取口径与 /api/connect 一致
    if let Some(v) = req.chip {
        *shared.chip_name.lock().unwrap() = v.trim().to_string();
    }
    if let Some(v) = req.iface_index {
        *shared.iface_index.lock().unwrap() = v.clamp(0, 1);
    }
    if let Some(v) = req.speed_index {
        *shared.speed_index.lock().unwrap() = v.clamp(0, SPEEDS_KHZ.len() as i32 - 1);
    }
    if let Some(v) = req.channel {
        *shared.channel.lock().unwrap() = v.clamp(0, 15);
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

/// 会话标记行(手动 /api/mark 与自动连接/断开标记共用款式):
/// 「── [HH:MM:SS] {label} ──」
fn marker_line(label: &str, stamp: &str) -> String {
    format!("── [{stamp}] {label} ──")
}

/// State 迁移的自动标记行:已连接 / 已断开
fn state_marker_line(connected: bool) -> String {
    marker_line(if connected { "已连接" } else { "已断开" }, &hms_stamp())
}

/// worker 状态文案 → /api/status 展示文案:去横幅圆点前缀
/// ("● 已连接 (demo)" → "已连接 (demo)")
fn clean_status_text(s: &str) -> String {
    s.trim().trim_start_matches('●').trim().to_string()
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
    pump.push_colored_line(&marker_line(&label, &hms_stamp()), MARK_COLOR);
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
        .send(serde_json::to_string(&WsEvent::Cleared { seq }).unwrap());
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
        let msg = serde_json::to_string(&WsEvent::Snapshot {
            rows: &rows_view(&snapshot),
        })
        .unwrap();
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
                        let msg = serde_json::to_string(&WsEvent::Snapshot {
                            rows: &rows_view(&snapshot),
                        })
                        .unwrap();
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
    fn text_to_hex_upper_matches_worker_hex_format() {
        assert_eq!(text_to_hex_upper(""), "");
        assert_eq!(text_to_hex_upper("AB"), "41 42 ");
        // 契约示例:"5B 20 64 65…" = "[ de" 的 UTF-8 字节(4 字节)
        assert_eq!(text_to_hex_upper("[ de"), "5B 20 64 65 ");
        // 多字节 UTF-8 按字节展开:"你" = E4 BD A0
        assert_eq!(text_to_hex_upper("你"), "E4 BD A0 ");
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
        assert_eq!(clean_status_text("● 已连接 (demo)"), "已连接 (demo)");
        assert_eq!(clean_status_text("● 未连接"), "未连接");
        assert_eq!(clean_status_text("plain"), "plain");
    }

    #[test]
    fn saved_connect_from_prefs_clamps_and_requires_chip() {
        let mut p = StoredPrefs::default();
        assert_eq!(SavedConnect::from_prefs(&p), None); // 从未连接过
        p.chip_name = "  STM32F103C8 ".into();
        p.iface_index = 5; // 越界 → 夹到 1
        p.speed_index = 99; // 越界 → 夹到 7(SPEEDS_KHZ 上限)
        p.channel = 99; // 越界 → 夹到 15
        assert_eq!(
            SavedConnect::from_prefs(&p),
            Some(SavedConnect {
                chip: "STM32F103C8".into(),
                iface_index: 1,
                speed_index: 7,
                channel: 15,
            })
        );
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
                status: "● 未连接 (demo)".into(),
            })
            .unwrap(),
            r#"{"type":"state","connected":false,"status":"● 未连接 (demo)"}"#
        );
        // device:info 载荷仍由原 json! 构造(与 /api/status 单点同源,未动),
        // 断言只锁外层包装与历史 format! 逐字节同形
        let info = serde_json::json!({
            "firmware": "J-Link V11 demo", "target": "STM32F103C8", "speedKhz": 4000,
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
