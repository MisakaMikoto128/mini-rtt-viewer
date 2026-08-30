// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ansi;
mod demo;
mod device_db;
mod jlink_dll;
mod log_model;
mod rtt;
mod single_instance;

use log_model::{LogPump, DEFAULT_FRAME_TIMEOUT_MS, FLUSH_MS};
use rtt::{WorkerCmd, WorkerHandle, WorkerMsg, APP_SHUTDOWN};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

slint::include_modules!();

const SPEEDS_KHZ: [u32; 8] = [100, 200, 500, 1000, 2000, 4000, 8000, 12000];

/// 默认日志前景色(Run.fg == None 时用)
fn log_default_fg() -> slint::Color {
    slint::Color::from_rgb_u8(0xdd, 0xdd, 0xdd)
}

/// 重建设备下拉候选:按输入大小写不敏感过滤(候选由 EditableCombo 的
/// 原生下拉展示,选中后回填输入框,Rust 无需维护选中态)
fn apply_device_filter(ui: &AppWindow, full: &[SharedString], needle: &str) {
    let n = needle.trim().to_uppercase();
    let list: Vec<SharedString> = if n.is_empty() {
        full.to_vec()
    } else {
        full.iter()
            .filter(|s| s.to_uppercase().contains(&n))
            .cloned()
            .collect()
    };
    ui.set_device_names(slint::ModelRc::new(VecModel::from(list)));
}

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
    // 枚举协议:Send=发数据,Power=电源输出开关
    let cmd_tx: Rc<RefCell<Option<mpsc::Sender<WorkerCmd>>>> = Rc::new(RefCell::new(None));
    // 连接互斥门闩:worker 线程存活期间(含阻塞在 connect() 时)不允许 spawn 新 worker
    let worker: Rc<RefCell<Option<Arc<WorkerHandle>>>> = Rc::new(RefCell::new(None));
    // 设备库全量名单(后台线程枚举/磁盘缓存回传),筛选下拉按输入重建
    let device_names: Rc<RefCell<Vec<SharedString>>> = Rc::new(RefCell::new(Vec::new()));
    // 本机接入的 J-Link (序列号, 显示名);下拉选中项在连接时换算成 selected_sn
    let jlinks: Rc<RefCell<Vec<(u32, String)>>> = Rc::new(RefCell::new(Vec::new()));

    // 断帧间隔共享变量:UI 改输入框 → worker 实时读取(断帧判定在 worker,5ms 精度)
    let frame_timeout_ms = Arc::new(AtomicU32::new(DEFAULT_FRAME_TIMEOUT_MS));
    // 日志消息泵:worker 消息 → 断行 → ANSI 带色行(逻辑在 log_model,可单测)
    let pump = Rc::new(RefCell::new(LogPump::default()));
    // 日志行模型(ListView 虚拟化;只在启动时装入一次,此后增量 push)
    let log_rows = Rc::new(VecModel::from(Vec::<LogRow>::new()));
    app.set_log_rows(ModelRc::from(log_rows.clone()));
    // 最近一次 worker 状态文案(清空日志后恢复用,避免状态栏退化成无参数的"已连接")
    let last_status: Rc<RefCell<SharedString>> = Rc::new(RefCell::new("● 未连接".into()));

    if demo_mode {
        demo::spawn(msg_tx.clone());
    } else {
        // 后台枚举:目标设备库候选(有磁盘缓存则零 DLL 调用)+ 本机接入的 J-Link 列表。
        // device_db 不依赖 WorkerMsg,这里用转发线程适配消息类型
        let (db_tx, db_rx) = mpsc::channel::<device_db::DbResult>();
        device_db::spawn_background(db_tx);
        let msg_tx = msg_tx.clone();
        std::thread::spawn(move || {
            while let Ok(r) = db_rx.recv() {
                let msg = match r {
                    device_db::DbResult::DeviceNames(names) => WorkerMsg::DeviceNames(names),
                    device_db::DbResult::Emulators(list) => WorkerMsg::JLinks(list),
                };
                if msg_tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }

    let timer = Timer::default();
    {
        let weak = app.as_weak();
        let pump = pump.clone();
        let worker = worker.clone();
        let last_status = last_status.clone();
        let frame_timeout_ms = frame_timeout_ms.clone();
        let device_names = device_names.clone();
        let jlinks = jlinks.clone();
        let log_rows = log_rows.clone();
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
                    Ok(WorkerMsg::DeviceInfo(info)) => {
                        // 设备信息区(字段对齐原 PySide6 工程;空字段 UI 显示 "—")
                        let rows = vec![
                            InfoRow { label: "固件版本".into(), value: info.firmware.into() },
                            InfoRow { label: "硬件版本".into(), value: info.hardware.into() },
                            InfoRow { label: "序列号".into(), value: info.serial.into() },
                            InfoRow { label: "核心名称".into(), value: info.core_name.into() },
                            InfoRow { label: "核心 ID".into(), value: info.core_id.into() },
                            InfoRow { label: "CPU 类型".into(), value: info.core_cpu.into() },
                            InfoRow { label: "目标设备".into(), value: info.target.into() },
                            InfoRow { label: "接口".into(), value: info.iface.into() },
                            InfoRow { label: "速度(kHz)".into(), value: info.speed_khz.to_string().into() },
                        ];
                        ui.set_info_rows(slint::ModelRc::new(VecModel::from(rows)));
                    }
                    Ok(WorkerMsg::DeviceNames(names)) => {
                        let full: Vec<SharedString> =
                            names.into_iter().map(SharedString::from).collect();
                        apply_device_filter(&ui, &full, &ui.get_chip_name());
                        *device_names.borrow_mut() = full;
                    }
                    Ok(WorkerMsg::JLinks(list)) => {
                        // 更新 J-Link 下拉;选中索引夹回范围,显示名 "产品: 序列号"
                        let mut descs: Vec<SharedString> = Vec::with_capacity(list.len());
                        for (sn, product) in &list {
                            if product.is_empty() {
                                descs.push(format!("J-Link: {sn}").into());
                            } else {
                                descs.push(format!("{product}: {sn}").into());
                            }
                        }
                        let cur = ui.get_jlink_index();
                        ui.set_jlink_names(slint::ModelRc::new(VecModel::from(descs)));
                        ui.set_jlink_index(if cur < list.len() as i32 && cur >= 0 {
                            cur
                        } else if list.is_empty() {
                            -1
                        } else {
                            0
                        });
                        *jlinks.borrow_mut() = list;
                    }
                    Ok(WorkerMsg::Exited) => {
                        // worker 真正退出(含 DLL close),解锁"再连接";
                        // 电源输出随连接一起失效(DLL close 会断电)
                        *worker.borrow_mut() = None;
                        ui.set_connecting(false);
                        ui.set_connected(false);
                        ui.set_power_output(false);
                    }
                    Err(_) => break,
                }
            }
            // 2. 单行长度兜底(超长帧/关闭自动断帧时的无换行流)
            pump.enforce_line_cap();
            // 3. 增量上屏:新行 → 行模型 push(ANSI 已在 pump 内解析为带色段;
            //    ListView 虚拟化只渲染可见行,长日志不全量重排)
            if let Some(rows) = pump.take_new_rows() {
                for runs in rows {
                    let spans: Vec<LogRun> = runs
                        .into_iter()
                        .map(|r| LogRun {
                            text: r.text.into(),
                            color: r
                                .fg
                                .map(|(r8, g8, b8)| slint::Color::from_rgb_u8(r8, g8, b8))
                                .unwrap_or_else(log_default_fg),
                        })
                        .collect();
                    log_rows.push(LogRow { runs: ModelRc::new(VecModel::from(spans)) });
                }
                ui.set_log_row_count(log_rows.row_count() as i32);
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
        let jlinks = jlinks.clone();
        app.on_connect_clicked(move || {
            if worker.borrow().as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed)) {
                return; // 上一个 worker 还活着(可能在阻塞 connect),严禁并发
            }
            *worker.borrow_mut() = None;
            let ui = weak.unwrap();
            // 首次启动设备库还在后台枚举:原 Python 项目踩过「枚举与 connect 并发
            // 损坏 DLL TLS」的坑,枚举期间拒绝连接(窗口仅数秒,且只发生在无缓存的首次)
            if device_db::busy() {
                ui.set_status_text("● 设备库加载中,请稍候…".into());
                return;
            }
            // chip 名去首尾空白;空名直接拒绝(空设备名会让 J-Link DLL 沿用上一次设备,行为不可预期)
            let chip = ui.get_chip_name().trim().to_string();
            if chip.is_empty() {
                ui.set_status_text("● 请先填写目标芯片型号".into());
                return;
            }
            ui.set_connecting(true);
            ui.set_status_text("● 连接中…".into());
            // 多台 J-Link:把下拉选中的序列号交给 worker(Open 前选定);未选中/空列表 = 自动
            let idx = ui.get_jlink_index();
            let selected_sn = jlinks
                .borrow()
                .get(idx as usize)
                .filter(|_| idx >= 0)
                .map(|(sn, _)| *sn);

            let (tx, rx) = mpsc::channel::<WorkerCmd>();
            *cmd_tx.borrow_mut() = Some(tx);
            let handle = rtt::spawn(
                rtt::WorkerConfig {
                    chip,
                    iface_index: ui.get_iface_index() as usize,
                    speed_khz: SPEEDS_KHZ[ui.get_speed_index().clamp(0, 7) as usize],
                    channel: ui.get_channel() as u32,
                    frame_timeout_ms: frame_timeout_ms.clone(),
                    selected_sn,
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
                let _ = tx.send(WorkerCmd::Send(text + ending));
            }
        });
    }

    // 电源输出:仅连接状态下真正下发;未连接时勾选状态回弹
    {
        let weak = app.as_weak();
        let cmd_tx = cmd_tx.clone();
        app.on_power_toggled(move |on| {
            let ui = weak.unwrap();
            if !ui.get_connected() {
                ui.set_power_output(false);
                return;
            }
            if let Some(tx) = cmd_tx.borrow().as_ref() {
                let _ = tx.send(WorkerCmd::Power(on));
            }
        });
    }

    // 目标设备输入过滤:按输入重建下拉候选
    {
        let weak = app.as_weak();
        let device_names = device_names.clone();
        app.on_chip_filtered(move |text| {
            if let Some(ui) = weak.upgrade() {
                apply_device_filter(&ui, &device_names.borrow(), &text);
            }
        });
    }

    // 清空
    {
        let weak = app.as_weak();
        let pump = pump.clone();
        let log_rows = log_rows.clone();
        let last_status = last_status.clone();
        app.on_clear_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                log_rows.set_vec(vec![]);
                ui.set_log_row_count(0);
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
