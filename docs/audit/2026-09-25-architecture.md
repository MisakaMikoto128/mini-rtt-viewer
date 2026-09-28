# Mini RTT Viewer — 全库架构审计（T1）

- 日期：2026-09-25 · 分支 `audit/nova-20260925` · HEAD `885a895`
- 范围：Cargo.toml / build.rs / AGENTS.md / src 全部（11 个模块，约 4950 行）/ ui/web/index.html（1352 行）/ examples / tests 结构
- 评级口径：P0 = 数据损坏/安全暴露/崩溃；P1 = 功能缺陷或边界错误，可稳定触发；P2 = 边界条件/可维护性风险；P3 = 死代码/文档漂移/轻微问题
- 每条发现附 文件:行号 + 原文引用（≤10 行）。不确定项明确标注。

---

## A 架构概述

单 exe 双形态：Rust 数据层 + 内嵌 Web 管理台（axum 0.8 + tokio），桌面壳用
wry 0.57 + tao 0.37 + tray-icon 0.25 把同一管理台加载进 WebView；`--no-window`
纯服务形态共用同一数据层。两种形态的差异只在装配层（`web::run` vs
`gui::run_gui` → `web::spawn_gui_service`），数据层零分叉。

线程模型（serve 启动后共 4+ 类常驻线程）：

| 线程 | 创建点 | 职责 |
|---|---|---|
| tokio runtime（多核） | `src/web.rs:148` `tokio::runtime::Builder::new_multi_thread()` | HTTP/WS 请求处理 |
| rtt-service（桌面壳） | `src/web.rs:162` `std::thread::Builder::new().name("rtt-service".into())`（`spawn_gui_service` 定义在 158 行） | 整个 tokio runtime 搬后台线程（tao 事件循环占死主线程） |
| tick 数据泵 | `src/web.rs:673` `std::thread::spawn(move || tick_loop(shared, msg_rx))`（`tick_loop` 定义在 859 行） | 10ms 消化 worker 消息 → LogPump → broadcast 广播；偏好快照落盘；定时发送 |
| worker（按需） | `src/rtt.rs:143` `pub fn spawn(...)` | 真机 J-Link DLL 连接序列 + 5ms RTT 读循环；demo 模式下由 `src/demo.rs:51` 的虚拟 worker 替代 |
| device_db 后台枚举 | `src/device_db.rs:64` `std::thread::spawn(move || ...)` | 缓存优先枚举设备库/J-Link 列表 |
| db→msg 转投线程 | `src/web.rs:657` `std::thread::spawn(move || { while let Ok(r) = db_rx.recv() ... })` | 把 DbResult 转 WorkerMsg 投进主消息通道 |
| reset reconnect 线程（按需） | `src/web.rs:1418` `std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(1)); ... })` | 等旧 worker 退出后重连 |

数据流单向：worker → `mpsc::Sender<WorkerMsg>`（无界）→ tick 线程 →
`LogPump`（Mutex 保护）→ `broadcast::Sender<String>`（容量 512，
`src/web.rs:583`）→ 各 WS 连接。前端 → REST POST → `Shared` 原子字段/互斥
字段 → worker 热切换。

依赖面：libloading（JLink_x64.dll 运行时加载）、vte 0.13（ANSI 状态机）、
encoding_rs 0.8（5 字符集）、unicode-width 0.2（显示列宽）、regex-lite 0.1
（搜索，前端正则走 JS RegExp，服务端无正则）、dirs 6（%APPDATA% 解析）、
serde/serde_json、axum/tokio/wry/tao/tray-icon、windows-sys 0.61（仅
TerminateProcess 与 GetLocalTime 两个 FFI）。Cargo.toml 未见开发期依赖外漏
到 release 的异常。build.rs 只做图标嵌入（PNG 解码 + winres），无网络、无
代码生成副作用。

## B 模块耦合

- `lib.rs`（18 行）是模块树唯一真源，bin/examples 一律 `use mini_rtt_viewer::…`，
  层次：main（装配/CLI）→ web::Shared（共享状态 + 业务方法）→
  log_model/ansi（纯逻辑，可单测）→ rtt（worker 线程）→ jlink_dll（FFI）→
  device_db（后台枚举）。无环依赖，耦合面窄。
- `gui.rs` 与 `web.rs` 的契约只有 `WebOptions`/`ServiceEvent`/`OnEvent`
  三个类型（`src/web.rs:135` `pub enum ServiceEvent`），回调注释明确要求非阻塞
  （`src/web.rs:142` `/// 事件回调:在服务线程/数据泵线程触发,**必须非阻塞**`）——
  这是隐式契约，靠注释而非类型保证。
- `ansi::Run` 同时是 WS 线格式（经 `serialize_fg`，`src/ansi.rs:21`）和 LogPump
  内部存储，跨模块共享序列化契约；线格式由单测
  `ws_event_wire_format_matches_legacy_contract` 锁死（`src/web.rs:1884`）。
- `WebOptions.port` 被存入 `Shared.port`（`src/web.rs:625`）供
  `/api/open-browser` 拼回环地址（`src/web.rs:786`），而监听地址是
  `Shared.listen`（`src/web.rs:626`）。FR-28 后这里出现口径分歧：
  `/api/addr` 用 `shared.listen.is_loopback()` 判断开放状态（`src/web.rs:771`
  `open: !shared.listen.is_loopback()`），但 `/api/open-browser` 的回退地址恒为
  `http://127.0.0.1:{port}`（`src/web.rs:786`
  `.unwrap_or_else(|| format!("http://127.0.0.1:{}", shared.port))`）。
  当 `--listen` 指定具体网卡 IP 时回环未监听（serve 的注释自己说明了这一点：
  `src/web.rs:703` "绑具体网卡 IP 时 回环未监听,只能用该 IP 访问"），此时
  关于框外链回退与壳内「浏览器打开」在纯回环不可达的部署下会指向一个无法
  访问的地址。**[P2]** 建议 open-browser 回退地址与 /api/addr 的本机地址口径
  对齐（listen 为回环时用 127.0.0.1，否则用 listen IP）。
- 前端 `ui/web/index.html` 经 `include_str!` 内嵌（`src/web.rs:101`），无独立
  构建步骤；tests/web 是 Python 黑盒（pytest 打 HTTP/WS 端点），Rust 侧
  tests/ 目前为空目录（本轮 T3 起填充）。

## C 并发与状态

- `Shared`（`src/web.rs:196`）按字段细粒度加锁：热参数用 Atomic
  （frame_timeout_ms/encoding_index/hex_rx/connected/seq），结构性数据用
  Mutex（pump/worker/cmd_tx/device_names/…）。字段约 30 个，无文档化锁序，
  当前所有路径都是"每次只借一个"，未见嵌套持锁。
- **tick 持 pump 锁横跨整段消息循环**：`src/web.rs:870`
  `let mut pump = shared.pump.lock().unwrap();` 持有到 `src/web.rs:1013`
  `drop(pump);`，中间在 State/Progress/DeviceInfo/DeviceNames/JLinks/Exited
  分支内还要分别锁 session_start/status_text/device_info/device_names/jlinks/
  worker/cmd_tx（如 `src/web.rs:913` `*shared.status_text.lock().unwrap() = ...`）。
  锁本身是逐条短借，不嵌套，不违规；但 pump 锁覆盖整个 try_recv 内循环，
  而 `send_payload`（HTTP 线程，`src/web.rs:275` `self.pump.lock().unwrap()`）
  与 `api_mark`/`api_pause`/`api_clear`（`src/web.rs:1590` 等）都竞争同一把锁。
  极端情况下 worker 突发大块时，泵线程持泵锁做 ANSI 解析 + 行切分，HTTP 侧
  清空/暂停/回显会阻塞到本 tick 结束。**[P3]** 10ms 一拍的粒度下影响有界
  （单个 tick 内消息量有限），登记即可，不建议为此拆锁。
- **`send_payload` 在 tick 线程内被定时发送路径调用**：`src/web.rs:1047`
  `let text = shared.timer_text.lock().unwrap().clone();` 之后
  `src/web.rs:1049` `if let Err(e) = shared.send_payload(&text, ...)`。此时
  pump 锁已在 `src/web.rs:1013` drop，`send_payload` 内部重新
  `self.pump.lock().unwrap()`（`src/web.rs:275`），无重入，安全——但这条
  路径与 AGENTS.md「tick 持 RefCell borrow 期间严禁再借」是同源教训，靠
  "drop 点早于调用"保证，属于易回归点（见 G-8）。
- 消息通道无界：`src/web.rs:584` `let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();`
  是 std::sync::mpsc 无界队列。worker 读循环 5ms 一拍（`src/rtt.rs:309`
  `while !stop.load(...)`），tick 10ms 一拍；若 tick 线程卡住（例如上条的
  锁竞争或极端慢盘落盘 `config::save`），worker 数据无上限堆积。设备侧速率
  有限（RTT 通道读取是 pull 式），实际风险低。**[P3]**
- 广播滞后处理正确：WS 订阅 `Lagged` 时重发全量快照（`src/web.rs:1666`
  `Err(broadcast::error::RecvError::Lagged(_)) =>`），与前端 `m.type === 'snapshot'` 的 resetRows 路径（`ui/web/index.html:548`
  `if (m.type === 'snapshot') { resetRows(); m.rows.forEach(r => appendRow(r, false)); ... }`）
  闭环。前端 2s 轮询 /api/status 是 WS 之外的兜底
  （`ui/web/index.html:1267` `setInterval(() => fetch(API + '/api/status')...`）。
- **J-Link 下拉选中值未接线**：`src/web.rs:1216` 注释自认——
  `/// 注意:selected_sn 取 J-Link 列表**首台**——前端 #jlink 下拉的选中值尚未接入`，
  实现在 `src/web.rs:1225`
  `selected_sn: shared.jlinks.lock().unwrap().first().map(|(sn, _)| *sn)`。
  前端 `connect()` 请求体只有 chip/ifaceIndex/speedIndex/channel
  （`ui/web/index.html:715` `const body = {` 起 5 行），确实不携带 jlink sn。
  多台 J-Link 接入时下拉形同虚设，实际永远连首台。**[P1]**（功能缺口，
  非崩溃；单台场景无感。）
- worker 生命周期门闩：`api_connect` 检查
  `h.alive.load(Ordering::Relaxed)`（`src/web.rs:1243`
  `.is_some_and(|h| h.alive.load(Ordering::Relaxed))`）拒绝并发 worker；
  `src/rtt.rs:160` `let _ = tx.send(WorkerMsg::Exited);` 保证线程退出必广播
  Exited；`wait_worker_exit` 超时（3s）后放弃重连（`src/web.rs:1422`
  `if !wait_worker_exit(old.as_ref(), Duration::from_secs(3)) {`）。
  注意 `take_worker_down` 立即清空句柄引用（`src/web.rs:1350`
  `let old = shared.worker.lock().unwrap().take();`），此后 1s 等待窗口内
  `api_connect` 的门闩检查会因 `worker == None` 而放行新连接——等待期内
  `src/web.rs:1428` `if shared2.worker.lock().unwrap().is_some() ... { return; }`
  重连线程自己会让位，但**用户在这 1s 窗口手动点连接会双 worker 并发**，
  正是 AGENTS.md「worker 生命周期三条铁律」第 1 条禁止的场景。窗口仅 1s、
  需精确时序触发。**[P1]**（不确定,需人工确认：是否可达——取决于用户在
  点「复位(reconnect)」后 1s 内恰好手动点「连接」；触发路径真实存在。）

## D 错误处理

- FFI 边界：JLinkDll 全部 `self.lib.get(...).unwrap()`（如 `src/jlink_dll.rs:56`
  `self.lib.get(b"JLINKARM_Open").unwrap();`），符号缺失 = 进程 panic。加载
  候选 DLL 本身走 `anyhow`（`JLinkDll::load()?`），但符号解析用 unwrap——
  SEGGER 官方 DLL 符号集稳定，实际风险低；release `panic="abort"` 下表现为
  进程直接消失。**[P3]**
- `exec_command` 用 `CString::new(cmd).unwrap()`（`src/jlink_dll.rs:125`
  `let c = CString::new(cmd).unwrap();`）——内嵌 NUL 会 panic。当前所有调用方
  的入参来自设备库名单或序列号数字（`src/rtt.rs:237`
  `let resp = jlink.exec_command(&format!("Device = {chip}"));` 与
  `src/rtt.rs:211` `jlink.exec_command(&format!("SetHostIF USB = {sel}"));`），
  而 chip 已过设备库校验（见 F 节）。无当前可达的 panic 路径。**[P3]**
- worker 错误不向上抛：`src/rtt.rs:155` `if let Err(e) = result {`
  → `State(false, format!("● 错误: {e}"))` 走 UI 通道，线程仍发 Exited
  （`src/rtt.rs:160`），UI 侧恢复路径完整。
- 配置读写全静默降级：`config::load()` 损坏/缺失回默认（serve 启动注释
  `src/web.rs:565` "文件缺失/损坏/字段缺失一律回落默认,绝不阻塞启动"）；
  落盘失败 `let _ =` 忽略。prefs 损坏后静默重置为默认——用户无感知地丢失
  所有面板配置（含发送历史）。**[P3]** 可接受，建议未来在 stderr 留痕。
- HTTP 错误语义：HEX 解析失败 400 + 中文错误信息回传前端 toast
  （`src/web.rs:1497` `return (StatusCode::BAD_REQUEST, format!("HEX 格式错误:{e}")).into_response();`），
  前端 `send()` 的 `else` 分支展示（`ui/web/index.html:784` 起）。连接冲突
  409、未连接 409 一致。`api_open_browser` 对任意畸形 body 静默回退本地地址
  并返回 200（`src/web.rs:782` 起），不报错——可接受，登记。
- 定时发送失败只 `eprintln!`（`src/web.rs:1051`
  `eprintln!("[mini-rtt-viewer] 定时发送失败:{e}");`），不打断泵，无 UI 提示。
  用户视角：定时开关开着但静默不发（HEX 内容非法时）。**[P3]**

## E 资源生命周期

- DLL 句柄：worker 所有退出路径都 `rtt_control(RTT_CMD_STOP)` + `close()`
  （`src/rtt.rs:419-420` `let _ = jlink.rtt_control(RTT_CMD_STOP); jlink.close();`；
  失败路径 `src/rtt.rs:255` `jlink.close();`）。`JLinkDll` **没有实现 Drop**
  （grep 无 `impl Drop`），释放靠调用方纪律；若未来新增提前 return 路径漏掉
  close 即泄漏 DLL 状态。建议加 Drop 兜底（登记，不动手）。
- 进程退出：`gui.rs` 用 `TerminateProcess(GetCurrentProcess())` 终局
  （`src/gui.rs:238` `TerminateProcess(GetCurrentProcess(), code);`），绕过
  WebView2 `DLL_PROCESS_DETACH` 僵尸态（模块头注释 `src/gui.rs:18` 记录了
  2026-09-19 现场）。`APP_SHUTDOWN` 静态量在退出前置位（`src/gui.rs:219`
  `crate::rtt::APP_SHUTDOWN.store(true, Ordering::Relaxed);`），worker/demo
  各自轮询它退出（`src/rtt.rs:309`、`src/demo.rs:66`）。
- WS 连接：30s ping 保活（`src/web.rs:1681`
  `if socket.send(Message::Ping("".into())).await.is_err() {`），发送失败即
  break，订阅随函数返回析构。无连接数上限——本机场景可忽略，`--listen 0.0.0.0`
  开放时任何人可开无限 WS/HTTP 连接（与 F 节无鉴权叠加，见 F-1）。**[P2]**
- 文件描述符/句柄泄漏面：`config::save` 原子写 tmp→remove→rename
  （`src/config.rs:147` 起），中途崩溃留 `.json.tmp` 残留但下次写覆盖；
  `prefs_path` 的 `RTT_PREFS_FILE` 覆盖分支对路径无校验
  （`src/config.rs:110` `if let Ok(p) = std::env::var("RTT_PREFS_FILE") {`），
  写目标完全受环境变量控制——本机单用户工具，登记为**[P3]**（攻击面同
  无鉴权问题，见 F-1）。
- 内存：LogPump 行上限 500（`src/log_model.rs:80`
  `pub const MAX_LOG_ROWS: usize = 500;`）+ 单行 512 字符
  （`src/log_model.rs:77`），pending 半行缓冲由 enforce_line_cap 兜底
  （`src/log_model.rs:183`），无无界增长。前端 `rowCap` 初始 500 由
  stats.cap 覆盖（`ui/web/index.html:493` `let rowCap = 500;`），两端同源。
- broadcast 容量 512（`src/web.rs:583` `let (events_tx, _) = broadcast::channel(512);`）
  ——注释与实际一致（本文件内 grep 确认仅此一处 channel 定义）。

## F 安全（web.rs + 前端 XSS）

**F-1 无鉴权 + 可开放监听 = 局域网全控面 [P1]**
服务无任何认证（全文件 grep 无 CORS/Authorization/Token 相关代码）。默认仅
回环（`src/main.rs:40` `let mut listen: IpAddr = IpAddr::from([127, 0, 0, 1]);`），
但 FR-28 后 `--listen 0.0.0.0` 成为一等公民
（`src/main.rs:63` `"--listen" => {`，help 文案
`src/main.rs:23` "0.0.0.0 开放局域网/互联网, 管理台无鉴权,仅建议可信网络使用"），
且前端有显式引导：远程框提示
`ui/web/index.html:478` "局域网未开放:以 --listen 0.0.0.0 启动后,其他设备打开
局域网地址即可远程操作"。开放后任何局域网设备可：读取 RTT 数据（WS/
/api/export）、向目标芯片发送任意字节（POST /api/send）、复位目标（/api/reset）、
开关 J-Link 电源（/api/power）、改配置（/api/settings）。这是**有意的设计取舍**
（代码与 SECURITY.md 均如实声明），但风险等级随产品形态升级了：从"本机工具"
变成"局域网调试服务"。建议最低限度补一层可选 token（query 或首屏口令）或至少
HTTP 层记录非回环来源的写操作。登记为 [P1]（设计已知，需用户决策）。

**F-2 前端 XSS 面：两处 innerHTML 拼接服务端数据**
1. `ui/web/index.html:627`
   `list.innerHTML = opts.map(o =>` —— 设备库候选名经 innerHTML 注入。候选名
   来自 J-Link DLL 设备库（`device_db.rs` 枚举）或磁盘缓存
   `device_names.txt`（`src/device_db.rs:27` `fn cache_path()`，用户可手工编辑
   该文件）。若名单中含 `<img onerror=...>`，管理台本地执行任意 JS——
   同域下可读取 WS 全部 RTT 数据、调用全部 API。缓存文件是纯文本每行一名，
   注入成本极低。真机 DLL 名单受 SEGGER 控制，风险低；但缓存文件是本机用户
   可读写的信任边界外文件。**[P1]**
2. `ui/web/index.html:679-680`
   `list.innerHTML = '<option value="">自动</option>' +`
   `list.map(j => '<option value="' + j.sn + '">' + j.name + '</option>').join('');`
   —— J-Link 产品名来自 DLL `acProduct` 字段（`enumerate_emulators`），
   理论上固件字符串可含 HTML 字符。实际 SEGGER 固件字符串受控，
   不确定,需人工确认是否可达。
其余渲染路径全部安全：日志行/设备信息/状态文案均走 `textContent`
（`ui/web/index.html:621` 起 `renderDevice` 用 createElement+textContent；
`setState` 用 `$('status').textContent = ...`）；搜索高亮的 `seg()` 也走
textContent（`ui/web/index.html:1115` `n.textContent = run.text.slice(from, to);`）。

**F-3 服务端注入面：已检查，未见异常**
- `/api/theme-css/{id}`：id 只在「本机 read_dir 扫描列表」里精确匹配
  （`src/web.rs:1165` 起 `let hit = scan_custom_themes(&themes_dir()).into_iter().find(|t| t.id == id);`），
  路径不拼用户输入；文件名白名单 `valid_theme_file_name`
  （`src/web.rs:361`）拒绝非 ASCII/分隔符/超长。**已检查,未见异常**。
- `/api/open-browser`：URL 前缀白名单 `https://github.com/MisakaMikoto128/`
  （`src/web.rs:781` `const REPO_PREFIX: &str = ...`），其余一律回退本地管理台。
  命令是 `cmd /c start "" url`（`src/web.rs:745`），url 不含空格注入面
  （白名单前缀 + 固定回退）。**已检查,未见异常**。
- `exec_command("Device = {chip}")`：chip 在 spawn 前经设备库全名/子串校验
  （`src/web.rs:1258` `let exact = full.iter().any(|n| n.eq_ignore_ascii_case(&chip_raw));`），
  设备库条目来自 DLL 或 demo 固定名单，`Device = X` 注入 DLL 命令解析器
  需先污染设备库文件——与 F-2-1 同信任边界（本机用户可编辑
  device_names.txt，但注入的是 DLL 命令而非 Shell）。**已检查,未见异常**
  （前提：设备库缓存可信）。
- 导出文件名来自服务端本地时间（`now_stamp`），不含客户端输入。
- CORS：服务不设 CORS 头，跨站页面无法读响应（CORS 拦读不拦同源外的
  简单请求发送；POST 带 JSON 触发预检，预检无响应即失败）。写操作仍可从
  跨站发起（no-cors 模式/简单请求），但无法读数据。与 F-1 同属"开放监听"
  子风险。**已检查,未见额外异常**。

**F-4 `lan_ipv4` 探测**：`src/web.rs:751` 起，UDP connect 8.8.8.8:80 选路，
无发包、无隐私外泄（DNS 不解析，纯选路）。**已检查,未见异常**。

## G 改进 TOP 10（按收益/成本排序）

| # | 级别 | 改进项 | 位置 |
|---|---|---|---|
| G-1 | P1 | 前端设备候选下拉与 J-Link 下拉改 `textContent` 建节点，消除 innerHTML 注入面（F-2） | `ui/web/index.html:627`、`ui/web/index.html:679` |
| G-2 | P1 | `--listen 0.0.0.0` 开放后补最低限度写保护（可选 token 或首屏口令），或至少在 SECURITY.md 明示「/api/send 可向目标注入任意字节」的攻击面 | `src/web.rs:716`（bind 处） |
| G-3 | P1 | J-Link 下拉 sn 接入 /api/connect 请求体（当前恒连首台） | `src/web.rs:1225`、`ui/web/index.html:715` |
| G-4 | P1 | reset reconnect 的 1s 窗口内加"重连中"门闩位，`api_connect` 检查它，堵双 worker 并发窗口 | `src/web.rs:1418`、`src/web.rs:1239` |
| G-5 | P2 | `/api/open-browser` 回退地址改用 listen 口径（非回环监听时回环不可达） | `src/web.rs:786` |
| G-6 | P2 | WS/HTTP 连接数上限（开放监听场景），或至少文档化 | `src/web.rs:716`、`src/web.rs:1645`（ws_loop） |
| G-7 | P3 | `JLinkDll` 实现 `Drop` 兜底 close，防未来新增路径漏释放 | `src/jlink_dll.rs`（无 Drop impl） |
| G-8 | P3 | `FLUSH_MS`（`src/log_model.rs:73`）与 `ScrollGeom`（`src/log_model.rs:106`）在 Web 化后均无消费方（tick 硬编码 10ms、滚动几何在前端），死代码去留决策；`/api/status` 的 `phase`/`port` 字段（`src/web.rs:1108-1109`）前端不消费，同属死契约 | 三处 |
| G-9 | P3 | 配置损坏静默重置加 stderr 留痕；prefs 原子写失败（磁盘满/只读目录）同样静默 | `src/config.rs:147` 起 |
| G-10 | P3 | 设备库缓存文件 `device_names.txt` 是 F-2 的信任边界外文件，可在 UI 加「重建设备库」按钮强制重新枚举（绕开被污染的缓存） | `src/device_db.rs:65`（缓存命中分支） |

---

### 本轮发现统计
P0: 0 · P1: 4（F-1 无鉴权开放监听、F-2 innerHTML 注入、C 节 J-Link 下拉未接线、C 节 reconnect 双 worker 窗口）· P2: 2（open-browser 回退地址口径、无连接数上限）· P3: 7
引用抽查（S5）：`src/web.rs:1225` selected_sn 首台、`ui/web/index.html:627` innerHTML 拼接——均已逐字核对，无坏条。
