//! `note`: notes pinned to the glasses, which then keep them alone. `erisg1 v0 app note "Gate code 4821" "Wifi:
//! hunter2 ..."` uploads up to eight notes, wrapped here, and leaves. The panel stays off until a tap on either
//! TouchBar: then the first note shows for 10 s; a tap while it shows moves to the next; hold hides it.
//!
//! The gestures go through `vmkit`'s shared events, so both arms act on a tap at the same moment and the eyes
//! match.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;
use crate::v5::{prop, trigger};
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "note",
    about: "notes kept on the glasses alone (\"gate code 4821\"): tap shows the next, hold hides",
    fun: false,
    options: Options { font: 2, big: 3, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[
        "Gate code 4821",
        "Wifi: hunter2-guest. The password is also on the fridge.",
        "Parking: level 3, row F",
    ],
};

fn demo() -> Script {
    Script::still(12.0)
        .touch(1.0, Side::Right, 1)
        .touch(3.5, Side::Left, 1)
        .touch(6.0, Side::Right, 1)
        .touch(8.0, Side::Left, 4)
        .touch(9.5, Side::Right, 1)
}

const MAX_NOTES: usize = 8;
/// Characters a line holds and lines a note keeps: in the 24 px font if it fits, else the 16 px one.
const BIG_WIDTH: usize = 19;
const BIG_LINES: usize = 4;
const WIDTH: usize = 30;
const LINES: usize = 7;
/// How long a note stays up after a tap, µs.
const SHOWN_US: u32 = 10_000_000;
/// The card: its outline, and the part of the frame that goes to the panel (with the hint under it).
const CARD: [i32; 4] = [60, 100, 520, 260];
const SHOWN: [i32; 4] = [56, 96, 528, 300];
const HINT: &str = "tap: next note   hold: hide";

/// Words into lines of at most `width` characters, at most `lines` of them, the last cut short with "..." if need
/// be; and whether anything was cut.
fn wrap(text: &str, width: usize, lines: usize) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut cut = false;
    for word in text.split_whitespace() {
        cut |= word.chars().count() > width;
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
        cut = true;
        out.truncate(lines);
        let last = out.last_mut().unwrap();
        *last = format!("{}...", last.chars().take(width - 3).collect::<String>());
    }
    (out, cut)
}

/// A note laid out: big if it fits, and its lines.
struct Note {
    big: bool,
    lines: Vec<String>,
}

fn layout(text: &str) -> Note {
    match wrap(text, BIG_WIDTH, BIG_LINES) {
        (lines, false) => Note { big: true, lines },
        _ => Note { big: false, lines: wrap(text, WIDTH, LINES).0 },
    }
}

fn program(side: Side, notes: &[Note]) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let n = notes.len() as u32;
    let [zero, one, two, four, shown_us, count] = [0, 1, 2, 4, SHOWN_US, n].map(|v| p.k(v));
    let (visible, current, hide_at) = (p.var(0), p.var(0), p.var(0));
    // Each note's header ("NOTE 2/3") and lines, as text buffers: (address, characters).
    let texts: Vec<Vec<(u32, u32)>> = notes
        .iter()
        .enumerate()
        .map(|(i, note)| {
            let header = format!("NOTE {}/{n}", i + 1);
            let mut texts = vec![(p.text(&header), header.len() as u32)];
            texts.extend(note.lines.iter().map(|l| (p.text(l), l.chars().count() as u32)));
            texts
        })
        .collect();
    let hint = p.text(HINT);

    p.on(trigger::START, "start");
    p.on(trigger::TIMER, "tick");
    p.events("event");

    // START: hidden until a tap.
    p.label("start");
    p.set(prop::PANEL, two);
    p.op(END, &[]);

    // TIMER at hide_at: hide, unless a tap since moved it on.
    p.label("tick");
    p.branch(BEQ, visible, zero, "tick_end");
    p.read(R0, prop::TIME_US);
    p.op(SUB, &[R0, R0, hide_at]);
    p.branch(BLTS, R0, zero, "tick_early");
    p.jump(CALL, "hide");
    p.jump(JMP, "tick_end");
    p.label("tick_early");
    p.timer(hide_at);
    p.label("tick_end");
    p.op(END, &[]);

    p.label("hide");
    p.op(MOV, &[visible, zero]);
    p.set(prop::PANEL, two);
    p.op(RET, &[]);

    // A gesture (r0) at the moment both arms share (r1).
    p.label("event");
    p.branch(BEQ, R0, one, "tap");
    p.branch(BEQ, R0, four, "hold");
    p.op(RET, &[]);
    p.label("tap");
    p.branch(BEQ, visible, zero, "first");
    p.op(ADD, &[current, current, one]);
    p.branch(BLTU, current, count, "show");
    p.label("first");
    p.op(MOV, &[current, zero]);
    p.label("show");
    p.op(MOV, &[visible, one]);
    p.op(ADD, &[hide_at, R1, shown_us]);
    p.timer(hide_at);
    p.jump(CALL, "draw");
    p.op(RET, &[]);
    p.label("hold");
    p.branch(BEQ, visible, zero, "hold_end");
    p.jump(CALL, "hide");
    p.label("hold_end");
    p.op(RET, &[]);

    // The current note on its card; the panel wakes at the DRAW.
    let [x, y, w, h] = CARD;
    p.label("draw");
    let [sx, sy, sw, sh] = SHOWN;
    p.fill_k(sx, sy, sw, sh, 0);
    p.line_k(x, y, x + w - 1, y, 12);
    p.line_k(x + w - 1, y, x + w - 1, y + h - 1, 12);
    p.line_k(x + w - 1, y + h - 1, x, y + h - 1, 12);
    p.line_k(x, y + h - 1, x, y, 12);
    p.line_k(x + 1, y + 40, x + w - 2, y + 40, 6);
    for (i, (texts, note)) in texts.iter().zip(notes).enumerate() {
        let skip = p.fresh("note");
        let k = p.k(i as u32);
        p.branch(BNE, current, k, skip);
        let ((header, header_len), lines) = (texts[0], &texts[1..]);
        p.text_k(Font::Main, x + 20, y + 14, header, 0, header_len, 8);
        // The lines in the middle of the space under the header.
        let (font, step) = if note.big { (Font::Big, 34) } else { (Font::Main, 28) };
        let tall = step * (lines.len() as i32 - 1) + p.size(font);
        let top = y + 44 + (h - 54 - tall) / 2;
        for (j, &(at, len)) in lines.iter().enumerate() {
            p.text_k(font, x + 20, top + step * j as i32, at, 0, len, 15);
        }
        p.label(skip);
    }
    p.text_k(Font::Main, 320 - 8 * HINT.len() as i32, y + h + 16, hint, 0, HINT.len() as u32, 6);
    p.draw_k(sx, sy, sw, sh);
    p.brightness(APP.options.boost);
    p.op(RET, &[]);
    p.finish()
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let notes: Vec<Note> = args.iter().map(|a| layout(a)).filter(|n| !n.lines.is_empty()).collect();
        if notes.is_empty() {
            return Err(Error::from("usage: erisg1 v0 app note \"first note\" [\"second note\" ...]"));
        }
        if notes.len() > MAX_NOTES {
            return Err(format!("at most {MAX_NOTES} notes").into());
        }
        g.deploy(|side| program(side, &notes)).await?;
        if !g.preview() {
            println!("{} notes on the glasses; tap a TouchBar to see them", notes.len());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_notes_are_big_long_ones_small_and_cut() {
        let short = layout("Gate code 4821");
        assert!(short.big);
        assert_eq!(short.lines, ["Gate code 4821"]);
        let long = layout(&"word ".repeat(80));
        assert!(!long.big);
        assert_eq!(long.lines.len(), LINES);
        assert!(long.lines.last().unwrap().ends_with("..."));
        assert!(long.lines.iter().all(|l| l.chars().count() <= WIDTH));
    }

    #[test]
    fn builds_for_eight_long_notes() {
        let notes: Vec<Note> = (0..MAX_NOTES).map(|i| layout(&"word ".repeat(4 + 20 * (i % 2)))).collect();
        for side in [Side::Left, Side::Right] {
            program(side, &notes).unwrap();
        }
    }
}
