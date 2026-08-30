//! `--demo-log` 演示/测试数据源:无设备时验证滚动、断行、UTF-8/ANSI 颜色渲染。
//!
//! - 跳过单实例互斥由 main 负责:demo 模式允许与真实实例并存(它不加载
//!   JLinkARM.dll,不会抢 J-Link)
//! - 发送打微秒时间戳(stderr),用于测量显示节奏是否与发送节奏一致;
//!   非 demo 模式 `T0` 为空,零开销

use crate::rtt::{WorkerMsg, APP_SHUTDOWN};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// demo 模式的时间基准(微秒时间戳测量用)
pub static T0: OnceLock<Instant> = OnceLock::new();

const INTERVAL_MS: u64 = 80;

pub fn is_enabled(args: &[String]) -> bool {
    args.iter().any(|a| a == "--demo-log")
}

/// 启动演示数据线程:中英混排 + emoji + ANSI 颜色
/// (行号青色、心跳值绿、每 10 条一次红色 ERROR 样式,覆盖解析的主路径)
pub fn spawn(msg_tx: mpsc::Sender<WorkerMsg>) {
    let _ = T0.set(Instant::now());
    std::thread::spawn(move || {
        let mut i: u64 = 0;
        loop {
            if APP_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let value = if i % 10 == 9 {
                format!("\x1b[31m{i}\x1b[0m")
            } else {
                format!("\x1b[32m{i}\x1b[0m")
            };
            let level = if i % 10 == 9 { " \x1b[41mERR\x1b[0m" } else { "" };
            let _ = msg_tx.send(WorkerMsg::Log(format!(
                "\x1b[36m[demo {i:04}]\x1b[0m Heartbeat: {value} 😊🍟❤ 心跳 中文 English mixed padding text{level}\r\n"
            )));
            if let Some(t0) = T0.get() {
                eprintln!("[tx] i={i} t={}us", t0.elapsed().as_micros());
            }
            i += 1;
            std::thread::sleep(Duration::from_millis(INTERVAL_MS));
        }
    });
}
