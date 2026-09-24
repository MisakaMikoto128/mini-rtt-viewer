# 决策记录(ADR)

一个决策一条,新决策追加;被推翻的保留原文并注明后继 ADR。

## ADR-1 Windows-only 定位

RTT 数据路径依赖 SEGGER `JLinkARM.dll`(闭源,无跨平台替代)。不做条件编译的
假跨平台,CI 与发布只覆盖 windows-x86_64。(PLAT-1)

## ADR-2 直连 JLinkARM.dll,不用 pylink-square

pylink-square 1.6+ 在本场景验证不可用(RTT 读不稳定);`libloading` + 显式 FFI
签名,连接序列按 DLL 状态机要求固定(disable_dialog → 枚举选 SN → RTT START →
TIF/速度 → Device → Connect)。换库类提议一律引用本条。

## ADR-3 MIT 许可

个人开源工具,MIT 足够;不接受 CLA。

## ADR-4 Slint 作为 UI 技术栈(2026-08)

选型时看中:纯 Rust、声明式 .slint、单二进制、体积可控(femtovg 渲染)。
**2026-09 起开发体验问题累积**(布局约束反直觉、样式覆盖要 hack 内置文件、
changed 回调链限制、可视化调试弱),评估迁移中——见 backlog「UI 技术栈评估」,
结论将出 ADR-11。迁移立项前 Slint 代码继续维护。

## ADR-5 体积与性能基调

release:opt-level 3 + LTO + strip + panic=abort + UPX(容错);依赖引入需过
体积审查(regex-lite 替代 regex、零依赖 Win32 FFI 替代 chrono/clipboard crate)。

## ADR-6 偏好持久化:JSON 快照比对

%APPDATA%/MiniRttViewer/prefs.json;tick 内 500ms 快照比对,变化才原子写,
退出强制补写。不引入 notify/watch 类依赖。

## ADR-7 数据路径分层:FFI / 纯逻辑 / UI 三层

`jlink_dll.rs`(FFI)与 `log_model.rs`/`ansi.rs`/`config.rs`(纯逻辑,可单测)
不依赖 Slint 类型;UI(main.rs 装配层)只做接线。回归测试建立在纯逻辑层。

## ADR-8 换行列数单一真源

LogView.columns 实时绑定,tick 开头无节流同步给 pump;列宽计算收敛
`char_width_cols`(emoji 3 列)。禁绝"多来源各自节流"——历史教训见 AGENTS.md。

## ADR-9 demo 模式是一等公民

`--demo-log` 无设备仿真(数据流 + 断连重连 + 假命令消费者):无真板也能跑端到端,
QA/UX/CI 冒烟全部基于它。真机验证仅在发版前由人工执行。

## ADR-10 标准化模板采 SerialHub

仓库结构、spec 编号、团队回路、CI 门禁纪律、发布产物契约照 SerialHub
(WorkPlace/serialhub)裁剪;差异点:本项目 Windows-only(CI 单平台)、无内嵌
Web 管理台(桌面 GUI)。
**2026-09 更新:差异点中的「无内嵌 Web 管理台」已被 ADR-11 推翻**,管理台已内嵌。

## ADR-11 UI 技术栈迁移(2026-09 定案,补记)

ui-stack-eval.md 评估后定案:**UI 全面转 Web 形态**——Rust 数据层与 worker 不动,
管理台为内嵌单页(`ui/web/index.html`,axum 服务,编译期内嵌);桌面形态采用
tao + wry WebView 内嵌同一管理台(SerialHub 同构),不引入 Tauri/egui/iced;
`--no-window` 纯服务为第二形态。0.3.0 起 Slint 退役(ADR-4 的迁移悬案就此关闭)。
落地证据:`src/web.rs` / `src/gui.rs`、CHANGELOG 0.3.0。
