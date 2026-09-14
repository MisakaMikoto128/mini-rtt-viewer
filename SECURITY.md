# 安全策略

## 报告漏洞

不要在公开 issue 里描述可被利用的细节。用 GitHub 的
[Privately report a security vulnerability](https://github.com/MisakaMikoto128/mini-rtt-viewer/security/advisories/new)
私密报告,或通过仓库主页的个人联系方式联系维护者。72 小时内确认,修复节奏视严重程度。

## 当前状态(如实说明)

- 本地工具,不监听任何网络端口,无遥测,无自动更新
- 通过 `libloading` 加载 SEGGER 官方 `JLinkARM.dll`——DLL 本身的安全性由 SEGGER
  分发渠道保证,本项目不修改、不捆绑该 DLL
- 用户偏好与设备库缓存写入 `%APPDATA%/MiniRttViewer/`,不含敏感凭据
- 发布产物未做代码签名(SmartScreen 会提示未知发布者),校验方式以 GitHub
  Release 页面的构建日志为准
