# 贡献指南

感谢改进 Mini RTT Viewer。问题反馈、文档修正、测试和功能代码都欢迎；参与讨论时请遵守
[行为准则](CODE_OF_CONDUCT.md)。

## 反馈问题

- 先搜索已有 issue 和 [常见问题](README.md#常见问题),避免重复。
- Bug 请提供应用版本、Windows 版本、J-Link 软件包版本、目标芯片和最小复现步骤。
- 功能请求请说明实际使用场景,以及现有操作为什么不够。
- **不要上传真实 J-Link 序列号、私有固件日志、本机用户名/路径或凭据。**安全漏洞请按
  [SECURITY.md](SECURITY.md) 私密报告,不要发公开 issue。

## 开发环境

- Windows 10/11 x64,Rust 1.75+;正式数据路径依赖 SEGGER `JLink_x64.dll`。
- Web 黑盒测试另需 Python 3.11+、pytest 和 Playwright Chromium。
- 无硬件开发可运行独立 QA 示例,不需要安装 J-Link:

```bash
cargo build --locked --bins --examples
python -m pip install pytest playwright
python -m playwright install chromium
```

正式程序只注册 J-Link。`examples/qa_server.rs` 只提供确定性测试数据流,不得添加正式
CLI/UI 入口,也不得用 QA 结果冒充真机验收。

## 修改流程

1. Fork 后从 `main` 创建 `feat/<主题>` 或 `fix/<主题>` 分支。
2. 行为变化先更新 `docs/product/spec.md` 的 FR 条目,再实现;测试名对应条目,如
   `test_fr30_...`。修复 bug 时先添加能复现问题的回归测试。
3. 保持现有架构与依赖纪律:前端使用原生 HTML/CSS/ES modules,资源编译期内嵌;
   不引入 npm 或前端打包步骤。新增 Rust 依赖需在 PR 中说明体积与维护收益。
4. 提交前运行完整门禁:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked --bins --examples
python -m pytest tests/web -q
```

5. 提交信息使用约定式提交,例如 `feat(web): ...` / `fix(rtt): ...`,一个提交聚焦一件事。
6. PR 写清改动原因、行为变化和实际执行的验证;涉及 UI 时附截图,涉及连接时说明是否
   做过真实 J-Link 验证。

## 硬件与配置隔离

测试一律使用临时 `APPDATA`、`RTT_PREFS_FILE` 和 `RTT_LOG_DIR`,不得读写开发者真实配置。
真实硬件测试需明确连接的是可复位、可供电的目标板;不要在生产板或正在烧录/调试的设备上
执行复位、供电和发送测试。

## 既定技术约束

- 项目为 **Windows-only**,通过 `libloading` 直连 `JLink_x64.dll`;不要提议换驱动或要求
  用户安装 WinUSB/Zadig。背景见 `docs/team/decisions.md` ADR-1/2。
- RTT START 必须早于 Connect;多 J-Link 的 SelectByUSBSN → SetHostIF → OpenEx 顺序不可改。
- 默认仅监听回环地址,管理台无鉴权;扩大监听或改变安全模型需要单独讨论。
- 发布由维护者执行:更新版本与 CHANGELOG,完整验证后推送 `vX.Y.Z` tag,由 Release
  workflow 构建 zip 和 SHA256SUMS。
