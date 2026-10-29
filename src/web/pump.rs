//! Worker 消息泵、日志广播、统计、偏好落盘与定时发送。

use super::*;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn SetThreadExecutionState(es_flags: u32) -> u32;
}

/// FR-24:只在有效状态翻变时调用 Win32；泵退出时由 Drop 撤销仍生效的请求。
struct ExecutionStateGuard {
    applied: Option<bool>,
}

impl ExecutionStateGuard {
    fn new() -> Self {
        Self { applied: None }
    }

    fn sync(&mut self, shared: &Shared) {
        let active =
            shared.keep_awake.load(Ordering::Relaxed) && shared.connected.load(Ordering::Relaxed);
        if self.applied != Some(active) {
            set_execution_state(active);
            self.applied = Some(active);
        }
    }
}

impl Drop for ExecutionStateGuard {
    fn drop(&mut self) {
        if self.applied == Some(true) {
            set_execution_state(false);
        }
    }
}

#[cfg(windows)]
fn set_execution_state(active: bool) {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
    let flags = if active {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    if unsafe { SetThreadExecutionState(flags) } == 0 {
        eprintln!("[mini-rtt-viewer] 更新系统常亮状态失败");
    }
}

#[cfg(not(windows))]
fn set_execution_state(_active: bool) {}

/// WS jlinks 事件的列表项 {sn, name}(与 /api/jlinks 输出同形)
#[derive(Serialize)]
pub(crate) struct JLinkEntry {
    pub(crate) sn: u32,
    pub(crate) name: String,
}

/// 单行 runs 包装:历史契约每行是 `{"runs":[{text,fg},…]}` **对象**而非裸数组
/// (Vec<Run> 直接序列化会丢掉 runs 键,这是替换时唯一需要显式建模的层级)
#[derive(Serialize)]
pub(crate) struct RunsRow<'a> {
    runs: &'a [ansi::Run],
}

/// 行数组 → runs 包装视图(借用,零文本拷贝;rows/snapshot 事件共用)
pub(crate) fn rows_view(rows: &[Vec<ansi::Run>]) -> Vec<RunsRow<'_>> {
    rows.iter().map(|r| RunsRow { runs: r }).collect()
}

/// 数据泵:消息消化 → cap → 增量上屏 → 事件广播;状态/设备信息同步进 Shared;
/// 偏好快照比对落盘;定时发送触发
pub(crate) fn tick_loop(shared: Arc<Shared>, msg_rx: mpsc::Receiver<WorkerMsg>) {
    let mut execution_state = ExecutionStateGuard::new();
    let mut last_stats = Instant::now();
    // 上次落盘的偏好快照(不同才写;首 tick 必写一次,与桌面版一致)
    let mut last_prefs: Option<StoredPrefs> = None;
    // 上次定时发送时刻(启用后等满一个周期再发;未连接时挂起,恢复连接后
    // 因 elapsed 已超时立即发——与桌面版 tick step 6 同语义)
    let mut last_timer = Instant::now();
    loop {
        // 先消化后睡眠(消化在前):装配阶段入队的消息(如后端启动状态)
        // 必须在 axum bind 前落地,保证首个 HTTP 可见时连接态已就绪
        // (设计文档确定性论证;旧版 sleep-first 首轮要等 10ms,存在
        // bind 后短窗内 connected=false 的竞态)。稳态节奏不变(每轮仍间隔
        // FLUSH_MS),仅首个 tick 提前
        let rx_ending = *shared.rx_ending.lock().unwrap();
        let auto_frame = shared.auto_frame.load(Ordering::Relaxed);
        let mut pump = shared.pump.lock().unwrap();
        loop {
            match msg_rx.try_recv() {
                Ok(WorkerMsg::Block { text, raw_len }) => {
                    if !pump.paused {
                        // raw_len 在字符集/HEX 转换前由 worker 记录；展示文本长度不参与统计。
                        shared.rx_bytes.fetch_add(raw_len as u64, Ordering::Relaxed);
                        pump.absorb_text(&text, rx_ending);
                    }
                }
                Ok(WorkerMsg::Tx(n)) => {
                    shared.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                }
                Ok(WorkerMsg::PowerResult { on, ok, message }) => {
                    if !pump.paused && !message.is_empty() {
                        pump.absorb_text(&message, rx_ending);
                    }
                    if !ok {
                        eprintln!("[mini-rtt-viewer] 电源输出操作失败:{message}");
                    }
                    broadcast_event(&shared, WsEvent::Power { on, ok, message });
                }
                Ok(WorkerMsg::Log(text)) => {
                    if !pump.paused {
                        pump.absorb_text(&text, rx_ending);
                    }
                }
                Ok(WorkerMsg::FrameEnd) => {
                    if !pump.paused && auto_frame {
                        pump.absorb_frame_end(rx_ending);
                    }
                }
                Ok(WorkerMsg::Progress(text)) => {
                    // 进度文案同步进 status_text(F3):2s 轮询的 sessionStatus
                    // 与 WS progress 同源,轮询回填不会倒退/打架
                    *shared.status_text.lock().unwrap() = clean_status_text(&text);
                    broadcast_event(&shared, WsEvent::Progress { text });
                }
                Ok(WorkerMsg::State(connected, status)) => {
                    // 翻变才回调(gui 壳驱动托盘图标;数据泵线程非阻塞,只 send_event)
                    let prev = shared.connected.swap(connected, Ordering::Relaxed);
                    if prev != connected {
                        // 自动连接/断开标记行(与手动标记同款式;直接用已持有的
                        // pump,严禁再借——见 AGENTS「tick 持 RefCell borrow」条)
                        pump.push_colored_line(&state_marker_line(connected), MARK_COLOR);
                        // 会话计时:连接记起点,断开清零
                        *shared.session_start.lock().unwrap() = if connected {
                            Some(Instant::now())
                        } else {
                            None
                        };
                        if let Some(cb) = &shared.on_state {
                            cb(ServiceEvent::ConnectedChanged(connected));
                        }
                    }
                    if !connected {
                        // 断开统一口径(F2):会话统计全清 + 设备信息清空,
                        // 消除「sessionSec 归零而 rxBytes 残留」的口径矛盾
                        // 自动重连、手动断开与 worker 退出都走同一分支
                        shared.rx_bytes.store(0, Ordering::Relaxed);
                        shared.tx_bytes.store(0, Ordering::Relaxed);
                        *shared.session_start.lock().unwrap() = None;
                        *shared.device_info.lock().unwrap() = None;
                    }
                    // 状态文案每条 State 都刷新(连接失败的错误详情也随 false 态带出)
                    *shared.status_text.lock().unwrap() = clean_status_text(&status);
                    // WS 广播用清洗后的文案(审计 P1):status_text 的语义是
                    // 「去横幅圆点前缀」,WS state 事件与 /api/status 的
                    // sessionStatus 必须同口径,避免前端按已清洗消费却拿到原文
                    broadcast_event(
                        &shared,
                        WsEvent::State {
                            connected,
                            status: clean_status_text(&status),
                        },
                    );
                }
                Ok(WorkerMsg::DeviceInfo(info)) => {
                    let json = device_info_json(&info);
                    *shared.device_info.lock().unwrap() = Some(info);
                    broadcast_event(&shared, WsEvent::Device { info: json });
                }
                Ok(WorkerMsg::DeviceNames(names)) => {
                    *shared.device_names.lock().unwrap() = names.clone();
                    broadcast_event(&shared, WsEvent::Names { names });
                }
                Ok(WorkerMsg::JLinks(list)) => {
                    *shared.jlinks.lock().unwrap() = list.clone();
                    let entries = list
                        .into_iter()
                        .map(|(sn, name)| JLinkEntry { sn, name })
                        .collect();
                    broadcast_event(&shared, WsEvent::JLinks { list: entries });
                }
                Ok(WorkerMsg::Exited) => {
                    // 只清"确实已停"的登记句柄:reset reconnect 场景下旧 worker
                    // 的 Exited 可能晚于新 worker 登记到达,不能误清新句柄
                    // (alive=false 由 worker 线程在发出 Exited 前置位)
                    let mine = {
                        let mut s = shared.session.lock().unwrap();
                        let mine = match s.as_ref() {
                            // 句柄已被 reset reconnect 提前取走:按幂等清理处理
                            None => true,
                            Some(h) => !h.handle.alive.load(Ordering::Relaxed),
                        };
                        if mine {
                            *s = None;
                        }
                        mine
                    };
                    if mine {
                        let prev = shared.connected.swap(false, Ordering::Relaxed);
                        if prev {
                            if let Some(cb) = &shared.on_state {
                                cb(ServiceEvent::ConnectedChanged(false));
                            }
                        }
                        // worker 线程已退:会话计时清零(State(false) 通常已先行
                        // 处理,这里兜底)
                        *shared.session_start.lock().unwrap() = None;
                    }
                }
                Err(_) => break,
            }
        }
        pump.enforce_line_cap();
        let dropped = pump.take_dropped();
        if let Some(rows) = pump.take_new_rows() {
            let seq = shared.seq.fetch_add(1, Ordering::Relaxed) + 1;
            broadcast_event(
                &shared,
                WsEvent::Rows {
                    seq,
                    dropped,
                    rows: &rows_view(&rows),
                },
            );
        }
        drop(pump);
        if last_stats.elapsed() >= Duration::from_millis(500) {
            last_stats = Instant::now();
            let session_sec = shared
                .session_start
                .lock()
                .unwrap()
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            broadcast_event(
                &shared,
                WsEvent::Stats {
                    rx: shared.rx_bytes.load(Ordering::Relaxed),
                    tx: shared.tx_bytes.load(Ordering::Relaxed),
                    rows: shared.pump.lock().unwrap().rows_len(),
                    session_sec,
                    cap: MAX_LOG_ROWS,
                },
            );
            // 偏好自动保存:与上次落盘快照不同才原子写(单文件几 KB,未连接态也保存)
            let snap = snapshot_prefs(&shared);
            if last_prefs.as_ref() != Some(&snap) {
                config::save(&snap);
                last_prefs = Some(snap);
            }
        }
        // 定时发送:开关 + 连接态 + 间隔合法 → 周期触发(复用 /api/send 发送管线,
        // 含回显/计数)
        if shared.timer_on.load(Ordering::Relaxed) && shared.connected.load(Ordering::Relaxed) {
            let secs = *shared.timer_interval.lock().unwrap();
            if TIMER_INTERVAL_RANGE.contains(&secs)
                && last_timer.elapsed() >= Duration::from_secs_f64(secs)
            {
                last_timer = Instant::now();
                let text = shared.timer_text.lock().unwrap().clone();
                if let Err(e) = shared.send_payload(&text, shared.timer_hex.load(Ordering::Relaxed))
                {
                    // 定时发送失败只写日志,不打断数据泵
                    eprintln!("[mini-rtt-viewer] 定时发送失败:{e}");
                }
            }
        }
        execution_state.sync(&shared);
        // 节流睡眠(本轮收尾):消化在前、睡眠在后,见循环头注释
        std::thread::sleep(Duration::from_millis(FLUSH_MS));
    }
}
