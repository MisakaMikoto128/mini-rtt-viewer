# 审计进度 · 引擎记忆（PROGRESS）

> 更新规则：每轮 S6 必须更新本文件；上下文被压缩后从这里恢复状态。
> 只允许引擎写本文件与 docs/audit/ 下报告、tests/ 下测试。

- 分支：audit/nova-20260925
- 当前阶段：T2 small4（config+ansi+demo+device_db）深审完成并合并(S5 4/4 通过)→ jlink_dll.rs 第 2 次派发进行中(首次限流失败)→ main.rs+gui.rs+lib.rs 第 3 个 subagent 进行中
- 队列状态：
  - [x] T1 全库架构评审 → docs/audit/2026-09-25-architecture.md（13 条发现：P1×4 / P2×2 / P3×7；行号坏条 12 处已全部就地修正，T4 无需再查）
  - [ ] T2 逐文件深审 → docs/audit/2026-09-25-file-reviews.md（✅web.rs ✅rtt.rs ✅log_model.rs ✅config+ansi+demo+device_db → jlink_dll.rs（第 2 次派发,限流重试）→ main.rs+gui.rs+lib.rs（第 3 个 subagent））
  - [ ] T3 测试生成（顺序：rtt.rs → log_model.rs → web.rs 纯逻辑）
  - [ ] T4 交叉复核轮（循环）
  - [ ] T5 深挖轮（循环）
- 发现统计（累计，含 T2 web.rs + rtt.rs + log_model.rs + small4）：P0:0 P1:8 P2:11 P3:34
  - T1（13 条）：P1×4 / P2×2 / P3×7
  - T2 web.rs（22 条）：P1×3 P2×4 P3×11；单测 15 个无假绿（1 弱断言已登记）
  - T2 rtt.rs（6 条）：P1×1（R1 连接阶段不检测退出）P2×2（R2 返回码丢 / R3 热切换丢 pending 半字节）P3×3；单测 5 条全有效
  - T2 log_model.rs（9 条,坏条 0,抽查 4/4 通过）：P1×0 P2×2（①单测 pump_clear_resets_everything 弱,守不住 pending 残留回归 ②absorb_frame_end 可产出 >512 字符单行,绕过 cap,行为与 AGENTS 口径不符——前端 pre-wrap 无显示损坏）P3×7（FLUSH_MS / ScrollGeom(内含不可达 line_h=0 panic)/ DEFAULT_FRAME_TIMEOUT_MS / 整个 wrap 子系统均死代码——Web 化后前端换行走 CSS / 死代码内 emoji 区段缺口+ZWJ 高估 / split_lines 行中 lone CR 泄漏+混流无单测 / marks 恒先于 data 行(10ms 窗口,桌面版同款)；另单测 30 条过审:26 有效、1 弱、1 半假绿(pause 语义不在本模块)、2 自洽校验）
  - T2 small4 合审（7 条,坏条 0,抽查 4/4 通过）：P1×0 P2×1（device_db `busy()` 门闩零调用方=死代码,原 rtt 线索"枚举线程与 connect 并发 TLS 损坏"据此降级——真实窗口需 缓存未命中+DLL 需 Open 兜底+多设备 三条件叠加）P3×6（config×4:RTT_PREFS_FILE 无校验且该分支恰无单测、load/save 静默降级、rename 失败残留 tmp+tmp 无 pid 熵、模块头注释指向 gui.rs 与 web 形态矛盾；ansi×1:分号式 SGR RGB >255 时 `as u8` 回绕；device_db×2:XSS 信任边界(缓存文件可被同用户预置)、`open()` 兜底路径孤儿 DLL 会话不 close+BUSY 置位晚于 load）；demo.rs 未见异常；单测:config 有效(5)、ansi 有效(8)、demo 无(设计形态)、device_db 弱(仅 normalize 1 测)；`cargo test --lib` 67 全绿
  - P1 清单（累计 8）：T1①无鉴权+--listen 开放监听 ②innerHTML 注入 ③J-Link 下拉 sn 未接入 ④reconnect 1s 窗口双 worker；T2 web⑤State 广播未清洗 status ⑥open-browser 2MB 体限+死接口 ⑦服务线程无 cancel 句柄；T2 rtt⑧连接阶段不检测退出,看门狗强杀未清理 DLL 会话
- 阻塞清单：（空）
- 待批准事项（依赖等）：（空）
- 已完成轮数：5
- 备注：
  - T1 报告 G-1..G-10 为续队规则的燃料（T4 两轮零新发现时展开成深审任务,架构报告 253-262 行表格已核实在位）
  - T2 已完成 web.rs（1995 行）；index.html 1352 行（FR-27/FR-28 已并入）
  - jlink_dll.rs subagent 失败 1 次（provider 限流,运行 2100s 无产出）,第 2 次派发按纪律#5 限流重试；main/gui/lib 为第 3 个并行 subagent
  - file-reviews.md 合并技巧教训:heredoc 含 `EOF` 风险,用 Edit 追加大段内容
