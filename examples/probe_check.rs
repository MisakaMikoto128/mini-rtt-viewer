//! 无界面 probe-rs 全链路验证探针(独立于 UI,真机排障用)。
//!
//! 用法:
//!   cargo run --example probe_check -- list
//!   cargo run --example probe_check -- rtt <芯片> [秒数]
//!   cargo run --example probe_check -- send <芯片> <文本>
//!   cargo run --example probe_check -- reset <芯片>
//!
//! 可选参数(任意位置):
//!   --sn <序列号>   指定调试器(多台时;缺省取第一台)
//!   --ch <通道>     RTT 通道号(缺省 0)
//!   --speed <kHz>   协议速率(缺省 1000)
//!
//! 说明:attach 一律默认模式(不复位、不下载);结束 session drop 自动清理,不留锁。

use anyhow::Context;
use mini_rtt_viewer::rtt::decode_utf8_incremental;
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;
use probe_rs::probe::WireProtocol;
use probe_rs::rtt::Rtt;
use probe_rs::{Permissions, Session};
use std::io::Write as _;
use std::time::{Duration, Instant};

struct Args {
    cmd: String,
    chip: Option<String>,
    secs: u64,
    text: String,
    sn: Option<u32>,
    channel: usize,
    speed_khz: u32,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| "list".into());
    let mut a = Args {
        cmd,
        chip: None,
        secs: 8,
        text: String::new(),
        sn: None,
        channel: 0,
        speed_khz: 1000,
    };
    let mut positional = Vec::new();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--sn" => a.sn = Some(it.next().context("--sn 需要一个序列号参数")?.parse()?),
            "--ch" => a.channel = it.next().context("--ch 需要一个通道号")?.parse()?,
            "--speed" => a.speed_khz = it.next().context("--speed 需要 kHz 值")?.parse()?,
            _ => positional.push(arg),
        }
    }
    // 位置参数:cmd=rtt → 芯片 [秒数];cmd=send → 芯片 文本…;cmd=reset → 芯片
    match a.cmd.as_str() {
        "list" => {}
        "rtt" => {
            a.chip = Some(positional.first().context("用法: rtt <芯片> [秒数]")?.clone());
            if let Some(s) = positional.get(1) {
                a.secs = s.parse()?;
            }
        }
        "send" => {
            a.chip = Some(positional.first().context("用法: send <芯片> <文本>")?.clone());
            a.text = positional.get(1..).context("缺文本参数")?.join(" ");
        }
        "reset" => {
            a.chip = Some(positional.first().context("用法: reset <芯片>")?.clone());
        }
        other => anyhow::bail!("未知子命令 {other}(支持 list/rtt/send/reset)"),
    }
    Ok(a)
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    match args.cmd.as_str() {
        "list" => list(),
        "rtt" => rtt(&args),
        "send" => send(&args),
        "reset" => reset(&args),
        _ => unreachable!("parse_args 已拒绝其他命令"),
    }
}

/// 枚举本机调试器:序列号 / VID:PID / 产品名 / 可访问性
fn list() -> anyhow::Result<()> {
    let items = Lister::new().list_all_with_access();
    println!("发现 {} 台调试器:", items.len());
    for item in &items {
        let info = &item.info;
        println!(
            "  - {} | VID:PID {:04x}:{:04x} | SN {:?} | 访问: {:?}",
            info.identifier,
            info.vendor_id,
            info.product_id,
            info.serial_number,
            item.accessibility,
        );
    }
    if items.is_empty() {
        println!("  (probe-rs 没有发现任何调试器)");
    }
    Ok(())
}

/// 打开调试器(按 --sn 或第一台)→ 电压/协议/速度 → attach(不复位、不下载)
fn open_session(args: &Args, chip: &str, registry: &Registry) -> anyhow::Result<Session> {
    let probes = Lister::new().list_all();
    let chosen = match args.sn {
        Some(sn) => probes
            .iter()
            .find(|p| p.serial_number.as_deref().and_then(|s| s.parse::<u32>().ok()) == Some(sn))
            .with_context(|| format!("找不到 SN {sn} 的调试器;当前接入 {probes:?}"))?,
        None => probes.first().context("未发现任何调试器")?,
    };
    println!(
        "[probe] 使用 {} (SN {:?})",
        chosen.identifier,
        chosen.serial_number.as_deref().unwrap_or("?")
    );
    let mut probe = chosen.open().map_err(|e| {
        anyhow::anyhow!(e).context(
            "打开调试器失败。Windows 下 probe-rs 需要 WinUSB 驱动 \
             (Zadig 替换 SEGGER 驱动;详见分支说明)",
        )
    })?;
    // 目标电压是调试器能力,必须在 attach 前从 Probe 上读(Session 不暴露)
    if let Ok(Some(v)) = probe.get_target_voltage() {
        println!("[probe] 目标电压 {v:.2} V");
    }
    probe.select_protocol(WireProtocol::Swd)?;
    probe.set_speed(args.speed_khz)?;
    println!(
        "[probe] attach {chip}(SWD,{}kHz,默认模式:不复位/不下载)…",
        args.speed_khz
    );
    Ok(probe.attach_with_registry(chip, Permissions::default(), registry)?)
}

/// attach → 挂 RTT(全 RAM 扫描)→ 返回 (session, rtt)
fn attach_rtt_checked(
    args: &Args,
    chip: &str,
    registry: &Registry,
) -> anyhow::Result<(Session, Rtt)> {
    let mut session = open_session(args, chip, registry)?;
    let mut core = session.core(0).context("打开核心 0 失败")?;
    println!(
        "[rtt] 扫描 RAM 找 \"SEGGER RTT\" 控制块(通道 {})…",
        args.channel
    );
    let mut rtt = match Rtt::attach(&mut core) {
        Ok(rtt) => rtt,
        Err(e) => {
            anyhow::bail!(
                "找不到 RTT 控制块: {e}。请确认固件已初始化 RTT(控制块须驻留 RAM)"
            );
        }
    };
    println!(
        "[rtt] 已挂上:{} 个 Up 通道,{} 个 Down 通道",
        rtt.up_channels().len(),
        rtt.down_channels().len()
    );
    drop(core); // 结束借用,session 才能移交给调用方
    Ok((session, rtt))
}

/// `rtt <芯片> [秒数]`:收 N 秒 RTT 数据直通 stdout
fn rtt(args: &Args) -> anyhow::Result<()> {
    let chip = args.chip.clone().expect("parse_args 已保证");
    let registry = Registry::from_builtin_families();
    let (mut session, mut rtt) = attach_rtt_checked(args, &chip, &registry)?;
    let mut core = session.core(0)?;

    if rtt.up_channel(args.channel).is_none() {
        anyhow::bail!("Up 通道 {} 不存在(固件未注册)", args.channel);
    }
    if let Some(down) = rtt.down_channel(args.channel) {
        println!(
            "[rtt] Down 通道 {} 存在(缓冲 {} 字节)",
            args.channel,
            down.buffer_size()
        );
    } else {
        println!(
            "[rtt] Down 通道 {} 不存在(send 子命令将无法写入)",
            args.channel
        );
    }

    let mut buf = [0u8; 4096];
    let mut carry: Vec<u8> = Vec::new();
    let start = Instant::now();
    let mut total = 0usize;
    println!("---- RTT 输出开始({chip},{}s)----", args.secs);
    while start.elapsed() < Duration::from_secs(args.secs) {
        // 每次循环重新借通道(read 需要 &mut)
        let n = match rtt.up_channel(args.channel) {
            Some(up) => up.read(&mut core, &mut buf)?,
            None => anyhow::bail!("Up 通道 {} 消失了", args.channel),
        };
        if n > 0 {
            total += n;
            print!("{}", decode_utf8_incremental(&mut carry, &buf[..n]));
            std::io::stdout().flush().ok();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("\n---- RTT 输出结束 ---- 共 {total} 字节");
    // session 出作用域自动 Drop:清理断点 + 核心去配置(0.32 无 detach)
    Ok(())
}

/// `send <芯片> <文本>`:向 Down 通道写一行(自动补 \n)
fn send(args: &Args) -> anyhow::Result<()> {
    let chip = args.chip.clone().expect("parse_args 已保证");
    let registry = Registry::from_builtin_families();
    let (mut session, mut rtt) = attach_rtt_checked(args, &chip, &registry)?;
    let mut core = session.core(0)?;
    let down = rtt
        .down_channel(args.channel)
        .with_context(|| format!("Down 通道 {} 不存在(固件未注册)", args.channel))?;
    let mut payload = args.text.clone().into_bytes();
    payload.push(b'\n');
    let n = down.write(&mut core, &payload)?;
    println!(
        "[ok] 已向 Down 通道 {} 写入 {n} 字节: {:?}",
        args.channel, args.text
    );
    Ok(())
}

/// `reset <芯片>`:复位并运行(等价旧后端 Reset+Go 两步)
fn reset(args: &Args) -> anyhow::Result<()> {
    let chip = args.chip.clone().expect("parse_args 已保证");
    let registry = Registry::from_builtin_families();
    let mut session = open_session(args, &chip, &registry)?;
    let mut core = session.core(0)?;
    core.reset().context("复位失败")?;
    println!("[ok] 目标已复位并恢复运行");
    // 复位后稍候再退出:验证"复位后干净脱钩,目标独立跑"的语义(session drop 清理)
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}
