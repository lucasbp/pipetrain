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
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Screen {
    rows: Vec<Vec<char>>,
    row: usize,
    column: usize,
    hidden: bool,
}

impl Screen {
    fn write(&mut self, text: &str) {
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            match character {
                '\x1b' => {
                    assert_eq!(characters.next(), Some('['));
                    let mut parameters = String::new();
                    for command in characters.by_ref() {
                        if !command.is_ascii_alphabetic() {
                            parameters.push(command);
                            continue;
                        }
                        match command {
                            'A' => {
                                self.row = self
                                    .row
                                    .saturating_sub(parameters.parse::<usize>().unwrap())
                            }
                            'J' => {
                                self.rows.resize_with(self.row + 1, Vec::new);
                                self.rows[self.row].truncate(self.column);
                            }
                            'K' => {
                                self.rows
                                    .resize_with(self.rows.len().max(self.row + 1), Vec::new);
                                self.rows[self.row].clear();
                            }
                            'l' if parameters == "?25" => self.hidden = true,
                            'h' if parameters == "?25" => self.hidden = false,
                            'm' => {}
                            _ => panic!("unexpected terminal command {parameters}{command}"),
                        }
                        break;
                    }
                }
                '\r' => self.column = 0,
                '\n' => {
                    self.row += 1;
                    self.column = 0;
                }
                other => {
                    self.rows
                        .resize_with(self.rows.len().max(self.row + 1), Vec::new);
                    self.rows[self.row].resize(self.column + 1, ' ');
                    self.rows[self.row][self.column] = other;
                    self.column += 1;
                }
            }
        }
    }

    fn visible(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| !row.is_empty())
            .map(|row| row.iter().collect())
            .collect()
    }
}

fn painter(color: bool) -> (Painter<Sink>, Sink) {
    let sink = Sink::default();
    (
        Painter::new(sink.clone(), true, Duration::ZERO).with_color(color),
        sink,
    )
}

#[test]
fn three_line_frames_redraw_in_place_and_transition_back_to_one_line() {
    let (mut painter, sink) = painter(false);
    painter.set_phase(
        "1/50 completed, 2 active, 49 remaining\nacme/api#42 → main\nwaiting for deployment",
    );
    for tick in 1..=3 {
        painter.tick_at_width(Duration::from_secs(tick), Some(80));
    }
    let mut screen = Screen::default();
    screen.write(&sink.text());
    assert_eq!(screen.visible().len(), 3);
    assert!(screen.visible()[0].contains("1/50 completed"));
    assert_eq!(screen.visible()[1], "acme/api#42 → main");
    assert_eq!(screen.visible()[2], "waiting for deployment");
    assert_eq!(screen.row, 0);
    assert!(screen.hidden);
    assert_eq!(sink.text().matches(HIDE_CURSOR).count(), 1);
    assert_eq!(sink.text().matches(ERASE_BLOCK).count(), 2);
    let start = sink.text().len();
    painter.set_phase("discovering");
    painter.tick_at_width(Duration::from_secs(4), Some(80));
    screen.write(&sink.text()[start..]);
    assert_eq!(screen.visible().len(), 1);
    assert!(screen.visible()[0].contains("discovering"));
}

#[test]
fn results_replace_the_whole_frame_and_survive_following_ticks_and_finish() {
    let (mut painter, sink) = painter(true);
    painter.set_phase("1/2 completed\nacme/api#1 → main\nwaiting for deploy");
    painter.tick_at_width(Duration::from_secs(1), Some(80));
    painter.line("done: acme/api#1");
    painter.set_phase("2/2 completed\nacme/web#2 → main\nmerging");
    painter.tick_at_width(Duration::from_secs(2), Some(80));
    let start = sink.text().len();
    let mut screen = Screen::default();
    screen.write(&sink.text());
    assert_eq!(screen.visible().len(), 4);
    assert_eq!(screen.visible()[0], "done: acme/api#1");
    assert_eq!(screen.visible()[2], "acme/web#2 → main");
    painter.finish();
    screen.write(&sink.text()[start..]);
    assert_eq!(screen.visible(), vec!["done: acme/api#1"]);
    assert!(!screen.hidden);
    assert!(!painter.hidden());
    let ended = sink.text();
    painter.finish();
    assert_eq!(sink.text(), ended);
}

#[test]
fn target_is_bold_only_with_color_and_detail_is_dim() {
    for color in [false, true] {
        let (mut painter, sink) = painter(color);
        painter.set_phase("1/2 completed\nacme/api#1 → main\nchecking readiness");
        painter.tick_at_width(Duration::from_secs(1), Some(80));
        let text = sink.text();
        assert_eq!(
            text.contains(&format!("{BOLD}acme/api#1 → main{RESET}")),
            color
        );
        assert_eq!(
            text.contains(&format!("{DIM}checking readiness{RESET}")),
            color
        );
        assert_eq!(text.contains(CYAN), color);
        if !color {
            assert!(!text.contains("\x1b[1m"));
            assert!(!text.contains("\x1b[2m"));
            assert!(!text.contains(RESET));
        }
    }
}

#[test]
fn each_line_fits_narrow_terminals_and_controls_cannot_inject_more_rows() {
    for width in [80, 40, 24, 12, 5] {
        let (mut painter, sink) = painter(true);
        painter.set_phase("1/50 completed, 2 active, 49 remaining\nacme/api#42 → deploy/old\nlong estágio 日本語 👩‍💻\r\t\x1b[31m description\nunexpected fourth line");
        painter.tick_at_width(Duration::from_secs(1), Some(width));
        let mut screen = Screen::default();
        screen.write(&sink.text());
        let rows = screen.visible();
        assert!(rows.len() <= 3);
        assert!(
            rows.iter().all(|line| line.len() < width),
            "{width}: {rows:?}"
        );
        assert!(!rows.iter().any(|line| line.contains(['\r', '\t', '\x1b'])));
        assert!(!rows.iter().any(|line| line.contains("unexpected")));
        if width < 24 {
            assert!(rows.len() <= 2);
        }
    }
}

#[test]
fn shrinking_a_multiline_frame_clears_reflowed_tail_below_its_anchor() {
    let (mut painter, sink) = painter(false);
    painter.set_phase("1/50 completed\nacme/api#42 → deploy/old\nvery long deployment message that can reflow after a resize");
    painter.tick_at_width(Duration::from_secs(1), Some(80));
    let mut screen = Screen::default();
    screen.write(&sink.text());
    screen.rows.extend([vec!['x'; 12], vec!['y'; 12]]);
    let start = sink.text().len();
    painter.tick_at_width(Duration::from_secs(2), Some(12));
    screen.write(&sink.text()[start..]);
    assert!(screen.visible().len() <= 2);
    assert!(screen.visible().iter().all(|line| line.len() < 12));
    assert_eq!(screen.row, 0);
    let start = sink.text().len();
    painter.tick_at_width(Duration::from_secs(3), Some(2));
    screen.write(&sink.text()[start..]);
    assert!(screen.visible().is_empty());
    assert!(!screen.hidden);
}

#[test]
fn dropping_during_unwinding_clears_every_status_line_and_restores_the_cursor() {
    let sink = Sink::default();
    let cloned = sink.clone();
    let result = std::panic::catch_unwind(move || {
        let mut painter = Painter::new(cloned, true, Duration::ZERO);
        painter.set_phase("0/2 completed\nacme/api#1 → main\nchecking readiness");
        painter.tick_at_width(Duration::from_secs(1), Some(80));
        panic!("simulated failure");
    });
    assert!(result.is_err());
    let mut screen = Screen::default();
    screen.write(&sink.text());
    assert!(screen.visible().is_empty());
    assert!(!screen.hidden);
    assert!(
        sink.text()
            .ends_with(std::str::from_utf8(RESTORE_BLOCK).unwrap())
    );
}

#[test]
fn merge_frames_keep_every_active_row_at_all_terminal_widths() {
    for color in [false, true] {
        for width in [160, 80, 40, 24, 12, 5] {
            let (mut painter, sink) = painter(color);
            painter.set_phase("waiting: 0, running: 4, merged: 2, deploy confirmed: 0, failed/stopped: 0\nacme/api#1 → main | readiness: checking approvals\nacme/ui#2 → main | merging: sending request\nacme/worker#3 → main | waiting for deploy: deploy.yml\nacme/tools#4 → main | resolving: later deploy 日本語\t\x1b[31m");
            painter.tick_at_width(Duration::from_secs(12), Some(width));
            let mut screen = Screen::default();
            screen.write(&sink.text());
            let rows = screen.visible();
            assert_eq!(rows.len(), 5, "{width}: {rows:?}");
            assert!(
                rows.iter().all(|row| row.len() < width),
                "{width}: {rows:?}"
            );
            assert!(rows.iter().all(|row| !row.contains(['\r', '\t', '\x1b'])));
            if width >= 80 {
                assert!(rows[1].contains("readiness:"));
                assert!(rows[2].contains("merging:"));
                assert!(rows[3].contains("waiting for deploy:"));
                assert!(rows[4].contains("resolving:"));
                assert!(rows.iter().skip(1).all(|row| row.contains(" · ")));
            }
            painter.finish();
            let mut cleared = Screen::default();
            cleared.write(&sink.text());
            assert!(cleared.visible().is_empty());
            assert!(!cleared.hidden);
        }
    }
}

#[test]
fn merge_row_clocks_survive_phase_updates_and_finished_rows_are_cleared() {
    let (mut painter, sink) = painter(false);
    painter.set_phase("waiting: 0, running: 2, merged: 0, deploy confirmed: 0, failed/stopped: 0\nacme/api#1 → main | readiness: starting\nacme/ui#2 → main | readiness: starting");
    let first = Instant::now() - Duration::from_secs(65);
    let second = Instant::now() - Duration::from_secs(12);
    painter
        .row_started
        .insert("acme/api#1 → main".into(), first);
    painter
        .row_started
        .insert("acme/ui#2 → main".into(), second);
    painter.set_phase("waiting: 0, running: 2, merged: 1, deploy confirmed: 0, failed/stopped: 0\nacme/api#1 → main | waiting for deploy: deploy.yml\nacme/ui#2 → main | merging: sending request");
    assert_eq!(painter.row_started["acme/api#1 → main"], first);
    assert_eq!(painter.row_started["acme/ui#2 → main"], second);
    painter.tick_at_width(Duration::from_secs(70), Some(120));
    let mut screen = Screen::default();
    screen.write(&sink.text());
    assert!(screen.visible()[0].ends_with(" · 1m 10s"));
    assert!(screen.visible()[1].ends_with(" · 1m 05s"));
    assert!(screen.visible()[2].contains(" · 12."));
    let start = sink.text().len();
    painter.set_phase("waiting: 0, running: 1, merged: 2, deploy confirmed: 1, failed/stopped: 0\nacme/ui#2 → main | waiting for deploy: deploy.yml");
    painter.tick_at_width(Duration::from_secs(71), Some(120));
    screen.write(&sink.text()[start..]);
    assert_eq!(screen.visible().len(), 2);
    assert!(
        !screen
            .visible()
            .iter()
            .any(|row| row.contains("acme/api#1"))
    );
    assert_eq!(painter.row_started.len(), 1);
    assert_eq!(painter.row_started["acme/ui#2 → main"], second);
}
