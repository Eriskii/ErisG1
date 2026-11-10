//! `pomodoro`: a focus timer that runs on the glasses alone. Focus and break phases (25 and 5 minutes unless
//! `--work` and `--rest` say otherwise, in minutes), a big countdown, the phase, a progress bar and the cycle. Tap
//! either TouchBar to pause or resume; hold to reset to a full focus phase, paused. When a phase ends a border
//! flashes for 3 s and the next phase starts.
//!
//! Time is kept in half-second ticks from a moment both arms share, and a gesture's moment (the same on both arms)
//! is taken in turn with the ticks: any tick due before it is counted first. So both eyes count down together even
//! when one arm is busy with the gesture as a tick comes due.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::proto::Side;
use crate::v5::trigger;
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "pomodoro",
    about: "focus timer on the glasses alone: `--work 25 --rest 5` (minutes); tap pause/resume, hold reset",
    fun: false,
    options: Options { font: 2, big: 6, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &["--work", "0.1", "--rest", "0.05"],
};

fn demo() -> Script {
    Script::still(12.0).touch(2.0, Side::Right, 1).touch(3.4, Side::Left, 1).touch(11.0, Side::Right, 4)
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let lengths = lengths(args)?;
        g.deploy(|side| program(side, lengths)).await
    })
}

/// The focus and break phases' lengths in seconds, from `--work` and `--rest` in minutes.
fn lengths(args: &[String]) -> Result<[u32; 2], String> {
    let mut minutes = [25.0, 5.0];
    let mut args = args.iter();
    while let Some(name) = args.next() {
        let slot = match name.as_str() {
            "--work" => 0,
            "--rest" => 1,
            other => return Err(format!("pomodoro: unknown argument {other}; it takes --work and --rest")),
        };
        let value = args.next().ok_or(format!("pomodoro: {name} needs minutes"))?;
        let value: f64 = value.parse().map_err(|_| format!("pomodoro: {name} {value}: not a number"))?;
        if !(value > 0.0 && value < 100.0) {
            return Err(format!("pomodoro: {name} {value}: between 0 and 100 minutes"));
        }
        minutes[slot] = value;
    }
    Ok(minutes.map(|m| ((m * 60.0).round() as u32).max(1)))
}

/// A tick every half second.
const TICK_US: u32 = 500_000;
/// The phase word, centred, its top.
const PHASE_TOP: i32 = 100;
/// The countdown in the big font (48 px): "MM:SS", its top left.
const TIME: [i32; 2] = [200, 148];
/// The progress bar's outline (x, y, w, h).
const BAR: [i32; 4] = [160, 226, 320, 16];
const CYCLE_TOP: i32 = 262;
const PAUSED_TOP: i32 = 298;
const HINT: &str = "tap pause, hold reset";
const HINT_TOP: i32 = 376;
/// The border that flashes when a phase ends: x0, y0, x1, y1 of its outer line.
const BORDER: [i32; 4] = [40, 70, 599, 409];
/// Ticks the border flashes for (3 s).
const FLASH_TICKS: u32 = 6;

/// A box around text of `n` main-font characters, centred, its top at `top`.
fn centred_box(n: i32, top: i32) -> [i32; 4] {
    [320 - 8 * n - 6, top - 4, 16 * n + 12, 24]
}

fn program(side: Side, [work, rest]: [u32; 2]) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let [zero, one, two, four, fifteen, period] = [0, 1, 2, 4, 15, TICK_US].map(|v| p.k(v));
    let (work_ticks, rest_ticks) = (p.k(2 * work), p.k(2 * rest));
    let tick = p.var_start(0);
    let (count, phase, cycle, running) = (p.var(0), p.var(0), p.var(1), p.var(1));
    let (left, flash, shown) = (p.var(2 * work), p.var(0), p.var(u32::MAX));
    let (border_level, gesture, moment) = (p.var(0), p.var(0), p.var(0));
    let focus = p.text("focus");
    let rest_text = p.text("break");
    let time = p.text("00:00");
    let cycle_text = p.text("cycle 01");
    let paused = p.text("paused");
    let hint = p.text(HINT);
    let big = p.size(Font::Big);

    p.on(trigger::START, "start");
    p.on(trigger::TIMER, "tick");
    p.events("event");

    // START: everything drawn, the first tick at the shared start moment.
    p.label("start");
    p.fill_k(0, 0, 640, 480, 0);
    p.text_k(Font::Main, 320 - 8 * HINT.len() as i32, HINT_TOP, hint, 0, HINT.len() as u32, 5);
    let [x, y, w, h] = BAR;
    p.line_k(x, y, x + w - 1, y, 9);
    p.line_k(x + w - 1, y, x + w - 1, y + h - 1, 9);
    p.line_k(x + w - 1, y + h - 1, x, y + h - 1, 9);
    p.line_k(x, y + h - 1, x, y, 9);
    p.jump(CALL, "draw_phase");
    p.jump(CALL, "draw_time");
    p.jump(CALL, "draw_bar");
    p.jump(CALL, "draw_paused");
    p.draw_k(0, 60, 640, 360);
    p.timer(tick);
    p.op(END, &[]);

    // TIMER: the tick due, unless a gesture has already counted it.
    p.label("tick");
    p.read(R0, crate::v5::prop::TIME_US);
    p.op(SUB, &[R0, R0, tick]);
    p.branch(BLTS, R0, zero, "tick_early");
    p.jump(CALL, "step");
    p.op(END, &[]);
    p.label("tick_early");
    p.timer(tick);
    p.op(END, &[]);

    // step, every half second: the brightness every second, the border while it flashes, the countdown while
    // running; at the end of a phase, the next.
    p.label("step");
    p.op(ADD, &[tick, tick, period]);
    p.timer(tick);
    p.op(ADD, &[count, count, one]);
    p.op(MODU, &[R0, count, two]);
    p.branch(BNE, R0, zero, "flash");
    p.brightness(APP.options.boost);
    p.label("flash");
    p.branch(BEQ, flash, zero, "count");
    p.op(SUB, &[flash, flash, one]);
    p.op(MODU, &[R0, flash, two]);
    p.op(MUL, &[border_level, R0, fifteen]);
    p.jump(CALL, "draw_border");
    p.label("count");
    p.branch(BEQ, running, zero, "tick_end");
    p.op(SUB, &[left, left, one]);
    p.branch(BEQ, left, zero, "next_phase");
    p.jump(CALL, "draw_time");
    p.jump(CALL, "draw_bar");
    p.label("tick_end");
    p.op(RET, &[]);
    p.label("next_phase");
    p.op(XOR, &[phase, phase, one]);
    p.op(MOV, &[left, rest_ticks]);
    p.branch(BNE, phase, zero, "phase_set");
    p.op(MOV, &[left, work_ticks]);
    p.op(ADD, &[cycle, cycle, one]);
    p.label("phase_set");
    let flashes = p.k(FLASH_TICKS);
    p.op(MOV, &[flash, flashes]);
    p.jump(CALL, "draw_phase");
    p.jump(CALL, "draw_time");
    p.jump(CALL, "draw_bar");
    p.op(RET, &[]);

    // The phase word and the cycle.
    p.label("draw_phase");
    let [x, y, w, h] = centred_box(5, PHASE_TOP);
    p.fill_k(x, y, w, h, 0);
    p.branch(BNE, phase, zero, "phase_break");
    p.text_k(Font::Main, 320 - 40, PHASE_TOP, focus, 0, 5, 15);
    p.jump(JMP, "phase_cycle");
    p.label("phase_break");
    p.text_k(Font::Main, 320 - 40, PHASE_TOP, rest_text, 0, 5, 12);
    p.label("phase_cycle");
    p.draw_k(x, y, w, h);
    p.digits(cycle, cycle_text, 6, 2);
    let [x, y, w, h] = centred_box(8, CYCLE_TOP);
    p.fill_k(x, y, w, h, 0);
    p.text_k(Font::Main, 320 - 64, CYCLE_TOP, cycle_text, 0, 8, 8);
    p.draw_k(x, y, w, h);
    p.op(RET, &[]);

    // The countdown, when the seconds it shows change: the half-second ticks left, rounded up.
    p.label("draw_time");
    p.op(ADD, &[R2, left, one]);
    p.op(DIVU, &[R2, R2, two]);
    p.branch(BEQ, R2, shown, "time_end");
    p.op(MOV, &[shown, R2]);
    p.mmss(R2, time, 0);
    let time_box = [TIME[0] - 4, TIME[1] - 4, 5 * big + 8, big + 8];
    let [x, y, w, h] = time_box;
    p.fill_k(x, y, w, h, 0);
    p.text_k(Font::Big, TIME[0], TIME[1], time, 0, 5, 15);
    p.draw_k(x, y, w, h);
    p.label("time_end");
    p.op(RET, &[]);

    // The bar: the share of the phase gone.
    p.label("draw_bar");
    let [x, y, w, h] = BAR;
    let inner = [x + 2, y + 2, w - 4, h - 4];
    let (inner_x, inner_y, inner_w, inner_h, twelve) =
        (p.ki(inner[0]), p.ki(inner[1]), p.ki(inner[2]), p.ki(inner[3]), p.k(12));
    p.op(MOV, &[R3, rest_ticks]);
    p.branch(BNE, phase, zero, "bar_total");
    p.op(MOV, &[R3, work_ticks]);
    p.label("bar_total");
    p.op(SUB, &[R4, R3, left]);
    p.op(MUL, &[R4, R4, inner_w]);
    p.op(DIVU, &[R4, R4, R3]);
    p.fill(inner_x, inner_y, inner_w, inner_h, zero);
    p.fill(inner_x, inner_y, R4, inner_h, twelve);
    p.draw_k(x, y, w, h);
    p.op(RET, &[]);

    p.label("draw_paused");
    let [x, y, w, h] = centred_box(6, PAUSED_TOP);
    p.fill_k(x, y, w, h, 0);
    p.branch(BNE, running, zero, "paused_end");
    p.text_k(Font::Main, 320 - 48, PAUSED_TOP, paused, 0, 6, 11);
    p.label("paused_end");
    p.draw_k(x, y, w, h);
    p.op(RET, &[]);

    // The border, two lines 4 px apart, at `border_level`.
    p.label("draw_border");
    let [x0, y0, x1, y1] = BORDER;
    for inset in [0, 4] {
        let [ax, ay, bx, by] = [x0 + inset, y0 + inset, x1 - inset, y1 - inset];
        let [ax, ay, bx, by] = [ax, ay, bx, by].map(|v| p.ki(v));
        p.line(ax, ay, bx, ay, border_level);
        p.line(bx, ay, bx, by, border_level);
        p.line(bx, by, ax, by, border_level);
        p.line(ax, by, ax, ay, border_level);
    }
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    p.draw_k(x0, y0, w, 6);
    p.draw_k(x0, y1 - 5, w, 6);
    p.draw_k(x0, y0, 6, h);
    p.draw_k(x1 - 5, y0, 6, h);
    p.op(RET, &[]);

    // A gesture (r0) at a moment both arms share (r1), after the ticks due before it: tap pauses or resumes, hold
    // resets.
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
    p.branch(BEQ, gesture, four, "reset");
    p.op(RET, &[]);
    p.label("tap");
    p.op(XOR, &[running, running, one]);
    p.jump(CALL, "draw_paused");
    p.op(RET, &[]);
    p.label("reset");
    p.op(MOV, &[running, zero]);
    p.op(MOV, &[phase, zero]);
    p.op(MOV, &[cycle, one]);
    p.op(MOV, &[left, work_ticks]);
    p.op(MOV, &[flash, zero]);
    p.op(MOV, &[border_level, zero]);
    let unshown = p.k(u32::MAX);
    p.op(MOV, &[shown, unshown]);
    p.jump(CALL, "draw_border");
    p.jump(CALL, "draw_phase");
    p.jump(CALL, "draw_time");
    p.jump(CALL, "draw_bar");
    p.jump(CALL, "draw_paused");
    p.op(RET, &[]);
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_for_both_arms() {
        for side in [Side::Left, Side::Right] {
            program(side, [1500, 300]).unwrap();
        }
    }

    #[test]
    fn takes_minutes() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(lengths(&[]).unwrap(), [1500, 300]);
        assert_eq!(lengths(&args(&["--work", "50", "--rest", "0.5"])).unwrap(), [3000, 30]);
        assert!(lengths(&args(&["--work"])).is_err());
        assert!(lengths(&args(&["--work", "0"])).is_err());
        assert!(lengths(&args(&["--long", "5"])).is_err());
    }
}
