//! 浏览器黑盒测试专用进程：确定性的虚拟数据源，不进入正式程序或后端注册表。

use mini_rtt_viewer::backend::{
    text_to_hex_upper, BackendCaps, ConnectParams, DeviceInfo, Host, RttBackend, SessionHandle,
    WorkerCmd, WorkerHandle, WorkerMsg, APP_SHUTDOWN,
};
use mini_rtt_viewer::web::{self, WebOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_millis(80);
const RESET_GAP: Duration = Duration::from_millis(800);

enum QaCmd {
    Connect,
    Disconnect,
}

struct QaBackend {
    command: Mutex<Option<mpsc::Sender<QaCmd>>>,
}

static QA_BACKEND: QaBackend = QaBackend {
    command: Mutex::new(None),
};

impl QaBackend {
    fn ensure_singleton(&self, host: &dyn Host) {
        let mut slot = self.command.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (qa_tx, qa_rx) = mpsc::channel();
        let handle = Arc::new(WorkerHandle {
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        host.set_session(Some(SessionHandle { handle, cmd_tx }));
        *slot = Some(qa_tx);
        drop(slot);
        spawn_stream(host, cmd_rx, qa_rx);
    }

    fn send_command(&self, command: QaCmd) {
        if let Some(tx) = self.command.lock().unwrap().as_ref() {
            let _ = tx.send(command);
        }
    }
}

impl RttBackend for QaBackend {
    fn id(&self) -> &'static str {
        "qa"
    }

    fn display_name(&self) -> &'static str {
        "QA fixture"
    }

    fn description(&self) -> &'static str {
        "Deterministic browser-test data source"
    }

    fn is_virtual(&self) -> bool {
        true
    }

    fn caps(&self) -> BackendCaps {
        BackendCaps {
            hardware_probes: false,
            reset_reconnect: false,
        }
    }

    fn initial_devices(&self) -> Vec<String> {
        vec![
            "STM32F103C8".into(),
            "STM32F030C8".into(),
            "STM32G474VET6".into(),
            "STM32H743ZIT6".into(),
            "GD32F303CCT6".into(),
            "CH32V307VCT6".into(),
        ]
    }

    fn on_selected(&self, host: &dyn Host) {
        host.set_device_names(self.initial_devices());
        self.ensure_singleton(host);
        self.send_command(QaCmd::Connect);
    }

    fn on_deselected(&self, _host: &dyn Host) {
        self.send_command(QaCmd::Disconnect);
    }

    fn can_connect(&self, _host: &dyn Host) -> Result<(), String> {
        Ok(())
    }

    fn connect(&self, host: &dyn Host, _params: &ConnectParams) -> Result<(), String> {
        self.ensure_singleton(host);
        self.send_command(QaCmd::Connect);
        Ok(())
    }

    fn disconnect(&self, _host: &dyn Host) {
        self.send_command(QaCmd::Disconnect);
    }

    fn reset_in_place(&self, host: &dyn Host) -> Result<(), String> {
        if !host.connected() {
            return Err("未连接".into());
        }
        if let Some(session) = host.session() {
            let _ = session.cmd_tx.send(WorkerCmd::Reset);
        }
        Ok(())
    }

    fn reset_reconnect(&self, _host: Arc<dyn Host>, _params: &ConnectParams) -> Result<(), String> {
        Err("QA fixture 不支持重连式复位".into())
    }

    fn power(&self, host: &dyn Host, on: bool) -> Result<(), String> {
        if !host.connected() {
            return Err("未连接".into());
        }
        if let Some(session) = host.session() {
            let _ = session.cmd_tx.send(WorkerCmd::Power(on));
        }
        Ok(())
    }
}

fn emit_connected(msg_tx: &mpsc::Sender<WorkerMsg>) {
    let _ = msg_tx.send(WorkerMsg::State(true, "● 已连接 (qa)".into()));
    let _ = msg_tx.send(WorkerMsg::DeviceInfo(DeviceInfo {
        firmware: "QA fixture".into(),
        hardware: "virtual".into(),
        serial: "0".into(),
        core_name: "Cortex-M4".into(),
        core_id: "0".into(),
        core_cpu: "ARM 32-Bit".into(),
        target: "STM32F103C8".into(),
        iface: "SWD".into(),
        speed_khz: 4000,
    }));
}

fn spawn_stream(host: &dyn Host, cmd_rx: mpsc::Receiver<WorkerCmd>, qa_rx: mpsc::Receiver<QaCmd>) {
    let msg_tx = host.msg_tx();
    let shared = host.worker_shared();
    std::thread::spawn(move || {
        let mut connected = false;
        let mut reset_at = None;
        let mut sequence = 0_u64;
        let mut last_rx = Instant::now();
        let mut frame_open = false;
        loop {
            if APP_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            while let Ok(command) = cmd_rx.try_recv() {
                match command {
                    WorkerCmd::Send(data) => {
                        let _ = msg_tx.send(WorkerMsg::Tx(data.len()));
                    }
                    WorkerCmd::Power(on) => {
                        let _ = msg_tx.send(WorkerMsg::PowerResult {
                            on,
                            ok: true,
                            message: format!(
                                "── J-Link 19 脚供电:{} (qa) ──\r\n",
                                if on { "开" } else { "关" }
                            ),
                        });
                    }
                    WorkerCmd::Reset if connected => {
                        let _ = msg_tx.send(WorkerMsg::Log("── 目标已重置 (qa) ──\r\n".into()));
                        let _ = msg_tx.send(WorkerMsg::State(false, "● 目标重置中 (qa)".into()));
                        connected = false;
                        reset_at = Some(Instant::now() + RESET_GAP);
                        frame_open = false;
                    }
                    WorkerCmd::Reset => {}
                }
            }
            while let Ok(command) = qa_rx.try_recv() {
                let next = matches!(command, QaCmd::Connect);
                reset_at = None;
                if connected != next {
                    connected = next;
                    if connected {
                        emit_connected(&msg_tx);
                    } else {
                        let _ = msg_tx.send(WorkerMsg::State(false, "● 未连接 (qa)".into()));
                    }
                    last_rx = Instant::now();
                    frame_open = false;
                }
            }
            if reset_at.is_some_and(|deadline| Instant::now() >= deadline) {
                reset_at = None;
                connected = true;
                emit_connected(&msg_tx);
                last_rx = Instant::now();
            }
            let now = Instant::now();
            let frame_timeout =
                Duration::from_millis(shared.frame_timeout_ms.load(Ordering::Relaxed) as u64);
            if connected {
                if frame_open && now.duration_since(last_rx) > frame_timeout {
                    let _ = msg_tx.send(WorkerMsg::FrameEnd);
                }
                let value = if sequence % 10 == 9 {
                    format!("\x1b[31m{sequence}\x1b[0m")
                } else {
                    format!("\x1b[32m{sequence}\x1b[0m")
                };
                let level = if sequence % 10 == 9 {
                    " \x1b[31mERR\x1b[0m"
                } else {
                    ""
                };
                let block = format!(
                    "\x1b[36m[qa {sequence:04}]\x1b[0m Heartbeat: {value} 😊🍟❤ 心跳 中文 English mixed padding text{level}\r\n"
                );
                let raw_len = block.len();
                let text = if shared.hex_rx.load(Ordering::Relaxed) {
                    text_to_hex_upper(&block)
                } else {
                    block
                };
                let _ = msg_tx.send(WorkerMsg::Block { text, raw_len });
                sequence += 1;
                last_rx = now;
                frame_open = true;
            } else if frame_open && now.duration_since(last_rx) > frame_timeout {
                let _ = msg_tx.send(WorkerMsg::FrameEnd);
                frame_open = false;
            }
            std::thread::sleep(INTERVAL);
        }
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut port = 18099;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                port = args
                    .get(index + 1)
                    .and_then(|value| value.parse().ok())
                    .expect("--port requires a valid port");
                index += 1;
            }
            "--no-window" => {}
            other => panic!("unsupported QA argument: {other}"),
        }
        index += 1;
    }
    let options = WebOptions {
        backend: Some("qa".into()),
        port,
        listen: std::net::IpAddr::from([127, 0, 0, 1]),
        open_browser: false,
    };
    if let Err(error) = web::run_with_backend(options, &QA_BACKEND) {
        eprintln!("QA server failed: {error}");
        std::process::exit(1);
    }
}
