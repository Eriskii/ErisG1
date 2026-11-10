//! `nowplaying`: the track playing in Music or Spotify on the Mac (whichever plays; Spotify first if both): its
//! title, artist and album, a progress bar with the times, and whether it's paused. The TouchBars are the remote:
//! tap to play or pause, double tap for the next track, triple tap for the previous one.
//!
//! The player is asked about once a second with `osascript` on a thread of its own (only an app already running is
//! asked, so none is started); the bar moves on between answers.

use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};

use super::kit::{Canvas, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;

pub(crate) const APP: App = App {
    name: "nowplaying",
    about: "the track playing in Music or Spotify, with progress; tap play/pause, 2 taps next, 3 taps back",
    fun: false,
    options: Options { font: 2, big: 3, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0).touch(2.5, Side::Right, 1).touch(4.5, Side::Left, 1).touch(6.5, Side::Right, 2).touch(
        9.5,
        Side::Right,
        3,
    )
}

#[derive(Clone, Debug, PartialEq)]
struct Track {
    /// "Music" or "Spotify".
    app: String,
    playing: bool,
    name: String,
    artist: String,
    album: String,
    /// Seconds.
    duration: f64,
    position: f64,
}

/// What the player said: a track, or nothing playing.
type Answer = Option<Track>;

/// Fields are joined with the unit separator, which no title holds.
const SEP: char = '\u{1f}';

/// The AppleScript asking `app` ("Music" or "Spotify") for its state, the track and the position.
fn script(app: &str) -> String {
    // Spotify gives the duration in ms, Music in seconds.
    let duration = if app == "Spotify" { "((duration of t) div 1000)" } else { "((duration of t) as integer)" };
    format!(
        "tell application \"{app}\"\n\
         set s to player state as text\n\
         if s is \"stopped\" then return \"stopped\"\n\
         set t to current track\n\
         set d to character id 31\n\
         return s & d & (name of t) & d & (artist of t) & d & (album of t) & d & {duration} & d & ((player position) as integer)\n\
         end tell"
    )
}

/// An answer from `script`.
fn parse(app: &str, text: &str) -> Answer {
    let fields: Vec<&str> = text.trim().split(SEP).collect();
    if fields.len() < 6 {
        return None;
    }
    Some(Track {
        app: app.to_string(),
        playing: fields[0] == "playing",
        name: fields[1].to_string(),
        artist: fields[2].to_string(),
        album: fields[3].to_string(),
        duration: fields[4].trim().parse().unwrap_or(0.0),
        position: fields[5].trim().parse().unwrap_or(0.0),
    })
}

fn running(app: &str) -> bool {
    Command::new("pgrep").args(["-x", app]).output().is_ok_and(|o| o.status.success())
}

/// Asks Spotify and Music (those running) what they play: the one playing first, else one paused.
fn ask() -> Answer {
    let mut answers: Vec<Track> = ["Spotify", "Music"]
        .into_iter()
        .filter(|app| running(app))
        .filter_map(|app| {
            let out = Command::new("osascript").args(["-e", &script(app)]).output().ok()?;
            parse(app, &String::from_utf8_lossy(&out.stdout))
        })
        .collect();
    answers.sort_by_key(|t| !t.playing);
    answers.into_iter().next()
}

/// Asks about once a second on a thread of its own.
fn poll(out: Sender<Answer>) {
    std::thread::spawn(move || {
        loop {
            if out.send(ask()).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

/// Tells `app` to `command` ("playpause", "next track", "previous track"), without waiting.
fn tell(app: &str, command: &str) {
    let script = format!("tell application \"{app}\" to {command}");
    std::thread::spawn(move || {
        let _ = Command::new("osascript").args(["-e", &script]).output();
    });
}

/// Two made-up albums' worth, for the preview.
const MADE_UP: [(&str, &str, &str, f64); 3] = [
    ("Paper Lanterns", "Mira Sol", "Tidelines", 214.0),
    ("Midnight Static", "The Low Hum", "Night Drive", 222.0),
    ("Glass Harbour", "Mira Sol", "Tidelines", 198.0),
];

/// "1:23".
fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
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

const HINT: &str = "tap play/pause, 2 taps next, 3 back";
/// The progress bar: x, y, w, h.
const BAR: [i32; 4] = [40, 252, 560, 14];
const TIMES: i32 = 276;

/// What the panel shows, to redraw only what changed.
#[derive(Default, PartialEq)]
struct Shown {
    head: Option<(String, String, String, String)>,
    state: String,
    filled: i32,
    times: (String, String),
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let (tx, rx): (Sender<Answer>, Receiver<Answer>) = mpsc::channel();
        let mut track: Answer = None;
        // When `track`'s position was given.
        let mut since = g.now();
        let mut made_up = 1;
        if g.preview() {
            let (name, artist, album, duration) = MADE_UP[made_up];
            track = Some(Track {
                app: "Spotify".into(),
                playing: true,
                name: name.into(),
                artist: artist.into(),
                album: album.into(),
                duration,
                position: 71.0,
            });
        } else {
            poll(tx);
        }
        let (mut shown, mut idle_since) = (Shown::default(), None::<f64>);
        while g.running() {
            while let Ok(answer) = rx.try_recv() {
                track = answer;
                since = g.now();
            }
            for (_, gesture) in g.input().await? {
                let Some(t) = &mut track else { continue };
                let elapsed = if t.playing { g.now() - since } else { 0.0 };
                let command = match gesture {
                    Gesture::Tap => "playpause",
                    Gesture::DoubleTap => "next track",
                    Gesture::TripleTap => "previous track",
                    _ => continue,
                };
                if g.preview() {
                    // The made-up player does it at once.
                    t.position = (t.position + elapsed).min(t.duration);
                    let step = match gesture {
                        Gesture::Tap => {
                            t.playing = !t.playing;
                            0
                        }
                        Gesture::DoubleTap => 1,
                        _ => MADE_UP.len() - 1,
                    };
                    if step > 0 {
                        made_up = (made_up + step) % MADE_UP.len();
                        let (name, artist, album, duration) = MADE_UP[made_up];
                        (t.name, t.artist, t.album, t.duration, t.position) =
                            (name.into(), artist.into(), album.into(), duration, 0.0);
                        t.playing = true;
                    }
                    since = g.now();
                } else {
                    tell(&t.app, command);
                }
            }
            match &track {
                Some(t) => {
                    idle_since = None;
                    let position = (t.position + if t.playing { g.now() - since } else { 0.0 }).min(t.duration);
                    draw(g, t, position, &mut shown).await?;
                }
                None => {
                    if shown.head.is_some() || idle_since.is_none() {
                        g.frame(|_, c| {
                            c.clear(0, 56, 640, 344);
                            c.text_centred(320, 200, "nothing playing", 8);
                            c.show(0, 56, 640, 344);
                        })
                        .await?;
                        shown = Shown::default();
                        idle_since = Some(g.now());
                    }
                    // A while with nothing to show: the panel sleeps.
                    if idle_since.is_some_and(|at| g.now() - at > 30.0) {
                        g.sleep_panel().await?;
                        idle_since = Some(f64::INFINITY);
                    }
                }
            }
            g.wait(0.25).await?;
        }
        Ok(())
    })
}

fn draw_head(c: &mut Canvas, t: &Track) {
    c.clear(0, 56, 640, 344);
    c.text(16, 66, &t.app, 8);
    c.big(16, 112, &cut(&t.name, 25), 15);
    c.text(16, 160, &cut(&t.artist, 38), 12);
    c.text(16, 188, &cut(&t.album, 38), 7);
    let [x, y, w, h] = BAR;
    c.rect(x, y, w, h, 7);
    c.text_centred(320, 372, HINT, 5);
    c.show(0, 56, 640, 344);
}

async fn draw(g: &mut Glasses, t: &Track, position: f64, shown: &mut Shown) -> Result<(), Error> {
    let head = Some((t.app.clone(), t.name.clone(), t.artist.clone(), t.album.clone()));
    let state = if t.playing { "playing" } else { "paused" }.to_string();
    let [x, y, w, h] = BAR;
    let inner = w - 4;
    let filled = if t.duration > 0.0 { (inner as f64 * position / t.duration).round() as i32 } else { 0 };
    let times = (mmss(position), mmss(t.duration));
    let fresh = shown.head != head;
    if !fresh && shown.state == state && shown.filled == filled && shown.times == times {
        return Ok(());
    }
    let old_state = shown.state.clone();
    let (old_filled, old_times) = (if fresh { -1 } else { shown.filled }, shown.times.clone());
    g.frame(|_, c| {
        if fresh {
            draw_head(c, t);
        }
        if fresh || state != old_state {
            c.clear(480, 66, 144, 16);
            let tw = c.width(&state);
            c.text(624 - tw, 66, &state, if t.playing { 12 } else { 15 });
            c.show(480, 66, 144, 16);
        }
        if filled != old_filled {
            c.clear(x + 2, y + 2, inner, h - 4);
            c.fill(x + 2, y + 2, filled, h - 4, if t.playing { 13 } else { 8 });
            c.show(x, y, w, h);
        }
        if fresh || times != old_times {
            c.clear(x, TIMES, w, 16);
            c.text(x, TIMES, &times.0, 11);
            let tw = c.width(&times.1);
            c.text(x + w - tw, TIMES, &times.1, 8);
            c.show(x, TIMES, w, 16);
        }
    })
    .await?;
    *shown = Shown { head, state, filled, times };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_answer() {
        let text = format!("playing{SEP}So What{SEP}Miles Davis{SEP}Kind of Blue{SEP}562{SEP}71\n");
        let t = parse("Music", &text).unwrap();
        assert!(t.playing);
        assert_eq!((t.name.as_str(), t.duration, t.position), ("So What", 562.0, 71.0));
        assert_eq!(parse("Music", "stopped"), None);
        assert!(script("Spotify").contains("div 1000"));
    }

    #[test]
    fn formats() {
        assert_eq!(mmss(222.4), "3:42");
        assert_eq!(cut("abcdefghijklmnop", 8), "abcde...");
        assert!(HINT.len() <= 38);
    }
}
