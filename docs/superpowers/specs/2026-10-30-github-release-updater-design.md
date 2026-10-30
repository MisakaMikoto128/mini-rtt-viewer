# GitHub Release 更新系统设计

日期：2026-10-30
状态：已获用户确认，待实施

## 目标

为 Mini RTT Viewer 增加 Windows 单 exe 更新能力，复用当前 GitHub tag、Release zip 与
`SHA256SUMS` 发布契约。启动时低频检查新版本，但下载、安装和重启均由用户明确触发。
不改变 J-Link 数据通路，不引入 Tauri、cargo-dist 或独立更新服务器。

## 方案选择

采用 Rust `self_update` 的 GitHub Releases 后端：

- `uv` 使用的 `axoupdater` 依赖 cargo-dist 安装 receipt，不适合当前手工解压 zip 的安装方式。
- Tauri updater 与 Zed updater 的签名、清单和桌面壳集成不能直接用于当前 tao + wry 应用。
- `self_update` 支持 Release 资产选择、zip 内 exe 路径、Windows 原地替换，以及从 Release
  的 `SHA256SUMS` 解析并校验摘要，与现有发布产物一致。

参考实现与文档：

- https://github.com/jaemk/self_update
- https://github.com/axodotdev/axoupdater
- https://github.com/astral-sh/uv/blob/main/crates/uv/src/commands/self_update.rs
- https://v2.tauri.app/plugin/updater/
- https://github.com/zed-industries/zed/blob/main/crates/auto_update/src/auto_update.rs

## 更新流程

1. 服务启动后执行一次后台检查；距上次自动检查不足 24 小时则跳过。
2. 从 `MisakaMikoto128/mini-rtt-viewer` 的正式 GitHub Release 获取最新版本，以 semver
   与 `CARGO_PKG_VERSION` 比较。草稿、预发布和不高于当前版本的 Release 不可安装。
3. 有新版时只更新界面状态，不下载资产。手动检查忽略 24 小时节流。
4. 用户点击“下载并安装”后，服务端重新确定待安装 Release，选择且只选择
   `mini-rtt-viewer-<版本>-windows-x86_64.zip`。
5. 下载必须有界超时；zip 必须匹配同 Release 的 `SHA256SUMS`，并包含
   `mini-rtt-viewer-<版本>-windows-x86_64/mini-rtt-viewer.exe`。
6. 校验和解包成功后由 `self_update`/`self_replace` 替换当前 exe。失败不得报告已安装。
7. 桌面版显示“重启完成更新”；用户再次确认后才启动新进程并退出旧进程。
   `--no-window` 模式只报告“请手动重启”，不自动终止服务。

所有仓库、目标平台和资产规则均由服务端固定。API 不接受 URL、仓库名、任意版本或 exe
路径，避免把更新接口变成任意下载或文件替换接口。

## 模块边界

### `src/update.rs`

拥有 Release 配置、版本与资产选择、更新状态机、下载/校验/安装以及自动检查节流。
同步网络与文件操作只通过 `tokio::task::spawn_blocking` 接入异步服务，不阻塞 tokio worker。
状态转换串行化：检查与安装不可重入，重复安装返回 409。

### Web 接缝

新增以下本机管理接口：

- `GET /api/update/status`：当前版本、检查结果、更新阶段、可用版本和错误信息。
- `POST /api/update/check`：立即检查，不下载。
- `POST /api/update/install`：安装已经确认的新版，无请求参数。
- `POST /api/update/restart`：仅在安装成功且为桌面模式时请求重启。

检查和状态读取可由远程页面使用；安装和重启必须根据请求对端地址限制为回环地址。
连接目标设备期间安装返回 409，防止更新重启打断活动 RTT 会话。接口状态不能由前端
单方面伪造，下载、校验或替换失败均保留错误状态并允许重试。

### 桌面壳

新增 `ServiceEvent::Restart` 与 `UserEvent::Restart`。GUI 主线程收到后先启动新 exe，
新进程通过内部等待参数等待旧 PID 退出，再绑定同一端口；旧进程继续沿用现有
`APP_SHUTDOWN → 移除托盘 → drop WebView → TerminateProcess(self)` 退出汇点。
启动新进程失败时旧应用保持运行并报告错误，不使用新的 `process::exit` 路径。

### 前端

在现有“关于”对话框增加检查按钮、状态文本、安装按钮、重启按钮和 Release 链接。
启动检查只产生非阻塞提示，不弹强制对话框、不自动安装。按钮随服务端阶段禁用，
网络错误通过现有 toast 和对话框状态呈现。

### 偏好

现有 `StoredPrefs` 仅增加自动检查时间戳。每次自动检查发起时记录时间，避免 GitHub
不可用时每次启动重复请求；手动检查不修改或绕过该限制之外的任何业务偏好。

## 安全与完整性边界

- Release zip 必须通过 SHA-256 校验后才允许解包和替换。
- 校验失败、资产缺失、zip 路径不符、网络错误和并发操作均不能改变当前 exe。
- 默认监听行为不变，不新增认证，不扩大监听范围。
- `SHA256SUMS` 与 zip 同处一个 GitHub Release，可验证完整性和资产匹配，但不能在
  GitHub 仓库本身被攻破时提供独立真实性保证。本次不擅自引入签名密钥或更改发布凭据。
- 不更新设备固件，不执行 J-Link reset/power/send；安装门禁要求先正常断开目标。

## 验证

采用测试先行：

- 单元测试覆盖 semver 比较、预发布/降级拒绝、唯一资产选择、内部 zip 路径、状态转换、
  24 小时节流和重复请求。
- 使用本地伪 GitHub API/资产服务验证 checksum 成功、checksum 错误、缺失 sums、缺失 exe
  和中断下载；更新目标始终是临时目录中的复制 exe，绝不覆盖开发或发布目录的正式 exe。
- HTTP 黑盒覆盖断开门禁、非回环安装/重启拒绝、409 并发语义及失败无副作用。
- 浏览器测试覆盖自动检查、手动检查、下载、失败重试、重启状态和 Release 链接。
- 对真实 v0.5.2 Release 只做只读元数据与资产兼容性检查，不触发当前安装实例更新。
- 最后运行完整 Rust、Web/Playwright、fmt、Clippy 与 Release 构建。

## 非目标

- 不迁移到 cargo-dist 或安装器 receipt。
- 不做自动下载、静默安装、强制重启或降级。
- 不支持 macOS/Linux 更新资产，不更改现有跨平台源码的构建方式。
- 不增加签名密钥管理、自动版本发布或更新服务器。
