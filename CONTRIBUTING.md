# 贡献指南

## 提交 PR

1. Fork → 分支(`feat/xxx` / `fix/xxx`)→ 改动 → `cargo fmt` → `cargo clippy --all-targets -- -D warnings` → `cargo test`
2. 提交信息用约定式提交:`feat(scope): ...` / `fix(scope): ...`,一个提交一件事
3. PR 描述写清:改了什么、为什么、怎么验证的。CI(格式/clippy/测试)必须绿

## 行为需求

改行为先改 `docs/product/spec.md` 的 FR 条目,再改代码;测试名对应 spec 条目
(如 `test_fr5_...`)。反过来只有代码没有 spec 的行为变更,PR 会被要求补。

## 已知约束

- 项目定位 **Windows-only**:RTT 依赖 SEGGER JLinkARM.dll,无跨平台替代方案
- `pylink-square` 1.6+ 已验证不可用于此场景,直连 DLL 是既定决策(见
  `docs/team/decisions.md` ADR-2),PR 请不要提议换回
- 体积敏感:新增依赖需要说明理由,纯重依赖(框架级)基本不会被接受
