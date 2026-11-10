//! `warp`: flying through a field of stars, in 3D. Each eye sees the stars from its own place, so they stream out
//! of the distance toward you as streaks, longer the faster you go. Turn your head to steer: the stars keep coming
//! from where you were heading, then swing round as the flight turns toward where you look. Tap for a burst of
//! speed, double tap to fly backwards.
//!
//! The stars are drawn as one list of short lines a frame (`Canvas::lines`, about two packets), in a band of the
//! panel that's cleared and shown each frame.

use super::fun::{Dirty, Rng, around, clip_to, segment, xywh};
use super::kit::{Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::proto::Side;
use crate::v0_pinned::{M, V, apply, exp, log, mul, scale, transpose};

pub(crate) const APP: App = App {
    name: "warp",
    about: "fly through stars in 3D: turn your head to steer, tap to speed up, double tap to fly backwards",
    fun: true,
    options: Options { font: 2, big: 0, imu: true, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(11.0)
        .head(|t| {
            // Ahead, then a look left and right.
            let turn = if t < 2.5 { 0.0 } else { 22.0 * ((t - 2.5) * 0.9).sin() };
            [turn, 5.0 * (t * 0.6).sin(), 0.0]
        })
        .touch(4.5, Side::Right, 1)
        .touch(8.0, Side::Left, 2)
        .touch(10.0, Side::Left, 2)
}

const STARS: usize = 48;
/// Stars come from this far and go once nearer than NEAR, metres.
const FAR: f64 = 32.0;
const NEAR: f64 = 1.5;
/// Speeds, m/s, and how fast a burst dies down (s).
const CRUISE: f64 = 12.0;
const BURST: f64 = 36.0;
const BURST_FADE: f64 = 1.2;
/// A streak is where the star was this long ago, seconds.
const STREAK: f64 = 0.1;
/// How long the flight takes to turn most of the way (1 − 1/e) toward where you look, seconds.
const STEER: f64 = 0.8;
/// The band of the panel the stars fly through (x0, y0, x1, y1), and where the speed shows below it.
const BAND: [i32; 4] = [0, 60, 640, 400];
const SPEED_AT: [i32; 2] = [264, 410];

/// A new star, in the flight's frame (x right, y up, z ahead): far away, or (flying backwards) fairly near; in
/// view either way.
fn spawn(rng: &mut Rng, forward: bool) -> V {
    let z = if forward { rng.range(0.75 * FAR, FAR) } else { rng.range(3.0, 8.0) };
    [rng.range(-0.28 * z, 0.28 * z), rng.range(-0.18 * z, 0.18 * z), z]
}

/// Whether a star (flight frame) is out of the band's view, so another takes its place.
fn gone(p: V) -> bool {
    p[2] < NEAR || p[2] > FAR + 5.0 || p[0].abs() > 0.3 * p[2] || p[1].abs() > 0.2 * p[2]
}

/// Nearer stars brighter.
fn level(z: f64) -> u8 {
    (15.0 - (z - 5.0) * 0.35).clamp(4.0, 15.0) as u8
}

/// `course` turned the share of the way toward `head` that a lag of STEER covers in `dt`.
fn steer(course: &M, head: &M, dt: f64) -> M {
    let gap = log(&mul(&transpose(course), head));
    mul(course, &exp(scale(gap, 1.0 - (-dt / STEER).exp())))
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let mut rng = Rng::new(0x5EED_57A2);
        let mut stars: Vec<V> = (0..STARS)
            .map(|_| {
                let z = rng.range(NEAR + 1.0, FAR);
                [rng.range(-0.25 * z, 0.25 * z), rng.range(-0.18 * z, 0.18 * z), z]
            })
            .collect();
        let identity = exp([0.0; 3]);
        let mut course = g.head().unwrap_or(identity);
        let (mut burst, mut forward) = (0.0f64, true);
        let mut dirty = Dirty::default();
        let mut shown: Option<String> = None;
        let mut last = g.now();
        while g.running() {
            for (_, gesture) in g.input().await? {
                match gesture {
                    Gesture::Tap => burst = 1.0,
                    Gesture::DoubleTap => forward = !forward,
                    _ => {}
                }
            }
            let now = g.now();
            let dt = (now - last).clamp(0.0, 0.2);
            last = now;
            burst *= (-dt / BURST_FADE).exp();
            let speed = (CRUISE + (BURST - CRUISE) * burst) * if forward { 1.0 } else { -1.0 };
            if let Some(head) = g.head() {
                course = steer(&course, &head, dt);
            }
            for star in &mut stars {
                star[2] -= speed * dt;
                if gone(*star) {
                    *star = spawn(&mut rng, forward);
                }
            }
            let readout = format!("{:>3} m/s", speed.round() as i32);
            let new_readout = shown.as_deref() != Some(readout.as_str());
            g.frame(|eye, c| {
                // A streak runs from the star back along its path.
                let behind = speed * STREAK;
                let lines: Vec<([f64; 4], u8)> = stars
                    .iter()
                    .filter_map(|s| {
                        let a = eye.from_room(apply(&course, [s[0], s[1], -s[2]]));
                        let b = eye.from_room(apply(&course, [s[0], s[1], -(s[2] + behind)]));
                        Some((clip_to(segment(eye, a, b)?, BAND)?, level(s[2])))
                    })
                    .collect();
                if let Some(rect) = dirty.take(eye.side, around(&lines, 1)) {
                    let [x, y, w, h] = xywh(rect);
                    c.clear(x, y, w, h);
                    c.lines(&lines);
                    c.show(x, y, w, h);
                }
                if new_readout {
                    let x = SPEED_AT[0] + eye.depth(2.0);
                    c.clear(SPEED_AT[0] - 40, SPEED_AT[1], c.width(&readout) + 80, c.size());
                    c.text(x, SPEED_AT[1], &readout, 8);
                    c.show(SPEED_AT[0] - 40, SPEED_AT[1], c.width(&readout) + 80, c.size());
                }
            })
            .await?;
            shown = Some(readout);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_start_far_and_leave_near_or_off_to_the_side() {
        let mut rng = Rng::new(1);
        for _ in 0..100 {
            let p = spawn(&mut rng, true);
            assert!(p[2] >= 0.75 * FAR && !gone(p));
        }
        assert!(gone([0.0, 0.0, 1.0]));
        assert!(gone([5.0, 0.0, 10.0]));
    }

    #[test]
    fn the_flight_turns_toward_the_head() {
        let head = exp([0.0, 0.5, 0.0]);
        let mut course = exp([0.0; 3]);
        for _ in 0..30 {
            course = steer(&course, &head, STEER / 30.0);
        }
        let left = crate::v0_pinned::norm(log(&mul(&transpose(&course), &head)));
        assert!((left - 0.5 / std::f64::consts::E).abs() < 1e-3);
    }
}
