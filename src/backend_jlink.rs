//! J-Link 真机后端(`RttBackend` 实现,后端抽象阶段一):本文件只做"缝"的
//! 胶水——协议类型在 backend.rs,worker 内部实现(DLL 连接序列/读循环/
//! CharsetDecoder/SPEEDS_KHZ)留在 rtt.rs。
//!
//! - connect = 原 web.rs `spawn_worker`(rtt::spawn + WorkerConfig 组装 +
//!   sn 回落列表首台)+ 登记会话;
//! - can_connect = 存活 worker 检查("已有连接")+ device_db BUSY 门闩
//!   (原 Python 项目实测:枚举与 connect 并发加载 DLL 有损坏 DLL TLS 风险,
//!   **别删**);
//! - disconnect = stop + 空 Send 唤醒(句柄保留到 Exited 到达,维持
//!   「worker 存活期间拒绝新连接」的门闩);
//! - reset_reconnect = 原 web.rs:1471-1509 整段迁移:take 会话 + stop +
//!   唤醒 → 'static 线程(持 Arc<dyn Host>)→ 1s 后等旧线程退出 → 检查
//!   手动连接/APP_SHUTDOWN → respawn → 清 reconnect 门闩。门闩置位时序:
//!   web 层在调用本方法**之前**置位(比现状 take 后置位更保守,两行间无
//!   await,可观测等价);
//! - start_background_enumeration = device_db 后台枚举 + DbResult→WorkerMsg
//!   桥接线程(原 web.rs:676-691 迁移,线程用 host.msg_tx())。**幂等**:
//!   serve 装配与 on_selected(切后端回来)都可能调用,AtomicBool 守卫保证
//!   进程内只启动一次——并发二次枚举会撞 device_db BUSY 门闩/重复拉桥接线程;
//! - on_selected = 清空 device_names 并启动设备库枚举。
//!
//! worker 生命周期铁律(AGENTS.md)全部保留:Exited 门闩、connect 阶段
//! stop 检查(worker 内)、reconnect 门闩、BUSY 门闩。

use crate::backend::{
    BackendCaps, ConnectParams, Host, RttBackend, SessionHandle, WorkerCmd, WorkerMsg, APP_SHUTDOWN,
};
use crate::device_db;
use crate::rtt::{self, WorkerConfig, SPEEDS_KHZ};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// J-Link 后端(enumeration_started = 进程内枚举已启动守卫,const 构造
/// &'static 注册表项)
pub struct JLinkBackend {
    enumeration_started: AtomicBool,
}

/// 注册表静态实例(backend::backend_by_id 按 id 取用)
pub static JLINK_BACKEND: JLinkBackend = JLinkBackend {
    enumeration_started: AtomicBool::new(false),
};

impl RttBackend for JLinkBackend {
    fn id(&self) -> &'static str {
        crate::backend::BACKEND_JLINK
    }

    fn display_name(&self) -> &'static str {
        "J-Link 真机"
    }

    fn description(&self) -> &'static str {
        "经 J-Link DLL 连接真实目标读取 RTT"
    }

    fn is_virtual(&self) -> bool {
        false
    }

    fn caps(&self) -> BackendCaps {
        BackendCaps {
            hardware_probes: true,
            reset_reconnect: true,
        }
    }

    /// 启动期设备库后台枚举(芯片型号 + 本机 J-Link),与桌面版同一模块;
    /// DbResult → WorkerMsg 桥接线程把结果送进统一消息泵。幂等:进程内
    /// 只启动一次(serve 装配 + on_selected 切回场景共用)
    fn start_background_enumeration(&self, host: &dyn Host) {
        if self.enumeration_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let (db_tx, db_rx) = mpsc::channel::<device_db::DbResult>();
        device_db::spawn_background(db_tx);
        let tx = host.msg_tx();
        std::thread::spawn(move || {
            while let Ok(r) = db_rx.recv() {
                let msg = match r {
                    device_db::DbResult::DeviceNames(names) => WorkerMsg::DeviceNames(names),
                    device_db::DbResult::Emulators(list) => WorkerMsg::JLinks(list),
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }

    /// 选中时清空候选并启动设备库枚举(若从未启动,幂等),随后异步回填。
    fn on_selected(&self, host: &dyn Host) {
        host.set_device_names(Vec::new());
        self.start_background_enumeration(host);
    }

    /// 连接前门禁(Err → 409,文案与现状逐字一致):
    /// ① 存活 worker 检查(Exited 门闩);② 设备库枚举进行中(仅首次启动
    /// 无缓存的几秒)拒绝,BUSY 门闩。
    fn can_connect(&self, host: &dyn Host) -> Result<(), String> {
        if host
            .session()
            .is_some_and(|s| s.handle.alive.load(Ordering::Relaxed))
        {
            return Err("已有连接".into());
        }
        if device_db::busy() {
            return Err("设备库枚举中,请稍候几秒再连接".into());
        }
        Ok(())
    }

    /// spawn 真 worker 并登记会话(api_connect 与 reset reconnect 共用)。
    /// params.jlink_sn:连接参数携带的下拉选定值(已在 api_connect 对列表
    /// 校验),无选定/已失效时回落列表首台(旧行为)
    fn connect(&self, host: &dyn Host, params: &ConnectParams) -> Result<(), String> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
        let ws = host.worker_shared();
        let handle = rtt::spawn(
            WorkerConfig {
                chip: params.chip.clone(),
                iface_index: params.iface_index,
                speed_khz: SPEEDS_KHZ[params.speed_index.min(SPEEDS_KHZ.len() - 1)],
                channel: params.channel.min(15),
                frame_timeout_ms: ws.frame_timeout_ms,
                selected_sn: params
                    .jlink_sn
                    .or_else(|| host.jlinks().first().map(|(sn, _)| *sn)),
                encoding_index: ws.encoding_index,
                hex_rx: ws.hex_rx,
            },
            host.msg_tx(),
            cmd_rx,
        );
        host.set_session(Some(SessionHandle { handle, cmd_tx }));
        Ok(())
    }

    /// 断开:置停止标志 + 空发送唤醒阻塞中的 worker 尽快退出;worker 的
    /// Exited 消息回到未连接态(与桌面版同协议)。
    fn disconnect(&self, host: &dyn Host) {
        if let Some(s) = host.session() {
            s.handle.stop.store(true, Ordering::Relaxed);
            let _ = s.cmd_tx.send(WorkerCmd::Send(Vec::new()));
        }
    }

    /// 复位目标并重挂 RTT 续收(现有 Reset 命令;未连接 409)
    fn reset_in_place(&self, host: &dyn Host) -> Result<(), String> {
        let Some(s) = host.session() else {
            return Err("未连接".into());
        };
        let _ = s.cmd_tx.send(WorkerCmd::Reset);
        Ok(())
    }

    /// 重连式复位:先走断开逻辑,1 秒后用上次连接参数重新 spawn worker。
    /// 前置(web 层已做):reconnecting 门闩置位、last_connect 非空检查。
    /// 本方法负责 take 会话 + 拉起重连线程,线程收尾清门闩(审计 P1)。
    fn reset_reconnect(&self, host: Arc<dyn Host>, params: &ConnectParams) -> Result<(), String> {
        // 断开 worker 供重连:置停止标志 + 空发送唤醒阻塞中的读循环,会话
        // 引用立即清空;旧句柄返回给重连线程,等线程真正退出后才重连。
        // 与 disconnect 有意不同:那边保留句柄到 Exited 到达,维持「worker
        // 存活期间拒绝新连接」的门闩;这边等待职责由重连线程接管。
        let old = {
            let old = host.session();
            if let Some(s) = &old {
                s.handle.stop.store(true, Ordering::Relaxed);
            }
            host.set_session(None);
            old
        };
        if let Some(s) = &old {
            let _ = s.cmd_tx.send(WorkerCmd::Send(Vec::new())); // 唤醒阻塞中的 worker 尽快退出
        }
        let params = params.clone();
        // 'static 闭包:捕获静态注册表项(而非短生命周期的 &self)
        let this: &'static JLinkBackend = &JLINK_BACKEND;
        // 断开 → 1 秒后用上次参数重连(独立线程,HTTP 立即返回 200)
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            // 铁律:旧 worker 线程存活期间严禁 spawn 新 worker(双 worker
            // 抢 J-Link 是数据损坏/状态错乱的根源)——先等旧线程真正退出
            if !wait_worker_exit(
                old.as_ref().map(|s| s.handle.as_ref()),
                Duration::from_secs(3),
            ) {
                let _ = host.msg_tx().send(WorkerMsg::Log(
                    "[自动重连] 上一连接未退出,已放弃本次重连\r\n".into(),
                ));
                host.set_reconnecting(false);
                return;
            }
            // 等待期内用户已手动连接则让位;进程退出中不再拉起 worker
            if host.session().is_some() || APP_SHUTDOWN.load(Ordering::Relaxed) {
                host.set_reconnecting(false);
                return;
            }
            let _ = this.connect(&*host, &params);
            host.set_reconnecting(false);
        });
        Ok(())
    }

    /// 电源输出开关(J-Link 19 脚):未登记会话 409「未连接」;会话存在但
    /// 尚未连上目标时命令照发(worker 侧连上后生效),与现状一致
    fn power(&self, host: &dyn Host, on: bool) -> Result<(), String> {
        let Some(s) = host.session() else {
            return Err("未连接".into());
        };
        let _ = s.cmd_tx.send(WorkerCmd::Power(on));
        Ok(())
    }
}

/// 等旧 worker 线程退出(alive=false 由线程在发出 Exited 前置位);超时返回
/// false——此时严禁 spawn 新 worker,见 rtt.rs「worker 生命周期铁律」
fn wait_worker_exit(h: Option<&crate::backend::WorkerHandle>, timeout: Duration) -> bool {
    let Some(h) = h else {
        return true;
    };
    let deadline = Instant::now() + timeout;
    while h.alive.load(Ordering::Relaxed) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}
