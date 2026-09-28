# 设计文档:可替换后端抽象(RttBackend seam)

状态:**已落地(2026-09-28,WP-A/B/C/D 全部完成)** — 本文件是后端抽象的
设计契约与落地记录;后续改后端相关行为,先改本文再改代码。
日期:2026-09-28 · 关联 ADR:docs/team/decisions.md(后端抽象条目,实现落地后追加)
评审:2026-09-28 经无上下文读者代理盲测(对照代码逐条核验),修正 2 处阻断
(reset_reconnect 的 Arc<dyn Host> 与门闩收尾归属、Host 补 owned msg_tx)与
8 处应修项(demo 409 归属、can_connect 幂等、DeviceInfo 发送点、hex 下沉的
rxBytes 口径对齐、归一化位置、gui.rs use 等),已全部回写正文。
WP-A 落地修正(2026-09-28):auto_connect 不发 Connect(保自动周期)、
api_connect 门禁顺序等价调整、tick 消化前置——正文已同步;本文引用的
web.rs 行号均指**重构前基线**(2026-09-28 main),WP-A 后行号已漂移,
仅作历史定位用。
WP-B 落地修正(2026-09-28):settings 400 判据 = 不在可用列表(含强制形态
排除项);同 id 设置幂等 no-op——正文已同步。jlink on_selected 幂等守卫
(枚举进程内只启动一次,AtomicBool)。

## 1. 背景与目标

当前数据采集后端(J-Link DLL)与 web 层耦合:web.rs 里存在 6 处 `demo_mode`
运行时布尔分叉 + 2 处展示分叉,demo 专用命令(`DemoConnect/DemoDisconnect`)
混在真机协议枚举 `WorkerCmd` 里。未来要接入 probe-rs(已 有实验分支
`feat/probe-rs-backend`,保留勿动)乃至其它数据源,必须先把"后端"变成可替换的缝。

**目标**:
1. 引入后端 trait,web 层只依赖 trait,不再感知具体后端;
2. 后端可在界面上选择(本期:jlink / demo 两个),选择持久化到偏好;
3. 全程零人工验证:所有行为变化由既有 19 条 pytest + 新增 pytest + cargo 单测锁定;
4. 为 probe-rs 接入铺路(接入 = 新增一个 impl + 注册,web 层零改动)。

**非目标(本期明确不做)**:不实现 TCP/UDP/串口后端;不实现 probe-rs 本体;
不改 WS 线格式;不动 `feat/probe-rs-backend` worktree;不动 `docs/audit/`
(审计引擎专属)。

## 2. 现状事实(勘察结论,行号以 2026-09-28 main 为准)

### 2.1 两套后端的本质差异

| 维度 | jlink(真机) | demo(虚拟) |
|---|---|---|
| 线程生命周期 | 连接级:每次 connect spawn `rtt::spawn` → `Exited` 结束(web.rs:1257) | 进程级单例:`serve()` 启动即 spawn(web.rs:653),永不退出(除 APP_SHUTDOWN) |
| 连接语义 | spawn worker 本身 | 向单例线程发 `DemoConnect`(状态翻转) |
| 断开语义 | `stop=true` + 空 Send 唤醒(web.rs:1377) | 发 `DemoDisconnect`,线程存活、不再自动重连 |
| 设备候选 | device_db 后台枚举 → `WorkerMsg::DeviceNames`(web.rs:676-691) | web.rs 硬编码 `demo_device_names()`(web.rs:1235) |
| 设备信息 | worker 采集 → `WorkerMsg::DeviceInfo` | web.rs 硬编码 `demo_device_json()`,api_status 分叉(web.rs:1132) |
| HEX 接收 | worker 内转换(hex_rx 热切换) | worker 发原文,web 层 tick 补转换(web.rs:912) |
| reset reconnect | 支持(take_worker_down + wait + respawn,web.rs:1471) | 不支持(demo 分支忽略 mode,一律 in-place,web.rs:1454) |

### 2.2 分叉点清单(全部要收敛)

运行时 6 处:serve 装配(web.rs:653-692)、api_connect(web.rs:1306 设备校验、
1355 命令分派)、api_disconnect(1371)、api_power(1388)、api_reset(1454)、
tick_loop Block 的 demo-only HEX 转换(912)。
展示 2 处:api_status device 字段(1132)、api_devices(1228)。
另有 1 处**保留不收敛**:启动 println 的 `(demo)` 后缀(web.rs:751 附近),
属 `--demo-log` 强制形态展示,用 WebOptions 判断,不随后端切换。

### 2.3 必须保持的既有语义(改动时逐条对照)

- worker 生命周期铁律:Exited 门闩、connect 阶段退出检测、reconnect 门闩
  (`Shared.reconnecting`)、device_db BUSY 门闩(api_connect 409)——见 AGENTS.md;
- tick_loop 持泵锁纪律:持锁段内只允许短借其它 Mutex 字段、on_state 非阻塞;
- demo 状态机(demo.rs 模块头):启动即连接 → 20s 断开 → 3s 重连循环;
  任何手动干预 → 自动周期永久挂起;手动断开后不自动重连;
- `/api/status` 8+1 字段、WS 线格式(`ws_event_wire_format_matches_legacy_contract`
  逐字节锁)、device 8 字段键名(`device_info_json_keys_match_status_contract`);
- demo 模式 `WorkerCmd::Send` 由 web 层计数+回显,demo 线程忽略(不重复计数)。

## 3. 目标架构

### 3.1 新模块 `src/backend.rs`(唯一的缝)

```rust
/// 会话句柄:web 层看到的"一次连接"的统一控制面。
/// jlink = 真 worker 的 handle+cmd_tx;demo = 单例线程的 cmd_tx + 恒活 handle。
pub struct SessionHandle {
    pub handle: Arc<WorkerHandle>,          // alive/stop(demo:alive 恒 true)
    pub cmd_tx: mpsc::Sender<WorkerCmd>,    // 数据面命令:Send/Power/Reset
}

/// 后端能力位:web 层据此做通用门禁,不为具体后端写 if
#[derive(Clone, Copy)]
pub struct BackendCaps {
    pub hardware_probes: bool,      // 是否有真实调试器列表(J-Link 下拉)
    pub reset_reconnect: bool,      // 是否支持 reset reconnect 模式
}

/// 连接参数(后端无关;现 web.rs 的 SavedConnect 迁移为此类型)
pub struct ConnectParams {
    pub chip: String,
    pub iface_index: usize,
    pub speed_index: usize,         // SPEEDS_KHZ 下标;khz 换算归 jlink 后端内部
    pub channel: u32,
    pub jlink_sn: Option<u32>,
}

/// worker 侧三个共享热切换原子(打包传递,后端在 worker 内消费)
pub struct WorkerShared {
    pub frame_timeout_ms: Arc<AtomicU32>,
    pub encoding_index: Arc<AtomicU32>,
    pub hex_rx: Arc<AtomicBool>,
}

/// web 层暴露给后端的最窄状态面(Shared 实现;后端禁止直接摸 Shared 其它字段)。
/// 注意:web.rs 自己的代码(api_* / tick_loop)直接操作 Shared,Host 只是
/// **后端实现**访问状态的边界;Host 必须 Send+Sync+'static,因为 reset_reconnect
/// 会把 Arc<dyn Host> 交给重连线程。
pub trait Host: Send + Sync + 'static {
    fn send_msg(&self, msg: WorkerMsg);                       // msg_tx.send 兜底
    /// owned 发送端:交给 'static 线程(jlink connect 的 worker 线程、
    /// device_db 桥接线程、demo 单例、reset 重连线程都靠它)
    fn msg_tx(&self) -> mpsc::Sender<WorkerMsg>;
    fn session(&self) -> Option<SessionHandle>;               // 读(clone;handle/cmd_tx 均可克隆)
    fn set_session(&self, s: Option<SessionHandle>);          // 写(connect/take 用)
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
    fn id(&self) -> &'static str;               // "jlink" | "demo"
    fn display_name(&self) -> &'static str;     // UI 下拉文案
    fn description(&self) -> &'static str;
    fn is_virtual(&self) -> bool;               // 纯软件后端(无硬件依赖)
    fn caps(&self) -> BackendCaps;
    /// 选定/启动时的初始设备候选(同步;demo=静态表,jlink=空走异步枚举)
    fn initial_devices(&self) -> Vec<String> { Vec::new() }
    /// 启动期后台枚举(结果以 WorkerMsg::DeviceNames/JLinks 异步回传)
    fn start_background_enumeration(&self, host: &dyn Host) {}
    /// 被选中为当前后端时回调(切后端时种子数据/懒启动);旧后端先收 on_deselected
    fn on_selected(&self, host: &dyn Host) {}
    fn on_deselected(&self, host: &dyn Host) {}
    fn can_connect(&self, host: &dyn Host) -> Result<(), String>;
    fn connect(&self, host: &dyn Host, params: &ConnectParams) -> Result<(), String>;
    fn disconnect(&self, host: &dyn Host);
    fn reset_in_place(&self, host: &dyn Host) -> Result<(), String>;
    /// 不支持时返回 Err(web 层对 demo 回落 in-place,与现状一致)。
    /// 拿 Arc<dyn Host>:重连线程是 'static,要克隆进去做 wait/respawn/清门闩
    fn reset_reconnect(&self, host: Arc<dyn Host>, params: &ConnectParams) -> Result<(), String>;
    fn power(&self, host: &dyn Host, on: bool) -> Result<(), String>;
    /// --demo-log 强制形态的启动自动连接(仅 demo 实现;普通形态不调用)
    fn auto_connect_at_startup(&self, host: &dyn Host) {}
}

/// Err 的统一映射(与现状文案逐字一致):can_connect/connect/reset_in_place/
/// power 的 Err → HTTP 409 + Err 文案(如"未连接"/"已有连接"/"设备库枚举中,
/// 请稍候几秒再连接")。web 层不做第二套文案。

/// 注册表(phase 1 只有按 id 取;phase 2 加 available 列表)。
/// 实现方式:两个后端都是 &'static —— jlink 无内部状态可 const 构造;demo 的
/// 内部可变性(Mutex<Option<Sender<DemoCmd>>> + 懒启动标志)用
/// `static DEMO: DemoBackend = DemoBackend { inner: Mutex::new(None), … }`
/// const 构造。**禁止经 WebOptions/Host 向后端传通道**,一切状态经 Host。
pub fn backend_by_id(id: &str) -> Option<&'static dyn RttBackend>;
pub const BACKEND_JLINK: &str = "jlink";
pub const BACKEND_DEMO: &str = "demo";
```

协议类型 `WorkerMsg / WorkerCmd / WorkerHandle / DeviceInfo / APP_SHUTDOWN`
**迁到 backend.rs**(协议 = 缝的公共契约);rtt.rs 保留 worker 内部实现。
`WorkerCmd` 删除 `DemoConnect/DemoDisconnect` 两个变体——demo 的连接/断开
走 demo 后端**内部私有**的 `DemoCmd { Connect, Disconnect }` 通道
(demo.rs 自定义,真机 worker 永远看不到),rtt.rs 里的防御性忽略分支随之删除。
`SPEEDS_KHZ`、`ENCODINGS`、`CharsetDecoder` 留在 rtt.rs(jlink 内部 + web 层
settings 夹取仍可用 `crate::rtt::SPEEDS_KHZ`)。

### 3.2 两个后端实现

**JLinkBackend**(实现在 rtt.rs 或新文件 `backend_jlink.rs`,agent 自定,模块
归 lib.rs 管):`connect` = 现 `spawn_worker`(rtt::spawn,WorkerConfig 组装、
sn 回落列表首台)+ `set_session`;`can_connect` = 存活 worker 检查 +
`device_db::busy()` 409 语义(返回 Err 文案与现状一致);`disconnect` = stop +
空 Send 唤醒;`reset_in_place` = cmd_tx 发 Reset(None → Err("未连接"));
`reset_reconnect(host: Arc<dyn Host>, params)` = 现 take_worker_down/
wait_worker_exit/1s 线程 respawn **整段迁移**,流程:take_session + stop +
唤醒 → spawn 'static 线程(持有 host 克隆)→ sleep 1s → wait_worker_exit
(用 host.session() 里旧句柄的 alive)→ 检查"等待期内用户已手动连接"
(host.session() 是否被重新填上)与 APP_SHUTDOWN → respawn → 
`host.set_reconnecting(false)` 收尾。门闩置位时序:**web 层在调用
reset_reconnect 之前置位**(web.rs:1471-1509 现状是 take 后置位,两行间无
await,等价;新时序更保守);`start_background_enumeration` =
device_db::spawn_background + DbResult→WorkerMsg 桥接线程(现 web.rs:676-691
整段迁移,线程用 `host.msg_tx()`)。

**DemoBackend**(实现在 demo.rs):单例线程保持现有结构,要点如下——
1. **懒启动**:单例线程在首次需要时(阶段二被选中后 connect,或强制形态
   auto_connect)由 `ensure_singleton(host)` 拉起:建 WorkerCmd 通道 + 
   恒活 SessionHandle(stop 标志**永远无人置位**,demo 线程只因 APP_SHUTDOWN
   退出;alive 恒 true)+ `host.set_session(...)`;内部 DemoCmd 通道存
   `static DEMO` 的 `Mutex<Option<Sender<DemoCmd>>>`。单例已存在时一切复用;
2. 命令源两个通道:WorkerCmd(Send/Power/Reset,经 session 槽)+ 内部
   DemoCmd(Connect/Disconnect)。Connect/Disconnect 分支行为 = 现
   DemoConnect/DemoDisconnect 逐行等价(manual 置位、自动周期永久挂起、
   State 发送、已连接时幂等忽略);
3. **can_connect 恒 Ok**(幂等虚拟连接,已连接再 connect 与现状一样返回
   200 由线程幂等忽略——不得照抄 jlink 的存活 409);
4. **power / reset_in_place 内部先查 `host.connected()`**,未连接返回
   Err("未连接")(保住 web.rs:41/45-46 契约注释的 demo 409 语义;
   demo.rs:96 的 Reset 静默忽略、:108 的 Power 无条件打日志不再可达);
5. `connect` 后**每次 false→true 迁移**(含启动首连、自动重连、reset 恢复、
   手动重连)线程发 `WorkerMsg::DeviceInfo(演示九字段,core_id 空串)`——
   发送点在 demo 线程的 State(true) 旁边,不在 backend 方法里,天然覆盖
   auto_connect 路径;api_status 的 demo 分叉因此可删;
6. `auto_connect_at_startup(host)`:**仅 ensure_singleton**(拉起单例线程)。
   线程原生以连接态启动(原 demo.rs 行为),首条消息自发
   `State(true, "● 已连接 (demo)")`(替代 serve() 手工预置段,由 tick 的
   State 分支统一完成 connected/session_start/标记行/托盘回调)。
   **不发内部 Connect 命令**——Connect 会置 manual=true 永久挂起 20s/3s
   自动周期,与 §2.3 状态机保留要求冲突(WP-A 落地修正)。
   **确定性论证**:demo 线程在 serve 装配阶段 spawn,首条 State 立即入队;
   tick 线程先于 axum bind 启动且**消化在前、睡眠在后**(WP-A 为此调整了
   tick 循环结构,稳态节奏不变)→ 首个 HTTP 可见时 connected=true,
   与现状预置段可观测行为一致,`test_api_status_contract` 的 connected 断言
   不受影响;
7. HEX 下沉:demo 线程发送 Block 前自己消费 `worker_shared().hex_rx`,开启时
   做 text→hex 转换(格式与现 web.rs:912-916 `text_to_hex_upper` 逐字节一致,
   该函数迁到 backend.rs 或 demo.rs 共用)。**统计口径变化(知情接受)**:现
   状 tick 先按原始文本长度计 rxBytes 再转换,下沉后 Block 已是 hex 文本,
   rxBytes 按 hex 长度计——这与真机 hex_rx 的口径**一致**(真机 worker 同样
   发 hex 文本、tick 按其长度计),属对齐修正而非回归;无测试锁定旧口径;
8. `initial_devices()` 返回现 `demo_device_names()` 六型号(表迁入 demo.rs);
   `on_selected` = `host.set_device_names(initial_devices())`;
   `on_deselected` = 发内部 Disconnect(清理连接态,单例保留);
   `start_background_enumeration` = 空实现。

### 3.3 web.rs 收敛后的形态

- `Shared` 实现 `Host`;`worker`/`cmd_tx` 两个槽合并为
  `session: Mutex<Option<SessionHandle>>`;`demo_mode: bool` 字段删除,
  换成 `backend: Mutex<&'static dyn RttBackend>`(phase 1 由 WebOptions.demo
  决定:demo → 仅 demo 后端,normal → 仅 jlink 后端,行为与现状完全一致);
- serve() 装配分叉 → `backend.start_background_enumeration(host)` +
  forced-demo 时 `backend.auto_connect_at_startup(host)` + demo `initial_devices`
  种子(set_device_names);jlink 分支的 device_db 桥接迁入 jlink 后端;
- tick_loop:Block 分支的 demo HEX 转换**删除**——demo 线程自己消费
  `worker_shared().hex_rx` 并在发送前做同样的 text→hex 转换(输出逐字节等价,
  转换函数抽到 backend.rs 或 demo.rs 共用);Exited 分支改为清 `session`
  (语义不变:仅当 alive=false 或槽已被取走);
- api_connect:校验/偏好写入不变 → `backend.can_connect`(Err→409)→
  `backend.connect`(Err→409)。门禁顺序:**reconnecting 门闩检查在
  can_connect 之前**(can_connect 是原子后端方法,alive/busy 检查在其内部;
  与原 alive→reconnecting→busy 顺序在全部可达状态下 409 结果一致,
  WP-A 落地确认);
- api_disconnect / api_power / api_reset:demo 分支全删,走 backend 方法
  (power/reset 的"未连接 409"由 demo 后端内部实现,见 §3.2 第 4 点);
  reset 的 mode 解析、last_connect 检查、reconnecting 门闩置位留在 web 层,
  `caps().reset_reconnect == false` 时(即 demo)回落调 reset_in_place
  ——与现状"demo 忽略 mode"逐字等价;reset_reconnect 传 `Arc<dyn Host>`;
- api_status:device 分叉删除(demo 走标准 DeviceInfo 消息后统一取
  shared.device_info);api_devices:demo 分叉删除(demo 的种子使
  shared.device_names 恒有值);
- 启动 println 的 `(demo)` 后缀(web.rs:751 附近)保留:它是 `--demo-log`
  **强制形态**的展示,用 WebOptions 判断,不随后端切换走;
- `demo_device_json`/`demo_device_names`/`spawn_worker`/`take_worker_down`/
  `wait_worker_exit` 从 web.rs 移除(分别迁入 demo.rs / rtt.rs 后端实现);
  `text_to_hex_upper` 抽出共用。

### 3.4 为什么不做"demo 改成连接级线程"

曾评估把 demo 改为 spawn-per-connect(与真机统一)。否决理由:demo 的
20s/3s 自动周期要在"断开态"继续存活重连,连接级线程断开即死,要么把重连
逻辑外溢到 web 层(违背解耦初衷),要么改变"手动干预后周期永久挂起"的
既有语义(破坏 test_ui_layout_p0 的确定性)。单例线程 + 内部状态翻转是
demo 后端的**内部实现细节**,trait 已把它藏在 `connect/disconnect` 之后。

## 4. 阶段二:后端可选面(API/UI/持久化)

### 4.1 契约变更(三件套同步:web.rs 模块头注释 / index.html / tests/web)

- **GET /api/backends**(新增)→
  `{"selected": "jlink", "items": [{"id","name","description","isVirtual"}]}`:
  selected = 当前选定;items = 本进程可用列表(普通形态 jlink+demo;
  `--demo-log` 强制形态仅 demo)。
- **GET /api/status** 增加 `"backend": "<id>"` 字段(其余字段不动,
  test_api_status_contract 同步加断言)。
- **POST /api/settings** 接受可选 `"backend": "<id>"`:不在本进程可用列表
  (未知 id,或已知但被强制形态排除)→ 400「未知或不可用的后端:{id}」;
  **连接态(`shared.connected`,不是 session 槽判据——demo 的 session 恒活)**
  → 409("断开后才能切换后端");成功 → 写偏好 + 换 `Shared.backend` +
  旧后端 on_deselected + 新后端 on_selected。**同 id 重复设置为幂等 no-op**,
  不触发 on_deselected/on_selected(WP-B 落地:避免 demo 的 Disconnect 无谓
  置 manual=true 挂起自动周期)。
- **StoredPrefs** 新增 `pub backend: String`(serde default "jlink";**归一化
  在 serve() 里做**——拿到 WebOptions 才知道可用列表:不在可用列表 → 回落
  列表首个,与 chip 等字段同款容错;config.rs 的 load 不感知 WebOptions)。
- **WebOptions**:`demo: bool` 保留(向后兼容 --demo-log),语义升级为
  "强制 demo 后端";新增 `backend: Option<String>`(`--backend <id>`,可选,
  优先级低于强制);`is_demo_forced()` 助手。main.rs 加 `--backend` 解析。
- **ConnectReq 不改**:连接永远用当前选定后端(切换是 settings 的职责,
  避免一条请求两个自由度的组合爆炸)。

### 4.2 前端(index.html,单文件)

- 连接设置组内、J-Link 字段之后插 `<select id="backend">`(DOM 模式照抄
  iface/speed 的 .field 结构);
- 初次加载 fetch `/api/backends` 填充(items 渲染 option,selected 置值;
  DOM API 构建option,遵循注入面纪律);
- change → `postPref('backend', { backend: value })`(防抖接线插在现有
  913-918 区);`setState` 的连接态禁用列表(:619)加入 'backend';
  demo 后端选中时 J-Link 下拉一并禁用(无硬件探针列表);
- `/api/prefs` 回填 handler 与 restorePrefs localStorage 兜底各加 backend。

### 4.3 语义决策(已定,不再讨论)

- 切换后端仅在断开态允许(连接态 409)。运行中切换会话归属太复杂,价值低;
- demo 后端在普通形态可选(这是"界面选后端"的核心价值:无硬件体验全功能);
  demo 单例线程懒启动(首次 connect 才 spawn),选中即种子设备表;
- `--demo-log` 强制形态:后端列表仅 demo、启动自动连接——与现状逐字节一致,
  19 条既有 pytest 不改一行仍须全绿;
- 切走 demo(demo 会话存在但已断开)→ on_deselected 发内部 Disconnect,
  保证状态干净;切回 jlink → on_selected **清空 device_names(避免 demo
  候选残留)+ 启动设备库枚举(若从未启动)**,候选随后由异步枚举回填。

## 5. 分工(子代理工作包)

| WP | 范围 | 验收(全自动化,零人工) |
|---|---|---|
| WP-A 阶段一 Rust 核心化 | backend.rs 新建 + 协议类型迁移(**gui.rs 的 `crate::rtt::APP_SHUTDOWN` use 须同步改**,gui.rs:219)+ rtt/demo 后端化 + web.rs 收敛(§3 全部) | `cargo test --lib` 全绿;`cargo build --release` 成功;`--demo-log` 冒烟(隔离 prefs)与现状一致;19 条 pytest 全绿(前端未动,复用现有 release exe 需重编) |
| WP-B 阶段二选择面 | config.rs 字段、WebOptions/CLI、/api/backends、status.backend、settings.backend、on_selected/deselected、index.html 下拉、web.rs 模块头契约注释(§4 全部)+ **test_api_status_contract 加 backend 断言**(契约同步三件套随改动即做) | cargo test --lib 全绿(新增 prefs/registry 单测);release 构建成功;冒烟:普通形态经 API 选 demo→连接→收流→发送→复位→断开→切回;19 条 pytest 全绿(含更新后的 status 契约) |
| WP-C 测试闭环 | 新增 tests/web/test_backends.py(§6 用例表) | `python -m pytest tests/web -q` 全量绿(19 旧 + 新增),无残留进程 |
| WP-D 总验证 + 文档 | 主会话执行:全量验证矩阵复跑、修复循环、AGENTS.md 回写、ADR 追加、设计文档状态更新 | §6 验证矩阵全绿;文档三处一致 |

依赖:WP-A → WP-B → WP-C → WP-D,严格串行(web.rs 是热点文件,禁止并行改)。
每个 WP 的代理只允许改自己范围内的文件;发现范围外问题 → 报告,不顺手修。

## 6. 验证矩阵(WP-D 执行)

1. `cargo test --lib` — 含既有全部单测(线格式/设备键名/log_model 断行)+ 新增
   backend 注册表与 prefs 容错单测;
2. `python -m pytest tests/web -q` — 19 条既有 + test_backends.py 新增:
   - 普通形态(无 --demo-log):/api/backends 契约、status.backend 字段、
     settings 选 demo → connect(STM32F103C8)→ connected=true + device 8 字段 +
     rowsTotal 增长 → send 使 txBytes 增长 → reset in-place 200 → 断开 →
     connected=false → 连接态切后端 409、断开态切回 200、非法 id 400;
   - 强制形态(--demo-log):/api/backends 仅 demo、启动即 connected、
     settings 选 jlink → 400;
   - 普通形态不触碰真实 J-Link(不测 jlink connect 的硬件路径);
3. 冒烟:两种形态 `--no-window` + curl(必须 `RTT_PREFS_FILE` 隔离,检查
   tasklist 无残留);
4. jlink 后端在无硬件下的行为回归:普通形态默认 jlink,api_connect 到真机
   失败路径不崩(仅人工观察一次冒烟,不进自动化——无硬件环境无法稳定断言)。

## 7. 风险与红线

- **latch 铁律逐条保命**:Exited 门闩语义(session 清理条件)、reconnect 门闩、
  BUSY 门闩一个都不能丢;demo 线程的 APP_SHUTDOWN 退出检查保留;
- **锁纪律**:Host 实现的方法体内禁止碰 pump 锁;持 session 锁期间不得再嵌套
  锁其它 Mutex 字段(现 worker/cmd_tx 双锁改单锁后风险下降,但保持短借);
- demo HEX 转换下沉必须逐字节等价(对比现 web.rs:912-916 的实现);
- demo 启动 State(true) 改由消息驱动后,gui 托盘回调时序差 ≤1 个 tick(10ms),
  可接受;若发现托盘/首屏测试受影响,回退方案:auto_connect 里同步调
  host 的 seed 路径(需在 Host 加 seed_connected),**先测再决定**;
- 契约三件套同步是硬要求:web.rs 模块头注释、index.html、tests/web、
  web.rs 内两个契约锁单测,任何 API 变更四处对齐;
- 禁止触碰:`feat/probe-rs-backend` worktree、`docs/audit/`、用户真实
  `%APPDATA%/MiniRttViewer/prefs.json`(一切 exe 运行必须 RTT_PREFS_FILE 隔离);
- 不发版、不推 tag、不 commit(留给用户确认后统一处理)。

## 8. 未来接入 probe-rs 的路径(本设计的验收标准)

接入 = ① 新模块 `backend_probe_rs.rs` 实现 RttBackend(enumerate→DebugProbe
列表,connect→probe-rs 会话,WorkerMsg 复用);② backend_by_id 注册;
③ StoredPrefs/UI 无需新字段(backend 选项自动出现)。若实现中发现 web 层
还需要为它写 if,说明缝漏了 —— 回改 backend.rs,而不是在 web.rs 加分支。
