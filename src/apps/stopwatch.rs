//! `stopwatch`: a stopwatch with laps that runs on the glasses alone. Tap either TouchBar to start and stop, double
//! tap for a lap (the last three show), hold to reset. Both arms keep the time from the same moments: a tap on one
//! reaches the other over the arm-to-arm link, and both act on it 200 ms later.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::proto::Side;
use crate::v5::trigger;
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "stopwatch",
    about: "stopwatch with laps, on the glasses alone: tap start/stop, double tap lap, hold reset",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(9.0)
        .touch(1.0, Side::Right, 1)
        .touch(3.0, Side::Left, 2)
        .touch(4.6, Side::Right, 2)
        .touch(6.4, Side::Left, 1)
        .touch(8.0, Side::Right, 4)
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move { g.deploy(program).await })
}

/// The time, in the big font (40 px): "MM:SS.t", its top left.
const TIME: [i32; 2] = [180, 150];
/// The laps, in the main font (16 px): "LAP nn  MM:SS.t", the first line's top left, and the lines' spacing.
const LAPS: [i32; 2] = [200, 230];
const LAP_STEP: i32 = 26;
const HINT: &str = "tap start/stop, 2 taps lap, hold reset";

/// Writes `us` (a word, µs) as "MM:SS.t" into `buf` + `at`.
fn put_time(p: &mut Prog, us: u16, buf: u32, at: u32) {
    let (tenth, ten) = (p.k(100_000), p.k(10));
    p.op(DIVU, &[R3, us, tenth]);
    p.op(MODU, &[R4, R3, ten]);
    p.op(DIVU, &[R3, R3, ten]);
    p.mmss(R3, buf, at);
    p.digits(R4, buf, at + 6, 1);
}

fn program(side: Side) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let [zero, one, two, four, ten, period] = [0, 1, 2, 4, 10, 100_000].map(|v| p.k(v));
    let (running, start, acc) = (p.var(0), p.var(0), p.var(0));
    let (tick, ticks, laps) = (p.var_start(0), p.var(0), p.var(0));
    let lap = [p.var(0), p.var(0), p.var(0)];
    let time = p.text("00:00.0");
    let lap_text = [p.text("LAP 00  00:00.0"), p.text("LAP 00  00:00.0"), p.text("LAP 00  00:00.0")];
    let hint = p.text(HINT);
    let size = p.size(Font::Big);
    let time_box = [TIME[0] - 4, TIME[1] - 4, 7 * size + 8, size + 8];
    let laps_box = [LAPS[0] - 8, LAPS[1] - 4, 15 * 16 + 16, 3 * LAP_STEP + 8];

    p.on(trigger::START, "start");
    p.on(trigger::TIMER, "tick");
    p.events("event");

    // START: everything drawn, the first tick at the shared start moment.
    p.label("start");
    p.fill_k(0, 0, 640, 480, 0);
    let hint_x = 320 - 8 * HINT.len() as i32;
    p.text_k(Font::Main, hint_x, 420, hint, 0, HINT.len() as u32, 6);
    p.draw_k(0, 400, 640, 60);
    p.jump(CALL, "draw_time");
    p.jump(CALL, "draw_laps");
    p.timer(tick);
    p.op(END, &[]);

    // TIMER, every 100 ms: the time while running, the brightness every second.
    p.label("tick");
    p.op(ADD, &[tick, tick, period]);
    p.timer(tick);
    p.op(ADD, &[ticks, ticks, one]);
    p.op(MODU, &[R0, ticks, ten]);
    p.branch(BNE, R0, zero, "tick_draw");
    p.brightness(APP.options.boost);
    p.label("tick_draw");
    p.branch(BEQ, running, zero, "tick_end");
    p.jump(CALL, "draw_time");
    p.label("tick_end");
    p.op(END, &[]);

    // elapsed: r2 = the time on the stopwatch, µs.
    p.label("elapsed");
    p.op(MOV, &[R2, acc]);
    p.branch(BEQ, running, zero, "elapsed_end");
    p.read(R3, crate::v5::prop::TIME_US);
    p.op(SUB, &[R3, R3, start]);
    p.op(ADD, &[R2, R2, R3]);
    p.label("elapsed_end");
    p.op(RET, &[]);

    p.label("draw_time");
    p.jump(CALL, "elapsed");
    put_time(&mut p, R2, time, 0);
    let [x, y, w, h] = time_box;
    p.fill_k(x, y, w, h, 0);
    p.text_k(Font::Big, TIME[0], TIME[1], time, 0, 7, 15);
    p.draw_k(x, y, w, h);
    p.op(RET, &[]);

    // The laps, newest first.
    p.label("draw_laps");
    let [x, y, w, h] = laps_box;
    p.fill_k(x, y, w, h, 0);
    for (i, (&lap, &text)) in lap.iter().zip(&lap_text).enumerate() {
        let skip = p.fresh("lap");
        let n = p.k(i as u32);
        p.branch(BGEU, n, laps, skip);
        p.op(SUB, &[R5, laps, n]);
        p.digits(R5, text, 4, 2);
        put_time(&mut p, lap, text, 8);
        let level = if i == 0 { 15 } else { 9 };
        p.text_k(Font::Main, LAPS[0], LAPS[1] + i as i32 * LAP_STEP, text, 0, 15, level);
        p.label(skip);
    }
    p.draw_k(x, y, w, h);
    p.op(RET, &[]);

    // A gesture (r0) at a moment both arms share (r1).
    p.label("event");
    p.branch(BEQ, R0, one, "tap");
    p.branch(BEQ, R0, two, "lap");
    p.branch(BEQ, R0, four, "reset");
    p.op(RET, &[]);
    p.label("tap");
    p.branch(BEQ, running, zero, "go");
    p.op(SUB, &[R3, R1, start]);
    p.op(ADD, &[acc, acc, R3]);
    p.op(MOV, &[running, zero]);
    p.jump(CALL, "draw_time");
    p.op(RET, &[]);
    p.label("go");
    p.op(MOV, &[start, R1]);
    p.op(MOV, &[running, one]);
    p.jump(CALL, "draw_time");
    p.op(RET, &[]);
    p.label("lap");
    p.branch(BEQ, running, zero, "lap_end");
    p.op(SUB, &[R3, R1, start]);
    p.op(ADD, &[R3, R3, acc]);
    p.op(MOV, &[lap[2], lap[1]]);
    p.op(MOV, &[lap[1], lap[0]]);
    p.op(MOV, &[lap[0], R3]);
    p.op(ADD, &[laps, laps, one]);
    p.jump(CALL, "draw_laps");
    p.label("lap_end");
    p.op(RET, &[]);
    p.label("reset");
    p.op(MOV, &[running, zero]);
    p.op(MOV, &[acc, zero]);
    p.op(MOV, &[laps, zero]);
    p.jump(CALL, "draw_time");
    p.jump(CALL, "draw_laps");
    p.op(RET, &[]);
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_for_both_arms() {
        for side in [Side::Left, Side::Right] {
            program(side).unwrap();
        }
    }
}
