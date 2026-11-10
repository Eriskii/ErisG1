//! `erisg1 v0 cube`: the clock, battery and spinning cube of `v0_cube`, run by the glasses on their own. The Mac
//! uploads a program to each arm and leaves: the program keeps the time, reads the battery, sets the brightness and
//! draws a frame of the cube every PERIOD_US until `v0 stop` or a reboot.
//!
//! - The cube's frames are worked out here once, for its whole loop (`LOOP_SECS`), and stored on the arms: each
//!   frame's corners, edge levels and the rectangle around it. Each arm gets its own eye's view, so the cube is 3D.
//! - Both arms keep to the right arm's clock: the left adds an offset, which the Mac measures at the start and the
//!   arm-to-arm link keeps up to date, so the eyes change together.
//! - Once a second each arm sends the other its battery over that link, so both show the average.
//! - A program may run 30 ms of every 100 ms (VM.md §5.4). A frame takes about 21 ms, so this runs at 10 frames a
//!   second, and redraws only the seconds of the clock each second (the whole of it each minute).
//! - In the case the panel is off.

use std::time::Duration;

use chrono::Timelike;

use crate::link::Error;
use crate::proto::Side;
use crate::v0_cube::{self, ADVANCE, BAR, BASELINE, BATTERY, BATTERY_TEXT, CLOCK, LOOP_SECS, SCALE};
use crate::v5::{self, prop, trigger};
use crate::v5_asm::*;
use crate::v5_link::{self, Arm, side_name};

const PERIOD_US: u32 = 100_000;
const FRAMES: u32 = (LOOP_SECS * 1e6) as u32 / PERIOD_US;
/// A frame of the loop: the corners (x, y), the edges' levels, and the rectangle (x0, y0, x1, y1) around the cube.
const F_LEVELS: u32 = 16;
const F_RECT: u32 = 28;
const FRAME_BYTES: u32 = 32 * 4;

// Where things sit in the arena: the loop above the font, the program at the top.
const TABLE: u32 = 0x29000;
const POOL: u32 = 0x3C000;
const VARS: u32 = 0x3C400;
// The program's words, by index.
/// When frame 0 of the loop is due, in the right arm's clock.
const V_E: u32 = 0;
/// The frame last drawn.
const V_K: u32 = 1;
/// The second of the day shown, and when the next one starts (right arm's clock).
const V_SEC: u32 = 2;
const V_T_NEXT: u32 = 3;
/// What to add to this arm's clock for the right arm's: 0 on the right.
const V_OFF: u32 = 4;
/// The other arm's battery, from its last message; above 100 unknown.
const V_OTHER: u32 = 5;
/// The battery and the minute as last drawn.
const V_SHOWN_PCT: u32 = 6;
const V_SHOWN_MIN: u32 = 7;
/// 1 while the panel is on.
const V_VISIBLE: u32 = 8;
/// What to draw next frame: everything (the panel just came on), the seconds, the whole clock, the battery.
const V_FULL: u32 = 9;
const V_SECS: u32 = 10;
const V_CLOCK: u32 = 11;
const V_BATT: u32 = 12;
/// The brightness last set (right arm).
const V_BRIGHT: u32 = 13;
/// The time this tick started, in the right arm's clock.
const V_NOW: u32 = 14;
const VAR_WORDS: u32 = 15;
/// The frame being drawn, the corners and rectangle drawn last, the text, and the trigger table.
const CUR: u32 = VARS + 0x40;
const LAST: u32 = VARS + 0xC0;
const LAST_RECT: u32 = VARS + 0x100;
const CLOCK_TEXT: u32 = VARS + 0x120;
const BATT_TEXT: u32 = VARS + 0x130;
const TRIGGERS: u32 = VARS + 0x140;
const CODE: u32 = VARS + 0x200;

/// How far a new clock offset from the arm-to-arm link may be from the one in use, in µs, to be taken. The Mac's
/// measurement is good to a few ms; this keeps out anything else.
const OFFSET_WINDOW: u32 = 25_000;

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

fn var(i: u32) -> u16 {
    ((VARS / 4) + i) as u16
}

fn word(at: u32, i: u32) -> u16 {
    ((at / 4) + i) as u16
}

fn glyph(c: char) -> u32 {
    v5::text_indices(&c.to_string())[0] as u32
}

/// The cube's whole loop as the eye on `side` sees it, FRAME_BYTES a frame.
fn table(side: Side) -> Vec<u8> {
    let mut table = Vec::with_capacity((FRAMES * FRAME_BYTES) as usize);
    for k in 0..FRAMES {
        let (corners, levels) = v0_cube::cube(k as f64 * PERIOD_US as f64 / 1e6, v0_cube::eye(Some(side)));
        let [x, y, w, h] = v0_cube::cube_rect(&corners, None);
        let words = corners.iter().flat_map(|&[x, y]| [x as u32, y as u32]).chain(levels).chain([
            x as u32,
            y as u32,
            (x + w) as u32,
            (y + h) as u32,
        ]);
        table.extend(words.flat_map(|w| w.to_le_bytes()));
    }
    table
}

struct Program {
    code: Vec<u8>,
    pool: Vec<u8>,
    /// The handlers: `tick` for `START` and `TIMER`, `peer` for `PEER`.
    tick: u32,
    peer: u32,
}

/// The program for the arm on `side`, `boost` levels brighter than Even's light table (used on the right arm, whose
/// brightness the left follows).
fn program(side: Side, boost: u8) -> Result<Program, String> {
    let mut a = Asm::new(CODE, POOL);
    let (zero, one, two, three) = (a.k(0), a.k(1), a.k(2), a.k(3));
    let (eight, ten, twelve, sixty, hundred) = (a.k(8), a.k(10), a.k(12), a.k(60), a.k(100));
    let (frame, stride, font, mask, bright) = (a.k(v5::FRAME), a.k(v5::STRIDE), a.k(v5::FONT), a.k(2), a.k(15));
    let [p_time, p_timer, p_in_case, p_batt, p_light, p_brightness, p_panel, p_send, p_msg, p_t_us] = [
        prop::TIME_US,
        prop::TIMER,
        prop::IN_CASE,
        prop::BATT_PCT,
        prop::LIGHT,
        prop::BRIGHTNESS,
        prop::PANEL,
        prop::PEER_SEND,
        prop::PEER_MSG,
        prop::PEER_T_US,
    ]
    .map(|p| a.k(p as u32));
    let (million, day, period, half_period) = (a.k(1_000_000), a.k(86_400), a.k(PERIOD_US), a.k(PERIOD_US / 2));
    let (frames, loop_us, frame_bytes) = (a.k(FRAMES), a.k(FRAMES * PERIOD_US), a.k(FRAME_BYTES));
    let (table, cur, last, last_rect) = (a.k(TABLE), a.k(CUR), a.k(LAST), a.k(LAST_RECT));
    let (cur_rect, corner_bytes, rect_bytes) = (a.k(CUR + F_RECT * 4), a.k(64), a.k(16));
    let f = |i: u32| word(CUR, i);
    let edges = v0_cube::edges();

    // tick, at START and at each frame's TIMER.
    a.label("tick");
    // The right arm's time.
    a.op(SYS_READ, &[R0, p_time]);
    a.op(ADD, &[var(V_NOW), R0, var(V_OFF)]);
    // A new second: the clock moves on, and `second` does what's done once a second.
    a.op(SUB, &[R1, var(V_NOW), var(V_T_NEXT)]);
    a.branch(BLTS, R1, zero, "frame");
    a.op(DIVU, &[R2, R1, million]);
    a.op(ADD, &[R2, R2, one]);
    a.op(ADD, &[var(V_SEC), var(V_SEC), R2]);
    a.op(MODU, &[var(V_SEC), var(V_SEC), day]);
    a.op(MUL, &[R2, R2, million]);
    a.op(ADD, &[var(V_T_NEXT), var(V_T_NEXT), R2]);
    a.jump(CALL, "second");
    a.label("frame");
    // The frame due now, to the nearest, so a tick a little early or late still finds its own. Every FRAMES frames
    // the loop starts again from a later V_E.
    a.op(SUB, &[R1, var(V_NOW), var(V_E)]);
    a.op(ADD, &[R1, R1, half_period]);
    a.op(DIVU, &[R2, R1, period]);
    a.op(DIVU, &[R3, R2, frames]);
    a.op(MUL, &[R3, R3, loop_us]);
    a.op(ADD, &[var(V_E), var(V_E), R3]);
    a.op(MODU, &[R2, R2, frames]);
    // The next frame's tick, in this arm's clock.
    a.op(ADD, &[R3, R2, one]);
    a.op(MUL, &[R3, R3, period]);
    a.op(ADD, &[R3, R3, var(V_E)]);
    a.op(SUB, &[R3, R3, var(V_OFF)]);
    a.op(SYS_WRITE, &[p_timer, R3]);
    a.branch(BEQ, R2, var(V_K), "end");
    a.op(MOV, &[var(V_K), R2]);
    // In the case: the panel off. (Not when `WORN` reads 0: it does while the glasses are on.)
    a.op(SYS_READ, &[R4, p_in_case]);
    a.branch(BEQ, R4, one, "hide");
    a.branch(BNE, var(V_VISIBLE), zero, "draw");
    a.op(MOV, &[var(V_VISIBLE), one]);
    a.op(MOV, &[var(V_FULL), one]);
    a.label("draw");
    // The last cube in black, then this one.
    a.op(MUL, &[R5, R2, frame_bytes]);
    a.op(ADD, &[R5, R5, table]);
    a.op(COPY, &[cur, R5, frame_bytes]);
    for &(i, j) in &edges {
        let l = |n: u32| word(LAST, n);
        a.op(LINE, &[frame, stride, l(2 * i), l(2 * i + 1), l(2 * j), l(2 * j + 1), zero]);
    }
    for (n, &(i, j)) in edges.iter().enumerate() {
        a.op(LINE, &[frame, stride, f(2 * i), f(2 * i + 1), f(2 * j), f(2 * j + 1), f(F_LEVELS + n as u32)]);
    }
    a.op(COPY, &[last, cur, corner_bytes]);
    a.branch(BNE, var(V_FULL), zero, "whole");
    // To the panel: the rectangle around this cube and the last.
    let (lr, fr) = (|n: u32| word(LAST_RECT, n), |n: u32| f(F_RECT + n));
    a.op(MOV, &[R6, fr(0)]);
    a.branch(BLTS, R6, lr(0), "x0");
    a.op(MOV, &[R6, lr(0)]);
    a.label("x0");
    a.op(MOV, &[R7, fr(1)]);
    a.branch(BLTS, R7, lr(1), "y0");
    a.op(MOV, &[R7, lr(1)]);
    a.label("y0");
    a.op(MOV, &[R8, fr(2)]);
    a.branch(BLTS, lr(2), R8, "x1");
    a.op(MOV, &[R8, lr(2)]);
    a.label("x1");
    a.op(MOV, &[R9, fr(3)]);
    a.branch(BLTS, lr(3), R9, "y1");
    a.op(MOV, &[R9, lr(3)]);
    a.label("y1");
    a.op(SUB, &[R8, R8, R6]);
    a.op(SUB, &[R9, R9, R7]);
    a.op(DRAW, &[frame, stride, R6, R7, R6, R7, R8, R9]);
    a.op(COPY, &[last_rect, cur_rect, rect_bytes]);
    a.jump(CALL, "text");
    a.jump(JMP, "end");
    // The panel just came on: everything.
    a.label("whole");
    for v in [V_SECS, V_CLOCK, V_BATT] {
        a.op(MOV, &[var(v), one]);
    }
    a.jump(CALL, "text");
    let (width, height) = (a.k(v5::WIDTH), a.k(v5::HEIGHT));
    a.op(DRAW, &[frame, stride, zero, zero, zero, zero, width, height]);
    a.op(COPY, &[last_rect, cur_rect, rect_bytes]);
    a.op(MOV, &[var(V_FULL), zero]);
    a.label("end");
    a.op(END, &[]);
    a.label("hide");
    a.branch(BEQ, var(V_VISIBLE), zero, "end");
    a.op(MOV, &[var(V_VISIBLE), zero]);
    a.op(SYS_WRITE, &[p_panel, zero]);
    a.op(END, &[]);

    // second: the clock's text to redraw, both arms' battery averaged, a message to the other arm and, on the
    // right, the brightness.
    a.label("second");
    a.op(MOV, &[var(V_SECS), one]);
    a.op(DIVU, &[R3, var(V_SEC), sixty]);
    a.branch(BEQ, R3, var(V_SHOWN_MIN), "same_minute");
    a.op(MOV, &[var(V_SHOWN_MIN), R3]);
    a.op(MOV, &[var(V_CLOCK), one]);
    a.label("same_minute");
    a.op(SYS_READ, &[R3, p_batt]);
    a.op(MOV, &[R4, var(V_OTHER)]);
    a.branch(BLTU, hundred, R3, "own_unknown");
    a.branch(BLTU, hundred, R4, "averaged");
    a.op(ADD, &[R3, R3, R4]);
    a.op(ADD, &[R3, R3, one]);
    a.op(DIVU, &[R3, R3, two]);
    a.jump(JMP, "averaged");
    a.label("own_unknown");
    a.op(MOV, &[R3, R4]);
    a.label("averaged");
    a.branch(BEQ, R3, var(V_SHOWN_PCT), "same_pct");
    a.op(MOV, &[var(V_SHOWN_PCT), R3]);
    a.op(MOV, &[var(V_BATT), one]);
    a.label("same_pct");
    // The message: this arm's battery (255 unknown) in the top byte, its clock in 256 µs steps below.
    let (k101, k255, k24, low24) = (a.k(101), a.k(255), a.k(24), a.k(0xFF_FFFF));
    a.op(SYS_READ, &[R4, p_batt]);
    a.branch(BLTU, R4, k101, "pct_known");
    a.op(MOV, &[R4, k255]);
    a.label("pct_known");
    a.op(SHL, &[R4, R4, k24]);
    a.op(SYS_READ, &[R5, p_time]);
    a.op(SHR, &[R5, R5, eight]);
    a.op(AND, &[R5, R5, low24]);
    a.op(OR, &[R4, R4, R5]);
    a.op(SYS_WRITE, &[p_send, R4]);
    if side == Side::Right {
        // Even's light table, VM.md §6.7, plus the boost; changed only by two levels or more, as Even does.
        let [k5, k25, k41, k42, k43, k50, k200, k1000, k2000, boost, no_reading] =
            [5, 25, 41, 42, 43, 50, 200, 1000, 2000, boost as u32, 0x1000_0000].map(|v| a.k(v));
        a.op(SYS_READ, &[R4, p_light]);
        a.branch(BLTU, no_reading, R4, "brightness_done");
        a.op(DIVU, &[R4, R4, hundred]);
        a.branch(BLTU, R4, k50, "under_50");
        a.branch(BLTU, R4, k200, "under_200");
        a.branch(BLTU, R4, k1000, "under_1000");
        a.branch(BLTU, R4, k2000, "under_2000");
        a.op(MOV, &[R5, k42]);
        a.jump(JMP, "boost");
        a.label("under_50");
        a.op(DIVU, &[R5, R4, k5]);
        a.jump(JMP, "boost");
        a.label("under_200");
        a.op(SUB, &[R5, R4, k50]);
        a.op(DIVU, &[R5, R5, ten]);
        a.op(ADD, &[R5, R5, ten]);
        a.jump(JMP, "boost");
        a.label("under_1000");
        a.op(SUB, &[R5, R4, k200]);
        a.op(DIVU, &[R5, R5, k50]);
        a.op(ADD, &[R5, R5, k25]);
        a.jump(JMP, "boost");
        a.label("under_2000");
        a.op(MOV, &[R5, k41]);
        a.label("boost");
        a.op(ADD, &[R5, R5, boost]);
        a.branch(BLTU, R5, k43, "capped");
        a.op(MOV, &[R5, k42]);
        a.label("capped");
        a.op(SUB, &[R6, R5, var(V_BRIGHT)]);
        a.op(ADD, &[R6, R6, one]);
        a.branch(BLTU, R6, three, "brightness_done");
        a.op(MOV, &[var(V_BRIGHT), R5]);
        a.op(SYS_WRITE, &[p_brightness, R5]);
        a.label("brightness_done");
    }
    a.op(RET, &[]);

    // text: the clock as " h:mm:ss AM", then whichever of the whole clock, its seconds and the battery is due,
    // drawn and copied to the panel.
    let (space, glyph_a) = (a.k(glyph(' ')), a.k(glyph('A')));
    let (one_from_space, p_from_a) = (a.k(glyph(' ') - glyph('1')), a.k(glyph('P') - glyph('A')));
    let (k3600, eleven, clock_text, batt_text) = (a.k(3600), a.k(11), a.k(CLOCK_TEXT), a.k(BATT_TEXT));
    let at = |a: &mut Asm, i: u32| a.k(i);
    a.label("text");
    a.op(DIVU, &[R3, var(V_SEC), k3600]);
    a.op(ADD, &[R4, R3, eleven]);
    a.op(MODU, &[R4, R4, twelve]);
    a.op(ADD, &[R4, R4, one]);
    // A space before 1–9, a 1 before 10–12.
    a.op(DIVU, &[R5, R4, ten]);
    a.op(MUL, &[R6, R5, one_from_space]);
    a.op(SUB, &[R6, space, R6]);
    a.op(ST8, &[clock_text, zero, R6]);
    a.op(MODU, &[R6, R4, ten]);
    a.op(ST8, &[clock_text, one, R6]);
    for (value, place) in [(sixty, 3u32), (one, 6)] {
        // Minutes, then seconds: two digits each.
        a.op(DIVU, &[R4, var(V_SEC), value]);
        a.op(MODU, &[R4, R4, sixty]);
        let (tens, ones) = (at(&mut a, place), at(&mut a, place + 1));
        a.op(DIVU, &[R6, R4, ten]);
        a.op(ST8, &[clock_text, tens, R6]);
        a.op(MODU, &[R6, R4, ten]);
        a.op(ST8, &[clock_text, ones, R6]);
    }
    a.op(DIVU, &[R5, R3, twelve]);
    a.op(MUL, &[R6, R5, p_from_a]);
    a.op(ADD, &[R6, R6, glyph_a]);
    let nine = a.k(9);
    a.op(ST8, &[clock_text, nine, R6]);

    let baseline = a.k(BASELINE);
    let boxed = |a: &mut Asm, [x, y, w, h]: [u32; 4]| [a.k(x), a.k(y), a.k(w), a.k(h)];
    let [cx, cy, cw, ch] = boxed(&mut a, CLOCK);
    let [sx, sy, sw, sh] = boxed(&mut a, [CLOCK[0] + 6 * ADVANCE, CLOCK[1], 2 * ADVANCE, CLOCK[3]]);
    a.branch(BEQ, var(V_CLOCK), zero, "seconds");
    a.op(FILL, &[frame, stride, cx, cy, cw, ch, zero]);
    a.op(GLYPHS, &[font, frame, stride, cx, baseline, mask, bright, clock_text, eleven]);
    a.op(DRAW, &[frame, stride, cx, cy, cx, cy, cw, ch]);
    a.op(MOV, &[var(V_CLOCK), zero]);
    a.op(MOV, &[var(V_SECS), zero]);
    a.label("seconds");
    a.branch(BEQ, var(V_SECS), zero, "battery");
    let seconds_text = a.k(CLOCK_TEXT + 6);
    a.op(FILL, &[frame, stride, sx, sy, sw, sh, zero]);
    a.op(GLYPHS, &[font, frame, stride, sx, baseline, mask, bright, seconds_text, two]);
    a.op(DRAW, &[frame, stride, sx, sy, sx, sy, sw, sh]);
    a.op(MOV, &[var(V_SECS), zero]);
    a.label("battery");
    a.branch(BEQ, var(V_BATT), zero, "text_done");
    // "100%", " 42%", "  7%" or "  ?%", and the bar: its width by the charge, dimmer at 20% and under.
    let (question, twenty, five, dim, bar_span, four) =
        (a.k(glyph('?')), a.k(20), a.k(5), a.k(8), a.k(BAR[2] - 1), a.k(4));
    a.op(MOV, &[R3, var(V_SHOWN_PCT)]);
    a.branch(BLTU, hundred, R3, "pct_unknown");
    a.op(DIVU, &[R4, R3, hundred]);
    a.op(MUL, &[R6, R4, one_from_space]);
    a.op(SUB, &[R6, space, R6]);
    a.op(ST8, &[batt_text, zero, R6]);
    a.op(MOV, &[R6, space]);
    a.branch(BLTU, R3, ten, "tens");
    a.op(DIVU, &[R6, R3, ten]);
    a.op(MODU, &[R6, R6, ten]);
    a.label("tens");
    a.op(ST8, &[batt_text, one, R6]);
    a.op(MODU, &[R6, R3, ten]);
    a.op(ST8, &[batt_text, two, R6]);
    a.op(MUL, &[R4, R3, bar_span]);
    a.op(DIVU, &[R4, R4, hundred]);
    a.op(ADD, &[R4, R4, one]);
    a.op(MOV, &[R5, bright]);
    a.branch(BLTU, twenty, R3, "bar_level");
    a.op(MOV, &[R5, dim]);
    a.label("bar_level");
    a.jump(JMP, "battery_draw");
    a.label("pct_unknown");
    a.op(ST8, &[batt_text, zero, space]);
    a.op(ST8, &[batt_text, one, space]);
    a.op(ST8, &[batt_text, two, question]);
    a.op(MOV, &[R4, one]);
    a.op(MOV, &[R5, five]);
    a.label("battery_draw");
    let [tx, ty, tw, th] = boxed(&mut a, BATTERY_TEXT);
    a.op(FILL, &[frame, stride, tx, ty, tw, th, zero]);
    a.op(GLYPHS, &[font, frame, stride, tx, baseline, mask, bright, batt_text, four]);
    let [bx, by, bw, bh] = boxed(&mut a, BAR);
    a.op(FILL, &[frame, stride, bx, by, bw, bh, zero]);
    a.op(FILL, &[frame, stride, bx, by, R4, bh, R5]);
    let [x, y, w, h] = boxed(&mut a, BATTERY);
    a.op(DRAW, &[frame, stride, x, y, x, y, w, h]);
    a.op(MOV, &[var(V_BATT), zero]);
    a.label("text_done");
    a.op(RET, &[]);

    // peer: a message from the other arm. Its battery; on the left also the right arm's clock against this one's,
    // from when it was read there (the low bits, in 256 µs steps) and when that was in this arm's clock.
    a.label("peer");
    a.op(SYS_READ, &[R0, p_msg]);
    a.op(SHR, &[R1, R0, k24]);
    a.op(MOV, &[var(V_OTHER), R1]);
    if side == Side::Left {
        let (k128, window, span) = (a.k(128), a.k(OFFSET_WINDOW), a.k(2 * OFFSET_WINDOW));
        a.op(SYS_READ, &[R2, p_t_us]);
        a.op(AND, &[R0, R0, low24]);
        a.op(SHL, &[R0, R0, eight]);
        a.op(ADD, &[R0, R0, k128]);
        a.op(SUB, &[R0, R0, R2]);
        a.op(SUB, &[R3, R0, var(V_OFF)]);
        a.op(ADD, &[R3, R3, window]);
        a.branch(BLTU, span, R3, "peer_done");
        a.op(MOV, &[var(V_OFF), R0]);
        a.label("peer_done");
    }
    a.op(END, &[]);

    let (tick, peer) = (a.address("tick"), a.address("peer"));
    let (code, pool) = a.finish()?;
    assert!(POOL + pool.len() as u32 <= VARS && CODE + code.len() as u32 <= v5::ARENA);
    Ok(Program { code, pool, tick, peer })
}

/// Stops whatever runs on `arm` and gives it the program, its eye's loop and a blank panel, ready to start.
async fn prepare(arm: &mut Arm, boost: u8) -> Result<(), Error> {
    let program = program(arm.side, boost)?;
    arm.control(v5::ctrl::VM_STOP, &[]).await?;
    crate::v0::upload_font(arm, SCALE).await?;
    let max = arm.max_code;
    let mut writes = v5::upload(TABLE, &table(arm.side), max);
    writes.extend(v5::upload(POOL, &program.pool, max));
    writes.extend(v5::upload(CODE, &program.code, max));
    let mut triggers = [0u32; 16];
    triggers[trigger::START as usize] = program.tick;
    triggers[trigger::TIMER as usize] = program.tick;
    triggers[trigger::PEER as usize] = program.peer;
    let triggers: Vec<u8> = triggers.iter().flat_map(|v| v.to_le_bytes()).collect();
    writes.push(v5::write(TRIGGERS, &triggers));
    // The text's fixed characters; the program fills in the rest.
    writes.push(v5::write(CLOCK_TEXT, &v5::text_indices(" 0:00:00 AM")));
    writes.push(v5::write(BATT_TEXT, &v5::text_indices("  ?%")));
    writes.push(v5::write(LAST, &[0; 64]));
    let middle: Vec<u8> = [320u32, 264, 321, 265].iter().flat_map(|v| v.to_le_bytes()).collect();
    writes.push(v5::write(LAST_RECT, &middle));
    writes.extend(v0_cube::blank_panel());
    // The left arm takes the right's brightness over the arm-to-arm link; the right sets its own.
    writes.push(v5::set(prop::PEER_LINK, 1));
    writes.push(v5::set(prop::AUTO_BRIGHTNESS, (arm.side == Side::Left) as u32));
    arm.run(writes).await?;
    arm.flush().await?;
    arm.sync_clock().await?;
    Ok(())
}

pub async fn cube_on_glasses(side: Option<Side>, boost: u8) -> Result<(), Error> {
    let mut arms = crate::v0::arms(side).await?;
    println!("uploading the program and the cube's {FRAMES} frames to the arms…");
    futures::future::try_join_all(arms.iter_mut().map(|arm| prepare(arm, boost))).await?;

    // Both arms keep the right arm's time, from frame 0 a second ago, and start on the second before this one.
    let host = v5_link::host_us();
    let wall = chrono::Local::now();
    let arm_now: Vec<u32> = arms.iter().map(|arm| arm.arm_time(host)).collect();
    let base = arms.iter().position(|arm| arm.side == Side::Right).unwrap_or(0);
    let right_now = arm_now[base];
    let second = wall.num_seconds_from_midnight();
    let into_second = (wall.nanosecond() / 1000).min(999_999);
    let mut offsets = Vec::new();
    for (arm, now) in arms.iter_mut().zip(arm_now) {
        let offset = right_now.wrapping_sub(now);
        offsets.push(offset);
        let mut vars = [0u32; VAR_WORDS as usize];
        vars[V_E as usize] = right_now.wrapping_sub(1_000_000);
        vars[V_K as usize] = u32::MAX;
        vars[V_SEC as usize] = (second + 86_399) % 86_400;
        vars[V_T_NEXT as usize] = right_now.wrapping_sub(into_second);
        vars[V_OFF as usize] = offset;
        vars[V_OTHER as usize] = 255;
        vars[V_SHOWN_PCT as usize] = u32::MAX;
        vars[V_SHOWN_MIN as usize] = u32::MAX;
        vars[V_BRIGHT as usize] = u32::MAX;
        let bytes: Vec<u8> = vars.iter().flat_map(|v| v.to_le_bytes()).collect();
        arm.run([v5::write(VARS, &bytes)]).await?;
        arm.flush().await?;
        arm.control(v5::ctrl::VM_START, &TRIGGERS.to_le_bytes()).await?;
    }

    // A look at how it runs before leaving it.
    tokio::time::sleep(Duration::from_secs(6)).await;
    for (arm, offset) in arms.iter_mut().zip(&offsets) {
        arm.poll().await?;
        while let Some(event) = arm.events.pop_front() {
            if let v5::EventKind::Halt { address, op, code, detail } = event.kind {
                return Err(format!(
                    "{} arm's program stopped at {address:#x} (op {op:#04x}): {} ({code}), detail {detail:#x}",
                    side_name(arm.side),
                    v5::error_name(code)
                )
                .into());
            }
        }
        let status = arm.control(v5::ctrl::VM_STATUS, &[]).await?;
        let (state, halt) = (status[0], status[15]);
        let tasks = u32::from_le_bytes(status[20..24].try_into().unwrap());
        let pauses = u16::from_le_bytes([status[29], status[30]]);
        let busy = status[31];
        if state != 1 {
            return Err(format!(
                "{} arm's program isn't running (state {state}, halt {} ({halt}))",
                side_name(arm.side),
                v5::error_name(halt)
            )
            .into());
        }
        let [peer, worn, in_case] = arm.props(&[prop::PEER_LINK, prop::WORN, prop::IN_CASE]).await?[..] else {
            return Err("short PROPS reply".into());
        };
        let mut line = format!(
            "{} arm: running, {tasks} tasks, busy {busy} ms of the last 100, {pauses} pauses at the cap, arm-to-arm \
             link {}, worn {}, in the case {}",
            side_name(arm.side),
            if peer == 2 { "up" } else { "quiet" },
            worn as i32,
            in_case as i32
        );
        if *offset != 0 {
            let at = VARS + V_OFF * 4;
            let mut body = at.to_le_bytes().to_vec();
            body.extend(4u32.to_le_bytes());
            let reply = arm.control(v5::ctrl::ARENA_CRC, &body).await?;
            let now = u32::from_le_bytes(reply[4..8].try_into().unwrap());
            line += &format!(", its clock offset {} µs from the Mac's measurement", now.wrapping_sub(*offset) as i32);
        }
        println!("{line}");
    }
    for arm in arms {
        arm.disconnect().await;
    }
    println!("running on the glasses; `erisg1 v0 stop` stops it");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_fits() {
        for side in [Side::Left, Side::Right] {
            let p = program(side, 5).unwrap();
            assert!(CODE <= p.tick && p.tick < p.peer && (p.peer as usize) < CODE as usize + p.code.len());
        }
        assert_eq!(FRAMES, 480);
        assert_eq!(table(Side::Left).len() as u32, FRAMES * FRAME_BYTES);
        assert!(v5::FONT + v5::font(SCALE).len() as u32 <= TABLE);
        assert!(TABLE + FRAMES * FRAME_BYTES <= POOL);
        assert!(VAR_WORDS * 4 <= CUR - VARS && CUR + FRAME_BYTES <= LAST && LAST + 64 <= LAST_RECT);
        assert!(LAST_RECT + 16 <= CLOCK_TEXT && CLOCK_TEXT + 11 <= BATT_TEXT && BATT_TEXT + 4 <= TRIGGERS);
        assert!(TRIGGERS + 64 <= CODE);
    }

    #[test]
    fn the_loop_comes_back_to_its_start() {
        let (first, _) = v0_cube::cube(0.0, 0.0);
        let (again, _) = v0_cube::cube(LOOP_SECS, 0.0);
        for (a, b) in first.iter().zip(&again) {
            assert!((a[0] - b[0]).abs() <= 1 && (a[1] - b[1]).abs() <= 1);
        }
    }

    #[test]
    fn frames_hold_their_rectangle() {
        let table = table(Side::Right);
        let words: Vec<u32> = table.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
        for frame in words.chunks(32) {
            let [x0, y0, x1, y1] = [28, 29, 30, 31].map(|i| frame[i] as i32);
            for c in frame[..16].chunks(2) {
                let (x, y) = (c[0] as i32, c[1] as i32);
                assert!(x0 <= x && x < x1 && y0 <= y && y < y1);
            }
            assert!(frame[16..28].iter().all(|l| (5..=15).contains(l)));
        }
    }

    #[test]
    fn glyphs_line_up() {
        assert_eq!(v5::text_indices("0123456789"), (0..10).collect::<Vec<u8>>());
        assert_eq!(glyph('P') - glyph('A'), 15);
    }
}
