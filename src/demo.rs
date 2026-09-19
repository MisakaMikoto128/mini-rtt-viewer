//! `--demo-log` 演示/测试数据源:无设备时验证滚动、断行、UTF-8/ANSI 颜色渲染。
//!
//! - 跳过单实例互斥由 main 负责:demo 模式允许与真实实例并存(它不加载
//!   JLinkARM.dll,不会抢 J-Link)
//! - 发送打微秒时间戳(stderr),用于测量显示节奏是否与发送节奏一致;
//!   非 demo 模式 `T0` 为空,零开销
//!
//! ## 虚拟 worker 命令协议(F2)
//! demo 线程是真机 worker 的无设备替身:消费 `WorkerCmd`(连接/断开/重置/电源),
//! 数据**仅在连接态产生**(与真机断开无 RTT 数据一致),断帧判定与真机 worker
//! 同款(相邻数据间隔超过断帧超时 → FrameEnd,自动断帧开关在 pump 侧生效)。
//!
//! - 自动周期(无手动干预时):启动即连接 → 20s 断开 → 3s 重连,循环;
//! - 手动「断开」→ State(false) 且**不再自动重连**;手动「连接」→ 立即
//!   State(true) 且**不再自动断开**(忠实模拟真机:连接态保持到用户断开);
//! - 重置 → 日志行 + State(false) + ~800ms 后 State(true)(短暂断开重挂);
//! - 电源 → 日志行反馈(demo 无法真供电);Send → 忽略(web 层已计数+回显)。

use crate::rtt::{WorkerCmd, WorkerMsg, APP_SHUTDOWN};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
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

pub fn is_enabled(args: &[String]) -> bool {
    args.iter().any(|a| a == "--demo-log")
}

/// 启动演示数据线程:中英混排 + emoji + ANSI 颜色
/// (行号青色、心跳值绿、每 10 条一次红色 ERROR 样式,覆盖解析的主路径)。
/// 同时模拟连接/断开循环(启动即连接,每 20s 断开 3s 再重连;手动干预后
/// 周期挂起,见模块头)——State(true/false) 消息会驱动 tick 里的连接分支
/// (统计清零/自动标记),demo 冒烟因此覆盖真机连接路径,不再只测数据流。
pub fn spawn(
    msg_tx: mpsc::Sender<WorkerMsg>,
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    frame_timeout_ms: Arc<AtomicU32>,
) {
    let _ = T0.set(Instant::now());
    std::thread::spawn(move || {
        let mut i: u64 = 0;
        // 启动即连接(与 web.rs 的启动种子一致,首屏即有数据/可发送)
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
        loop {
            if APP_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            // 1) 命令消费(虚拟 worker 的 Connect/Disconnect/Reset/Power)
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    WorkerCmd::DemoConnect => {
                        manual = true;
                        disconnect_at = None;
                        reconnect_at = None;
                        reset_at = None;
                        if !connected {
                            let _ = msg_tx.send(WorkerMsg::State(true, "● 已连接 (demo)".into()));
                            connected = true;
                            last_rx = Instant::now();
                            frame_open = false;
                        }
                    }
                    WorkerCmd::DemoDisconnect => {
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
            // 2) 状态迁移(手动模式下无任何自动迁移)
            let now = Instant::now();
            if let Some(t) = reset_at {
                if now >= t {
                    let _ = msg_tx.send(WorkerMsg::State(true, "● 已连接 (demo)".into()));
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
                        let _ = msg_tx.send(WorkerMsg::State(true, "● 已连接 (demo)".into()));
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
                    // 前景红(31)而非背景红(41):ansi.rs 暂不解析背景 SGR(记 backlog),
                    // 背景码会被吞掉导致 ERR 无色
                    " \x1b[31mERR\x1b[0m"
                } else {
                    ""
                };
                // Block 而非 Log:demo 模拟的是设备输出流(计入 RX 统计),
                // Log 是 J-Link 横幅语义,不参与统计
                let _ = msg_tx.send(WorkerMsg::Block(format!(
                    "\x1b[36m[demo {i:04}]\x1b[0m Heartbeat: {value} 😊🍟❤ 心跳 中文 English mixed padding text{level}\r\n"
                )));
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
