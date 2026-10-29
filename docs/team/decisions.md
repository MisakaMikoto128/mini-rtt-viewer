# 架构决策记录(ADR)

只保留仍指导当前实现的决策;被替代方案的历史代码可从对应版本 tag 查阅。

## ADR-1 Windows-only 定位

RTT 数据路径依赖 SEGGER `JLink_x64.dll`。不做条件编译的假跨平台,CI 与发布只覆盖
windows-x86_64。(PLAT-1)

## ADR-2 直连 JLink_x64.dll,不用 pylink-square

pylink-square 1.6+ 在本场景验证不可用(RTT 读不稳定);采用 `libloading` + 显式 FFI。
连接序列必须保持:disable_dialog → 枚举选 SN → RTT START → TIF/速度 → Device → Connect;
多探针使用 SelectByUSBSN → SetHostIF → OpenEx。Connect 成功后按设备数据库的 RAM 区域
分块扫描 `SEGGER RTT` 签名并校验缓冲区头部,找到则 SetRTTAddr 后重挂 RTT;找不到保留
J-Link 自动发现。STM32G070 真机证实仅依赖自动发现会连接成功但读不到已有日志。
不得要求用户改装 WinUSB/Zadig 驱动。

## ADR-3 MIT 许可

个人开源工具采用 MIT,不接受 CLA。发布物不包含 SEGGER DLL。

## ADR-5 体积与性能基调

release 使用 opt-level 3 + LTO + strip + panic=abort + 可选 UPX。新增依赖需审查体积和
维护收益;优先使用浏览器原生能力和已有 Rust 生态中的轻量实现。

## ADR-6 偏好持久化:JSON 快照比对

业务偏好存于 `%APPDATA%/MiniRttViewer/prefs.json`,500ms 快照比对后原子写。异常强退
可能丢失最后不超过 500ms 的改动。主题、字号、时间戳、音效等显示偏好由 localStorage 持有。

## ADR-7 数据路径分层:FFI / 纯逻辑 / UI

`jlink_dll.rs` 负责 FFI;`log_model.rs`、`ansi.rs`、`config.rs` 保持可单测;Web、桌面壳
和后端只做装配与协议接线。硬件状态机不能依赖界面生命周期。

## ADR-11 UI 使用内嵌 Web 管理台

Rust 数据层与 worker 保持不变;管理台为 `ui/web/` 原生 HTML/CSS/ES modules,经
`include_str!` 编译期内嵌,axum 提供 HTTP/WS。桌面形态用 tao + wry + tray-icon 承载同一页面,
`--no-window` 提供纯服务。不引入 Tauri、npm、React/Vue 或前端打包步骤。

## ADR-13 正式程序只保留 J-Link,测试数据流独立注入

正式后端注册表仅含 J-Link,不提供 Demo CLI/UI。无硬件 HTTP/WS/Playwright 测试由
`examples/qa_server.rs` 通过 `run_with_backend` 显式注入确定性 QA 后端;该进程不加载
J-Link DLL,也不进入正式产品。保留 `RttBackend` 抽象用于隔离数据层,但新后端必须单独评审
驱动共存和硬件安全影响。发布前仍需使用正式 exe 与真实 J-Link 验证。
