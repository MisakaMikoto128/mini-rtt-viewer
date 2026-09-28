//! 日志消息泵:worker 消息 → 按接收行尾模式断行 → ANSI 解析 → 带色行。
//!
//! 与 UI 完全解耦(纯逻辑,可单测);web.rs 的数据泵线程 `tick_loop` 每个
//! 10ms 周期:
//! 1. 逐条取 worker 消息,调用 [`LogPump::absorb_text`] / [`LogPump::absorb_frame_end`]
//! 2. 调 [`LogPump::enforce_line_cap`] 兜底超长行
//! 3. 调 [`LogPump::take_new_rows`] 拿**增量**新行,经 WS rows 事件推给前端
//!    (前端 #log 逐行 append,超 [`MAX_LOG_ROWS`] 由前端从头部裁剪 DOM 行)

use crate::ansi::{AnsiLines, Run};

/// UI 刷新周期(数据泵单点真源:web.rs tick_loop 的 sleep 取本常量)。过大
/// (如 200ms)会把均匀到达的多条消息攒成一批同时冒出来,视觉上"两条一次"。
/// 10ms 粒度下每 tick 通常 0~1 条,显示节奏与设备发送节奏一致。空 tick 只有
/// 一次 try_recv 的开销,可忽略。
pub const FLUSH_MS: u64 = 10;
/// 断帧间隔解析失败时的默认值(ms;web.rs 恢复偏好与 config.rs 默认值共用)
pub const DEFAULT_FRAME_TIMEOUT_MS: u32 = 20;
/// 单行硬上限兜底(超长帧/关闭断行时的无换行流)
pub const MAX_LINE_CHARS: usize = 512;
/// 日志行数上限(超限丢最旧行;经 stats.cap 事件下发给前端,前端裁剪 DOM 行
/// 与服务端同一真源)
pub const MAX_LOG_ROWS: usize = 500;

/// 按接收行尾模式切行:0=自动(\n 断行、吞 \r) 1=CRLF 2=LF 3=CR 4=无(不断行)。
/// 切出的行内容**一律不含行尾符**——行尾模式只决定"在哪断行",不改变内容。
/// 已知口径(审计登记):自动模式行中的 lone CR(非行尾)保留在内容里,
/// 前端 pre-wrap 会把它渲染成换行;混流场景见单测。
fn split_lines(p: &mut String, rx_ending: i32, out: &mut Vec<String>) {
    let pat: &str = match rx_ending {
        1 => "\r\n",
        2 => "\n",
        3 => "\r",
        4 => return,
        _ => "\n",
    };
    while let Some(pos) = p.find(pat) {
        let line: String = p.drain(..pos + pat.len()).collect();
        out.push(line.trim_end_matches(['\r', '\n']).to_string());
    }
}

/// 日志泵状态(web.rs 数据泵线程与 API handlers 共享,经 Mutex)
#[derive(Default)]
pub struct LogPump {
    /// 半行缓冲:等待换行符/帧结束/兜底上限的未完成行
    pending: String,
    /// 已切出的完整行(含 ANSI 转义原样)
    new_lines: Vec<String>,
    /// 展示用带色行(全量,超出 [`MAX_LOG_ROWS`] 丢最旧)
    rows: Vec<Vec<Run>>,
    /// 固定色整行(会话标记/发送回显):与数据行同队列上屏、同上限裁剪
    marks: Vec<Vec<Run>>,
    /// 因行数上限被丢弃的行数(UI 侧行模型需同步移除同样数量)
    dropped: usize,
    /// ANSI 解析器(持久实例:颜色状态跨行跨块保持)
    ansi: AnsiLines,
    /// 暂停接收:置位后新数据直接丢弃(不进日志、不占缓冲)
    pub paused: bool,
}

impl LogPump {
    /// 消化一段文本数据(Log 横幅 / RTT 读块),按接收行尾模式切出完整行
    pub fn absorb_text(&mut self, text: &str, rx_ending: i32) {
        self.pending.push_str(text);
        split_lines(&mut self.pending, rx_ending, &mut self.new_lines);
    }

    /// worker 判定一帧结束(相邻数据间隔超过断帧超时):缓冲整体切为一行。
    /// 强制成行的整段同样受 [`MAX_LINE_CHARS`] 兜底(同一 tick 内先收大块再
    /// 帧结束的窗口里,enforce_line_cap 尚未跑过,必须在这里切,审计 P2)
    pub fn absorb_frame_end(&mut self, rx_ending: i32) {
        if self.pending.is_empty() {
            return;
        }
        split_lines(&mut self.pending, rx_ending, &mut self.new_lines);
        self.cap_pending();
        if !self.pending.is_empty() {
            self.new_lines.push(std::mem::take(&mut self.pending));
        }
    }

    /// 单行长度兜底:超长帧/无换行流按 [`MAX_LINE_CHARS`] 硬切(前 512 字符
    /// 成行,tail 留作新 pending;按字符切,多字节边界天然安全)
    pub fn enforce_line_cap(&mut self) {
        self.cap_pending();
    }

    fn cap_pending(&mut self) {
        if self.pending.chars().count() > MAX_LINE_CHARS {
            let mut chars: Vec<char> = self.pending.chars().collect();
            let tail: String = chars.split_off(MAX_LINE_CHARS).into_iter().collect();
            // 行内容 = 前 MAX_LINE_CHARS 个字符;tail 是新的 pending。
            // (不能把 replace 出来的整个原串当行——尾部会随下一行重复输出)
            self.new_lines.push(chars.into_iter().collect());
            self.pending = tail;
        }
    }

    /// 插入一条固定颜色的整行(会话标记/发送回显)。绕过 ANSI 解析——
    /// 不污染设备流的颜色状态;与数据行同队列,由下一次 [`Self::take_new_rows`]
    /// 增量上屏,同样受 [`MAX_LOG_ROWS`] 裁剪。暂停接收不影响标记(标记是
    /// 用户/应用动作,不是设备数据)。
    pub fn push_colored_line(&mut self, text: &str, fg: (u8, u8, u8)) {
        self.marks.push(vec![Run {
            text: text.to_string(),
            fg: Some(fg),
        }]);
    }

    /// 把已切出的行经 ANSI 解析转为带色行;有新行返回它们(增量上屏)。
    /// 超出 [`MAX_LOG_ROWS`] 时丢最旧行,返回值仍只含**本次新增**的行。
    pub fn take_new_rows(&mut self) -> Option<Vec<Vec<Run>>> {
        if self.new_lines.is_empty() && self.marks.is_empty() {
            return None;
        }
        let mut fresh: Vec<Vec<Run>> = std::mem::take(&mut self.marks);
        for l in self.new_lines.drain(..) {
            fresh.push(self.ansi.feed_line(&l));
        }
        self.rows.extend(fresh.iter().cloned());
        if self.rows.len() > MAX_LOG_ROWS {
            let drop = self.rows.len() - MAX_LOG_ROWS;
            self.rows.drain(..drop);
            self.dropped += drop;
        }
        Some(fresh)
    }

    /// 取走"因行数上限被丢弃"的行数(UI 侧行模型需从头部移除同样数量,
    /// 否则行模型无限增长)。每次调用后归零。
    pub fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.dropped)
    }

    /// 全量行快照(重同步/导出用)
    pub fn snapshot_rows(&self) -> Vec<Vec<Run>> {
        self.rows.clone()
    }

    /// 当前保留行数(状态栏/只读统计用)
    pub fn rows_len(&self) -> usize {
        self.rows.len()
    }

    /// 测试专用:内部保留行数(裁剪断言用;仅 lib 测试编译)
    #[cfg(test)]
    pub fn test_rows_len(&self) -> usize {
        self.rows.len()
    }

    /// "清空"按钮:缓冲、行、解析器颜色状态全部丢弃
    pub fn clear(&mut self) {
        self.pending.clear();
        self.new_lines.clear();
        self.rows.clear();
        self.marks.clear();
        self.dropped = 0;
        self.ansi.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(p: &str, ending: i32) -> (String, Vec<String>) {
        let mut buf = p.to_string();
        let mut out = Vec::new();
        split_lines(&mut buf, ending, &mut out);
        assert!(out.iter().all(|l| !l.contains('\r') && !l.contains('\n')));
        (buf, out)
    }

    #[test]
    fn split_auto_breaks_on_lf_and_eats_cr() {
        let (rest, out) = lines("a\r\nb\nc", 0);
        assert_eq!(out, vec!["a", "b"]);
        assert_eq!(rest, "c");
    }

    #[test]
    fn split_crlf_mode() {
        let (rest, out) = lines("a\r\nb\r\nc", 1);
        assert_eq!(out, vec!["a", "b"]);
        assert_eq!(rest, "c");
    }

    #[test]
    fn split_lf_mode_trims_cr() {
        let (rest, out) = lines("a\r\nb\n", 2);
        assert_eq!(out, vec!["a", "b"]);
        assert_eq!(rest, "");
    }

    #[test]
    fn split_cr_mode() {
        let (rest, out) = lines("a\rb\rc", 3);
        assert_eq!(out, vec!["a", "b"]);
        assert_eq!(rest, "c");
    }

    #[test]
    fn split_none_keeps_buffer() {
        let (rest, out) = lines("a\r\nb", 4);
        assert!(out.is_empty());
        assert_eq!(rest, "a\r\nb");
    }

    #[test]
    fn pump_accumulates_blocks_into_rows() {
        let mut pump = LogPump::default();
        pump.absorb_text("Hel", 0);
        assert!(pump.take_new_rows().is_none()); // 未成行不上屏
        pump.absorb_text("lo\r\nWorld", 0);
        // "Hello" 已成行上屏;"World" 还没有换行符,留在缓冲
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].text, "Hello");
        assert!(pump.take_new_rows().is_none());
        pump.absorb_frame_end(0);
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].text, "World");
    }

    #[test]
    fn pump_frame_end_flushes_partial_line() {
        let mut pump = LogPump::default();
        pump.absorb_text("no-newline-tail", 0);
        pump.absorb_frame_end(0);
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows[0][0].text, "no-newline-tail");
    }

    #[test]
    fn pump_line_cap_splits_overlong_stream() {
        let mut pump = LogPump::default();
        pump.absorb_text(&"x".repeat(MAX_LINE_CHARS + 10), 4);
        pump.enforce_line_cap();
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].text.chars().count(), MAX_LINE_CHARS);
        // 剩余 10 字符留在 pending
        pump.absorb_frame_end(0);
        let rows = pump.take_new_rows().unwrap();
        let joined: String = rows
            .iter()
            .flat_map(|r| r.iter().map(|s| s.text.clone()))
            .collect();
        assert!(joined.ends_with("xxxxxxxxxx"));
    }

    #[test]
    fn pump_trims_to_max_rows_dropping_oldest() {
        let mut pump = LogPump::default();
        for i in 0..(MAX_LOG_ROWS + 5) {
            pump.absorb_text(&format!("line{i}\n"), 0);
            pump.enforce_line_cap();
        }
        assert!(pump.take_new_rows().is_some());
        assert_eq!(pump.test_rows_len(), MAX_LOG_ROWS);
        // 最旧的 line0..line4 被挤出窗口;fresh 行包含全部输入
        let mut pump2 = LogPump::default();
        for i in 0..(MAX_LOG_ROWS + 5) {
            pump2.absorb_text(&format!("line{i}\n"), 0);
            pump2.enforce_line_cap();
        }
        let rows = pump2.take_new_rows().unwrap();
        assert_eq!(rows.first().unwrap()[0].text, "line0");
        let last = rows.last().unwrap().last().unwrap();
        assert_eq!(last.text, format!("line{}", MAX_LOG_ROWS + 4));
    }

    #[test]
    fn pump_preserves_ansi_escapes_for_parser() {
        let mut pump = LogPump::default();
        pump.absorb_text("\x1b[32mOK\r\n", 0);
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows[0][0].text, "OK");
        assert_eq!(rows[0][0].fg, Some((0x0d, 0xbc, 0x79)));
    }

    #[test]
    fn pump_color_state_carries_between_rows() {
        let mut pump = LogPump::default();
        pump.absorb_text("\x1b[31mred\nstill red\n", 0);
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows[0][0].fg, Some((0xcd, 0x31, 0x31)));
        assert_eq!(rows[1][0].fg, Some((0xcd, 0x31, 0x31)));
    }

    #[test]
    fn pump_take_dropped_tracks_trimmed_rows() {
        let mut pump = LogPump::default();
        for i in 0..(MAX_LOG_ROWS + 5) {
            pump.absorb_text(&format!("line{i}\n"), 0);
            pump.enforce_line_cap();
        }
        assert!(pump.take_new_rows().is_some());
        assert_eq!(pump.take_dropped(), 5);
        assert_eq!(pump.take_dropped(), 0); // 取一次即归零
        pump.absorb_text("more\n", 0);
        assert!(pump.take_new_rows().is_some());
        assert_eq!(pump.take_dropped(), 1); // 每加一行挤掉一行
    }

    #[test]
    fn pump_clear_resets_everything() {
        // 弱断言加固(审计 P2):clear 前必须留有 pending 半行,否则删掉
        // clear() 里的 pending/new_lines 清理也能通过
        let mut pump = LogPump::default();
        pump.absorb_text("a\r\n", 0);
        assert!(pump.take_new_rows().is_some());
        pump.absorb_text("partial", 0); // 无行尾:留在 pending
        pump.absorb_text("\x1b[31mcolored", 0); // ANSI 颜色状态也应随 clear 重置
        pump.clear();
        assert!(pump.take_new_rows().is_none());
        // pending 残留回归守护:clear 后的新数据不得与清空前的半行拼接
        pump.absorb_text("more\n", 0);
        pump.enforce_line_cap();
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].text, "more");
    }

    #[test]
    fn push_colored_line_works_and_clear_wipes_marks() {
        // 审计 P3:LogPump 无 pause 语义(暂停判断在 web.rs 泵线程),原测试名
        // 夸大了覆盖面;改名只锁本模块事实:push 可取、clear 清空
        let mut pump = LogPump::default();
        pump.push_colored_line("mark", (1, 2, 3));
        assert!(pump.take_new_rows().is_some());
        pump.clear();
        assert!(pump.take_new_rows().is_none());
    }

    #[test]
    fn push_colored_line_flows_through_take_new_rows() {
        let mut pump = LogPump::default();
        pump.push_colored_line("── 标记 ──", (0x28, 0xaf, 0xe9));
        pump.push_colored_line("» echo", (0x77, 0x77, 0x88));
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].text, "── 标记 ──");
        assert_eq!(rows[0][0].fg, Some((0x28, 0xaf, 0xe9)));
        assert_eq!(rows[1][0].fg, Some((0x77, 0x77, 0x88)));
        assert!(pump.take_new_rows().is_none());
    }

    #[test]
    fn marks_merge_with_data_rows_in_order() {
        let mut pump = LogPump::default();
        pump.push_colored_line("mark", (1, 2, 3));
        pump.absorb_text("data\n", 0);
        pump.enforce_line_cap();
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].text, "mark");
        assert_eq!(rows[1][0].text, "data");
    }

    #[test]
    fn marks_count_toward_row_cap() {
        let mut pump = LogPump::default();
        for _ in 0..(MAX_LOG_ROWS + 3) {
            pump.push_colored_line("m", (1, 2, 3));
        }
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.len(), MAX_LOG_ROWS + 3); // 新增全部返回
        assert_eq!(pump.test_rows_len(), MAX_LOG_ROWS); // 保留窗口裁掉最旧
        assert_eq!(pump.take_dropped(), 3);
    }

    fn joined(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.clone()).collect()
    }

    #[test]
    fn frame_end_line_respects_max_chars() {
        // 同一 tick 内「大块进 pending → 帧结束强制成行」的窗口里
        // enforce_line_cap 还没跑过:成行也必须受 512 兜底
        // (审计 P2:此前整段推入绕过单行上限)
        let mut pump = LogPump::default();
        pump.absorb_text(&"y".repeat(MAX_LINE_CHARS + 7), 4); // 行尾=无,全进 pending
        pump.absorb_frame_end(4);
        let rows = pump.take_new_rows().unwrap();
        assert!(rows
            .iter()
            .all(|r| joined(r).chars().count() <= MAX_LINE_CHARS));
        assert_eq!(rows[0][0].text.chars().count(), MAX_LINE_CHARS);
        let tail: String = rows
            .iter()
            .skip(1)
            .flat_map(|r| r.iter().map(|s| s.text.clone()))
            .collect();
        assert_eq!(tail, "yyyyyyy");
    }

    #[test]
    fn split_auto_keeps_lone_cr_inside_line() {
        // 混流口径(审计 P3):自动模式按 \n 断行,行中 lone CR 不在断行位,
        // 行内容保留 \r(前端 pre-wrap 渲染为折行)——锁定现状防无意识变更
        let mut buf = "a\rb\nc".to_string();
        let mut out = Vec::new();
        split_lines(&mut buf, 0, &mut out);
        assert_eq!(out, vec!["a\rb"]);
        assert_eq!(buf, "c");
    }
}
