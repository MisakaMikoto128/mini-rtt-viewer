# 安全策略

## 报告漏洞

不要在公开 issue 里描述可被利用的细节。用 GitHub 的
[Privately report a security vulnerability](https://github.com/MisakaMikoto128/mini-rtt-viewer/security/advisories/new)
私密报告,或通过仓库主页的个人联系方式联系维护者。72 小时内确认,修复节奏视严重程度。

## 当前状态(如实说明)

- 本地工具,管理台服务默认仅绑定 127.0.0.1 回环(默认端口 8686),无遥测,无自动更新;
  `--listen 0.0.0.0` 可显式开放局域网/互联网访问——管理台无鉴权,仅建议在可信网络使用
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
- 用户偏好与设备库缓存写入 `%APPDATA%/MiniRttViewer/`,不含敏感凭据;
  `%APPDATA%/MiniRttViewer/device_names.txt` 为本机同权限进程可写文件,
  其内容会显示进管理台(前端以 textContent 渲染,无脚本执行面)
- 发布产物未做代码签名(SmartScreen 会提示未知发布者),校验方式以 GitHub
  Release 页面的构建日志为准
