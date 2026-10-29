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

/// 在设备 RAM 中定位 RTT 控制块。读取按 4KB 分块并保留签名长度减一字节的重叠,
/// 签名跨块时也不会漏掉;命中后再读完整头部,用缓冲区数量和所选通道排除假签名。
fn find_rtt_control_block<F>(regions: &[(u32, u32)], channel: u32, mut read: F) -> Option<u32>
where
    F: FnMut(u32, &mut [u8]) -> bool,
{
    const CHUNK_SIZE: u32 = 4096;
    const SIGNATURE: &[u8] = b"SEGGER RTT";

    for &(region_addr, region_size) in regions {
        let mut offset = 0u32;
        let mut carry = Vec::new();
        while offset < region_size {
            let read_len = (region_size - offset).min(CHUNK_SIZE) as usize;
            let Some(chunk_addr) = region_addr.checked_add(offset) else {
                break;
            };
            let mut chunk = vec![0u8; read_len];
            if !read(chunk_addr, &mut chunk) {
                carry.clear();
                offset += read_len as u32;
                continue;
            }

            let mut scan = carry.clone();
            scan.extend_from_slice(&chunk);
            let scan_addr = chunk_addr - carry.len() as u32;
            for (position, _) in scan
                .windows(SIGNATURE.len())
                .enumerate()
                .filter(|(_, window)| *window == SIGNATURE)
            {
                let Some(candidate) = scan_addr.checked_add(position as u32) else {
                    continue;
                };
                let mut header = [0u8; 24];
                if !read(candidate, &mut header) {
                    continue;
                }
                let up_buffers = u32::from_le_bytes(header[16..20].try_into().unwrap());
                let down_buffers = u32::from_le_bytes(header[20..24].try_into().unwrap());
                if up_buffers > channel && up_buffers <= 64 && down_buffers <= 64 {
                    return Some(candidate);
                }
            }

            carry = chunk[chunk.len().saturating_sub(SIGNATURE.len() - 1)..].to_vec();
            offset += read_len as u32;
        }
    }
    None
}

fn check_rtt_start(rc: i32) -> anyhow::Result<()> {
    if rc < 0 {
        anyhow::bail!("RTT 启动失败(错误码 {rc}):请检查 J-Link 连接及目标固件的 SEGGER RTT 初始化");
    }
    Ok(())
}

const RTT_WRITE_MAX_STALLS: usize = 100;
const RESET_DISCOVERY_ATTEMPTS: usize = 20;
const RESET_DISCOVERY_DELAY: Duration = Duration::from_millis(100);

/// Tx 只统计真实写入的字节;零进展有界重试,负返回码或异常长度明确失败。
fn send_rtt_payload<F, W>(data: &[u8], tx: &mpsc::Sender<WorkerMsg>, mut write: F, mut wait: W)
where
    F: FnMut(&[u8]) -> i32,
    W: FnMut(),
{
    let mut written = 0usize;
    let mut stalls = 0usize;
    let mut failure = None;
    while written < data.len() {
        let rc = write(&data[written..]);
        if rc < 0 {
            failure = Some(format!("错误码 {rc}"));
            break;
        }
        if rc == 0 {
            stalls += 1;
            if stalls >= RTT_WRITE_MAX_STALLS {
                failure = Some("RTT 写入持续无进展".into());
                break;
            }
            wait();
            continue;
        }

        let n = rc as usize;
        if n > data.len() - written {
            failure = Some(format!("DLL 返回异常写入长度 {n}"));
            break;
        }
        written += n;
        stalls = 0;
    }

    if let Some(error) = failure {
        if written > 0 {
            let _ = tx.send(WorkerMsg::Tx(written));
        }
        let _ = tx.send(WorkerMsg::Log(format!(
            "[发送失败: {error},已写入 {written}/{} 字节]\r\n",
            data.len()
        )));
    } else {
        let _ = tx.send(WorkerMsg::Tx(written));
    }
}

fn rx_block(bytes: &[u8], hex: bool, decoder: &mut CharsetDecoder) -> WorkerMsg {
    let text = if hex {
        bytes_to_hex_upper(bytes)
    } else {
        decoder.decode(bytes)
    };
    WorkerMsg::Block {
        text,
        raw_len: bytes.len(),
    }
}

/// 固件复位后需要一段时间重建控制块;只在有限窗口内重试,并响应退出。
fn discover_rtt_after_reset<F, W>(
    stop: &AtomicBool,
    mut discover: F,
    mut wait: W,
) -> anyhow::Result<Option<u32>>
where
    F: FnMut() -> Option<u32>,
    W: FnMut(),
{
    for _ in 0..RESET_DISCOVERY_ATTEMPTS {
        if stop_requested(stop) {
            anyhow::bail!("退出中,已取消复位后 RTT 重挂");
        }
        // Go 后先给固件覆盖旧控制块的时间,避免立即扫到复位前的陈旧签名。
        wait();
        if stop_requested(stop) {
            anyhow::bail!("退出中,已取消复位后 RTT 重挂");
        }
        if let Some(addr) = discover() {
            return Ok(Some(addr));
        }
    }
    Ok(None)
}

fn restart_rtt<F>(mut control: F) -> anyhow::Result<()>
where
    F: FnMut(i32) -> i32,
{
    let rc = control(RTT_CMD_STOP);
    if rc < 0 {
        anyhow::bail!("RTT 停止失败(错误码 {rc})");
    }
    check_rtt_start(control(RTT_CMD_START))
}

/// 初始连接与原地复位共用的显式控制块设置/RTT 重挂路径。
fn restart_rtt_at<S, C>(addr: u32, mut set_addr: S, control: C) -> anyhow::Result<String>
where
    S: FnMut(u32) -> (i32, String),
    C: FnMut(i32) -> i32,
{
    let (rc, response) = set_addr(addr);
    if rc < 0 {
        anyhow::bail!("设置 RTT 控制块地址 0x{addr:08X} 失败(错误码 {rc})");
    }
    restart_rtt(control)?;
    Ok(response)
}

fn reset_target<R, G, A>(tx: &mpsc::Sender<WorkerMsg>, reset: R, go: G, reattach: A)
where
    R: FnOnce() -> i32,
    G: FnOnce() -> i32,
    A: FnOnce() -> anyhow::Result<()>,
{
    let result = (|| {
        let rc = reset();
        if rc < 0 {
            anyhow::bail!("目标复位返回错误码 {rc}");
        }
        let go_rc = go();
        if go_rc < 0 {
            anyhow::bail!("Go 失败 rc={go_rc}");
        }
        reattach()
    })();
    match result {
        Ok(()) => {
            let _ = tx.send(WorkerMsg::Log("[目标已复位,RTT 已重挂]\r\n".into()));
        }
        Err(error) => {
            let _ = tx.send(WorkerMsg::Log(format!("[复位失败: {error}]\r\n")));
        }
    }
}

fn power_target<E>(on: bool, tx: &mpsc::Sender<WorkerMsg>, exec: E)
where
    E: FnOnce(&str) -> (i32, String),
{
    let (rc, response) = exec(if on {
        "SupplyPower = 1"
    } else {
        "SupplyPower = 0"
    });
    let response = response.trim();
    let suffix = if response.is_empty() {
        String::new()
    } else {
        format!("({response})")
    };
    let action = if on { "开" } else { "关" };
    let message = if rc < 0 {
        format!("[J-Link] 电源输出 {action}失败(错误码 {rc}) {suffix}\r\n")
    } else {
        format!("[J-Link] 电源输出 {action} {suffix}\r\n")
    };
    let _ = tx.send(WorkerMsg::PowerResult {
        on,
        ok: rc >= 0,
        message,
    });
}

/// 连接序列:加载 DLL → 抑制弹窗 → 选定调试器 → OpenEx → RTT START →
/// TIF/速度/设备名 → Connect(带重试)→ 设备 RAM 显式定位 RTT 控制块 →
/// State(true) + DeviceInfo。
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
        channel,
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
    check_rtt_start(jlink.rtt_control(RTT_CMD_START))?;
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

    // 部分设备(实测 STM32G070)的 J-Link 自动发现会漏掉有效 RTT 控制块。
    // 用设备数据库的 RAM 区域显式定位并设置地址;找不到时保留原有自动发现,
    // 不把固件尚未初始化 RTT 误判成连接失败。初始 RTT START 仍严格早于 Connect。
    let regions = jlink.ram_regions_for_device(chip);
    let control_block = find_rtt_control_block(&regions, *channel, |addr, buf| {
        jlink.read_mem(addr, buf) >= 0
    });
    if let Some(addr) = control_block {
        let response = restart_rtt_at(
            addr,
            |addr| jlink.exec_command_with_status(&format!("SetRTTAddr 0x{addr:08X}")),
            |cmd| jlink.rtt_control(cmd),
        )?;
        if !response.trim().is_empty() {
            let _ = tx.send(WorkerMsg::Log(format!("[J-Link] {response}\r\n")));
        }
        let _ = tx.send(WorkerMsg::Log(format!(
            "[RTT] 已定位控制块 0x{addr:08X}\r\n"
        )));
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
            // raw_len 在 HEX/字符集转换前固定,RX 统计不受展示文本膨胀影响。
            let _ = tx.send(rx_block(&buf[..n as usize], current_hex, &mut decoder));
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
                    send_rtt_payload(
                        &data,
                        tx,
                        |remaining| jlink.rtt_write(*channel as i32, remaining),
                        || thread::sleep(Duration::from_millis(1)),
                    );
                }
                WorkerCmd::Reset => {
                    reset_target(
                        tx,
                        || jlink.reset(),
                        || jlink.go(),
                        || {
                            // 固件可能在复位后重建或移动控制块;重走连接路径的 RAM
                            // 发现 → SetRTTAddr → STOP/START。有限等待后仍找不到
                            // 则重挂自动发现,保持旧设备兼容。
                            let regions = jlink.ram_regions_for_device(&config.chip);
                            let control_block = discover_rtt_after_reset(
                                stop,
                                || {
                                    find_rtt_control_block(&regions, *channel, |addr, buf| {
                                        jlink.read_mem(addr, buf) >= 0
                                    })
                                },
                                || thread::sleep(RESET_DISCOVERY_DELAY),
                            )?;
                            if let Some(addr) = control_block {
                                let response = restart_rtt_at(
                                    addr,
                                    |addr| {
                                        jlink.exec_command_with_status(&format!(
                                            "SetRTTAddr 0x{addr:08X}"
                                        ))
                                    },
                                    |cmd| jlink.rtt_control(cmd),
                                )?;
                                if !response.trim().is_empty() {
                                    let _ =
                                        tx.send(WorkerMsg::Log(format!("[J-Link] {response}\r\n")));
                                }
                                let _ = tx.send(WorkerMsg::Log(format!(
                                    "[RTT] 复位后已定位控制块 0x{addr:08X}\r\n"
                                )));
                            } else {
                                restart_rtt(|cmd| jlink.rtt_control(cmd))?;
                                let _ = tx.send(WorkerMsg::Log(
                                    "[RTT] 复位后未定位控制块,使用自动发现\r\n".into(),
                                ));
                            }
                            Ok(())
                        },
                    );
                }
                WorkerCmd::Power(on) => {
                    // SupplyPower:J-Link 19 脚给目标供电;返回码决定真实成败。
                    power_target(on, tx, |command| jlink.exec_command_with_status(command));
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
    fn rtt_control_block_scan_finds_signature_across_read_chunks() {
        let mut ram = vec![0u8; 0x3000];
        let offset = 0x0ffc;
        ram[offset..offset + 10].copy_from_slice(b"SEGGER RTT");
        ram[offset + 16..offset + 20].copy_from_slice(&3u32.to_le_bytes());
        ram[offset + 20..offset + 24].copy_from_slice(&2u32.to_le_bytes());
        let found = find_rtt_control_block(&[(0x2000_0000, ram.len() as u32)], 0, |addr, buf| {
            let start = (addr - 0x2000_0000) as usize;
            buf.copy_from_slice(&ram[start..start + buf.len()]);
            true
        });
        assert_eq!(found, Some(0x2000_0000 + offset as u32));
    }

    #[test]
    fn rtt_control_block_scan_rejects_invalid_channel_counts() {
        let mut ram = vec![0u8; 0x1000];
        ram[..10].copy_from_slice(b"SEGGER RTT");
        ram[16..20].copy_from_slice(&1u32.to_le_bytes());
        ram[20..24].copy_from_slice(&1u32.to_le_bytes());
        let found = find_rtt_control_block(&[(0x2000_0000, ram.len() as u32)], 1, |addr, buf| {
            let start = (addr - 0x2000_0000) as usize;
            buf.copy_from_slice(&ram[start..start + buf.len()]);
            true
        });
        assert_eq!(found, None);
    }

    #[test]
    fn rtt_control_block_scan_continues_after_unreadable_region() {
        let mut second = vec![0u8; 0x1000];
        second[0x100..0x10a].copy_from_slice(b"SEGGER RTT");
        second[0x110..0x114].copy_from_slice(&1u32.to_le_bytes());
        second[0x114..0x118].copy_from_slice(&1u32.to_le_bytes());
        let found = find_rtt_control_block(
            &[(0x1000_0000, 0x1000), (0x2000_0000, 0x1000)],
            0,
            |addr, buf| {
                if addr < 0x2000_0000 {
                    return false;
                }
                let start = (addr - 0x2000_0000) as usize;
                buf.copy_from_slice(&second[start..start + buf.len()]);
                true
            },
        );
        assert_eq!(found, Some(0x2000_0100));
    }

    #[test]
    fn rtt_start_failure_is_not_silently_accepted() {
        assert!(check_rtt_start(-1).is_err());
        assert!(check_rtt_start(0).is_ok());
    }

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

    #[test]
    fn send_short_writes_continue_with_remaining_bytes() {
        use std::collections::VecDeque;

        let (tx, rx) = mpsc::channel();
        let mut results = VecDeque::from([2, 1, 3]);
        let mut calls = Vec::new();
        send_rtt_payload(
            b"abcdef",
            &tx,
            |remaining| {
                calls.push(remaining.to_vec());
                results.pop_front().unwrap()
            },
            || {},
        );

        assert_eq!(
            calls,
            vec![b"abcdef".to_vec(), b"cdef".to_vec(), b"def".to_vec()]
        );
        assert!(matches!(rx.try_recv(), Ok(WorkerMsg::Tx(6))));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn send_zero_progress_is_bounded_and_reports_failure() {
        let (tx, rx) = mpsc::channel();
        let mut calls = 0;
        send_rtt_payload(
            b"abc",
            &tx,
            |_| {
                calls += 1;
                assert!(calls <= 1_000, "零进展写入必须有界");
                0
            },
            || {},
        );

        assert!(calls > 1 && calls <= 1_000);
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Log(text)) if text.contains("发送失败")
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn send_negative_write_does_not_report_tx() {
        let (tx, rx) = mpsc::channel();
        send_rtt_payload(b"abc", &tx, |_| -9, || {});

        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Log(text)) if text.contains("发送失败") && text.contains("-9")
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn send_partial_progress_is_counted_before_failure() {
        let (tx, rx) = mpsc::channel();
        let mut calls = 0;
        send_rtt_payload(
            b"abcde",
            &tx,
            |_| {
                calls += 1;
                if calls == 1 {
                    2
                } else {
                    -5
                }
            },
            || {},
        );

        assert!(matches!(rx.try_recv(), Ok(WorkerMsg::Tx(2))));
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Log(text)) if text.contains("发送失败") && text.contains("2/5")
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn reset_go_failure_skips_reattach_and_success_message() {
        let (tx, rx) = mpsc::channel();
        let mut reattached = false;
        reset_target(
            &tx,
            || 0,
            || -7,
            || {
                reattached = true;
                Ok(())
            },
        );

        assert!(!reattached);
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Log(text)) if text.contains("Go 失败") && !text.contains("已重挂")
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn reset_reattach_failure_is_not_reported_as_success() {
        let (tx, rx) = mpsc::channel();
        reset_target(&tx, || 0, || 0, || anyhow::bail!("RTT 启动失败(错误码 -3)"));

        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Log(text)) if text.contains("复位失败") && text.contains("-3") && !text.contains("已重挂")
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn reset_discovery_waits_before_each_stale_ram_scan() {
        let stop = AtomicBool::new(false);
        let events = std::cell::RefCell::new(Vec::new());
        let mut calls = 0;
        let found = discover_rtt_after_reset(
            &stop,
            || {
                calls += 1;
                events.borrow_mut().push("scan");
                (calls == 3).then_some(0x2000_0100)
            },
            || events.borrow_mut().push("wait"),
        )
        .unwrap();

        assert_eq!(found, Some(0x2000_0100));
        assert_eq!(calls, 3);
        assert_eq!(
            events.into_inner(),
            ["wait", "scan", "wait", "scan", "wait", "scan"]
        );
    }

    #[test]
    fn reset_discovery_without_control_block_is_bounded() {
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        let found = discover_rtt_after_reset(
            &stop,
            || {
                calls += 1;
                assert!(calls <= 100, "控制块发现必须有界");
                None
            },
            || {},
        )
        .unwrap();

        assert_eq!(found, None);
        assert!(calls > 1 && calls <= 100);
    }

    #[test]
    fn reset_discovery_honors_stop_before_hardware_access() {
        let stop = AtomicBool::new(true);
        let mut called = false;
        let result = discover_rtt_after_reset(
            &stop,
            || {
                called = true;
                Some(0x2000_0000)
            },
            || {},
        );

        assert!(result.is_err());
        assert!(!called);
    }

    #[test]
    fn restart_rtt_sets_address_before_stop_and_start() {
        let events = std::cell::RefCell::new(Vec::new());
        let response = restart_rtt_at(
            0x2000_0100,
            |addr| {
                events.borrow_mut().push(format!("addr:{addr:08X}"));
                (0, "ok".into())
            },
            |cmd| {
                events.borrow_mut().push(format!("control:{cmd}"));
                0
            },
        )
        .unwrap();

        assert_eq!(response, "ok");
        assert_eq!(
            events.into_inner(),
            ["addr:20000100", "control:1", "control:0"]
        );
    }

    #[test]
    fn restart_rtt_start_failure_is_returned() {
        let result = restart_rtt_at(
            0x2000_0100,
            |_| (0, String::new()),
            |cmd| if cmd == RTT_CMD_START { -4 } else { 0 },
        );
        assert!(result.is_err());
    }

    #[test]
    fn power_result_reflects_exec_command_status() {
        let (tx, rx) = mpsc::channel();
        power_target(true, &tx, |command| {
            assert_eq!(command, "SupplyPower = 1");
            (-6, "denied".into())
        });
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::PowerResult { on: true, ok: false, message })
                if message.contains("-6") && message.contains("denied")
        ));

        let (tx, rx) = mpsc::channel();
        power_target(false, &tx, |command| {
            assert_eq!(command, "SupplyPower = 0");
            (0, "ok".into())
        });
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::PowerResult { on: false, ok: true, message })
                if message.contains("ok")
        ));
    }

    #[test]
    fn rx_block_counts_raw_bytes_before_text_conversion() {
        let mut decoder = CharsetDecoder::for_label("utf-8");
        assert!(matches!(
            rx_block("你".as_bytes(), false, &mut decoder),
            WorkerMsg::Block { text, raw_len: 3 } if text == "你"
        ));

        let mut decoder = CharsetDecoder::for_label("utf-8");
        assert!(matches!(
            rx_block(&[0x00, 0xFF], true, &mut decoder),
            WorkerMsg::Block { text, raw_len: 2 } if text == "00 FF "
        ));
    }

    fn assert_split_decode_counts_each_input_byte_once(
        label: &str,
        chunks: &[&[u8]],
        expected_raw_len: usize,
        expected_text: &str,
    ) {
        let mut decoder = CharsetDecoder::for_label(label);
        let mut decoded_text = String::new();
        let mut raw_total = 0;
        for (index, chunk) in chunks.iter().enumerate() {
            match rx_block(chunk, false, &mut decoder) {
                WorkerMsg::Block { text, raw_len } => {
                    if index == 0 {
                        assert!(text.is_empty(), "{label} 首块应只有半字符");
                    }
                    assert_eq!(raw_len, chunk.len(), "{label} 第 {index} 块");
                    raw_total += raw_len;
                    decoded_text.push_str(&text);
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(raw_total, expected_raw_len, "{label} 原始字节总量");
        assert_eq!(decoded_text, expected_text, "{label} 跨块解码文本");
    }

    #[test]
    fn rx_block_counts_empty_incremental_decode_chunks_without_duplication() {
        assert_split_decode_counts_each_input_byte_once(
            "utf-8",
            &[&[0xE4, 0xBD][..], &[0xA0][..]],
            3,
            "你",
        );
        assert_split_decode_counts_each_input_byte_once(
            "gbk",
            &[&[0xC4][..], &[0xE3][..]],
            2,
            "你",
        );
        assert_split_decode_counts_each_input_byte_once(
            "utf-16le",
            &[&[0x3D, 0xD8][..], &[0x00, 0xDE][..]],
            4,
            "😀",
        );
    }

    #[test]
    fn rx_block_text_and_hex_have_same_cross_chunk_raw_total() {
        let chunks: [&[u8]; 3] = [&[0xE4, 0xBD], &[0xA0, 0xF0, 0x9F], &[0x98, 0x80]];
        let mut text_decoder = CharsetDecoder::for_label("utf-8");
        let mut hex_decoder = CharsetDecoder::for_label("utf-8");
        let mut text = String::new();
        let mut hex = String::new();
        let mut text_raw_total = 0;
        let mut hex_raw_total = 0;

        for chunk in chunks {
            match rx_block(chunk, false, &mut text_decoder) {
                WorkerMsg::Block {
                    text: decoded,
                    raw_len,
                } => {
                    text.push_str(&decoded);
                    text_raw_total += raw_len;
                }
                _ => unreachable!(),
            }
            match rx_block(chunk, true, &mut hex_decoder) {
                WorkerMsg::Block {
                    text: encoded,
                    raw_len,
                } => {
                    hex.push_str(&encoded);
                    hex_raw_total += raw_len;
                }
                _ => unreachable!(),
            }
        }

        assert_eq!(text, "你😀");
        assert_eq!(hex, "E4 BD A0 F0 9F 98 80 ");
        assert_eq!(text_raw_total, 7);
        assert_eq!(hex_raw_total, 7);
    }
}
