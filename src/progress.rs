use std::io::{IsTerminal, Write};
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const DEFAULT_DELAY: Duration = Duration::from_millis(300);
const REDRAW: Duration = Duration::from_millis(80);

type Active = Option<Painter<Box<dyn Write + Send>>>;

static ACTIVE: Mutex<Active> = Mutex::new(None);

pub struct Painter<W: Write> {
    writer: W,
    delay: Duration,
    phase: Option<String>,
    counter: Option<(usize, usize)>,
    frame: usize,
    drawn_len: usize,
    enabled: bool,
}

impl<W: Write> Painter<W> {
    pub fn new(writer: W, enabled: bool, delay: Duration) -> Self {
        Painter {
            writer,
            delay,
            phase: None,
            counter: None,
            frame: 0,
            drawn_len: 0,
            enabled,
        }
    }

    pub fn set_phase(&mut self, text: &str) {
        self.phase = Some(text.to_string());
        self.counter = None;
    }

    pub fn set_count(&mut self, done: usize, total: usize) {
        self.counter = Some((done, total));
    }

    pub fn tick(&mut self, elapsed: Duration) {
        if !self.enabled || elapsed < self.delay {
            return;
        }
        let Some(phase) = &self.phase else {
            return;
        };
        let mut text = format!("{} {phase}", FRAMES[self.frame % FRAMES.len()]);
        if let Some((done, total)) = self.counter {
            text.push_str(&format!(" {done}/{total}"));
        }
        let len = text.chars().count();
        let pad = " ".repeat(self.drawn_len.saturating_sub(len));
        let _ = write!(self.writer, "\r{text}{pad}");
        let _ = self.writer.flush();
        self.drawn_len = len;
        self.frame += 1;
    }

    pub fn clear(&mut self) {
        if self.drawn_len == 0 {
            return;
        }
        let _ = write!(self.writer, "\r{}\r", " ".repeat(self.drawn_len));
        let _ = self.writer.flush();
        self.drawn_len = 0;
    }

    pub fn line(&mut self, text: &str) {
        self.clear();
        let _ = writeln!(self.writer, "{text}");
        let _ = self.writer.flush();
    }
}

fn active() -> MutexGuard<'static, Active> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Progress {
    handle: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(enabled: bool) -> Progress {
        if !enabled {
            return Progress { handle: None };
        }
        let writer: Box<dyn Write + Send> = Box::new(std::io::stderr());
        *active() = Some(Painter::new(writer, true, DEFAULT_DELAY));
        let started = Instant::now();
        let handle = thread::spawn(move || {
            loop {
                thread::park_timeout(REDRAW);
                match active().as_mut() {
                    Some(painter) => painter.tick(started.elapsed()),
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
        if let Some(mut painter) = active().take() {
            painter.clear();
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
        Some(painter) => painter.line(text),
        None => eprintln!("{text}"),
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

    const DELAY: Duration = Duration::from_millis(300);

    fn painter(enabled: bool) -> (Painter<Sink>, Sink) {
        let sink = Sink::default();
        (Painter::new(sink.clone(), enabled, DELAY), sink)
    }

    #[test]
    fn tick_before_the_delay_writes_nothing() {
        let (mut p, sink) = painter(true);
        p.set_phase("discovering");
        p.tick(Duration::from_millis(299));
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn tick_after_the_delay_draws_the_first_frame_and_phase() {
        let (mut p, sink) = painter(true);
        p.set_phase("discovering");
        p.tick(DELAY);
        assert_eq!(sink.text(), "\r⠋ discovering");
    }

    #[test]
    fn tick_without_a_phase_writes_nothing() {
        let (mut p, sink) = painter(true);
        p.tick(DELAY);
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn frames_advance_on_each_tick() {
        let (mut p, sink) = painter(true);
        p.set_phase("a");
        p.tick(DELAY);
        p.tick(DELAY);
        assert_eq!(sink.text(), "\r⠋ a\r⠙ a");
    }

    #[test]
    fn counter_shows_done_over_total() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading");
        p.set_count(12, 34);
        p.tick(DELAY);
        assert_eq!(sink.text(), "\r⠋ reading 12/34");
    }

    #[test]
    fn set_phase_resets_the_counter() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading");
        p.set_count(1, 2);
        p.set_phase("checking");
        p.tick(DELAY);
        assert!(sink.text().ends_with("checking"));
    }

    #[test]
    fn clear_after_a_draw_erases_the_drawn_length() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(DELAY);
        p.clear();
        assert_eq!(sink.text(), format!("\r⠋ abc\r{}\r", " ".repeat(5)));
    }

    #[test]
    fn clear_without_a_draw_writes_nothing() {
        let (mut p, sink) = painter(true);
        p.clear();
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn clear_twice_writes_once() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(DELAY);
        p.clear();
        p.clear();
        assert_eq!(sink.text(), format!("\r⠋ abc\r{}\r", " ".repeat(5)));
    }

    #[test]
    fn line_after_a_draw_clears_first() {
        let (mut p, sink) = painter(true);
        p.set_phase("abc");
        p.tick(DELAY);
        p.line("request   1  repos");
        assert_eq!(
            sink.text(),
            format!("\r⠋ abc\r{}\rrequest   1  repos\n", " ".repeat(5))
        );
    }

    #[test]
    fn line_on_a_disabled_painter_writes_only_the_line() {
        let (mut p, sink) = painter(false);
        p.line("hello");
        assert_eq!(sink.text(), "hello\n");
    }

    #[test]
    fn disabled_painter_never_draws() {
        let (mut p, sink) = painter(false);
        p.set_phase("abc");
        p.tick(Duration::from_secs(10));
        assert_eq!(sink.text(), "");
    }

    #[test]
    fn shorter_phase_pads_over_the_longer_one() {
        let (mut p, sink) = painter(true);
        p.set_phase("reading manifests");
        p.tick(DELAY);
        p.set_phase("ab");
        p.tick(DELAY);
        let longer = "⠋ reading manifests".chars().count();
        let shorter = "⠙ ab".chars().count();
        let expected = format!(
            "\r⠋ reading manifests\r⠙ ab{}",
            " ".repeat(longer - shorter)
        );
        assert_eq!(sink.text(), expected);
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
        phase("anything");
        count(1, 2);
    }
}
