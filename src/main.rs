// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mini_rtt_viewer::rtt::{self, APP_SHUTDOWN, WorkerHandle};
use rtt::WorkerMsg;
use slint::{ComponentHandle, SharedString, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

slint::include_modules!();

const SPEEDS_KHZ: [u32; 8] = [100, 200, 500, 1000, 2000, 4000, 8000, 12000];
const FLUSH_MS: u64 = 200;
const DEFAULT_FRAME_TIMEOUT_MS: u128 = 20; // 断帧间隔默认值(1~200ms)
const MAX_LINE_CHARS: usize = 512; // 单行硬上限兜底
const MAX_LOG_CHARS: usize = 60_000; // 日志文本上限(只读文本全量渲染,超限丢最旧)

/// 按接收行尾模式切行:0=自动(\n 断行、吞 \r) 1=CRLF 2=LF 3=CR 4=无(不断行)
fn split_lines(p: &mut String, rx_ending: i32, out: &mut Vec<String>) {
    let pat: &str = match rx_ending {
        1 => "\r\n",
        2 => "\n",
        3 => "\r",
        4 => return,
        _ => "\n",
    };
    while let Some(pos) = p.find(pat) {
        let line: String = p.drain(..pos + pat.len()).collect();
        let line = if rx_ending == 0 || rx_ending == 2 {
            line.trim_end_matches('\r').to_string()
        } else {
            line
        };
        out.push(line.into());
    }
}

/// 单实例互斥:第二个实例弹窗提示后退出。
/// 不做互斥的话两个进程会同时连同一个 J-Link(数据各收一份,状态互相干扰)。
#[cfg(windows)]
fn enforce_single_instance() {
    use libloading::{Library, Symbol};
    use std::ffi::c_void;
    let kernel32 = match unsafe { Library::new("kernel32.dll") } {
        Ok(l) => l,
        Err(_) => return, // 拿不到 kernel32 不合理,但不要因此挡启动
    };
    unsafe {
        // 先取齐函数指针:libloading 的 get() 内部会调 Win32 API 清掉 GetLastError,
        // 所以 GetLastError 必须在 CreateMutexW 之后立刻调用
        let create: Symbol<unsafe extern "C" fn(*mut c_void, i32, *const u16) -> *mut c_void> =
            kernel32.get(b"CreateMutexW").unwrap();
        let last_err: Symbol<unsafe extern "C" fn() -> u32> =
            kernel32.get(b"GetLastError").unwrap();
        let name: Vec<u16> = "Local\\MiniRttViewerSingleInstance\0".encode_utf16().collect();
        let _mutex_guard = create(std::ptr::null_mut(), 0, name.as_ptr());
        if last_err() == 183 {
            // ERROR_ALREADY_EXISTS:已有实例在跑。mutex 故意不释放,随进程存活
            let user32 = match unsafe { Library::new("user32.dll") } {
                Ok(l) => l,
                Err(_) => std::process::exit(0),
            };
            let msgbox: Symbol<
                unsafe extern "C" fn(*mut c_void, *const u16, *const u16, u32) -> i32,
            > = user32.get(b"MessageBoxW").unwrap();
            let title: Vec<u16> = "Mini RTT Viewer\0".encode_utf16().collect();
            let msg: Vec<u16> =
                "Mini RTT Viewer 已经在运行。\0".encode_utf16().collect();
            msgbox(
                std::ptr::null_mut(),
                msg.as_ptr(),
                title.as_ptr(),
                0x30, // MB_ICONWARNING
            );
            std::mem::forget(user32);
            std::process::exit(0);
        }
        // 保持 kernel32 Library 活到进程结束(句柄泄漏即设计)
        std::mem::forget(kernel32);
    }
}

#[cfg(not(windows))]
fn enforce_single_instance() {}

fn main() -> anyhow::Result<()> {
    enforce_single_instance();
    let app = AppWindow::new()?;

    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    // 每次连接新建一条命令管道;UI 持有"当前 worker 的 sender"。
    let cmd_tx: Rc<RefCell<Option<mpsc::Sender<String>>>> = Rc::new(RefCell::new(None));
    // 连接互斥门闩:worker 线程存活期间(含阻塞在 connect() 时)不允许 spawn 新 worker
    let worker: Rc<RefCell<Option<Arc<WorkerHandle>>>> = Rc::new(RefCell::new(None));

    // 日志文本缓冲:只读 TextEdit 全量渲染,超限丢最旧(按行边界)
    let log_buf: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    // 半行缓冲(等待换行符/帧结束标记的未完成行)
    let pending: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    // 断帧间隔共享变量:UI 改输入框 → worker 实时读取(断帧判定在 worker,5ms 精度)
    let frame_timeout_ms = Arc::new(AtomicU32::new(20));
    // 暂停接收:置位后新数据直接丢弃,不进日志
    let paused: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    // 最近一次 worker 状态文案(清空日志后恢复用,避免状态栏退化成无参数的"已连接")
    let last_status: Rc<RefCell<SharedString>> = Rc::new(RefCell::new("● 未连接".into()));

    let timer = Timer::default();
    {
        let weak = app.as_weak();
        let pending = pending.clone();
        let paused = paused.clone();
        let log_buf = log_buf.clone();
        let worker = worker.clone();
        let last_status = last_status.clone();
        let frame_timeout_ms = frame_timeout_ms.clone();
        // 断行切出的完整行(换行符/帧结束处切),tick 末统一刷新到 TextEdit
        let new_lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        timer.start(TimerMode::Repeated, Duration::from_millis(FLUSH_MS), move || {
            let ui = match weak.upgrade() { Some(u) => u, None => return };
            // 0. 把输入框的断帧间隔同步给 worker(判定在 worker,精度 5ms)
            if ui.get_auto_frame() {
                let v = ui.get_frame_timeout().trim().parse::<u32>().unwrap_or(20);
                frame_timeout_ms.store(v.clamp(1, 200), Ordering::Relaxed);
            }
            // 接收行尾:0=自动 1=CRLF 2=LF 3=CR 4=无
            let rx_ending = ui.get_rx_ending();
            // 1. 消化 worker 消息
            loop {
                match msg_rx.try_recv() {
                    Ok(WorkerMsg::Log(text)) => {
                        if *paused.borrow() {
                            continue; // 暂停接收:数据直接丢弃
                        }
                        let mut p = pending.borrow_mut();
                        p.push_str(&text);
                        split_lines(&mut p, rx_ending, &mut new_lines.borrow_mut());
                    }
                    Ok(WorkerMsg::Block(text)) => {
                        if *paused.borrow() {
                            continue; // 暂停接收:数据直接丢弃
                        }
                        pending.borrow_mut().push_str(&text);
                        split_lines(&mut pending.borrow_mut(), rx_ending, &mut new_lines.borrow_mut());
                    }
                    Ok(WorkerMsg::FrameEnd) => {
                        // worker 判定一帧结束(间隔超过断帧超时):切出缓冲为完整行
                        if *paused.borrow() || !ui.get_auto_frame() {
                            continue;
                        }
                        let mut p = pending.borrow_mut();
                        if !p.is_empty() {
                            split_lines(&mut p, rx_ending, &mut new_lines.borrow_mut());
                            if !p.is_empty() {
                                new_lines.borrow_mut().push(std::mem::take(&mut *p));
                            }
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
            {
                let mut p = pending.borrow_mut();
                if p.chars().count() > MAX_LINE_CHARS {
                    let mut chars: Vec<char> = p.chars().collect();
                    let tail: String =
                        chars.split_off(MAX_LINE_CHARS).into_iter().collect();
                    let line = std::mem::replace(&mut *p, tail);
                    new_lines.borrow_mut().push(line);
                }
            }

            let mut lines = new_lines.borrow_mut();
            if lines.is_empty() {
                drop(lines);
                return;
            }
            // 3. 追加到日志文本;超限按行丢最旧
            let at_bottom = ui.get_log_viewport_y()
                >= -(ui.get_log_viewport_height() - ui.get_log_area_height()) - 4.0;
            let mut buf = log_buf.borrow_mut();
            for l in lines.iter() {
                buf.push_str(l);
                buf.push('\n');
            }
            lines.clear();
            drop(lines);
            if buf.len() > MAX_LOG_CHARS {
                // 按字符边界 + 行边界截断到上限以内
                let cut = buf.len() - MAX_LOG_CHARS;
                let boundary = (cut..buf.len())
                    .find(|i| buf.is_char_boundary(*i) && buf.as_bytes()[*i] == b'\n')
                    .map(|i| i + 1)
                    .unwrap_or(buf.len());
                buf.drain(..boundary);
            }
            ui.set_log_text(buf.clone().into());
            drop(buf);
            // 4. 自动滚底:仅在插入前就位于底部时跟随(用户上翻不被拉回)
            if at_bottom {
                let area = ui.get_log_area_height();
                let vh = ui.get_log_viewport_height();
                ui.set_log_viewport_y((-(vh - area)).min(0.0) as f32);
            }
        });
    }

    // 连接
    {
        let weak = app.as_weak();
        let cmd_tx = cmd_tx.clone();
        let worker = worker.clone();
        let msg_tx = msg_tx.clone();
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

            let iface = ui.get_iface_index() as usize;
            let speed = SPEEDS_KHZ[ui.get_speed_index().clamp(0, 7) as usize];
            let channel = ui.get_channel() as u32;

            let (tx, rx) = mpsc::channel::<String>();
            *cmd_tx.borrow_mut() = Some(tx);
            let handle = rtt::spawn(
                chip,
                iface,
                speed,
                channel,
                frame_timeout_ms.clone(),
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
        let log_buf = log_buf.clone();
        let pending = pending.clone();
        let last_status = last_status.clone();
        app.on_clear_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_log_viewport_y(0.0);
                ui.set_log_text("".into());
                ui.set_status_text(last_status.borrow().clone());
            }
            pending.borrow_mut().clear();
            log_buf.borrow_mut().clear();
        });
    }

    // 暂停/继续接收:暂停期间 worker 读到的新数据直接丢弃(不进日志、不占缓冲)
    {
        let weak = app.as_weak();
        let paused = paused.clone();
        let last_status = last_status.clone();
        app.on_pause_toggled(move || {
            let ui = weak.unwrap();
            let now = !ui.get_paused();
            ui.set_paused(now);
            *paused.borrow_mut() = now;
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
