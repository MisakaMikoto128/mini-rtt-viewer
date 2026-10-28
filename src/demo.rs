//! `--demo-log` 演示/测试数据源:无设备时验证滚动、断行、UTF-8/ANSI 颜色渲染。
//!
//! - 跳过单实例互斥由 main 负责:demo 模式允许与真实实例并存(它不加载
//!   JLink_x64.dll,不会抢 J-Link)
//! - 发送打微秒时间戳(stderr),用于测量显示节奏是否与发送节奏一致;
//!   非 demo 模式 `T0` 为空,零开销
//!
//! ## DemoBackend(后端抽象阶段一,设计契约 backend-abstraction.md §3.2)
//! `RttBackend` 实现:进程级**单例线程**(懒启动:强制形态 auto_connect 或
//! 首次 connect 时由 `ensure_singleton` 拉起;永不退出,除 APP_SHUTDOWN)。
//! - 连接/断开走**内部私有** `DemoCmd { Connect, Disconnect }` 通道(存
//!   `DEMO_BACKEND` 的 Mutex 槽,真机 worker 永远看不到);数据面命令
//!   (Send/Power/Reset)仍走 SessionHandle 的 WorkerCmd 通道;
//! - 恒活 SessionHandle:stop 标志永远无人置位、alive 恒 true;
//! - HEX 接收下沉:线程发送 Block 前自己消费 `worker_shared().hex_rx`,
//!   开启时把块文本按 UTF-8 字节转十六进制大写(与真机 worker 的 HEX 输出
//!   同格式,共用 `backend::text_to_hex_upper`)——原 web.rs tick 的 demo
//!   分支转换已迁入,rxBytes 统计口径与真机 hex 对齐(设计文档知情接受);
//! - 每次 false→true 迁移随发 `WorkerMsg::DeviceInfo`(演示九字段),替代
//!   原 web.rs 的 demo_device_json 展示分叉,api_status 走统一 DeviceInfo。
//!
//! ## 虚拟 worker 命令协议(F2)
//! demo 线程是真机 worker 的无设备替身,数据**仅在连接态产生**(与真机断开
//! 无 RTT 数据一致),断帧判定与真机 worker 同款(相邻数据间隔超过断帧超时
//! → FrameEnd,自动断帧开关在 pump 侧生效)。
//!
//! - 自动周期(无手动干预时):启动即连接 → 20s 断开 → 3s 重连,循环;
//! - 手动「断开」→ State(false) 且**不再自动重连**;手动「连接」→ 立即
//!   State(true) 且**不再自动断开**(忠实模拟真机:连接态保持到用户断开);
//! - 重置 → 日志行 + State(false) + ~800ms 后 State(true)(短暂断开重挂);
//! - 电源 → 日志行反馈(demo 无法真供电,未连接由后端 409 拦下);
//! - Send → 忽略(web 层已计数+回显)。

use crate::backend::{
    text_to_hex_upper, BackendCaps, ConnectParams, DeviceInfo, Host, RttBackend, SessionHandle,
    WorkerCmd, WorkerHandle, WorkerMsg, WorkerShared, APP_SHUTDOWN, BACKEND_DEMO,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// demo 模式的时间基准(微秒时间戳测量用)
pub static T0: OnceLock<Instant> = OnceLock::new();

const INTERVAL_MS: u64 = 80;
/// 自动周期:连接保持时长(此后自动断开)
const AUTO_SESSION_SECS: u64 = 20;
/// 自动周期:断开后到自动重连的间隔
const AUTO_RECONNECT_SECS: u64 = 3;
/// 重置模拟:State(false) 到 State(true) 的间隔(短暂断开重挂)
const RESET_GAP_MS: u64 = 800;

/// demo 后端私有连接命令(真机协议 WorkerCmd 已删除 DemoConnect/DemoDisconnect,
/// 连接面与数据面分离;真机 worker 永远看不到本类型)
enum DemoCmd {
    Connect,
    Disconnect,
}

/// demo 后端:仅持有内部 DemoCmd 通道槽(Some = 单例线程已拉起),
/// const 构造为 &'static 注册表项;会话句柄经 Host 的 session 槽管理
pub struct DemoBackend {
    demo_tx: Mutex<Option<mpsc::Sender<DemoCmd>>>,
}

/// 注册表静态实例(backend::backend_by_id 按 id 取用)
pub static DEMO_BACKEND: DemoBackend = DemoBackend {
    demo_tx: Mutex::new(None),
};

impl DemoBackend {
    /// 懒启动单例:首次需要时建 WorkerCmd 通道 + 恒活 SessionHandle 并登记;
    /// DemoCmd 通道存 static 槽。单例已存在时一切复用。
    fn ensure_singleton(&self, host: &dyn Host) {
        let mut slot = self.demo_tx.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
        let (demo_tx, demo_rx) = mpsc::channel::<DemoCmd>();
        // 恒活句柄:stop 永远无人置位,demo 线程只因 APP_SHUTDOWN 退出
        let handle = Arc::new(WorkerHandle {
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        host.set_session(Some(SessionHandle { handle, cmd_tx }));
        *slot = Some(demo_tx);
        drop(slot);
        spawn_singleton(host.msg_tx(), cmd_rx, demo_rx, host.worker_shared());
    }

    /// 向单例线程发内部命令(单例未拉起时无事可做)
    fn send_demo_cmd(&self, cmd: DemoCmd) {
        if let Some(tx) = self.demo_tx.lock().unwrap().as_ref() {
            let _ = tx.send(cmd); // worker 命令通道尽力而为语义
        }
    }
}

impl RttBackend for DemoBackend {
    fn id(&self) -> &'static str {
        BACKEND_DEMO
    }

    fn display_name(&self) -> &'static str {
        "Demo 演示数据源"
    }

    fn description(&self) -> &'static str {
        "内置虚拟数据源,无 J-Link 设备即可体验/测试"
    }

    fn is_virtual(&self) -> bool {
        true
    }

    fn caps(&self) -> BackendCaps {
        BackendCaps {
            hardware_probes: false,
            reset_reconnect: false,
        }
    }

    /// 演示设备库(/api/devices 种子与 /api/connect 校验同源)
    fn initial_devices(&self) -> Vec<String> {
        vec![
            "STM32F103C8".into(),
            "STM32F030C8".into(),
            "STM32G474VET6".into(),
            "STM32H743ZIT6".into(),
            "GD32F303CCT6".into(),
            "CH32V307VCT6".into(),
        ]
    }

    /// 被选中时种子设备表(切后端场景,阶段二启用)
    fn on_selected(&self, host: &dyn Host) {
        host.set_device_names(self.initial_devices());
    }

    /// 切走时清理连接态(手动断开后不自动重连,由 demo 状态机保证);
    /// 单例保留,再次选中时复用
    fn on_deselected(&self, _host: &dyn Host) {
        self.send_demo_cmd(DemoCmd::Disconnect);
    }

    /// 幂等虚拟连接:恒 Ok(已连接再 connect 与现状一样返回 200,由线程
    /// 幂等忽略——不得照抄 jlink 的存活 409)。首次 connect 懒启动单例。
    fn can_connect(&self, _host: &dyn Host) -> Result<(), String> {
        Ok(())
    }

    fn connect(&self, host: &dyn Host, _params: &ConnectParams) -> Result<(), String> {
        self.ensure_singleton(host);
        self.send_demo_cmd(DemoCmd::Connect);
        Ok(())
    }

    fn disconnect(&self, _host: &dyn Host) {
        self.send_demo_cmd(DemoCmd::Disconnect);
    }

    /// 复位目标(demo 无真实目标:日志行 + 短暂 State(false→true));
    /// 未连接 409(保住原 api_reset demo 分支的 409 语义,线程内
    /// 「未连接静默忽略」分支因此不再可达)
    fn reset_in_place(&self, host: &dyn Host) -> Result<(), String> {
        if !host.connected() {
            return Err("未连接".into());
        }
        if let Some(s) = host.session() {
            let _ = s.cmd_tx.send(WorkerCmd::Reset);
        }
        Ok(())
    }

    /// demo 不支持重连式复位:web 层按 caps().reset_reconnect == false 先行
    /// 回落 in-place(与现状「demo 忽略 mode」逐字等价),本方法不可达;
    /// 防御性返回 Err
    fn reset_reconnect(&self, _host: Arc<dyn Host>, _params: &ConnectParams) -> Result<(), String> {
        Err("demo 后端不支持重连式复位".into())
    }

    /// 电源输出(demo 无法真供电:日志行反馈);未连接 409(与原 api_power
    /// demo 分支同语义,线程内「未连接无条件打日志」分支因此不再可达)
    fn power(&self, host: &dyn Host, on: bool) -> Result<(), String> {
        if !host.connected() {
            return Err("未连接".into());
        }
        if let Some(s) = host.session() {
            let _ = s.cmd_tx.send(WorkerCmd::Power(on));
        }
        Ok(())
    }

    /// --demo-log 强制形态的启动自动连接:懒启动单例即可——线程**启动即
    /// 连接**(启动首连是自动周期的一部分),首条消息 State(true) 由线程
    /// 自己发出,tick 据此统一完成 connected/session_start/标记行/托盘回调。
    /// 刻意不发内部 Connect 命令:Connect 属手动干预,会永久挂起 20s/3s
    /// 自动周期(与现状启动种子的语义一致)
    fn auto_connect_at_startup(&self, host: &dyn Host) {
        self.ensure_singleton(host);
    }
}

/// 演示设备信息(每次 false→true 迁移随 State(true) 发送;core_id 空串,
/// 经 web.rs device_info_json 单点映射成 /api/status device 的 8 字段,与原
/// demo_device_json 演示值逐字节一致)
fn demo_device_info() -> DeviceInfo {
    DeviceInfo {
        firmware: "J-Link V11 demo".into(),
        hardware: "V11".into(),
        serial: "600788888".into(),
        core_name: "Cortex-M4".into(),
        core_id: String::new(),
        core_cpu: "ARM 32-Bit".into(),
        target: "STM32F103C8".into(),
        iface: "SWD".into(),
        speed_khz: 4000,
    }
}

/// false→true 迁移的统一消息:State(true) + 演示设备信息
fn emit_connected(msg_tx: &mpsc::Sender<WorkerMsg>) {
    let _ = msg_tx.send(WorkerMsg::State(true, "● 已连接 (demo)".into()));
    let _ = msg_tx.send(WorkerMsg::DeviceInfo(demo_device_info()));
}

/// 启动演示数据单例线程:中英混排 + emoji + ANSI 颜色
/// (行号青色、心跳值绿、每 10 条一次红色 ERROR 样式,覆盖解析的主路径)。
/// 同时模拟连接/断开循环(启动即连接,每 20s 断开 3s 再重连;手动干预后
/// 周期挂起,见模块头)——State(true/false) 消息会驱动 tick 里的连接分支
/// (统计清零/自动标记),demo 冒烟因此覆盖真机连接路径,不再只测数据流。
fn spawn_singleton(
    msg_tx: mpsc::Sender<WorkerMsg>,
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    demo_rx: mpsc::Receiver<DemoCmd>,
    ws: WorkerShared,
) {
    let _ = T0.set(Instant::now());
    std::thread::spawn(move || {
        let hex_rx = ws.hex_rx;
        let frame_timeout_ms = ws.frame_timeout_ms;
        let mut i: u64 = 0;
        // 启动即连接(与旧 web.rs 的启动种子一致,首屏即有数据/可发送)
        let mut connected = true;
        // 手动干预后置位:自动断开/重连周期挂起,连接态完全由手动命令驱动
        let mut manual = false;
        // 自动周期倒计时(Some = 倒计时中;None = 无自动断开)
        let mut disconnect_at = Some(Instant::now() + Duration::from_secs(AUTO_SESSION_SECS));
        let mut reconnect_at: Option<Instant> = None;
        // 重置序列:到点发 State(true)(期间 connected=false,数据暂停)
        let mut reset_at: Option<Instant> = None;
        // 断帧判定(与真机 worker 同款:相邻数据到达间隔超过断帧超时 → FrameEnd)
        let mut last_rx = Instant::now();
        let mut frame_open = false;
        // 启动首连消息(线程的第一条消息 = State(true) + 设备信息):替代
        // 旧 serve() 手工预置段,由 tick 的 State 分支统一完成 connected/
        // session_start/标记行/托盘回调(axum bind 前 tick 必已消化)
        emit_connected(&msg_tx);
        loop {
            if APP_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            // 1) 命令消费:数据面(Reset/Power/Send)+ 连接面(Connect/Disconnect)
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    WorkerCmd::Reset => {
                        if connected {
                            let _ =
                                msg_tx.send(WorkerMsg::Log("── 目标已重置 (demo) ──\r\n".into()));
                            let _ =
                                msg_tx.send(WorkerMsg::State(false, "● 目标重置中 (demo)".into()));
                            connected = false;
                            reset_at = Some(Instant::now() + Duration::from_millis(RESET_GAP_MS));
                            disconnect_at = None;
                            reconnect_at = None;
                            frame_open = false;
                        }
                    }
                    WorkerCmd::Power(on) => {
                        // demo 无法真供电:日志行反馈即可
                        let _ = msg_tx.send(WorkerMsg::Log(format!(
                            "── J-Link 19 脚供电:{} (demo) ──\r\n",
                            if on { "开" } else { "关" }
                        )));
                    }
                    // web 层已计数+回显,demo 不重复
                    WorkerCmd::Send(_) => {}
                }
            }
            while let Ok(cmd) = demo_rx.try_recv() {
                match cmd {
                    DemoCmd::Connect => {
                        manual = true;
                        disconnect_at = None;
                        reconnect_at = None;
                        reset_at = None;
                        if !connected {
                            emit_connected(&msg_tx);
                            connected = true;
                            last_rx = Instant::now();
                            frame_open = false;
                        }
                    }
                    DemoCmd::Disconnect => {
                        manual = true;
                        disconnect_at = None;
                        reconnect_at = None;
                        reset_at = None;
                        if connected {
                            let _ = msg_tx.send(WorkerMsg::State(false, "● 未连接 (demo)".into()));
                            connected = false;
                        }
                        // 手动断开后不自动重连(忠实模拟真机)
                    }
                }
            }
            // 2) 状态迁移(手动模式下无任何自动迁移)
            let now = Instant::now();
            if let Some(t) = reset_at {
                if now >= t {
                    emit_connected(&msg_tx);
                    connected = true;
                    reset_at = None;
                    last_rx = now;
                    frame_open = false;
                    if !manual {
                        disconnect_at = Some(now + Duration::from_secs(AUTO_SESSION_SECS));
                    }
                }
            } else if connected {
                if let Some(t) = disconnect_at {
                    if now >= t {
                        let _ = msg_tx.send(WorkerMsg::State(false, "● 未连接 (demo)".into()));
                        connected = false;
                        disconnect_at = None;
                        if !manual {
                            reconnect_at = Some(now + Duration::from_secs(AUTO_RECONNECT_SECS));
                        }
                    }
                }
            } else if !manual {
                if let Some(t) = reconnect_at {
                    if now >= t {
                        emit_connected(&msg_tx);
                        connected = true;
                        reconnect_at = None;
                        disconnect_at = Some(now + Duration::from_secs(AUTO_SESSION_SECS));
                        last_rx = now;
                        frame_open = false;
                    }
                }
            }
            // 3) 帧间隔判定 + 数据:仅连接态产生数据(断开无 RTT 数据,与真机一致)
            let now = Instant::now();
            let to = Duration::from_millis(frame_timeout_ms.load(Ordering::Relaxed) as u64);
            if connected {
                if frame_open && now.duration_since(last_rx) > to {
                    // 上一帧到此结束;新块马上开启新帧(frame_open 统一在下面置位)
                    let _ = msg_tx.send(WorkerMsg::FrameEnd);
                }
                let value = if i % 10 == 9 {
                    format!("\x1b[31m{i}\x1b[0m")
                } else {
                    format!("\x1b[32m{i}\x1b[0m")
                };
                let level = if i % 10 == 9 {
                    // 前景红(31)而非背景红(41):ansi.rs 暂不解析背景 SGR,
                    // 背景码会被吞掉导致 ERR 无色
                    " \x1b[31mERR\x1b[0m"
                } else {
                    ""
                };
                // Block 而非 Log:demo 模拟的是设备输出流(计入 RX 统计),
                // Log 是 J-Link 横幅语义,不参与统计
                let block = format!(
                    "\x1b[36m[demo {i:04}]\x1b[0m Heartbeat: {value} 😊🍟❤ 心跳 中文 English mixed padding text{level}\r\n"
                );
                // HEX 接收下沉(与原 web.rs tick demo 分支逐字节同款转换):
                // hexRx 开启时把块文本按 UTF-8 字节转十六进制大写再上屏,
                // 演示 HEX 接收效果(热切换动态生效)
                let block = if hex_rx.load(Ordering::Relaxed) {
                    text_to_hex_upper(&block)
                } else {
                    block
                };
                let _ = msg_tx.send(WorkerMsg::Block(block));
                if let Some(t0) = T0.get() {
                    eprintln!("[tx] i={i} t={}us", t0.elapsed().as_micros());
                }
                frame_open = true;
                last_rx = now;
                i += 1;
            } else if frame_open && now.duration_since(last_rx) > to {
                // 数据停了:补发帧结束(收尾半帧;自动断帧关闭时 pump 侧忽略)
                let _ = msg_tx.send(WorkerMsg::FrameEnd);
                frame_open = false;
            }
            std::thread::sleep(Duration::from_millis(INTERVAL_MS));
        }
    });
}
