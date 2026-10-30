# 安全策略

## 报告漏洞

不要在公开 issue 里描述可被利用的细节。用 GitHub 的
[Privately report a security vulnerability](https://github.com/MisakaMikoto128/mini-rtt-viewer/security/advisories/new)
私密报告,或通过仓库主页的个人联系方式联系维护者。72 小时内确认,修复节奏视严重程度。

## 当前状态(如实说明)

- 本地工具,管理台服务默认仅绑定 127.0.0.1 回环(默认端口 8686),无遥测;
  启动时按 24 小时间隔检查固定官方 GitHub 仓库的 Release,手动检查也可访问该仓库。
  下载与安装必须由用户显式触发;`--listen 0.0.0.0` 可显式开放局域网/互联网访问——
  管理台无鉴权,仅建议在可信网络使用
- **开放监听的具体攻击面**(开放前请确认可接受):管理台无鉴权,能打开页面的
  局域网内任何设备**等价于拿到调试器与目标板的完全控制权**,包括但不限于:
  - `POST /api/send` / `/api/timer` —— 向目标芯片**注入任意字节**(固件协议
    灌数、触发固件命令),可写 RTT 下行通道;
  - `POST /api/reset` / `/api/power` —— 复位目标 MCU、开关 J-Link 对目标板的供电;
  - `POST /api/connect` / `/api/settings` —— 重建/改写调试会话与全部运行参数;
  - WS `/ws` 与 `GET /api/export` —— **持续读取全部 RTT 日志数据**(可能含
    固件日志中泄露的密钥/地址等敏感信息)并导出落盘;
  - HTTP 层无来源校验、无连接数上限;WS 有慢消费者断连保护,但带宽不设限。
- 通过 `libloading` 加载 SEGGER 官方驱动 DLL(`JLink_x64.dll`,SEGGER 安装包自带)
  ——DLL 本身的安全性由 SEGGER 分发渠道保证,本项目不修改、不捆绑该 DLL
- 用户偏好、设备库缓存与导出日志写入 `%APPDATA%/MiniRttViewer/`;偏好不含敏感凭据,
  但 RTT 日志可能包含固件输出的敏感信息。导出文件仅由当前本机用户目录权限保护,
  分享日志前必须脱敏。`device_names.txt` 为本机同权限进程可写文件,
  其内容会显示进管理台(前端以 textContent 渲染,无脚本执行面)
- 发布产物未做代码签名(SmartScreen 会提示未知发布者);每个 Release 附带 CI 生成的
  `SHA256SUMS`,下载后应核对 zip 校验值,并只从本仓库 Releases 获取发布包

## 更新完整性与本机边界

- 更新器只访问固定官方仓库,只选择 `mini-rtt-viewer-<版本>-windows-x86_64.zip`
  及包内约定 exe 路径;更新 API 不接受调用方提供的 URL、仓库或安装路径。
  下载后必须通过**同一个 GitHub Release** 的 `SHA256SUMS` SHA-256 校验才会解包替换。
- 同 Release 校验和能验证 zip 的完整性及资产匹配,不能提供独立的真实性保证:
  如果仓库、Release 或发布凭据被攻破,攻击者可能同时替换 zip 和校验和。
  当前发布包也没有代码签名,不能把 SHA-256 描述成发布者身份认证。
- `GET /api/update/status` 与 `POST /api/update/check` 可由能访问管理台的客户端使用;
  `POST /api/update/install` 与 `/api/update/restart` 只接受请求对端为回环地址的调用,
  非回环请求返回 403。安装另要求目标已断开且状态允许;纯服务不接受 API 重启。
- 回环校验只证明直接对端地址是本机,不是用户或进程认证,不能区分本机上的调用者,
  也不给其他开放监听下的无鉴权接口增加保护。需要更强边界时不要开放监听,
  并限制本机不可信进程与页面访问管理台。
