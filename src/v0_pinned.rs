//! `erisg1 v0 pinned`: the 3D cube of `v0 cube`, pinned in the room. The Mac follows your head with the arms'
//! gyroscopes and accelerometers and draws the cube, still, where it was when you started, as each eye would see it
//! from where it now is.
//!
//! - Turning: the gyroscopes' rates, less what they read at rest, integrated; gravity from the accelerometers pulls
//!   pitch and roll back, so only the heading drifts.
//! - Moving: the accelerometers less gravity, integrated twice. That runs away within a second, so speed and
//!   position leak back to zero: a quick move of the head gives a little parallax, then the cube settles back.
//! - About 0.1 s passes between a reading and the frame it shapes appearing, so each frame is drawn for where the
//!   head will be by then, at the rate it turns now.
//! - A tap on either TouchBar puts the cube back 1 m ahead of where you look.
//!
//! Each frame the Mac sends each arm its eye's twelve edges, clipped to the panel, as u16 coordinates with a level
//! each (one packet); a routine on the arm erases the last ones by drawing them again in black and draws these.

use std::time::{Duration, Instant};

use crate::link::{self, Error};
use crate::proto::Side;
use crate::v0_cube::{self, HALF_SIDE};
use crate::v5::{self, prop, trigger};
use crate::v5_asm::*;
use crate::v5_link::{self, Arm};

const POOL: u32 = 0x30000;
/// Written every frame: the edges (x0, y0, x1, y1 as u16), their levels (u8), then the rectangle (x, y, w, h) that
/// covers these edges and the last ones.
const STATE: u32 = 0x30400;
const LEVELS: u32 = STATE + 96;
const RECT: u32 = STATE + 108;
const STATE_LEN: usize = 124;
/// The edges drawn last, to erase them with.
const LAST: u32 = 0x30480;
const CODE: u32 = 0x30600;
const IDLE: u32 = 0x3F000;

/// The panel's middle row: the line of sight.
const CY: f64 = 240.0;
const DISTANCE: f64 = 1.0;
/// The cube's turn in the room: 35° about the vertical, then its top 25° toward you, so three faces show.
const TILT: [f64; 2] = [35.0, 25.0];
/// Edges nearer than this to the eyes are cut there.
const NEAR: f64 = 0.05;

const PERIOD: Duration = Duration::from_millis(33);
const QUEUE: usize = 4;
const CALIBRATE: Duration = Duration::from_millis(1500);
/// How far past the frame's moment to predict: the readings are about this old when they arrive.
const READING_AGE: f64 = 0.03;
/// Gravity's pull on pitch and roll, per reading, while the accelerometer reads about 1 g.
const LEVELLING: f64 = 0.02;
/// The share of the way the rate used to predict moves toward each new reading: a little smoothing for the
/// readings' noise, which predicting a tenth of a second ahead magnifies, without missing a turn's start.
const RATE_SMOOTHING: f64 = 0.7;
/// How fast speed and position leak back to zero (s), and the acceleration taken as none (m/s²).
const SPEED_LEAK: f64 = 0.4;
const POSITION_LEAK: f64 = 1.5;
const STILL: f64 = 0.2;
const G: f64 = 9.81;

pub(crate) type V = [f64; 3];
pub(crate) type M = [[f64; 3]; 3];

const IDENTITY: M = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub(crate) fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn scale(a: V, s: f64) -> V {
    a.map(|v| v * s)
}

pub(crate) fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: V, b: V) -> V {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub(crate) fn norm(a: V) -> f64 {
    dot(a, a).sqrt()
}

pub(crate) fn apply(m: &M, v: V) -> V {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

pub(crate) fn mul(a: &M, b: &M) -> M {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

pub(crate) fn transpose(m: &M) -> M {
    std::array::from_fn(|i| std::array::from_fn(|j| m[j][i]))
}

/// The rotation by |w| radians about w (Rodrigues).
pub(crate) fn exp(w: V) -> M {
    let angle = norm(w);
    if angle < 1e-12 {
        return IDENTITY;
    }
    let [x, y, z] = scale(w, 1.0 / angle);
    let k: M = [[0.0, -z, y], [z, 0.0, -x], [-y, x, 0.0]];
    let k2 = mul(&k, &k);
    let (s, c) = angle.sin_cos();
    std::array::from_fn(|i| std::array::from_fn(|j| IDENTITY[i][j] + s * k[i][j] + (1.0 - c) * k2[i][j]))
}

/// The rotation vector of `m`: the axis, as long as the angle in radians (the inverse of `exp`, for angles below π).
pub(crate) fn log(m: &M) -> V {
    let angle = ((m[0][0] + m[1][1] + m[2][2] - 1.0) / 2.0).clamp(-1.0, 1.0).acos();
    let axis = [m[2][1] - m[1][2], m[0][2] - m[2][0], m[1][0] - m[0][1]];
    if angle < 1e-9 {
        return scale(axis, 0.5);
    }
    scale(axis, angle / (2.0 * angle.sin()).max(1e-9))
}

/// The head in the room. The head frame is the arms': x right, y up, z back toward the wearer. The room's frame is
/// the head's at the start, turned so y is straight up.
pub(crate) struct Tracker {
    /// From the head frame to the room's.
    pub(crate) turn: M,
    /// Where the head is and how fast it moves, in the room, m and m/s.
    position: V,
    speed: V,
    /// The rate of turn, rad/s, head frame, smoothed over the last few readings.
    rate: V,
    /// What the accelerometers read at rest, mg.
    gravity: f64,
    translate: bool,
}

impl Tracker {
    /// A tracker for a head at rest whose accelerometers read `at_rest` (mg, head frame).
    pub(crate) fn new(at_rest: V, translate: bool) -> Tracker {
        let gravity = norm(at_rest);
        let up = scale(at_rest, 1.0 / gravity);
        // The smallest turn that takes the head's up to the room's.
        let axis = cross(up, [0.0, 1.0, 0.0]);
        let angle = dot(up, [0.0, 1.0, 0.0]).clamp(-1.0, 1.0).acos();
        let turn = if norm(axis) < 1e-9 { IDENTITY } else { exp(scale(axis, angle / norm(axis))) };
        Tracker { turn, position: [0.0; 3], speed: [0.0; 3], rate: [0.0; 3], gravity, translate }
    }

    /// Moves on `dt` seconds with the head turning at `rate` (rad/s) and the accelerometers reading `acc` (mg).
    pub(crate) fn update(&mut self, rate: V, acc: V, dt: f64) {
        self.rate = add(self.rate, scale(sub(rate, self.rate), RATE_SMOOTHING));
        self.turn = mul(&self.turn, &exp(scale(rate, dt)));
        // Gravity levels pitch and roll while nothing else accelerates the head.
        let size = norm(acc);
        if (size - self.gravity).abs() < 100.0 {
            let up = apply(&self.turn, scale(acc, 1.0 / size));
            let fix = cross(up, [0.0, 1.0, 0.0]);
            self.turn = mul(&exp(scale(fix, LEVELLING)), &self.turn);
        }
        if self.translate {
            let mut a = sub(scale(apply(&self.turn, acc), G / 1000.0), [0.0, self.gravity * G / 1000.0, 0.0]);
            if norm(a) < STILL {
                a = [0.0; 3];
            }
            self.speed = add(scale(self.speed, (-dt / SPEED_LEAK).exp()), scale(a, dt));
            self.position = add(scale(self.position, (-dt / POSITION_LEAK).exp()), scale(self.speed, dt));
        }
    }

    /// The head's turn and position `ahead` seconds from now, at the present rate and speed.
    pub(crate) fn predicted(&self, ahead: f64) -> (M, V) {
        (mul(&self.turn, &exp(scale(self.rate, ahead))), add(self.position, scale(self.speed, ahead)))
    }
}

/// The cube, still in the room: its middle and its turn.
struct Cube {
    middle: V,
    turn: M,
}

impl Cube {
    /// DISTANCE ahead of a head at `position` turned by `turn`, facing it.
    fn ahead(turn: &M, position: V) -> Cube {
        let ahead = apply(turn, [0.0, 0.0, -1.0]);
        let heading = (-ahead[0]).atan2(-ahead[2]);
        let tilt = mul(&exp([0.0, TILT[0].to_radians(), 0.0]), &exp([TILT[1].to_radians(), 0.0, 0.0]));
        Cube { middle: add(position, scale(ahead, DISTANCE)), turn: mul(&exp([0.0, heading, 0.0]), &tilt) }
    }

    fn corners(&self) -> [V; 8] {
        std::array::from_fn(|i| {
            let s = [1, 2, 4].map(|b| if i & b == 0 { -HALF_SIDE } else { HALF_SIDE });
            add(self.middle, apply(&self.turn, s))
        })
    }
}

/// The edges as the eye `eye` across a head at `position`, turned by `turn`, sees them: each clipped to the panel
/// (`None` off it) with its level, nearer brighter.
fn view(cube: &Cube, turn: &M, position: V, eye: f64) -> Vec<Option<([i32; 4], u8)>> {
    let back = transpose(turn);
    let eye_at = add(position, apply(turn, [eye, 0.0, 0.0]));
    // In the eye's own terms: x right, y up, z ahead.
    let seen = |p: V| {
        let [x, y, z] = apply(&back, sub(p, eye_at));
        [x, y, -z]
    };
    let corners = cube.corners().map(seen);
    let middle = seen(cube.middle)[2];
    v0_cube::edges()
        .into_iter()
        .map(|(i, j)| {
            let (mut a, mut b) = (corners[i as usize], corners[j as usize]);
            if a[2] < NEAR && b[2] < NEAR {
                return None;
            }
            // Cut at NEAR where the edge passes the eyes.
            let cut = |p: V, q: V| add(p, scale(sub(q, p), (NEAR - p[2]) / (q[2] - p[2])));
            if a[2] < NEAR {
                a = cut(a, b);
            } else if b[2] < NEAR {
                b = cut(b, a);
            }
            let line = v0_cube::clip(v0_cube::project(a, 0.0, CY), v0_cube::project(b, 0.0, CY))?;
            let far = (((a[2] + b[2]) / 2.0 - middle) / (HALF_SIDE * 2f64.sqrt()) + 1.0) / 2.0;
            Some((line, (15.0 - 10.0 * far.clamp(0.0, 1.0)).round() as u8))
        })
        .collect()
}

/// The state for one arm: its edges and levels, and the rectangle around them and `before`. Returns it with the
/// rectangle around these edges alone, for the next frame.
fn state(edges: &[Option<([i32; 4], u8)>], before: Option<[i32; 4]>) -> (Vec<u8>, Option<[i32; 4]>) {
    let mut state = vec![0u8; STATE_LEN];
    let mut around: Option<[i32; 4]> = None;
    for (n, edge) in edges.iter().enumerate() {
        let Some(([x0, y0, x1, y1], level)) = *edge else { continue };
        for (k, v) in [x0, y0, x1, y1].into_iter().enumerate() {
            state[n * 8 + k * 2..n * 8 + k * 2 + 2].copy_from_slice(&(v as u16).to_le_bytes());
        }
        state[96 + n] = level;
        let [ax, ay, bx, by] = around.unwrap_or([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]);
        around = Some([ax.min(x0).min(x1), ay.min(y0).min(y1), bx.max(x0).max(x1), by.max(y0).max(y1)]);
    }
    let now = around.map(|[x0, y0, x1, y1]| [x0, y0, x1 + 1, y1 + 1]);
    let rect = match (now, before) {
        (Some([a, b, c, d]), Some([e, f, g, h])) => Some([a.min(e), b.min(f), c.max(g), d.max(h)]),
        (one, other) => one.or(other),
    };
    let [x, y, w, h] = rect.map_or([0; 4], |[x0, y0, x1, y1]| [x0, y0, x1 - x0, y1 - y0]);
    for (k, v) in [x, y, w, h].into_iter().enumerate() {
        state[108 + k * 4..112 + k * 4].copy_from_slice(&(v as u32).to_le_bytes());
    }
    (state, now)
}

/// The routines each arm keeps: at CODE, erase the last edges and draw the new ones; `show`, the rectangle to the
/// panel. Returns the code, its pool and `show`'s address.
fn render_code() -> Result<(Vec<u8>, Vec<u8>, u32), String> {
    let mut a = Asm::new(CODE, POOL);
    let (frame, stride, zero) = (a.k(v5::FRAME), a.k(v5::STRIDE), a.k(0));
    let (state, last, levels) = (a.k(STATE), a.k(LAST), a.k(LEVELS));
    let lines = a.k(96);
    let [r0, r1, r2, r3, r4] = [0, 1, 2, 3, 4];
    a.label("render");
    for (from, drawing) in [(last, false), (state, true)] {
        for n in 0..12 {
            for (k, reg) in [r0, r1, r2, r3].into_iter().enumerate() {
                let i = a.k(n * 4 + k as u32);
                a.op(LD16, &[reg, from, i]);
            }
            let level = if drawing {
                let i = a.k(n);
                a.op(LD8, &[r4, levels, i]);
                r4
            } else {
                zero
            };
            a.op(LINE, &[frame, stride, r0, r1, r2, r3, level]);
        }
    }
    a.op(COPY, &[last, state, lines]);
    a.op(RET, &[]);
    a.label("show");
    let rect = |i: u32| ((RECT / 4) + i) as u16;
    a.op(DRAW, &[frame, stride, rect(0), rect(1), rect(0), rect(1), rect(2), rect(3)]);
    a.op(RET, &[]);
    let show = a.address("show");
    let (code, pool) = a.finish()?;
    assert!(POOL + pool.len() as u32 <= STATE && CODE + code.len() as u32 <= IDLE);
    Ok((code, pool, show))
}

/// Host code that shows `text` in the middle of a cleared panel, in the font at `scale`.
fn message(text: &str, scale: u8) -> Vec<Vec<u8>> {
    let glyphs = v5::text_indices(text);
    let x = 320 - 4 * scale as i32 * glyphs.len() as i32;
    vec![
        v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0),
        v5::write(v5::TEXT, &glyphs),
        v5::glyphs(x, 240, 15, glyphs.len()),
        v5::draw(0, 0, v5::WIDTH, v5::HEIGHT),
    ]
}

const READINGS: [u8; 6] = [prop::GYR_X, prop::GYR_Y, prop::GYR_Z, prop::ACC_X, prop::ACC_Y, prop::ACC_Z];

/// One arm's gyroscope (rad/s) and accelerometer (mg), from a PROPS reply to READINGS.
fn reading(values: &[u32]) -> Option<(V, V)> {
    if values.len() < 6 || values[..6].contains(&v5::UNKNOWN) {
        return None;
    }
    let v: Vec<f64> = values[..6].iter().map(|&v| v as i32 as f64).collect();
    Some(([v[0], v[1], v[2]].map(|mdps| (mdps / 1000.0).to_radians()), [v[3], v[4], v[5]]))
}

/// Readies `arm` for a head-following program: the font at `scale`, `writes` (its routines), the motion sensors at
/// full rate, "hold still" on the panel, the do-nothing program for the light sensor, TouchBar taps reported, the
/// fast link and the clocks lined up. Returns how far ahead frames must be sent.
pub(crate) async fn start(arm: &mut Arm, scale: u8, mut writes: Vec<Vec<u8>>) -> Result<Duration, Error> {
    arm.control(v5::ctrl::VM_STOP, &[]).await?;
    crate::v0::upload_font(arm, scale).await?;
    writes.push(v5::set(prop::IMU_RATE, 104));
    writes.push(v5::set(prop::GYRO_RATE, 104));
    writes.extend(message("hold still", scale));
    arm.run(writes).await?;
    arm.flush().await?;
    crate::v0_sensors::start_idle_program(arm, IDLE).await?;
    arm.control(v5::ctrl::EVENTS, &(1u16 << trigger::TOUCH).to_le_bytes()).await?;
    for _ in 0..2 {
        match arm.ask_for_fast_link().await {
            Ok(interval) if interval <= Duration::from_millis(15) => break,
            Ok(_) => {}
            Err(e) => link::trace(format_args!("{e}")),
        }
    }
    let round_trip = arm.sync_clock().await?;
    Ok(2 * arm.interval() + round_trip / 2 + Duration::from_millis(15))
}

/// Stops the program on each arm, puts the sensors back as they were at boot and switches the panel off.
pub(crate) async fn finish(mut arms: Vec<Arm>) -> Result<(), Error> {
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        arm.run([v5::set(prop::IMU_RATE, 13), v5::set(prop::GYRO_RATE, 0), v5::set(prop::PANEL, 0)]).await?;
        arm.flush().await?;
    }
    for arm in arms {
        arm.disconnect().await;
    }
    Ok(())
}

/// The head, from the arms' readings: each frame takes in what arrived and asks each arm for the next.
pub(crate) struct Head {
    pub(crate) tracker: Tracker,
    /// What each arm's gyroscope reads at rest.
    bias: Vec<V>,
    reads: Vec<Option<(u8, Vec<u8>, Instant)>>,
    updated: Instant,
    asked: u64,
    /// The right arm's light, 0.01 lux, as last read.
    pub(crate) light: u32,
    /// Readings taken in, for the rate shown.
    pub(crate) readings: u64,
}

impl Head {
    /// Holds still for CALIBRATE: what each gyroscope reads at rest, and which way gravity pulls.
    pub(crate) async fn calibrate(arms: &mut [Arm], translate: bool) -> Result<Head, Error> {
        println!("hold still…");
        let mut rest: Vec<(V, usize)> = vec![([0.0; 3], 0); arms.len()];
        let mut gravity = ([0.0; 3], 0);
        let started = Instant::now();
        while started.elapsed() < CALIBRATE {
            for (arm, rest) in arms.iter_mut().zip(&mut rest) {
                if let Some((rate, acc)) = reading(&arm.props(&READINGS).await?) {
                    *rest = (add(rest.0, rate), rest.1 + 1);
                    gravity = (add(gravity.0, acc), gravity.1 + 1);
                }
            }
        }
        if gravity.1 == 0 {
            return Err("no readings from the gyroscopes".into());
        }
        for arm in arms.iter_mut() {
            arm.run([v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0), v5::draw(0, 0, v5::WIDTH, v5::HEIGHT)]).await?;
        }
        Ok(Head {
            tracker: Tracker::new(scale(gravity.0, 1.0 / gravity.1 as f64), translate),
            bias: rest.iter().map(|(sum, n)| scale(*sum, 1.0 / (*n).max(1) as f64)).collect(),
            reads: arms.iter().map(|_| None).collect(),
            updated: Instant::now(),
            asked: 0,
            light: v5::UNKNOWN,
            readings: 0,
        })
    }

    /// Takes in the readings that arrived, moves the tracker on, and asks for the next (the right arm's light now
    /// and then too). Returns the TouchBar gestures since the last call: the arm and the gesture (VM.md §6.7).
    pub(crate) async fn poll(&mut self, arms: &mut [Arm]) -> Result<Vec<(Side, u32)>, Error> {
        self.asked += 1;
        let (mut fresh, mut gestures): (Vec<(V, V)>, Vec<(Side, u32)>) = (Vec::new(), Vec::new());
        for (i, (arm, read)) in arms.iter_mut().zip(&mut self.reads).enumerate() {
            arm.poll().await?;
            if let Some((tag, ids, _)) = read
                && let Some(reply) = arm.reply(*tag)
            {
                let values: Vec<u32> = reply?.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
                if let Some((rate, acc)) = reading(&values) {
                    fresh.push((sub(rate, self.bias[i]), acc));
                }
                if let Some(&value) = ids.iter().position(|&id| id == prop::LIGHT).and_then(|at| values.get(at)) {
                    self.light = value;
                }
                *read = None;
            }
            while let Some(event) = arm.events.pop_front() {
                if let v5::EventKind::Input { trigger: trigger::TOUCH, value } = event.kind {
                    gestures.push((arm.side, value));
                }
            }
            if read.as_ref().is_none_or(|r| r.2.elapsed() > Duration::from_millis(300)) {
                let mut ids = READINGS.to_vec();
                if arm.side == Side::Right && self.asked.is_multiple_of(20) {
                    ids.push(prop::LIGHT);
                }
                let tag = arm.request(v5::ctrl::PROPS, &ids).await?;
                *read = Some((tag, ids, Instant::now()));
            }
        }
        // Both arms' readings averaged, taken as holding since the last ones.
        if !fresh.is_empty() {
            let n = fresh.len() as f64;
            let rate = scale(fresh.iter().fold([0.0; 3], |s, f| add(s, f.0)), 1.0 / n);
            let acc = scale(fresh.iter().fold([0.0; 3], |s, f| add(s, f.1)), 1.0 / n);
            let dt = self.updated.elapsed().as_secs_f64().min(0.2);
            self.updated = Instant::now();
            self.tracker.update(rate, acc, dt);
            self.readings += 1;
        }
        Ok(gestures)
    }

    /// The head's turn and position when a frame due at `due` (the Mac's clock) appears, as the latest readings
    /// say.
    pub(crate) fn at(&self, due: u32) -> (M, V) {
        let ahead = (due.wrapping_sub(v5_link::host_us()) as i32).max(0) as f64 / 1e6
            + READING_AGE
            + self.updated.elapsed().as_secs_f64();
        self.tracker.predicted(ahead)
    }
}

/// One arm's frame: the host code that draws it, and the host code that puts it on the panel at its moment.
pub(crate) type Frame = (Vec<Vec<u8>>, Vec<Vec<u8>>);

/// Frames on a steady beat, PERIOD apart, each sent far enough ahead to reach both arms, and no more than QUEUE
/// waiting on an arm.
pub(crate) struct Beat {
    lead: Duration,
    next_due: Option<u32>,
    queued: Vec<std::collections::VecDeque<u16>>,
    brightness: Option<u32>,
}

impl Beat {
    pub(crate) fn new(lead: Duration, arms: &[Arm]) -> Beat {
        Beat { lead, next_due: None, queued: vec![Default::default(); arms.len()], brightness: None }
    }

    /// Waits until the arms have room and it's time to send the next frame. Returns the moment it's to appear, in
    /// the Mac's clock.
    pub(crate) async fn next(&mut self, arms: &mut [Arm]) -> Result<u32, Error> {
        self.next_with(arms, Duration::ZERO).await
    }

    /// As `next`, for a frame that takes `extra` longer to reach the arms (several packets).
    pub(crate) async fn next_with(&mut self, arms: &mut [Arm], extra: Duration) -> Result<u32, Error> {
        let waits = arms.iter_mut().zip(&mut self.queued).map(|(arm, queued)| async move {
            while queued.len() >= QUEUE {
                arm.wait_for(queued.pop_front().unwrap()).await?;
            }
            Ok::<(), Error>(())
        });
        futures::future::try_join_all(waits).await?;
        let interval = arms.iter().map(|a| a.interval()).max().unwrap_or_default();
        let lead = (self.lead + interval + extra).as_micros() as u32;
        let earliest = v5_link::host_us().wrapping_add(lead);
        let due = match self.next_due {
            Some(due) if (due.wrapping_sub(earliest) as i32) > 0 => due,
            _ => earliest,
        };
        let send_at = due.wrapping_sub(lead).wrapping_sub(v5_link::host_us()) as i32;
        if send_at > 0 {
            tokio::time::sleep(Duration::from_micros(send_at as u64)).await;
        }
        self.next_due = Some(due.wrapping_add(PERIOD.as_micros() as u32));
        Ok(due)
    }

    /// Sends each arm its frame: `render` (drawing it), a brightness change for `light` if one is due, then at
    /// `due` `show` (putting it on the panel).
    pub(crate) async fn send(
        &mut self,
        arms: &mut [Arm],
        due: u32,
        light: u32,
        boost: u8,
        frames: Vec<Frame>,
    ) -> Result<(), Error> {
        let mut extra = None;
        if light != v5::UNKNOWN {
            let want = crate::v0_sensors::brightness_for(light, boost);
            if self.brightness.is_none_or(|b| b.abs_diff(want) >= 2) {
                extra = Some(v5::set(prop::BRIGHTNESS, want));
                self.brightness = Some(want);
            }
        }
        let sends = arms.iter_mut().zip(&mut self.queued).zip(frames).map(|((arm, queued), (render, show))| {
            let mut code = render;
            code.extend(extra.clone());
            code.push(v5::sleep_until(arm.arm_time(due)));
            code.extend(show);
            async move {
                arm.run(code).await?;
                queued.push_back(arm.sent());
                Ok::<(), Error>(())
            }
        });
        futures::future::try_join_all(sends).await?;
        Ok(())
    }
}

pub async fn pinned(side: Option<Side>, boost: u8, translate: bool) -> Result<(), Error> {
    let (code, pool, show) = render_code()?;
    let mut arms = crate::v0::arms(side).await?;
    let mut lead = Duration::ZERO;
    for arm in &mut arms {
        let max = arm.max_code;
        let mut writes = v5::upload(POOL, &pool, max);
        writes.extend(v5::upload(CODE, &code, max));
        writes.push(v5::write(LAST, &[0; 96]));
        lead = lead.max(start(arm, 3, writes).await?);
    }
    let mut head = Head::calibrate(&mut arms, translate).await?;
    let mut cube = Cube::ahead(&head.tracker.turn, head.tracker.position);
    println!("pinned 1 m ahead; tap a TouchBar to bring it back in front. Ctrl-C to stop");

    let mut beat = Beat::new(lead, &arms);
    let mut rects: Vec<Option<[i32; 4]>> = vec![None; arms.len()];
    let mut frames = 0u64;
    let mut reported = Instant::now();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        frames += 1;
        let frame = async {
            if head.poll(&mut arms).await?.iter().any(|&(_, gesture)| gesture == 1) {
                head.tracker.position = [0.0; 3];
                head.tracker.speed = [0.0; 3];
                cube = Cube::ahead(&head.tracker.turn, head.tracker.position);
            }
            let due = beat.next(&mut arms).await?;
            // Each eye's view from where the head will be then.
            let (turn, position) = head.at(due);
            let frames = arms
                .iter()
                .zip(&mut rects)
                .map(|(arm, rect)| {
                    let edges = view(&cube, &turn, position, v0_cube::eye(Some(arm.side)));
                    let (state, now) = state(&edges, *rect);
                    *rect = now;
                    (vec![v5::write(STATE, &state), v5::call(CODE)], vec![v5::call(show)])
                })
                .collect();
            beat.send(&mut arms, due, head.light, boost, frames).await
        };
        tokio::select! {
            _ = &mut stop => break,
            done = frame => done?,
        }
        if reported.elapsed() > Duration::from_secs(5) {
            let secs = reported.elapsed().as_secs_f64();
            let ahead = apply(&head.tracker.turn, [0.0, 0.0, -1.0]);
            println!(
                "{:.1} frames/s, {:.1} readings/s, heading {:.0}°, pitch {:.0}°, head {:.0} cm from the start",
                frames as f64 / secs,
                head.readings as f64 / secs,
                (-ahead[0]).atan2(-ahead[2]).to_degrees(),
                ahead[1].asin().to_degrees(),
                norm(head.tracker.position) * 100.0
            );
            (frames, head.readings) = (0, 0);
            reported = Instant::now();
        }
    }
    finish(arms).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: V, b: V) -> bool {
        norm(sub(a, b)) < 1e-6
    }

    #[test]
    fn builds_and_fits() {
        let (code, _, show) = render_code().unwrap();
        assert!(show > CODE && CODE + code.len() as u32 <= IDLE);
        assert!(LAST >= STATE + STATE_LEN as u32 && LAST + 96 <= CODE && RECT % 4 == 0);
        // The state, the two calls, a brightness change and the sleep fit one packet.
        assert!(5 + STATE_LEN + 4 + 15 + 11 + 4 <= 242);
    }

    #[test]
    fn turns_about_the_axis() {
        // A quarter turn about up takes right to ahead (−z).
        let r = exp([0.0, std::f64::consts::FRAC_PI_2, 0.0]);
        assert!(close(apply(&r, [1.0, 0.0, 0.0]), [0.0, 0.0, -1.0]));
        assert!(close(apply(&mul(&r, &transpose(&r)), [0.3, 0.2, 0.1]), [0.3, 0.2, 0.1]));
    }

    #[test]
    fn log_undoes_exp() {
        for w in [[0.1, -0.2, 0.3], [0.0, 1.5, 0.0], [1e-12, 0.0, 0.0], [-2.0, 0.5, 1.0]] {
            assert!(close(log(&exp(w)), w), "{w:?}");
        }
    }

    #[test]
    fn levels_a_tilted_head() {
        // Looking 30° down: gravity reads partly along z (VM.md §2.5: acc_z below 0 looking up).
        let (s, c) = 30f64.to_radians().sin_cos();
        let t = Tracker::new([0.0, 1000.0 * c, 1000.0 * s], false);
        assert!(close(apply(&t.turn, [0.0, c, s]), [0.0, 1.0, 0.0]));
        // Its line of sight in the room points down.
        assert!(apply(&t.turn, [0.0, 0.0, -1.0])[1] < -0.49);
    }

    #[test]
    fn the_cube_stays_put_as_the_head_turns() {
        let t = Tracker::new([0.0, 1000.0, 0.0], false);
        let cube = Cube::ahead(&t.turn, t.position);
        // Straight ahead: in the middle, nearer corners further apart in the two eyes.
        let edges = |turn: &M, eye: f64| view(&cube, turn, [0.0; 3], eye);
        let middle = |turn: &M| {
            let lines: Vec<[i32; 4]> = edges(turn, 0.0).into_iter().flatten().map(|e| e.0).collect();
            let xs: Vec<i32> = lines.iter().flat_map(|l| [l[0], l[2]]).collect();
            (xs.iter().min().unwrap() + xs.iter().max().unwrap()) / 2
        };
        assert!((middle(&t.turn) - 320).abs() < 10);
        // The head turned 5° left: the cube moves right by about tan 5° × FOCAL.
        let left = mul(&t.turn, &exp([0.0, 5f64.to_radians(), 0.0]));
        let moved = middle(&left) - middle(&t.turn);
        assert!((moved as f64 - 5f64.to_radians().tan() * v0_cube::FOCAL).abs() < 8.0, "{moved}");
        // Turned right round, it's behind: nothing to draw.
        let back = mul(&t.turn, &exp([0.0, std::f64::consts::PI, 0.0]));
        assert!(edges(&back, 0.0).iter().all(Option::is_none));
    }

    #[test]
    fn integrates_a_steady_turn() {
        let mut t = Tracker::new([0.0, 1000.0, 0.0], false);
        for _ in 0..100 {
            t.update([0.0, 0.5, 0.0], [0.0, 1000.0, 0.0], 0.01);
        }
        let ahead = apply(&t.turn, [0.0, 0.0, -1.0]);
        assert!(((-ahead[0]).atan2(-ahead[2]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn state_holds_the_edges_and_the_rectangle() {
        let edges = vec![Some(([10, 20, 30, 40], 15)), None];
        let (s, now) = state(&edges, Some([0, 0, 5, 5]));
        assert_eq!(&s[..8], &[10, 0, 20, 0, 30, 0, 40, 0]);
        assert_eq!(s[96], 15);
        assert_eq!(now, Some([10, 20, 31, 41]));
        let rect: Vec<u32> = s[108..124].as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
        assert_eq!(rect, [0, 0, 31, 41]);
    }
}
