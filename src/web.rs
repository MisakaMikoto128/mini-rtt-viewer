//! 浏览器管理台服务(ADR-11:UI 全面转 Web 形态):**一个 exe,双击启动桌面壳
//! (内嵌同一管理台的 WebView)并在本机起服务**;也可 `--no-window` 纯服务后用
//! 浏览器访问 `http://127.0.0.1:8686`(SerialHub 同构——Rust 数据层 + 内嵌
//! Web UI)。两种形态共用同一套数据层与 worker
//! (rtt::spawn / LogPump / demo / device_db)。
//!
//! API 契约(与前端/测试对齐,改这里必同步 ui/web/index.html 与 tests/web):
//! - GET  /                管理台单页
//! - GET  /api/status      → {connected, rxBytes, txBytes, rowsTotal,
//!   uptimeSec, sessionSec, sessionStatus, device, version, backend};device 8 字段
//!   {firmware,hardware,serial,core,cpu,target,iface,speedKhz}(demo 模式为演示
//!   数据);sessionSec=连接会话秒(State 连接迁移记起点,断开清零,demo 启动即
//!   计);sessionStatus=当前状态文案(如「已连接 (demo)」);backend=当前数据
//!   采集后端 id("jlink"/"demo",阶段二新增)
//! - GET  /api/backends    → {selected, items:[{id,name,description,isVirtual}]}
//!   (阶段二后端可选面:selected=当前选定;items=本进程可用后端列表——普通
//!   形态 jlink+demo,--demo-log 强制形态仅 demo)
//! - GET  /api/prefs       → 持久化偏好快照(面板初值恢复)
//! - GET  /api/themes      → [{id, name, custom}];内置 4 套(custom:false)+
//!   自定义项(exe 旁 themes/*.css,id=文件名去扩展,name=文件名,custom:true)
//! - GET  /api/theme-css/{id} → 自定义主题 CSS 文本(text/css);内置主题与未知
//!   id 一律 404(内置主题由前端内嵌表覆盖,不落盘)
//! - GET  /api/devices     → [芯片型号]
//! - GET  /api/jlinks      → [{sn, name}]
//! - GET  /api/history     → [text] 发送历史(最新在前,上限 50)
//! - POST /api/connect     {chip, ifaceIndex, speedIndex, channel, jlinkSn?};
//!   jlinkSn = 「J-Link」下拉选中的序列号(缺省/无效回落列表首台);连接参数
//!   写入偏好快照源;demo 模式命令进 demo 线程模拟连接(demo 状态机),不加载 DLL
//! - POST /api/disconnect  demo 模式命令进 demo 线程(手动断开后不自动重连)
//! - POST /api/power       {on};demo 未连接 409,连接态日志行反馈(demo 无法真供电)
//! - POST /api/reset       {mode?: "in-place"|"reconnect"};缺省 in-place(现有
//!   Reset 命令,复位目标并重挂 RTT 续收);reconnect=先走断开逻辑,1 秒后用
//!   上次连接参数重新 spawn worker(无上次参数 409;demo 未连接 409,连接态
//!   由 demo 线程模拟日志行 + 短暂 State(false→true));空请求体容忍为缺省
//!   in-place(旧前端兼容)
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
    /// --demo-log:内置演示数据源,无设备即可体验/测试。阶段二语义升级为
    /// 「强制 demo 后端」:后端列表仅 demo + 启动自动连接(向后兼容)
    pub demo: bool,
    /// --backend <id>(阶段二):启动时选定数据采集后端;必须命中可用列表
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

impl WebOptions {
    /// --demo-log 强制形态(demo 后端独占 + 启动自动连接)
    pub fn is_demo_forced(&self) -> bool {
        self.demo
    }
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
    // ---- 会话槽(后端抽象阶段一:worker/cmd_tx 双槽合并;内容由当前后端
    // 维护——jlink = 真 worker 句柄+命令通道,demo = 恒活单例句柄)----
    session: Mutex<Option<SessionHandle>>,
    msg_tx: mpsc::Sender<WorkerMsg>,
    /// reset reconnect 的 1 秒等待窗口门闩(审计 P1):重连线程已把 session
    /// 清空,此期间 api_connect 的存活检查会放行——用独立标志堵住
    /// 「复位重连等待期内手动点连接 → 双 worker 并发抢 J-Link」的窗口
    reconnecting: AtomicBool,
    frame_timeout_ms: Arc<AtomicU32>,
    encoding_index: Arc<AtomicU32>,
    hex_rx: Arc<AtomicBool>,
    device_names: Mutex<Vec<String>>,
    jlinks: Mutex<Vec<(u32, String)>>,
    /// 「J-Link」下拉选中的调试器序列号(连接时校验后在列表内才生效;
    /// None = 自动选首台。经快照落盘,prefs.jlink_serial 恢复)
    jlink_serial: Mutex<Option<u32>>,
    device_info: Mutex<Option<DeviceInfo>>,
    /// 当前数据采集后端(阶段二起可经 /api/settings 切换)
    backend: Mutex<&'static dyn RttBackend>,
    /// 本进程可用后端列表(/api/backends items 与 settings 校验的单一来源;
    /// 装配期定死:强制形态仅 demo,普通形态 jlink+demo)
    backends_available: Vec<&'static dyn RttBackend>,
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
    /// 发送行尾 0=CRLF 1=LF 2=CR 3=无(发送时按选择追加行尾字节,文本/HEX 同规则)
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
    last_connect: Mutex<Option<ConnectParams>>,
    /// 服务监听端口(壳内前端「浏览器打开」按钮经 /api/open-browser 复用)
    port: u16,
    /// 服务监听地址(FR-28 /api/addr 据此判断局域网地址是否对外可达)
    listen: std::net::IpAddr,
}

impl Shared {
    /// 当前数据采集后端(装配期写入;阶段二起可经 /api/settings 切换)
    fn backend(&self) -> &'static dyn RttBackend {
        *self.backend.lock().unwrap()
    }

    /// 发送管线(手动 /api/send 与定时发送共用):HEX/文本 → worker;回显 +
    /// TX 计数 + 记录为定时内容。demo 后端线程忽略 Send,只回显与计数
    /// (前端连接态才允许发送,语义为虚拟发送)
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
        if let Some(tx) = self.session.lock().unwrap().as_ref().map(|s| s.cmd_tx.clone()) {
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

/// 后端实现访问 Shared 的边界(Host trait):方法体只做短借其它 Mutex 字段、
/// 原子读写与通道克隆,**禁止碰 pump 锁**(tick_loop 持泵锁横跨消息内循环,
/// 见 AGENTS「并发/锁纪律」)。
impl Host for Shared {
    fn send_msg(&self, msg: WorkerMsg) {
        let _ = self.msg_tx.send(msg);
    }

    fn msg_tx(&self) -> mpsc::Sender<WorkerMsg> {
        self.msg_tx.clone()
    }

    fn session(&self) -> Option<SessionHandle> {
        self.session.lock().unwrap().clone()
    }

    fn set_session(&self, s: Option<SessionHandle>) {
        *self.session.lock().unwrap() = s;
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn jlinks(&self) -> Vec<(u32, String)> {
        self.jlinks.lock().unwrap().clone()
    }

    fn set_device_names(&self, names: Vec<String>) {
        *self.device_names.lock().unwrap() = names;
    }

    fn worker_shared(&self) -> backend::WorkerShared {
        backend::WorkerShared {
            frame_timeout_ms: self.frame_timeout_ms.clone(),
            encoding_index: self.encoding_index.clone(),
            hex_rx: self.hex_rx.clone(),
        }
    }

    fn reconnecting(&self) -> bool {
        self.reconnecting.load(Ordering::Relaxed)
    }

    fn set_reconnecting(&self, v: bool) {
        self.reconnecting.store(v, Ordering::Relaxed);
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

/// 从 Shared 当前状态构建偏好快照(快照比对落盘的单一来源)
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
    p.jlink_serial = *s.jlink_serial.lock().unwrap();
    p.send_ending = *s.send_ending.lock().unwrap();
    p.hex_send = s.hex_send.load(Ordering::Relaxed);
    // 后端 id(阶段二):随快照落盘,重启恢复上次选定(serve 内已归一化)
    p.backend = s.backend().id().to_string();
    p
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendReq {
    text: String,
    /// HEX 发送模式:输入按十六进制字节解析(规则见下方 `parse_hex_bytes`)
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
    /// 「J-Link」下拉选中的序列号(前端 1273 行起接线;缺省/不在当前列表内
    /// 一律回落列表首台,与旧行为兼容)
    #[serde(default)]
    jlink_sn: Option<u32>,
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
    /// 数据采集后端 id(阶段二;None = 不切换。校验与连接态门禁在
    /// api_settings:未知/不可用 id → 400,连接态 → 409)
    #[serde(default)]
    backend: Option<String>,
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

/// 阶段二后端选定解析(serve 装配用,纯函数便于单测):CLI --backend(必须
/// 命中可用列表才生效,非法忽略)→ 偏好 backend(serve 内归一化:不在可用
/// 列表回落列表首个,与 chip 等字段同款容错;config 的 load 不感知可用面)→
/// 列表首个。普通形态列表首 = jlink(历史缺省行为),强制形态仅 demo。
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
    // 可用后端列表(阶段二):--demo-log 强制形态仅 demo(不加载 DLL,保留
    // 「demo 可与真实实例并存」语义);普通形态 jlink + demo(界面选后端的
    // 核心价值:无硬件体验全功能)
    let backends_available: Vec<&'static dyn RttBackend> = if opts.is_demo_forced() {
        vec![backend::backend_by_id(backend::BACKEND_DEMO).expect("demo 后端未注册")]
    } else {
        vec![
            backend::backend_by_id(backend::BACKEND_JLINK).expect("jlink 后端未注册"),
            backend::backend_by_id(backend::BACKEND_DEMO).expect("demo 后端未注册"),
        ]
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

    // 后端装配(阶段二,三入口统一,jlink/demo 各取所需,缺失能力为 trait
    // default 空实现):① jlink 在可用列表时启动设备库后台枚举(普通形态恒
    // 启动;强制形态 jlink 不可用,不加载 DLL);② 选定后端 on_selected 种子
    // 设备候选(demo = 静态演示表;jlink = 清空候选走异步枚举回填,枚举已被
    // ① 启动,幂等);③ 强制 demo 形态启动自动连接(demo:懒启动单例,线程
    // 启动即连接,首条 State(true) 消息驱动 tick 完成连接态/标记行/托盘回调
    // ——web 层不再预置,tick 消化先于 axum bind,首个 HTTP 可见时 connected=true)
    if let Some(jlink) = backends_available
        .iter()
        .copied()
        .find(|b| b.id() == backend::BACKEND_JLINK)
    {
        jlink.start_background_enumeration(&*shared);
    }
    backend.on_selected(&*shared);
    if opts.is_demo_forced() {
        backend.auto_connect_at_startup(&*shared);
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

/// 本机出网网卡 IPv4:UDP connect 探测默认路由源地址(UDP 假连接不实际发包,
/// 只让 OS 选路)。无网络/特殊多网卡环境可能拿不到,如实返回 None。
fn lan_ipv4() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], 0))).ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (ip.is_ipv4() && !ip.is_loopback()).then_some(ip)
}

/// /api/addr(FR-28 远程访问框数据):本机/局域网地址是否开放 + 局域网 IP。
/// open = 监听地址非回环(0.0.0.0 或具体网卡)时其他设备才可达;
/// 回环监听时 lanIp 仍返回(端口开放后即用),前端只决定展示与否。
#[derive(serde::Serialize)]
struct AddrInfo {
    port: u16,
    open: bool,
    #[serde(rename = "lanIp")]
    lan_ip: Option<String>,
}
async fn api_addr(State(shared): State<Arc<Shared>>) -> Json<AddrInfo> {
    // lan_ipv4 的 UDP connect 是同步阻塞调用(审计 P2):网络栈异常/杀软拦截
    // 时可达数百 ms,放 spawn_blocking 避免冻结 tokio worker 线程上的并发请求
    let lan_ip = tokio::task::spawn_blocking(lan_ipv4)
        .await
        .ok()
        .flatten()
        .map(|i| i.to_string());
    Json(AddrInfo {
        port: shared.port,
        open: !shared.listen.is_loopback(),
        lan_ip,
    })
}

/// 在系统默认浏览器打开管理台(壳内前端按钮用;SerialHub Sprint7 实测壳内
/// window.open 被原生层吞掉,浏览器访问用不到此端点)。可选 JSON body
/// `{"url": …}` 供关于框打开 GitHub 外链,仅放行本仓库前缀、其余一律回落
/// 管理台本页——保持「不接受任意 URL」的打开面约束。
async fn api_open_browser(State(shared): State<Arc<Shared>>, body: String) -> StatusCode {
    const REPO_PREFIX: &str = "https://github.com/MisakaMikoto128/";
    let url = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(str::to_owned))
        .filter(|u| u.starts_with(REPO_PREFIX))
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", shared.port));
    open_in_browser(&url);
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
    /// 同形,继续由 serde_json::json! 构造(与 device_info_json 单点同源)
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
        // 先消化后睡眠(消化在前):装配阶段入队的消息(强制 demo 的启动
        // State(true))必须在 axum bind 前落地,保证首个 HTTP 可见时连接态
        // 已就绪(设计文档确定性论证;旧版 sleep-first 首轮要等 10ms,存在
        // bind 后短窗内 connected=false 的竞态)。稳态节奏不变(每轮仍间隔
        // FLUSH_MS),仅首个 tick 提前
        let rx_ending = *shared.rx_ending.lock().unwrap();
        let auto_frame = shared.auto_frame.load(Ordering::Relaxed);
        let mut pump = shared.pump.lock().unwrap();
        loop {
            match msg_rx.try_recv() {
                Ok(WorkerMsg::Block(text)) => {
                    if !pump.paused {
                        // RX 统计按 worker 发出的块长计:HEX 接收的文本→hex
                        // 转换已统一下沉到各后端 worker 内(真机 rtt.rs、demo
                        // 线程消费 worker_shared().hex_rx),块到达时已是最终
                        // 展示文本,demo 与真机 hex 的统计口径由此对齐
                        shared.rx_bytes.fetch_add(text.len() as u64, Ordering::Relaxed);
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
                    // WS 广播用清洗后的文案(审计 P1):status_text 的语义是
                    // 「去横幅圆点前缀」,WS state 事件与 /api/status 的
                    // sessionStatus 必须同口径,避免前端按已清洗消费却拿到原文
                    let _ = shared.events_tx.send(
                        serde_json::to_string(&WsEvent::State {
                            connected,
                            status: clean_status_text(&status),
                        })
                        .unwrap(),
                    );
                }
                Ok(WorkerMsg::DeviceInfo(info)) => {
                    let json = device_info_json(&info);
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
                    // (alive=false 由 worker 线程在发出 Exited 前置位);
                    // demo 后端的进程级单例线程不发 Exited(只因 APP_SHUTDOWN 退出)
                    let mine = {
                        let mut s = shared.session.lock().unwrap();
                        let mine = match s.as_ref() {
                            // 句柄已被 reset reconnect 提前取走:按幂等清理处理
                            None => true,
                            Some(h) => !h.handle.alive.load(Ordering::Relaxed),
                        };
                        if mine {
                            *s = None;
                        }
                        mine
                    };
                    if mine {
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
        // 节流睡眠(本轮收尾):消化在前、睡眠在后,见循环头注释
        std::thread::sleep(Duration::from_millis(FLUSH_MS));
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

/// DeviceInfo → WS device 事件 / /api/status device 字段共用的 8 字段 JSON
/// (单一构造点,键名契约由 device_info_json_keys_match_status_contract 锁定;
/// demo 后端每次 false→true 迁移随发 DeviceInfo 演示九字段,亦经此单点)
fn device_info_json(info: &DeviceInfo) -> serde_json::Value {
    serde_json::json!({
        "firmware": info.firmware, "hardware": info.hardware,
        "serial": info.serial, "core": info.core_name,
        "cpu": info.core_cpu, "target": info.target,
        "iface": info.iface, "speedKhz": info.speed_khz,
    })
}

async fn api_status(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let connected = shared.connected.load(Ordering::Relaxed);
    // device 8 字段(与 WS device 事件同形):由后端连接成功时发 DeviceInfo
    // 消息供数,断开统一清空(State(false) 分支),与统计清零同口径
    let device = shared
        .device_info
        .lock()
        .unwrap()
        .as_ref()
        .map(device_info_json);
    let session_sec = shared
        .session_start
        .lock()
        .unwrap()
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    Json(serde_json::json!({
        "connected": connected,
        "rxBytes": shared.rx_bytes.load(Ordering::Relaxed),
        "txBytes": shared.tx_bytes.load(Ordering::Relaxed),
        "rowsTotal": shared.pump.lock().unwrap().rows_len(),
        "uptimeSec": shared.started.elapsed().as_secs(),
        "sessionSec": session_sec,
        "sessionStatus": shared.status_text.lock().unwrap().clone(),
        "device": device,
        "version": env!("CARGO_PKG_VERSION"),
        "backend": shared.backend().id(),
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
        "jlinkSerial": *shared.jlink_serial.lock().unwrap(),
        "backend": shared.backend().id(),
    }))
}

/// /api/backends(阶段二后端可选面):当前选定 + 本进程可用后端列表。
/// items 恒非空(普通形态 jlink+demo;--demo-log 强制形态仅 demo)。
/// 文案来自后端实现的 &'static 字面量,前端仍按注入面纪律用 DOM API 渲染
async fn api_backends(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let items: Vec<serde_json::Value> = shared
        .backends_available
        .iter()
        .map(|b| {
            serde_json::json!({
                "id": b.id(),
                "name": b.display_name(),
                "description": b.description(),
                "isVirtual": b.is_virtual(),
            })
        })
        .collect();
    Json(serde_json::json!({
        "selected": shared.backend().id(),
        "items": items,
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
    // 设备候选统一从 shared.device_names 供数:jlink 来自后台枚举回传,
    // demo 在装配期已由 initial_devices 种子(演示设备库)
    Json(shared.device_names.lock().unwrap().clone())
}

async fn api_jlinks(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let list = shared.jlinks.lock().unwrap().clone();
    Json(serde_json::json!(list
        .iter()
        .map(|(sn, name)| serde_json::json!({"sn": sn, "name": name}))
        .collect::<Vec<_>>()))
}

/// 连接:连接前门禁(后端 can_connect,Err→409)→ 芯片名补全(库内子串
/// 匹配首个全称,与桌面版同规则)→ 连接参数写入偏好快照源 → 后端 connect
/// (jlink spawn worker;demo 命令进单例线程模拟连接,不加载 DLL)
async fn api_connect(State(shared): State<Arc<Shared>>, Json(req): Json<ConnectReq>) -> Response {
    let backend = shared.backend();
    // reset reconnect 的 1 秒等待窗口内 session 已被 take 清空,存活检查
    // 会放行——重连门闩在此堵住双 worker 并发(审计 P1;demo 后端不会
    // 置位此门闩,通用检查对其恒放行,与现状一致)
    if shared.reconnecting.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "正在自动重连,请稍候").into_response();
    }
    // 后端门禁(存活检查「已有连接」/ BUSY 门闩「设备库枚举中,请稍候几秒
    // 再连接」;demo 恒 Ok)——文案由后端提供,与现状逐字一致
    if let Err(msg) = backend.can_connect(&*shared) {
        return (StatusCode::CONFLICT, msg).into_response();
    }
    let chip_raw = req.chip.trim().to_string();
    if chip_raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "请先填写目标芯片型号").into_response();
    }
    // 校验名单与前端看到的候选同源:统一取 shared.device_names(demo 的
    // 演示设备库已在装配期种子进同一槽)
    let full: Vec<String> = shared.device_names.lock().unwrap().clone();
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
    // J-Link 选定:请求里的 sn 必须在当前列表内才生效(下拉可能是过期列表,
    // 设备拔出后回落自动选首台,避免 connect 阶段「找不到序列号」直接失败)
    let jlink_sn = req
        .jlink_sn
        .filter(|sn| shared.jlinks.lock().unwrap().iter().any(|(s, _)| s == sn));

    // 连接参数进偏好快照源(tick 内 500ms 落盘;重启后面板初值恢复)
    *shared.chip_name.lock().unwrap() = chip.clone();
    *shared.iface_index.lock().unwrap() = iface_index as i32;
    *shared.speed_index.lock().unwrap() = speed_index as i32;
    *shared.channel.lock().unwrap() = channel;
    *shared.jlink_serial.lock().unwrap() = jlink_sn;
    // 上次连接参数:reset reconnect 模式的重连来源(demo 也记录,语义一致)
    let params = ConnectParams {
        chip: chip.clone(),
        iface_index,
        speed_index,
        channel,
        jlink_sn,
    };
    *shared.last_connect.lock().unwrap() = Some(params.clone());

    if let Err(msg) = backend.connect(&*shared, &params) {
        return (StatusCode::CONFLICT, msg).into_response();
    }
    StatusCode::OK.into_response()
}

/// 断开:走当前后端(jlink = 置停止标志 + 空发送唤醒,Exited 消息回到
/// 未连接态;demo = 内部 Disconnect 命令,手动断开后不自动重连,由 demo
/// 状态机保证)
async fn api_disconnect(State(shared): State<Arc<Shared>>) -> StatusCode {
    shared.backend().disconnect(&*shared);
    StatusCode::OK
}

async fn api_power(State(shared): State<Arc<Shared>>, Json(req): Json<PowerReq>) -> Response {
    // demo 与真机同语义:未连接拒绝(前端此时也禁用勾选框;409 文案由
    // 后端提供,与现状逐字一致)
    match shared.backend().power(&*shared, req.on) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetReq {
    /// 复位模式:"in-place"(缺省)| "reconnect";缺省/未知值一律 in-place
    #[serde(default)]
    mode: Option<String>,
}

/// 复位两模式:in-place = 现有 Reset 命令(worker 复位目标并重挂 RTT 续收);
/// reconnect = 先走断开逻辑,1 秒后用上次连接参数重新 spawn worker(重启服务
/// 且无上次参数 → 409)。mode 解析、last_connect 检查、reconnecting 门闩置位
/// 留在 web 层;具体执行走当前后端——reset_reconnect 不被支持时(demo,
/// caps().reset_reconnect == false)一律回落 reset_in_place,与现状「demo
/// 忽略 mode」逐字等价。
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
    let backend = shared.backend();
    // 不支持重连式复位的后端(demo):忽略 mode,一律 in-place 模拟
    // (未连接 409,文案由后端提供与现状逐字一致)
    if !backend.caps().reset_reconnect {
        return match backend.reset_in_place(&*shared) {
            Ok(()) => StatusCode::OK.into_response(),
            Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
        };
    }
    match parse_reset_mode(req.mode.as_deref()) {
        ResetMode::InPlace => match backend.reset_in_place(&*shared) {
            Ok(()) => StatusCode::OK.into_response(),
            Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
        },
        ResetMode::Reconnect => {
            // 已在重连等待期:拒绝重复触发(避免多个重连线程叠跑)
            if shared.reconnecting.load(Ordering::Relaxed) {
                return (StatusCode::CONFLICT, "正在自动重连,请稍候").into_response();
            }
            let Some(params) = shared.last_connect.lock().unwrap().clone() else {
                return (StatusCode::CONFLICT, "无上次连接参数,无法重连").into_response();
            };
            // 重连等待期门闩(审计 P1):调用 reset_reconnect 之前置位(设计
            // 契约 §3.2,比历史 take 后置位更保守,两行间无 await,可观测
            // 等价)——此刻起 api_connect 以 409 拒绝,直到重连线程收尾清除
            shared.reconnecting.store(true, Ordering::Relaxed);
            match backend.reset_reconnect(shared.clone(), &params) {
                Ok(()) => StatusCode::OK.into_response(),
                Err(msg) => {
                    // 后端未受理(理论不可达):门闩回滚,避免永久堵死连接
                    shared.reconnecting.store(false, Ordering::Relaxed);
                    (StatusCode::CONFLICT, msg).into_response()
                }
            }
        }
    }
}

/// 运行时参数:逐字段可选,worker 共享原子热切换(与桌面版同语义)。
/// 负数一律 400(审计 P2:此前 i32 负值被 clamp(0,N) 静默夹成 0,调用方
/// 得不到任何非法输入反馈;无符号字段负数由 serde 反序列化直接 400)
async fn api_settings(State(shared): State<Arc<Shared>>, Json(req): Json<SettingsReq>) -> Response {
    if let Some(v) = req.rx_ending {
        if v < 0 {
            return (StatusCode::BAD_REQUEST, "rxEnding 不能为负数").into_response();
        }
        *shared.rx_ending.lock().unwrap() = v.clamp(0, 4);
    }
    if let Some(v) = req.frame_timeout {
        shared
            .frame_timeout_ms
            .store(v.clamp(1, 200), Ordering::Relaxed);
    }
    if let Some(v) = req.encoding_index {
        if v < 0 {
            return (StatusCode::BAD_REQUEST, "encodingIndex 不能为负数").into_response();
        }
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
        if v < 0 {
            return (StatusCode::BAD_REQUEST, "sendEnding 不能为负数").into_response();
        }
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
        if v < 0 {
            return (StatusCode::BAD_REQUEST, "ifaceIndex 不能为负数").into_response();
        }
        *shared.iface_index.lock().unwrap() = v.clamp(0, 1);
    }
    if let Some(v) = req.speed_index {
        if v < 0 {
            return (StatusCode::BAD_REQUEST, "speedIndex 不能为负数").into_response();
        }
        *shared.speed_index.lock().unwrap() = v.clamp(0, SPEEDS_KHZ.len() as i32 - 1);
    }
    if let Some(v) = req.channel {
        *shared.channel.lock().unwrap() = v.clamp(0, 15);
    }
    // 后端切换(阶段二):未知/不可用 id → 400;连接态(判据 shared.connected,
    // 不是 session 槽——demo 的会话槽恒活)→ 409「断开后才能切换后端」;
    // 成功 = 换 Shared.backend 引用(写偏好走 snapshot_prefs 快照链路)+
    // 旧后端 on_deselected + 新后端 on_selected(切走 demo 发内部 Disconnect
    // 清连接态;切回 jlink 清候选 + 枚举回填)。同 id 幂等 no-op:不触发回调,
    // 防止 demo 的 on_deselected 手动断开永久挂起 20s/3s 自动周期
    if let Some(id) = req.backend.as_deref() {
        let Some(target) = shared
            .backends_available
            .iter()
            .copied()
            .find(|b| b.id() == id)
        else {
            return (
                StatusCode::BAD_REQUEST,
                format!("未知或不可用的后端:{id}"),
            )
                .into_response();
        };
        if shared.connected.load(Ordering::Relaxed) {
            return (StatusCode::CONFLICT, "断开后才能切换后端").into_response();
        }
        let old = shared.backend();
        if old.id() != target.id() {
            old.on_deselected(&*shared);
            *shared.backend.lock().unwrap() = target;
            target.on_selected(&*shared);
        }
    }
    StatusCode::OK.into_response()
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
/// ("● 已连接 (demo)" → "已连接 (demo)")。循环剥离:双 bullet/前导空格
/// 等变体也剥干净(审计 P3)
fn clean_status_text(s: &str) -> String {
    let mut out = s.trim();
    while let Some(stripped) = out.strip_prefix('●') {
        out = stripped.trim();
    }
    out.to_string()
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
    const MARK_LABEL_CAP: usize = 200;
    let label = if req.text.is_empty() {
        "标记".to_string()
    } else {
        // 512 单行兜底只作用于设备流(push_colored_line 不截),HTTP 直发的
        // 标记在入口限长,防超长行经 WS/导出放大(审计 P3)
        req.text.chars().take(MARK_LABEL_CAP).collect()
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

/// 导出当前显示的全部日志(.log 纯文本附件:行文本 + \r\n 行尾,时间戳取
/// 本地时间)
async fn api_export(State(shared): State<Arc<Shared>>) -> Response {
    // 锁内只做快照克隆,拼接放到锁外(审计 P3:缩短泵锁持有时间)
    let rows = shared.pump.lock().unwrap().snapshot_rows();
    let mut body = String::new();
    for row in &rows {
        for run in row {
            body.push_str(&run.text);
        }
        body.push_str("\r\n");
    }
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

/// WS 会话:初次全量快照 + 订阅事件流;Lagged 自动重同步。
/// 重同步限流(审计 P2):慢消费者持续 Lagged 时,若每次都无条件发 500 行
/// 快照,会形成「Lagged → 全量快照 → 又 Lagged」的重同步风暴(持泵锁全量
/// 克隆+序列化,反噬数据泵节奏)。短窗内重同步过频即断开,由客户端重连取
/// 新快照——重连粒度的循环替代每条消息粒度的风暴。
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
    // Lagged 重同步节流:1 秒内超过 3 次视为追不平,断开让客户端重连
    let mut resync_window = Instant::now();
    let mut resync_burst = 0u32;
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
                        if resync_window.elapsed() >= Duration::from_secs(1) {
                            resync_window = Instant::now();
                            resync_burst = 0;
                        }
                        resync_burst += 1;
                        if resync_burst > 3 {
                            break;
                        }
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
        // 阶段二:backend 可选键(缺省 None = 不切换请求;命中 = 切数据源)
        let req: SettingsReq = serde_json::from_str(r#"{"backend":"demo"}"#).unwrap();
        assert_eq!(req.backend.as_deref(), Some("demo"));
        let req: SettingsReq = serde_json::from_str("{}").unwrap();
        assert_eq!(req.backend, None);
        // 与其它字段共存(逐字段可选语义)
        let req: SettingsReq =
            serde_json::from_str(r#"{"backend":"jlink","hexRx":true}"#).unwrap();
        assert_eq!(req.backend.as_deref(), Some("jlink"));
        assert_eq!(req.hex_rx, Some(true));
    }

    #[test]
    fn resolve_backend_cli_then_pref_then_first() {
        let jlink = backend::backend_by_id(backend::BACKEND_JLINK).unwrap();
        let demo = backend::backend_by_id(backend::BACKEND_DEMO).unwrap();
        let normal = [jlink, demo];
        let forced = [demo];
        let id_of = |b: &'static dyn RttBackend| b.id();
        // ① CLI 命中可用列表 → 最高优先
        assert_eq!(id_of(resolve_backend(Some("demo"), "jlink", &normal)), "demo");
        // ② CLI 非法(未知 id)→ 忽略,偏好生效
        assert_eq!(id_of(resolve_backend(Some("nope"), "demo", &normal)), "demo");
        // ③ CLI 合法但强制形态不含该后端 → 忽略回落(--demo-log 恒 demo)
        assert_eq!(id_of(resolve_backend(Some("jlink"), "jlink", &forced)), "demo");
        // ④ 偏好归一化:不在可用列表(未知/空)→ 回落列表首个(普通形态 = jlink)
        assert_eq!(id_of(resolve_backend(None, "probe-rs", &normal)), "jlink");
        assert_eq!(id_of(resolve_backend(None, "", &normal)), "jlink");
        // ⑤ 强制形态偏好是 jlink 也不可用 → 列表首个 = demo
        assert_eq!(id_of(resolve_backend(None, "jlink", &forced)), "demo");
        // ⑥ 偏好 demo + 普通形态 → demo(界面选后端持久化恢复的核心价值)
        assert_eq!(id_of(resolve_backend(None, "demo", &normal)), "demo");
        // id 匹配大小写敏感(与 backend_by_id 同口径),不静默归一
        assert_eq!(id_of(resolve_backend(Some("DEMO"), "jlink", &normal)), "jlink");
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
                status: "● 未连接 (demo)".into(),
            })
            .unwrap(),
            r#"{"type":"state","connected":false,"status":"● 未连接 (demo)"}"#
        );
        // device:info 载荷与 /api/status 的 device 同源(device_info_json 单点),
        // 断言锁外层包装与历史 format! 逐字节同形;8 字段键名契约由
        // device_info_json_keys_match_status_contract 单独锁定
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
