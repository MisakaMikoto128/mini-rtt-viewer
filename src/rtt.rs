use crate::jlink_dll::{JLinkDll, RTT_CMD_START, RTT_CMD_STOP, TIF_JTAG, TIF_SWD};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

pub enum WorkerMsg {
    Log(String),
    State(bool, String),
}

/// 启动 RTT 工作线程:加载 DLL → 按验证过的序列连接 → 循环读通道。
/// 返回 stop 标志;置 true 后线程自行清理退出。
pub fn spawn(
    chip: String,
    iface_index: usize,
    speed_khz: u32,
    channel: u32,
    tx: mpsc::Sender<WorkerMsg>,
    cmd_rx: mpsc::Receiver<String>,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    thread::spawn(move || {
        if let Err(e) = run(&chip, iface_index, speed_khz, channel, &tx, &cmd_rx, &stop2) {
            let _ = tx.send(WorkerMsg::State(false, format!("● 错误: {e}")));
        }
    });
    stop
}

fn run(
    chip: &str,
    iface_index: usize,
    speed_khz: u32,
    channel: u32,
    tx: &mpsc::Sender<WorkerMsg>,
    cmd_rx: &mpsc::Receiver<String>,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let _ = tx.send(WorkerMsg::State(false, "● 正在加载 JLinkARM.dll…".into()));
    let jlink = JLinkDll::load()?;
    let sn = jlink.serial_number();
    let _ = tx.send(WorkerMsg::Log(format!(
        "[J-Link] 序列号 {sn},建立连接…\r\n"
    )));

    jlink.open();

    // 序列遵循原项目验证过的 DLL 状态机要求:RTT START 必须在 connect 之前
    let tif = if iface_index == 0 { TIF_SWD } else { TIF_JTAG };
    jlink.rtt_control(RTT_CMD_START);
    jlink.select_tif(tif);
    jlink.set_speed(speed_khz as i32);
    let resp = jlink.exec_command(&format!("Device = {chip}"));
    if !resp.trim().is_empty() {
        let _ = tx.send(WorkerMsg::Log(format!("[J-Link] {resp}\r\n")));
    }
    let rc = jlink.connect();
    if rc < 0 {
        jlink.close();
        anyhow::bail!("J-Link 连接目标失败 (错误码 {rc}),请检查芯片型号/接线/供电");
    }

    let _ = tx.send(WorkerMsg::State(
        true,
        format!("● 已连接 ({chip}, {}, {speed_khz}kHz)", if tif == TIF_SWD { "SWD" } else { "JTAG" }),
    ));

    let mut buf = [0u8; 4096];
    // 跨块 UTF-8 增量解码:emoji 4 字节可能被 RTT 读块边界切断
    let mut carry: Vec<u8> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let n = jlink.rtt_read(channel as i32, &mut buf);
        if n > 0 {
            let text = decode_utf8_incremental(&mut carry, &buf[..n as usize]);
            let _ = tx.send(WorkerMsg::Log(strip_ansi(&text)));
        } else if n < 0 {
            let _ = tx.send(WorkerMsg::State(false, format!("● RTT 读取失败 ({n}),已断开")));
            jlink.rtt_control(RTT_CMD_STOP);
            jlink.close();
            return Ok(());
        }
        while let Ok(data) = cmd_rx.try_recv() {
            let w = jlink.rtt_write(channel as i32, data.as_bytes());
            if w < 0 {
                let _ = tx.send(WorkerMsg::Log("[发送失败]\r\n".into()));
            }
        }
        thread::sleep(Duration::from_millis(20));
    }

    // 清理:逐个包 try,单次失败不阻断退出路径
    let _ = jlink.rtt_control(RTT_CMD_STOP);
    jlink.close();
    let _ = tx.send(WorkerMsg::State(false, "● 未连接".into()));
    Ok(())
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

/// 去掉 ANSI 转义序列(CSI 与常见单字符转义),只保留可见文本。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for f in chars.by_ref() {
                        if f.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') | Some('(') | Some(')') => {
                    chars.next();
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}
