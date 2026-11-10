//! `erisg1 v0 follow`: a cross that follows your gaze lazily. When you turn your head it stays where it was in the
//! room, then glides back to the middle of your view (a first-order lag of `lag` seconds), never more than `radius`
//! pixels away. Its place on the panel goes through a 1€ filter: smoothed hard while it barely moves, so the head's
//! small wobbles don't shake it, and hardly at all once it moves, so a turn shows at once. Text under it moves with
//! it; text above it is fixed to your view, to compare. All of it is drawn in 3D, DISTANCE ahead.
//!
//! The head is followed as in `v0 pinned`, by its turn only: the cross always settles back to the middle, so the
//! heading's drift never shows. Each frame the Mac sends each arm its eye's view: the rectangle to clear and copy,
//! the cross's four arms (clipped to the panel) and where the text under it goes.

use std::time::{Duration, Instant};

use crate::link::Error;
use crate::proto::Side;
use crate::v0_cube::{self, FOCAL};
use crate::v0_pinned::{self, Beat, Frame, Head, M, V, add, apply, exp, log, mul, norm, scale, sub, transpose};
use crate::v5;
use crate::v5_asm::*;

const POOL: u32 = 0x30000;
/// Written every frame: the state words, then the glyphs of the text under the cross.
const STATE: u32 = 0x30400;
/// The rectangle (x, y, w, h) cleared, drawn into and copied to the panel: this frame's things and the last's.
const S_RECT: u32 = 0;
/// The cross's arms (x0, y0, x1, y1 each) and their levels, 0 for one off the panel.
const S_LINES: u32 = 4;
const S_LEVELS: u32 = 20;
/// The text under the cross: x, baseline and how many glyphs (0 when off the panel).
const S_TEXT: u32 = 24;
/// 1 when the rectangle takes in part of the text above, which is then drawn again.
const S_FIXED: u32 = 27;
const TEXT_AT: u32 = STATE + 28 * 4;
const TEXT_MAX: usize = 16;
const STATE_LEN: usize = 28 * 4 + TEXT_MAX;
/// The text above, set once: its glyphs, then its x, baseline and length.
const FIXED_TEXT: u32 = 0x30480;
const FIXED_AT: u32 = 0x304A0;
const CODE: u32 = 0x30600;

const SCALE: u8 = 2;
const ADVANCE: i32 = 8 * SCALE as i32;
const ASCENT: i32 = 7 * SCALE as i32;
const CY: f64 = 240.0;
const DISTANCE: f64 = 1.5;
/// The cross's arms, from GAP to ARM pixels out from its middle; the text under it, BELOW pixels down.
const ARM: f64 = 30.0;
const GAP: f64 = 6.0;
const BELOW: f64 = 40.0;
/// The text above, ABOVE pixels over the middle of the view.
const ABOVE: i32 = 80;
const UNDER_TEXT: &str = "follows";
const FIXED: &str = "locked to view";
/// The 1€ filter on the cross's place, in pixels: the cutoff at rest (Hz), how much it rises per pixel a second of
/// movement, and the cutoff for the speed itself, high so a movement's start soon counts.
const MIN_CUTOFF: f64 = 1.0;
const BETA: f64 = 0.05;
const SPEED_CUTOFF: f64 = 4.0;
const NEAR: f64 = 0.05;

/// The routines each arm keeps: at CODE, clear the rectangle, draw the text above again if it's in it, then the
/// cross and the text under it; `show`, the rectangle to the panel.
fn render_code() -> Result<(Vec<u8>, Vec<u8>, u32), String> {
    let mut a = Asm::new(CODE, POOL);
    let (frame, stride, font, mask, zero, bright) =
        (a.k(v5::FRAME), a.k(v5::STRIDE), a.k(v5::FONT), a.k(2), a.k(0), a.k(15));
    let (text, fixed_text) = (a.k(TEXT_AT), a.k(FIXED_TEXT));
    let s = |i: u32| ((STATE / 4) + i) as u16;
    let f = |i: u32| ((FIXED_AT / 4) + i) as u16;
    let rect = [s(S_RECT), s(S_RECT + 1), s(S_RECT + 2), s(S_RECT + 3)];
    a.label("render");
    a.op(FILL, &[frame, stride, rect[0], rect[1], rect[2], rect[3], zero]);
    a.branch(BEQ, s(S_FIXED), zero, "cross");
    a.op(GLYPHS, &[font, frame, stride, f(0), f(1), mask, bright, fixed_text, f(2)]);
    a.label("cross");
    for n in 0..4 {
        let l = |k: u32| s(S_LINES + 4 * n + k);
        a.op(LINE, &[frame, stride, l(0), l(1), l(2), l(3), s(S_LEVELS + n)]);
    }
    a.op(GLYPHS, &[font, frame, stride, s(S_TEXT), s(S_TEXT + 1), mask, bright, text, s(S_TEXT + 2)]);
    a.op(RET, &[]);
    a.label("show");
    a.op(DRAW, &[frame, stride, rect[0], rect[1], rect[0], rect[1], rect[2], rect[3]]);
    a.op(RET, &[]);
    let show = a.address("show");
    let (code, pool) = a.finish()?;
    assert!(POOL + pool.len() as u32 <= STATE && CODE + code.len() as u32 <= v5::ARENA);
    Ok((code, pool, show))
}

/// Where the text above goes for the eye at `eye`: x, baseline, and the box (x0, y0, x1, y1) it covers. Straight
/// ahead at DISTANCE: the left eye's copy right of the middle, the right eye's left of it.
fn fixed_place(eye: f64) -> (i32, i32, [i32; 4]) {
    let shift = (-FOCAL * eye / DISTANCE).round() as i32;
    let n = FIXED.len() as i32;
    let x = 320 - n * ADVANCE / 2 + shift;
    let top = 240 - ABOVE;
    (x, top + ASCENT, [x, top, x + n * ADVANCE, top + ADVANCE])
}

/// One eye's view of the cross, turned by `to_head` from straight ahead, for the eye at `eye`: the four arms
/// (clipped, `None` off the panel), and the text under it (x, top), if it's all on the panel.
fn view(to_head: &M, eye: f64) -> (Vec<Option<[i32; 4]>>, Option<[i32; 2]>) {
    let at = |x: f64, y: f64| {
        let [x, y, z] = sub(apply(to_head, scale([x, y, -FOCAL], DISTANCE / FOCAL)), [eye, 0.0, 0.0]);
        [x, y, -z]
    };
    let place = |p: V| (p[2] > NEAR).then(|| v0_cube::project(p, 0.0, CY));
    let arms = [[-ARM, 0.0, -GAP, 0.0], [GAP, 0.0, ARM, 0.0], [0.0, -ARM, 0.0, -GAP], [0.0, GAP, 0.0, ARM]]
        .map(|[x0, y0, x1, y1]| v0_cube::clip(place(at(x0, y0))?, place(at(x1, y1))?));
    let text = place(at(0.0, -BELOW)).and_then(|[x, y]| {
        let (x, top) = (x.round() as i32 - UNDER_TEXT.len() as i32 * ADVANCE / 2, y.round() as i32);
        let width = UNDER_TEXT.len() as i32 * ADVANCE;
        // Wholly inside the panel's rows (below it would draw into the arena past the frame), at least partly in its
        // columns.
        (top >= 0 && top + ADVANCE <= v5::HEIGHT as i32 && x + width > 0 && x < v5::WIDTH as i32).then_some([x, top])
    });
    (arms.to_vec(), text)
}

/// The state for one arm, with the rectangle around this view and `before`; and the box around this view alone,
/// for the next frame.
fn state(
    arms: &[Option<[i32; 4]>],
    text: Option<[i32; 2]>,
    before: Option<[i32; 4]>,
    fixed: [i32; 4],
) -> (Vec<u8>, Option<[i32; 4]>) {
    let mut words = [0u32; 28];
    let mut boxes: Vec<[i32; 4]> = Vec::new();
    for (n, arm) in arms.iter().enumerate() {
        if let Some([x0, y0, x1, y1]) = *arm {
            words[(S_LINES as usize) + 4 * n..][..4].copy_from_slice(&[x0, y0, x1, y1].map(|v| v as u32));
            words[S_LEVELS as usize + n] = 15;
            boxes.push([x0.min(x1), y0.min(y1), x0.max(x1) + 1, y0.max(y1) + 1]);
        }
    }
    if let Some([x, top]) = text {
        words[S_TEXT as usize..][..3].copy_from_slice(&[x as u32, (top + ASCENT) as u32, UNDER_TEXT.len() as u32]);
        let w = UNDER_TEXT.len() as i32 * ADVANCE;
        boxes.push([x.max(0), top, (x + w).min(v5::WIDTH as i32), top + ADVANCE]);
    }
    let join = |a: [i32; 4], b: [i32; 4]| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
    let now = boxes.into_iter().reduce(join);
    let rect = match (now, before) {
        (Some(a), Some(b)) => Some(join(a, b)),
        (one, other) => one.or(other),
    };
    let (w, h) = (v5::WIDTH as i32, v5::HEIGHT as i32);
    let [x0, y0, x1, y1] = rect.map_or([0; 4], |[x0, y0, x1, y1]| {
        [(x0 - 2).clamp(0, w), (y0 - 2).clamp(0, h), (x1 + 2).clamp(0, w), (y1 + 2).clamp(0, h)]
    });
    let (x1, y1) = (x1.max(x0), y1.max(y0));
    words[..4].copy_from_slice(&[x0, y0, x1 - x0, y1 - y0].map(|v| v as u32));
    let overlaps = x0 < fixed[2] && fixed[0] < x1 && y0 < fixed[3] && fixed[1] < y1;
    words[S_FIXED as usize] = overlaps as u32;
    let mut state: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut glyphs = v5::text_indices(UNDER_TEXT);
    glyphs.resize(TEXT_MAX, 0);
    state.extend(glyphs);
    debug_assert_eq!(state.len(), STATE_LEN);
    (state, now)
}

/// Moves `lagged` toward `turn` by the share of the way a lag of `lag` seconds covers in `dt`, and no further from
/// it than `max` radians.
fn follow(lagged: &M, turn: &M, lag: f64, max: f64, dt: f64) -> M {
    let gap = log(&mul(&transpose(lagged), turn));
    let step = 1.0 - (-dt / lag).exp();
    let left = scale(gap, 1.0 - step);
    let left = if norm(left) > max { scale(left, max / norm(left)) } else { left };
    // What's left of the gap, measured back from the head.
    mul(turn, &exp(scale(left, -1.0)))
}

/// The 1€ filter (Casiez, Roussel and Vogel, 2012) on a vector: a low-pass whose cutoff rises with how fast the
/// vector moves.
struct OneEuro {
    value: Option<V>,
    speed: V,
}

impl OneEuro {
    fn filter(&mut self, x: V, dt: f64) -> V {
        let Some(value) = self.value else {
            self.value = Some(x);
            return x;
        };
        let share = |cutoff: f64| 1.0 / (1.0 + 1.0 / (std::f64::consts::TAU * cutoff * dt));
        let speed = scale(sub(x, value), 1.0 / dt);
        self.speed = add(self.speed, scale(sub(speed, self.speed), share(SPEED_CUTOFF)));
        let value = add(value, scale(sub(x, value), share(MIN_CUTOFF + BETA * norm(self.speed))));
        self.value = Some(value);
        value
    }
}

pub async fn follow_hud(side: Option<Side>, boost: u8, lag: f64, radius: f64) -> Result<(), Error> {
    let (code, pool, show) = render_code()?;
    let mut arms = crate::v0::arms(side).await?;
    let mut lead = Duration::ZERO;
    for arm in &mut arms {
        let max = arm.max_code;
        let mut writes = v5::upload(POOL, &pool, max);
        writes.extend(v5::upload(CODE, &code, max));
        lead = lead.max(v0_pinned::start(arm, SCALE, writes).await?);
    }
    let mut head = Head::calibrate(&mut arms, false).await?;
    // The text above, once.
    let mut fixed = Vec::new();
    for arm in &mut arms {
        let (x, baseline, [x0, y0, x1, y1]) = fixed_place(v0_cube::eye(Some(arm.side)));
        let glyphs = v5::text_indices(FIXED);
        let place: Vec<u8> =
            [x, baseline, glyphs.len() as i32].iter().flat_map(|v| (*v as u32).to_le_bytes()).collect();
        arm.run([
            v5::write(FIXED_TEXT, &glyphs),
            v5::write(FIXED_AT, &place),
            v5::write(v5::TEXT, &glyphs),
            v5::glyphs(x, baseline, 15, glyphs.len()),
            v5::draw(x0.max(0), y0, (x1 - x0.max(0)) as u32, (y1 - y0) as u32),
        ])
        .await?;
        fixed.push([x0, y0, x1, y1]);
    }
    println!("the cross follows your view with a {lag} s lag. Ctrl-C to stop");

    let mut lagged = head.tracker.turn;
    // The turn of the view away from the cross as shown, in pixels, smoothed.
    let mut shown = OneEuro { value: None, speed: [0.0; 3] };
    let mut beat = Beat::new(lead, &arms);
    let mut boxes: Vec<Option<[i32; 4]>> = vec![None; arms.len()];
    let mut stepped = Instant::now();
    let mut frames = 0u64;
    let mut reported = Instant::now();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        frames += 1;
        let frame = async {
            head.poll(&mut arms).await?;
            let dt = stepped.elapsed().as_secs_f64().min(0.2);
            stepped = Instant::now();
            lagged = follow(&lagged, &head.tracker.turn, lag, radius / FOCAL, dt);
            let due = beat.next(&mut arms).await?;
            let (turn, _) = head.at(due);
            let gap = scale(log(&mul(&transpose(&lagged), &turn)), FOCAL);
            let to_head = exp(scale(shown.filter(gap, dt.max(1e-3)), -1.0 / FOCAL));
            let frames: Vec<Frame> = arms
                .iter()
                .zip(&mut boxes)
                .zip(&fixed)
                .map(|((arm, before), fixed)| {
                    let (lines, text) = view(&to_head, v0_cube::eye(Some(arm.side)));
                    let (state, now) = state(&lines, text, *before, *fixed);
                    *before = now;
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
            let behind = norm(log(&mul(&transpose(&lagged), &head.tracker.turn))).to_degrees();
            println!(
                "{:.1} frames/s, {:.1} readings/s, the cross {behind:.1}° behind the view",
                frames as f64 / secs,
                head.readings as f64 / secs
            );
            (frames, head.readings) = (0, 0);
            reported = Instant::now();
        }
    }
    v0_pinned::finish(arms).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_fits() {
        let (code, pool, show) = render_code().unwrap();
        assert!(show > CODE && !code.is_empty() && !pool.is_empty());
        assert!(STATE + STATE_LEN as u32 <= FIXED_TEXT && FIXED_TEXT + 32 <= FIXED_AT && FIXED_AT + 12 <= CODE);
        // The state and the calls, a brightness change and the sleep fit one packet.
        assert!(5 + STATE_LEN + 4 + 15 + 11 + 4 <= 242);
    }

    #[test]
    fn sits_in_the_middle_when_caught_up() {
        let (arms, text) = view(&exp([0.0; 3]), 0.0);
        let [x0, y0, x1, y1] = arms[0].unwrap();
        assert_eq!((x0, x1, y0, y1), (290, 314, 240, 240));
        assert_eq!(text, Some([320 - 7 * 8, 280]));
        // Each eye's copy half the disparity at 1.5 m (80 / 1.5 ≈ 53 px, 27 each way rounded) to its side.
        let (left, _) = view(&exp([0.0; 3]), v0_cube::eye(Some(Side::Left)));
        let (right, _) = view(&exp([0.0; 3]), v0_cube::eye(Some(Side::Right)));
        assert_eq!(left[0].unwrap()[0] - right[0].unwrap()[0], 54);
        let (lx, _, _) = fixed_place(v0_cube::eye(Some(Side::Left)));
        let (rx, _, _) = fixed_place(v0_cube::eye(Some(Side::Right)));
        assert_eq!(lx - rx, 54);
    }

    #[test]
    fn smooths_wobbles_but_follows_a_move() {
        // A 3 px wobble back and forth at 30 frames a second comes out much smaller...
        let mut f = OneEuro { value: None, speed: [0.0; 3] };
        let mut widest: f64 = 0.0;
        for n in 0..60 {
            let out = f.filter([if n % 2 == 0 { 3.0 } else { -3.0 }, 0.0, 0.0], 1.0 / 30.0);
            if n > 30 {
                widest = widest.max(out[0].abs());
            }
        }
        assert!(widest < 1.5, "{widest}");
        // ...while a quick 30 px move is most of the way there within a few frames.
        let mut f = OneEuro { value: None, speed: [0.0; 3] };
        f.filter([0.0; 3], 1.0 / 30.0);
        let mut out = [0.0; 3];
        for _ in 0..3 {
            out = f.filter([30.0, 0.0, 0.0], 1.0 / 30.0);
        }
        assert!(out[0] > 20.0, "{out:?}");
    }

    #[test]
    fn stays_put_then_catches_up() {
        // The head turns 5° left at once: the cross, still where it was, shows right of the middle...
        let still = exp([0.0; 3]);
        let turned = exp([0.0, 5f64.to_radians(), 0.0]);
        let (arms, _) = view(&mul(&transpose(&turned), &still), 0.0);
        assert!(arms[0].unwrap()[0] > 290 + 100);
        // ...then a lag later is about 1/e of the way, and after five lags back in the middle.
        let mut lagged = still;
        for _ in 0..30 {
            lagged = follow(&lagged, &turned, 0.7, 1.0, 0.7 / 30.0);
        }
        let behind = norm(log(&mul(&transpose(&lagged), &turned)));
        assert!((behind - 5f64.to_radians() / std::f64::consts::E).abs() < 1e-3, "{behind}");
        for _ in 0..120 {
            lagged = follow(&lagged, &turned, 0.7, 1.0, 0.7 / 30.0);
        }
        assert!(norm(log(&mul(&transpose(&lagged), &turned))) < 1e-3);
    }

    #[test]
    fn never_trails_more_than_the_radius() {
        let lagged = follow(&exp([0.0; 3]), &exp([0.0, 1.0, 0.0]), 0.7, 30.0 / FOCAL, 0.03);
        let behind = norm(log(&mul(&transpose(&lagged), &exp([0.0, 1.0, 0.0]))));
        assert!((behind * FOCAL - 30.0).abs() < 1e-6);
    }

    #[test]
    fn never_copies_a_negative_rectangle() {
        // The view turned far off: whatever is left on or off the panel, the rectangle stays on it.
        for degrees in [-60.0f64, -12.0, -8.0, 8.0, 12.0, 60.0] {
            for axis in [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0]] {
                let turned = exp(scale(axis, degrees.to_radians()));
                let (arms, text) = view(&transpose(&turned), 0.0);
                let (s, _) = state(&arms, text, Some([600, 400, 700, 500]), [0, 0, 1, 1]);
                let r: Vec<i32> = s[..16].as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b)).collect();
                assert!(r[0] >= 0 && r[1] >= 0 && r[2] >= 0 && r[3] >= 0, "{degrees} {axis:?}: {r:?}");
                assert!(r[0] + r[2] <= 640 && r[1] + r[3] <= 480, "{r:?}");
            }
        }
    }

    #[test]
    fn redraws_the_text_above_when_it_overlaps() {
        let (_, _, fixed) = fixed_place(0.0);
        let (s, now) = state(&[Some([300, 165, 340, 165])], None, None, fixed);
        assert_eq!(u32::from_le_bytes(s[S_FIXED as usize * 4..][..4].try_into().unwrap()), 1);
        assert_eq!(now, Some([300, 165, 341, 166]));
        let (s, _) = state(&[Some([300, 300, 340, 300])], None, None, fixed);
        assert_eq!(u32::from_le_bytes(s[S_FIXED as usize * 4..][..4].try_into().unwrap()), 0);
    }
}
