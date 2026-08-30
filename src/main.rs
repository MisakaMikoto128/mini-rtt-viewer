// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 模块树统一在 lib(crate mini_rtt_viewer):UI 生成代码与业务模块都从那里来,
// 本文件只是"装配层"——创建 AppWindow、把回调接到 Ctx 的方法上、起 timer 泵、
// 管理退出编排。业务规则一律不在这里实现。
use mini_rtt_viewer::log_model::{LogPump, DEFAULT_FRAME_TIMEOUT_MS, FLUSH_MS};
use mini_rtt_viewer::rtt::{self, WorkerCmd, WorkerHandle, WorkerMsg, APP_SHUTDOWN};
use mini_rtt_viewer::{device_db, demo, single_instance, AppWindow, InfoRow, LogRun, LogRow};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const SPEEDS_KHZ: [u32; 8] = [100, 200, 500, 1000, 2000, 4000, 8000, 12000];

/// 会话标记行颜色(与主题强调色同系)
const MARK_COLOR: (u8, u8, u8) = (0x28, 0xaf, 0xe9);
/// 发送回显行颜色(中性灰,与设备数据一眼区分)
const ECHO_COLOR: (u8, u8, u8) = (0x8f, 0x8f, 0x9a);

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
        year: 0, month: 0, day_of_week: 0, day: 0,
        hour: 0, minute: 0, second: 0, millis: 0,
    };
    unsafe { GetLocalTime(&mut st) };
    st
}
/// "HH:MM:SS"(标记行内嵌)
fn now_hms() -> String {
    let st = local_time();
    format!("{:02}:{:02}:{:02}", st.hour, st.minute, st.second)
}
/// "YYYYMMDD_HHMMSS"(导出文件名)
fn now_stamp() -> String {
    let st = local_time();
    format!("{:04}{:02}{:02}_{:02}{:02}{:02}", st.year, st.month, st.day, st.hour, st.minute, st.second)
}

/// HEX 发送模式输入解析:容忍空格/冒号/连字符分隔与 0x 前缀,按字节解析。
/// 空、奇数长度、非法字符均报错(原文回显在状态栏)。
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
    for pair in cleaned.as_bytes().chunks_exact(2) {
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

fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.2} MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 3600 {
        format!("{:02}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

/// 标记行文本(自动带本地时间戳;label 为空则只有时间)。独立自由函数:
/// tick(持 pump borrow)与 UI 回调(不持)两条路径共用
fn mark_text(label: &str) -> String {
    if label.is_empty() {
        format!("── {} ──", now_hms())
    } else {
        format!("── [{}] {label} ──", now_hms())
    }
}

/// 收发统计(会话级:连接成功时清零,断开停止累计)
struct Stats {
    tx: u64,
    rx: u64,
    since: Option<Instant>,
    /// 上次刷新统计栏文本的时刻(节流)
    last_ui: Instant,
}
impl Default for Stats {
    fn default() -> Self {
        Self { tx: 0, rx: 0, since: None, last_ui: Instant::now() }
    }
}

/// 默认日志前景色(Run.fg == None 时用)
fn log_default_fg() -> slint::Color {
    slint::Color::from_rgb_u8(0xdd, 0xdd, 0xdd)
}

/// UI 侧全部共享状态。回调闭包与 timer 只克隆这一个 Rc;
/// 方法即业务规则,main() 不出现任何 if 业务判断。
struct Ctx {
    pump: Rc<RefCell<LogPump>>,
    worker: Rc<RefCell<Option<Arc<WorkerHandle>>>>,
    /// 当前 worker 的命令管道(每次连接整体替换);None=无 worker 可命令
    cmd_tx: Rc<RefCell<Option<mpsc::Sender<WorkerCmd>>>>,
    msg_tx: mpsc::Sender<WorkerMsg>,
    msg_rx: mpsc::Receiver<WorkerMsg>,
    frame_timeout_ms: Arc<AtomicU32>,
    /// 设备库全量名单(后台枚举/磁盘缓存回传),筛选下拉按输入重建
    device_names: Rc<RefCell<Vec<SharedString>>>,
    /// 本机接入的 J-Link (序列号, 显示名);下拉选中项在连接时换算成 selected_sn
    jlinks: Rc<RefCell<Vec<(u32, String)>>>,
    log_rows: Rc<VecModel<LogRow>>,
    last_status: Rc<RefCell<SharedString>>,
    /// 收发字节与会话时长统计
    stats: RefCell<Stats>,
}

impl Ctx {
    /// timer 每 [`FLUSH_MS`] 一次的泵周期:同步断帧参数 → 消化 worker 消息 → 增量上屏
    fn tick(&self, ui: &AppWindow) {
        // 0. 把输入框的断帧间隔同步给 worker(判定在 worker,精度 5ms)
        if ui.get_auto_frame() {
            let v = ui
                .get_frame_timeout()
                .trim()
                .parse::<u32>()
                .unwrap_or(DEFAULT_FRAME_TIMEOUT_MS);
            self.frame_timeout_ms.store(v.clamp(1, 200), Ordering::Relaxed);
        }
        // 接收行尾:0=自动 1=CRLF 2=LF 3=CR 4=无
        let rx_ending = ui.get_rx_ending();
        let mut pump = self.pump.borrow_mut();
        // 1. 消化 worker 消息
        loop {
            match self.msg_rx.try_recv() {
                Ok(WorkerMsg::Log(text)) => {
                    // 横幅提示(J-Link 报文)不是设备数据,不计 RX;暂停时同样丢弃
                    if !pump.paused {
                        pump.absorb_text(&text, rx_ending);
                    }
                }
                Ok(WorkerMsg::Block(text)) => {
                    // 暂停接收:数据直接丢弃
                    if !pump.paused {
                        self.stats.borrow_mut().rx += text.len() as u64;
                        pump.absorb_text(&text, rx_ending);
                    }
                }
                Ok(WorkerMsg::FrameEnd) => {
                    // worker 判定一帧结束(间隔超过断帧超时):切出缓冲为完整行
                    if !pump.paused && ui.get_auto_frame() {
                        pump.absorb_frame_end(rx_ending);
                    }
                }
                Ok(WorkerMsg::Progress(text)) => {
                    // 连接过程进度:只刷状态栏文字,绝不改变连接标志(防按钮闪烁)
                    ui.set_status_text(text.into());
                }
                Ok(WorkerMsg::State(connected, status)) => {
                    ui.set_connected(connected);
                    if !connected {
                        ui.set_connecting(false);
                    }
                    *self.last_status.borrow_mut() = status.clone().into();
                    ui.set_status_text(status.into());
                    // 会话统计与自动标记:连接清零起算,断开只在确有会话时补一条
                    // 标记(n<0 异常断开与正常断开都会发 State(false),take 去重)
                    if connected {
                        let mut st = self.stats.borrow_mut();
                        st.tx = 0;
                        st.rx = 0;
                        st.since = Some(Instant::now());
                        drop(st);
                        // 用已借用的 pump 直插标记——严禁调 self.insert_mark,
                        // 它会再 borrow_mut 同一 RefCell(tick 正持有),连接成功瞬间即 panic
                        pump.push_colored_line(&mark_text("已连接"), MARK_COLOR);
                    } else if self.stats.borrow_mut().since.take().is_some() {
                        pump.push_colored_line(&mark_text("已断开"), MARK_COLOR);
                    }
                }
                Ok(WorkerMsg::DeviceInfo(info)) => self.apply_device_info(ui, info),
                Ok(WorkerMsg::DeviceNames(names)) => self.apply_device_names(ui, names),
                Ok(WorkerMsg::JLinks(list)) => self.apply_jlinks(ui, list),
                Ok(WorkerMsg::Exited) => {
                    // worker 真正退出(含 DLL close),解锁"再连接";
                    // 电源输出随连接一起失效(DLL close 会断电)
                    *self.worker.borrow_mut() = None;
                    ui.set_connecting(false);
                    ui.set_connected(false);
                    ui.set_power_output(false);
                    self.stats.borrow_mut().since = None;
                }
                Err(_) => break,
            }
        }
        // 2. 单行长度兜底(超长帧/关闭自动断帧时的无换行流)
        pump.enforce_line_cap();
        // 3. 增量上屏:先同步头部裁剪(行模型只保留最新 MAX_LOG_ROWS 行,
        //    不裁会无限增长),再 push 新行(ANSI 已在 pump 内解析为带色段)
        let dropped = pump.take_dropped();
        for _ in 0..dropped {
            self.log_rows.remove(0);
        }
        if let Some(rows) = pump.take_new_rows() {
            for runs in rows {
                let spans: Vec<LogRun> = runs
                    .into_iter()
                    .map(|r| LogRun {
                        text: r.text.into(),
                        color: r
                            .fg
                            .map(|(r8, g8, b8)| slint::Color::from_rgb_u8(r8, g8, b8))
                            .unwrap_or_else(log_default_fg),
                    })
                    .collect();
                self.log_rows.push(LogRow { runs: ModelRc::new(VecModel::from(spans)) });
            }
            ui.set_log_row_count(self.log_rows.row_count() as i32);
        }
        drop(pump);
        // 4. 统计栏(500ms 节流:时长按秒变化,再快也是白画)
        let mut st = self.stats.borrow_mut();
        if st.last_ui.elapsed() >= Duration::from_millis(500) {
            st.last_ui = Instant::now();
            let (tx, rx, dur) = (st.tx, st.rx, st.since.map(|t| t.elapsed()));
            drop(st);
            let dur_text = dur.map(fmt_dur).unwrap_or_else(|| "--:--".into());
            ui.set_stats_text(
                format!("TX {} · RX {} · {}", fmt_bytes(tx), fmt_bytes(rx), dur_text).into(),
            );
        }
    }

    /// 设备信息区(字段对齐原 PySide6 工程;空字段 UI 显示 "—")
    fn apply_device_info(&self, ui: &AppWindow, info: rtt::DeviceInfo) {
        let rows = vec![
            InfoRow { label: "固件版本".into(), value: info.firmware.into() },
            InfoRow { label: "硬件版本".into(), value: info.hardware.into() },
            InfoRow { label: "序列号".into(), value: info.serial.into() },
            InfoRow { label: "核心名称".into(), value: info.core_name.into() },
            InfoRow { label: "核心 ID".into(), value: info.core_id.into() },
            InfoRow { label: "CPU 类型".into(), value: info.core_cpu.into() },
            InfoRow { label: "目标设备".into(), value: info.target.into() },
            InfoRow { label: "接口".into(), value: info.iface.into() },
            InfoRow { label: "速度(kHz)".into(), value: info.speed_khz.to_string().into() },
        ];
        ui.set_info_rows(ModelRc::new(VecModel::from(rows)));
    }

    /// 设备库候选全量替换 + 按当前输入重筛
    fn apply_device_names(&self, ui: &AppWindow, names: Vec<String>) {
        let full: Vec<SharedString> = names.into_iter().map(SharedString::from).collect();
        refilter(ui, &self.device_names.borrow(), &ui.get_chip_name());
        *self.device_names.borrow_mut() = full;
    }

    /// J-Link 下拉更新;选中索引夹回范围,显示名 "产品: 序列号"
    fn apply_jlinks(&self, ui: &AppWindow, list: Vec<(u32, String)>) {
        let descs: Vec<SharedString> = list
            .iter()
            .map(|(sn, product)| {
                if product.is_empty() {
                    format!("J-Link: {sn}")
                } else {
                    format!("{product}: {sn}")
                }
                .into()
            })
            .collect();
        let cur = ui.get_jlink_index();
        ui.set_jlink_names(ModelRc::new(VecModel::from(descs)));
        ui.set_jlink_index(if cur < list.len() as i32 && cur >= 0 {
            cur
        } else if list.is_empty() {
            -1
        } else {
            0
        });
        *self.jlinks.borrow_mut() = list;
    }

    /// 连接:校验 → 选定 SN → spawn worker。上一个 worker 还活着(可能阻塞在
    /// connect)时严禁并发——这是"严禁并发抢 J-Link"的门闩。
    fn start_connect(&self, ui: &AppWindow) {
        if self.worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed)) {
            return;
        }
        *self.worker.borrow_mut() = None;
        // 首次启动设备库还在后台枚举:原 Python 项目踩过「枚举与 connect 并发
        // 损坏 DLL TLS」的坑,枚举期间拒绝连接(窗口仅数秒,且只发生在无缓存的首次)
        if device_db::busy() {
            ui.set_status_text("● 设备库加载中,请稍候…".into());
            return;
        }
        // chip 名去首尾空白;空名直接拒绝(空设备名会让 J-Link DLL 沿用上一次设备,行为不可预期)
        let chip = ui.get_chip_name().trim().to_string();
        if chip.is_empty() {
            ui.set_status_text("● 请先填写目标芯片型号".into());
            return;
        }
        ui.set_connecting(true);
        ui.set_status_text("● 连接中…".into());
        // 多台 J-Link:把下拉选中的序列号交给 worker(Open 前选定);未选中/空列表 = 自动
        let idx = ui.get_jlink_index();
        let selected_sn =
            self.jlinks.borrow().get(idx as usize).filter(|_| idx >= 0).map(|(sn, _)| *sn);

        let (tx, rx) = mpsc::channel::<WorkerCmd>();
        *self.cmd_tx.borrow_mut() = Some(tx);
        let handle = rtt::spawn(
            rtt::WorkerConfig {
                chip,
                iface_index: ui.get_iface_index() as usize,
                speed_khz: SPEEDS_KHZ[ui.get_speed_index().clamp(0, 7) as usize],
                channel: ui.get_channel() as u32,
                frame_timeout_ms: self.frame_timeout_ms.clone(),
                selected_sn,
            },
            self.msg_tx.clone(),
            rx,
        );
        *self.worker.borrow_mut() = Some(handle);
    }

    /// 断开/取消连接:置停止标志,等 worker 的 Exited 消息回到未连接态。
    /// 不在此处清 worker 句柄 —— worker 可能还阻塞在 DLL 调用里,此刻 spawn 新
    /// worker 会并发抢 J-Link(数据损坏 + 状态错乱的根源)。
    fn request_disconnect(&self, ui: &AppWindow) {
        if let Some(h) = self.worker.borrow().as_ref() {
            h.stop.store(true, Ordering::Relaxed);
        }
        *self.cmd_tx.borrow_mut() = None; // 掐断旧管道,worker try_recv 后自行退出
        ui.set_connecting(true);
        ui.set_status_text("● 断开中…".into());
    }

    /// 发送:文本/HEX 两模式按发送行尾拼成原始字节交 worker;投递成功后回显
    fn send_text(&self, ui: &AppWindow) {
        let text = ui.get_send_text().to_string();
        if text.is_empty() {
            return;
        }
        let ending: &[u8] = match ui.get_send_ending() {
            1 => b"\n",
            2 => b"\r",
            3 => b"",
            _ => b"\r\n",
        };
        let payload = if ui.get_hex_send() {
            match parse_hex_bytes(&text) {
                Ok(mut b) => {
                    b.extend_from_slice(ending);
                    b
                }
                Err(e) => {
                    ui.set_status_text(format!("● HEX 格式错误:{e}").into());
                    return;
                }
            }
        } else {
            let mut b = text.clone().into_bytes();
            b.extend_from_slice(ending);
            b
        };
        if let Some(tx) = self.cmd_tx.borrow().as_ref() {
            let _ = tx.send(WorkerCmd::Send(payload.clone()));
            self.stats.borrow_mut().tx += payload.len() as u64;
            // 回显显示用户输入原文(HEX 模式下原文即 hex 串),不写发送框
            self.pump.borrow_mut().push_colored_line(&format!("» {text}"), ECHO_COLOR);
        }
    }

    /// 插入一条会话标记行。**只能在 UI 回调上下文调用**(此时不持 pump 的
    /// borrow);tick 内持 borrow 期间必须直接 `pump.push_colored_line(...)`
    fn insert_mark(&self, label: &str) {
        self.pump.borrow_mut().push_colored_line(&mark_text(label), MARK_COLOR);
    }

    /// 复位目标并恢复运行(仅连接状态;复位后 worker 重挂 RTT 继续收)
    fn reset_target(&self, ui: &AppWindow) {
        if !ui.get_connected() {
            return;
        }
        if let Some(tx) = self.cmd_tx.borrow().as_ref() {
            let _ = tx.send(WorkerCmd::Reset);
        }
        self.insert_mark("复位目标");
    }

    /// 导出当前显示的全部日志为 .log(纯文本;对话取消/空日志只作状态栏提示)
    fn save_log(&self, ui: &AppWindow) {
        let n = self.log_rows.row_count();
        if n == 0 {
            ui.set_status_text("● 日志为空,无需保存".into());
            return;
        }
        let mut body = String::new();
        for i in 0..n {
            let Some(row) = self.log_rows.row_data(i) else { continue };
            for j in 0..row.runs.row_count() {
                if let Some(seg) = row.runs.row_data(j) {
                    body.push_str(&seg.text);
                }
            }
            body.push_str("\r\n");
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("rtt_{}.log", now_stamp()))
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, body) {
            Ok(_) => ui.set_status_text(format!("● 已保存 {}", path.display()).into()),
            Err(e) => ui.set_status_text(format!("● 保存失败:{e}").into()),
        }
    }

    /// 电源输出:仅连接状态下真正下发;未连接时勾选状态回弹
    fn set_power(&self, ui: &AppWindow, on: bool) {
        if !ui.get_connected() {
            ui.set_power_output(false);
            return;
        }
        if let Some(tx) = self.cmd_tx.borrow().as_ref() {
            let _ = tx.send(WorkerCmd::Power(on));
        }
    }

    /// 清空:行模型清空 + 状态栏恢复(不退化为无参数的"已连接")
    fn clear_log(&self, ui: &AppWindow) {
        self.log_rows.set_vec(vec![]);
        ui.set_log_row_count(0);
        ui.set_status_text(self.last_status.borrow().clone());
        self.pump.borrow_mut().clear();
    }

    /// 暂停/继续接收:暂停期间 worker 读到的新数据直接丢弃(不进日志、不占缓冲)
    fn toggle_pause(&self, ui: &AppWindow) {
        let now = !ui.get_paused();
        ui.set_paused(now);
        self.pump.borrow_mut().paused = now;
        ui.set_status_text(if now {
            "● 已暂停接收(新数据被丢弃)".into()
        } else {
            self.last_status.borrow().clone()
        });
    }

    /// 应用退出编排:通知 worker 停止,等它清理完 DLL;超时强制退出,不留僵尸进程
    fn wait_worker_shutdown(&self) {
        APP_SHUTDOWN.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let alive =
                self.worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed));
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed)) {
            // worker 卡死在不可中断的 DLL 调用里(如模态弹窗):强制退出,宁可不优雅也不留僵尸
            std::process::exit(0);
        }
    }
}

/// 重建设备下拉候选:按输入大小写不敏感过滤(候选由 EditableCombo 的
/// 原生下拉展示,选中后回填输入框,Rust 无需维护选中态)
fn refilter(ui: &AppWindow, full: &[SharedString], needle: &str) {
    let n = needle.trim().to_uppercase();
    let list: Vec<SharedString> = if n.is_empty() {
        full.to_vec()
    } else {
        full.iter()
            .filter(|s| s.to_uppercase().contains(&n))
            .cloned()
            .collect()
    };
    ui.set_device_names(ModelRc::new(VecModel::from(list)));
}

fn main() -> anyhow::Result<()> {
    // --demo-log 是无设备的自动化测试模式:跳过单实例互斥,
    // 允许与真实实例并存(它不加载 JLinkARM.dll,不会抢 J-Link)
    let demo_mode = demo::is_enabled(&std::env::args().collect::<Vec<_>>());
    if !demo_mode {
        single_instance::enforce_single_instance();
    }
    let app = AppWindow::new()?;

    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    // 断帧间隔共享变量:UI 改输入框 → worker 实时读取(断帧判定在 worker,5ms 精度)
    let frame_timeout_ms = Arc::new(AtomicU32::new(DEFAULT_FRAME_TIMEOUT_MS));
    let ctx = Rc::new(Ctx {
        pump: Rc::new(RefCell::new(LogPump::default())),
        worker: Rc::new(RefCell::new(None)),
        cmd_tx: Rc::new(RefCell::new(None)),
        msg_tx,
        msg_rx,
        frame_timeout_ms,
        device_names: Rc::new(RefCell::new(Vec::new())),
        jlinks: Rc::new(RefCell::new(Vec::new())),
        log_rows: Rc::new(VecModel::from(Vec::<LogRow>::new())),
        last_status: Rc::new(RefCell::new("● 未连接".into())),
        stats: RefCell::default(),
    });
    app.set_log_rows(ModelRc::from(ctx.log_rows.clone()));

    if demo_mode {
        demo::spawn(ctx.msg_tx.clone());
    } else {
        // 后台枚举:目标设备库候选(有磁盘缓存则零 DLL 调用)+ 本机接入的 J-Link 列表。
        // device_db 不依赖 WorkerMsg,这里用转发线程适配消息类型
        let (db_tx, db_rx) = mpsc::channel::<device_db::DbResult>();
        device_db::spawn_background(db_tx);
        let msg_tx = ctx.msg_tx.clone();
        std::thread::spawn(move || {
            while let Ok(r) = db_rx.recv() {
                let msg = match r {
                    device_db::DbResult::DeviceNames(names) => WorkerMsg::DeviceNames(names),
                    device_db::DbResult::Emulators(list) => WorkerMsg::JLinks(list),
                };
                if msg_tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }

    // timer 泵:worker 消息 → 断行/ANSI → 增量上屏
    let timer = Timer::default();
    {
        let weak = app.as_weak();
        let ctx = ctx.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(FLUSH_MS), move || {
            if let Some(ui) = weak.upgrade() {
                ctx.tick(&ui);
            }
        });
    }

    // ---- 回调接线:闭包只做 weak 升级,业务全在 Ctx 方法 ----
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_connect_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.start_connect(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_disconnect_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.request_disconnect(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_send_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.send_text(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_clear_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.clear_log(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_pause_toggled(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.toggle_pause(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_power_toggled(move |on| {
            if let Some(ui) = weak.upgrade() {
                ctx.set_power(&ui, on);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_chip_filtered(move |text| {
            if let Some(ui) = weak.upgrade() {
                refilter(&ui, &ctx.device_names.borrow(), &text);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_reset_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.reset_target(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        app.on_mark_clicked(move || {
            ctx.insert_mark("");
        });
    }
    {
        let ctx = ctx.clone();
        let weak = app.as_weak();
        app.on_save_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ctx.save_log(&ui);
            }
        });
    }

    app.run()?;
    ctx.wait_worker_shutdown();
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
    }

    #[test]
    fn hex_parse_rejects_bad_input() {
        assert!(parse_hex_bytes("abc").is_err()); // 奇数位
        assert!(parse_hex_bytes("zz").is_err()); // 非法字符
        assert!(parse_hex_bytes("  ").is_err()); // 空
    }

    #[test]
    fn byte_and_duration_formatting() {
        assert_eq!(fmt_bytes(999), "999 B");
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024), "3.00 MB");
        assert_eq!(fmt_dur(Duration::from_secs(65)), "01:05");
        assert_eq!(fmt_dur(Duration::from_secs(3675)), "1:01:15");
    }
}
