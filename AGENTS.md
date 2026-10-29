# Mini RTT Viewer — 项目经验笔记

## 项目现状速览(2026-10-29,v0.5.2 + RTT 收发与状态可靠性修复)

**一句话**:J-Link RTT 日志查看器,单 exe 双形态——桌面壳(tao+wry+tray 内嵌
WebView)与纯服务(`--no-window`)共用同一套 Rust 数据层 + axum Web 管理台。
**数据采集后端已抽象化**(`RttBackend` trait,正式程序仅注册 J-Link),
接缝定义在 `src/backend.rs`,现行决策见 `docs/team/decisions.md` ADR-13。
无硬件 Web 测试使用独立 `examples/qa_server.rs` 显式注入 QA 后端,不进入正式
UI/CLI/注册表。**旧 Slint 前端已退役**,
凡是描述 Slint/`log_view.slint`/`main.rs Ctx::tick` 的旧笔记(含 git 历史
里的本文件旧版)只作历史参考,别照着改代码。

**架构与数据流**(模块树唯一真源 `src/lib.rs`,bin/examples 一律
`use mini_rtt_viewer::…`):
- `main.rs` 只做 CLI 装配:`--backend/--port/--listen/--no-window/--no-tray`
  → `web::run`(纯服务)或 `gui::run_gui`(桌面壳)
- `backend.rs`(缝,2026-09-28 新增):`RttBackend` trait + `Host`(Shared
  实现的最窄状态面)+ `SessionHandle` + 协议类型 `WorkerMsg/WorkerCmd/
  WorkerHandle/DeviceInfo/APP_SHUTDOWN` + 注册表 `backend_by_id`;
  `backend_jlink.rs`:JLink 后端(worker spawn/枚举桥接/reset reconnect 全在
  这);web 层**禁止**再出现按后端名的 if 分叉
- `web.rs` 是 Web facade(服务装配/固定路由/后端解析);`src/web/` 按职责拆为
  `state.rs`(`Shared` + Host)、`api.rs`(HTTP)、`pump.rs`(10ms 数据泵)、
  `ws.rs`(事件线格式/快照/重同步)与 `theme.rs`。数据泵消化 worker 消息 →
  LogPump → broadcast;500ms 统计节流 + 偏好快照落盘 + 定时发送
- `rtt.rs`:JLink worker 线程,`connect_target`(连接序列)+ `rtt_read_loop`
  (5ms 轮询 + 断帧判定 + 字符集/HEX 热切换);`jlink_dll.rs`:FFI
- `log_model.rs`:纯逻辑日志泵(断行/ANSI/512 兜底/500 行上限),可单测
- `config.rs` 偏好持久化(含 `backend` 字段)/ `device_db.rs` 设备库后台枚举
  (归 jlink 后端驱使);`examples/qa_server.rs` 是仅供黑盒测试的独立数据流
- 前端 `ui/web/index.html` + `styles.css` + 原生 ES modules 经 `include_str!`
  内嵌,无独立构建步骤;`app.js` 只做全局协调,API/日志视图/搜索/显示设置/
  弹窗分别位于同名模块;`sound.js` 用 Web Audio 合成短正弦波,只在用户手势后
  解锁。业务偏好只由服务端持有,localStorage 仅存主题/字号/音效等界面偏好

## 后端抽象(2026-09-28,RttBackend 缝的纪律)

- **加新后端 = 新文件实现 `RttBackend` + `backend_by_id` 注册**,web 层零改动;
  若发现 web 层需要为某后端写 if,说明缝漏了——回改 backend.rs,别在 web.rs
  加分支。
- **probe-rs 实验结论(勿重走弯路)**:J-Link 探针 × probe-rs 在 Windows 上
  **驱动互斥**(probe-rs 走 nusb 纯 USB 要求 WinUSB,与 SEGGER 驱动不共存;
  pyOCD 的 J-Link 插件底层也是 pylink→DLL)。产品决策:J-Link 永远走 jlink
  后端(默认,零操作);probers 实验整体在 v2 分支,含 tools/winusb 免 Zadig
  附录,**不要再提议给用户换驱动**
- 正式后端只有 J-Link;无硬件测试由 `web::run_with_backend` 在独立 QA 示例进程中
  显式注入,**不得注册到 `backend_by_id` 或增加正式 UI/CLI 入口**。连接门禁判据用
  `Shared.connected`,不要仅以 session 槽判空推断可连接状态
- 后端设置契约:settings 切后端断开态才允许(connected 判据 409);同 id 幂等
  no-op;旧偏好或 CLI 中的未知/已删除后端 id 在 serve 归一化为 J-Link
- 归一化时机:`StoredPrefs.backend` 在 serve()(拿到 WebOptions 后)才校验
  归位,config.rs 的 load 不感知可用列表

---

## J-Link DLL 连接序列是硬性要求,不能调换

**现象**:单次 `Open → Connect` 的"简洁"序列能连上但 RTT 永远没数据。

**硬约束**(全部来自实测,`src/rtt.rs` `connect_target`):
1. `JLINK_RTTERMINAL_Control(START)` 必须在 `Connect` **之前**;
2. `disable_dialog_boxes()` 必须在 Open **之前**(调试器选择窗/固件升级提示
   都发生在 Open 内部,晚抑制就卡死在隐藏模态框上);
3. **多台 J-Link 选定三件套缺一不可**(实测 V8.24):
   `JLINKARM_EMU_SelectByUSBSN(sn)` → `ExecCommand("SetHostIF USB = <sn>")`
   (无它 Open 会弹 probe 选择窗并用弹窗里的选择覆盖选定,这正是
   "下拉选了 888 实际连了 758"的根源)→ **`OpenEx(null,0)` 代替 `Open`**
   (OpenEx 尊重选定,不再弹窗);打开后读 GetSN 与选定比对,不符告警
   (Open 前读 GetSN 只反映选定值,会误导);
4. `open_ex`/`select_tif`/`set_speed` 的返回码必须检查(审计 R2 后已加)——
   否则 Open 失败时 GetSN=0 的误导日志会顶掉真实失败环节;
5. 首次 connect 失败 worker 内自动重试 3 次(间隔 400ms),不回退 UI 状态。
6. Connect 成功后按设备数据库 RAM 区域分块定位 `SEGGER RTT` 控制块,校验缓冲区
   数量和通道后用 `SetRTTAddr` 显式设置并重挂 RTT;找不到才保留 J-Link 自动发现。
   STM32G070 实测自动发现连接成功但 RX=0,显式定位后立即恢复;这不改变第 1 条
   初始 START 早于 Connect 的硬约束。

## worker 生命周期(铁律 + 2026-09 补强)

1. **Exited 门闩**:worker 存活期间(含阻塞在 DLL connect 时)严禁 spawn
   新 worker。UI 收到 `Exited` 才允许再连;`alive=false` 在 `Exited` 前置位,
   web.rs 依赖此顺序。
2. **connect 阶段也检测退出**(审计 R1 后已加):`connect_target` 在每个
   DLL 阻塞调用之间检查 `stop_requested(stop)`(stop 或 APP_SHUTDOWN)——
   否则关窗时 worker 卡在 `JLINKARM_Connect`,3s 看门狗强杀不清理 DLL 会话。
3. **reconnect 门闩**:reset reconnect 的 1 秒等待窗口内 `worker` 已被
   take 清空,存活检查会放行——`Shared.reconnecting: AtomicBool` 堵窗口
   (api_connect/api_reset 双侧检查,审计 P1 后已加)。
4. **BUSY 门闩必须接回**:`device_db::busy()` 枚举期间置位(先于
   `JLinkDll::load()`),`api_connect` 检查到 BUSY 返回 409——枚举与
   connect 并发加载 DLL 有损坏 DLL TLS 的风险(原 Python 项目实测)。
   此门闩曾因 Slint→Web 迁移丢失调用方,审计 P2 后已恢复,**别再删**。
5. **退出**:关窗/托盘退出 → `gui.rs exit_process`:3s 看门狗先武装 →
   置 APP_SHUTDOWN → 移除托盘 → drop WebView(controller.Close)→
   `TerminateProcess(self)` 终局。**不要用 process::exit/ExitProcess**
   (DLL detach 阶段与 msedgewebview2 子进程同步清理死等 → 1 线程僵尸、
   端口残留、TerminateProcess 杀不死,2026-09-19 现场)。纯服务形态
   Ctrl-C 退出;`spawn_gui_service` 的 ready 通道是一次性握手,**调用方
   超时后必须退进程**(否则后台服务线程占端口),见 web.rs 契约注释。

## JLinkDll 的 Drop 与 close 幂等

`JLinkDll` 已实现 `Drop` 兜底 close + `closed: AtomicBool` 幂等标志
(审计 G-7 后)。新增提前 return 路径不需要手写 close(写了也无妨,
重复调用是 no-op)。device_db 枚举线程的 open() 兜底路径用完显式 close,
不留孤儿 DLL 会话占调试器。

## 并发/锁纪律(web.rs Shared)

- **tick_loop 持泵锁横跨整个消息内循环**,期间只允许:用已借用的 `pump`
  调方法 + 锁 Shared 的其它 Mutex 字段(短借、不嵌套)+ 调 `on_state`
  回调(约定**必须非阻塞**,gui 侧只 `send_event`)。新增分支时先查
  借用链上有没有再借 pump 的方法(历史教训:send_payload 内部借 pump,
  tick 必须 drop(pump) 之后才能调它——定时发送路径就是这么排的)。
- 所有 `lock().unwrap()` 依赖"持锁段内无可 panic 点";release
  `panic="abort"` 下锁中毒即整进程消失,加代码时保持持锁段干净。
- worker 命令通道是尽力而为语义:`tx.send` 失败 `let _` 静默
  (worker 退出瞬间发送/回显/TX 计数可能不一致,已登记接受)。
- 消息通道 mpsc 无界:设备侧 pull 式读取速率有限,长跑无积压风险,不修。

## API/WS 契约同步三件套

改任何 API/WS 行为必须同步三处:`src/web.rs` 及 `src/web/`(含模块头 API 契约注释)、
`ui/web/` 对应前端模块、`tests/web/`(Python 黑盒)。线格式由单测
`ws_event_wire_format_matches_legacy_contract` 逐字节锁定;device 8 字段
键名由 `device_info_json_keys_match_status_contract` 锁定(单一构造点
`device_info_json`)。camelCase 坑:serde `rename_all="camelCase"` 只改
**变体名**,不变体字段,多词字段必须显式 rename(Stats.sessionSec 前科)。
后端选择面(2026-09-28):GET /api/backends、/api/status 的 `backend` 字段、
settings 的 `backend` 键、StoredPrefs 的 `backend` 字段同属契约面。
2026-09-28 已删除前端/测试都不消费的 `/api/status` phase/port 死字段
(旧 port 字段承载过后端 id,与字段名语义相悖)。

**前端注入面纪律(审计 F-2 后)**:设备名/J-Link 产品名等任何来自 DLL
或磁盘缓存(`device_names.txt` 本机可篡改)的字符串,渲染一律 DOM API +
textContent,**禁止 innerHTML 拼接**(即使做了 `"` 转义也不行)。

## 断行/断帧语义(与旧桌面版对齐)

- 块边界断行(worker 每次读一块=设备一次发送);自动断帧在 worker 用真实
  时间戳判定(相邻数据间隔超设定 → FrameEnd),判定放 worker 是因为 UI
  刷新粒度会吞掉 20ms 级设定;行尾模式只决定"在哪断行",行内容一律不含行尾符
- **512 单行兜底覆盖所有成行路径**(审计 P2 后):`enforce_line_cap` 之外
  `absorb_frame_end` 强制成行也先 `cap_pending`——同 tick 内"大块进 pending
  → 帧结束成行"的窗口此前绕过上限
- 已知口径(锁定现状,勿无意识改):自动模式行中 lone CR 保留在内容里
  (前端 pre-wrap 渲染为折行,单测 split_auto_keeps_lone_cr_inside_line 锁定);
  同 tick 内 marks 恒排在数据行之前(~10ms 窗口)
- 改断行行为先跑 `cargo test --lib log_model`

## encoding_rs 的 decode_to_string 不自动扩容(仍然有效)

容量不足静默丢输出:必须先 `max_utf8_buffer_length` 预足容量再
`decode_to_string`(见 rtt.rs `CharsetDecoder::decode`)。

## 偏好持久化:tick 快照比对(500ms)+ 退出不补写

- `StoredPrefs` 全字段 `serde(default)`:旧配置缺新字段/坏 JSON/文件缺失
  一律回落默认;损坏/读取失败在 stderr 留痕(审计 G-9 后)
- J-Link 按序列号恢复(`jlink_serial`,2026-09-28 起连接时随下拉写入并落盘)
- 原子写:tmp → remove 旧 → rename(Windows rename 不覆盖);最后 ≤500ms
  改动在强退时不落盘(知情取舍,gui.rs/web.rs 模块头有说明)
- **冒烟测试铁律(2026-09-28 实踩)**:跑 exe 验证 API 必须带
  `RTT_PREFS_FILE=<临时文件>` 环境变量,否则 connect/settings 会写进用户
  真实的 `%APPDATA%/MiniRttViewer/prefs.json`(实测覆盖了 chip/jlinkSerial,
  靠手工恢复)

## 安全口径

- 默认仅回环监听;`--listen 0.0.0.0` 开放 = 无鉴权全控面(/api/send 任意
  字节注入、reset/power/export 全部暴露)——SECURITY.md 已如实列明攻击面,
  "加 token/口令"是留给用户的决策项,**未经用户点头别擅自加鉴权功能**
- 慢 WS 消费者保护:Lagged 重同步 1 秒内 >3 次即断开(防快照重同步风暴);
  api_mark 标签 200 字符上限;api_settings 负数一律 400(不静默夹 0);
  api_addr 的 UDP 探测放 spawn_blocking(不冻结 tokio worker)

## 构建工程

- **cargo target 目录会锁 exe**:僵尸进程锁死 target 时换
  `CARGO_TARGET_DIR=target2` 绕开;`.gitignore` 已统一 `/target*`
- **build_release.bat**:UPX 压缩;bat 里不能设名为 `UPX` 的环境变量
  (用 `UPXBIN`);bat 文件写英文注释(中文 GBK 乱码)
- **发布**:推 annotated tag `vX.Y.Z` → GitHub Actions 构建+UPX → 自动挂 Release zip
  与 SHA256SUMS;Release 正文使用对应 CHANGELOG 的详细说明。不要随便发版,
  先本地验证、用户确认
- 编译基线:`opt-level = 3 + target-cpu=x86-64-v2`(.cargo/config.toml 随
  仓库生效,含 CI),lto + codegen-units=1 + strip + panic=abort;
  性能优先于体积(RTT 数据路径与 serde/axum 都在 CPU 上);旧 Slint 时代的
  极限体积阶梯实验记录见 git 历史里的本文件
- **Bash 工具里 `&` 启动的 GUI 子进程会随命令结束被回收**;`--no-window`
  模式可在单条命令内 起服务→curl→kill 完成冒烟(注意 kill 在 Git Bash 下
  可能杀不干净,结束后 tasklist 复查)

## 验证工具

- `cargo test --lib`(backend/log_model/web/rtt/config/ansi/gui 纯逻辑)
- `python -m pytest tests/web -q`(完整 HTTP/后端/主题/HEX/持久化/Playwright
  黑盒;服务固定 18099 端口,组间残留进程会挡下一组;数量以当前测试收集为准)。
  先 `cargo build --release --locked --bins --examples`,默认同时定位正式 exe 与
  `target/release/examples/qa_server.exe`;自定义构建分别经 `RTT_TEST_EXE`、
  `RTT_QA_EXE` 指定。QA 进程不加载 J-Link DLL,也不属于正式后端注册表
- 真机冒烟:`mini-rtt-viewer.exe --no-window --port 8777`(必须带临时
  `RTT_PREFS_FILE` 与临时 `APPDATA` 隔离!)+ curl 打 API;连接验证需要真实
  J-Link 与目标板,不得用测试后端冒充真机验收
- `examples/rtt_check.rs`(无界面 RTT 直读)、`examples/emu_check.rs`
  (无界面枚举)

## 已知未修(用户知情,勿"顺手修")

1. 底边空隙/键盘滚动:旧 Slint 版遗留条目,Web 版不适用(浏览器原生滚动)
2. 设备极高速率下偶发合批(虚拟化渲染是独立大工程;Web 版 500 行上限 +
   10ms 泵粒度下同样存在)
3. worker 退出瞬间的发送静默丢失(尽力而为语义,登记接受)
4. 字符集热切换丢弃解码器 pending 半字节(切错时机丢 1 字符;按新编码解释
   旧残留同样错,审计 R3 判"登记不修")
