// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mini_rtt_viewer::rtt::{self, APP_SHUTDOWN, WorkerHandle};
use rtt::WorkerMsg;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

slint::include_modules!();

const SPEEDS_KHZ: [u32; 8] = [100, 200, 500, 1000, 2000, 4000, 8000, 12000];
const FLUSH_MS: u64 = 50;
const DEFAULT_FRAME_TIMEOUT_MS: u128 = 100; // 设备不发换行符时,静默超时自动断帧
const MAX_LINE_CHARS: usize = 256; // 单行硬上限:积压数据批量到达时强制切行,防止超长行
const MAX_LINES: usize = 3000;

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
        // CreateMutexW(lpMutexAttributes, bInitialOwner, lpName) -> HANDLE
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

    // 行式日志模型:worker 文本 → 断帧成行 → VecModel,ListView 虚拟化渲染
    let lines: Rc<VecModel<SharedString>> = Rc::new(VecModel::default());
    app.set_log_lines(ModelRc::from(Rc::clone(&lines) as Rc<VecModel<SharedString>>));

    // 半行缓冲(等待换行符/断帧超时的未完成行)
    let pending: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let last_data: Rc<RefCell<Instant>> = Rc::new(RefCell::new(Instant::now()));
    // 最近一次 worker 状态文案(清空日志后恢复用,避免状态栏退化成无参数的"已连接")
    let last_status: Rc<RefCell<SharedString>> = Rc::new(RefCell::new("● 未连接".into()));

    let timer = Timer::default();
    {
        let weak = app.as_weak();
        let pending = pending.clone();
        let last_data = last_data.clone();
        let lines = lines.clone();
        let worker = worker.clone();
        let last_status = last_status.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(FLUSH_MS), move || {
            let ui = match weak.upgrade() { Some(u) => u, None => return };
            // 1. 消化 worker 消息
            loop {
                match msg_rx.try_recv() {
                    Ok(WorkerMsg::Log(text)) => {
                        *last_data.borrow_mut() = Instant::now();
                        pending.borrow_mut().push_str(&text);
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
            // 2. 断帧:完整行立即入模型;半行超过静默超时或超过单行上限也入模型
            let frame_timeout = if ui.get_auto_frame() {
                ui.get_frame_timeout().trim().parse::<u128>().unwrap_or(DEFAULT_FRAME_TIMEOUT_MS)
            } else {
                u128::MAX // 关闭自动断帧:半行一直等到换行符或单行上限
            };
            let mut pending = pending.borrow_mut();
            let mut new_lines: Vec<SharedString> = Vec::new();
            while let Some(pos) = pending.find('\n') {
                let line: String = pending.drain(..pos + 1).collect();
                new_lines.push(line.trim_end_matches(['\r', '\n']).into());
            }
            // 半行:静默超时切行;或长度超上限立即切(积压数据单块到达的场景)
            if !pending.is_empty() {
                let idle = last_data.borrow().elapsed().as_millis() > frame_timeout;
                let mut chars: Vec<char> = pending.chars().collect();
                if chars.len() > MAX_LINE_CHARS {
                    let tail: String = chars.split_off(MAX_LINE_CHARS).into_iter().collect();
                    let line = std::mem::replace(&mut *pending, tail);
                    new_lines.push(line.into());
                } else if idle {
                    new_lines.push(std::mem::take(&mut *pending).into());
                }
            }
            drop(pending);

            if !new_lines.is_empty() {
                // 自动滚动判断要在插入前做:插入后再判断,viewport 高度已更新,永远"在底部"
                let at_bottom = ui.get_log_viewport_y()
                    >= -(ui.get_log_viewport_height() - ui.get_log_area_height()) - 4.0;
                for l in new_lines {
                    lines.push(l);
                }
                while lines.row_count() > MAX_LINES {
                    lines.remove(0);
                }
                if at_bottom {
                    let area = ui.get_log_area_height();
                    let vh = ui.get_log_viewport_height();
                    ui.set_log_viewport_y((-(vh - area)).min(0.0) as f32);
                }
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
            let handle = rtt::spawn(chip, iface, speed, channel, msg_tx.clone(), rx);
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
            if let Some(tx) = cmd_tx.borrow().as_ref() {
                let _ = tx.send(text + "\r\n");
            }
        });
    }

    // 清空
    {
        let weak = app.as_weak();
        let lines = lines.clone();
        let pending = pending.clone();
        let last_status = last_status.clone();
        app.on_clear_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_log_viewport_y(0.0);
                ui.set_status_text(last_status.borrow().clone());
            }
            pending.borrow_mut().clear();
            while lines.row_count() > 0 {
                lines.remove(0);
            }
        });
    }

    // 复制全部日志到剪贴板
    {
        let weak = app.as_weak();
        let lines = lines.clone();
        app.on_copy_log_clicked(move || {
            let ui = weak.unwrap();
            let n = lines.row_count();
            let mut text = String::new();
            for i in 0..n {
                text.push_str(lines.row_data(i).unwrap_or_default().as_str());
                text.push('\n');
            }
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                Ok(()) => ui.set_status_text(format!("● 已复制 {n} 行到剪贴板").into()),
                Err(e) => ui.set_status_text(format!("● 复制失败: {e}").into()),
            }
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
