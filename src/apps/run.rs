//! `run`: runs a command on the Mac and watches it from the glasses: `erisg1 v0 app run -- cargo build --release`.
//! The command line and the time it has taken sit at the top, its last 12 lines of output (stdout and stderr) below,
//! and when it ends, a big "done" or "failed" with the total time. Tap either TouchBar to switch between the output
//! and that summary. Ctrl-C stops the command too. `erisg1 v0 app run -- tail -f some.log` follows a log.
//!
//! New lines scroll the output up on the arm itself (`Canvas::blit`), so only they cross the link and are drawn.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

use super::kit::{Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;

pub(crate) const APP: App = App {
    name: "run",
    about: "runs a command and watches it: `erisg1 v0 app run -- CMD ARGS`; its output, time, done or failed",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &["--", "cargo", "build", "--release"],
};

fn demo() -> Script {
    Script::still(12.0).touch(4.6, Side::Right, 1).touch(6.2, Side::Left, 1)
}

/// Characters a line holds: 16 px each, inside 16 px margins.
const WIDTH: usize = 38;
/// Output lines shown, their first line's top, and their spacing.
const ROWS: usize = 12;
const TOP: i32 = 100;
const STEP: i32 = 22;
const HEADER: i32 = 68;
const STATUS: i32 = 378;
/// The output's band on the panel.
const BAND: [i32; 4] = [0, TOP, 640, ROWS as i32 * STEP];
/// The most often the output is redrawn, seconds.
const EVERY: f64 = 0.25;

/// A line of output, cleaned for the font: escape sequences gone, the last of any carriage-return rewrites kept,
/// tabs as spaces, control characters dropped.
fn clean(line: &str) -> String {
    let line = line.rsplit('\r').find(|part| !part.trim().is_empty()).unwrap_or("");
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // CSI: ESC [ parameters, then a final letter; other escapes are two characters.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() || c == '~' {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    // Kept whole (up to a point): cut where it's drawn.
    out.trim_end().chars().take(400).collect()
}

/// How bright a line of output is: errors brightest, warnings next.
fn level(line: &str) -> u8 {
    let lower = line.trim_start().to_ascii_lowercase();
    if lower.starts_with("error") || lower.contains("panicked") || lower.contains("failed") {
        15
    } else if lower.starts_with("warning") {
        12
    } else {
        9
    }
}

/// "0:42", "12:03", "1:02:03".
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// "4.2 s", "42 s", "3 min 12 s".
fn took(seconds: f64) -> String {
    if seconds < 10.0 {
        format!("{seconds:.1} s")
    } else if seconds < 60.0 {
        format!("{} s", seconds as u64)
    } else {
        format!("{} min {} s", seconds as u64 / 60, seconds as u64 % 60)
    }
}

fn cut(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(n - 3).collect();
        s.push_str("...");
        s
    }
}

enum Out {
    Line(String),
    Exit(Option<i32>),
}

/// The command, its output so far and whether it has ended.
struct Job {
    command: String,
    lines: VecDeque<String>,
    /// Every line seen, so the screen knows how far to scroll.
    total: usize,
    started: f64,
    ended: Option<(f64, Option<i32>)>,
}

impl Job {
    fn push(&mut self, line: String) {
        self.lines.push_back(line);
        self.total += 1;
        while self.lines.len() > ROWS {
            self.lines.pop_front();
        }
    }

    /// The line under the output, and its level.
    fn status(&self) -> (String, u8) {
        match self.ended {
            None => (format!("running, {} lines", self.total), 8),
            Some((at, Some(0))) => (format!("done in {}", took(at - self.started)), 15),
            Some((at, Some(code))) => (format!("failed (code {code}) after {}", took(at - self.started)), 15),
            Some((at, None)) => (format!("stopped after {}", took(at - self.started)), 15),
        }
    }
}

/// Starts the command with its output read on two threads into `out`; a third waits for it to end.
fn spawn(args: &[String], out: Sender<Out>) -> Result<Child, Error> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", args[0]))?;
    fn read(pipe: impl Read + Send + 'static, out: Sender<Out>) {
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).split(b'\n').map_while(Result::ok) {
                if out.send(Out::Line(clean(&String::from_utf8_lossy(&line)))).is_err() {
                    break;
                }
            }
        });
    }
    read(child.stdout.take().expect("piped"), out.clone());
    read(child.stderr.take().expect("piped"), out);
    Ok(child)
}

/// A made-up `cargo build`, for the preview: when each line appears, and when it ends.
const MADE_UP: [(f64, &str); 14] = [
    (0.3, "   Compiling proc-macro2 v1.0.89"),
    (0.8, "   Compiling unicode-ident v1.0.13"),
    (1.3, "   Compiling libc v0.2.161"),
    (1.9, "   Compiling quote v1.0.37"),
    (2.6, "   Compiling syn v2.0.85"),
    (3.2, "   Compiling serde v1.0.214"),
    (3.9, "   Compiling tokio v1.41.0"),
    (4.5, "   Compiling btleplug v0.11.6"),
    (5.2, "   Compiling erisg1 v0.1.0 (/Users/you/ErisG1)"),
    (6.0, "warning: unused variable: `frames`"),
    (6.1, "  --> src/apps/run.rs:212:9"),
    (6.2, "  = note: `#[warn(unused_variables)]` on by default"),
    (7.6, "warning: `erisg1` (bin \"erisg1\") generated 1 warning"),
    (8.4, "    Finished `release` profile [optimized] target(s) in 9.52s"),
];
const MADE_UP_END: f64 = 9.6;

#[derive(Clone, Copy, PartialEq)]
enum View {
    Output,
    Summary,
}

/// What the panel shows now, to redraw only what changed.
#[derive(Default)]
struct Shown {
    view: Option<View>,
    header: String,
    rows: Vec<String>,
    total: usize,
    status: String,
    summary: Vec<String>,
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let args: Vec<String> = args.iter().skip_while(|a| a.as_str() == "--").cloned().collect();
        if args.is_empty() {
            return Err("give a command: `erisg1 v0 app run -- CMD ARGS`".into());
        }
        let (tx, rx): (Sender<Out>, Receiver<Out>) = mpsc::channel();
        let mut child = if g.preview() { None } else { Some(spawn(&args, tx.clone())?) };
        let mut job = Job { command: args.join(" "), lines: VecDeque::new(), total: 0, started: g.now(), ended: None };
        let mut made_up = 0;
        let (mut view, mut shown, mut drawn_at) = (View::Output, Shown::default(), f64::NEG_INFINITY);
        while g.running() {
            // New output, and whether the command has ended.
            if g.preview() {
                while made_up < MADE_UP.len() && MADE_UP[made_up].0 <= g.now() - job.started {
                    let _ = tx.send(Out::Line(clean(MADE_UP[made_up].1)));
                    made_up += 1;
                }
                if job.ended.is_none() && g.now() - job.started >= MADE_UP_END {
                    let _ = tx.send(Out::Exit(Some(0)));
                }
            } else if let Some(c) = &mut child
                && let Ok(Some(status)) = c.try_wait()
            {
                let _ = tx.send(Out::Exit(status.code()));
                child = None;
            }
            while let Ok(out) = rx.try_recv() {
                match out {
                    Out::Line(line) => job.push(line),
                    Out::Exit(code) if job.ended.is_none() => {
                        job.ended = Some((g.now(), code));
                        view = View::Summary;
                    }
                    Out::Exit(_) => {}
                }
            }
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap {
                    view = if view == View::Output { View::Summary } else { View::Output };
                }
            }
            if g.now() - drawn_at >= EVERY || shown.view != Some(view) {
                draw(g, &job, view, &mut shown).await?;
                drawn_at = g.now();
            }
            g.wait(0.05).await?;
        }
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        Ok(())
    })
}

/// Redraws what changed since `shown`.
async fn draw(g: &mut Glasses, job: &Job, view: View, shown: &mut Shown) -> Result<(), Error> {
    let now = g.now();
    let elapsed = job.ended.map_or(now, |(at, _)| at) - job.started;
    let fresh = shown.view != Some(view);
    match view {
        View::Output => {
            let header = format!("$ {}", cut(&job.command, 28));
            let time = clock(elapsed);
            let header_line = format!("{header}|{time}");
            let rows: Vec<String> = job.lines.iter().map(|l| cut(l, WIDTH)).collect();
            let (status, status_level) = job.status();
            // The lines that came since the output was drawn: they fill it from the top, then scroll it up.
            let came = job.total - shown.total.min(job.total);
            let kept = shown.rows.len().min(ROWS.saturating_sub(came));
            let up = (shown.rows.len() + came).saturating_sub(ROWS);
            let redraw_rows = fresh || came >= ROWS || rows.len() < shown.rows.len();
            let rows_changed = redraw_rows || came > 0;
            if !fresh && !rows_changed && header_line == shown.header && status == shown.status {
                return Ok(());
            }
            let (old_header, old_status) = (shown.header.clone(), shown.status.clone());
            g.frame(|_, c| {
                if fresh {
                    c.clear(0, 56, 640, 344);
                    c.line(16, 92, 623, 92, 5);
                }
                if fresh || header_line != old_header {
                    c.clear(0, HEADER, 640, 16);
                    c.text(16, HEADER, &header, 12);
                    let w = c.width(&time);
                    c.text(624 - w, HEADER, &time, 9);
                    c.show(0, HEADER, 640, 16);
                }
                if rows_changed {
                    let [x, y, w, h] = BAND;
                    let first_new = if redraw_rows {
                        c.clear(x, y, w, h);
                        0
                    } else {
                        // The lines kept move up on the arm by those that fell off the top; then only the new
                        // ones are drawn.
                        if up > 0 {
                            let up = up as i32 * STEP;
                            c.blit(x, y + up, w, h - up, [x, y]);
                            c.clear(x, y + h - up, w, up);
                        }
                        kept
                    };
                    for (i, line) in rows.iter().enumerate().skip(first_new) {
                        c.text(16, TOP + i as i32 * STEP, line, level(line));
                    }
                    c.show(x, y, w, h);
                }
                if fresh || status != old_status {
                    c.clear(0, STATUS, 640, 16);
                    c.text(16, STATUS, &status, status_level);
                    c.show(0, STATUS, 640, 16);
                }
                if fresh {
                    c.show(0, 56, 640, 344);
                }
            })
            .await?;
            shown.header = header_line;
            shown.rows = rows;
            shown.total = job.total;
            shown.status = status;
            shown.summary.clear();
        }
        View::Summary => {
            let (headline, detail) = match job.ended {
                None => (clock(elapsed), "running".to_string()),
                Some((_, Some(0))) => ("DONE".to_string(), format!("in {}", took(elapsed))),
                Some((_, Some(code))) => ("FAILED".to_string(), format!("code {code} after {}", took(elapsed))),
                Some((_, None)) => ("STOPPED".to_string(), format!("after {}", took(elapsed))),
            };
            let last = job.lines.back().cloned().unwrap_or_default();
            let summary = vec![
                headline,
                detail,
                format!("$ {}", cut(&job.command, 34)),
                cut(last.trim(), 36),
                format!("{} lines of output", job.total),
            ];
            if !fresh && summary == shown.summary {
                return Ok(());
            }
            let old = shown.summary.clone();
            g.frame(|_, c| {
                if fresh {
                    c.clear(0, 56, 640, 344);
                    c.text_centred(320, STATUS, "tap: the output", 5);
                }
                let tops = [110, 176, 226, 256, 290];
                for (i, (line, top)) in summary.iter().zip(tops).enumerate() {
                    if !fresh && old.get(i) == Some(line) {
                        continue;
                    }
                    let tall = if i == 0 { 40 } else { 16 };
                    c.clear(0, top, 640, tall);
                    if i == 0 {
                        c.big_centred(320, top, line, 15);
                    } else {
                        c.text_centred(320, top, line, [0, 13, 10, 8, 6][i]);
                    }
                    c.show(0, top, 640, tall);
                }
                if fresh {
                    c.show(0, 56, 640, 344);
                }
            })
            .await?;
            shown.summary = summary;
            shown.rows.clear();
            shown.total = 0;
            shown.header.clear();
            shown.status.clear();
        }
    }
    shown.view = Some(view);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_output_lines() {
        assert_eq!(clean("\x1b[1m\x1b[32m   Compiling\x1b[0m foo"), "   Compiling foo");
        assert_eq!(clean("10%\r50%\r100%"), "100%");
        assert_eq!(clean("a\tb"), "a    b");
        assert_eq!(clean(&"x".repeat(800)).len(), 400);
    }

    #[test]
    fn levels_and_times() {
        assert_eq!(level("error[E0308]: mismatched types"), 15);
        assert_eq!(level("warning: unused"), 12);
        assert_eq!(level("   Compiling x"), 9);
        assert_eq!(clock(62.0), "1:02");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(took(4.21), "4.2 s");
        assert_eq!(took(192.0), "3 min 12 s");
        assert_eq!(cut("abcdefghij", 6), "abc...");
    }

    #[test]
    fn runs_a_command_and_reads_both_outputs() {
        let (tx, rx) = mpsc::channel();
        let args: Vec<String> = ["sh", "-c", "echo out; echo err >&2; exit 3"].map(String::from).to_vec();
        let mut child = spawn(&args, tx).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(3));
        let mut lines: Vec<String> = Vec::new();
        while let Ok(Out::Line(l)) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            lines.push(l);
            if lines.len() == 2 {
                break;
            }
        }
        lines.sort();
        assert_eq!(lines, ["err", "out"]);
    }

    #[test]
    fn keeps_the_last_lines() {
        let mut job = Job { command: "x".into(), lines: VecDeque::new(), total: 0, started: 0.0, ended: None };
        for i in 0..20 {
            job.push(i.to_string());
        }
        assert_eq!(job.lines.len(), ROWS);
        assert_eq!(job.lines.front().map(String::as_str), Some("8"));
        assert_eq!(job.total, 20);
    }
}
