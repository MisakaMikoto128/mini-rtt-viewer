//! 后端抽象缝(设计契约:docs/product/backend-abstraction.md §3,阶段一):
//! web 层只依赖 `RttBackend` trait,不再感知具体后端;worker 协议类型
//! (`WorkerMsg`/`WorkerCmd`/`WorkerHandle`/`DeviceInfo`/`APP_SHUTDOWN`)是缝的
//! 公共契约,统一迁到这里(rtt.rs 保留 worker 内部实现)。
//!
//! - `SessionHandle`:web 层看到的"一次连接"的统一控制面(jlink = 真 worker
//!   的 handle+cmd_tx;demo = 单例线程的 cmd_tx + 恒活 handle);
//! - `Host`:web 层(Shared)暴露给**后端实现**的最窄状态面;后端禁止直接摸
//!   Shared 其它字段;必须 Send+Sync+'static(reset_reconnect 把 Arc<dyn Host>
//!   交给重连线程);
//! - 注册表:两个后端都是 `&'static`(jlink 无内部状态;demo 的内部可变性用
//!   `Mutex<Option<Sender<DemoCmd>>>` const 构造)。**禁止经 WebOptions/Host
//!   向后端传通道**,一切状态经 Host;
//! - Err 统一映射:can_connect/connect/reset_in_place/power 的 Err →
//!   HTTP 409 + Err 文案(与现状逐字一致,web 层不做第二套文案)。
//!
//! 阶段一只有按 id 取注册项;阶段二(后端可选面)再加 available 列表与
//! on_selected/on_deselected 切换流程。

use crate::config::StoredPrefs;
use crate::rtt::SPEEDS_KHZ;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{mpsc, Arc};

/// 独立提示行(横幅等,自带换行)
pub enum WorkerMsg {
    /// 独立提示行(横幅等,自带换行)
    Log(String),
    /// 一次 RTT read 的解码输出(帧边界由 worker 按间隔判定后以 FrameEnd 发出)
    Block(String),
    /// 帧结束标记:相邻数据到达间隔超过断帧超时,worker 判定一帧结束。
    /// 断帧判定在 worker(5ms 轮询的真实时间戳)进行,UI 刷新粒度不影响精度。
    FrameEnd,
    /// 连接/断开的**最终结果**:true=已连接,false=已断开或失败。
    /// 中间进度用 Progress(只刷状态栏文字,绝不改变连接标志,
    /// 否则按钮会在 连接/断开 之间来回跳)。
    State(bool, String),
    /// 连接过程的中间进度(只更新状态栏文字)
    Progress(String),
    /// 连接成功后采集的设备信息(DLL 查询,字段对齐原项目;demo 为演示数据)
    DeviceInfo(DeviceInfo),
    /// J-Link 设备库候选名(启动时后台枚举/磁盘缓存,用于目标设备下拉)
    DeviceNames(Vec<String>),
    /// 本机已接入的 J-Link 调试器列表(序列号, 显示名),后台线程 + 每次连接时刷新
    JLinks(Vec<(u32, String)>),
    /// worker 线程已完全退出(含 DLL close),UI 收到后才允许再次连接,
    /// 防止上一个 worker 还阻塞在 connect() 时 spawn 新 worker 并发抢 RTT。
    /// (demo 后端的进程级单例线程不发此消息,只因 APP_SHUTDOWN 退出)
    Exited,
}

/// 连接成功后从 J-Link / 目标采集到的信息(UI 设备信息区展示)
pub struct DeviceInfo {
    pub firmware: String,
    pub hardware: String,
    pub serial: String,
    pub core_name: String,
    pub core_id: String,
    pub core_cpu: String,
    pub target: String,
    pub iface: String,
    pub speed_khz: u32,
}

/// UI → worker 命令(数据面)。原来只有发数据一种,引入电源输出后升级为
/// 枚举协议,避免在字符串上做前缀解析(易错且没有类型保障)。
/// demo 的连接/断开不走此协议(用 demo 后端私有的 DemoCmd 通道,真机 worker
/// 永远看不到)。
pub enum WorkerCmd {
    /// 向 RTT 通道写原始字节(行尾已含;文本模式为 UTF-8 编码,HEX 模式为解析后的字节)
    Send(Vec<u8>),
    /// 电源输出开关(J-Link 19 脚):true=开 false=关
    Power(bool),
    /// 复位目标并恢复运行(复位后重挂 RTT:固件重新初始化后控制块可能移动)
    Reset,
}

/// 应用退出信号:主窗口关闭后置位,worker 循环(包括阻塞中的轮询间隔)
/// 检测到后尽快退出,否则非 daemon 线程会让进程在窗口关闭后残留。
pub static APP_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// worker 生命周期句柄:stop 请求退出,alive 反映线程是否还在。
pub struct WorkerHandle {
    pub stop: AtomicBool,
    pub alive: AtomicBool,
}

/// 会话句柄:web 层看到的"一次连接"的统一控制面。
/// jlink = 真 worker 的 handle+cmd_tx;demo = 单例线程的 cmd_tx + 恒活 handle
/// (stop 标志永远无人置位,alive 恒 true)。
#[derive(Clone)]
pub struct SessionHandle {
    pub handle: Arc<WorkerHandle>,
    /// 数据面命令:Send/Power/Reset
    pub cmd_tx: mpsc::Sender<WorkerCmd>,
}

/// 后端能力位:web 层据此做通用门禁,不为具体后端写 if
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendCaps {
    /// 是否有真实调试器列表(J-Link 下拉)
    pub hardware_probes: bool,
    /// 是否支持 reset reconnect 模式(demo 不支持,web 层回落 in-place)
    pub reset_reconnect: bool,
}

/// 连接参数(后端无关;原 web.rs 的 SavedConnect 迁移为此类型,web 层与
/// 后端实现共用同一类型)。speed_index 为 SPEEDS_KHZ 下标,kHz 换算归
/// jlink 后端内部。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectParams {
    pub chip: String,
    pub iface_index: usize,
    pub speed_index: usize,
    pub channel: u32,
    pub jlink_sn: Option<u32>,
}

impl ConnectParams {
    /// 偏好底版 → 上次连接参数(启动恢复;chip 为空 = 从未连接过 → None,
    /// reset reconnect 对此返回 409)
    pub fn from_prefs(p: &StoredPrefs) -> Option<Self> {
        let chip = p.chip_name.trim().to_string();
        if chip.is_empty() {
            return None;
        }
        Some(Self {
            iface_index: p.iface_index.clamp(0, 1) as usize,
            speed_index: p.speed_index.clamp(0, SPEEDS_KHZ.len() as i32 - 1) as usize,
            channel: p.channel.clamp(0, 15) as u32,
            jlink_sn: p.jlink_serial,
            chip,
        })
    }
}

/// worker 侧三个共享热切换原子(打包传递,后端在 worker 内消费)
#[derive(Clone)]
pub struct WorkerShared {
    pub frame_timeout_ms: Arc<AtomicU32>,
    pub encoding_index: Arc<AtomicU32>,
    pub hex_rx: Arc<AtomicBool>,
}

/// web 层暴露给后端的最窄状态面(Shared 实现;后端禁止直接摸 Shared 其它
/// 字段)。web.rs 自己的代码(api_* / tick_loop)直接操作 Shared,Host 只是
/// **后端实现**访问状态的边界。Send+Sync+'static:reset_reconnect 会把
/// Arc<dyn Host> 交给重连线程。
pub trait Host: Send + Sync + 'static {
    /// msg_tx.send 兜底
    fn send_msg(&self, msg: WorkerMsg);
    /// owned 发送端:交给 'static 线程(jlink connect 的 worker 线程、
    /// device_db 桥接线程、demo 单例、reset 重连线程都靠它)
    fn msg_tx(&self) -> mpsc::Sender<WorkerMsg>;
    /// 读(clone;handle/cmd_tx 均可克隆)
    fn session(&self) -> Option<SessionHandle>;
    /// 写(connect/take 用)
    fn set_session(&self, s: Option<SessionHandle>);
    fn connected(&self) -> bool;
    fn jlinks(&self) -> Vec<(u32, String)>;
    fn set_device_names(&self, names: Vec<String>);
    fn worker_shared(&self) -> WorkerShared;
    /// reset reconnect 等待窗口门闩:web 层置位,重连线程收尾清除
    fn reconnecting(&self) -> bool;
    fn set_reconnecting(&self, v: bool);
}

/// 后端 trait:一个数据采集后端 = 一个实现
pub trait RttBackend: Send + Sync {
    fn id(&self) -> &'static str; // "jlink" | "demo"
    fn display_name(&self) -> &'static str; // UI 下拉文案
    fn description(&self) -> &'static str;
    /// 纯软件后端(无硬件依赖)
    fn is_virtual(&self) -> bool;
    fn caps(&self) -> BackendCaps;
    /// 选定/启动时的初始设备候选(同步;demo=静态表,jlink=空走异步枚举)
    fn initial_devices(&self) -> Vec<String> {
        Vec::new()
    }
    /// 启动期后台枚举(结果以 WorkerMsg::DeviceNames/JLinks 异步回传)
    fn start_background_enumeration(&self, _host: &dyn Host) {}
    /// 被选中为当前后端时回调(切后端时种子数据/懒启动);旧后端先收 on_deselected
    fn on_selected(&self, _host: &dyn Host) {}
    fn on_deselected(&self, _host: &dyn Host) {}
    /// Err → HTTP 409 + Err 文案(与现状逐字一致)
    fn can_connect(&self, host: &dyn Host) -> Result<(), String>;
    fn connect(&self, host: &dyn Host, params: &ConnectParams) -> Result<(), String>;
    fn disconnect(&self, host: &dyn Host);
    fn reset_in_place(&self, host: &dyn Host) -> Result<(), String>;
    /// 不支持时返回 Err(web 层对 demo 回落 in-place,与现状一致)。
    /// 拿 Arc<dyn Host>:重连线程是 'static,要克隆进去做 wait/respawn/清门闩
    fn reset_reconnect(&self, host: Arc<dyn Host>, params: &ConnectParams) -> Result<(), String>;
    fn power(&self, host: &dyn Host, on: bool) -> Result<(), String>;
    /// --demo-log 强制形态的启动自动连接(仅 demo 实现;普通形态不调用)
    fn auto_connect_at_startup(&self, _host: &dyn Host) {}
}

/// 后端 id(阶段二 /api/backends、偏好持久化共用)
pub const BACKEND_JLINK: &str = "jlink";
pub const BACKEND_DEMO: &str = "demo";

/// 注册表(阶段一只有按 id 取;阶段二加 available 列表)。
/// 两个后端都是 &'static 静态实例。
pub fn backend_by_id(id: &str) -> Option<&'static dyn RttBackend> {
    match id {
        BACKEND_JLINK => Some(&crate::backend_jlink::JLINK_BACKEND),
        BACKEND_DEMO => Some(&crate::demo::DEMO_BACKEND),
        _ => None,
    }
}

/// 文本 → 十六进制大写文本(UTF-8 字节,每字节两位、空格分隔,与真机 worker
/// 的 HEX 接收输出同格式,如 "[ de" → "5B 20 64 65 ")。真机 worker 的 HEX
/// 路径与 demo 下沉的 HEX 转换共用本函数(单点,防两处实现漂移)。
pub fn text_to_hex_upper(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let _ = write!(out, "{b:02X} ");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_registry_resolves_both_ids() {
        let jlink = backend_by_id(BACKEND_JLINK).expect("jlink 后端应已注册");
        let demo = backend_by_id(BACKEND_DEMO).expect("demo 后端应已注册");
        assert_eq!(jlink.id(), "jlink");
        assert_eq!(demo.id(), "demo");
        // 虚拟位:jlink 真机 / demo 纯软件
        assert!(!jlink.is_virtual());
        assert!(demo.is_virtual());
        // 能力位:reset reconnect 仅 jlink 支持(web 层据此回落 in-place)
        assert!(jlink.caps().reset_reconnect);
        assert!(!demo.caps().reset_reconnect);
        assert!(jlink.caps().hardware_probes);
        assert!(!demo.caps().hardware_probes);
        // demo 的初始设备候选 = 演示设备库(api/devices 种子来源)
        assert!(demo.initial_devices().contains(&"STM32F103C8".to_string()));
        assert_eq!(demo.initial_devices().len(), 6);
        // jlink 初始候选为空(走 device_db 异步枚举)
        assert!(jlink.initial_devices().is_empty());
    }

    #[test]
    fn backend_registry_rejects_unknown_id() {
        assert!(backend_by_id("").is_none());
        assert!(backend_by_id("probe-rs").is_none());
        // id 匹配大小写敏感,不静默归一
        assert!(backend_by_id("JLINK").is_none());
        assert!(backend_by_id("Demo").is_none());
    }

    #[test]
    fn text_to_hex_upper_matches_worker_hex_format() {
        assert_eq!(text_to_hex_upper(""), "");
        assert_eq!(text_to_hex_upper("AB"), "41 42 ");
        // 契约示例:"5B 20 64 65…" = "[ de" 的 UTF-8 字节(4 字节)
        assert_eq!(text_to_hex_upper("[ de"), "5B 20 64 65 ");
        // 多字节 UTF-8 按字节展开:"你" = E4 BD A0
        assert_eq!(text_to_hex_upper("你"), "E4 BD A0 ");
    }

    #[test]
    fn connect_params_from_prefs_clamps_and_requires_chip() {
        let mut p = StoredPrefs::default();
        assert_eq!(ConnectParams::from_prefs(&p), None); // 从未连接过
        p.chip_name = "  STM32F103C8 ".into();
        p.iface_index = 5; // 越界 → 夹到 1
        p.speed_index = 99; // 越界 → 夹到 7(SPEEDS_KHZ 上限)
        p.channel = 99; // 越界 → 夹到 15
        assert_eq!(
            ConnectParams::from_prefs(&p),
            Some(ConnectParams {
                chip: "STM32F103C8".into(),
                iface_index: 1,
                speed_index: 7,
                channel: 15,
                jlink_sn: None,
            })
        );
        // J-Link 序列号随偏好恢复(reset reconnect 重连同一台)
        p.jlink_serial = Some(602717758);
        assert_eq!(
            ConnectParams::from_prefs(&p).unwrap().jlink_sn,
            Some(602717758)
        );
    }
}
