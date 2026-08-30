use crate::jlink_dll::{JLinkDll, RTT_CMD_START, RTT_CMD_STOP, TIF_JTAG, TIF_SWD};
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
    /// 连接成功后采集的设备信息(DLL 查询,字段对齐原项目)
    DeviceInfo(DeviceInfo),
    /// J-Link 设备库候选名(启动时后台枚举/磁盘缓存,用于目标设备下拉)
    DeviceNames(Vec<String>),
    /// 本机已接入的 J-Link 调试器列表 (序列号, 显示名),后台线程 + 每次连接时刷新
    JLinks(Vec<(u32, String)>),
    /// worker 线程已完全退出(含 DLL close),UI 收到后才允许再次连接,
    /// 防止上一个 worker 还阻塞在 connect() 时 spawn 新 worker 并发抢 RTT。
    Exited,
}

/// 连接成功后从 J-Link / 目标采集到的信息(UI 设备信息区展示)
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
    /// 电源输出开关(J-Link 19 脚):true=开 false=关
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
    /// 用户在「J-Link」下拉选中的调试器序列号;None = 交给 DLL 自动选(单机场景)
    pub selected_sn: Option<u32>,
    /// 字符集标签("utf-8"/"gbk"/"utf-16le"/"windows-1252"/"ascii");
    /// 未知标签回落 UTF-8。UI 下拉顺序见 [`ENCODINGS`]
    pub encoding: String,
}

/// 字符集下拉表:**与 app.slint 的「字符集」ComboBox 顺序严格一致**。
/// (显示名, encoding_rs 标签)
pub const ENCODINGS: [(&str, &str); 5] = [
    ("UTF-8", "utf-8"),
    ("GBK", "gbk"),
    ("UTF-16 LE", "utf-16le"),
    ("Latin-1", "windows-1252"),
    ("ASCII", "ascii"),
];

/// 字符集增量解码器(encoding_rs 包装):跨读块的多字节边界由 decoder 内部
/// 管理,替换原先手写的 UTF-8 carry 逻辑并额外支持 GBK/UTF-16 等
pub struct CharsetDecoder {
    decoder: encoding_rs::Decoder,
}

impl CharsetDecoder {
    /// 按编码标签构建;未知标签回落 UTF-8
    pub fn for_label(label: &str) -> Self {
        let enc = encoding_rs::Encoding::for_label(label.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);
        let decoder = enc.new_decoder();
        Self { decoder }
    }

    /// 解码一块原始字节(块边界可落在任意多字节字符中间)
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        // encoding_rs 的 decode_to_string 不自动扩容:容量不足时静默丢弃输出,
        // 必须先按"每输入字节至多 3 个输出字节"预足容量
        let cap = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len() * 3 + 4);
        let mut out = String::with_capacity(cap);
        // last=false:保留尾部截断的多字节序列到下一块;非法字节输出 U+FFFD
        let (_, _, _) = self.decoder.decode_to_string(bytes, &mut out, false);
        out
    }
}

/// 启动 RTT 工作线程:加载 DLL → 按验证过的序列连接 → 循环读通道。
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
    let jlink = connect_target(config, tx, stop)?;
    rtt_read_loop(&jlink, config, tx, cmd_rx, stop);
    Ok(())
}

/// 连接序列:加载 DLL → 抑制弹窗 → 选定调试器 → OpenEx → RTT START →
/// TIF/速度/设备名 → Connect(带重试)→ State(true) + DeviceInfo。
/// 成功返回已打开的 JLinkDll;失败路径自行 close 后返回 Err。
fn connect_target(
    config: &WorkerConfig,
    tx: &mpsc::Sender<WorkerMsg>,
    stop: &AtomicBool,
) -> anyhow::Result<JLinkDll> {
    let WorkerConfig { chip, iface_index, speed_khz, selected_sn, .. } = config;
    let _ = tx.send(WorkerMsg::Progress("● 正在加载 JLinkARM.dll…".into()));
    let jlink = JLinkDll::load()?;
    // 抑制 DLL 模态弹窗必须最先做(调试器选择窗/固件升级提示都发生在 Open 内部),
    // 否则 worker 会卡在无人应答的隐藏对话框上
    jlink.disable_dialog_boxes();
    // 刷新 J-Link 下拉列表(用户可能插拔过)
    let emus = jlink.enumerate_emulators();
    let _ = tx.send(WorkerMsg::JLinks(emus.clone()));

    // 调试器选定,绝不让 DLL 自己弹选择窗:用户下拉指定优先;
    // 未指定但本机有 J-Link 时自动选第一台
    let sn_to_use = selected_sn.or_else(|| emus.first().map(|(sn, _)| *sn));
    if let Some(sel) = sn_to_use {
        let rc = jlink.select_by_usb_sn(sel);
        if rc < 0 {
            jlink.close();
            anyhow::bail!("找不到序列号 {sel} 的 J-Link(可能已拔出,请重新选择)");
        }
        // SetHostIF 是关键:实测 V8.24 只靠 SelectByUSBSN 时,无参 Open 仍会弹
        // probe 选择窗并用弹窗里的选择(默认第一台)覆盖我们的选定
        jlink.exec_command(&format!("SetHostIF USB = {sel}"));
        let _ = tx.send(WorkerMsg::Log(format!("[J-Link] 已选定序列号 {sel}\r\n")));
    }

    // 必须用 OpenEx:无参 Open 在多台 J-Link 时会重新弹 probe 选择窗,
    // OpenEx(pylink 同款入口)尊重上面的选定
    jlink.open_ex();

    // Open 之后读到的序列号才是实际打开的设备(Open 前读到的只是选定值)
    let sn = jlink.serial_number();
    if let Some(sel) = sn_to_use {
        if sn != sel {
            let _ = tx.send(WorkerMsg::Log(format!(
                "[J-Link] 警告:选定 {sel} 但实际打开 {sn},请重新选择或拔插\r\n"
            )));
        }
    }
    let _ = tx.send(WorkerMsg::Log(format!(
        "[J-Link] 序列号 {sn},建立连接…\r\n"
    )));

    // 序列遵循原项目验证过的 DLL 状态机要求:RTT START 必须在 connect 之前
    let tif = if *iface_index == 0 { TIF_SWD } else { TIF_JTAG };
    jlink.rtt_control(RTT_CMD_START);
    jlink.select_tif(tif);
    jlink.set_speed(*speed_khz as i32);
    let resp = jlink.exec_command(&format!("Device = {chip}"));
    if !resp.trim().is_empty() {
        let _ = tx.send(WorkerMsg::Log(format!("[J-Link] {resp}\r\n")));
    }
    // 目标连接第一次常会失败(DLL 内部状态机/目标未就绪),自动重试而非回退 UI 状态,
    // 避免按钮"断开→连接→断开"来回跳
    let mut rc = jlink.connect();
    for attempt in 1..4 {
        if rc >= 0 || stop.load(Ordering::Relaxed) {
            break;
        }
        let _ = tx.send(WorkerMsg::Progress(format!(
            "● 连接中…(第 {attempt} 次重试)"
        )));
        thread::sleep(Duration::from_millis(400));
        rc = jlink.connect();
    }
    if rc < 0 {
        jlink.close();
        anyhow::bail!(
            "J-Link 连接目标失败 (错误码 {rc});请检查芯片型号/接线/供电,或尝试降低速率(高速率在 Cortex-M0 上可能不稳定)"
        );
    }

    let iface_name = if tif == TIF_SWD { "SWD" } else { "JTAG" };
    let _ = tx.send(WorkerMsg::State(
        true,
        format!("● 已连接 ({chip}, {iface_name}, {speed_khz}kHz, SN {sn})"),
    ));
    // 连接成功后采集设备信息(字段对齐原项目);单字段失败返回空串,UI 显示 "—"
    let _ = tx.send(WorkerMsg::DeviceInfo(DeviceInfo {
        firmware: jlink.firmware_version(),
        hardware: jlink.hardware_version(),
        serial: sn.to_string(),
        core_name: jlink.core_name(),
        core_id: jlink.core_id(),
        core_cpu: jlink.core_cpu(),
        target: chip.clone(),
        iface: iface_name.into(),
        speed_khz: *speed_khz,
    }));
    Ok(jlink)
}

/// RTT 读循环:5ms 轮询读通道 → 帧间隔判定 → 透传块;同时消化命令管道
/// (发送/电源)。退出(停止/关窗/读失败)时清理 RTT 与 DLL 并回报未连接。
fn rtt_read_loop(
    jlink: &JLinkDll,
    config: &WorkerConfig,
    tx: &mpsc::Sender<WorkerMsg>,
    cmd_rx: &mpsc::Receiver<WorkerCmd>,
    stop: &AtomicBool,
) {
    let WorkerConfig { channel, frame_timeout_ms, encoding, .. } = config;

    let mut buf = [0u8; 4096];
    // 按用户选定字符集增量解码:跨读块的多字节边界(emoji 4 字节/GBK 2 字节
    // 被块切断)由 decoder 内部管理
    let mut decoder = CharsetDecoder::for_label(encoding);
    // 帧边界判定(在 worker 用真实时间戳,不受 UI 刷新粒度影响):
    // 相邻两次数据到达的间隔超过断帧超时 → 上一帧结束(FrameEnd)
    let mut last_rx = Instant::now();
    let mut frame_open = false;
    while !stop.load(Ordering::Relaxed) && !APP_SHUTDOWN.load(Ordering::Relaxed) {
        let n = jlink.rtt_read(*channel as i32, &mut buf);
        let now = Instant::now();
        let to = Duration::from_millis(frame_timeout_ms.load(Ordering::Relaxed) as u64);
        if n > 0 {
            if frame_open && now.duration_since(last_rx) > to {
                // 上一帧到此结束;新块马上开启新帧(frame_open 统一在下面置位)
                let _ = tx.send(WorkerMsg::FrameEnd);
            }
            // ANSI 转义序列原样透传,由 UI 端 vte 解析着色(本层不再剥掉)
            let text = decoder.decode(&buf[..n as usize]);
            let _ = tx.send(WorkerMsg::Block(text));
            frame_open = true;
            last_rx = now;
        } else if frame_open && now.duration_since(last_rx) > to {
            // 数据停了:补发帧结束
            let _ = tx.send(WorkerMsg::FrameEnd);
            frame_open = false;
        } else if n < 0 {
            let _ = tx.send(WorkerMsg::State(false, format!("● RTT 读取失败 ({n}),已断开")));
            jlink.rtt_control(RTT_CMD_STOP);
            jlink.close();
            return;
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                WorkerCmd::Send(data) => {
                    let w = jlink.rtt_write(*channel as i32, &data);
                    if w < 0 {
                        let _ = tx.send(WorkerMsg::Log("[发送失败]\r\n".into()));
                    }
                }
                WorkerCmd::Reset => {
                    // 复位并运行:JLINKARM_Reset 后 CPU 处于 halted,调 Go 才跑起来
                    // (pylink reset(halt=false) 的两步);< 0 视为失败只报告不打断读循环
                    let rc = jlink.reset();
                    if rc < 0 {
                        let _ = tx.send(WorkerMsg::Log(format!("[复位失败 rc={rc}]\r\n")));
                    } else {
                        jlink.go();
                        // 复位后固件重新初始化 RTT 控制块,重挂 RTT 保证继续可读
                        jlink.rtt_control(RTT_CMD_STOP);
                        jlink.rtt_control(RTT_CMD_START);
                        let _ = tx.send(WorkerMsg::Log("[目标已复位,RTT 已重挂]\r\n".into()));
                    }
                }
                WorkerCmd::Power(on) => {
                    // SupplyPower:J-Link 19 脚给目标供电(与原项目 pylink power_on/off 一致)
                    let resp = jlink.exec_command(if on { "SupplyPower = 1" } else { "SupplyPower = 0" });
                    let resp = resp.trim();
                    let suffix = if resp.is_empty() {
                        String::new()
                    } else {
                        format!("({resp})")
                    };
                    let _ = tx.send(WorkerMsg::Log(format!(
                        "[J-Link] 电源输出 {} {suffix}\r\n",
                        if on { "开" } else { "关" }
                    )));
                }
            }
        }
        // 5ms 轮询:自动断帧按"相邻数据间隔"判定,轮询间隔决定间隔测量的精度上限
        thread::sleep(Duration::from_millis(5));
    }

    // 清理:逐个包 try,单次失败不阻断退出路径
    let _ = jlink.rtt_control(RTT_CMD_STOP);
    jlink.close();
    let _ = tx.send(WorkerMsg::State(false, "● 未连接".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbk_two_byte_chars_split_across_chunks() {
        // "你好" 的 GBK 编码:[C4 E3][BA C3],从中间切开
        let mut d = CharsetDecoder::for_label("gbk");
        let a = d.decode(&[0xC4, 0xE3]);
        let b = d.decode(&[0xBA, 0xC3]);
        assert_eq!(a, "你");
        assert_eq!(b, "好");
    }

    #[test]
    fn utf16le_surrogate_split_across_chunks() {
        // "😀" UTF-16LE:[3D D8 00 DE](代理对),从中间切开
        let mut d = CharsetDecoder::for_label("utf-16le");
        let a = d.decode(&[0x3D, 0xD8]);
        let b = d.decode(&[0x00, 0xDE]);
        assert_eq!(format!("{a}{b}"), "😀");
    }

    #[test]
    fn utf8_multibyte_split_matches_old_carry_behavior() {
        // "你" UTF-8 = [E4 BD A0],切成三块逐字节喂
        let mut d = CharsetDecoder::for_label("utf-8");
        let mut s = String::new();
        for b in [0xE4u8, 0xBD, 0xA0] {
            s.push_str(&d.decode(&[b]));
        }
        assert_eq!(s, "你");
    }

    #[test]
    fn unknown_label_falls_back_to_utf8() {
        let mut d = CharsetDecoder::for_label("not-a-charset");
        assert_eq!(d.decode("hello".as_bytes()), "hello");
    }

    #[test]
    fn encodings_table_labels_are_all_resolvable() {
        for (_, label) in ENCODINGS {
            assert!(encoding_rs::Encoding::for_label(label.as_bytes()).is_some(), "{label}");
        }
    }
}
