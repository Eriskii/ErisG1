//! `fireworks`: rockets climb from below and burst into balls of sparks, in 3D: each burst hangs at its own
//! distance (1.5 to 5 m), and the sparks fly out, fall and fade. One goes up about every 1.5 s; tap for another.
//!
//! The sparks are short streaks along their paths, sent as one list of lines a frame (`Canvas::lines`, about two
//! packets); only the part of the panel they were in or are in is cleared and shown.

use super::fun::{Dirty, Rng, around, segment, xywh};
use super::kit::{Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::proto::Side;
use crate::v0_pinned::{V, add, scale};

pub(crate) const APP: App = App {
    name: "fireworks",
    about: "fireworks bursting in 3D at different distances; one every 1.5 s, tap for another",
    fun: true,
    options: Options { font: 2, big: 0, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(11.0).touch(2.2, Side::Right, 1).touch(5.6, Side::Left, 1).touch(8.3, Side::Right, 1)
}

const EVERY: f64 = 1.5;
/// Sparks a burst throws, and how long they last (s).
const SPARKS: usize = 20;
const LIFE: f64 = 1.6;
/// Gravity on the sparks and the rockets, m/s² (gentler than Earth's: a slow, wide fall).
const GRAVITY: f64 = 0.5;
/// A spark's streak is where it was this long ago, seconds.
const STREAK: f64 = 0.09;
/// Where bursts may be: how far, metres, and how high on the panel (rows).
const NEAR: f64 = 1.5;
const FAR: f64 = 5.0;
const BURST_ROWS: [f64; 2] = [140.0, 240.0];
/// At most this many lines a frame.
const MOST: usize = 64;

/// Something flying: a rocket going up or a spark. Positions are metres from the head: x right, y up, z ahead.
struct Spark {
    at: V,
    velocity: V,
    born: f64,
    life: f64,
    /// A rocket bursts when it gets this high.
    burst_at: Option<f64>,
}

fn launch(rng: &mut Rng, now: f64) -> Spark {
    let z = rng.range(NEAR, FAR);
    // From below the panel, aimed to burst at a row in BURST_ROWS.
    let row = rng.range(BURST_ROWS[0], BURST_ROWS[1]);
    let burst_y = (240.0 - row) * z / crate::v0_cube::FOCAL;
    let bottom = -260.0 * z / crate::v0_cube::FOCAL;
    let rise = burst_y - bottom;
    let up = (2.0 * GRAVITY * rise).sqrt() * 1.15;
    Spark {
        at: [rng.range(-0.18, 0.18) * z, bottom, z],
        velocity: [rng.range(-0.04, 0.04) * z, up, 0.0],
        born: now,
        life: 6.0,
        burst_at: Some(burst_y),
    }
}

/// A rocket's burst: sparks flying out from where it is, faster for further bursts so they look about as big.
fn burst(rng: &mut Rng, rocket: &Spark, now: f64) -> Vec<Spark> {
    let speed = 0.34 * rocket.at[2];
    (0..SPARKS)
        .map(|_| Spark {
            at: rocket.at,
            velocity: add(scale(rng.direction(), speed * rng.range(0.8, 1.0)), [0.0, 0.1 * speed, 0.0]),
            born: now,
            life: LIFE * rng.range(0.8, 1.1),
            burst_at: None,
        })
        .collect()
}

/// Moves everything on by `dt`, bursting the rockets that got high enough; drops what's burnt out.
fn step(sparks: &mut Vec<Spark>, rng: &mut Rng, now: f64, dt: f64) {
    let mut born = Vec::new();
    sparks.retain_mut(|s| {
        s.velocity[1] -= GRAVITY * dt;
        // Sparks slow in the air; rockets don't.
        if s.burst_at.is_none() {
            s.velocity = scale(s.velocity, (-dt * 0.9).exp());
        }
        s.at = add(s.at, scale(s.velocity, dt));
        match s.burst_at {
            Some(high) if s.at[1] >= high || s.velocity[1] <= 0.0 => {
                born.extend(burst(rng, s, now));
                false
            }
            _ => now - s.born < s.life,
        }
    });
    sparks.extend(born);
}

/// How bright a spark is: fading out over its life.
fn level(s: &Spark, now: f64) -> u8 {
    if s.burst_at.is_some() {
        return 12;
    }
    let left = 1.0 - (now - s.born) / s.life;
    (2.0 + 13.0 * left * left.sqrt()).clamp(2.0, 15.0) as u8
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let mut rng = Rng::new(0xF12E);
        let mut sparks: Vec<Spark> = Vec::new();
        let mut next = g.now() + 0.3;
        let mut dirty = Dirty::default();
        let mut last = g.now();
        while g.running() {
            let now = g.now();
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap {
                    sparks.push(launch(&mut rng, now));
                }
            }
            if now >= next {
                sparks.push(launch(&mut rng, now));
                next = now + EVERY * rng.range(0.8, 1.2);
            }
            let dt = (now - last).clamp(0.0, 0.2);
            last = now;
            step(&mut sparks, &mut rng, now, dt);
            // The newest first, if there are more than a frame takes.
            let shown: Vec<&Spark> = sparks.iter().rev().take(MOST).collect();
            g.frame(|eye, c| {
                let lines: Vec<([f64; 4], u8)> = shown
                    .iter()
                    .filter_map(|s| {
                        let tail = if s.burst_at.is_some() { 0.12 } else { STREAK };
                        let was = add(s.at, scale(s.velocity, -tail));
                        Some((segment(eye, eye.from_head(s.at), eye.from_head(was))?, level(s, now)))
                    })
                    .collect();
                if let Some(rect) = dirty.take(eye.side, around(&lines, 1)) {
                    let [x, y, w, h] = xywh(rect);
                    c.clear(x, y, w, h);
                    c.lines(&lines);
                    c.show(x, y, w, h);
                }
            })
            .await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rocket_rises_bursts_and_its_sparks_burn_out() {
        let mut rng = Rng::new(9);
        let mut sparks = vec![launch(&mut rng, 0.0)];
        let target = sparks[0].burst_at.unwrap();
        let mut t = 0.0;
        while sparks.len() == 1 && t < 10.0 {
            t += 0.02;
            step(&mut sparks, &mut rng, t, 0.02);
        }
        assert_eq!(sparks.len(), SPARKS);
        assert!(sparks[0].at[1] >= target - 0.05);
        // The burst is where the panel shows it: on a row in BURST_ROWS.
        let row = 240.0 - crate::v0_cube::FOCAL * sparks[0].at[1] / sparks[0].at[2];
        assert!((BURST_ROWS[0] - 20.0..BURST_ROWS[1] + 20.0).contains(&row), "{row}");
        for _ in 0..200 {
            t += 0.02;
            step(&mut sparks, &mut rng, t, 0.02);
        }
        assert!(sparks.is_empty());
    }

    #[test]
    fn sparks_fade() {
        let s = Spark { at: [0.0; 3], velocity: [0.0; 3], born: 0.0, life: 1.0, burst_at: None };
        assert!(level(&s, 0.0) > level(&s, 0.5) && level(&s, 0.5) > level(&s, 0.95));
    }
}
