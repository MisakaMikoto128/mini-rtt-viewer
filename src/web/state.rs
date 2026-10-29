//! Web 共享状态、偏好快照与发送管线。

use super::*;

/// 服务共享状态。
pub(crate) struct Shared {
    pub(crate) pump: Mutex<LogPump>,
    pub(crate) rx_ending: Mutex<i32>,
    pub(crate) tx_bytes: AtomicU64,
    pub(crate) rx_bytes: AtomicU64,
    pub(crate) started: Instant,
    /// 事件流:rows/snapshot/cleared/state/device/progress/names/jlinks/stats
    pub(crate) events_tx: broadcast::Sender<String>,
    pub(crate) connected: AtomicBool,
    pub(crate) seq: AtomicU64,
    pub(crate) clear_seq: AtomicU64,
    // ---- 会话槽:由当前后端维护 worker 句柄与命令通道 ----
    pub(crate) session: Mutex<Option<SessionHandle>>,
    pub(crate) msg_tx: mpsc::Sender<WorkerMsg>,
    /// reset reconnect 的 1 秒等待窗口门闩(审计 P1):重连线程已把 session
    /// 清空,此期间 api_connect 的存活检查会放行——用独立标志堵住
    /// 「复位重连等待期内手动点连接 → 双 worker 并发抢 J-Link」的窗口
    pub(crate) reconnecting: AtomicBool,
    pub(crate) frame_timeout_ms: Arc<AtomicU32>,
    pub(crate) encoding_index: Arc<AtomicU32>,
    pub(crate) hex_rx: Arc<AtomicBool>,
    pub(crate) device_names: Mutex<Vec<String>>,
    pub(crate) jlinks: Mutex<Vec<(u32, String)>>,
    /// 「J-Link」下拉选中的调试器序列号(连接时校验后在列表内才生效;
    /// None = 自动选首台。经快照落盘,prefs.jlink_serial 恢复)
    pub(crate) jlink_serial: Mutex<Option<u32>>,
    pub(crate) device_info: Mutex<Option<DeviceInfo>>,
    /// 当前数据采集后端(阶段二起可经 /api/settings 切换)
    pub(crate) backend: Mutex<&'static dyn RttBackend>,
    /// 本进程可用后端列表(/api/backends items 与 settings 校验的单一来源;
    /// 装配期定死:正式服务仅 J-Link,独立测试进程可显式注入测试后端)
    pub(crate) backends_available: Vec<&'static dyn RttBackend>,
    /// 连接状态变化回调(gui 壳注入,驱动托盘图标;纯服务模式 None)
    pub(crate) on_state: Option<OnEvent>,
    // ---- 偏好快照源(tick 内 500ms 比对落盘;未连接态也保存)----
    /// 启动时读到的完整偏好底版:桌面专用字段原样保留写回,不丢失旧值
    pub(crate) prefs_base: StoredPrefs,
    pub(crate) auto_frame: AtomicBool,
    pub(crate) search_regex: AtomicBool,
    pub(crate) chip_name: Mutex<String>,
    pub(crate) iface_index: Mutex<i32>,
    pub(crate) speed_index: Mutex<i32>,
    pub(crate) channel: Mutex<u32>,
    /// 发送历史(最新在前,去重,上限 50;/api/history 读,快照落盘)
    pub(crate) send_history: Mutex<Vec<String>>,
    /// 发送行尾 0=CRLF 1=LF 2=CR 3=无(发送时按选择追加行尾字节,文本/HEX 同规则)
    pub(crate) send_ending: Mutex<i32>,
    /// HEX 发送模式(前端发送框按十六进制字节解析)
    pub(crate) hex_send: AtomicBool,
    /// FR-24:连接采集期间阻止系统睡眠与关闭显示器
    pub(crate) keep_awake: AtomicBool,
    // ---- 定时发送(复用 /api/send 发送管线)----
    pub(crate) timer_on: AtomicBool,
    /// 定时发送间隔(秒)
    pub(crate) timer_interval: Mutex<f64>,
    /// 定时发送内容:最近一次 /api/send 的文本(可经 /api/timer 显式指定)
    pub(crate) timer_text: Mutex<String>,
    pub(crate) timer_hex: AtomicBool,
    // ---- /api/status 会话扩展 + reset reconnect 模式 ----
    /// 连接会话计时起点:State 迁移到已连接时记,断开清零
    pub(crate) session_start: Mutex<Option<Instant>>,
    /// 当前状态文案(State 消息携带,去横幅圆点前缀;初始"未连接")
    pub(crate) status_text: Mutex<String>,
    /// 上次成功连接的参数(reset reconnect 的重连来源;/api/connect 成功时
    /// 刷新,启动时从偏好底版恢复)
    pub(crate) last_connect: Mutex<Option<ConnectParams>>,
    /// 服务监听端口(壳内前端「浏览器打开」按钮经 /api/open-browser 复用)
    pub(crate) port: u16,
    /// 服务监听地址(FR-28 /api/addr 据此判断局域网地址是否对外可达)
    pub(crate) listen: std::net::IpAddr,
}

#[derive(Debug)]
pub(crate) enum SendError {
    Invalid(String),
    Unavailable(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Unavailable(message) => f.write_str(message),
        }
    }
}

impl Shared {
    /// 当前数据采集后端(装配期写入;阶段二起可经 /api/settings 切换)
    pub(crate) fn backend(&self) -> &'static dyn RttBackend {
        *self.backend.lock().unwrap()
    }

    /// 发送管线(手动 /api/send 与定时发送共用):先确认 worker 可接收，
    /// 命令入队成功后才产生回显、定时内容与历史副作用；TX 由 worker 的 Tx 回报计数。
    pub(crate) fn send_payload(&self, text: &str, hex: bool) -> Result<(), SendError> {
        if text.is_empty() {
            return Err(SendError::Invalid("空输入".into()));
        }
        let payload = if hex {
            parse_hex_bytes(text).map_err(SendError::Invalid)?
        } else {
            text.as_bytes().to_vec()
        };
        // 发送行尾(FR-12/F7):按 UI 选择追加行尾字节,文本/HEX 同规则
        let payload = append_send_ending(payload, *self.send_ending.lock().unwrap());
        if !self.connected.load(Ordering::Relaxed) {
            return Err(SendError::Unavailable("未连接,无法发送".into()));
        }
        let tx = {
            let session = self.session.lock().unwrap();
            let Some(session) = session.as_ref() else {
                return Err(SendError::Unavailable("无活动会话,无法发送".into()));
            };
            if !session.handle.alive.load(Ordering::Relaxed) {
                return Err(SendError::Unavailable("采集线程已退出,无法发送".into()));
            }
            session.cmd_tx.clone()
        };
        tx.send(WorkerCmd::Send(payload))
            .map_err(|_| SendError::Unavailable("发送通道已关闭".into()))?;

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
pub(crate) fn history_push(history: &mut Vec<String>, text: &str) {
    const SEND_HISTORY_CAP: usize = 50;
    if let Some(i) = history.iter().position(|t| t == text) {
        history.remove(i);
    }
    history.insert(0, text.to_string());
    history.truncate(SEND_HISTORY_CAP);
}

/// 从 Shared 当前状态构建偏好快照(快照比对落盘的单一来源)
pub(crate) fn snapshot_prefs(s: &Shared) -> StoredPrefs {
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
    p.timer_text = s.timer_text.lock().unwrap().clone();
    p.timer_hex = s.timer_hex.load(Ordering::Relaxed);
    p.send_history = s.send_history.lock().unwrap().clone();
    p.jlink_serial = *s.jlink_serial.lock().unwrap();
    p.send_ending = *s.send_ending.lock().unwrap();
    p.hex_send = s.hex_send.load(Ordering::Relaxed);
    p.keep_awake = s.keep_awake.load(Ordering::Relaxed);
    // 后端 id(阶段二):随快照落盘,重启恢复上次选定(serve 内已归一化)
    p.backend = s.backend().id().to_string();
    p
}

pub(crate) fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
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
pub(crate) fn append_send_ending(mut payload: Vec<u8>, ending: i32) -> Vec<u8> {
    match ending {
        0 => payload.extend_from_slice(b"\r\n"),
        1 => payload.extend_from_slice(b"\n"),
        2 => payload.extend_from_slice(b"\r"),
        _ => {}
    }
    payload
}
