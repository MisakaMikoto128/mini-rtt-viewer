//! 无界面 RTT 验证脚本(走 JLinkARM.dll)。
//! 用法: cargo run --example rtt_check -- [芯片型号] [秒数]

use mini_rtt_viewer::jlink_dll::{JLinkDll, RTT_CMD_START, RTT_CMD_STOP, TIF_SWD};
use mini_rtt_viewer::rtt::CharsetDecoder;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let chip = std::env::args().nth(1).unwrap_or_else(|| "STM32F030F4".into());
    let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(8);

    let jlink = JLinkDll::load()?;
    println!("[dll] loaded, SN = {}", jlink.serial_number());
    jlink.open();

    // 序列与 src/rtt.rs 一致:RTT START → TIF → 速度 → Device → Connect
    jlink.rtt_control(RTT_CMD_START);
    jlink.select_tif(TIF_SWD);
    jlink.set_speed(4000);
    let resp = jlink.exec_command(&format!("Device = {chip}"));
    if !resp.trim().is_empty() {
        println!("[exec] {resp}");
    }
    let rc = jlink.connect();
    if rc < 0 {
        anyhow::bail!("connect 失败,错误码 {rc}");
    }
    println!("[ok] connected to {chip}");

    let mut buf = [0u8; 4096];
    let mut decoder = CharsetDecoder::for_label("utf-8");
    let start = Instant::now();
    let mut total = 0usize;
    println!("---- RTT 输出开始 ----");
    while start.elapsed() < Duration::from_secs(secs) {
        let n = jlink.rtt_read(0, &mut buf);
        if n > 0 {
            total += n as usize;
            print!("{}", decoder.decode(&buf[..n as usize]));
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("\n---- RTT 输出结束 ---- 共 {total} 字节");
    let _ = jlink.rtt_control(RTT_CMD_STOP);
    jlink.close();
    Ok(())
}
