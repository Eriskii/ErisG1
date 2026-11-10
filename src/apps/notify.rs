//! `notify`: notifications on the glasses from anything on the Mac. It listens on a socket; `erisg1 v0 say [TITLE:]
//! TEXT` (or any program writing a line to the socket) pops a card up in both eyes, about 2 m away, for 8 s. Tap to
//! dismiss it, double tap to see the last one again. With nothing to show the panel sleeps.
//!
//! Handy at the end of a long command (`cargo build; erisg1 v0 say "build done"`), or from a script or an agent.

use std::collections::VecDeque;
use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::kit::{Canvas, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;

pub(crate) const APP: App = App {
    name: "notify",
    about: "pop-up notifications from anything on the Mac: `erisg1 v0 say [TITLE:] TEXT`; tap dismisses",
    fun: false,
    options: Options { font: 2, big: 3, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(10.0).touch(4.0, Side::Right, 1).touch(8.6, Side::Left, 2)
}

/// Where the socket is.
pub(crate) fn socket() -> PathBuf {
    std::env::temp_dir().join("erisg1-notify.sock")
}

/// Sends a notification to a running `notify`.
pub(crate) async fn say(text: &str) -> Result<(), Error> {
    let mut stream =
        UnixStream::connect(socket()).await.map_err(|e| format!("no `erisg1 v0 app notify` running ({e})"))?;
    let line = text.replace('\n', " ");
    stream.write_all(format!("{line}\n").as_bytes()).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// How long a card stays up, seconds.
const SHOWN: f64 = 8.0;
const CARD: [i32; 4] = [60, 130, 520, 220];
const DEPTH: f64 = 2.0;
/// Characters a body line holds (16 px each, inside the card).
const LINE: usize = 30;
const LINES: usize = 6;

struct Note {
    title: String,
    body: Vec<String>,
}

impl Note {
    /// "TITLE: TEXT", or just the text under the title "Note".
    fn parse(line: &str) -> Note {
        let (title, text) = match line.split_once(": ") {
            Some((t, rest)) if t.len() <= 20 => (t.trim().to_string(), rest.trim()),
            _ => ("Note".to_string(), line.trim()),
        };
        Note { title, body: wrap(text, LINE, LINES) }
    }
}

/// Words into lines of at most `width` characters, at most `lines` of them.
fn wrap(text: &str, width: usize, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word: String = word.chars().take(width).collect();
        match out.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(&word);
            }
            _ => out.push(word),
        }
    }
    if out.len() > lines {
        out.truncate(lines);
        if let Some(last) = out.last_mut() {
            let mut cut: String = last.chars().take(width - 3).collect();
            cut.push_str("...");
            *last = cut;
        }
    }
    out
}

fn card(eye: &super::kit::Eye, c: &mut Canvas, note: Option<&Note>) {
    let [x, y, w, h] = CARD;
    let shift = eye.depth(DEPTH);
    // Clear a little wider than the card: the eyes' copies sit apart.
    c.clear(x - 40, y - 4, w + 80, h + 8);
    if let Some(note) = note {
        let x = x + shift;
        // As tall as its text, centred where the tallest would be.
        let tall = 68 + 26 * note.body.len().max(1) as i32;
        let y = y + (h - tall) / 2;
        c.rect(x, y, w, tall, 12);
        c.line(x + 1, y + 44, x + w - 2, y + 44, 6);
        c.big(x + 16, y + 10, &note.title.chars().take(20).collect::<String>(), 15);
        for (i, line) in note.body.iter().enumerate() {
            c.text(x + 16, y + 56 + 26 * i as i32, line, 13);
        }
    }
    c.show(CARD[0] - 40, CARD[1] - 4, CARD[2] + 80, CARD[3] + 8);
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let mut queue: VecDeque<Note> = VecDeque::new();
        // In the preview, two notifications come in.
        let mut made_up = if g.preview() {
            vec![
                (5.0, "Calendar: Standup in 5 minutes, room 4B"),
                (0.5, "cargo build: finished in 42.1 s, 0 errors, 3 warnings"),
            ]
        } else {
            Vec::new()
        };
        let listener = if g.preview() {
            None
        } else {
            let path = socket();
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("listening on {}; `erisg1 v0 say TEXT` to show something. Ctrl-C to stop", path.display());
            Some(listener)
        };
        let (mut showing, mut last): (Option<(Note, f64)>, Option<Note>) = (None, None);
        g.sleep_panel().await?;
        while g.running() {
            if let Some(listener) = &listener {
                while let Ok(Ok((stream, _))) =
                    tokio::time::timeout(std::time::Duration::from_millis(1), listener.accept()).await
                {
                    let mut lines = BufReader::new(stream).lines();
                    if let Ok(Ok(Some(line))) =
                        tokio::time::timeout(std::time::Duration::from_millis(200), lines.next_line()).await
                    {
                        queue.push_back(Note::parse(&line));
                    }
                }
            }
            while made_up.last().is_some_and(|(at, _)| *at <= g.now()) {
                queue.push_back(Note::parse(made_up.pop().unwrap().1));
            }
            let mut dismiss = showing.as_ref().is_some_and(|(_, since)| g.now() - since > SHOWN);
            for (_, gesture) in g.input().await? {
                match gesture {
                    Gesture::Tap => dismiss |= showing.is_some(),
                    Gesture::DoubleTap if showing.is_none() => {
                        if let Some(note) = last.take() {
                            queue.push_front(note);
                        }
                    }
                    _ => {}
                }
            }
            if dismiss {
                last = showing.take().map(|(note, _)| note);
                g.frame(|eye, c| card(eye, c, None)).await?;
                if queue.is_empty() {
                    g.sleep_panel().await?;
                }
            }
            if showing.is_none()
                && let Some(note) = queue.pop_front()
            {
                g.frame(|eye, c| card(eye, c, Some(&note))).await?;
                showing = Some((note, g.now()));
            }
            g.wait(0.05).await?;
        }
        if listener.is_some() {
            let _ = std::fs::remove_file(socket());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_cuts() {
        let lines = wrap("one two three four five six seven", 9, 2);
        assert_eq!(lines, ["one two", "three..."]);
        let note = Note::parse("Build: done in 3 s");
        assert_eq!((note.title.as_str(), note.body[0].as_str()), ("Build", "done in 3 s"));
        assert_eq!(Note::parse("plain text").title, "Note");
    }
}
