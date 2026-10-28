use crate::backend::{
    bytes_to_hex_upper, DeviceInfo, WorkerCmd, WorkerHandle, WorkerMsg, APP_SHUTDOWN,
};
use crate::jlink_dll::{JLinkDll, RTT_CMD_START, RTT_CMD_STOP, TIF_JTAG, TIF_SWD};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

// worker 协议类型(WorkerMsg/WorkerCmd/WorkerHandle/DeviceInfo/APP_SHUTDOWN)
// 已统一迁到 backend.rs(后端抽象缝的公共契约,阶段一起 web 层与各后端实现
// 共用);本模块保留 worker 内部实现:连接序列、读循环、字符集/HEX 解码。

/// 速度下拉表(kHz):连接设置的单一真源,下标即前端下拉索引(桌面壳与
/// 纯服务两种形态共用)
pub const SPEEDS_KHZ: [u32; 8] = [100, 200, 500, 1000, 2000, 4000, 8000, 12000];

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
    /// 字符集下拉索引(ENCODINGS 下标)共享变量,UI 侧运行时可改——读循环每块
    /// 检测变化并热重建解码器,切换**动态生效**(无需重连)
    pub encoding_index: Arc<AtomicU32>,
    /// HEX 接收模式共享变量:开启后原始字节直接转十六进制文本(跳过字符集解码,
    /// 切换动态生效;行语义退化为断帧——一帧一行 hex)
    pub hex_rx: Arc<AtomicBool>,
}

/// 字符集下拉表:**与前端 ui/web/index.html 的「字符集」下拉 option 顺序严格一致**
/// (option 的 value 即本表下标)。
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
        let enc = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
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

/// 下拉索引 → encoding_rs 标签(越界回落 UTF-8)
fn enc_label(index: u32) -> &'static str {
    ENCODINGS
        .get(index as usize)
        .map(|(_, label)| *label)
        .unwrap_or("utf-8")
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

/// 退出请求判定(worker 线程各阻塞段之间检查):UI 断开(stop)或应用退出
/// (APP_SHUTDOWN)都算。连接阶段的 DLL 调用(JLINKARM_Connect 等)本身
/// 不可中断,只能在调用间隙检测——见 connect_target 的检查点布置(审计 R1)。
fn stop_requested(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed) || APP_SHUTDOWN.load(Ordering::Relaxed)
}

/// 连接序列:加载 DLL → 抑制弹窗 → 选定调试器 → OpenEx → RTT START →
/// TIF/速度/设备名 → Connect(带重试)→ State(true) + DeviceInfo。
/// 成功返回已打开的 JLinkDll;失败路径自行 close 后返回 Err
/// (JLinkDll 有 Drop 兜底,中途 return 也不会漏 close)。
fn connect_target(
    config: &WorkerConfig,
    tx: &mpsc::Sender<WorkerMsg>,
    stop: &AtomicBool,
) -> anyhow::Result<JLinkDll> {
    let WorkerConfig {
        chip,
        iface_index,
        speed_khz,
        selected_sn,
        ..
    } = config;
    let _ = tx.send(WorkerMsg::Progress("● 正在加载 JLink_x64.dll…".into()));
    let jlink = JLinkDll::load()?;
    if stop_requested(stop) {
        anyhow::bail!("退出中,已取消连接");
    }
    // 抑制 DLL 模态弹窗必须最先做(调试器选择窗/固件升级提示都发生在 Open 内部),
    // 否则 worker 会卡在无人应答的隐藏对话框上
    jlink.disable_dialog_boxes();
    // 刷新 J-Link 下拉列表(用户可能插拔过)
    let emus = jlink.enumerate_emulators();
    let _ = tx.send(WorkerMsg::JLinks(emus.clone()));
    if stop_requested(stop) {
        anyhow::bail!("退出中,已取消连接");
    }

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
    // 返回码检查(审计 R2):Open 失败继续走会把 GetSN=0 的误导信息发给 UI,
    // 真实失败环节(Open)反而被 connect() 的通用报错掩盖
    let rc = jlink.open_ex();
    if rc < 0 {
        anyhow::bail!("J-Link 打开失败(错误码 {rc}):请检查驱动安装/设备连接");
    }
    if stop_requested(stop) {
        anyhow::bail!("退出中,已取消连接");
    }

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
    let rc = jlink.select_tif(tif);
    if rc < 0 {
        anyhow::bail!("选择接口失败(错误码 {rc})");
    }
    let rc = jlink.set_speed(*speed_khz as i32);
    if rc < 0 {
        anyhow::bail!("设置速率 {speed_khz}kHz 失败(错误码 {rc})");
    }
    let resp = jlink.exec_command(&format!("Device = {chip}"));
    if !resp.trim().is_empty() {
        let _ = tx.send(WorkerMsg::Log(format!("[J-Link] {resp}\r\n")));
    }
    if stop_requested(stop) {
        anyhow::bail!("退出中,已取消连接");
    }
    // 目标连接第一次常会失败(DLL 内部状态机/目标未就绪),自动重试而非回退 UI 状态,
    // 避免按钮"断开→连接→断开"来回跳
    let mut rc = jlink.connect();
    for attempt in 1..4 {
        if rc >= 0 || stop_requested(stop) {
            break;
        }
        let _ = tx.send(WorkerMsg::Progress(format!(
            "● 连接中…(第 {attempt} 次重试)"
        )));
        thread::sleep(Duration::from_millis(400));
        if stop_requested(stop) {
            anyhow::bail!("退出中,已取消连接");
        }
        rc = jlink.connect();
    }
    if rc < 0 {
        jlink.close();
        anyhow::bail!(
            "J-Link 连接目标失败(错误码 {rc}):请检查芯片型号/接线/供电,或尝试降低速率(高速率在 Cortex-M0 上可能不稳定)"
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
    let WorkerConfig {
        channel,
        frame_timeout_ms,
        encoding_index,
        hex_rx,
        ..
    } = config;

    let mut buf = [0u8; 4096];
    // 按用户选定字符集增量解码:跨读块的多字节边界(emoji 4 字节/GBK 2 字节
    // 被块切断)由 decoder 内部管理
    let mut current_enc = encoding_index.load(Ordering::Relaxed);
    let mut decoder = CharsetDecoder::for_label(enc_label(current_enc));
    // HEX 接收开关(切换时发横幅提示)
    let mut current_hex = hex_rx.load(Ordering::Relaxed);
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
            // 字符集动态生效:检测到下拉变化即热重建解码器(旧编码的半字节残留
            // 一并丢弃——按新编码解释旧残留没有意义),并发横幅提示
            let want_enc = encoding_index.load(Ordering::Relaxed);
            if want_enc != current_enc {
                current_enc = want_enc;
                decoder = CharsetDecoder::for_label(enc_label(current_enc));
                let _ = tx.send(WorkerMsg::Log(format!(
                    "[字符集] {}\r\n",
                    ENCODINGS
                        .get(current_enc as usize)
                        .map(|(name, _)| *name)
                        .unwrap_or("UTF-8")
                )));
            }
            // HEX 接收动态生效:切换时发横幅
            let want_hex = hex_rx.load(Ordering::Relaxed);
            if want_hex != current_hex {
                current_hex = want_hex;
                let _ = tx.send(WorkerMsg::Log(format!(
                    "[HEX 接收 {}]\r\n",
                    if current_hex { "开" } else { "关" }
                )));
            }
            // ANSI 转义序列原样透传,由 UI 端 vte 解析着色(本层不再剥掉);
            // HEX 接收模式:原始字节直接转十六进制文本(跳过字符集解码保真),
            // 行语义退化为断帧——一帧一行 hex
            let text = if current_hex {
                bytes_to_hex_upper(&buf[..n as usize])
            } else {
                decoder.decode(&buf[..n as usize])
            };
            let _ = tx.send(WorkerMsg::Block(text));
            frame_open = true;
            last_rx = now;
        } else if frame_open && now.duration_since(last_rx) > to {
            // 数据停了:补发帧结束
            let _ = tx.send(WorkerMsg::FrameEnd);
            frame_open = false;
        } else if n < 0 {
            let _ = tx.send(WorkerMsg::State(
                false,
                format!("● RTT 读取失败 ({n}),已断开"),
            ));
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
                        // go() 返回码不再静默丢弃(审计 R5):失败时 RTT 重挂成功
                        // 也读不到运行中的固件,明确报出来
                        let go_rc = jlink.go();
                        if go_rc < 0 {
                            let _ = tx.send(WorkerMsg::Log(format!("[Go 失败 rc={go_rc}]\r\n")));
                        }
                        // 复位后固件重新初始化 RTT 控制块,重挂 RTT 保证继续可读
                        jlink.rtt_control(RTT_CMD_STOP);
                        jlink.rtt_control(RTT_CMD_START);
                        let _ = tx.send(WorkerMsg::Log("[目标已复位,RTT 已重挂]\r\n".into()));
                    }
                }
                WorkerCmd::Power(on) => {
                    // SupplyPower:J-Link 19 脚给目标供电(与原项目 pylink power_on/off 一致)
                    let resp = jlink.exec_command(if on {
                        "SupplyPower = 1"
                    } else {
                        "SupplyPower = 0"
                    });
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
            assert!(
                encoding_rs::Encoding::for_label(label.as_bytes()).is_some(),
                "{label}"
            );
        }
    }
}
