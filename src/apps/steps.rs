//! `steps`: a pedometer that runs on the glasses alone. The right arm reads its accelerometer every 40 ms and
//! counts a step at each peak of the acceleration's size (over 1.15 g, after it fell under 1.05 g, at least 250 ms
//! after the last), and once a second sends the count to the left arm; both show it at the same moment: steps,
//! steps a minute over the last 5 s, and the distance at 0.75 m a step. Tap either TouchBar to hide or show it (the
//! panel sleeps while hidden, the count goes on), hold to reset.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::proto::Side;
use crate::v5::{prop, trigger};
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "steps",
    about: "pedometer on the glasses alone: steps, steps a minute, distance; tap hides/shows, hold resets",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0)
        .accel(|t| {
            // Walking from 0.5 s to 10.5 s: the head bounces about 1.8 times a second.
            let bounce = if (0.5..10.5).contains(&t) { 250.0 * (std::f64::consts::TAU * 1.8 * t).sin() } else { 0.0 };
            [0.0, bounce, 0.0]
        })
        .touch(6.0, Side::Left, 1)
        .touch(7.5, Side::Right, 1)
        .touch(11.0, Side::Right, 4)
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move { g.deploy(program).await })
}

const SAMPLE_US: u32 = 40_000;
/// A sample's size squared (mg²) over HIGH is a peak; under LOW the next one may count.
const HIGH: u32 = 1150 * 1150;
const LOW: u32 = 1050 * 1050;
const STEP_GAP_US: u32 = 250_000;
/// Samples between sending the count: once a second.
const SEND_EVERY: u32 = 1_000_000 / SAMPLE_US;
/// Messages carry the count above this, so they're never taken for a gesture.
const COUNT: u32 = 0x10000;

/// Where things go, all centred: the label, the count (big), steps a minute, distance; the hint.
const LABEL: [i32; 2] = [280, 110];
const BIG_TOP: i32 = 140;
const PACE_TOP: i32 = 210;
const DISTANCE_TOP: i32 = 238;
/// The part redrawn each second.
const VALUES: [i32; 4] = [200, 136, 240, 124];
const HINT: &str = "tap: hide/show   hold: reset";

/// Writes `value` (a word) as `digits` digits into `buf`, whose next `suffix` characters follow it (" m"), and
/// draws it centred on the panel without the leading zeros. Uses r7–r10.
#[allow(clippy::too_many_arguments)]
fn centred(p: &mut Prog, value: u16, buf: u32, digits: u32, suffix: u32, font: Font, top: i32, level: u8) {
    p.digits(value, buf, 0, digits);
    // r7: how many digits it has.
    let (one, done) = (p.k(1), p.fresh("width"));
    p.op(MOV, &[R7, one]);
    let mut limit = 10;
    for _ in 1..digits {
        let k = p.k(limit);
        p.branch(BLTU, value, k, done);
        p.op(ADD, &[R7, R7, one]);
        limit *= 10;
    }
    p.label(done);
    // r8 characters from r10, at x r9.
    let half = (p.size(font) / 2) as u32;
    let [suffix, half, centre, end] = [suffix, half, 320, buf + digits].map(|v| p.k(v));
    p.op(ADD, &[R8, R7, suffix]);
    p.op(MUL, &[R9, R8, half]);
    p.op(SUB, &[R9, centre, R9]);
    p.op(SUB, &[R10, end, R7]);
    let (top, level) = (p.ki(top), p.k(level as u32));
    p.glyphs(font, R9, top, R10, R8, level);
}

fn program(side: Side) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let [zero, one, two, four, twelve, three] = [0, 1, 2, 4, 12, 3].map(|v| p.k(v));
    let (visible, shown) = (p.var(1), p.var(0));
    // The counts received, newest first, a second apart: steps a minute from the newest and the one 5 s before.
    let seen: Vec<u16> = (0..6).map(|_| p.var(0)).collect();
    let big = p.text("    0");
    let pace = p.text("  0 steps/min");
    let distance = p.text("    0 m");
    let label = p.text("STEPS");
    let hint = p.text(HINT);

    p.on(trigger::START, "start");
    if p.right() {
        p.on(trigger::TIMER, "sample");
    }
    p.events("event");

    // The right arm counts.
    let (tick, samples, count, armed, last_step) = (p.var_start(0), p.var(0), p.var(0), p.var(1), p.var(0));
    p.label("start");
    p.jump(CALL, "draw_all");
    if p.right() {
        let rate = p.k(52);
        p.set(prop::IMU_RATE, rate);
        p.timer(tick);
    }
    p.op(END, &[]);

    if p.right() {
        let [period, high, low, gap, every, offset] =
            [SAMPLE_US, HIGH, LOW, STEP_GAP_US, SEND_EVERY, COUNT].map(|v| p.k(v));
        p.label("sample");
        p.op(ADD, &[tick, tick, period]);
        p.timer(tick);
        // The acceleration's size squared, mg² (signed values square the same in wrapping arithmetic).
        p.read(R0, prop::ACC_X);
        p.read(R1, prop::ACC_Y);
        p.read(R2, prop::ACC_Z);
        p.op(MUL, &[R0, R0, R0]);
        p.op(MUL, &[R1, R1, R1]);
        p.op(MUL, &[R2, R2, R2]);
        p.op(ADD, &[R0, R0, R1]);
        p.op(ADD, &[R0, R0, R2]);
        p.branch(BGEU, R0, low, "not_low");
        p.op(MOV, &[armed, one]);
        p.jump(JMP, "sampled");
        p.label("not_low");
        p.branch(BLTU, R0, high, "sampled");
        p.branch(BEQ, armed, zero, "sampled");
        p.read(R3, prop::TIME_US);
        p.op(SUB, &[R4, R3, last_step]);
        p.branch(BLTU, R4, gap, "sampled");
        p.op(ADD, &[count, count, one]);
        p.op(MOV, &[armed, zero]);
        p.op(MOV, &[last_step, R3]);
        p.label("sampled");
        p.op(ADD, &[samples, samples, one]);
        p.op(MODU, &[R5, samples, every]);
        p.branch(BNE, R5, zero, "sample_end");
        p.op(ADD, &[R6, count, offset]);
        p.broadcast(R6);
        p.brightness(APP.options.boost);
        p.label("sample_end");
        p.op(END, &[]);
    }

    // A gesture or the count (r0), at the moment both arms share.
    p.label("event");
    let offset = p.k(COUNT);
    p.branch(BGEU, R0, offset, "counted");
    p.branch(BEQ, R0, one, "tap");
    p.branch(BEQ, R0, four, "reset");
    p.op(RET, &[]);
    p.label("counted");
    p.op(SUB, &[R2, R0, offset]);
    for i in (1..seen.len()).rev() {
        p.op(MOV, &[seen[i], seen[i - 1]]);
    }
    p.op(MOV, &[seen[0], R2]);
    p.op(MOV, &[shown, R2]);
    p.branch(BEQ, visible, zero, "event_end");
    p.jump(CALL, "draw_values");
    p.op(RET, &[]);
    p.label("tap");
    p.branch(BEQ, visible, zero, "unhide");
    p.op(MOV, &[visible, zero]);
    p.set(prop::PANEL, two);
    p.op(RET, &[]);
    p.label("unhide");
    p.op(MOV, &[visible, one]);
    p.jump(CALL, "draw_all");
    p.op(RET, &[]);
    p.label("reset");
    p.op(MOV, &[count, zero]);
    p.op(MOV, &[armed, one]);
    p.op(MOV, &[shown, zero]);
    for &s in &seen {
        p.op(MOV, &[s, zero]);
    }
    p.branch(BEQ, visible, zero, "event_end");
    p.jump(CALL, "draw_values");
    p.label("event_end");
    p.op(RET, &[]);

    // The count, steps a minute and distance.
    p.label("draw_values");
    let [x, y, w, h] = VALUES;
    p.fill_k(x, y, w, h, 0);
    centred(&mut p, shown, big, 5, 0, Font::Big, BIG_TOP, 15);
    p.op(SUB, &[R3, seen[0], seen[5]]);
    p.op(MUL, &[R3, R3, twelve]);
    centred(&mut p, R3, pace, 3, 10, Font::Main, PACE_TOP, 12);
    p.op(MUL, &[R4, shown, three]);
    p.op(DIVU, &[R4, R4, four]);
    centred(&mut p, R4, distance, 5, 2, Font::Main, DISTANCE_TOP, 12);
    p.draw_k(x, y, w, h);
    p.op(RET, &[]);

    // Everything, after the panel slept.
    p.label("draw_all");
    p.fill_k(0, 90, 640, 310, 0);
    p.text_k(Font::Main, LABEL[0], LABEL[1], label, 0, 5, 8);
    p.text_k(Font::Main, 320 - 8 * HINT.len() as i32, 380, hint, 0, HINT.len() as u32, 6);
    p.jump(CALL, "draw_values");
    p.draw_k(0, 90, 640, 310);
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

    #[test]
    fn walking_crosses_both_thresholds() {
        // The demo's bounce: 1 g ± 250 mg goes over HIGH and under LOW each stride.
        let (top, bottom) = (1250u32 * 1250, 750u32 * 750);
        assert!(top > HIGH && bottom < LOW);
        assert_eq!(SEND_EVERY, 25);
    }
}
