//! `metronome`: a visual metronome that runs on the glasses alone, `--bpm 100 --beats 4` unless told otherwise. A
//! square flashes on each beat at exact times, filled on the bar's first beat and an outline on the others, and goes
//! out halfway to the next; a row of dots shows the beat in the bar, and the tempo shows above.
//!
//! Tap either TouchBar in time (taps more than 305 ms apart, as the TouchBar tells taps apart) to set the tempo
//! from the last few intervals: the bar starts again on the next beat. Double tap for 2, 3, 4 or 6 beats a bar;
//! hold to stop or start.
//!
//! Beats come from a moment both arms share, and gestures are taken in turn with them (any beat due before a
//! gesture's moment counted first), so both eyes flash together.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::proto::Side;
use crate::v5::{prop, trigger};
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "metronome",
    about: "visual metronome on the glasses alone: `--bpm 100 --beats 4`; tap tempo, double tap beats, hold stop",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0)
        .touch(2.0, Side::Right, 1)
        .touch(2.5, Side::Right, 1)
        .touch(3.0, Side::Right, 1)
        .touch(3.5, Side::Right, 1)
        .touch(6.5, Side::Left, 2)
        .touch(10.4, Side::Right, 4)
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let (bpm, beats) = settings(args)?;
        g.deploy(|side| program(side, bpm, beats)).await
    })
}

/// The tempo and the beats a bar, from `--bpm` (30–240) and `--beats` (2, 3, 4 or 6).
fn settings(args: &[String]) -> Result<(u32, u32), String> {
    let (mut bpm, mut beats) = (100, 4);
    let mut args = args.iter();
    while let Some(name) = args.next() {
        let value = args.next().ok_or(format!("metronome: {name} needs a number"))?;
        let number: u32 = value.parse().map_err(|_| format!("metronome: {name} {value}: not a whole number"))?;
        match name.as_str() {
            "--bpm" if (30..=240).contains(&number) => bpm = number,
            "--beats" if [2, 3, 4, 6].contains(&number) => beats = number,
            "--bpm" | "--beats" => {
                return Err(format!("metronome: {name} {value}: --bpm 30–240, --beats 2, 3, 4 or 6"));
            }
            other => return Err(format!("metronome: unknown argument {other}; it takes --bpm and --beats")),
        }
    }
    Ok((bpm, beats))
}

/// µs between beats at `bpm`.
fn period(bpm: u32) -> u32 {
    60_000_000 / bpm
}

/// The tempo, big digits, and "bpm" after them.
const BPM: [i32; 2] = [260, 76];
/// The square that flashes: x, y and side.
const SQUARE: [i32; 3] = [265, 160, 110];
/// How thick the outline on the beats after the first is.
const OUTLINE: i32 = 7;
/// The row of dots, one a beat: its top, a dot's side and the step from one to the next.
const DOTS: [i32; 3] = [300, 18, 30];
const STATUS_TOP: i32 = 336;
const HINT: &str = "tap tempo, 2 taps beats, hold stop";
const HINT_TOP: i32 = 376;
/// About how long after a tap its event comes: the TouchBar's 305 ms to tell taps apart, then the 200 ms both
/// arms wait. The bar after tap tempo starts a period after the last tap, less this.
const TAP_DELAY_US: u32 = 505_000;
/// Taps further apart than this start again; nearer than the TouchBar can tell apart, they don't count.
const TAP_GAP_US: [u32; 2] = [250_000, 2_000_000];

fn program(side: Side, bpm: u32, beats: u32) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let [zero, one, two, three, four, six, fifteen] = [0, 1, 2, 3, 4, 6, 15].map(|v| p.k(v));
    let next = p.var_start(0);
    let (period_us, beats_bar, beat, half) = (p.var(period(bpm)), p.var(beats), p.var(0), p.var(0));
    let (beat_at, running, count) = (p.var(0), p.var(1), p.var(0));
    let (last_tap, taps, gaps) = (p.var(0), p.var(0), [p.var(0), p.var(0), p.var(0)]);
    let (gesture, moment) = (p.var(0), p.var(0));
    let digits = p.text("100");
    let bpm_text = p.text("bpm");
    let stopped = p.text("stopped");
    let hint = p.text(HINT);
    let big = p.size(Font::Big);
    let [sx, sy, side_len] = SQUARE;

    p.on(trigger::START, "start");
    p.on(trigger::TIMER, "tick");
    p.events("event");

    p.label("start");
    p.fill_k(0, 0, 640, 480, 0);
    p.text_k(Font::Main, 320 - 8 * HINT.len() as i32, HINT_TOP, hint, 0, HINT.len() as u32, 5);
    p.text_k(Font::Main, BPM[0] + 3 * big + 8, BPM[1] + big - 16, bpm_text, 0, 3, 8);
    p.jump(CALL, "draw_bpm");
    p.draw_k(0, 60, 640, 340);
    p.timer(next);
    p.op(END, &[]);

    // TIMER: the beat or half beat due, unless a gesture has already taken it.
    p.label("tick");
    p.read(R0, prop::TIME_US);
    p.op(SUB, &[R0, R0, next]);
    p.branch(BLTS, R0, zero, "tick_early");
    p.jump(CALL, "step");
    p.op(END, &[]);
    p.label("tick_early");
    p.timer(next);
    p.op(END, &[]);

    // step: at a beat, the square and the dots; halfway to the next, the square out and on to the next beat.
    p.label("step");
    p.branch(BNE, half, zero, "step_off");
    p.op(MOV, &[beat_at, next]);
    p.branch(BEQ, running, zero, "step_on_quiet");
    p.jump(CALL, "draw_on");
    p.label("step_on_quiet");
    p.op(MOV, &[half, one]);
    p.op(DIVU, &[R0, period_us, two]);
    p.op(ADD, &[next, beat_at, R0]);
    p.jump(JMP, "step_end");
    p.label("step_off");
    p.branch(BEQ, running, zero, "step_off_quiet");
    p.jump(CALL, "draw_off");
    p.label("step_off_quiet");
    p.op(MOV, &[half, zero]);
    p.op(ADD, &[beat, beat, one]);
    p.branch(BLTU, beat, beats_bar, "step_bar");
    p.op(MOV, &[beat, zero]);
    p.label("step_bar");
    p.op(ADD, &[next, beat_at, period_us]);
    p.op(ADD, &[count, count, one]);
    p.op(MODU, &[R0, count, four]);
    p.branch(BNE, R0, zero, "step_end");
    p.brightness(APP.options.boost);
    p.label("step_end");
    p.timer(next);
    p.op(RET, &[]);

    // The square: filled on the bar's first beat, an outline on the others; and the dots.
    p.label("draw_on");
    p.branch(BNE, beat, zero, "on_outline");
    p.fill_k(sx, sy, side_len, side_len, 15);
    p.jump(JMP, "on_dots");
    p.label("on_outline");
    p.fill_k(sx, sy, side_len, OUTLINE, 15);
    p.fill_k(sx, sy + side_len - OUTLINE, side_len, OUTLINE, 15);
    p.fill_k(sx, sy, OUTLINE, side_len, 15);
    p.fill_k(sx + side_len - OUTLINE, sy, OUTLINE, side_len, 15);
    p.label("on_dots");
    p.draw_k(sx, sy, side_len, side_len);
    p.jump(CALL, "draw_dots");
    p.op(RET, &[]);

    p.label("draw_off");
    p.fill_k(sx, sy, side_len, side_len, 0);
    p.draw_k(sx, sy, side_len, side_len);
    p.op(RET, &[]);

    // The dots, centred: the beat's bright, the others dim. x0 = 320 − (step·beats − (step − dot)) / 2.
    p.label("draw_dots");
    let [top, dot, step] = DOTS;
    let row = [320 - 3 * step - 4, top - 2, 6 * step + 8, dot + 4];
    p.fill_k(row[0], row[1], row[2], row[3], 0);
    p.branch(BEQ, running, zero, "dots_shown");
    let (k_step, k_half_gap, k_top, k_dot, dim) =
        (p.ki(step), p.ki(320 + (step - dot) / 2), p.ki(top), p.ki(dot), p.k(4));
    p.op(MUL, &[R3, beats_bar, k_step]);
    p.op(DIVU, &[R3, R3, two]);
    p.op(SUB, &[R3, k_half_gap, R3]);
    for i in 0..6u32 {
        let (skip, put) = (p.fresh("dot"), p.fresh("dot_put"));
        let n = p.k(i);
        p.branch(BGEU, n, beats_bar, skip);
        p.op(MOV, &[R5, dim]);
        p.branch(BNE, n, beat, put);
        p.op(MOV, &[R5, fifteen]);
        p.label(put);
        p.fill(R3, k_top, k_dot, k_dot, R5);
        p.op(ADD, &[R3, R3, k_step]);
        p.label(skip);
    }
    p.label("dots_shown");
    p.draw_k(row[0], row[1], row[2], row[3]);
    p.op(RET, &[]);

    // The tempo: 60 000 000 / the period, rounded.
    p.label("draw_bpm");
    let minute = p.k(60_000_000);
    p.op(DIVU, &[R0, period_us, two]);
    p.op(ADD, &[R0, R0, minute]);
    p.op(DIVU, &[R0, R0, period_us]);
    p.digits_spaced(R0, digits, 0, 3);
    let bpm_box = [BPM[0] - 4, BPM[1] - 4, 3 * big + 8, big + 8];
    p.fill_k(bpm_box[0], bpm_box[1], bpm_box[2], bpm_box[3], 0);
    p.text_k(Font::Big, BPM[0], BPM[1], digits, 0, 3, 15);
    p.draw_k(bpm_box[0], bpm_box[1], bpm_box[2], bpm_box[3]);
    p.op(RET, &[]);

    p.label("draw_status");
    let status = [320 - 7 * 8 - 6, STATUS_TOP - 4, 7 * 16 + 12, 24];
    p.fill_k(status[0], status[1], status[2], status[3], 0);
    p.branch(BNE, running, zero, "status_end");
    p.text_k(Font::Main, 320 - 7 * 8, STATUS_TOP, stopped, 0, 7, 10);
    p.label("status_end");
    p.draw_k(status[0], status[1], status[2], status[3]);
    p.op(RET, &[]);

    // A gesture (r0) at a moment both arms share (r1), after the beats due before it.
    p.label("event");
    p.op(MOV, &[gesture, R0]);
    p.op(MOV, &[moment, R1]);
    p.label("catch_up");
    p.op(SUB, &[R2, moment, next]);
    p.branch(BLTS, R2, zero, "act");
    p.jump(CALL, "step");
    p.jump(JMP, "catch_up");
    p.label("act");
    p.branch(BEQ, gesture, one, "tap");
    p.branch(BEQ, gesture, two, "beats");
    p.branch(BEQ, gesture, four, "hold");
    p.op(RET, &[]);

    // Tap tempo: the interval since the last tap, if it's one, into the last three; the period their average.
    p.label("tap");
    let [near, far] = TAP_GAP_US.map(|v| p.k(v));
    p.op(SUB, &[R2, moment, last_tap]);
    p.op(MOV, &[last_tap, moment]);
    p.branch(BLTU, R2, near, "tap_again");
    p.branch(BLTU, far, R2, "tap_again");
    p.op(MOV, &[gaps[2], gaps[1]]);
    p.op(MOV, &[gaps[1], gaps[0]]);
    p.op(MOV, &[gaps[0], R2]);
    p.branch(BEQ, taps, three, "tap_full");
    p.op(ADD, &[taps, taps, one]);
    p.label("tap_full");
    p.op(MOV, &[R3, gaps[0]]);
    p.branch(BLTU, taps, two, "tap_sum");
    p.op(ADD, &[R3, R3, gaps[1]]);
    p.branch(BLTU, taps, three, "tap_sum");
    p.op(ADD, &[R3, R3, gaps[2]]);
    p.label("tap_sum");
    p.op(DIVU, &[period_us, R3, taps]);
    p.jump(CALL, "draw_bpm");
    // The bar starts again on the beat after the last tap.
    let delay = p.k(TAP_DELAY_US);
    p.op(SUB, &[R4, moment, delay]);
    p.op(ADD, &[R4, R4, period_us]);
    p.jump(CALL, "restart");
    p.op(RET, &[]);
    p.label("tap_again");
    p.op(MOV, &[taps, zero]);
    p.op(RET, &[]);

    p.label("beats");
    p.op(MOV, &[R2, three]);
    p.branch(BEQ, beats_bar, two, "beats_set");
    p.op(MOV, &[R2, four]);
    p.branch(BEQ, beats_bar, three, "beats_set");
    p.op(MOV, &[R2, six]);
    p.branch(BEQ, beats_bar, four, "beats_set");
    p.op(MOV, &[R2, two]);
    p.label("beats_set");
    p.op(MOV, &[beats_bar, R2]);
    p.op(MOV, &[beat, zero]);
    // The square out until the next beat, which starts the bar: the dots show it's next.
    p.jump(CALL, "draw_off");
    p.jump(CALL, "draw_dots");
    p.op(RET, &[]);

    // Hold: stop, or start again a moment later on the bar's first beat.
    p.label("hold");
    p.op(XOR, &[running, running, one]);
    p.jump(CALL, "draw_off");
    p.jump(CALL, "draw_dots");
    p.jump(CALL, "draw_status");
    p.branch(BEQ, running, zero, "hold_end");
    let soon = p.k(100_000);
    p.op(ADD, &[R4, moment, soon]);
    p.jump(CALL, "restart");
    p.label("hold_end");
    p.op(RET, &[]);

    // restart: the bar's first beat at r4 (moved on by periods until it's after the gesture), the square out.
    p.label("restart");
    let ahead = p.k(50_000);
    p.label("restart_later");
    p.op(SUB, &[R2, R4, moment]);
    p.branch(BGES, R2, ahead, "restart_set");
    p.op(ADD, &[R4, R4, period_us]);
    p.jump(JMP, "restart_later");
    p.label("restart_set");
    p.branch(BEQ, half, zero, "restart_dark");
    p.jump(CALL, "draw_off");
    p.label("restart_dark");
    p.op(MOV, &[next, R4]);
    p.op(MOV, &[half, zero]);
    p.op(MOV, &[beat, zero]);
    p.timer(next);
    p.op(RET, &[]);
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_for_both_arms() {
        for side in [Side::Left, Side::Right] {
            program(side, 100, 4).unwrap();
        }
    }

    #[test]
    fn takes_a_tempo_and_beats() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(settings(&[]).unwrap(), (100, 4));
        assert_eq!(settings(&args(&["--bpm", "72", "--beats", "3"])).unwrap(), (72, 3));
        assert!(settings(&args(&["--bpm", "400"])).is_err());
        assert!(settings(&args(&["--beats", "5"])).is_err());
        assert_eq!(period(120), 500_000);
    }
}
