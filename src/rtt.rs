//! RTT 工作线程——probe-rs 纯 Rust 后端(实验分支 feat/probe-rs-backend)。
//!
//! 与旧 JLinkARM.dll FFI 后端的语义对齐关系:
//!   - 枚举调试器:EMU_GetList → `Lister::list_all()`(nusb 扫 USB,无需打开设备)
//!   - 选定序列号:SelectByUSBSN → 直接按 serial_number 挑选 `DebugProbeInfo` 再 open
//!   - 连接序列:RTT START→TIF→速度→Device→Connect → `select_protocol`→`set_speed`→
//!     `attach`(probe-rs 自带目标库,默认 attach 不复位、不下载)
//!   - RTT 读/写:RTTERMINAL_Read/Write → `Rtt::attach` 后 Up/Down 通道 `read`/`write`
//!   - 复位:Reset+Go 两步 → `Core::reset()`(复位并继续运行,一步到位)
//!   - 电源输出:SupplyPower → probe-rs 通用 API 未暴露(J-Link 驱动内部有 SetKsPower
//!     能力但 `DebugProbe` trait 不通),命令收到后如实回 Log 提示
//!
//! 注意:probe-rs 的 `attach(self, …)` 按值消耗 Probe 且失败不归还,重试必须
//! 整个「open → 协议/速度 → attach」重来一遍(见 `open_and_attach`)。
//!
//! WorkerMsg/WorkerCmd/WorkerConfig/WorkerHandle/spawn 的签名与语义保持完全不变,
//! 装配层 main.rs 零改动。

use anyhow::Context;
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;
use probe_rs::probe::{DebugProbeInfo, WireProtocol};
use probe_rs::rtt::Rtt;
use probe_rs::{Core, CoreType, Permissions, Session};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

pub enum WorkerMsg {
    /// 独立提示行(横幅等,自带换行)
    Log(String),
    /// 一次 RTT read 的解码输出(帧边界由 worker 按间隔判定后以 FrameEnd 发出)
    Block(String),
    /// 帧结束标记:相邻数据到达间隔超过断帧超时,worker 判定一帧结束。
    /// 断帧判定在 worker(5ms 轮询的真实时间戳)进行,UI 刷新粒度不影响精度。
    FrameEnd,
    /// 连接/断开的**最终结果**:true=已连接,false=已断开或失败。
    /// 中间进度用 Progress(只刷状态栏文字,绝不改变连接标志,
    /// 否则按钮会在 连接/断开 之间来回跳)。
    State(bool, String),
    /// 连接过程的中间进度(只更新状态栏文字)
    Progress(String),
    /// 连接成功后采集的设备信息(能采的采集,采不到的留空串,UI 显示 "—")
    DeviceInfo(DeviceInfo),
    /// 目标设备候选名(来自 probe-rs 内置目标库,连接时刷新,用于目标设备下拉)
    DeviceNames(Vec<String>),
    /// 本机已接入的调试器列表 (序列号, 显示名),后台线程 + 每次连接时刷新
    JLinks(Vec<(u32, String)>),
    /// worker 线程已完全退出(含 session 清理),UI 收到后才允许再次连接,
    /// 防止上一个 worker 还阻塞在 attach() 时 spawn 新 worker 并发抢 RTT。
    Exited,
}

/// 连接成功后从调试器 / 目标采集到的信息(UI 设备信息区展示)。
/// probe-rs 不提供 J-Link 固件版本等 DLL 专有信息,对应字段留空串(UI 显示 "—")。
pub struct DeviceInfo {
    pub firmware: String,
    pub hardware: String,
    pub serial: String,
    pub core_name: String,
    pub core_id: String,
    pub core_cpu: String,
    pub target: String,
    pub iface: String,
    pub speed_khz: u32,
}

/// UI → worker 命令。原来只有发数据一种,引入电源输出后升级为枚举协议,
/// 避免在字符串上做前缀解析(易错且没有类型保障)。
pub enum WorkerCmd {
    /// 向 RTT 通道写原始字节(行尾已含;文本模式为 UTF-8 编码,HEX 模式为解析后的字节)
    Send(Vec<u8>),
    /// 电源输出开关(J-Link 19 脚):true=开 false=关。
    /// probe-rs 后端不支持,收到后回 Log 提示
    Power(bool),
    /// 复位目标并恢复运行(复位后重挂 RTT:固件重新初始化后控制块可能移动)
    Reset,
}

/// 应用退出信号:主窗口关闭后置位,worker 循环(包括阻塞中的轮询间隔)
/// 检测到后尽快退出,否则非 daemon 线程会让进程在窗口关闭后残留。
pub static APP_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// worker 生命周期句柄:stop 请求退出,alive 反映线程是否还在。
pub struct WorkerHandle {
    pub stop: AtomicBool,
    pub alive: AtomicBool,
}

/// 一次连接的全部参数(UI 收集 → worker 使用)
pub struct WorkerConfig {
    pub chip: String,
    pub iface_index: usize,
    pub speed_khz: u32,
    pub channel: u32,
    /// 断帧间隔(毫秒)共享变量,UI 侧运行时可改
    pub frame_timeout_ms: Arc<AtomicU32>,
    /// 用户在「J-Link」下拉选中的调试器序列号;None = 自动选(单机场景取第一台)
    pub selected_sn: Option<u32>,
}

/// 启动 RTT 工作线程:枚举/打开调试器 → attach 目标 → 挂 RTT → 循环读通道。
pub fn spawn(
    config: WorkerConfig,
    tx: mpsc::Sender<WorkerMsg>,
    cmd_rx: mpsc::Receiver<WorkerCmd>,
) -> Arc<WorkerHandle> {
    let handle = Arc::new(WorkerHandle {
        stop: AtomicBool::new(false),
        alive: AtomicBool::new(true),
    });
    let h = handle.clone();
    thread::spawn(move || {
        let result = run(&config, &tx, &cmd_rx, &h.stop);
        if let Err(e) = result {
            let _ = tx.send(WorkerMsg::State(false, format!("● 错误: {e}")));
        }
        h.alive.store(false, Ordering::Relaxed);
        // 无论成败,线程结束必须广播 Exited,解锁 UI 的"再连接"门闩
        let _ = tx.send(WorkerMsg::Exited);
    });
    handle
}

fn run(
    config: &WorkerConfig,
    tx: &mpsc::Sender<WorkerMsg>,
    cmd_rx: &mpsc::Receiver<WorkerCmd>,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let (session, rtt) = connect_target(config, tx, stop)?;
    rtt_read_loop(session, rtt, config, tx, cmd_rx, stop);
    Ok(())
}

/// 连接序列:枚举调试器 → 按序列号选定 → open → 协议/速度 → attach(默认不复位)
/// → 挂 RTT 控制块 → State(true) + DeviceInfo。
/// 成功返回 (Session, Rtt);Rtt 不借用 Session(只存控制块地址与通道配置),
/// 两者可分别持有。失败路径 drop Session 触发清理后返回 Err(spawn 统一转
/// State(false)),与旧后端「失败自行 close 再 bail」等价。
fn connect_target(
    config: &WorkerConfig,
    tx: &mpsc::Sender<WorkerMsg>,
    stop: &AtomicBool,
) -> anyhow::Result<(Session, Rtt)> {
    let WorkerConfig { chip, iface_index, speed_khz, selected_sn, .. } = config;
    let _ = tx.send(WorkerMsg::Progress("● 正在初始化 probe-rs…".into()));

    // probe-rs 内置目标库:既用于 attach,也用于设备下拉候选(约 1.7 万型号,
    // 与 J-Link DLL 设备库同量级)。构建一次,后面 attach_with_registry 复用。
    let registry = Registry::from_builtin_families();
    let mut names: Vec<String> = registry
        .families()
        .iter()
        .flat_map(|f| f.variants.iter().map(|c| c.name.clone()))
        .collect();
    names.sort_by_key(|a| a.to_uppercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let _ = tx.send(WorkerMsg::Log(format!(
        "[probe-rs] 内置设备库 {} 个型号\r\n",
        names.len()
    )));
    let _ = tx.send(WorkerMsg::DeviceNames(names));

    // 枚举本机调试器(纯 USB 扫描,无需打开设备)
    let probes = Lister::new().list_all();
    let jlinks: Vec<(u32, String)> = probes
        .iter()
        .filter_map(|p| {
            let sn: u32 = p.serial_number.as_deref()?.parse().ok()?;
            Some((sn, p.identifier.clone()))
        })
        .collect();
    let _ = tx.send(WorkerMsg::JLinks(jlinks.clone()));

    // 调试器选定:用户下拉指定优先;未指定但本机有调试器时自动选第一台。
    // SN 比对必须数值化(见 sn_matches):J-Link 的 USB 序列号带前导零
    let chosen: &DebugProbeInfo = match selected_sn {
        Some(sn) => probes
            .iter()
            .find(|p| sn_matches(p.serial_number.as_deref(), *sn))
            .with_context(|| {
                format!("找不到序列号 {sn} 的调试器(可能已拔出,请重新选择);当前接入 {jlinks:?}")
            })?,
        None => probes.first().with_context(|| {
            "未发现任何调试器,请确认已插入 J-Link(或 probe-rs 支持的其他调试器)"
        })?,
    };
    if let Some(sel) = selected_sn {
        let _ = tx.send(WorkerMsg::Log(format!("[probe-rs] 已选定序列号 {sel}\r\n")));
    }
    let probe_name = chosen.identifier.clone();
    // 显示用 SN 与 UI 下拉(JLinks 协议的 u32)一致:去前导零;非纯数字才用原始串
    let sn = chosen
        .serial_number
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
        .map_or_else(
            || chosen.serial_number.clone().unwrap_or_else(|| "未知".into()),
            |v| v.to_string(),
        );

    // 接口协议(0=SWD 1=JTAG,与旧后端下拉语义一致)
    let iface_name = if *iface_index == 0 { "SWD" } else { "JTAG" };
    let wire = if *iface_index == 0 { WireProtocol::Swd } else { WireProtocol::Jtag };

    // attach:默认 Normal 模式——不复位、不 halt、绝不触碰 flash(纯调试连接)。
    // 第一次失败自动重试而非回退 UI 状态,避免按钮"断开→连接→断开"来回跳;
    // 重试必须重开设备(attach 按值消耗 Probe,失败不归还)
    let mut session: Option<Session> = None;
    let mut last_err = None;
    for attempt in 0..3 {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match open_and_attach(chosen, wire, *speed_khz, chip, &registry) {
            Ok(s) => {
                session = Some(s);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    let _ = tx.send(WorkerMsg::Progress(format!(
                        "● 连接中…(第 {} 次重试)",
                        attempt + 1
                    )));
                    thread::sleep(Duration::from_millis(400));
                }
            }
        }
    }
    let mut session = match session {
        Some(s) => s,
        None if stop.load(Ordering::Relaxed) => anyhow::bail!("连接已中止"),
        None => {
            let e = last_err.unwrap_or_else(|| anyhow::anyhow!("未知错误"));
            return Err(e).with_context(|| {
                format!("连接目标 {chip} 失败(已重试 3 次);请检查芯片型号/接线/供电,或尝试降低速率(高速率在 Cortex-M0 上可能不稳定)")
            });
        }
    };

    // 挂 RTT 控制块:全 RAM 扫描 "SEGGER RTT"(F030C8 只有 8KB RAM,扫描毫秒级)。
    // 固件刚复位/延迟初始化时控制块可能还没写好,多试几轮。
    // 失败路径无需手动 detach:0.32 起 Session 清理全部在 Drop 内完成
    let mut core = session
        .core(0)
        .map_err(|e| anyhow::anyhow!("打开核心 0 失败:{e}"))?;
    let rtt = attach_rtt(&mut core, 6, Duration::from_millis(300)).map_err(|e| {
        anyhow::anyhow!(
            "已连接 {chip},但找不到 RTT 控制块({e})。请确认固件已初始化 RTT \
             (SEGGER RTT 控制块须驻留 RAM,且上电后至少跑过一次初始化)"
        )
    })?;

    let _ = tx.send(WorkerMsg::State(
        true,
        format!("● 已连接 ({chip}, {iface_name}, {speed_khz}kHz, SN {sn})"),
    ));
    // 连接成功后采集设备信息;probe-rs 拿不到的字段留空串,UI 显示 "—"
    let _ = tx.send(WorkerMsg::DeviceInfo(DeviceInfo {
        firmware: String::new(), // J-Link 固件版本为 DLL 专有,probe-rs 不提供
        hardware: probe_name,    // 调试器产品名(USB product string),如 "J-Link PLUS"
        serial: sn,
        core_name: core_type_name(core.core_type()),
        core_id: String::new(), // DP IDCODE 未被 probe-rs 通用 API 暴露
        core_cpu: String::new(),
        target: chip.clone(),
        iface: iface_name.into(),
        speed_khz: *speed_khz,
    }));
    // 结束 core 对 session 的借用,Session 才能移交给读循环(读循环内重新 core(0))
    drop(core);
    Ok((session, rtt))
}

/// 单次「打开调试器 → 协议/速度 → attach」全流程。任何一步失败都会把设备
/// 完整释放(Probe drop → nusb 释放 USB 接口)后返回错误。
fn open_and_attach(
    chosen: &DebugProbeInfo,
    wire: WireProtocol,
    speed_khz: u32,
    chip: &str,
    registry: &Registry,
) -> anyhow::Result<Session> {
    // 打开调试器。Windows 下 SEGGER 官方驱动与 probe-rs 的 WinUSB 访问互斥,
    // 打不开时把 Zadig 提示原样带给用户(这是本分支最大的部署前提)
    let mut probe = chosen.open().map_err(|e| {
        anyhow::anyhow!(e).context(format!(
            "打开调试器 {chosen} 失败。Windows 下 probe-rs 需要 WinUSB 驱动:\
             用 Zadig(https://zadig.akeo.ie/)把该调试器的驱动从 SEGGER 换成 WinUSB \
             ——注意这会使 SEGGER 官方工具无法再直接使用该设备"
        ))
    })?;
    probe
        .select_protocol(wire)
        .with_context(|| format!("该调试器不支持 {wire:?} 接口(或当前模式下不可用)"))?;
    probe
        .set_speed(speed_khz)
        .with_context(|| format!("设置速率 {speed_khz}kHz 失败"))?;
    probe.attach_with_registry(chip, Permissions::default(), registry).with_context(|| {
        format!("attach 目标 {chip} 失败(probe-rs 默认模式:不复位、不下载)")
    })
}

/// RTT 读循环:5ms 轮询读 Up 通道 → 帧间隔判定 → 透传块;同时消化命令管道
/// (发送/复位/电源)。退出(停止/关窗/读失败)时按依赖顺序 drop,
/// Session 的 Drop 链完成清理,并回报未连接。
fn rtt_read_loop(
    mut session: Session,
    mut rtt: Rtt,
    config: &WorkerConfig,
    tx: &mpsc::Sender<WorkerMsg>,
    cmd_rx: &mpsc::Receiver<WorkerCmd>,
    stop: &AtomicBool,
) {
    let WorkerConfig { channel, frame_timeout_ms, .. } = config;
    let channel = *channel as usize;

    // Core 借用 session 直到本函数结束;循环里只碰 core / rtt。
    // 各失败路径直接 return:Session/Core 的 Drop 链自动完成清理(0.32 无 detach)
    let mut core = match session.core(0) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(WorkerMsg::State(false, format!("● 错误: 打开核心失败: {e}")));
            return;
        }
    };

    let mut buf = [0u8; 4096];
    // 跨块 UTF-8 增量解码:emoji 4 字节可能被 RTT 读块边界切断
    let mut carry: Vec<u8> = Vec::new();
    // 帧边界判定(在 worker 用真实时间戳,不受 UI 刷新粒度影响):
    // 相邻两次数据到达的间隔超过断帧超时 → 上一帧结束(FrameEnd)
    let mut last_rx = Instant::now();
    let mut frame_open = false;
    while !stop.load(Ordering::Relaxed) && !APP_SHUTDOWN.load(Ordering::Relaxed) {
        let read = match rtt.up_channel(channel) {
            Some(up) => up.read(&mut core, &mut buf),
            None => {
                let n = rtt.up_channels().len();
                let _ = tx.send(WorkerMsg::State(
                    false,
                    format!("● 错误: Up 通道 {channel} 不存在(固件只注册了 {n} 个)"),
                ));
                return;
            }
        };
        let now = Instant::now();
        let to = Duration::from_millis(frame_timeout_ms.load(Ordering::Relaxed) as u64);
        match read {
            Ok(0) => {}
            Ok(n) => {
                if frame_open && now.duration_since(last_rx) > to {
                    // 上一帧到此结束;新块马上开启新帧(frame_open 统一在下面置位)
                    let _ = tx.send(WorkerMsg::FrameEnd);
                }
                // ANSI 转义序列原样透传,由 UI 端 vte 解析着色(本层不再剥掉)
                let text = decode_utf8_incremental(&mut carry, &buf[..n]);
                let _ = tx.send(WorkerMsg::Block(text));
                frame_open = true;
                last_rx = now;
            }
            // 目标复位/固件重初始化导致读指针跳变:重挂 RTT 后继续
            Err(probe_rs::rtt::Error::ReadPointerChanged) => {
                let _ = tx.send(WorkerMsg::Log(
                    "[probe-rs] RTT 读指针跳变(目标可能复位过),尝试重挂…\r\n".into(),
                ));
                rtt = match attach_rtt(&mut core, 5, Duration::from_millis(300)) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::State(
                            false,
                            format!("● RTT 重挂失败({e}),已断开"),
                        ));
                        return;
                    }
                };
            }
            Err(e) => {
                let _ = tx.send(WorkerMsg::State(
                    false,
                    format!("● RTT 读取失败({e}),已断开"),
                ));
                return;
            }
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                WorkerCmd::Send(data) => match rtt.down_channel(channel) {
                    Some(down) => {
                        if let Err(e) = down.write(&mut core, &data) {
                            let _ = tx.send(WorkerMsg::Log(format!("[发送失败: {e}]\r\n")));
                        }
                    }
                    None => {
                        let _ = tx.send(WorkerMsg::Log(format!(
                            "[发送失败: Down 通道 {channel} 不存在]\r\n"
                        )));
                    }
                },
                WorkerCmd::Reset => {
                    // 复位并运行:probe-rs Core::reset() 一步到位(复位后继续执行,
                    // 等价旧后端 Reset+Go 两步)。失败只报告不打断读循环
                    match core.reset() {
                        Ok(()) => {
                            // 固件重新初始化 RTT 控制块需要时间:稍候再重挂
                            thread::sleep(Duration::from_millis(200));
                            rtt = match attach_rtt(&mut core, 8, Duration::from_millis(250)) {
                                Ok(r) => {
                                    let _ = tx.send(
                                        WorkerMsg::Log("[目标已复位,RTT 已重挂]\r\n".into()),
                                    );
                                    r
                                }
                                Err(e) => {
                                    // 保留旧 rtt 继续读:控制块地址通常不变,
                                    // 读坏了会走 ReadPointerChanged 重挂路径
                                    let _ = tx.send(WorkerMsg::Log(format!(
                                        "[目标已复位,但 RTT 重挂失败: {e}]\r\n"
                                    )));
                                    rtt
                                }
                            };
                        }
                        Err(e) => {
                            let _ = tx.send(WorkerMsg::Log(format!("[复位失败: {e}]\r\n")));
                        }
                    }
                }
                WorkerCmd::Power(on) => {
                    // J-Link 19 脚供电在 probe-rs 的 J-Link 驱动内部有实现
                    // (SetKsPower),但 DebugProbe 通用 trait 未暴露,通用 Probe
                    // 拿不到具体驱动实例——如实报告,不假装成功
                    let _ = tx.send(WorkerMsg::Log(format!(
                        "[probe-rs 后端不支持电源输出({}),命令已忽略]\r\n",
                        if on { "开" } else { "关" }
                    )));
                }
            }
        }
        // 5ms 轮询:自动断帧按"相邻数据间隔"判定,轮询间隔决定间隔测量的精度上限
        thread::sleep(Duration::from_millis(5));
    }

    // 清理:显式按依赖顺序 drop(先 rtt/core 再 session),Drop 链完成
    // 断点清理与核心去配置(debug_core_stop),0.32 起 Session 无 detach 方法
    drop(rtt);
    drop(core);
    drop(session);
    let _ = tx.send(WorkerMsg::State(false, "● 未连接".into()));
}

/// USB 序列号(字符串,可能带前导零,如 "000602717758")与 UI 协议里的 u32 SN
/// 是否指向同一设备。必须数值化比较——字符串直比会因前导零永久失配
/// (JLinks/selected_sn 协议是 u32,u32→string 不保留前导零)。
/// 非 u32 序列号(某些 CMSIS-DAP 探针)不参与数值匹配,返回 false。
fn sn_matches(serial: Option<&str>, sn: u32) -> bool {
    serial.and_then(|s| s.parse::<u32>().ok()) == Some(sn)
}

/// 反复尝试挂 RTT 控制块(全 RAM 扫描);全部失败返回最后一次错误
fn attach_rtt(
    core: &mut Core,
    attempts: usize,
    delay: Duration,
) -> Result<Rtt, probe_rs::rtt::Error> {
    let mut last = None;
    for _ in 0..attempts {
        match Rtt::attach(core) {
            Ok(rtt) => return Ok(rtt),
            Err(e) => {
                last = Some(e);
                thread::sleep(delay);
            }
        }
    }
    Err(last.expect("attempts > 0 时必有错误"))
}

/// probe-rs 核心类型 → 显示名。DAP 层只能到 ARMv?-M 粒度,区分不出 M0/M0+,
/// 如实按架构族显示
fn core_type_name(t: CoreType) -> String {
    match t {
        CoreType::Armv6m => "Armv6-M (Cortex-M0/M0+/M1)".into(),
        CoreType::Armv7m => "Armv7-M (Cortex-M3)".into(),
        CoreType::Armv7em => "Armv7E-M (Cortex-M4/M7)".into(),
        CoreType::Armv8m => "Armv8-M (Cortex-M23/M33)".into(),
        CoreType::Armv7a => "Armv7-A".into(),
        CoreType::Armv7r => "Armv7-R".into(),
        CoreType::Armv8a => "Armv8-A".into(),
        CoreType::Riscv => "RISC-V (32-bit)".into(),
        CoreType::Riscv64 => "RISC-V (64-bit)".into(),
        CoreType::Xtensa => "Xtensa".into(),
    }
}

/// 跨块安全的 UTF-8 增量解码:
/// - 尾部被截断的多字节序列保留到下次拼接(error_len() == None);
/// - 真正非法的字节序列丢弃并输出替换符(error_len() == Some(n));
/// - 解码循环结束后 carry 只可能是 <4 字节的合法截断尾,超限即垃圾,丢弃。
pub fn decode_utf8_incremental(carry: &mut Vec<u8>, data: &[u8]) -> String {
    carry.extend_from_slice(data);
    let mut out = String::new();
    loop {
        match std::str::from_utf8(carry) {
            Ok(s) => {
                out.push_str(s);
                carry.clear();
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    out.push_str(unsafe { std::str::from_utf8_unchecked(&carry[..valid]) });
                    carry.drain(..valid);
                }
                match e.error_len() {
                    Some(bad_len) => {
                        out.push('\u{FFFD}');
                        carry.drain(..bad_len);
                    }
                    None => break, // 尾部截断,留给下一块
                }
            }
        }
    }
    // 合法截断尾最多 3 字节;超限说明是垃圾数据,丢弃防止无限堆积
    if carry.len() > 3 {
        carry.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sn_matches;

    #[test]
    fn sn_matches_ignores_leading_zeros() {
        // J-Link 实测形态:USB 字符串带前导零,UI 协议(u32)不带
        assert!(sn_matches(Some("000602717758"), 602_717_758));
        assert!(sn_matches(Some("602717758"), 602_717_758));
        assert!(!sn_matches(Some("000609788888"), 602_717_758));
        assert!(!sn_matches(None, 602_717_758));
        // 非 u32 序列号(某些 CMSIS-DAP 探针)不参与数值匹配
        assert!(!sn_matches(Some("rpi-pico"), 0));
    }
}
