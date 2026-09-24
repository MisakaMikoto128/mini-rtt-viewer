# Backlog(唯一事实来源)

状态:`☐ 待做` `◐ 进行中` `◑ 有方案未做` `✔ 完成` `✖ 不做`。完成条目移到文末存档。

## 进行中

- ☐ (暂无)

## 待做

- ☐ CI 首跑盯日志:fmt/clippy/测试三道门禁在工作流里是第一次真实执行,红了先清
  债务再收紧(不放宽门禁)
- ☐ 发布产物签名或至少发布 SHA256SUMS(消除 SmartScreen 信任疑虑)
- ☐ AGENTS.md「项目现状速览」与 docs/ 结构对齐(标准化后结构变了)
- ☐ 桌面版遗留 UI 问题(状态栏文本间距/面板控件超界)——浏览器版布局体系
  天然规避;桌面版待 ADR-11 决策后定优先级
- ☐ UI 截图刷新:README/docs/images 换 0.1.9 界面(浅色主题、1280+ 宽)
- ☐ Web 管理台:暂停态拖选 → Ctrl+F 预填为空(QA E2,复现 1 次,未稳定复现)。
  推测机制:暂停打点前后流式行仍在途 → trim/onRowsRemoved 触发 renderRow
  重写行 DOM → selection 被清除;待稳定复现后决定是否在 renderRow 前保护选区
- ☐ Web 管理台:暂停态 reload 按钮文案不同步(服务端暂停中 reload 显示静态
  「暂停」,首点无效需二击恢复)——「按钮文本=状态源」反模式;修法:/api/status
  或 snapshot 带服务端 paused 字段,reload 时按真状态渲染按钮(2026-09-19 QA4 发现)
- ☐ serialhub M 档差距(feature-gap.md 定案,M5 明确不做):
  M1 窗口几何记忆(config.rs window_* 字段已在、壳层未接线,简化版 ~50 行)>
  M2 第二实例唤起(补齐 FR-23 规格债,现二次启动弹错误框,~60 行)>
  M3 托盘菜单连接/断开(~60 行)> M4 时间戳服务端真时刻版(升级 S4 口径,~60 行)
- ☐ J-Link 下拉接线:前端 #jlink 选中值未进入 /api/connect(spawn_worker 恒取
  列表首台),jlink_serial 偏好未用——多台接入时选择不生效(2026-09 语言审计发现)

## 存档(✔ / ✖)

- ✔ 发布 v0.4.1(2026-09-24):版本号 + CHANGELOG + tag;内容:关闭僵尸修复、
  轮子替换、字距/行距对齐 VS Code、S1-S9 新功能、右键复制修复
- ✔ README 顶部挂站点与下载链接(GitHub Pages 已上线)
- ✔ UI 技术栈评估 → ADR-11 定案(2026-09):UI 全面转 Web 形态,Slint 退役
- ✔ UI 技术栈:浏览器管理台 spike 落地 + 真机接入(web.rs 复用同一 worker)
- ✔ target 构建目录失控(9 个共 ~45G):已清理;构建统一走默认 target,
  需要"另一份干净构建"时才临时指定 CARGO_TARGET_DIR,用完即删
- ✔ 工程标准化(SerialHub 模板):spec/团队文档/CI 门禁/发布产物契约/站点
- ✖ Linux/macOS 支持:PLAT-1 明确不做
- ✖ 代码签名证书(个人项目成本不成比例,SECURITY.md 已如实说明)
