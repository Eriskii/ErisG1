//! `erisg1 v0 flight`: a mock flight HUD with your head as the controls. Tilting it up climbs and banking it turns a
//! simulated aircraft, whose heading, speed and altitude the HUD shows, around a pitch ladder that follows your head.
//! The G reading is your head's real acceleration.
//!
//! The arms draw it themselves: a rendering routine sits in each arm's memory, built with `v5_asm`, and each frame
//! the Mac sends only the state (about 110 bytes, one packet), then has both arms run the routine and show the
//! result at the same moment.

use std::time::Duration;

use crate::link::{self, Error};
use crate::proto::Side;
use crate::v5::{self, prop};
use crate::v5_asm::*;
use crate::v5_link;

// Where things sit in the arena, above the frame and the 16-pixel font.
const POOL: u32 = 0x30000;
/// Written every frame: STATE_WORDS words, then the text slots from TEXTS.
const STATE: u32 = 0x30400;
const TEXTS: u32 = STATE + 0x40;
const STATE_LEN: usize = 0x6C;
/// The pitch ladder's segments: x0, y0, x1, y1 as u16 plus OFF (loads don't sign-extend).
const LADDER: u32 = 0x30500;
/// The ladder's labels: x, y (plus OFF), then where its glyphs are from LABEL_TEXT and how many, as u16.
const LABELS: u32 = 0x30A00;
const LABEL_TEXT: u32 = 0x30C00;
const HDG_DIGITS: u32 = 0x30D00;
const CODE: u32 = 0x30E00;
/// A program that does nothing, which keeps the right arm's light sensor reading for the brightness.
const IDLE: u32 = 0x3F000;
const OFF: i32 = 4096;

// The state words, by index.
/// The ladder's transform: its centre x and y, then cos and sin of its turn, ×1024.
const S_CX: u32 = 0;
/// The ladder's visible part: its first segment and how many, then its first label and how many.
const S_LADDER_FIRST: u32 = 4;
const S_HEADING: u32 = 8;
/// The ladder's rectangle (x, y, w, h), which the Mac works out to cover where the ladder is and was.
const S_LADDER_RECT: u32 = 9;
/// Where the routines note the arm's clock: when rendering starts and ends, and when the panel has the frame.
const TIMES: u32 = STATE + 0x70;
/// The routines' own words: the transform `xf` applies and the level the ladder is drawn at, then the last frame's
/// ladder (its four state words and its ranges), to erase it by drawing it again in black. The host zeroes them.
const WORK: u32 = STATE + 0xA0;
const W_XF: u32 = 0;
const W_LEVEL: u32 = 4;
const W_LAST: u32 = 5;
const WORK_WORDS: u32 = 13;
/// The parts of the panel that always have something in them: the heading tape, the speed and altitude boxes and
/// the bottom line. With the ladder's rectangle, they're all that's cleared and copied to the panel each frame.
const REGIONS: [[u32; 4]; 4] = [[100, 6, 440, 82], [20, 190, 96, 56], [518, 190, 112, 56], [90, 450, 460, 22]];
/// Around the aircraft symbol, which sits inside the ladder's rectangle.
const SYMBOL: [i32; 4] = [268, 234, 372, 256];
// The text slots: offset from TEXTS, and length.
const T_HEADING: (u32, usize) = (0, 3);
const T_SPEED: (u32, usize) = (4, 4);
const T_ALTITUDE: (u32, usize) = (8, 5);
const T_BOTTOM: (u32, usize) = (16, 28);

const SCALE: u8 = 2;
/// Pixels per degree of pitch on the ladder, and per degree of heading on the tape.
const LADDER_PX: f64 = 8.0;
const HEADING_PX: i32 = 4;
/// Ladder rungs within this many degrees of the current pitch are drawn.
const LADDER_SPAN: f64 = 16.0;
const PERIOD: Duration = Duration::from_millis(50);
const QUEUE: usize = 4;

fn state_word(i: u32) -> u16 {
    ((STATE / 4) + i) as u16
}

/// One rung of the ladder: its pitch, segments in ladder space (pixels, y down, origin on the horizon at the
/// middle), and labels (anchor, text).
struct Rung {
    pitch: i32,
    segments: Vec<[i32; 4]>,
    labels: Vec<([i32; 2], String)>,
}

/// The ladder, from −90° to 90°: a rung every 5°, long and labelled every 10°, with ticks toward the horizon.
fn ladder() -> Vec<Rung> {
    (-18..=18)
        .map(|i| {
            let pitch: i32 = i * 5;
            let y = -(pitch as f64 * LADDER_PX) as i32;
            let (segments, labels) = if pitch == 0 {
                (vec![[-200, 0, -50, 0], [50, 0, 200, 0]], vec![])
            } else if pitch % 10 == 0 {
                let tick = if pitch > 0 { 10 } else { -10 };
                let text = pitch.abs().to_string();
                (
                    vec![[-130, y, -50, y], [50, y, 130, y], [-130, y, -130, y + tick], [130, y, 130, y + tick]],
                    vec![([-160, y], text.clone()), ([160, y], text)],
                )
            } else {
                (vec![[-90, y, -50, y], [50, y, 90, y]], vec![])
            };
            Rung { pitch, segments, labels }
        })
        .collect()
}

/// The routines each arm keeps and the host calls every frame: at CODE, clear the frame's regions and draw the whole
/// HUD from the state; after it, `show`, which copies the regions to the panel. Returns the code, its constant pool
/// and `show`'s address.
fn render_code(
    static_text: u32,
    spd_label: (u32, usize),
    alt_label: (u32, usize),
) -> Result<(Vec<u8>, Vec<u8>, u32), String> {
    const R0: u16 = 0;
    const R1: u16 = 1;
    const R2: u16 = 2;
    const R3: u16 = 3;
    const R4: u16 = 4;
    const R5: u16 = 5;
    const R6: u16 = 6;
    const R7: u16 = 7;
    const R8: u16 = 8;
    const R9: u16 = 9;
    const R10: u16 = 10;
    const R11: u16 = 11;
    const R12: u16 = 12;
    let mut a = Asm::new(CODE, POOL);
    let (frame, stride, font, mask) = (a.k(v5::FRAME), a.k(v5::STRIDE), a.k(v5::FONT), a.k(2));
    let (zero, one, two, three, eight, bright, dim) = (a.k(0), a.k(1), a.k(2), a.k(3), a.k(8), a.k(15), a.k(9));
    let off = a.k(OFF as u32);

    let time = a.k(0); // TIME_US
    let times = |i: u32| ((TIMES / 4) + i) as u16;
    let rect = |i: u32| state_word(S_LADDER_RECT + i);
    a.label("render");
    a.op(SYS_READ, &[times(0), time]);
    for [x, y, w, h] in REGIONS {
        let (x, y, w, h) = (a.k(x), a.k(y), a.k(w), a.k(h));
        a.op(FILL, &[frame, stride, x, y, w, h, zero]);
    }
    a.op(SYS_READ, &[times(3), time]);
    // The last frame's ladder in black, cheaper than clearing its whole rectangle, then this frame's. Both go
    // through the transform and level in WORK.
    let work = |i: u32| ((WORK / 4) + i) as u16;
    for (from, level) in [(None, zero), (Some(()), bright)] {
        // Its centre, cos and sin, then its ladder and label ranges.
        let source = |i: u32| if from.is_none() { work(W_LAST + i) } else { state_word(S_CX + i) };
        let range = |i: u32| if from.is_none() { work(W_LAST + 4 + i) } else { state_word(S_LADDER_FIRST + i) };
        for i in 0..4 {
            a.op(MOV, &[work(W_XF + i), source(i)]);
        }
        a.op(MOV, &[work(W_LEVEL), level]);
        a.op(MOV, &[R0, range(0)]);
        a.op(MOV, &[R1, range(1)]);
        a.jump(CALL, "ladder");
        a.op(MOV, &[R0, range(2)]);
        a.op(MOV, &[R1, range(3)]);
        a.jump(CALL, "labels");
    }
    for i in 0..4 {
        a.op(MOV, &[work(W_LAST + i), state_word(S_CX + i)]);
        a.op(MOV, &[work(W_LAST + 4 + i), state_word(S_LADDER_FIRST + i)]);
    }
    a.op(SYS_READ, &[times(4), time]);
    a.jump(CALL, "heading");
    a.op(SYS_READ, &[times(5), time]);
    // Fixed lines: the aircraft symbol, the heading caret and readout box, and the speed and altitude boxes.
    let fixed: Vec<[u32; 4]> = vec![
        [272, 240, 302, 240],
        [302, 240, 311, 250],
        [311, 250, 320, 240],
        [320, 240, 329, 250],
        [329, 250, 338, 240],
        [338, 240, 368, 240],
        [312, 58, 320, 50],
        [320, 50, 328, 58],
        [292, 60, 348, 60],
        [348, 60, 348, 84],
        [348, 84, 292, 84],
        [292, 84, 292, 60],
        [24, 214, 112, 214],
        [112, 214, 112, 242],
        [112, 242, 24, 242],
        [24, 242, 24, 214],
        [522, 214, 626, 214],
        [626, 214, 626, 242],
        [626, 242, 522, 242],
        [522, 242, 522, 214],
    ];
    for [x0, y0, x1, y1] in fixed {
        let (x0, y0, x1, y1) = (a.k(x0), a.k(y0), a.k(x1), a.k(y1));
        a.op(LINE, &[frame, stride, x0, y0, x1, y1, bright]);
    }
    // Text: the labels that never change, then the slots the host fills.
    let texts: [(u32, u32, u32, usize, u16); 6] = [
        (24, 206, static_text + spd_label.0, spd_label.1, dim),
        (522, 206, static_text + alt_label.0, alt_label.1, dim),
        (296, 78, TEXTS + T_HEADING.0, T_HEADING.1, bright),
        (36, 236, TEXTS + T_SPEED.0, T_SPEED.1, bright),
        (534, 236, TEXTS + T_ALTITUDE.0, T_ALTITUDE.1, bright),
        (96, 466, TEXTS + T_BOTTOM.0, T_BOTTOM.1, bright),
    ];
    for (x, y, at, n, level) in texts {
        let (x, y, at, n) = (a.k(x), a.k(y), a.k(at), a.k(n as u32));
        a.op(GLYPHS, &[font, frame, stride, x, y, mask, level, at, n]);
    }
    a.op(SYS_READ, &[times(1), time]);
    a.op(RET, &[]);

    // show: the regions and the ladder's rectangle to the panel, then how long rendering and this took, in µs.
    a.label("show");
    for [x, y, w, h] in REGIONS {
        let (x, y, w, h) = (a.k(x), a.k(y), a.k(w), a.k(h));
        a.op(DRAW, &[frame, stride, x, y, x, y, w, h]);
    }
    a.op(DRAW, &[frame, stride, rect(0), rect(1), rect(0), rect(1), rect(2), rect(3)]);
    a.op(SYS_READ, &[times(2), time]);
    // Each part's time, tagged with its number in the top byte: 1 clearing, 2 the ladder, 3 the heading tape, 4 the
    // rest, 5 copying to the panel.
    for (end, start, tag, reg) in [(3, 0, 1, R0), (4, 3, 2, R1), (5, 4, 3, R2), (1, 5, 4, R3), (2, 1, 5, R4)] {
        let tag = a.k(tag << 24);
        a.op(SUB, &[reg, times(end), times(start)]);
        a.op(ADD, &[reg, reg, tag]);
    }
    a.op(NOTIFY, &[R0, R1]);
    a.op(NOTIFY, &[R2, R3]);
    a.op(NOTIFY, &[R4, zero]);
    a.op(RET, &[]);

    // xf: (r3, r4) in ladder space to (r7, r8) on the panel: rotated by the state's cos and sin (×1024) and moved to
    // its centre.
    let k1024 = a.k(1024);
    let work = |i: u32| ((WORK / 4) + i) as u16;
    let (cx, cy, cos, sin) = (work(W_XF), work(W_XF + 1), work(W_XF + 2), work(W_XF + 3));
    let level = work(W_LEVEL);
    a.label("xf");
    a.op(MUL, &[R7, R3, cos]);
    a.op(MUL, &[R12, R4, sin]);
    a.op(SUB, &[R7, R7, R12]);
    a.op(DIVS, &[R7, R7, k1024]);
    a.op(ADD, &[R7, R7, cx]);
    a.op(MUL, &[R8, R3, sin]);
    a.op(MUL, &[R12, R4, cos]);
    a.op(ADD, &[R8, R8, R12]);
    a.op(DIVS, &[R8, R8, k1024]);
    a.op(ADD, &[R8, R8, cy]);
    a.op(RET, &[]);

    // ladder: segments r0… (r1 of them).
    let (ladder, labels, label_text) = (a.k(LADDER), a.k(LABELS), a.k(LABEL_TEXT));
    a.label("ladder");
    a.op(MUL, &[R2, R0, eight]);
    a.op(ADD, &[R2, R2, ladder]);
    a.label("ladder_next");
    a.branch(BEQ, R1, zero, "ladder_done");
    a.op(LD16, &[R3, R2, zero]);
    a.op(SUB, &[R3, R3, off]);
    a.op(LD16, &[R4, R2, one]);
    a.op(SUB, &[R4, R4, off]);
    a.jump(CALL, "xf");
    a.op(MOV, &[R10, R7]);
    a.op(MOV, &[R11, R8]);
    a.op(LD16, &[R3, R2, two]);
    a.op(SUB, &[R3, R3, off]);
    a.op(LD16, &[R4, R2, three]);
    a.op(SUB, &[R4, R4, off]);
    a.jump(CALL, "xf");
    a.op(LINE, &[frame, stride, R10, R11, R7, R8, level]);
    a.op(ADD, &[R2, R2, eight]);
    a.op(SUB, &[R1, R1, one]);
    a.jump(JMP, "ladder_next");
    a.label("ladder_done");
    a.op(RET, &[]);

    // labels: ladder labels r0… (r1 of them), centred on their rotated anchors.
    let seven = a.k(7);
    a.label("labels");
    a.op(MUL, &[R2, R0, eight]);
    a.op(ADD, &[R2, R2, labels]);
    a.label("labels_next");
    a.branch(BEQ, R1, zero, "labels_done");
    a.op(LD16, &[R3, R2, zero]);
    a.op(SUB, &[R3, R3, off]);
    a.op(LD16, &[R4, R2, one]);
    a.op(SUB, &[R4, R4, off]);
    a.jump(CALL, "xf");
    a.op(LD16, &[R5, R2, two]);
    a.op(ADD, &[R5, R5, label_text]);
    a.op(LD16, &[R6, R2, three]);
    a.op(MUL, &[R9, R6, eight]);
    a.op(SUB, &[R7, R7, R9]);
    a.op(ADD, &[R8, R8, seven]);
    a.op(GLYPHS, &[font, frame, stride, R7, R8, mask, level, R5, R6]);
    a.op(ADD, &[R2, R2, eight]);
    a.op(SUB, &[R1, R1, one]);
    a.jump(JMP, "labels_next");
    a.label("labels_done");
    a.op(RET, &[]);

    // heading: the tape, a tick every 10° and every third labelled with the heading's tens (03, 06, …), within
    // 50° either side of the heading.
    let heading = state_word(S_HEADING);
    let (first, ten, k30, k360) = (a.k(360 - 50 + 9), a.k(10), a.k(30), a.k(360));
    let (px, centre, right) = (a.k(HEADING_PX as u32), a.k(320), a.k(320 + 50 * HEADING_PX as u32));
    let (major_top, minor_top, base, label_y, sixteen, digits) =
        (a.k(30), a.k(38), a.k(46), a.k(24), a.k(16), a.k(HDG_DIGITS));
    a.label("heading");
    a.op(ADD, &[R0, heading, first]);
    a.op(DIVU, &[R0, R0, ten]);
    a.op(MUL, &[R0, R0, ten]);
    a.label("heading_next");
    a.op(SUB, &[R1, R0, heading]);
    a.op(SUB, &[R1, R1, k360]);
    a.op(MUL, &[R1, R1, px]);
    a.op(ADD, &[R1, R1, centre]);
    a.branch(BLTS, right, R1, "heading_done");
    a.op(MODU, &[R2, R0, k30]);
    a.branch(BNE, R2, zero, "heading_minor");
    a.op(LINE, &[frame, stride, R1, major_top, R1, base, bright]);
    a.op(MODU, &[R3, R0, k360]);
    a.op(DIVU, &[R3, R3, ten]);
    a.op(DIVU, &[R4, R3, ten]);
    a.op(MODU, &[R5, R3, ten]);
    a.op(ST8, &[digits, zero, R4]);
    a.op(ST8, &[digits, one, R5]);
    a.op(SUB, &[R6, R1, sixteen]);
    a.op(GLYPHS, &[font, frame, stride, R6, label_y, mask, bright, digits, two]);
    a.jump(JMP, "heading_step");
    a.label("heading_minor");
    a.op(LINE, &[frame, stride, R1, minor_top, R1, base, dim]);
    a.label("heading_step");
    a.op(ADD, &[R0, R0, ten]);
    a.jump(JMP, "heading_next");
    a.label("heading_done");
    a.op(RET, &[]);

    assert_eq!(a.address("render"), CODE);
    let show = a.address("show");
    let (code, pool) = a.finish()?;
    assert!(POOL + pool.len() as u32 <= STATE && CODE + code.len() as u32 <= IDLE);
    Ok((code, pool, show))
}

/// The ladder's segments and labels in the arena's layout, the label glyphs, and for each rung the index of its
/// first segment and label, so a range of rungs is a range of each.
struct Tables {
    segments: Vec<u8>,
    labels: Vec<u8>,
    text: Vec<u8>,
    first: Vec<(i32, usize, usize)>,
    static_text: u32,
    spd: (u32, usize),
    alt: (u32, usize),
}

fn tables() -> Tables {
    let u16s = |v: &[i32]| v.iter().flat_map(|&x| ((x + OFF) as u16).to_le_bytes()).collect::<Vec<u8>>();
    let mut t = Tables {
        segments: Vec::new(),
        labels: Vec::new(),
        text: Vec::new(),
        first: Vec::new(),
        static_text: 0,
        spd: (0, 0),
        alt: (0, 0),
    };
    let (mut segments, mut labels) = (0, 0);
    for rung in ladder() {
        t.first.push((rung.pitch, segments, labels));
        for s in &rung.segments {
            t.segments.extend(u16s(s));
            segments += 1;
        }
        for ([x, y], text) in &rung.labels {
            let glyphs = v5::text_indices(text);
            let at = t.text.len() as u16;
            t.labels.extend(u16s(&[*x, *y]));
            t.labels.extend(at.to_le_bytes());
            t.labels.extend((glyphs.len() as u16).to_le_bytes());
            t.text.extend(glyphs);
            labels += 1;
        }
    }
    t.first.push((i32::MAX, segments, labels));
    // The fixed labels go after the ladder's.
    t.static_text = LABEL_TEXT + t.text.len() as u32;
    for (slot, label) in [(&mut t.spd, "SPD"), (&mut t.alt, "ALT")] {
        *slot = (LABEL_TEXT + t.text.len() as u32 - t.static_text, label.len());
        t.text.extend(v5::text_indices(label));
    }
    t
}

/// The ranges of segments and labels for the rungs within LADDER_SPAN of `pitch`.
fn visible(first: &[(i32, usize, usize)], pitch: f64) -> [u32; 4] {
    let inside: Vec<usize> =
        (0..first.len() - 1).filter(|&i| (first[i].0 as f64 - pitch).abs() <= LADDER_SPAN).collect();
    let (Some(&a), Some(&b)) = (inside.first(), inside.last()) else { return [0; 4] };
    let (s0, l0) = (first[a].1, first[a].2);
    let (s1, l1) = (first[b + 1].1, first[b + 1].2);
    [s0 as u32, (s1 - s0) as u32, l0 as u32, (l1 - l0) as u32]
}

/// The simulated aircraft.
struct Aircraft {
    speed: f64,
    altitude: f64,
    heading: f64,
}

impl Aircraft {
    /// Flies it for `dt` seconds at `pitch` and `bank` (degrees, bank right positive).
    fn fly(&mut self, pitch: f64, bank: f64, dt: f64) {
        let (pitch, bank) = (pitch.to_radians(), bank.clamp(-60.0, 60.0).to_radians());
        // Climbing bleeds speed, diving gains it, and the engine pulls it back toward 250 kt.
        self.speed = (self.speed + (-30.0 * pitch.sin() + (250.0 - self.speed) * 0.15) * dt).clamp(80.0, 600.0);
        self.altitude = (self.altitude + self.speed * 1.688 * pitch.sin() * dt).max(0.0);
        // The standard rate-of-turn formula: degrees a second for a bank at a true airspeed in knots.
        self.heading = (self.heading + 1091.0 * bank.tan() / self.speed * dt).rem_euclid(360.0);
    }
}

/// The rectangle (x, y, w, h) the ladder's visible rungs and the aircraft symbol cover on the panel, turned by `c`
/// and `s` about `centre`, joined with `before` so clearing it also clears the last frame's ladder.
fn ladder_rect(rungs: &[Rung], pitch: f64, centre: [f64; 2], c: f64, s: f64, before: Option<[i32; 4]>) -> [i32; 4] {
    let [mut x0, mut y0, mut x1, mut y1] = SYMBOL;
    let mut take = |x: f64, y: f64| {
        let (px, py) = (centre[0] + x * c - y * s, centre[1] + x * s + y * c);
        x0 = x0.min(px.floor() as i32);
        y0 = y0.min(py.floor() as i32);
        x1 = x1.max(px.ceil() as i32);
        y1 = y1.max(py.ceil() as i32);
    };
    for rung in rungs.iter().filter(|r| (r.pitch as f64 - pitch).abs() <= LADDER_SPAN) {
        for [ax, ay, bx, by] in &rung.segments {
            take(*ax as f64, *ay as f64);
            take(*bx as f64, *by as f64);
        }
        for ([x, y], text) in &rung.labels {
            let half = (text.len() * 8 + 2) as f64;
            for (dx, dy) in [(-half, -10.0), (half, -10.0), (-half, 10.0), (half, 10.0)] {
                take(*x as f64 + dx, *y as f64 + dy);
            }
        }
    }
    if let Some([bx, by, bw, bh]) = before {
        x0 = x0.min(bx);
        y0 = y0.min(by);
        x1 = x1.max(bx + bw);
        y1 = y1.max(by + bh);
    }
    let (x0, y0) = ((x0 - 2).clamp(0, v5::WIDTH as i32), (y0 - 2).clamp(0, v5::HEIGHT as i32));
    let (x1, y1) = ((x1 + 2).clamp(0, v5::WIDTH as i32), (y1 + 2).clamp(0, v5::HEIGHT as i32));
    [x0, y0, x1 - x0, y1 - y0]
}

/// Text padded or cut to `n` characters, as glyph indices.
fn slot(text: &str, n: usize) -> Vec<u8> {
    let text: String = format!("{text:<n$}").chars().take(n).collect();
    v5::text_indices(&text)
}

pub async fn flight(side: Option<Side>, boost: u8) -> Result<(), Error> {
    let tables = tables();
    let rungs = ladder();
    let (code, pool, show) = render_code(tables.static_text, tables.spd, tables.alt)?;
    let mut arms = crate::v0::arms(side).await?;
    let mut lead = Duration::ZERO;
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        crate::v0::upload_font(arm, SCALE).await?;
        let max = arm.max_code;
        let mut writes = v5::upload(POOL, &pool, max);
        writes.extend(v5::upload(CODE, &code, max));
        writes.extend(v5::upload(LADDER, &tables.segments, max));
        writes.extend(v5::upload(LABELS, &tables.labels, max));
        writes.extend(v5::upload(LABEL_TEXT, &tables.text, max));
        writes.push(v5::write(WORK, &[0; WORK_WORDS as usize * 4]));
        writes.push(v5::set(prop::IMU_RATE, 104));
        writes.push(v5::set(prop::GYRO_RATE, 104));
        arm.run(writes).await?;
        arm.flush().await?;
        crate::v0_sensors::start_idle_program(arm, IDLE).await?;
        for _ in 0..2 {
            match arm.ask_for_fast_link().await {
                Ok(interval) if interval <= Duration::from_millis(15) => break,
                Ok(_) => {}
                Err(e) => link::trace(format_args!("{e}")),
            }
        }
        let round_trip = arm.sync_clock().await?;
        lead = lead.max(2 * arm.interval() + round_trip / 2 + Duration::from_millis(15));
    }
    println!("hold your head still a moment; then tilt it up to climb, bank or turn it to turn. Ctrl-C to land");

    let mut aircraft = Aircraft { speed: 250.0, altitude: 12000.0, heading: 90.0 };
    let mut acc = vec![[v5::UNKNOWN; 3]; arms.len()];
    // Rotation about the vertical (mdps), and the gyroscope's reading at rest, measured over the first 40 readings.
    let mut yaw = vec![v5::UNKNOWN; arms.len()];
    let mut rest: Vec<f64> = Vec::new();
    let mut rect: Option<[i32; 4]> = None;
    // How long the arms take to render a frame and to put it on the panel, from their reports.
    let mut parts = [0u64; 5];
    let mut reports = 0u64;
    let mut light = v5::UNKNOWN;
    let mut brightness: Option<u32> = None;
    let mut reads: Vec<Option<(u8, Vec<u8>, std::time::Instant)>> = arms.iter().map(|_| None).collect();
    let mut queued: Vec<std::collections::VecDeque<u16>> = vec![Default::default(); arms.len()];
    let mut next_due: Option<u32> = None;
    let mut last = std::time::Instant::now();
    let mut frames = 0u64;
    let mut reported = std::time::Instant::now();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        frames += 1;
        let frame = async {
            // The latest readings in, the next ones asked for; the right arm's light now and then for brightness.
            for (i, (arm, read)) in arms.iter_mut().zip(&mut reads).enumerate() {
                arm.poll().await?;
                if let Some((tag, ids, _)) = read
                    && let Some(reply) = arm.reply(*tag)
                {
                    for (&id, value) in ids.iter().zip(reply?.as_chunks::<4>().0) {
                        let value = u32::from_le_bytes(*value);
                        match id {
                            prop::LIGHT => light = value,
                            prop::GYR_Y => yaw[i] = value,
                            _ => acc[i][(id - prop::ACC_X) as usize] = value,
                        }
                    }
                    *read = None;
                }
                while let Some(event) = arm.events.pop_front() {
                    if let v5::EventKind::Notify { a, b, .. } = event.kind {
                        for part in [a, b] {
                            let (tag, us) = ((part >> 24) as usize, (part & 0xFF_FFFF) as u64);
                            if (1..=5).contains(&tag) {
                                parts[tag - 1] += us;
                                if tag == 5 {
                                    reports += 1;
                                }
                            }
                        }
                    }
                }
                if read.as_ref().is_none_or(|r| r.2.elapsed() > Duration::from_millis(500)) {
                    let mut ids = vec![prop::ACC_X, prop::ACC_Y, prop::ACC_Z, prop::GYR_Y];
                    if arm.side == Side::Right && frames.is_multiple_of(20) {
                        ids.push(prop::LIGHT);
                    }
                    let tag = arm.request(v5::ctrl::PROPS, &ids).await?;
                    *read = Some((tag, ids, std::time::Instant::now()));
                }
            }

            // Head attitude from both arms' accelerometers averaged.
            let known: Vec<[f64; 3]> = acc
                .iter()
                .filter(|a| a.iter().all(|&v| v != v5::UNKNOWN))
                .map(|a| a.map(|v| v as i32 as f64))
                .collect();
            if known.is_empty() {
                tokio::time::sleep(PERIOD).await;
                return Ok::<(), Error>(());
            }
            let [ax, ay, az] = [0, 1, 2].map(|i| known.iter().map(|a| a[i]).sum::<f64>() / known.len() as f64);
            let across = ax.hypot(ay).max(1.0);
            let pitch = (-az).atan2(across).to_degrees();
            let bank = (-ax).atan2(ay).to_degrees();
            let g = (ax * ax + ay * ay + az * az).sqrt() / 1000.0;
            let now = std::time::Instant::now();
            let dt = now.duration_since(last).as_secs_f64().min(0.2);
            aircraft.fly(pitch, bank, dt);
            last = now;
            // Turning the head turns too: rotation about the vertical, left positive (right-hand rule, y up), less
            // what the gyroscope reads at rest.
            let yaws: Vec<f64> = yaw.iter().filter(|&&v| v != v5::UNKNOWN).map(|&v| v as i32 as f64).collect();
            if !yaws.is_empty() {
                let rate = yaws.iter().sum::<f64>() / yaws.len() as f64;
                if rest.len() < 40 {
                    rest.push(rate);
                } else {
                    let bias = rest.iter().sum::<f64>() / rest.len() as f64;
                    aircraft.heading = (aircraft.heading - (rate - bias) / 1000.0 * dt).rem_euclid(360.0);
                }
            }

            // The state: the ladder turned with the bank (cos, sin ×1024, so the horizon tilts against the head)
            // and moved along it by the pitch, which rungs show, and the readouts.
            let (c, s) = (ay / across, ax / across);
            let shift = pitch * LADDER_PX;
            let [ladder_first, ladder_n, label_first, label_n] = visible(&tables.first, pitch);
            let centre = [320.0 - shift * s, 240.0 + shift * c];
            let now_rect = ladder_rect(&rungs, pitch, centre, c, s, None);
            let [rx, ry, rw, rh] = ladder_rect(&rungs, pitch, centre, c, s, rect);
            rect = Some(now_rect);
            let words = [
                centre[0] as i32 as u32,
                centre[1] as i32 as u32,
                (c * 1024.0) as i32 as u32,
                (s * 1024.0) as i32 as u32,
                ladder_first,
                ladder_n,
                label_first,
                label_n,
                aircraft.heading as u32 % 360,
                rx as u32,
                ry as u32,
                rw as u32,
                rh as u32,
            ];
            let mut state = vec![0u8; STATE_LEN];
            for (i, w) in words.iter().enumerate() {
                state[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            let side = if bank > 0.5 {
                "R"
            } else if bank < -0.5 {
                "L"
            } else {
                " "
            };
            let time = chrono::Local::now().format("%H:%M:%S");
            let texts = [
                (T_HEADING, format!("{:03}", aircraft.heading as u32 % 360)),
                (T_SPEED, format!("{:>4}", aircraft.speed.round() as i32)),
                (T_ALTITUDE, format!("{:>5}", aircraft.altitude.round() as i32)),
                (T_BOTTOM, format!("G {g:.1}  BANK {:>2}{side}  {time}", bank.abs().round() as i32)),
            ];
            for ((at, n), text) in texts {
                let at = (TEXTS - STATE + at) as usize;
                state[at..at + n].copy_from_slice(&slot(&text, n));
            }

            let mut code = vec![v5::write(STATE, &state), v5::call(CODE)];
            if light != v5::UNKNOWN {
                let want = crate::v0_sensors::brightness_for(light, boost);
                if brightness.is_none_or(|b| b.abs_diff(want) >= 2) {
                    code.push(v5::set(prop::BRIGHTNESS, want));
                    brightness = Some(want);
                }
            }

            let waits = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| async move {
                while queued.len() >= QUEUE {
                    arm.wait_for(queued.pop_front().unwrap()).await?;
                }
                Ok::<(), Error>(())
            });
            futures::future::try_join_all(waits).await?;
            let interval = arms.iter().map(|a| a.interval()).max().unwrap_or_default();
            let lead = (lead + interval).as_micros() as u32;
            let earliest = v5_link::host_us().wrapping_add(lead);
            let due = match next_due {
                Some(due) if (due.wrapping_sub(earliest) as i32) > 0 => due,
                _ => earliest,
            };
            let send_at = due.wrapping_sub(lead).wrapping_sub(v5_link::host_us()) as i32;
            if send_at > 0 {
                tokio::time::sleep(Duration::from_micros(send_at as u64)).await;
            }
            next_due = Some(due.wrapping_add(PERIOD.as_micros() as u32));
            let sends = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| {
                let mut code = code.clone();
                code.push(v5::sleep_until(arm.arm_time(due)));
                code.push(v5::call(show));
                async move {
                    arm.run(code).await?;
                    queued.push_back(arm.sent());
                    Ok::<(), Error>(())
                }
            });
            futures::future::try_join_all(sends).await?;
            Ok::<(), Error>(())
        };
        tokio::select! {
            _ = &mut stop => break,
            done = frame => done?,
        }
        if reported.elapsed() > Duration::from_secs(5) {
            let rate = frames as f64 / reported.elapsed().as_secs_f64();
            println!(
                "{rate:.1} frames/s, heading {:.0}, {:.0} kt, {:.0} ft",
                aircraft.heading, aircraft.speed, aircraft.altitude
            );
            if let Some([clear, ladder, tape, rest, show]) = parts
                .map(|p| p.checked_div(reports))
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .map(|v| [v[0], v[1], v[2], v[3], v[4]])
            {
                link::trace(format_args!(
                    "arms, µs a frame: clear {clear}, ladder {ladder}, tape {tape}, rest {rest}, copy to panel {show}"
                ));
            }
            (parts, reports) = ([0; 5], 0);
            frames = 0;
            reported = std::time::Instant::now();
        }
    }
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        arm.run([v5::set(prop::IMU_RATE, 13), v5::set(prop::PANEL, 0)]).await?;
        arm.flush().await?;
    }
    for arm in arms {
        arm.disconnect().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_fits() {
        let t = tables();
        let (code, pool, show) = render_code(t.static_text, t.spd, t.alt).unwrap();
        assert!(show > CODE);
        assert!(!code.is_empty() && !pool.is_empty());
        assert!(LADDER + t.segments.len() as u32 <= LABELS);
        assert!(LABELS + t.labels.len() as u32 <= LABEL_TEXT);
        assert!(LABEL_TEXT + t.text.len() as u32 <= HDG_DIGITS);
        // The state and its text slots, plus the call, sleep and draw, fit one packet.
        assert!(5 + STATE_LEN + 4 + 11 + 4 <= 242);
        assert!(TIMES >= STATE + STATE_LEN as u32 && (S_LADDER_RECT + 4) * 4 <= TEXTS - STATE);
        assert!(WORK >= TIMES + 12 && WORK + WORK_WORDS * 4 <= LADDER);
    }

    #[test]
    fn shows_the_rungs_near_the_pitch() {
        let t = tables();
        let [s0, sn, l0, ln] = visible(&t.first, 0.0);
        // −15° to 15°: two labelled rungs (4 segments, 2 labels each), four short ones (2) and the horizon (2).
        assert_eq!(sn, 2 * 4 + 4 * 2 + 2);
        assert_eq!(ln, 2 * 2);
        assert!(s0 > 0 && l0 > 0);
        assert_eq!(visible(&t.first, 1000.0), [0; 4]);
    }

    #[test]
    fn ladder_rect_covers_the_ladder_and_the_last_one() {
        let rungs = ladder();
        let level = ladder_rect(&rungs, 0.0, [320.0, 240.0], 1.0, 0.0, None);
        // The horizon reaches 200 either side of the middle.
        assert!(level[0] <= 120 && level[0] + level[2] >= 520);
        let higher = ladder_rect(&rungs, 10.0, [320.0, 320.0], 1.0, 0.0, Some(level));
        assert!(higher[1] <= level[1] && higher[1] + higher[3] >= level[1] + level[3]);
    }

    #[test]
    fn flies() {
        let mut a = Aircraft { speed: 250.0, altitude: 1000.0, heading: 0.0 };
        a.fly(10.0, 30.0, 1.0);
        assert!(a.altitude > 1000.0 && a.heading > 0.0 && a.speed < 250.0);
        a.fly(0.0, -30.0, 1.0);
        assert!(a.heading < 10.0);
    }
}
