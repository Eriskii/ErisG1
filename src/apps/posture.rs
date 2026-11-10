//! `posture`: a guardian for the neck and eyes that runs on the glasses alone and stays out of sight. Once a second
//! it reads the head's pitch. After `--minutes 3` of looking down (below −20°) without a break, a gentle "chin up"
//! comes up until the head has been up (above −10°) for 3 s. Every `--every 20` minutes an eye break asks you to
//! look 20 ft away for 20 s, counting down. A tap on either TouchBar dismisses either. Otherwise the panel sleeps.
//!
//! Seconds count from a moment both arms share, and a tap is taken in turn with them, so both eyes agree.

use super::kit::{Glasses, Options};
use super::sim::Script;
use super::vmkit::*;
use super::{App, Running};
use crate::proto::Side;
use crate::v5::{self, prop, trigger};
use crate::v5_asm::*;

pub(crate) const APP: App = App {
    name: "posture",
    about: "neck and eye guardian on the glasses alone: chin up after `--minutes 3` looking down, an eye break \
            `--every 20`; tap dismisses",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &["--minutes", "0.05", "--every", "0.16"],
};

/// The head down 30° from 1 s to 6 s, then up again.
fn demo() -> Script {
    Script::still(12.0)
        .head(|t| {
            let down = ((t - 1.0) / 0.5).clamp(0.0, 1.0) * (1.0 - ((t - 6.0) / 0.5).clamp(0.0, 1.0));
            [0.0, -30.0 * down, 0.0]
        })
        .touch(11.3, Side::Right, 1)
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let limits = limits(args)?;
        g.deploy(|side| program(side, limits)).await
    })
}

/// Seconds looking down before the reminder, and between eye breaks, from `--minutes` and `--every` in minutes.
fn limits(args: &[String]) -> Result<[u32; 2], String> {
    let mut minutes = [3.0, 20.0];
    let mut args = args.iter();
    while let Some(name) = args.next() {
        let slot = match name.as_str() {
            "--minutes" => 0,
            "--every" => 1,
            other => return Err(format!("posture: unknown argument {other}; it takes --minutes and --every")),
        };
        let value = args.next().ok_or(format!("posture: {name} needs minutes"))?;
        let value: f64 = value.parse().map_err(|_| format!("posture: {name} {value}: not a number"))?;
        if !(value > 0.0 && value <= 600.0) {
            return Err(format!("posture: {name} {value}: between 0 and 600 minutes"));
        }
        minutes[slot] = value;
    }
    Ok(minutes.map(|m| ((m * 60.0).round() as u32).max(1)))
}

const TICK_US: u32 = 1_000_000;
/// Pitch below this counts as looking down, above `UP` as up again, degrees.
const DOWN: i32 = -20;
const UP: i32 = -10;
/// Seconds up before the reminder goes, and the eye break's length.
const UP_SECS: u32 = 3;
const BREAK_SECS: u32 = 20;
/// Where everything shows: cleared, drawn into and copied to the panel.
const AREA: [i32; 4] = [100, 116, 440, 230];
/// The chevron over "chin up": its tip, how far it bobs, and its arms' reach.
const TIP: [i32; 2] = [320, 150];
const BOB: i32 = 6;
const REACH: i32 = 30;
const CHIN_TOP: i32 = 202;
const CHIN_HINT: &str = "lift your head a little";
const CHIN_HINT_TOP: i32 = 262;
const BREAK_TITLE: &str = "eye break";
const BREAK_LOOK: &str = "look 20 ft away";
const BREAK_HINT: &str = "tap to skip";

/// The x that centres `n` characters of `size` pixels.
fn centred(n: usize, size: i32) -> i32 {
    320 - n as i32 * size / 2
}

fn program(side: Side, [down_limit, every]: [u32; 2]) -> Result<Program, String> {
    let mut p = Prog::new(side, &APP.options);
    let [zero, one, two, period, unknown] = [0, 1, 2, TICK_US, v5::UNKNOWN].map(|v| p.k(v));
    let (down_secs, every_secs, up_secs, break_secs) = (p.k(down_limit), p.k(every), p.k(UP_SECS), p.k(BREAK_SECS));
    let (down_deg, up_deg) = (p.ki(DOWN), p.ki(UP));
    let tick = p.var_start(0);
    let (mode, down, up, since, left, bob) = (p.var(0), p.var(0), p.var(0), p.var(0), p.var(0), p.var(0));
    let (gesture, moment) = (p.var(0), p.var(0));
    let chin = p.text("chin up");
    let chin_hint = p.text(CHIN_HINT);
    let break_title = p.text(BREAK_TITLE);
    let break_look = p.text(BREAK_LOOK);
    let break_hint = p.text(BREAK_HINT);
    let countdown = p.text("20");
    let big = p.size(Font::Big);
    let [ax, ay, aw, ah] = AREA;

    p.on(trigger::START, "start");
    p.on(trigger::TIMER, "tick");
    p.events("event");

    // START: the panel asleep, the first second at the shared start moment.
    p.label("start");
    p.fill_k(0, 0, 640, 480, 0);
    p.set(prop::PANEL, two);
    p.timer(tick);
    p.op(END, &[]);

    // TIMER: the second due, unless a tap has already taken it.
    p.label("tick");
    p.read(R0, prop::TIME_US);
    p.op(SUB, &[R0, R0, tick]);
    p.branch(BLTS, R0, zero, "tick_early");
    p.jump(CALL, "step");
    p.op(END, &[]);
    p.label("tick_early");
    p.timer(tick);
    p.op(END, &[]);

    // step, once a second.
    p.label("step");
    p.op(ADD, &[tick, tick, period]);
    p.timer(tick);
    p.brightness(APP.options.boost);
    // The pitch; none yet counts as level.
    p.read(R0, prop::PITCH);
    p.branch(BNE, R0, unknown, "step_mode");
    p.op(MOV, &[R0, zero]);
    p.label("step_mode");
    p.branch(BEQ, mode, one, "chin");
    p.branch(BEQ, mode, two, "eye_break");

    // Watching: how long the head has been down, and since the last eye break.
    p.op(ADD, &[since, since, one]);
    p.branch(BLTS, R0, down_deg, "watch_down");
    p.op(MOV, &[down, zero]);
    p.jump(JMP, "watch_check");
    p.label("watch_down");
    p.op(ADD, &[down, down, one]);
    p.label("watch_check");
    p.branch(BLTU, down, down_secs, "watch_eyes");
    p.op(MOV, &[mode, one]);
    p.op(MOV, &[up, zero]);
    p.jump(CALL, "show_chin");
    p.op(RET, &[]);
    p.label("watch_eyes");
    p.branch(BLTU, since, every_secs, "step_end");
    p.op(MOV, &[mode, two]);
    p.op(MOV, &[left, break_secs]);
    p.jump(CALL, "show_break");
    p.label("step_end");
    p.op(RET, &[]);

    // Chin up: gone once the head has been up UP_SECS; meanwhile the chevron bobs.
    p.label("chin");
    p.op(ADD, &[since, since, one]);
    p.branch(BLTS, up_deg, R0, "chin_up");
    p.op(MOV, &[up, zero]);
    p.jump(JMP, "chin_bob");
    p.label("chin_up");
    p.op(ADD, &[up, up, one]);
    p.branch(BLTU, up, up_secs, "chin_bob");
    p.op(MOV, &[down, zero]);
    p.jump(CALL, "hide");
    p.op(RET, &[]);
    p.label("chin_bob");
    p.op(XOR, &[bob, bob, one]);
    p.jump(CALL, "draw_chevron");
    p.op(RET, &[]);

    // The eye break counts down to its end.
    p.label("eye_break");
    p.op(SUB, &[left, left, one]);
    p.branch(BNE, left, zero, "break_count");
    p.op(MOV, &[since, zero]);
    p.jump(CALL, "hide");
    p.op(RET, &[]);
    p.label("break_count");
    p.jump(CALL, "draw_countdown");
    p.op(RET, &[]);

    p.label("show_chin");
    p.fill_k(ax, ay, aw, ah, 0);
    p.text_k(Font::Big, centred(7, big), CHIN_TOP, chin, 0, 7, 14);
    p.text_k(Font::Main, centred(CHIN_HINT.len(), 16), CHIN_HINT_TOP, chin_hint, 0, CHIN_HINT.len() as u32, 8);
    p.op(MOV, &[bob, zero]);
    p.jump(CALL, "draw_chevron");
    p.draw_k(ax, ay, aw, ah);
    p.op(RET, &[]);

    // The chevron, BOB px higher every other second: the last one erased, this one drawn.
    p.label("draw_chevron");
    let band = [TIP[0] - REACH - 2, TIP[1] - BOB - 2, 2 * REACH + 5, REACH + BOB + 8];
    p.fill_k(band[0], band[1], band[2], band[3], 0);
    let bob_px = p.ki(BOB);
    let (tip_x, tip_y, reach) = (p.ki(TIP[0]), p.ki(TIP[1]), p.ki(REACH));
    let level = p.k(12);
    p.op(MUL, &[R3, bob, bob_px]);
    p.op(SUB, &[R3, tip_y, R3]);
    // Three lines thick: each arm from the tip down to each side.
    for _ in 0..3 {
        p.op(ADD, &[R4, R3, reach]);
        p.op(SUB, &[R5, tip_x, reach]);
        p.op(ADD, &[R6, tip_x, reach]);
        p.line(R5, R4, tip_x, R3, level);
        p.line(tip_x, R3, R6, R4, level);
        p.op(ADD, &[R3, R3, one]);
    }
    p.draw_k(band[0], band[1], band[2], band[3]);
    p.op(RET, &[]);

    p.label("show_break");
    p.fill_k(ax, ay, aw, ah, 0);
    p.text_k(Font::Main, centred(BREAK_TITLE.len(), 16), 140, break_title, 0, BREAK_TITLE.len() as u32, 7);
    p.text_k(Font::Main, centred(BREAK_LOOK.len(), 16), 172, break_look, 0, BREAK_LOOK.len() as u32, 15);
    p.text_k(Font::Main, centred(BREAK_HINT.len(), 16), 300, break_hint, 0, BREAK_HINT.len() as u32, 5);
    p.jump(CALL, "draw_countdown");
    p.draw_k(ax, ay, aw, ah);
    p.op(RET, &[]);

    p.label("draw_countdown");
    p.digits_spaced(left, countdown, 0, 2);
    let digits = [centred(2, big) - 4, 220, 2 * big + 8, big + 8];
    p.fill_k(digits[0], digits[1], digits[2], digits[3], 0);
    p.text_k(Font::Big, centred(2, big), 224, countdown, 0, 2, 14);
    p.draw_k(digits[0], digits[1], digits[2], digits[3]);
    p.op(RET, &[]);

    // hide: watching again, the panel asleep.
    p.label("hide");
    p.op(MOV, &[mode, zero]);
    p.fill_k(ax, ay, aw, ah, 0);
    p.set(prop::PANEL, two);
    p.op(RET, &[]);

    // A tap (r0 = 1) at a moment both arms share (r1), after the seconds due before it: dismisses what shows.
    p.label("event");
    p.op(MOV, &[gesture, R0]);
    p.op(MOV, &[moment, R1]);
    p.label("catch_up");
    p.op(SUB, &[R2, moment, tick]);
    p.branch(BLTS, R2, zero, "act");
    p.jump(CALL, "step");
    p.jump(JMP, "catch_up");
    p.label("act");
    p.branch(BNE, gesture, one, "act_end");
    p.branch(BEQ, mode, zero, "act_end");
    p.branch(BEQ, mode, one, "act_chin");
    p.op(MOV, &[since, zero]);
    p.jump(CALL, "hide");
    p.op(RET, &[]);
    p.label("act_chin");
    p.op(MOV, &[down, zero]);
    p.jump(CALL, "hide");
    p.label("act_end");
    p.op(RET, &[]);
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_for_both_arms() {
        for side in [Side::Left, Side::Right] {
            program(side, [180, 1200]).unwrap();
        }
    }

    #[test]
    fn takes_minutes() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(limits(&[]).unwrap(), [180, 1200]);
        assert_eq!(limits(&args(&["--minutes", "0.05", "--every", "0.16"])).unwrap(), [3, 10]);
        assert!(limits(&args(&["--minutes", "-1"])).is_err());
        assert!(limits(&args(&["--soon", "1"])).is_err());
    }
}
