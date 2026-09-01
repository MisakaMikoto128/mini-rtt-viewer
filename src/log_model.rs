//! 日志消息泵:worker 消息 → 按接收行尾模式断行 → ANSI 解析 → 带色行。
//!
//! 与 Slint UI 完全解耦(纯逻辑,可单测);main 的 UI timer 每个周期:
//! 1. 逐条取 worker 消息,调用 [`LogPump::absorb_text`] / [`LogPump::absorb_frame_end`]
//! 2. 调 [`LogPump::enforce_line_cap`] 兜底超长行
//! 3. 调 [`LogPump::take_new_rows`] 拿**增量**新行,逐行 push 进 UI 的行模型
//!    (ListView 虚拟化,只渲染可见行,长日志不再全量重排)

use crate::ansi::{AnsiLines, Run};
use unicode_width::UnicodeWidthChar;

/// 按显示宽度把一行(带色段)硬切成多行:每段切到 cols 列为止,续行继承
/// 当前颜色状态。切分单位是**显示列**(ASCII/半角 1 列,CJK/全角 2 列)——
/// 与等宽字体渲染宽度对齐,UI 按列数换算像素即可。
fn wrap_runs(runs: &[Run], cols: usize) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Run>> = Vec::new();
    let mut cur: Vec<Run> = Vec::new();
    let mut cur_w: usize = 0;
    for run in runs {
        let mut seg = String::new();
        for ch in run.text.chars() {
            // 控制字符宽度按 0 处理会卡死切分循环,兜底按 1 列
            let w = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if cur_w + w > cols && cur_w > 0 {
                cur.push(Run { text: std::mem::take(&mut seg), fg: run.fg });
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            seg.push(ch);
            cur_w += w;
        }
        if !seg.is_empty() {
            cur.push(Run { text: seg, fg: run.fg });
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        // 空行保留(纯 \n 场景)
        out.push(vec![Run { text: String::new(), fg: runs.first().and_then(|r| r.fg) }]);
    }
    out
}

/// UI 刷新周期:决定消息从到达(队列)到上屏的额外延迟。过大(如 200ms)会把
/// 均匀到达的多条消息攒成一批同时冒出来,视觉上"两条一次"。10ms 粒度下
/// 每 tick 通常 0~1 条,显示节奏与设备发送节奏一致。空 tick 只有一次
/// try_recv 的开销,可忽略。
pub const FLUSH_MS: u64 = 10;
/// 断帧间隔输入框解析失败时的默认值(ms)
pub const DEFAULT_FRAME_TIMEOUT_MS: u32 = 20;
/// 单行硬上限兜底(超长帧/关闭断行时的无换行流)
pub const MAX_LINE_CHARS: usize = 512;
/// 日志行数上限(整体平移渲染,无虚拟化;超限丢最旧行,量级与旧版 6 万字符相当)
pub const MAX_LOG_ROWS: usize = 500;

/// 按接收行尾模式切行:0=自动(\n 断行、吞 \r) 1=CRLF 2=LF 3=CR 4=无(不断行)。
/// 切出的行内容**一律不含行尾符**——行尾模式只决定"在哪断行",不改变内容。
pub fn split_lines(p: &mut String, rx_ending: i32, out: &mut Vec<String>) {
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

/// 滚动几何(纯逻辑,与 `log_view.slint` 的 `scroll-by-px` **逐条对应**;
/// Slint 表达式抽不成 Rust,只能双份——改任一侧必须同步另一侧,单测锁语义)。
///
/// 语义要点:手动滚到底与自动跟随必须停在**同一个** offset(max-offset,像素
/// 精确,底部 padding 完整)。行格量化只用于上翻的行位稳定——若撞底也停在
/// floor 后的 grid-max,会裁掉 (max-offset mod 行高) 像素,最后一行被遮一点
/// 且与跟随态贴底距离不一致(踩过:见 commit "滚到底被裁一小截")。
#[derive(Clone, Copy, Debug)]
pub struct ScrollGeom {
    pub line_h: f64,
    pub content_h: f64,
    pub viewport_h: f64,
}

impl ScrollGeom {
    pub fn max_offset(&self) -> f64 {
        (self.content_h - self.viewport_h).max(0.0)
    }

    /// 行格上界(上翻吸附用):floor 到行高网格
    pub fn grid_max(&self) -> f64 {
        (self.max_offset() / self.line_h).floor() * self.line_h
    }

    /// 应用一次像素滚动(delta 正 = 上翻),入参/返回值均为未量化累积量 raw;
    /// 返回 (new_raw, new_offset, follow)。
    pub fn scroll_by(&self, raw: f64, delta: f64) -> (f64, f64, bool) {
        let raw = raw - delta;
        let u = (raw / self.line_h).floor() * self.line_h;
        let max_off = self.max_offset();
        let grid_max = self.grid_max();
        let (offset, raw) = if u >= grid_max {
            // 撞底:像素精确贴死 max-offset(与自动跟随同一终点)
            (max_off, max_off)
        } else {
            let offset = u.clamp(0.0, grid_max);
            // 撞顶时累积量对齐,反向滚动立即响应(不消化攒量)
            let raw = if offset != u { offset } else { raw };
            (offset, raw)
        };
        (raw, offset, offset >= max_off - 0.5)
    }
}

/// 日志泵状态(main 与"清空"回调共享)
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
    /// 换行列数(等宽显示列,CJK 记 2 列;0 = 不换行)。变更时全量重排
    wrap_cols: usize,
}

impl LogPump {
    /// 消化一段文本数据(Log 横幅 / RTT 读块),按接收行尾模式切出完整行
    pub fn absorb_text(&mut self, text: &str, rx_ending: i32) {        self.pending.push_str(text);
        split_lines(&mut self.pending, rx_ending, &mut self.new_lines);
    }

    /// worker 判定一帧结束(相邻数据间隔超过断帧超时):缓冲整体切为一行
    pub fn absorb_frame_end(&mut self, rx_ending: i32) {
        if self.pending.is_empty() {
            return;
        }
        split_lines(&mut self.pending, rx_ending, &mut self.new_lines);
        if !self.pending.is_empty() {
            self.new_lines.push(std::mem::take(&mut self.pending));
        }
    }

    /// 单行长度兜底:超长帧/无换行流按 [`MAX_LINE_CHARS`] 硬切
    pub fn enforce_line_cap(&mut self) {
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
        self.marks.push(vec![Run { text: text.to_string(), fg: Some(fg) }]);
    }

    /// 把已切出的行经 ANSI 解析转为带色行;有新行返回它们(增量上屏)。
    /// 超出 [`MAX_LOG_ROWS`] 时丢最旧行,返回值仍只含**本次新增**的行。
    pub fn take_new_rows(&mut self) -> Option<Vec<Vec<Run>>> {
        if self.new_lines.is_empty() && self.marks.is_empty() {
            return None;
        }
        let mut fresh: Vec<Vec<Run>> = self.marks.drain(..).collect();
        for l in self.new_lines.drain(..) {
            fresh.push(self.ansi.feed_line(&l));
        }
        // 按当前列数硬换行(0 = 不换行):每行仍是单视觉行,UI 滚动数学不变
        if self.wrap_cols > 0 {
            fresh = fresh.iter().flat_map(|r| wrap_runs(r, self.wrap_cols)).collect();
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

    /// 设置换行列数(0 = 不换行)。变更时全量重排既有行,返回 true 表示
    /// 行集已变(调用方需全量刷新 UI 行模型)。
    pub fn set_wrap_cols(&mut self, cols: usize) -> bool {
        let cols = cols.min(512); // 荒谬大列数等同不换行,防 resize 抖动出天文数字
        if cols == self.wrap_cols {
            return false;
        }
        self.wrap_cols = cols;
        if cols > 0 {
            self.rows = self.rows.iter().flat_map(|r| wrap_runs(r, cols)).collect();
        }
        true
    }

    /// 全量行快照(重排后整体刷新 UI 行模型用)
    pub fn snapshot_rows(&self) -> Vec<Vec<Run>> {
        self.rows.clone()
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
        let joined: String = rows.iter().flat_map(|r| r.iter().map(|s| s.text.clone())).collect();
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
        let mut pump = LogPump::default();
        pump.absorb_text("a\r\n", 0);
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

    #[test]
    fn marks_survive_pause_and_clear_wipes_them() {
        let mut pump = LogPump::default();
        pump.paused = true;
        pump.push_colored_line("mark", (1, 2, 3));
        assert!(pump.take_new_rows().is_some()); // 暂停只丢数据,不丢标记
        pump.clear();
        assert!(pump.take_new_rows().is_none());
    }

    fn joined(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.clone()).collect()
    }

    #[test]
    fn wrap_runs_splits_ascii_by_columns() {
        let runs = vec![Run { text: "abcdefghij".into(), fg: None }];
        let out = wrap_runs(&runs, 4);
        assert_eq!(out.iter().map(|l| joined(l)).collect::<Vec<_>>(), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrap_runs_counts_cjk_as_two_columns() {
        // "你好啊" 每字 2 列:4 列宽 → "你好" | "啊"
        let runs = vec![Run { text: "你好啊".into(), fg: None }];
        let out = wrap_runs(&runs, 4);
        assert_eq!(out.iter().map(|l| joined(l)).collect::<Vec<_>>(), vec!["你好", "啊"]);
    }

    #[test]
    fn wrap_runs_carries_color_into_continuation() {
        // 红色长行切两段:续行仍为红色
        let runs = vec![Run { text: "abcdef".into(), fg: Some((0xcc, 0x33, 0x44)) }];
        let out = wrap_runs(&runs, 3);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1][0].fg, Some((0xcc, 0x33, 0x44)));
    }

    #[test]
    fn wrap_runs_keeps_empty_line() {
        let out = wrap_runs(&[], 10);
        assert_eq!(out.len(), 1);
        assert_eq!(joined(&out[0]), "");
    }

    #[test]
    fn pump_rewrap_reflows_all_rows() {
        let mut pump = LogPump::default();
        pump.absorb_text("abcdefghij\n", 0);
        pump.enforce_line_cap();
        assert!(pump.take_new_rows().is_some());
        assert!(pump.set_wrap_cols(4)); // 行集已变
        let rows = pump.snapshot_rows();
        assert_eq!(rows.iter().map(|l| joined(l)).collect::<Vec<_>>(), vec!["abcd", "efgh", "ij"]);
        assert!(!pump.set_wrap_cols(4)); // 同值不重排
        assert!(pump.set_wrap_cols(0)); // 关闭换行不重排既有行(增量按新行生效)
    }

    #[test]
    fn pump_wraps_new_lines_at_current_cols() {
        let mut pump = LogPump::default();
        pump.set_wrap_cols(4);
        pump.absorb_text("abcdefghij\n", 0);
        pump.enforce_line_cap();
        let rows = pump.take_new_rows().unwrap();
        assert_eq!(rows.iter().map(|l| joined(l)).collect::<Vec<_>>(), vec!["abcd", "efgh", "ij"]);
    }

    // ---- 滚动几何(与 log_view.slint scroll-by-px 同步,语义锁定)----

    /// 复刻"手动滚回底部被裁一小截"的几何:max_offset 不落在行格上,
    /// 余量 r = 2316-2304 = 12,超过底部 padding——旧实现停在 2304 就裁字。
    fn geom() -> ScrollGeom {
        ScrollGeom { line_h: 32.0, content_h: 3216.0, viewport_h: 900.0 }
    }

    #[test]
    fn scroll_lands_bottom_pixel_exact_not_on_grid() {
        let g = geom();
        assert_eq!(g.max_offset(), 2316.0);
        assert_eq!(g.grid_max(), 2304.0); // floor 少 12,行格≠贴底
        // 贴底状态继续下滚(哪怕已到底):必须停在 max-offset 而非 grid-max
        let (raw, offset, follow) = g.scroll_by(2316.0, -64.0);
        assert_eq!(offset, 2316.0);
        assert_eq!(raw, 2316.0);
        assert!(follow);
    }

    #[test]
    fn scroll_up_from_bottom_snaps_to_grid() {
        let g = geom();
        let (raw, offset, follow) = g.scroll_by(2316.0, 64.0);
        assert_eq!(offset, 2240.0);
        assert_eq!(offset % 32.0, 0.0); // 上翻落行格,行位稳定
        assert_eq!(raw, 2252.0);
        assert!(!follow);
    }

    #[test]
    fn scroll_down_from_grid_returns_pixel_bottom() {
        // 用户操作序列:上翻(行格 2240)后向下滚回底部 → 与自然跟随同一终点
        let g = geom();
        let (_, offset, follow) = g.scroll_by(2252.0, -64.0);
        assert_eq!(offset, g.max_offset()); // 2316,不是 grid_max 2304
        assert!(follow);
    }

    #[test]
    fn scroll_clamps_at_top_and_syncs_raw() {
        let g = geom();
        let (raw, offset, follow) = g.scroll_by(64.0, 1000.0);
        assert_eq!(offset, 0.0);
        assert_eq!(raw, 0.0); // 撞顶累积量对齐,反向滚动不消化攒量
        assert!(!follow);
    }

    #[test]
    fn scroll_short_content_is_always_following() {
        // 内容不足一屏:max_offset=0,任何滚动都停在 0 且视为贴底
        let g = ScrollGeom { line_h: 32.0, content_h: 200.0, viewport_h: 900.0 };
        let (raw, offset, follow) = g.scroll_by(0.0, -240.0);
        assert_eq!(offset, 0.0);
        assert_eq!(raw, 0.0);
        assert!(follow);
    }
}
