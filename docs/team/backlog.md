# Backlog(唯一事实来源)

状态:`☐ 待做` `◐ 进行中` `◑ 有方案未做` `✔ 完成` `✖ 不做`。完成条目移到文末存档。

## 进行中

- ◐ **UI 技术栈评估**:Slint 开发体验痛点(布局约束反直觉、style-override 需
  hack 内置样式、changed 回调链限制、可视化调试弱)已多轮拖慢迭代。候选方向:
  Tauri + Web 前端(生态最大、SerialHub 同款思路)、egui(纯 Rust 即时模式,调试
  直观)、iced(Elm 架构)。评估结论出 ADR-11 后决策。**未立项重写前,现有 Slint
  代码继续维护,不摆烂。**

## 待做

- ☐ CI 首跑盯日志:fmt/clippy/测试三道门禁在工作流里是第一次真实执行,红了先清
  债务再收紧(不放宽门禁)
- ☐ GitHub Pages 站点上线后,README 顶部挂站点与下载链接
- ☐ 发布产物签名或至少发布 SHA256SUMS(消除 SmartScreen 信任疑虑)
- ☐ AGENTS.md「项目现状速览」与 docs/ 结构对齐(标准化后结构变了)
- ☐ UI 截图刷新:README/docs/images 换 0.1.9 界面(浅色主题、1280+ 宽)

## 存档(✔ / ✖)

- ✔ target 构建目录失控(9 个共 ~45G):已清理;构建统一走默认 target,
  需要"另一份干净构建"时才临时指定 CARGO_TARGET_DIR,用完即删
- ✔ 工程标准化(SerialHub 模板):spec/团队文档/CI 门禁/发布产物契约/站点
- ✖ Linux/macOS 支持:PLAT-1 明确不做
- ✖ 代码签名证书(个人项目成本不成比例,SECURITY.md 已如实说明)
