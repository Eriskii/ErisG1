//! `prompter`: a teleprompter. `erisg1 v0 app prompter FILE [--wpm 140]` scrolls the file's text smoothly up a
//! window, at the speaking rate given, past two marks at the reading line. Tap either TouchBar to pause and go on,
//! double tap to go faster, triple tap slower, hold to start over.
//!
//! Each frame is tiny: the arms slide what's already in the frame up a few pixels (`Canvas::blit`), and a new line
//! is drawn only when it's due, into rows below the window that are never shown, from where it slides into view.

use super::kit::{Canvas, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;

pub(crate) const APP: App = App {
    name: "prompter",
    about: "teleprompter for a text file: tap pause/go, double tap faster, triple tap slower, hold to start over",
    fun: false,
    options: Options { font: 2, big: 0, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &["--wpm", "300"],
};

fn demo() -> Script {
    Script::still(12.0)
        .touch(3.0, Side::Right, 2)
        .touch(6.0, Side::Left, 1)
        .touch(7.5, Side::Right, 1)
        .touch(9.0, Side::Left, 3)
        .touch(11.0, Side::Right, 4)
}

/// The text column: its left edge and width (35 characters of 16 px).
const X: i32 = 40;
const W: i32 = 560;
const CHARS: usize = 35;
/// The window shown: its top and height, ten lines of LINE pixels. Below it, one line of rows that are never
/// shown, where the next line is drawn before it slides up into view.
const TOP: i32 = 80;
const WINDOW: i32 = 300;
const LINE: i32 = 30;
const STAGE: i32 = TOP + WINDOW;
const BOTTOM: i32 = STAGE + LINE;
/// The glyphs' top within a line: a line drawn at most PAD pixels into the window shows nothing yet.
const PAD: i32 = 7;
/// The reading line, counted from the window's top: the text starts there.
const READ: i32 = 3;
/// The most a frame slides, under PAD.
const MAX_STEP: i32 = 6;
const STATUS: i32 = 420;

const SPEECH: &str = "Good morning, everyone, and thank you for coming.

A year ago we set out to make something small: a pair of glasses that shows you what you need and nothing more.

It turned out to be the hardest small thing any of us has built. Every pixel costs power, every word costs attention, and the screen sits a finger's width from your eye.

So we learned to say less. A time. A name. The next step of a recipe. A line of a speech, like this one, scrolling past while you look at the room instead of your notes.

Today I want to show you three things we found along the way.";

/// The text wrapped to the column: paragraphs with a blank line between them, typographic punctuation made
/// plain (the font is ASCII).
fn wrap(text: &str) -> Vec<String> {
    let plain: String = text
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            '\t' => ' ',
            c => c,
        })
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for paragraph in plain.replace("\r\n", "\n").split("\n\n") {
        let words: Vec<&str> = paragraph.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        let mut line = String::new();
        for word in words {
            let word: String = word.chars().take(CHARS).collect();
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > CHARS {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        lines.push(line);
    }
    lines
}

struct Prompter {
    lines: Vec<String>,
    /// Words on an average line, to turn words a minute into pixels a second.
    words_per_line: f64,
    wpm: f64,
    /// The next line not yet drawn.
    drawn: usize,
    /// Pixels the text has slid up, as drawn, and as far as the time says.
    shown: i32,
    position: f64,
    paused: bool,
}

impl Prompter {
    fn new(lines: Vec<String>, wpm: f64) -> Prompter {
        let words: usize = lines.iter().map(|l| l.split_whitespace().count()).sum();
        let full = lines.iter().filter(|l| !l.is_empty()).count().max(1);
        let words_per_line = (words as f64 / full as f64).max(1.0);
        Prompter { lines, words_per_line, wpm, drawn: 0, shown: 0, position: 0.0, paused: false }
    }

    /// Pixels a second at the present rate.
    fn speed(&self) -> f64 {
        self.wpm / 60.0 / self.words_per_line * LINE as f64
    }

    /// Where line `j`'s top is in the frame.
    fn top(&self, j: usize) -> i32 {
        TOP + (READ + j as i32) * LINE - self.shown
    }

    /// The lines that have come wholly into the rows the arms slide (below the window, unseen): drawn now.
    fn due(&mut self) -> Vec<(i32, String)> {
        let mut due = Vec::new();
        while self.drawn < self.lines.len() && self.top(self.drawn) <= STAGE {
            due.push((self.top(self.drawn), self.lines[self.drawn].clone()));
            self.drawn += 1;
        }
        due
    }

    /// The last line has reached the reading line.
    fn ended(&self) -> bool {
        self.top(self.lines.len().saturating_sub(1)) <= TOP + READ * LINE
    }

    /// How far through, percent.
    fn percent(&self) -> u32 {
        let total = (self.lines.len().saturating_sub(1) as i32 * LINE).max(1);
        (self.shown * 100 / total).clamp(0, 100) as u32
    }

    fn status(&self) -> String {
        let state = if self.ended() {
            "end - hold to restart"
        } else if self.paused {
            "paused"
        } else {
            "reading"
        };
        format!("{state:<21} {:>3} wpm {:>3}%", self.wpm.round() as u32, self.percent())
    }
}

/// The marks either side of the reading line, outside the column the arms slide.
fn marks(c: &mut Canvas) {
    let y = TOP + READ * LINE + LINE / 2;
    c.line(22, y - 7, 31, y, 10);
    c.line(31, y, 22, y + 7, 10);
    c.line(617, y - 7, 608, y, 10);
    c.line(608, y, 617, y + 7, 10);
}

fn status(c: &mut Canvas, text: &str) {
    c.clear(X, STATUS, W, 20);
    c.text(X, STATUS + 2, text, 7);
    c.show(X, STATUS, W, 20);
}

/// The whole screen from scratch: the marks, the lines due, the status.
fn first(c: &mut Canvas, lines: &[(i32, String)], text: &str) {
    c.clear_all();
    marks(c);
    for (top, line) in lines {
        c.text(X, top + PAD, line, 15);
    }
    c.text(X, STATUS + 2, text, 7);
    c.show(0, TOP, 640, STATUS + 20 - TOP);
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let mut wpm = 140.0;
        let mut file = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--wpm" => {
                    wpm = args.next().and_then(|v| v.parse().ok()).ok_or("--wpm takes words a minute")?;
                }
                path => file = Some(path.to_string()),
            }
        }
        let text = match (&file, g.preview()) {
            (Some(path), _) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
            (None, true) => SPEECH.to_string(),
            (None, false) => return Err(Error::from("usage: erisg1 v0 app prompter FILE [--wpm 140]")),
        };
        let lines = wrap(&text);
        if lines.is_empty() {
            return Err("nothing to read in the file".into());
        }
        let mut p = Prompter::new(lines.clone(), wpm);
        let due = p.due();
        let text = p.status();
        g.frame(|_, c| first(c, &due, &text)).await?;
        let mut shown_status = text;
        let mut last = g.now();
        while g.running() {
            let mut restart = false;
            for (_, gesture) in g.input().await? {
                match gesture {
                    Gesture::Tap => p.paused = !p.paused,
                    Gesture::DoubleTap => p.wpm = (p.wpm * 1.15).min(400.0),
                    Gesture::TripleTap => p.wpm = (p.wpm / 1.15).max(40.0),
                    Gesture::Hold => restart = true,
                    _ => {}
                }
            }
            if restart {
                let wpm = p.wpm;
                p = Prompter::new(lines.clone(), wpm);
                let due = p.due();
                let text = p.status();
                g.frame(|_, c| first(c, &due, &text)).await?;
                shown_status = text;
                last = g.now();
                continue;
            }
            let now = g.now();
            if !p.paused && !p.ended() {
                p.position += p.speed() * (now - last);
            }
            last = now;
            let step = (p.position.floor() as i32 - p.shown).clamp(0, MAX_STEP);
            p.shown += step;
            let due = p.due();
            let text = p.status();
            let new_status = text != shown_status;
            if step == 0 && !new_status {
                g.wait(0.03).await?;
                continue;
            }
            g.frame(|_, c| {
                if step > 0 {
                    c.blit(X, TOP + step, W, BOTTOM - TOP - step, [X, TOP]);
                    c.clear(X, BOTTOM - step, W, step);
                }
                for (top, line) in &due {
                    c.text(X, top + PAD, line, 15);
                }
                if step > 0 {
                    c.show(X, TOP, W, WINDOW);
                }
                if new_status {
                    status(c, &text);
                }
            })
            .await?;
            shown_status = text;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_paragraphs_to_the_column() {
        let lines = wrap("one two three\n\nfour \u{2014} five");
        assert_eq!(lines, ["one two three", "", "four - five"]);
        let long = wrap(&"word ".repeat(30));
        assert!(long.iter().all(|l| l.chars().count() <= CHARS));
        assert_eq!(long.len(), 5);
    }

    #[test]
    fn draws_lines_as_they_come_into_the_unseen_rows() {
        let mut p = Prompter::new((0..20).map(|i| format!("line {i}")).collect(), 140.0);
        // At the start, lines from the reading line down to the unseen rows.
        let first = p.due();
        assert_eq!(first.len(), (10 - READ + 1) as usize);
        assert_eq!(first[0].0, TOP + READ * LINE);
        assert!(first.iter().all(|(top, _)| *top <= STAGE));
        // Nothing new until a whole line has slid up; then the next, still unseen.
        p.shown = LINE - 1;
        assert!(p.due().is_empty());
        p.shown = LINE;
        let next = p.due();
        assert_eq!(next.len(), 1);
        assert!(next[0].0 + PAD >= STAGE - MAX_STEP);
    }

    #[test]
    fn ends_at_the_reading_line() {
        let mut p = Prompter::new(vec!["a".into(), "b".into(), "c".into()], 140.0);
        assert!(!p.ended());
        p.shown = 2 * LINE;
        assert!(p.ended());
        assert_eq!(p.percent(), 100);
    }
}
