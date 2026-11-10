//! `dodge`: wireframe rocks tumble toward you out of the distance, in 3D; steer your little ship out of their way by
//! turning your head (left, right, up, down: the head is the joystick, and the view stays still). A rock that
//! reaches the ship flashes the border and costs one of three lives; the score is how long you last, and the rocks
//! come faster and thicker as it goes. Tap to play again after the game is over.
//!
//! The ship flies 1.5 m ahead and the rocks go once they pass it, so nothing comes nearer than about 1.3 m. Each
//! frame is one list of lines (`Canvas::lines`): the rocks, the ship, the border when it flashes.

use super::fun::{Dirty, Rng, around, clip_to, segment, xywh};
use super::kit::{Eye, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::v0_pinned::{M, V, add, apply, exp, mul, scale, transpose};

pub(crate) const APP: App = App {
    name: "dodge",
    about: "dodge 3D rocks flying at you by turning your head; three lives, score by time; tap to play again",
    fun: true,
    options: Options { font: 2, big: 0, imu: true, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0).head(|t| {
        let t = (t - 1.0).max(0.0);
        [14.0 * (t * 1.3).sin() + 6.0 * (t * 0.4).sin(), 7.0 * (t * 0.9).cos() - 2.0, 0.0]
    })
}

/// The ship's plane, metres ahead, and how far it may go from the middle (on the panel about ±300 and ±170 px).
const SHIP_Z: f64 = 1.5;
const ROOM: [f64; 2] = [0.36, 0.2];
/// Metres of ship movement per degree of head turn, and how quickly it follows (s).
const GAIN: f64 = 0.012;
const SMOOTH: f64 = 0.08;
const SHIP_SIZE: f64 = 0.04;
/// Where rocks come from, metres.
const FAR: f64 = 25.0;
/// The band of the panel the game is in (x0, y0, x1, y1), and the score line above it.
const BAND: [i32; 4] = [0, 64, 640, 416];
const SCORE_Y: i32 = 40;
const LIVES: u32 = 3;
/// How long the border flashes after a hit, seconds.
const FLASH: f64 = 0.35;

/// A rock: where it is (x right, y up, z ahead, metres), its size, its shape, and how it tumbles.
struct Rock {
    at: V,
    size: f64,
    eight: bool,
    axis: V,
    spin: f64,
}

/// Unit shapes: an octahedron (6 corners, 12 edges) and a tetrahedron (4, 6).
const OCTA: [V; 6] =
    [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
const OCTA_EDGES: [(usize, usize); 12] =
    [(0, 2), (0, 3), (0, 4), (0, 5), (1, 2), (1, 3), (1, 4), (1, 5), (2, 4), (2, 5), (3, 4), (3, 5)];
const TETRA: [V; 4] = [[1.0, 1.0, 1.0], [1.0, -1.0, -1.0], [-1.0, 1.0, -1.0], [-1.0, -1.0, 1.0]];
const TETRA_EDGES: [(usize, usize); 6] = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];

impl Rock {
    fn new(rng: &mut Rng) -> Rock {
        Rock {
            at: [rng.range(-ROOM[0], ROOM[0]), rng.range(-ROOM[1], ROOM[1]), FAR],
            size: rng.range(0.06, 0.11),
            eight: rng.next() < 0.6,
            axis: rng.direction(),
            spin: rng.range(1.0, 3.0),
        }
    }

    /// Its edges at time `t`, in the head's terms.
    fn edges(&self, t: f64) -> Vec<(V, V)> {
        let turn = exp(scale(self.axis, self.spin * t));
        let corner = |p: V| add(self.at, apply(&turn, scale(p, if self.eight { self.size } else { self.size * 0.6 })));
        if self.eight {
            OCTA_EDGES.iter().map(|&(a, b)| (corner(OCTA[a]), corner(OCTA[b]))).collect()
        } else {
            TETRA_EDGES.iter().map(|&(a, b)| (corner(TETRA[a]), corner(TETRA[b]))).collect()
        }
    }
}

/// The head's turn as left and up angles, degrees, from `zero`.
fn angles(zero: &M, head: &M) -> [f64; 2] {
    let ahead = apply(&mul(&transpose(zero), head), [0.0, 0.0, -1.0]);
    [(-ahead[0]).atan2(-ahead[2]).to_degrees(), ahead[1].clamp(-1.0, 1.0).asin().to_degrees()]
}

/// Where the ship heads for, given the head's angles: turning left moves it left.
fn ship_target([left, up]: [f64; 2]) -> [f64; 2] {
    [(-left * GAIN).clamp(-ROOM[0], ROOM[0]), (up * GAIN).clamp(-ROOM[1], ROOM[1])]
}

/// Whether a rock crossing the ship's plane at `rock` hits a ship at `ship`.
fn hits(rock: &Rock, ship: [f64; 2]) -> bool {
    let (dx, dy) = (rock.at[0] - ship[0], rock.at[1] - ship[1]);
    (dx * dx + dy * dy).sqrt() < rock.size * 0.8 + SHIP_SIZE
}

struct Game {
    rocks: Vec<Rock>,
    ship: [f64; 2],
    lives: u32,
    started: f64,
    score: f64,
    over: bool,
    next_rock: f64,
    flash_until: f64,
}

impl Game {
    fn new(now: f64) -> Game {
        Game {
            rocks: Vec::new(),
            ship: [0.0; 2],
            lives: LIVES,
            started: now,
            score: 0.0,
            over: false,
            next_rock: now + 1.0,
            flash_until: 0.0,
        }
    }

    /// How fast rocks come (m/s) and how often (s), harder as the game goes on.
    fn pace(&self, now: f64) -> (f64, f64) {
        let played = now - self.started;
        ((7.0 + 0.25 * played).min(15.0), (1.1 - 0.02 * played).max(0.45))
    }

    fn step(&mut self, now: f64, dt: f64, target: [f64; 2], rng: &mut Rng) {
        let follow = 1.0 - (-dt / SMOOTH).exp();
        self.ship = [0, 1].map(|i| self.ship[i] + (target[i] - self.ship[i]) * follow);
        if self.over {
            return;
        }
        self.score = now - self.started;
        let (speed, every) = self.pace(now);
        if now >= self.next_rock && self.rocks.len() < 4 {
            self.rocks.push(Rock::new(rng));
            self.next_rock = now + every * rng.range(0.7, 1.3);
        }
        let ship = self.ship;
        let mut hit = false;
        self.rocks.retain_mut(|rock| {
            let before = rock.at[2];
            rock.at[2] -= speed * dt;
            if before >= SHIP_Z && rock.at[2] < SHIP_Z && hits(rock, ship) {
                hit = true;
                return false;
            }
            rock.at[2] > SHIP_Z - 0.2
        });
        if hit {
            self.lives = self.lives.saturating_sub(1);
            self.flash_until = now + FLASH;
            self.over = self.lives == 0;
        }
    }

    /// One eye's lines: the rocks, the ship, the border while it flashes.
    fn lines(&self, eye: &Eye, now: f64) -> Vec<([f64; 4], u8)> {
        let mut lines = Vec::new();
        if self.over {
            // Cleared away for the message.
            return lines;
        }
        let mut add_line = |a: V, b: V, level: u8| {
            if let Some(s) = segment(eye, eye.from_head(a), eye.from_head(b)).and_then(|s| clip_to(s, BAND)) {
                lines.push((s, level));
            }
        };
        for rock in &self.rocks {
            let level = (15.0 - (rock.at[2] - 3.0) * 0.4).clamp(5.0, 15.0) as u8;
            for (a, b) in rock.edges(now) {
                add_line(a, b, level);
            }
        }
        // The ship: a small arrowhead with a fin.
        let [x, y] = self.ship;
        let s = SHIP_SIZE;
        let p = |dx: f64, dy: f64| [x + dx * s, y + dy * s, SHIP_Z];
        for (a, b) in [
            ((0.0, 0.8), (-1.0, -0.6)),
            ((-1.0, -0.6), (0.0, -0.2)),
            ((0.0, -0.2), (1.0, -0.6)),
            ((1.0, -0.6), (0.0, 0.8)),
        ] {
            add_line(p(a.0, a.1), p(b.0, b.1), 15);
        }
        if now < self.flash_until {
            let [x0, y0, x1, y1] = BAND.map(|v| v as f64);
            let (x1, y1) = (x1 - 1.0, y1 - 1.0);
            for l in [[x0, y0, x1, y0], [x1, y0, x1, y1], [x1, y1, x0, y1], [x0, y1, x0, y0]] {
                lines.push((l, 15));
            }
        }
        lines
    }
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let mut rng = Rng::new(0xD0D6E);
        let identity = exp([0.0; 3]);
        let mut zero = g.head().unwrap_or(identity);
        let mut game = Game::new(g.now());
        let mut dirty = Dirty::default();
        let (mut shown, mut over_shown): (Option<String>, bool) = (None, false);
        let mut last = g.now();
        while g.running() {
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap && game.over {
                    zero = g.head().unwrap_or(identity);
                    game = Game::new(g.now());
                }
            }
            let now = g.now();
            let dt = (now - last).clamp(0.0, 0.2);
            last = now;
            let target = ship_target(angles(&zero, &g.head().unwrap_or(identity)));
            game.step(now, dt, target, &mut rng);
            let score = format!("TIME {:>3}   LIVES {}", game.score as u32, "+ ".repeat(game.lives as usize));
            let new_score = shown.as_deref() != Some(score.as_str());
            let (end, new_end) = (game.over, game.over != over_shown);
            g.frame(|eye, c| {
                let shift = eye.depth(1.5);
                if new_score {
                    c.clear(0, SCORE_Y, 640, c.size());
                    c.text(150 + shift, SCORE_Y, &score, 11);
                    c.show(0, SCORE_Y, 640, c.size());
                }
                let lines = game.lines(eye, now);
                if let Some(rect) = dirty.take(eye.side, around(&lines, 1)) {
                    let [x, y, w, h] = xywh(rect);
                    c.clear(x, y, w, h);
                    c.lines(&lines);
                    c.show(x, y, w, h);
                }
                if new_end {
                    c.clear(0, 200, 640, 48);
                    if end {
                        let text = format!("GAME OVER   {} s", game.score as u32);
                        c.text_centred(320 + shift, 200, &text, 15);
                        c.text_centred(320 + shift, 228, "tap to play again", 9);
                    }
                    c.show(0, 200, 640, 48);
                }
            })
            .await?;
            shown = Some(score);
            over_shown = end;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ship_follows_the_head() {
        let zero = exp([0.0; 3]);
        let left = exp([0.0, 10f64.to_radians(), 0.0]);
        let [l, u] = angles(&zero, &left);
        assert!((l - 10.0).abs() < 1e-9 && u.abs() < 1e-9);
        let [x, _] = ship_target([l, u]);
        assert!(x < 0.0);
        assert_eq!(ship_target([90.0, -90.0]), [-ROOM[0], -ROOM[1]]);
    }

    #[test]
    fn a_rock_through_the_ship_costs_a_life() {
        let mut rng = Rng::new(3);
        let mut game = Game::new(0.0);
        game.rocks.push(Rock {
            at: [0.0, 0.0, SHIP_Z + 0.1],
            size: 0.08,
            eight: true,
            axis: [0.0, 1.0, 0.0],
            spin: 1.0,
        });
        game.next_rock = 99.0;
        game.step(1.0, 0.05, [0.0, 0.0], &mut rng);
        assert_eq!(game.lives, LIVES - 1);
        assert!(game.rocks.is_empty());
        // One that passes wide does nothing.
        game.rocks.push(Rock {
            at: [0.3, 0.0, SHIP_Z + 0.1],
            size: 0.08,
            eight: true,
            axis: [0.0, 1.0, 0.0],
            spin: 1.0,
        });
        game.step(1.1, 0.05, [0.0, 0.0], &mut rng);
        assert_eq!(game.lives, LIVES - 1);
    }
}
