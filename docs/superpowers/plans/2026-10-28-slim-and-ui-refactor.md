# Mini RTT Viewer 瘦身与界面整改 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** 删除可再生垃圾，拆分 Rust/Web 复杂度中心，统一状态，并完成可验证的桌面界面整改。

**Architecture:** 保留 `RttBackend`、worker、FFI 与日志模型；将 `web.rs` 改为 facade + 职责子模块，将单页前端改为原生 ES modules。业务偏好由服务端持有，纯显示偏好留在 localStorage；静态文件继续编译期内嵌。

**Tech Stack:** Rust 2021、axum 0.8、tokio、serde、tao/wry、原生 HTML/CSS/ES modules、pytest/Playwright。

**Spec:** `docs/superpowers/specs/2026-10-28-slim-and-ui-refactor-design.md`

## Global Constraints

- Windows 10/11 x64；不引入 npm、前端框架或新运行时依赖。
- 不改变 J-Link DLL 调用顺序、Exited/reconnecting/BUSY 门闩及现有 WS/API 字段语义。
- 不修改 probe-rs 实验分支，不读取或删除真实 `%APPDATA%/MiniRttViewer/prefs.json`。
- 测试服务必须设置临时 `RTT_PREFS_FILE`；每个结构改动后运行格式、单元测试和相关黑盒测试。

---

### Task 1: 清理构建垃圾并恢复工程门禁

**Files:**
- Modify: `.gitignore`
- Modify: `.github/workflows/ci.yml`
- Modify: `src/config.rs:248`
- Modify: `src/backend_jlink.rs`, `src/demo.rs`, `src/web.rs`, `src/main.rs`（仅 rustfmt）
- Delete: 仅未跟踪的 `target*`、`__pycache__`、`.pytest_cache`、临时构建产物

**Interfaces:**
- Produces: 干净工作区与可重复的全量 CI 命令。

- [x] **Step 1: 盘点删除目标和磁盘占用**

运行 `git status --short --ignored`、`git ls-files`，逐目录确认待删路径未被跟踪且可由 Cargo/pytest 重建。保留源码、资产和主题。

- [x] **Step 2: 删除确认可再生的目录**

只删除盘点列出的构建/缓存目录；若 exe 被占用，先确认进程命令行属于本项目测试实例，再停止该实例，不结束用户正在使用的 J-Link 会话。

- [x] **Step 3: 修复格式与 Clippy**

将 `src/config.rs` 测试构造改为：

```rust
let prefs = StoredPrefs {
    backend: "demo".into(),
    ..Default::default()
};
```

运行 `cargo fmt`，随后：

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

预期全部通过。

- [x] **Step 4: CI 运行完整 Web 套件**

将 `.github/workflows/ci.yml` 的测试命令改为：

```yaml
run: python -m pytest tests/web -v
```

本地设置 `RTT_TEST_EXE` 指向当前构建，运行 `python -m pytest tests/web -q`，保留基线结果。

### Task 2: 用回归测试锁定暂停状态与设置原子性

**Files:**
- Modify: `src/web.rs`, `src/log_model.rs`
- Modify: `tests/web/test_spec.py`
- Create: `tests/web/test_settings_atomic.py`

**Interfaces:**
- Consumes: 现有 `/api/status`、`/api/pause`、`/api/settings`。
- Produces: `/api/status.paused: bool`；400 设置响应不修改任何字段。

- [x] **Step 1: 添加暂停 reload 失败测试**

Playwright 暂停后刷新，断言 `#pause-btn` 文本为“继续”，首次点击即恢复，且后续行继续增长。先在旧实现运行并确认失败。

- [x] **Step 2: 添加设置原子性失败测试**

提交同时包含合法 `rxEnding` 和非法负数 `speedIndex` 的请求，断言 HTTP 400，随后 `/api/prefs` 中 `rxEnding` 仍保持原值。先在旧实现确认失败。

- [x] **Step 3: 最小实现**

状态接口增加：

```json
{"paused": true}
```

前端通过独立 `setPaused(paused)` 渲染按钮，不再读取按钮文字推断状态。设置处理改为 `validate_settings(&req) -> Result<ValidatedSettings, Response>`，校验全部通过后统一应用。

- [x] **Step 4: 运行相关测试**

```bash
cargo test --lib web::tests
python -m pytest tests/web/test_spec.py tests/web/test_settings_atomic.py -q
```

### Task 3: 拆分 Rust Web 层但不改变路由契约

**Files:**
- Modify: `src/web.rs`
- Create: `src/web/state.rs`, `src/web/api.rs`, `src/web/pump.rs`, `src/web/ws.rs`, `src/web/theme.rs`
- Test: `src/web.rs` 现有单元测试、完整 `tests/web`

**Interfaces:**
- Preserves: `WebOptions`, `ServiceEvent`, `run`, `spawn_gui_service`, 全部现有路由和 `WsEvent` 线格式。
- Produces: `Shared`  crate-private 状态接口；API/pump/ws 不再共用一个 2000 行实现文件。

- [x] **Step 1: 先移动纯函数与类型**

按“主题 → DTO/状态 → WS → pump → API”顺序做纯移动，保持函数体不变；每移动一组运行 `cargo check` 和线格式单测。

- [x] **Step 2: 收紧可见性**

仅 facade 和跨模块使用项为 `pub(crate)`，其余保持私有；不得复制 `Shared` 字段或新增按后端 id 的 Web 分支。

- [x] **Step 3: 合并序列化样板**

使用统一广播辅助函数序列化 `WsEvent`；API 响应采用 serde DTO，保留 camelCase 与既有逐字节契约测试。

- [x] **Step 4: 全量回归**

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
python -m pytest tests/web -q
```

### Task 4: 前端静态资源与模块拆分

**Files:**
- Modify: `ui/web/index.html`, `src/web.rs` 或 `src/web/api.rs`
- Create: `ui/web/styles.css`, `ui/web/app.js`, `ui/web/api.js`, `ui/web/log-view.js`, `ui/web/search.js`, `ui/web/settings.js`, `ui/web/dialogs.js`
- Test: `tests/web/test_spec.py`, `tests/web/test_theme_plugin.py`

**Interfaces:**
- Produces: `/styles.css` 与 `/app.js` 等固定白名单模块路由，均来自 `include_str!`；`index.html` 以 `<script type="module" src="/app.js">` 启动。

- [x] **Step 1: 先加静态资源路由和 MIME**

保持 `/` 可用，新增 CSS/JS 路由；错误路径返回 404，不接受任意文件路径。

- [x] **Step 2: 纯移动 CSS 和 JS**

先保持行为不变，将全局协调留在 `app.js`，将日志/搜索/设置/弹窗逐步改为显式导入导出。禁止一次同时重写搜索算法。

- [x] **Step 3: 收敛 API 与显示偏好**

提供统一 `request(path, {method, body})`，非 2xx 读取文本并抛错；业务字段不再双写 localStorage，显示字段保留原 key 以兼容现有用户。

- [x] **Step 4: 验证模块和资源契约**

运行完整 Playwright，额外断言所有静态资源 200、页面无 console error、WebSocket 重连和主题插件仍工作。

### Task 5: 重排界面并修复响应式与可访问性

**Files:**
- Modify: `ui/web/index.html`, `ui/web/styles.css`, `ui/web/app.js`, `ui/web/settings.js`
- Modify: `tests/web/test_spec.py`
- Create: `tests/web/test_layout_accessibility.py`

**Interfaces:**
- Consumes: Task 2 的 `paused` 和 Task 4 的模块接口。
- Produces: 连接侧栏、日志工具条、发送区、状态栏四块清晰区域。

- [x] **Step 1: 添加 800px 和键盘回归测试**

在 1440×900、1024×700、800×600 断言：

```python
assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
assert page.evaluate("document.querySelector('#send-opts').scrollWidth <= document.querySelector('#send-opts').clientWidth")
```

用 Tab 到达区分大小写、正则、上一项、下一项和关闭按钮；断言 `:focus-visible` 可见。

- [x] **Step 2: 重排语义 HTML**

日志操作迁到右侧工具条；电源/重置归入设备控制；主题字号归入视图菜单；定时间隔只在 `timer-on` 选中时显示。保留原元素 id，避免无关测试和 JS 大面积改写。

- [x] **Step 3: 实现折叠侧栏与窄窗口布局**

宽屏保留 420px（兼容既有视觉契约）；中屏 320px；窄屏为可开关抽屉。发送选项允许成组换行，底栏按优先级隐藏次级信息，不设置逐字断行。

- [x] **Step 4: 完善 combobox/menu 语义和对比度**

增加 `aria-expanded`、`aria-controls`、`role=listbox/option`、活动项同步；菜单项采用 button 语义。提升四主题 `--muted` 小字对比度并运行现有主题测试。

- [x] **Step 5: 截图审查和全量测试**

保存三档截图人工检查，运行完整 Rust/Web 测试；不得只以 DOM 数值代替视觉检查。

### Task 6: 清理剩余死代码、更新文档并做真机验收

**Files:**
- Modify: `README.md`, `CHANGELOG.md`, `docs/team/backlog.md`
- Modify: `docs/screenshot.png`
- Delete: 仅经引用搜索确认无消费方的代码/缓存

**Interfaces:**
- Produces: 与当前架构、界面和验证命令一致的文档及最终验证记录。

- [x] **Step 1: 删除确认无引用项**

用 Grep/rustc/测试三重确认；不删除仅因“暂时没用到”而猜测的公共后端能力。删除后运行完整门禁。

- [x] **Step 2: 更新文档与截图**

README 补后端模块和当前 CLI，移除过期“零警告”绝对描述；changelog 记录本次结构/UI 变化；backlog 将已完成项移入存档。

- [x] **Step 3: Demo 全链路验证**

当前构建运行 Demo，验证连接/断开、发送、暂停 reload、清空、搜索、主题、800px 布局和 WS 重连。

- [x] **Step 4: J-Link 真机冒烟**

使用隔离偏好文件启动普通形态，枚举并选择用户已连接的 J-Link，连接目标、确认日志接收，发送测试数据并断开。若芯片型号或目标环境不明确，从设备列表和既有偏好读取；不得更改用户真实偏好。记录实际结果与任何硬件侧限制。

- [x] **Step 5: 最终清洁检查**

运行 `git status --short --branch`，确认无新构建垃圾进入差异，汇总删除体积、文件变化、测试数量和真机结果。

## 执行验证记录（2026-10-28）

- `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings` 通过。
- `cargo test --locked`：64 项通过；`python -m pytest tests/web -q`：56 项通过。
- 当前构建的 Demo 已逐项验证日志接收、发送/TX 计数、暂停 reload、清空、
  恢复、搜索高亮、主题切换、800px 无横向溢出、断开与重连；服务重启后的
  WebSocket 自动重连由自动化回归覆盖。
- 真实硬件使用隔离偏好文件验证：枚举到 J-Link V9（SN 59521895），目标
  STM32F030F4，SWD 4000kHz 连接成功；RX 从 1069 增至 1115 字节，发送
  `mini-rtt-viewer smoke` 返回 200、TX 23 字节，断开成功。
- 1440×900、1024×700、800×600 及窄窗口抽屉截图已人工检查；`docs/screenshot.png`
  已更新为当前 1440×900 Demo 界面。
- 删除前测得默认 `target/` 约 438MB；另删除可再生 pytest/pycache 与仓库内
  可选 UPX 工具目录（约 808KB）。最终构建统一放在仓库外的临时 target。
