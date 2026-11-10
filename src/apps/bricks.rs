//! `bricks`: knock out a wall of bricks with a ball, steering the paddle by tilting your head (right ear down moves
//! it right). Tap to send the ball off; three balls, ten points a brick (more for the higher rows), and a new wall
//! when one's cleared. Tap to play again after the last ball.
//!
//! Only what moves is redrawn: each frame clears and draws the ball and the paddle where they were and are, and a
//! brick when it's hit, so a frame is a few small rectangles (under a packet). The whole game sits about 2 m away.
//! In the preview the paddle follows the ball by itself, as nothing can tilt a head to the ball there.

use super::kit::{Canvas, Eye, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::proto::Side;
use crate::v0_pinned::{M, apply};

pub(crate) const APP: App = App {
    name: "bricks",
    about: "knock out a wall of bricks: tilt your head to move the paddle, tap to send the ball",
    fun: true,
    options: Options { font: 2, big: 0, imu: true, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0).touch(1.0, Side::Right, 1).touch(6.5, Side::Left, 1).touch(9.5, Side::Right, 1)
}

/// The field (x0, y0, x1, y1): walls left, right and at the top, open at the bottom.
const FIELD: [i32; 4] = [120, 64, 520, 420];
const COLUMNS: i32 = 8;
const ROWS: i32 = 5;
/// The bricks: the first row's top, a brick's size and the gaps between.
const BRICKS_Y: i32 = 88;
const BRICK: [i32; 2] = [46, 12];
const GAP: i32 = 4;
const PADDLE: [i32; 2] = [64, 6];
const PADDLE_Y: i32 = 396;
const BALL: i32 = 6;
/// Ball speeds, pixels a second.
const SPEED: f64 = 240.0;
const TOP_SPEED: f64 = 380.0;
/// Degrees of head tilt that take the paddle from the middle to a wall.
const TILT: f64 = 18.0;
const BALLS: u32 = 3;
const DEPTH: f64 = 2.0;

fn brick_rect(col: i32, row: i32) -> [i32; 4] {
    let step = BRICK[0] + GAP;
    let left = FIELD[0] + (FIELD[2] - FIELD[0] - COLUMNS * step + GAP) / 2;
    [left + col * step, BRICKS_Y + row * (BRICK[1] + GAP), BRICK[0], BRICK[1]]
}

fn row_level(row: i32) -> u8 {
    (15 - 2 * row) as u8
}

/// Whether two rectangles (x, y, w, h) overlap.
fn overlap(a: [f64; 4], b: [i32; 4]) -> bool {
    let b = b.map(|v| v as f64);
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

/// The head's roll, degrees, right ear down positive.
fn roll(head: &M) -> f64 {
    let right = apply(head, [1.0, 0.0, 0.0]);
    (-right[1]).clamp(-1.0, 1.0).asin().to_degrees()
}

struct Game {
    bricks: Vec<bool>,
    /// The ball's top left and its velocity, pixels and pixels a second; whether it's in play.
    ball: [f64; 2],
    velocity: [f64; 2],
    flying: bool,
    paddle: f64,
    balls: u32,
    score: u32,
    /// Bricks knocked out since the last frame, for it to clear.
    hit: Vec<usize>,
    /// The whole field wants drawing again: a new wall or a new game.
    redraw: bool,
}

impl Game {
    fn new() -> Game {
        let mut game = Game {
            bricks: vec![true; (COLUMNS * ROWS) as usize],
            ball: [0.0; 2],
            velocity: [0.0; 2],
            flying: false,
            paddle: ((FIELD[0] + FIELD[2]) / 2) as f64,
            balls: BALLS,
            score: 0,
            hit: Vec::new(),
            redraw: true,
        };
        game.rest_ball();
        game
    }

    fn rest_ball(&mut self) {
        self.flying = false;
        self.ball = [self.paddle - BALL as f64 / 2.0, (PADDLE_Y - BALL - 1) as f64];
    }

    fn launch(&mut self) {
        if !self.flying && self.balls > 0 {
            self.flying = true;
            self.velocity = [SPEED * 0.45, -SPEED * 0.89];
        }
    }

    fn paddle_rect(&self) -> [i32; 4] {
        [(self.paddle - PADDLE[0] as f64 / 2.0).round() as i32, PADDLE_Y, PADDLE[0], PADDLE[1]]
    }

    fn ball_rect(&self) -> [i32; 4] {
        [self.ball[0].round() as i32, self.ball[1].round() as i32, BALL, BALL]
    }

    /// Moves the paddle toward `target` (its middle's x) and the ball on by `dt`, a few pixels at a time.
    fn step(&mut self, dt: f64, target: f64) {
        let half = PADDLE[0] as f64 / 2.0;
        let target = target.clamp(FIELD[0] as f64 + half + 1.0, FIELD[2] as f64 - half - 1.0);
        self.paddle += (target - self.paddle) * (1.0 - (-dt / 0.05).exp());
        if !self.flying {
            self.ball[0] = self.paddle - BALL as f64 / 2.0;
            return;
        }
        let distance = (self.velocity[0].hypot(self.velocity[1]) * dt).max(1e-9);
        let steps = (distance / 3.0).ceil() as usize;
        for _ in 0..steps {
            self.substep(dt / steps as f64);
            if !self.flying {
                break;
            }
        }
    }

    fn substep(&mut self, dt: f64) {
        let [x0, y0, x1, _] = FIELD.map(|v| v as f64);
        let b = BALL as f64;
        self.ball[0] += self.velocity[0] * dt;
        self.ball[1] += self.velocity[1] * dt;
        // Walls.
        if self.ball[0] < x0 + 1.0 {
            self.ball[0] = x0 + 1.0;
            self.velocity[0] = self.velocity[0].abs();
        }
        if self.ball[0] + b > x1 - 1.0 {
            self.ball[0] = x1 - 1.0 - b;
            self.velocity[0] = -self.velocity[0].abs();
        }
        if self.ball[1] < y0 + 1.0 {
            self.ball[1] = y0 + 1.0;
            self.velocity[1] = self.velocity[1].abs();
        }
        let rect = [self.ball[0], self.ball[1], b, b];
        // The paddle: where on it the ball lands sets the angle it leaves at.
        if self.velocity[1] > 0.0 && overlap(rect, self.paddle_rect()) {
            let speed = self.velocity[0].hypot(self.velocity[1]);
            let off = ((self.ball[0] + b / 2.0 - self.paddle) / (PADDLE[0] as f64 / 2.0)).clamp(-1.0, 1.0);
            let angle = off * 60f64.to_radians();
            self.velocity = [speed * angle.sin(), -speed * angle.cos()];
            self.ball[1] = (PADDLE_Y - BALL - 1) as f64;
        }
        // Bricks: knock out the first it meets and bounce off the side it came through.
        for i in 0..self.bricks.len() {
            if !self.bricks[i] {
                continue;
            }
            let r = brick_rect(i as i32 % COLUMNS, i as i32 / COLUMNS);
            if !overlap(rect, r) {
                continue;
            }
            self.bricks[i] = false;
            self.hit.push(i);
            self.score += 10 * (ROWS - i as i32 / COLUMNS) as u32;
            let r = r.map(|v| v as f64);
            let across = (self.ball[0] + b - r[0]).min(r[0] + r[2] - self.ball[0]);
            let down = (self.ball[1] + b - r[1]).min(r[1] + r[3] - self.ball[1]);
            if across < down {
                self.velocity[0] = -self.velocity[0];
            } else {
                self.velocity[1] = -self.velocity[1];
            }
            let speed = self.velocity[0].hypot(self.velocity[1]);
            let faster = (speed + 4.0).min(TOP_SPEED) / speed;
            self.velocity = self.velocity.map(|v| v * faster);
            break;
        }
        if self.bricks.iter().all(|b| !b) {
            self.bricks.iter_mut().for_each(|b| *b = true);
            self.redraw = true;
            self.rest_ball();
        }
        // Lost below the paddle.
        if self.ball[1] > FIELD[3] as f64 {
            self.balls = self.balls.saturating_sub(1);
            self.rest_ball();
        }
    }
}

/// The walls and every brick standing: a fill a brick. Only at the start of a wall or a game: about 40 fills, some
/// seven packets.
fn draw_field(c: &mut Canvas, game: &Game, shift: i32) {
    let [x0, y0, x1, y1] = FIELD;
    c.clear(x0 - 40, y0 - 4, x1 - x0 + 80, y1 - y0 + 8);
    let s = shift as f64;
    let walls: Vec<([f64; 4], u8)> = [[x0, y1, x0, y0], [x0, y0, x1 - 1, y0], [x1 - 1, y0, x1 - 1, y1]]
        .iter()
        .map(|l| ([l[0] as f64 + s, l[1] as f64, l[2] as f64 + s, l[3] as f64], 6))
        .collect();
    c.lines(&walls);
    for (i, _) in game.bricks.iter().enumerate().filter(|(_, standing)| **standing) {
        let (col, row) = (i as i32 % COLUMNS, i as i32 / COLUMNS);
        let [x, y, w, h] = brick_rect(col, row);
        c.fill(x + shift, y, w, h, row_level(row));
    }
    c.show(x0 - 40, y0 - 4, x1 - x0 + 80, y1 - y0 + 8);
}

/// Clears where a rectangle was and draws where it is, then shows both.
fn move_rect(c: &mut Canvas, before: [i32; 4], after: [i32; 4], shift: i32) {
    c.clear(before[0] + shift, before[1], before[2], before[3]);
    c.fill(after[0] + shift, after[1], after[2], after[3], 15);
    let x0 = before[0].min(after[0]);
    let y0 = before[1].min(after[1]);
    let x1 = (before[0] + before[2]).max(after[0] + after[2]);
    let y1 = (before[1] + before[3]).max(after[1] + after[3]);
    c.show(x0 + shift, y0, x1 - x0, y1 - y0);
}

fn score_line(game: &Game) -> String {
    let balls = "o ".repeat(game.balls as usize);
    if game.balls == 0 {
        format!("{:>5}   GAME OVER, tap to play again", game.score)
    } else {
        format!("{:>5}   {balls}", game.score)
    }
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let mut game = Game::new();
        let (mut ball_was, mut paddle_was) = (game.ball_rect(), game.paddle_rect());
        let mut shown: Option<String> = None;
        let mut last = g.now();
        while g.running() {
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap {
                    if game.balls == 0 {
                        game = Game::new();
                    } else {
                        game.launch();
                    }
                }
            }
            let now = g.now();
            let dt = (now - last).clamp(0.0, 0.1);
            last = now;
            let middle = ((FIELD[0] + FIELD[2]) / 2) as f64;
            let target = match (g.preview(), g.head()) {
                // The preview's paddle chases the ball, a little behind.
                (true, _) => game.ball[0] + 10.0 * (now * 1.7).sin(),
                (false, Some(head)) => middle + roll(&head) / TILT * (FIELD[2] - FIELD[0]) as f64 / 2.0,
                (false, None) => middle,
            };
            game.step(dt, target);
            let (ball, paddle) = (game.ball_rect(), game.paddle_rect());
            let score = score_line(&game);
            let new_score = shown.as_deref() != Some(score.as_str());
            let redraw = std::mem::take(&mut game.redraw);
            let hit = std::mem::take(&mut game.hit);
            g.frame(|eye: &Eye, c| {
                let shift = eye.depth(DEPTH);
                if redraw {
                    draw_field(c, &game, shift);
                }
                for &i in &hit {
                    let [x, y, w, h] = brick_rect(i as i32 % COLUMNS, i as i32 / COLUMNS);
                    c.clear(x + shift, y, w, h);
                    c.show(x + shift, y, w, h);
                }
                if redraw || ball != ball_was || !hit.is_empty() {
                    move_rect(c, ball_was, ball, shift);
                }
                if redraw || paddle != paddle_was {
                    move_rect(c, paddle_was, paddle, shift);
                }
                if new_score || redraw {
                    c.clear(0, 36, 640, c.size());
                    c.text(FIELD[0] + shift, 36, &score, 11);
                    c.show(0, 36, 640, c.size());
                }
            })
            .await?;
            (ball_was, paddle_was, shown) = (ball, paddle, Some(score));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_fits_the_field() {
        let [x, _, _, _] = brick_rect(0, 0);
        let [lx, ly, lw, lh] = brick_rect(COLUMNS - 1, ROWS - 1);
        assert!(x > FIELD[0] && lx + lw < FIELD[2] && ly + lh < PADDLE_Y - 100);
    }

    #[test]
    fn the_ball_bounces_off_the_paddle_and_knocks_out_a_brick() {
        let mut game = Game::new();
        game.launch();
        // Straight up from the paddle's middle: it reaches the bottom row, knocks one out and comes back down.
        game.velocity = [0.0, -SPEED];
        let mut knocked = false;
        for _ in 0..200 {
            let middle = game.ball[0] + 3.0;
            game.step(0.02, middle);
            knocked |= !game.hit.is_empty();
            if knocked && game.velocity[1] > 0.0 {
                break;
            }
        }
        assert!(knocked && game.velocity[1] > 0.0);
        assert_eq!(game.bricks.iter().filter(|b| !**b).count(), 1);
        assert_eq!(game.balls, BALLS);
    }

    #[test]
    fn a_missed_ball_costs_one() {
        let mut game = Game::new();
        game.launch();
        game.velocity = [0.0, SPEED];
        game.paddle = FIELD[0] as f64 + 40.0;
        game.ball = [FIELD[2] as f64 - 20.0, PADDLE_Y as f64 - 20.0];
        for _ in 0..50 {
            game.step(0.02, FIELD[0] as f64);
        }
        assert_eq!(game.balls, BALLS - 1);
        assert!(!game.flying);
    }

    #[test]
    fn rolling_right_reads_positive() {
        let right = crate::v0_pinned::exp([0.0, 0.0, -10f64.to_radians()]);
        assert!((roll(&right) - 10.0).abs() < 1e-9);
    }
}
