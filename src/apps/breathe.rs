//! `breathe`: a breathing guide that runs on the glasses alone. A square grows as you breathe in, holds, shrinks as
//! you breathe out and holds again, with the phase below it and the seconds left in the phase inside it.
//! `--pattern 4-4-4-4` (box breathing, the default) gives each phase's seconds: in, hold, out and, optionally, hold;
//! 4-7-8 is the other one to hand. Tap either TouchBar to pause or go on; hold to switch between the two patterns.
//!
//! The square moves ten times a second, erased by drawing the last one again in black. Ticks come from a moment
//! both arms share, and gestures are taken in turn with them, so both eyes breathe together.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::proto::Side;
use crate::v5::{prop, trigger};
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "breathe",
    about: "breathing guide on the glasses alone: `--pattern 4-4-4-4` (or 4-7-8); tap pause, hold switch pattern",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &["--pattern", "3-2-3-1"],
};

fn demo() -> Script {
    Script::still(12.0).touch(6.0, Side::Right, 1).touch(7.2, Side::Left, 1).touch(9.0, Side::Right, 4)
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let patterns = patterns(args)?;
        g.deploy(|side| program(side, patterns)).await
    })
}

const BOX: [u32; 4] = [4, 4, 4, 4];
const RELAXING: [u32; 4] = [4, 7, 8, 0];

/// The pattern from `--pattern` (seconds in, hold, out and hold, 0–9 each, at least 1 for in and out), and the other
/// one that hold switches to.
fn patterns(args: &[String]) -> Result<[[u32; 4]; 2], String> {
    let given = match args {
        [] => BOX,
        [name, value] if name == "--pattern" => {
            let parts: Vec<u32> = value
                .split('-')
                .map(|s| s.parse().map_err(|_| format!("breathe: --pattern {value}: seconds like 4-7-8")))
                .collect::<Result<_, _>>()?;
            let phases = match parts[..] {
                [a, b, c] => [a, b, c, 0],
                [a, b, c, d] => [a, b, c, d],
                _ => return Err(format!("breathe: --pattern {value}: three or four phases")),
            };
            if phases.iter().any(|&s| s > 9) || phases[0] == 0 || phases[2] == 0 {
                return Err(format!("breathe: --pattern {value}: 0–9 s a phase, breathing in and out at least 1"));
            }
            phases
        }
        _ => return Err("breathe: it takes --pattern, like 4-4-4-4".into()),
    };
    Ok([given, if given == RELAXING { BOX } else { RELAXING }])
}

/// A pattern as text: "4-7-8", the last hold left out when it's 0.
fn label(pattern: [u32; 4]) -> String {
    let shown = if pattern[3] == 0 { &pattern[..3] } else { &pattern[..] };
    shown.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("-")
}

const TICK_US: u32 = 100_000;
/// The square's middle, and its smallest and largest sides; the inner line is INSET inside the outer.
const MIDDLE: [i32; 2] = [320, 226];
const SMALLEST: i32 = 64;
const LARGEST: i32 = 216;
const INSET: i32 = 6;
const PATTERN_TOP: i32 = 78;
const PHASE_TOP: i32 = 352;
const PHASES: [&str; 4] = ["breathe in", "hold", "breathe out", "hold"];
const HINT: &str = "tap pause, hold switch";
const HINT_TOP: i32 = 384;

fn program(side: Side, patterns: [[u32; 4]; 2]) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let [zero, one, two, four, ten, period] = [0, 1, 2, 4, 10, TICK_US].map(|v| p.k(v));
    let tick = p.var_start(0);
    let (count, paused, pattern, phase, t) = (p.var(0), p.var(0), p.var(0), p.var(0), p.var(0));
    let (size, shown_secs, gesture, moment) = (p.var(SMALLEST as u32), p.var(u32::MAX), p.var(0), p.var(0));
    // Each phase's length in ticks, pattern by pattern; each phase's words, and each pattern's label.
    let lengths: Vec<u8> = patterns.iter().flatten().flat_map(|s| (s * 10).to_le_bytes()).collect();
    let lengths = p.data(&lengths);
    let words: Vec<(u32, u32)> = PHASES.iter().map(|w| (p.text(w), w.len() as u32)).collect();
    let word_table: Vec<u8> = words.iter().flat_map(|&(at, n)| [at.to_le_bytes(), n.to_le_bytes()]).flatten().collect();
    let word_table = p.data(&word_table);
    let labels: Vec<(u32, u32)> = patterns.iter().map(|&pt| (p.text(&label(pt)), label(pt).len() as u32)).collect();
    let label_table: Vec<u8> =
        labels.iter().flat_map(|&(at, n)| [at.to_le_bytes(), n.to_le_bytes()]).flatten().collect();
    let label_table = p.data(&label_table);
    let digit = p.text("0");
    let paused_text = p.text("paused");
    let hint = p.text(HINT);
    let big = p.size(Font::Big);

    p.on(trigger::START, "start");
    p.on(trigger::TIMER, "tick");
    p.events("event");

    p.label("start");
    p.fill_k(0, 0, 640, 480, 0);
    p.text_k(Font::Main, 320 - 8 * HINT.len() as i32, HINT_TOP, hint, 0, HINT.len() as u32, 5);
    p.jump(CALL, "draw_pattern");
    p.jump(CALL, "draw_phase");
    let (smallest, size_level) = (p.ki(SMALLEST), p.k(15));
    p.op(MOV, &[R6, smallest]);
    p.op(MOV, &[R7, size_level]);
    p.jump(CALL, "square");
    p.jump(CALL, "draw_secs");
    p.draw_k(0, 60, 640, 350);
    p.timer(tick);
    p.op(END, &[]);

    // TIMER: the tick due, unless a gesture has already taken it.
    p.label("tick");
    p.read(R0, prop::TIME_US);
    p.op(SUB, &[R0, R0, tick]);
    p.branch(BLTS, R0, zero, "tick_early");
    p.jump(CALL, "step");
    p.op(END, &[]);
    p.label("tick_early");
    p.timer(tick);
    p.op(END, &[]);

    // step, ten times a second: on through the phase, into the next at its end; the square and the seconds.
    p.label("step");
    p.op(ADD, &[tick, tick, period]);
    p.timer(tick);
    p.op(ADD, &[count, count, one]);
    p.op(MODU, &[R0, count, ten]);
    p.branch(BNE, R0, zero, "step_breath");
    p.brightness(APP.options.boost);
    p.label("step_breath");
    p.branch(BNE, paused, zero, "step_end");
    p.op(ADD, &[t, t, one]);
    p.jump(CALL, "length");
    p.branch(BLTU, t, R5, "step_draw");
    // The next phase that lasts at all (in and out always do).
    p.op(MOV, &[t, zero]);
    p.label("step_next");
    p.op(ADD, &[phase, phase, one]);
    p.op(MODU, &[phase, phase, four]);
    p.jump(CALL, "length");
    p.branch(BEQ, R5, zero, "step_next");
    p.jump(CALL, "draw_phase");
    p.label("step_draw");
    p.jump(CALL, "move");
    p.jump(CALL, "draw_secs");
    p.label("step_end");
    p.op(RET, &[]);

    // length: r5 = this phase's length in ticks.
    p.label("length");
    p.op(MUL, &[R5, pattern, four]);
    p.op(ADD, &[R5, R5, phase]);
    let lengths_at = p.k(lengths);
    p.op(LD32, &[R5, lengths_at, R5]);
    p.op(RET, &[]);

    // move: the square's side for where the breath is: growing, full, shrinking or small. Erased and drawn again
    // only when it changes, and the larger of the two copied to the panel.
    p.label("move");
    let (span, largest) = (p.ki(LARGEST - SMALLEST), p.ki(LARGEST));
    p.jump(CALL, "length");
    p.op(MUL, &[R6, t, span]);
    p.op(DIVU, &[R6, R6, R5]);
    p.branch(BEQ, phase, zero, "move_in");
    p.branch(BEQ, phase, one, "move_full");
    p.branch(BEQ, phase, two, "move_out");
    p.op(MOV, &[R6, smallest]);
    p.jump(JMP, "move_set");
    p.label("move_in");
    p.op(ADD, &[R6, R6, smallest]);
    p.jump(JMP, "move_set");
    p.label("move_full");
    p.op(MOV, &[R6, largest]);
    p.jump(JMP, "move_set");
    p.label("move_out");
    p.op(SUB, &[R6, largest, R6]);
    p.label("move_set");
    p.branch(BEQ, R6, size, "move_end");
    p.op(MOV, &[R8, R6]);
    p.op(MOV, &[R6, size]);
    p.op(MOV, &[R7, zero]);
    p.jump(CALL, "square");
    p.op(MOV, &[R9, size]);
    p.op(MOV, &[size, R8]);
    p.op(MOV, &[R6, R8]);
    p.op(MOV, &[R7, size_level]);
    p.jump(CALL, "square");
    // The larger of the old (r9) and new sides, to the panel.
    p.op(MOV, &[R6, R8]);
    p.branch(BLTU, R9, R8, "move_draw");
    p.op(MOV, &[R6, R9]);
    p.label("move_draw");
    let (mx, my) = (p.ki(MIDDLE[0]), p.ki(MIDDLE[1]));
    p.op(DIVU, &[R0, R6, two]);
    p.op(SUB, &[R1, my, R0]);
    p.op(SUB, &[R0, mx, R0]);
    p.op(ADD, &[R2, R6, one]);
    p.draw(R0, R1, R2, R2);
    p.label("move_end");
    p.op(RET, &[]);

    // square: the outline of side r6 about the middle at level r7, and a dimmer one INSET inside it (also black
    // when r7 is).
    p.label("square");
    let (inset, inner_level) = (p.ki(2 * INSET), p.k(7));
    outline(&mut p, [mx, my, one, two]);
    p.op(SUB, &[R6, R6, inset]);
    p.branch(BEQ, R7, zero, "square_inner");
    p.op(MOV, &[R7, inner_level]);
    p.label("square_inner");
    outline(&mut p, [mx, my, one, two]);
    p.op(RET, &[]);

    // The seconds left in the phase, inside the square, when they change.
    p.label("draw_secs");
    p.jump(CALL, "length");
    p.op(SUB, &[R0, R5, t]);
    let nine = p.k(9);
    p.op(ADD, &[R0, R0, nine]);
    p.op(DIVU, &[R0, R0, ten]);
    p.branch(BEQ, R0, shown_secs, "secs_end");
    p.op(MOV, &[shown_secs, R0]);
    p.digits(R0, digit, 0, 1);
    let [x, y] = [MIDDLE[0] - big / 2, MIDDLE[1] - big / 2];
    p.fill_k(x - 2, y - 2, big + 4, big + 4, 0);
    p.text_k(Font::Big, x, y, digit, 0, 1, 12);
    p.draw_k(x - 2, y - 2, big + 4, big + 4);
    p.label("secs_end");
    p.op(RET, &[]);

    // The phase's words, centred, or "paused".
    p.label("draw_phase");
    let line = [320 - 6 * 16 - 8, PHASE_TOP - 4, 12 * 16 + 16, 24];
    p.fill_k(line[0], line[1], line[2], line[3], 0);
    let (eight, phase_top, level, table) = (p.k(8), p.ki(PHASE_TOP), p.k(13), p.k(word_table));
    p.branch(BNE, paused, zero, "phase_paused");
    p.op(MUL, &[R0, phase, two]);
    p.op(LD32, &[R2, table, R0]);
    p.op(ADD, &[R0, R0, one]);
    p.op(LD32, &[R3, table, R0]);
    p.op(MUL, &[R4, R3, eight]);
    p.op(SUB, &[R4, mx, R4]);
    p.glyphs(Font::Main, R4, phase_top, R2, R3, level);
    p.jump(JMP, "phase_shown");
    p.label("phase_paused");
    p.text_k(Font::Main, 320 - 48, PHASE_TOP, paused_text, 0, 6, 10);
    p.label("phase_shown");
    p.draw_k(line[0], line[1], line[2], line[3]);
    p.op(RET, &[]);

    // The pattern's label, dim, above.
    p.label("draw_pattern");
    let row = [320 - 4 * 16 - 8, PATTERN_TOP - 4, 8 * 16 + 16, 24];
    p.fill_k(row[0], row[1], row[2], row[3], 0);
    let (pattern_top, dim, table) = (p.ki(PATTERN_TOP), p.k(6), p.k(label_table));
    p.op(MUL, &[R0, pattern, two]);
    p.op(LD32, &[R2, table, R0]);
    p.op(ADD, &[R0, R0, one]);
    p.op(LD32, &[R3, table, R0]);
    p.op(MUL, &[R4, R3, eight]);
    p.op(SUB, &[R4, mx, R4]);
    p.glyphs(Font::Main, R4, pattern_top, R2, R3, dim);
    p.draw_k(row[0], row[1], row[2], row[3]);
    p.op(RET, &[]);

    // A gesture (r0) at a moment both arms share (r1), after the ticks due before it: tap pauses or goes on,
    // hold switches pattern and starts it from breathing in.
    p.label("event");
    p.op(MOV, &[gesture, R0]);
    p.op(MOV, &[moment, R1]);
    p.label("catch_up");
    p.op(SUB, &[R2, moment, tick]);
    p.branch(BLTS, R2, zero, "act");
    p.jump(CALL, "step");
    p.jump(JMP, "catch_up");
    p.label("act");
    p.branch(BEQ, gesture, one, "tap");
    p.branch(BEQ, gesture, four, "hold");
    p.op(RET, &[]);
    p.label("tap");
    p.op(XOR, &[paused, paused, one]);
    p.jump(CALL, "draw_phase");
    p.op(RET, &[]);
    p.label("hold");
    p.op(XOR, &[pattern, pattern, one]);
    p.op(MOV, &[phase, zero]);
    p.op(MOV, &[t, zero]);
    p.op(MOV, &[paused, zero]);
    let unshown = p.k(u32::MAX);
    p.op(MOV, &[shown_secs, unshown]);
    p.jump(CALL, "draw_pattern");
    p.jump(CALL, "draw_phase");
    p.jump(CALL, "move");
    p.jump(CALL, "draw_secs");
    p.op(RET, &[]);
    p.finish()
}

/// The outline of the square of side r6 about the middle at level r7, given the words holding the middle's x and y,
/// 1 and 2. Uses r0, r2–r5.
fn outline(p: &mut Prog, [mx, my, one, two]: [u16; 4]) {
    p.op(DIVU, &[R0, R6, two]);
    p.op(SUB, &[R2, mx, R0]);
    p.op(SUB, &[R3, my, R0]);
    p.op(ADD, &[R4, R2, R6]);
    p.op(SUB, &[R4, R4, one]);
    p.op(ADD, &[R5, R3, R6]);
    p.op(SUB, &[R5, R5, one]);
    p.line(R2, R3, R4, R3, R7);
    p.line(R4, R3, R4, R5, R7);
    p.line(R4, R5, R2, R5, R7);
    p.line(R2, R5, R2, R3, R7);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_for_both_arms() {
        for side in [Side::Left, Side::Right] {
            program(side, [BOX, RELAXING]).unwrap();
        }
    }

    #[test]
    fn reads_patterns() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(patterns(&[]).unwrap(), [BOX, RELAXING]);
        assert_eq!(patterns(&args(&["--pattern", "4-7-8"])).unwrap(), [RELAXING, BOX]);
        assert_eq!(patterns(&args(&["--pattern", "3-2-3-1"])).unwrap()[0], [3, 2, 3, 1]);
        assert!(patterns(&args(&["--pattern", "0-4-4"])).is_err());
        assert!(patterns(&args(&["--pattern", "4-12-4"])).is_err());
        assert_eq!(label(RELAXING), "4-7-8");
        assert_eq!(label(BOX), "4-4-4-4");
    }
}
