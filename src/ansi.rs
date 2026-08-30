//! ANSI/VT 转义序列 → 带色文本段。
//!
//! 解析用 [`vte`](https://crates.io/crates/vte)(Alacritty/WezTerm 同款状态机):
//! 增量式、颜色状态跨行跨块保持。本模块只做 SGR(颜色)→ RGB 的映射与
//! 按颜色切段,不做任何渲染。不支持的颜色属性(如背景色/斜体)忽略。

use vte::{Params, Parser, Perform};

/// 一段同色文本
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    /// None = 默认前景色
    pub fg: Option<(u8, u8, u8)>,
}

/// xterm 标准 16 色(VSCode Dark+ 同款色值)
const BASIC16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // 0 black
    (0xcd, 0x31, 0x31), // 1 red
    (0x0d, 0xbc, 0x79), // 2 green
    (0xe5, 0xe5, 0x10), // 3 yellow
    (0x24, 0x72, 0xc8), // 4 blue
    (0xbc, 0x3f, 0xbc), // 5 magenta
    (0x11, 0xa8, 0xcd), // 6 cyan
    (0xe5, 0xe5, 0xe5), // 7 white
    (0x66, 0x66, 0x66), // 8 bright black
    (0xf1, 0x4c, 0x4c), // 9 bright red
    (0x23, 0xd1, 0x8b), // 10 bright green
    (0xf5, 0xf5, 0x43), // 11 bright yellow
    (0x3b, 0x8e, 0xea), // 12 bright blue
    (0xd6, 0x70, 0xd6), // 13 bright magenta
    (0x29, 0xb8, 0xdb), // 14 bright cyan
    (0xe5, 0xe5, 0xe5), // 15 bright white
];

/// xterm 256 色表:0-15 基本色,16-231 6×6×6 色立方,232-255 灰阶
fn palette256(n: u16) -> (u8, u8, u8) {
    if n < 16 {
        return BASIC16[n as usize];
    }
    if n < 232 {
        let i = n - 16;
        let ch = |v: u16| if v == 0 { 0 } else { 55 + 40 * v };
        let r = ch(i / 36);
        let g = ch((i / 6) % 6);
        let b = ch(i % 6);
        (r as u8, g as u8, b as u8)
    } else {
        let v = 8 + 10 * (n - 232);
        (v as u8, v as u8, v as u8)
    }
}

/// 单行收集器:print 累积文本,SGR 变色时切段
#[derive(Default)]
struct Collector {
    fg: Option<(u8, u8, u8)>,
    runs: Vec<Run>,
    text: String,
}

impl Collector {
    fn flush_run(&mut self) {
        if !self.text.is_empty() {
            self.runs.push(Run { text: std::mem::take(&mut self.text), fg: self.fg });
        }
    }
}

impl Perform for Collector {
    fn print(&mut self, c: char) {
        self.text.push(c);
    }

    fn execute(&mut self, byte: u8) {
        // 行内控制字符 v1 一律忽略(行切分由 log_model 的行尾模式负责)
        let _ = byte;
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        if action != 'm' {
            return; // 只关心 SGR(颜色);光标移动/清屏等在日志场景忽略
        }
        // SGR 可能改变颜色:先切断已累积的文本段,让旧颜色归属旧段
        self.flush_run();
        // params.iter():每个分号段一组 &[]u16,冒号子参数在同一组内。
        // "38;5;196" → [38],[5],[196];"38:5:196" → [38,5,196];RGB 同理。
        let mut iter = params.iter();
        while let Some(p) = iter.next() {
            match p.first().copied().unwrap_or(0) {
                0 | 39 => self.fg = None,
                30..=37 => self.fg = Some(BASIC16[(p[0] - 30) as usize]),
                90..=97 => self.fg = Some(BASIC16[(p[0] - 90 + 8) as usize]),
                38 => {
                    // 冒号式:子参数在同一组内,如 [38,5,196] / [38,2,r,g,b] /
                    // [38,2,0,r,g,b](空子参数=默认色彩空间,vte 记为 0)
                    if p.len() >= 2 {
                        match p[1] {
                            5 if p.len() >= 3 => self.fg = Some(palette256(p[2])),
                            2 if p.len() >= 5 => {
                                let base = if p.len() >= 6 { 3 } else { 2 };
                                self.fg =
                                    Some((p[base] as u8, p[base + 1] as u8, p[base + 2] as u8));
                            }
                            _ => {}
                        }
                    } else {
                        // 分号式:子参数在后续各组,如 [38],[5],[196] / [38],[2],[r],[g],[b]
                        match iter.next() {
                            Some(&[5]) => {
                                if let Some(n) = iter.next().and_then(|g| g.first().copied()) {
                                    self.fg = Some(palette256(n));
                                }
                            }
                            Some(&[2]) => {
                                let mut rgb = [0u16; 3];
                                let mut ok = true;
                                for slot in &mut rgb {
                                    match iter.next().and_then(|g| g.first().copied()) {
                                        Some(v) => *slot = v,
                                        None => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                                if ok {
                                    self.fg =
                                        Some((rgb[0] as u8, rgb[1] as u8, rgb[2] as u8));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // 背景色(40-47/48/100-107)、粗体斜体等:v1 忽略
                _ => {}
            }
        }
    }
}

/// 持久 ANSI → 带色行转换器。整体持有一个 vte Parser,
/// 颜色状态跨行、跨块(SGR 序列被读块切断也能续上)保持。
#[derive(Default)]
pub struct AnsiLines {
    parser: Parser,
    collector: Collector,
}

impl AnsiLines {
    /// 喂入一行(不含行尾符),返回该行的带色文本段。
    /// 颜色状态保留到下一行(终端语义:SGR 持续到被重置)。
    pub fn feed_line(&mut self, line: &str) -> Vec<Run> {
        for b in line.as_bytes() {
            self.parser.advance(&mut self.collector, *b);
        }
        self.collector.flush_run();
        std::mem::take(&mut self.collector.runs)
    }

    /// 清空(连同解析器颜色状态一起重置)
    pub fn reset(&mut self) {
        self.collector.fg = None;
        self.collector.runs.clear();
        self.collector.text.clear();
        self.parser = Parser::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(line: &str) -> Vec<Run> {
        AnsiLines::default().feed_line(line)
    }

    fn fg_of(r: &Run) -> Option<(u8, u8, u8)> {
        r.fg
    }

    #[test]
    fn plain_text_single_run_default_color() {
        let out = runs("hello world");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "hello world");
        assert_eq!(out[0].fg, None);
    }

    #[test]
    fn basic_colors_split_runs() {
        let out = runs("\x1b[32mOK\x1b[0m done");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "OK");
        assert_eq!(out[0].fg, Some((0x0d, 0xbc, 0x79))); // green
        assert_eq!(out[1].text, " done");
        assert_eq!(out[1].fg, None);
    }

    #[test]
    fn bright_and_256_and_rgb() {
        let out = runs("\x1b[91mA\x1b[38;5;196mB\x1b[38;2;10;20;30mC");
        assert_eq!(fg_of(&out[0]), Some((0xf1, 0x4c, 0x4c))); // bright red
        assert_eq!(fg_of(&out[1]), Some((0xff, 0x00, 0x00))); // 256 -> #ff0000
        assert_eq!(fg_of(&out[2]), Some((10, 20, 30)));
    }

    #[test]
    fn colon_form_extended_color() {
        let out = runs("\x1b[38:5:46mX\x1b[38:2::1:2:3mY");
        assert_eq!(fg_of(&out[0]), Some((0x00, 0xff, 0x00)));
        assert_eq!(fg_of(&out[1]), Some((1, 2, 3)));
    }

    #[test]
    fn color_state_carries_across_lines_and_resets() {
        let mut ansi = AnsiLines::default();
        let l1 = ansi.feed_line("\x1b[31mred line");
        assert_eq!(l1[0].fg, Some((0xcd, 0x31, 0x31)));
        // 下一行没有出现任何 SGR:颜色按终端语义延续
        let l2 = ansi.feed_line("still red");
        assert_eq!(l2[0].fg, Some((0xcd, 0x31, 0x31)));
        let l3 = ansi.feed_line("\x1b[0mplain again");
        assert_eq!(l3[0].fg, None);
    }

    #[test]
    fn escape_split_across_feed_calls_is_reassembled() {
        // 序列被两个读块切断:前块含 "\x1b[3",后块含 "1m..."——解析器必须续上
        let mut ansi = AnsiLines::default();
        let mut out = ansi.feed_line("\x1b[3");
        out.extend(ansi.feed_line("1mred"));
        assert_eq!(out.iter().filter(|r| !r.text.is_empty()).count(), 1);
        assert_eq!(out[0].fg, Some((0xcd, 0x31, 0x31)));
        assert_eq!(out[0].text, "red");
    }

    #[test]
    fn non_sgr_csi_ignored() {
        let out = runs("\x1b[2J\x1b[1;1Htext");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "text");
        assert_eq!(out[0].fg, None);
    }

    #[test]
    fn reset_clears_parser_state() {
        let mut ansi = AnsiLines::default();
        let _ = ansi.feed_line("\x1b[31mred");
        ansi.reset();
        let out = ansi.feed_line("plain");
        assert_eq!(out[0].fg, None);
    }
}
