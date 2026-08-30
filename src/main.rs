// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod demo;
mod jlink_dll;
mod log_model;
mod rtt;
mod single_instance;

use log_model::{LogPump, DEFAULT_FRAME_TIMEOUT_MS, FLUSH_MS};
use rtt::{WorkerHandle, WorkerMsg, APP_SHUTDOWN};
use slint::{ComponentHandle, SharedString, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

slint::include_modules!();

const SPEEDS_KHZ: [u32; 8] = [100, 200, 500, 1000, 2000, 4000, 8000, 12000];

fn main() -> anyhow::Result<()> {
    // --demo-log 是无设备的自动化测试模式:跳过单实例互斥,
    // 允许与真实实例并存(它不加载 JLinkARM.dll,不会抢 J-Link)
    let demo_mode = demo::is_enabled(&std::env::args().collect::<Vec<_>>());
    if !demo_mode {
        single_instance::enforce_single_instance();
    }
    let app = AppWindow::new()?;

    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    // 每次连接新建一条命令管道;UI 持有"当前 worker 的 sender"。
    let cmd_tx: Rc<RefCell<Option<mpsc::Sender<String>>>> = Rc::new(RefCell::new(None));
    // 连接互斥门闩:worker 线程存活期间(含阻塞在 connect() 时)不允许 spawn 新 worker
    let worker: Rc<RefCell<Option<Arc<WorkerHandle>>>> = Rc::new(RefCell::new(None));

    // 断帧间隔共享变量:UI 改输入框 → worker 实时读取(断帧判定在 worker,5ms 精度)
    let frame_timeout_ms = Arc::new(AtomicU32::new(DEFAULT_FRAME_TIMEOUT_MS));
    // 日志消息泵:worker 消息 → 断行 → 展示文本(逻辑在 log_model,可单测)
    let pump = Rc::new(RefCell::new(LogPump::default()));
    // 最近一次 worker 状态文案(清空日志后恢复用,避免状态栏退化成无参数的"已连接")
    let last_status: Rc<RefCell<SharedString>> = Rc::new(RefCell::new("● 未连接".into()));

    if demo_mode {
        demo::spawn(msg_tx.clone());
    }

    let timer = Timer::default();
    {
        let weak = app.as_weak();
        let pump = pump.clone();
        let worker = worker.clone();
        let last_status = last_status.clone();
        let frame_timeout_ms = frame_timeout_ms.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(FLUSH_MS), move || {
            let ui = match weak.upgrade() { Some(u) => u, None => return };
            // 0. 把输入框的断帧间隔同步给 worker(判定在 worker,精度 5ms)
            if ui.get_auto_frame() {
                let v = ui
                    .get_frame_timeout()
                    .trim()
                    .parse::<u32>()
                    .unwrap_or(DEFAULT_FRAME_TIMEOUT_MS);
                frame_timeout_ms.store(v.clamp(1, 200), Ordering::Relaxed);
            }
            // 接收行尾:0=自动 1=CRLF 2=LF 3=CR 4=无
            let rx_ending = ui.get_rx_ending();
            let mut pump = pump.borrow_mut();
            // 1. 消化 worker 消息
            loop {
                match msg_rx.try_recv() {
                    Ok(WorkerMsg::Log(text) | WorkerMsg::Block(text)) => {
                        // 暂停接收:数据直接丢弃
                        if !pump.paused {
                            pump.absorb_text(&text, rx_ending);
                        }
                    }
                    Ok(WorkerMsg::FrameEnd) => {
                        // worker 判定一帧结束(间隔超过断帧超时):切出缓冲为完整行
                        if !pump.paused && ui.get_auto_frame() {
                            pump.absorb_frame_end(rx_ending);
                        }
                    }
                    Ok(WorkerMsg::Progress(text)) => {
                        // 连接过程进度:只刷状态栏文字,绝不改变连接标志(防按钮闪烁)
                        ui.set_status_text(text.into());
                    }
                    Ok(WorkerMsg::State(connected, status)) => {
                        ui.set_connected(connected);
                        if !connected {
                            ui.set_connecting(false);
                        }
                        *last_status.borrow_mut() = status.clone().into();
                        ui.set_status_text(status.into());
                    }
                    Ok(WorkerMsg::Exited) => {
                        // worker 真正退出(含 DLL close),解锁"再连接"
                        *worker.borrow_mut() = None;
                        ui.set_connecting(false);
                        ui.set_connected(false);
                    }
                    Err(_) => break,
                }
            }
            // 2. 单行长度兜底(超长帧/关闭自动断帧时的无换行流)
            pump.enforce_line_cap();
            // 3. 有新行才写文本;贴底跟随/上翻失随由 LogView 内部闭环
            if let Some(text) = pump.take_text() {
                demo::trace_flush(text.lines().count());
                ui.set_log_text(text.into());
            }
        });
    }

    // 连接
    {
        let weak = app.as_weak();
        let cmd_tx = cmd_tx.clone();
        let worker = worker.clone();
        let msg_tx = msg_tx.clone();
        let frame_timeout_ms = frame_timeout_ms.clone();
        app.on_connect_clicked(move || {
            if worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed)) {
                return; // 上一个 worker 还活着(可能在阻塞 connect),严禁并发
            }
            *worker.borrow_mut() = None;
            let ui = weak.unwrap();
            // chip 名去首尾空白;空名直接拒绝(空设备名会让 J-Link DLL 沿用上一次设备,行为不可预期)
            let chip = ui.get_chip_name().trim().to_string();
            if chip.is_empty() {
                ui.set_status_text("● 请先填写目标芯片型号".into());
                return;
            }
            ui.set_connecting(true);
            ui.set_status_text("● 连接中…".into());

            let (tx, rx) = mpsc::channel::<String>();
            *cmd_tx.borrow_mut() = Some(tx);
            let handle = rtt::spawn(
                rtt::WorkerConfig {
                    chip,
                    iface_index: ui.get_iface_index() as usize,
                    speed_khz: SPEEDS_KHZ[ui.get_speed_index().clamp(0, 7) as usize],
                    channel: ui.get_channel() as u32,
                    frame_timeout_ms: frame_timeout_ms.clone(),
                },
                msg_tx.clone(),
                rx,
            );
            *worker.borrow_mut() = Some(handle);
        });
    }

    // 断开/取消连接:置停止标志,等 worker 的 Exited 消息回到未连接态。
    // 不在此处清 worker 句柄 —— worker 可能还阻塞在 DLL 调用里,此刻 spawn 新
    // worker 会并发抢 J-Link(数据损坏 + 状态错乱的根源)。
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let cmd_tx = cmd_tx.clone();
        app.on_disconnect_clicked(move || {
            if let Some(h) = worker.borrow().as_ref() {
                h.stop.store(true, Ordering::Relaxed);
            }
            *cmd_tx.borrow_mut() = None; // 掐断旧管道,worker try_recv 后自行退出
            let ui = weak.unwrap();
            ui.set_connecting(true);
            ui.set_status_text("● 断开中…".into());
        });
    }

    // 发送
    {
        let weak = app.as_weak();
        let cmd_tx = cmd_tx.clone();
        app.on_send_clicked(move || {
            let ui = weak.unwrap();
            let text = ui.get_send_text().to_string();
            if text.is_empty() {
                return;
            }
            let ending = match ui.get_send_ending() {
                1 => "\n",
                2 => "\r",
                3 => "",
                _ => "\r\n",
            };
            if let Some(tx) = cmd_tx.borrow().as_ref() {
                let _ = tx.send(text + ending);
            }
        });
    }

    // 清空
    {
        let weak = app.as_weak();
        let pump = pump.clone();
        let last_status = last_status.clone();
        app.on_clear_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_log_text("".into());
                ui.set_status_text(last_status.borrow().clone());
            }
            pump.borrow_mut().clear();
        });
    }

    // 暂停/继续接收:暂停期间 worker 读到的新数据直接丢弃(不进日志、不占缓冲)
    {
        let weak = app.as_weak();
        let pump = pump.clone();
        let last_status = last_status.clone();
        app.on_pause_toggled(move || {
            let ui = weak.unwrap();
            let now = !ui.get_paused();
            ui.set_paused(now);
            pump.borrow_mut().paused = now;
            ui.set_status_text(if now {
                "● 已暂停接收(新数据被丢弃)".into()
            } else {
                last_status.borrow().clone()
            });
        });
    }

    app.run()?;

    // 应用退出:通知 worker 停止,等它清理完 DLL;超时强制退出,不留僵尸进程
    APP_SHUTDOWN.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let alive = worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed));
        if !alive {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed)) {
        // worker 卡死在不可中断的 DLL 调用里(如模态弹窗):强制退出,宁可不优雅也不留僵尸
        std::process::exit(0);
    }
    Ok(())
}
