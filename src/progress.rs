pub(crate) mod merge;
mod term;

#[cfg(test)]
mod multiline;

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const DEFAULT_DELAY: Duration = Duration::from_millis(300);
const REDRAW: Duration = Duration::from_millis(80);
pub(crate) const HIDE_CURSOR: &str = "\x1b[?25l";
pub(crate) const ERASE_LINE: &str = "\r\x1b[2K";
const RESTORE: &[u8] = b"\r\x1b[2K\x1b[?25h";
const RESTORE_BLOCK: &[u8] = b"\r\x1b[0J\x1b[?25h";
const ERASE_BLOCK: &str = "\r\x1b[0J";
const BOLD: &str = "\x1b[1m";
pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const DIM: &str = "\x1b[2m";
pub(crate) const RESET: &str = "\x1b[0m";

type Active = Option<Painter<Box<dyn Write + Send>>>;

static ACTIVE: Mutex<Active> = Mutex::new(None);
static CURSOR_HIDDEN: AtomicBool = AtomicBool::new(false);
static DRAWN_LINES: AtomicUsize = AtomicUsize::new(0);
static PANIC_HOOK: Once = Once::new();

pub struct Painter<W: Write> {
    writer: W,
    delay: Duration,
    phase: Option<String>,
    counter: Option<(usize, usize)>,
    frame: usize,
    hidden: bool,
    enabled: bool,
    color: bool,
    drawn_lines: usize,
    row_started: std::collections::BTreeMap<String, Instant>,
}

fn ellipsize(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let marker = if budget >= "…".len() { "…" } else { "." };
    let available = budget.saturating_sub(marker.len());
    let mut out = String::new();
    for character in text.chars() {
        if out.len() + character.len_utf8() > available {
            break;
        }
        out.push(character);
    }
    if budget > 0 {
        out.push_str(marker);
    }
    out
}

fn fit_phase(phase: &str, budget: usize) -> String {
    let plain: String = phase
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if plain.len() <= budget {
        return plain;
    }
    if let Some(open) = plain.find('[')
        && let Some(close) = plain[open..].find("]: ").map(|offset| open + offset + 1)
        && close + 2 > budget
    {
        let mut words = plain[..open].split_whitespace();
        let first = words.next().unwrap_or_default();
        let counter = if first == "step" {
            words.next().unwrap_or_default()
        } else {
            first
        };
        let counter = counter.trim_end_matches(',');
        if counter.split_once('/').is_some_and(|(done, total)| {
            !done.is_empty()
                && !total.is_empty()
                && done
                    .chars()
                    .chain(total.chars())
                    .all(|character| character.is_ascii_digit())
        }) {
            return ellipsize(
                &format!("{counter} {}{}", &plain[open..close], &plain[close..]),
                budget,
            );
        }
    }
    ellipsize(&plain, budget)
}

impl<W: Write> Painter<W> {
    pub fn new(writer: W, enabled: bool, delay: Duration) -> Self {
        Painter {
            writer,
            delay,
            phase: None,
            counter: None,
            frame: 0,
            hidden: false,
            enabled,
            color: false,
            drawn_lines: 0,
            row_started: std::collections::BTreeMap::new(),
        }
    }

    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    pub fn set_phase(&mut self, text: &str) {
        if text.starts_with("waiting: ") {
            let targets: Vec<_> = text
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(" | ").map(|(target, _)| target.to_string()))
                .collect();
            self.row_started
                .retain(|target, _| targets.contains(target));
            for target in targets {
                self.row_started.entry(target).or_insert_with(Instant::now);
            }
        } else {
            self.row_started.clear();
        }
        self.phase = Some(
            text.chars()
                .map(|character| {
                    if character != '\n' && character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect(),
        );
        self.counter = None;
    }

    pub fn set_count(&mut self, done: usize, total: usize) {
        self.counter = Some((done, total));
    }

    pub fn tick(&mut self, elapsed: Duration) {
        self.tick_at_width(elapsed, terminal_width());
    }

    fn tick_at_width(&mut self, elapsed: Duration, width: Option<usize>) {
        if !self.enabled || elapsed < self.delay {
            return;
        }
        let Some(phase) = &self.phase else {
            return;
        };
        let hide = if self.hidden { "" } else { HIDE_CURSOR };
        let glyph = FRAMES[self.frame % FRAMES.len()];
        let (glyph_on, dim_on, off) = if self.color {
            (CYAN, DIM, RESET)
        } else {
            ("", "", "")
        };
        let limit = width.unwrap_or(80).saturating_sub(1);
        let glyph_bytes = glyph.len_utf8() + 1;
        if limit < glyph_bytes {
            self.finish();
            return;
        }
        let merge = phase.starts_with("waiting: ");
        let mut lines = phase.split('\n').take(if merge { usize::MAX } else { 3 });
        let header = lines.next().unwrap_or_default();
        let mut extra: Vec<&str> = lines.collect();
        if limit < 24 && !merge {
            extra.truncate(1);
        }
        let mut counter = self
            .counter
            .map(|(done, total)| format!(" {done}/{total}"))
            .unwrap_or_default();
        let mut elapsed_text = format!(" · {}", crate::elapsed::format(elapsed));
        if glyph_bytes + counter.len() + elapsed_text.len() > limit {
            counter.clear();
            elapsed_text = ellipsize(&elapsed_text, limit - glyph_bytes);
        }
        let phase = fit_phase(
            header,
            limit - glyph_bytes - counter.len() - elapsed_text.len(),
        );
        let erase = self.erase();
        let mut text = format!(
            "{hide}{erase}{glyph_on}{glyph}{off} {phase}{counter}{dim_on}{elapsed_text}{off}"
        );
        for (index, line) in extra.iter().enumerate() {
            let fitted = if merge {
                let elapsed = line
                    .split_once(" | ")
                    .and_then(|(target, _)| self.row_started.get(target))
                    .map(|started| format!(" · {}", crate::elapsed::format(started.elapsed())))
                    .unwrap_or_default();
                let elapsed = ellipsize(&elapsed, limit);
                format!("{}{elapsed}", ellipsize(line, limit - elapsed.len()))
            } else {
                ellipsize(line, limit)
            };
            let style = if self.color {
                if index == 0 { BOLD } else { DIM }
            } else {
                ""
            };
            text.push_str(&format!("\r\n{ERASE_LINE}{style}{fitted}{off}"));
        }
        if !extra.is_empty() {
            text.push_str(&format!("\x1b[{}A\r", extra.len()));
        }
        let drawn_lines = extra.len() + 1;
        self.emit(text.as_bytes());
        self.hidden = true;
        self.drawn_lines = drawn_lines;
        self.frame += 1;
    }

    fn erase(&self) -> &'static str {
        if self.drawn_lines > 1 {
            ERASE_BLOCK
        } else {
            ERASE_LINE
        }
    }

    pub fn finish(&mut self) {
        if !self.hidden {
            return;
        }
        let restore = if self.drawn_lines > 1 {
            RESTORE_BLOCK
        } else {
            RESTORE
        };
        self.emit(restore);
        self.hidden = false;
        self.drawn_lines = 0;
    }

    pub fn line(&mut self, text: &str) {
        let choice = if self.color {
            anstream::ColorChoice::AlwaysAnsi
        } else {
            anstream::ColorChoice::Never
        };
        let mut stream = anstream::AutoStream::new(Vec::new(), choice);
        let _ = write!(stream, "{text}");
        let text = String::from_utf8_lossy(&stream.into_inner()).into_owned();
        let out = if self.hidden {
            format!("{}{text}\n", self.erase())
        } else {
            format!("{text}\n")
        };
        self.emit(out.as_bytes());
        self.drawn_lines = 0;
    }

    fn emit(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }
}

impl<W: Write> Drop for Painter<W> {
    fn drop(&mut self) {
        self.finish();
    }
}

fn active() -> MutexGuard<'static, Active> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn restore_into<W: Write>(writer: &mut W) -> bool {
    if !CURSOR_HIDDEN.swap(false, Ordering::SeqCst) {
        return false;
    }
    let _ = writer.write_all(restoration());
    let _ = writer.flush();
    true
}

fn restoration() -> &'static [u8] {
    if DRAWN_LINES.swap(0, Ordering::SeqCst) > 1 {
        RESTORE_BLOCK
    } else {
        RESTORE
    }
}

pub fn terminal_width() -> Option<usize> {
    term::width()
}

pub(crate) fn mark_cursor_hidden(hidden: bool) {
    CURSOR_HIDDEN.store(hidden, Ordering::SeqCst);
    if !hidden {
        DRAWN_LINES.store(0, Ordering::SeqCst);
    }
}

pub(crate) fn prepare_terminal() -> bool {
    if !term::prepare() {
        return false;
    }
    install_panic_hook();
    true
}

pub fn restore_cursor() {
    restore_into(&mut std::io::stderr());
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let taken = match ACTIVE.try_lock() {
                Ok(mut guard) => guard.take(),
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().take(),
                Err(TryLockError::WouldBlock) => None,
            };
            drop(taken);
            restore_cursor();
            previous(info);
        }));
    });
}

pub struct Progress {
    handle: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(enabled: bool) -> Progress {
        Self::start_at(enabled, Instant::now())
    }

    pub(crate) fn start_at(enabled: bool, started: Instant) -> Progress {
        if !enabled || !term::prepare() {
            return Progress { handle: None };
        }
        install_panic_hook();
        Progress::start_into_at(
            Box::new(std::io::stderr()),
            DEFAULT_DELAY,
            REDRAW,
            color_enabled(),
            started,
        )
    }

    #[cfg(test)]
    pub(crate) fn start_into(
        writer: Box<dyn Write + Send>,
        delay: Duration,
        redraw: Duration,
        color: bool,
    ) -> Progress {
        Self::start_into_at(writer, delay, redraw, color, Instant::now())
    }

    fn start_into_at(
        writer: Box<dyn Write + Send>,
        delay: Duration,
        redraw: Duration,
        color: bool,
        started: Instant,
    ) -> Progress {
        *active() = Some(Painter::new(writer, true, delay).with_color(color));
        let handle = thread::spawn(move || {
            loop {
                thread::park_timeout(redraw);
                let mut guard = active();
                match guard.as_mut() {
                    Some(painter) => {
                        painter.tick(started.elapsed());
                        CURSOR_HIDDEN.store(painter.hidden(), Ordering::SeqCst);
                        DRAWN_LINES.store(painter.drawn_lines, Ordering::SeqCst);
                    }
                    None => return,
                }
            }
        });
        Progress {
            handle: Some(handle),
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        {
            let mut guard = active();
            if let Some(mut painter) = guard.take() {
                painter.finish();
            }
            CURSOR_HIDDEN.store(false, Ordering::SeqCst);
            DRAWN_LINES.store(0, Ordering::SeqCst);
        }
        handle.thread().unpark();
        let _ = handle.join();
    }
}

pub fn phase(text: &str) {
    if let Some(painter) = active().as_mut() {
        painter.set_phase(text);
    }
}

pub fn count(done: usize, total: usize) {
    if let Some(painter) = active().as_mut() {
        painter.set_count(done, total);
    }
}

pub fn line(text: &str) {
    match active().as_mut() {
        Some(painter) => {
            painter.line(text);
            DRAWN_LINES.store(painter.drawn_lines, Ordering::SeqCst);
        }
        None => anstream::eprintln!("{text}"),
    }
}

pub fn enabled(
    no_progress: bool,
    json: bool,
    emit_queue: bool,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
    ci: Option<&str>,
) -> bool {
    let in_ci = ci.is_some_and(|value| {
        let value = value.trim();
        value.eq_ignore_ascii_case("true") || value == "1"
    });
    !(no_progress || json || !stderr_is_terminal || in_ci || (emit_queue && !stdout_is_terminal))
}

pub fn color_from(
    no_color: Option<&str>,
    term: Option<&str>,
    choice: anstream::ColorChoice,
) -> bool {
    if matches!(choice, anstream::ColorChoice::Never) {
        return false;
    }
    if matches!(choice, anstream::ColorChoice::Always) {
        return true;
    }
    !(no_color.is_some_and(|value| !value.is_empty()) || term == Some("dumb"))
}

pub fn color_enabled() -> bool {
    color_from(
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        anstream::ColorChoice::global(),
    )
}

pub fn detect(no_progress: bool, json: bool, emit_queue: bool) -> bool {
    enabled(
        no_progress,
        json,
        emit_queue,
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
        std::env::var("CI").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn len(&self) -> usize {
            self.0.lock().unwrap().len()
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

    const DELAY: Duration = Duration::from_millis(300);
    const HIDE: &str = "\x1b[?25l";
    const ERASE: &str = "\r\x1b[2K";
    const SHOW_AFTER_ERASE: &str = "\r\x1b[2K\x1b[?25h";

    fn painter(enabled: bool) -> (Painter<Sink>, Sink) {
        let sink = Sink::default();
        (Painter::new(sink.clone(), enabled, DELAY), sink)
    }

    fn plain_frame(text: &str) -> String {
        regex::Regex::new("\x1b\\[[0-9;?]*[A-Za-z]")
            .unwrap()
            .replace_all(text, "")
            .replace('\r', "")
    }

    #[test]
    fn long_frames_fit_eighty_and_forty_columns_without_splitting_styles_or_utf8() {
        for width in [80, 40] {
            for color in [false, true] {
                for phase in [
                    "step 1/2, 1 remaining [acme/api#1]: deploy acme/api: workflow still running for a very long description",
                    "3/50 completed, 2 active, 47 remaining [acme/api#123]: verificando publicação 日本語 👩‍💻 é andamento muito longo",
                ] {
                    let (p, sink) = painter(true);
                    let mut p = p.with_color(color);
                    p.set_phase(phase);
                    p.tick_at_width(Duration::from_secs(65), Some(width));
                    let raw = sink.text();
                    let frame = plain_frame(&raw);
                    assert!(frame.len() < width, "{width}: {frame:?}");
                    assert!(!frame.contains('\n'));
                    assert!(frame.contains(if phase.starts_with("step") {
                        "[acme/api#1]"
                    } else {
                        "[acme/api#123]"
                    }));
                    assert!(frame.contains(if phase.starts_with("step") {
                        "1/2"
                    } else {
                        "3/50"
                    }));
                    assert!(frame.ends_with(" · 1m 05s"));
                    assert!(frame.contains('…'));
                    assert!(!frame.contains('\x1b'));
                    assert_eq!(raw.contains(CYAN), color);
                    assert_eq!(raw.contains(DIM), color);
                    if color {
                        assert!(raw.ends_with(RESET));
                    }
                    p.finish();
                    assert!(sink.text().ends_with(SHOW_AFTER_ERASE));
                    assert!(!p.hidden());
                }
            }
        }
    }

    #[test]
    fn shrinking_terminal_recomputes_budget_and_preserves_separate_counter() {
        let (mut p, sink) = painter(true);
        p.set_phase("verificando 日本語 👩‍💻 é publicação de dependências com uma descrição longa");
        p.set_count(12, 34);
        for width in [80, 40] {
            let start = sink.len();
            p.tick_at_width(Duration::from_secs(2), Some(width));
            let frame = plain_frame(&sink.text()[start..]);
            assert!(frame.len() < width);
            assert!(frame.ends_with(" 12/34 · 2.0 s"));
        }
    }

    #[test]
    fn only_explicit_newlines_create_another_terminal_line() {
        let (mut p, sink) = painter(true);
        p.set_phase("deploy\r\n\t日本語");
        p.tick_at_width(Duration::from_secs(1), Some(40));
        let frame = plain_frame(&sink.text());
        assert!(!frame.contains(['\r', '\t']));
        assert_eq!(frame.lines().count(), 2);
        assert!(frame.lines().all(|line| line.len() < 40));
    }

    #[test]
    fn result_colors_preserve_spinner_controls_and_follow_color_choice() {
        for color in [false, true] {
            let (p, sink) = painter(true);
            let mut p = p.with_color(color);
            p.set_phase("deploy");
            p.tick(DELAY);
            p.line("\x1b[31mfailed:\x1b[0m deploy");
            assert!(sink.text().contains(ERASE));
            assert_eq!(sink.text().contains("\x1b[31mfailed:"), color);
            assert!(sink.text().contains("failed:"));
        }
    }

    #[test]
    fn tick_before_the_delay_writes_nothing() {
        let (mut p, sink) = painter(true);
        p.set_phase("discovering");
        p.tick(Duration::from_millis(299));
        assert_eq!(sink.text(), "");
        assert!(!p.hidden());
    }

    #[test]
    fn first_frame_hides_the_cursor_in_the_same_write() {
        let (mut p, sink) = painter(true);
        p.set_phase("discovering");
        p.tick(DELAY);
        assert_eq!(sink.text(), format!("{HIDE}{ERASE}⠋ discovering · 0.3 s"));
        assert!(p.hidden());
    }

    #[test]
    fn first_frame_with_color_wraps_the_glyph_and_dims_the_elapsed_time() {
        let sink = Sink::default();
        let mut p = Painter::new(sink.clone(), true, DELAY).with_color(true);
        p.set_phase("discovering");
        p.tick(DELAY);
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}\x1b[36m⠋\x1b[0m discovering\x1b[2m · 0.3 s\x1b[0m")
        );
    }

    #[test]
    fn counter_with_color_stays_plain() {
        let sink = Sink::default();
        let mut p = Painter::new(sink.clone(), true, DELAY).with_color(true);
        p.set_phase("reading");
        p.set_count(12, 34);
        p.tick(DELAY);
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}\x1b[36m⠋\x1b[0m reading 12/34\x1b[2m · 0.3 s\x1b[0m")
        );
    }

    #[test]
    fn without_color_no_style_codes_are_written() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading");
        p.set_count(1, 2);
        p.tick(DELAY);
        p.tick(Duration::from_millis(65_000));
        let text = sink.text();
        assert!(!text.contains("\x1b[3"));
        assert!(!text.contains("\x1b[2m"));
        assert!(!text.contains("\x1b[0m"));
    }

    #[test]
    fn elapsed_time_updates_on_every_redraw() {
        let (mut p, sink) = painter(true);
        p.set_phase("a");
        p.tick(DELAY);
        p.tick(Duration::from_millis(1_500));
        p.tick(Duration::from_millis(65_000));
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}⠋ a · 0.3 s{ERASE}⠙ a · 1.5 s{ERASE}⠹ a · 1m 05s")
        );
    }

    #[test]
    fn tick_without_a_phase_writes_nothing() {
        let (mut p, sink) = painter(true);
        p.tick(DELAY);
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn second_frame_does_not_hide_again() {
        let (mut p, sink) = painter(true);
        p.set_phase("a");
        p.tick(DELAY);
        p.tick(DELAY);
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}⠋ a · 0.3 s{ERASE}⠙ a · 0.3 s")
        );
    }

    #[test]
    fn counter_shows_done_over_total() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading");
        p.set_count(12, 34);
        p.tick(DELAY);
        assert_eq!(sink.text(), format!("{HIDE}{ERASE}⠋ reading 12/34 · 0.3 s"));
    }

    #[test]
    fn set_phase_resets_the_counter() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading");
        p.set_count(1, 2);
        p.set_phase("checking");
        p.tick(DELAY);
        assert!(sink.text().ends_with("checking · 0.3 s"));
    }

    #[test]
    fn shorter_phase_needs_no_padding() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading manifests");
        p.tick(DELAY);
        p.set_phase("ab");
        p.tick(DELAY);
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}⠋ reading manifests · 0.3 s{ERASE}⠙ ab · 0.3 s")
        );
    }

    #[test]
    fn finish_after_a_draw_erases_and_shows_the_cursor_last() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(DELAY);
        p.finish();
        let text = sink.text();
        assert_eq!(
            text,
            format!("{HIDE}{ERASE}⠋ abc · 0.3 s{SHOW_AFTER_ERASE}")
        );
        assert!(text.ends_with(SHOW_AFTER_ERASE));
        assert!(!p.hidden());
    }

    #[test]
    fn finish_without_a_draw_writes_nothing() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(Duration::from_millis(10));
        p.finish();
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn finish_twice_writes_once() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(DELAY);
        p.finish();
        p.finish();
        assert_eq!(
            sink.text(),
            format!("{HIDE}{ERASE}⠋ abc · 0.3 s{SHOW_AFTER_ERASE}")
        );
    }

    #[test]
    fn line_after_a_draw_is_one_erase_and_keeps_the_cursor_hidden() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(DELAY);
        let before = sink.len();
        p.line("request   1  repos");
        let text = sink.text();
        assert_eq!(
            &text[before..],
            format!("{ERASE}request   1  repos\n").as_str()
        );
        assert!(p.hidden());
    }

    #[test]
    fn line_on_a_disabled_painter_writes_only_the_line() {
        let (mut p, sink) = painter(false);
        p.line("hello");
        assert_eq!(sink.text(), "hello\n");
    }

    #[test]
    fn line_before_any_draw_writes_only_the_line() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.line("hello");
        assert_eq!(sink.text(), "hello\n");
    }

    #[test]
    fn disabled_painter_never_draws() {
        let (mut p, sink) = painter(false);
        p.set_phase("abc");
        p.tick(Duration::from_secs(10));
        p.finish();
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn restore_into_writes_only_when_the_cursor_was_hidden() {
        let _serial = serial();
        CURSOR_HIDDEN.store(false, Ordering::SeqCst);
        let mut out: Vec<u8> = Vec::new();
        assert!(!restore_into(&mut out));
        assert!(out.is_empty());
        CURSOR_HIDDEN.store(true, Ordering::SeqCst);
        assert!(restore_into(&mut out));
        assert_eq!(out, RESTORE);
        assert!(!restore_into(&mut out));
        assert_eq!(out, RESTORE);
    }

    #[test]
    fn signal_fallback_clears_the_multiline_block_and_restores_only_once() {
        let _serial = serial();
        CURSOR_HIDDEN.store(true, Ordering::SeqCst);
        DRAWN_LINES.store(3, Ordering::SeqCst);
        let mut out = Vec::new();
        assert!(restore_into(&mut out));
        assert_eq!(out, RESTORE_BLOCK);
        assert_eq!(DRAWN_LINES.load(Ordering::SeqCst), 0);
        assert!(!restore_into(&mut out));
        assert_eq!(out, RESTORE_BLOCK);
    }

    #[test]
    fn progress_lifecycle_tracks_multiline_frames_and_cleans_up_after_a_result() {
        let _serial = serial();
        let sink = Sink::default();
        let progress = Progress::start_into(
            Box::new(sink.clone()),
            Duration::ZERO,
            Duration::from_millis(5),
            true,
        );
        phase("1/2 completed\nacme/api#1 → main\nwaiting for deploy");
        thread::sleep(Duration::from_millis(120));
        assert_eq!(DRAWN_LINES.load(Ordering::SeqCst), 3);
        line("done: acme/api#1");
        phase("2/2 completed\nacme/web#2 → main\nchecking readiness");
        thread::sleep(Duration::from_millis(120));
        drop(progress);
        assert!(
            sink.text()
                .contains(&format!("{ERASE_BLOCK}done: acme/api#1\n"))
        );
        assert!(
            sink.text()
                .ends_with(std::str::from_utf8(RESTORE_BLOCK).unwrap())
        );
        assert!(!CURSOR_HIDDEN.load(Ordering::SeqCst));
        assert_eq!(DRAWN_LINES.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn start_into_with_a_long_delay_writes_zero_bytes() {
        let _serial = serial();
        let sink = Sink::default();
        let progress = Progress::start_into(
            Box::new(sink.clone()),
            Duration::from_secs(30),
            Duration::from_millis(5),
            false,
        );
        phase("reading");
        count(1, 3);
        thread::sleep(Duration::from_millis(40));
        drop(progress);
        assert_eq!(sink.len(), 0);
        assert!(!CURSOR_HIDDEN.load(Ordering::SeqCst));
    }

    #[test]
    fn start_into_lifecycle_hides_draws_and_restores_last() {
        let _serial = serial();
        let sink = Sink::default();
        let progress = Progress::start_into(
            Box::new(sink.clone()),
            Duration::from_millis(20),
            Duration::from_millis(5),
            false,
        );
        phase("reading");
        count(1, 3);
        thread::sleep(Duration::from_millis(120));
        assert!(CURSOR_HIDDEN.load(Ordering::SeqCst));
        line("x");
        drop(progress);
        let text = sink.text();
        assert!(text.starts_with(&format!("{HIDE}{ERASE}⠋ reading")));
        assert_eq!(text.matches(HIDE).count(), 1);
        assert!(text.contains(&format!("{ERASE}x\n")));
        assert!(!text.replace(&format!("{ERASE}x\n"), "").contains("x\n"));
        assert!(text.ends_with(SHOW_AFTER_ERASE));
        assert!(!CURSOR_HIDDEN.load(Ordering::SeqCst));
        let length = sink.len();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(sink.len(), length);
    }

    #[test]
    fn color_truth_table() {
        use anstream::ColorChoice::{Always, Auto, Never};
        assert!(color_from(None, None, Auto));
        assert!(color_from(None, Some("xterm-256color"), Auto));
        assert!(color_from(Some(""), None, Auto));
        assert!(!color_from(Some("1"), None, Auto));
        assert!(!color_from(None, Some("dumb"), Auto));
        assert!(!color_from(Some("1"), Some("dumb"), Auto));
        assert!(!color_from(None, None, Never));
        assert!(!color_from(Some(""), Some("xterm"), Never));
        assert!(color_from(None, None, Always));
        assert!(color_from(Some("1"), Some("dumb"), Always));
    }

    fn interactive(
        no_progress: bool,
        json: bool,
        emit_queue: bool,
        stdout_tty: bool,
        stderr_tty: bool,
        ci: Option<&str>,
    ) -> bool {
        enabled(no_progress, json, emit_queue, stdout_tty, stderr_tty, ci)
    }

    #[test]
    fn enabled_on_an_interactive_terminal() {
        assert!(interactive(false, false, false, true, true, None));
        assert!(interactive(false, false, false, false, true, None));
    }

    #[test]
    fn disabled_when_stderr_is_not_a_terminal() {
        assert!(!interactive(false, false, false, true, false, None));
    }

    #[test]
    fn disabled_by_the_flag_and_by_json() {
        assert!(!interactive(true, false, false, true, true, None));
        assert!(!interactive(false, true, false, true, true, None));
    }

    #[test]
    fn disabled_in_ci() {
        assert!(!interactive(false, false, false, true, true, Some("true")));
        assert!(!interactive(false, false, false, true, true, Some("TRUE")));
        assert!(!interactive(false, false, false, true, true, Some(" 1 ")));
        assert!(interactive(false, false, false, true, true, Some("false")));
    }

    #[test]
    fn queue_needs_stdout_on_a_terminal() {
        assert!(!interactive(false, false, true, false, true, None));
        assert!(interactive(false, false, true, true, true, None));
    }

    #[test]
    fn free_functions_without_a_spinner_do_not_panic() {
        let _serial = serial();
        phase("anything");
        count(1, 2);
    }
}
