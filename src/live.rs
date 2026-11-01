use std::io::Write;
use std::time::Duration;

use crate::progress::{CYAN, DIM, ERASE_LINE, FRAMES, HIDE_CURSOR, RESET};

const SHOW_CURSOR: &str = "\x1b[?25h";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const BOLD_RED: &str = "\x1b[1;31m";
const MIN_WIDTH: usize = 20;
const MIN_TEXT: usize = 8;
const ROW_INDENT: &str = "  ";
const PREFIX_WIDTH: usize = 4;
const REASON_INDENT: usize = 6;
const SEPARATOR: &str = "  ";
const ELLIPSIS: char = '\u{2026}';
const PENDING_ICON: char = '\u{b7}';
const DONE_ICON: char = '\u{2713}';
const SKIPPED_ICON: char = '\u{2013}';
const FAILED_ICON: char = '\u{2717}';

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Pending,
    Running,
    Done(Duration),
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub text: String,
    pub state: State,
    pub header: bool,
}

fn paint(color: bool, code: &str, text: &str) -> String {
    if color && !code.is_empty() && !text.is_empty() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn fit(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(room - 1).collect();
    out.push(ELLIPSIS);
    out
}

fn fit_row(text: &str, tail: &str, room: usize) -> (String, String, String) {
    if tail.is_empty() {
        return (fit(text, room), String::new(), String::new());
    }
    let text_len = text.chars().count();
    let sep_len = SEPARATOR.chars().count();
    let tail_len = tail.chars().count();
    if text_len + sep_len + tail_len <= room {
        return (text.to_string(), SEPARATOR.to_string(), tail.to_string());
    }
    if room >= sep_len + tail_len + MIN_TEXT {
        let head = fit(text, room - sep_len - tail_len);
        return (head, SEPARATOR.to_string(), tail.to_string());
    }
    let cut = fit(&format!("{text}{SEPARATOR}{tail}"), room);
    let head = cut.chars().take(text_len).collect();
    let sep = cut.chars().skip(text_len).take(sep_len).collect();
    let body = cut.chars().skip(text_len + sep_len).collect();
    (head, sep, body)
}

fn render_row(row: &Row, frame: usize, budget: usize, color: bool, lines: &mut Vec<String>) {
    let text = clean(&row.text);
    if row.header {
        lines.push(paint(color, BOLD, &fit(&text, budget)));
        return;
    }
    let (icon, icon_code, text_code, tail, tail_code) = match &row.state {
        State::Pending => (PENDING_ICON, DIM, DIM, String::new(), ""),
        State::Running => (FRAMES[frame % FRAMES.len()], CYAN, "", String::new(), ""),
        State::Done(elapsed) => (DONE_ICON, GREEN, "", crate::elapsed::format(*elapsed), DIM),
        State::Skipped(reason) => (SKIPPED_ICON, DIM, DIM, clean(reason), DIM),
        State::Failed(_) => (FAILED_ICON, BOLD_RED, "", String::new(), ""),
    };
    let (head, sep, body) = fit_row(&text, &tail, budget - PREFIX_WIDTH);
    lines.push(format!(
        "{ROW_INDENT}{} {}{sep}{}",
        paint(color, icon_code, &icon.to_string()),
        paint(color, text_code, &head),
        paint(color, tail_code, &body),
    ));
    if let State::Failed(reason) = &row.state {
        let reason = fit(&clean(reason), budget - REASON_INDENT);
        lines.push(format!(
            "{}{}",
            " ".repeat(REASON_INDENT),
            paint(color, RED, &reason)
        ));
    }
}

pub fn render_rows(rows: &[Row], frame: usize, width: usize, color: bool) -> Vec<String> {
    let budget = width.max(MIN_WIDTH) - 1;
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows {
        render_row(row, frame, budget, color, &mut lines);
    }
    lines
}

pub struct Live<W: Write> {
    writer: W,
    color: bool,
    width: usize,
    drawn: usize,
    hidden: bool,
}

impl<W: Write> Live<W> {
    pub fn new(writer: W, color: bool, width: usize) -> Self {
        Live {
            writer,
            color,
            width,
            drawn: 0,
            hidden: false,
        }
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    pub fn drawn(&self) -> usize {
        self.drawn
    }

    pub fn draw(&mut self, rows: &[Row], frame: usize) {
        let lines = render_rows(rows, frame, self.width, self.color);
        let mut out = String::new();
        if !self.hidden {
            out.push_str(HIDE_CURSOR);
        }
        if self.drawn > 0 {
            out.push_str(&format!("\x1b[{}A", self.drawn));
        }
        for line in &lines {
            out.push_str(&format!("{ERASE_LINE}{line}\n"));
        }
        if self.drawn > lines.len() {
            let leftover = self.drawn - lines.len();
            for _ in 0..leftover {
                out.push_str(&format!("{ERASE_LINE}\n"));
            }
            out.push_str(&format!("\x1b[{leftover}A"));
        }
        self.emit(out.as_bytes());
        self.drawn = lines.len();
        self.hidden = true;
    }

    pub fn finish(&mut self) {
        if !self.hidden {
            return;
        }
        self.emit(SHOW_CURSOR.as_bytes());
        self.hidden = false;
    }

    fn emit(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const ERASE: &str = "\r\x1b[2K";
    const HIDE: &str = "\x1b[?25l";
    const SHOW: &str = "\x1b[?25h";

    fn row(text: &str, state: State) -> Row {
        Row {
            text: text.to_string(),
            state,
            header: false,
        }
    }

    fn header(text: &str) -> Row {
        Row {
            text: text.to_string(),
            state: State::Pending,
            header: true,
        }
    }

    fn done(text: &str) -> Row {
        row(text, State::Done(Duration::from_millis(400)))
    }

    fn visible(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            assert_eq!(
                chars.next(),
                Some('['),
                "escape without bracket in {line:?}"
            );
            let mut terminated = false;
            for next in chars.by_ref() {
                if next == 'm' {
                    terminated = true;
                    break;
                }
                assert!(
                    next.is_ascii_digit() || next == ';',
                    "bad escape byte in {line:?}"
                );
            }
            assert!(terminated, "unterminated escape in {line:?}");
        }
        out
    }

    fn long(len: usize) -> String {
        "abcdefghij"
            .repeat(len / 10 + 1)
            .chars()
            .take(len)
            .collect()
    }

    fn mixed_rows() -> Vec<Row> {
        let text = long(100);
        vec![
            header(&text),
            row(&text, State::Pending),
            row(&text, State::Running),
            row(&text, State::Done(Duration::from_millis(400))),
            row(&text, State::Skipped(long(100))),
            row(&text, State::Failed(long(100))),
        ]
    }

    #[test]
    fn pending_row_plain() {
        let lines = render_rows(&[row("wait", State::Pending)], 0, 80, false);
        assert_eq!(lines, vec!["  \u{b7} wait"]);
    }

    #[test]
    fn running_row_plain_uses_frame_glyph() {
        let rows = [row("run", State::Running)];
        assert_eq!(render_rows(&rows, 0, 80, false), vec!["  \u{280b} run"]);
        assert_eq!(render_rows(&rows, 1, 80, false), vec!["  \u{2819} run"]);
        assert_eq!(render_rows(&rows, 10, 80, false), vec!["  \u{280b} run"]);
    }

    #[test]
    fn done_row_plain_shows_time() {
        let lines = render_rows(&[done("create branch")], 0, 80, false);
        assert_eq!(lines, vec!["  \u{2713} create branch  0.4 s"]);
    }

    #[test]
    fn skipped_row_plain_shows_reason() {
        let rows = [row("create branch", State::Skipped("exists".into()))];
        assert_eq!(
            render_rows(&rows, 0, 80, false),
            vec!["  \u{2013} create branch  exists"]
        );
    }

    #[test]
    fn failed_row_plain_adds_reason_line() {
        let rows = [row("create branch", State::Failed("boom".into()))];
        assert_eq!(
            render_rows(&rows, 0, 80, false),
            vec!["  \u{2717} create branch", "      boom"]
        );
    }

    #[test]
    fn header_row_plain_has_no_icon_or_indent() {
        let lines = render_rows(&[header("bundle deploy")], 0, 80, false);
        assert_eq!(lines, vec!["bundle deploy"]);
    }

    #[test]
    fn pending_row_color() {
        let lines = render_rows(&[row("wait", State::Pending)], 0, 80, true);
        assert_eq!(lines, vec!["  \x1b[2m\u{b7}\x1b[0m \x1b[2mwait\x1b[0m"]);
    }

    #[test]
    fn running_row_color() {
        let lines = render_rows(&[row("run", State::Running)], 0, 80, true);
        assert_eq!(lines, vec!["  \x1b[36m\u{280b}\x1b[0m run"]);
    }

    #[test]
    fn done_row_color() {
        let lines = render_rows(&[done("create")], 0, 80, true);
        assert_eq!(
            lines,
            vec!["  \x1b[32m\u{2713}\x1b[0m create  \x1b[2m0.4 s\x1b[0m"]
        );
    }

    #[test]
    fn skipped_row_color() {
        let rows = [row("create", State::Skipped("nope".into()))];
        assert_eq!(
            render_rows(&rows, 0, 80, true),
            vec!["  \x1b[2m\u{2013}\x1b[0m \x1b[2mcreate\x1b[0m  \x1b[2mnope\x1b[0m"]
        );
    }

    #[test]
    fn failed_row_color() {
        let rows = [row("create", State::Failed("boom".into()))];
        assert_eq!(
            render_rows(&rows, 0, 80, true),
            vec![
                "  \x1b[1;31m\u{2717}\x1b[0m create",
                "      \x1b[31mboom\x1b[0m"
            ]
        );
    }

    #[test]
    fn header_row_color() {
        let lines = render_rows(&[header("bundle")], 0, 80, true);
        assert_eq!(lines, vec!["\x1b[1mbundle\x1b[0m"]);
    }

    #[test]
    fn no_escape_byte_without_color() {
        for line in render_rows(&mixed_rows(), 3, 40, false) {
            assert!(!line.contains('\x1b'), "{line:?}");
        }
    }

    #[test]
    fn control_characters_in_text_are_neutralized() {
        let rows = [row("a\x1b[31mb\nc", State::Failed("x\ry\x1bz".into()))];
        let lines = render_rows(&rows, 0, 80, false);
        assert_eq!(lines, vec!["  \u{2717} a [31mb c", "      x y z"]);
    }

    #[test]
    fn long_text_is_truncated_with_ellipsis() {
        let lines = render_rows(&[row(&long(100), State::Pending)], 0, 40, false);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].chars().count(), 39);
        assert!(lines[0].ends_with('\u{2026}'));
    }

    #[test]
    fn done_row_keeps_time_when_text_is_truncated() {
        let lines = render_rows(&[done(&long(100))], 0, 40, false);
        assert_eq!(lines[0].chars().count(), 39);
        assert!(lines[0].ends_with("\u{2026}  0.4 s"));
    }

    #[test]
    fn every_line_fits_with_and_without_color() {
        for color in [false, true] {
            for width in [20, 40, 80] {
                for line in render_rows(&mixed_rows(), 0, width, color) {
                    assert!(
                        visible(&line).chars().count() < width,
                        "width {width} color {color}: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn truncation_with_color_never_splits_an_escape() {
        for line in render_rows(&mixed_rows(), 0, 40, true) {
            let plain = visible(&line);
            assert!(plain.chars().count() <= 39, "{line:?}");
            assert!(!plain.contains('\x1b'));
            for segment in line.split("\x1b[").skip(1) {
                let terminator = segment.chars().find(|c| !c.is_ascii_digit() && *c != ';');
                assert_eq!(terminator, Some('m'), "{line:?}");
            }
        }
    }

    #[test]
    fn truncated_skipped_row_cuts_the_reason_when_the_text_is_tight() {
        let rows = [row("short", State::Skipped(long(100)))];
        let lines = render_rows(&rows, 0, 20, true);
        let plain = visible(&lines[0]);
        assert!(plain.chars().count() <= 19);
        assert!(plain.starts_with("  \u{2013} short  "));
        assert!(plain.ends_with('\u{2026}'));
    }

    #[test]
    fn width_below_minimum_behaves_as_twenty() {
        let rows = mixed_rows();
        let narrow = render_rows(&rows, 0, 5, true);
        assert_eq!(narrow, render_rows(&rows, 0, 20, true));
        for line in narrow {
            assert!(visible(&line).chars().count() <= 19);
        }
    }

    fn draw_sink(color: bool, width: usize) -> (Live<Sink>, Sink) {
        let sink = Sink::default();
        (Live::new(sink.clone(), color, width), sink)
    }

    fn pending(texts: &[&str]) -> Vec<Row> {
        texts.iter().map(|t| row(t, State::Pending)).collect()
    }

    #[test]
    fn first_draw_hides_cursor_and_writes_lines() {
        let (mut live, sink) = draw_sink(false, 80);
        live.draw(&pending(&["a", "b"]), 0);
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}  \u{b7} a\n{ERASE}  \u{b7} b\n")
        );
        assert!(live.hidden());
        assert_eq!(live.drawn(), 2);
    }

    #[test]
    fn second_draw_moves_up_without_hiding_again() {
        let (mut live, sink) = draw_sink(false, 80);
        let rows = pending(&["a", "b"]);
        live.draw(&rows, 0);
        let first = sink.text().len();
        live.draw(&rows, 1);
        let second = sink.text()[first..].to_string();
        assert_eq!(
            second,
            format!("\x1b[2A{ERASE}  \u{b7} a\n{ERASE}  \u{b7} b\n")
        );
        assert!(!second.contains(HIDE));
    }

    #[test]
    fn shrinking_draw_erases_leftovers_and_moves_back_up() {
        let (mut live, sink) = draw_sink(false, 80);
        live.draw(&pending(&["a", "b", "c"]), 0);
        let first = sink.text().len();
        live.draw(&pending(&["a"]), 0);
        let second = sink.text()[first..].to_string();
        assert_eq!(
            second,
            format!("\x1b[3A{ERASE}  \u{b7} a\n{ERASE}\n{ERASE}\n\x1b[2A")
        );
        assert_eq!(live.drawn(), 1);
    }

    #[test]
    fn drawn_counts_the_failed_reason_line() {
        let (mut live, _sink) = draw_sink(false, 80);
        let rows = vec![header("bundle"), row("step", State::Failed("boom".into()))];
        live.draw(&rows, 0);
        assert_eq!(live.drawn(), 3);
    }

    #[test]
    fn finish_shows_cursor_once_and_keeps_the_list() {
        let (mut live, sink) = draw_sink(false, 80);
        live.draw(&pending(&["a"]), 0);
        let before = sink.text().len();
        live.finish();
        assert_eq!(sink.text()[before..], *SHOW);
        assert!(!live.hidden());
        live.finish();
        assert_eq!(sink.text()[before..], *SHOW);
        assert_eq!(live.drawn(), 1);
    }

    #[test]
    fn finish_writes_nothing_when_never_drawn() {
        let (mut live, sink) = draw_sink(false, 80);
        live.finish();
        assert_eq!(sink.text(), "");
        assert!(!live.hidden());
    }

    #[test]
    fn draw_after_finish_hides_the_cursor_again() {
        let (mut live, sink) = draw_sink(false, 80);
        live.draw(&pending(&["a"]), 0);
        live.finish();
        let before = sink.text().len();
        live.draw(&pending(&["a"]), 0);
        assert!(sink.text()[before..].starts_with(&format!("{HIDE}\x1b[1A")));
    }

    #[test]
    fn terminal_width_is_none_or_positive() {
        if let Some(width) = crate::progress::terminal_width() {
            assert!(width > 0);
        }
    }
}
