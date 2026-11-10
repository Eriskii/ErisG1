//! Pieces the "super fun" apps share: a small random number generator, lines in 3D as an eye sees them, and the
//! part of the panel each eye changed last frame, so the next clears and shows only that and what's new.

use super::kit::Eye;
use crate::proto::Side;
use crate::v0_pinned::{V, add, scale, sub};
use crate::v5;

/// xorshift64*: plenty for scattering stars and sparks, and the same every run for a given seed.
pub(crate) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    /// A number in [0, 1).
    pub fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number in [lo, hi).
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }

    /// A direction, evenly spread over the sphere.
    pub fn direction(&mut self) -> V {
        let z = self.range(-1.0, 1.0);
        let angle = self.range(0.0, std::f64::consts::TAU);
        let r = (1.0 - z * z).sqrt();
        [r * angle.cos(), r * angle.sin(), z]
    }
}

/// Points nearer the eye than this are cut off.
const NEAR: f64 = 0.3;

/// The line between two points as `eye` sees them (x right, y up, z ahead), on the panel: cut where it comes
/// nearer than NEAR, `None` if it's all behind that.
pub(crate) fn segment(eye: &Eye, a: V, b: V) -> Option<[f64; 4]> {
    let (mut a, mut b) = (a, b);
    if a[2] < NEAR && b[2] < NEAR {
        return None;
    }
    let cut = |p: V, q: V| add(p, scale(sub(q, p), (NEAR - p[2]) / (q[2] - p[2])));
    if a[2] < NEAR {
        a = cut(a, b);
    } else if b[2] < NEAR {
        b = cut(b, a);
    }
    let (p, q) = (eye.project(a)?, eye.project(b)?);
    Some([p[0], p[1], q[0], q[1]])
}

/// The part of a line inside the box x0, y0, x1, y1 (ends excluded), if any (Liang–Barsky).
pub(crate) fn clip_to([ax, ay, bx, by]: [f64; 4], [x0, y0, x1, y1]: [i32; 4]) -> Option<[f64; 4]> {
    let (dx, dy) = (bx - ax, by - ay);
    let (lo_x, lo_y, hi_x, hi_y) = (x0 as f64, y0 as f64, x1 as f64 - 1.0, y1 as f64 - 1.0);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, ax - lo_x), (dx, hi_x - ax), (-dy, ay - lo_y), (dy, hi_y - ay)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else if p < 0.0 {
            t0 = t0.max(q / p);
        } else {
            t1 = t1.min(q / p);
        }
    }
    (t0 <= t1).then_some([ax + t0 * dx, ay + t0 * dy, ax + t1 * dx, ay + t1 * dy])
}

/// The box (x0, y0, x1, y1, ends excluded) around the parts of `lines` on the panel, `pad` pixels wider.
pub(crate) fn around(lines: &[([f64; 4], u8)], pad: i32) -> Option<[i32; 4]> {
    let (w, h) = (v5::WIDTH as f64, v5::HEIGHT as f64);
    let mut b: Option<[f64; 4]> = None;
    for ([x0, y0, x1, y1], _) in lines {
        // Only what's on the panel counts; a line reaching off it is clipped there when drawn.
        let Some([x0, y0, x1, y1]) = crate::v0_cube::clip([*x0, *y0], [*x1, *y1]) else { continue };
        let [x0, y0, x1, y1] = [x0, y0, x1, y1].map(|v| v as f64);
        let next = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        b = Some(match b {
            Some(o) => [o[0].min(next[0]), o[1].min(next[1]), o[2].max(next[2]), o[3].max(next[3])],
            None => next,
        });
    }
    b.map(|[x0, y0, x1, y1]| {
        [
            (x0 as i32 - pad).clamp(0, w as i32),
            (y0 as i32 - pad).clamp(0, h as i32),
            (x1 as i32 + 1 + pad).clamp(0, w as i32),
            (y1 as i32 + 1 + pad).clamp(0, h as i32),
        ]
    })
}

/// Two boxes joined.
pub(crate) fn join(a: Option<[i32; 4]>, b: Option<[i32; 4]>) -> Option<[i32; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (one, other) => one.or(other),
    }
}

/// What each eye drew last frame.
#[derive(Default)]
pub(crate) struct Dirty {
    last: [Option<[i32; 4]>; 2],
}

impl Dirty {
    /// The box to clear and show for `side` this frame: last frame's joined with `now`. Keeps `now` for the next.
    pub fn take(&mut self, side: Side, now: Option<[i32; 4]>) -> Option<[i32; 4]> {
        let i = (side == Side::Right) as usize;
        let both = join(self.last[i], now);
        self.last[i] = now;
        both
    }
}

/// x0, y0, x1, y1 as x, y, w, h.
pub(crate) fn xywh([x0, y0, x1, y1]: [i32; 4]) -> [i32; 4] {
    [x0, y0, x1 - x0, y1 - y0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_evenly_and_repeats() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        let xs: Vec<f64> = (0..1000).map(|_| a.next()).collect();
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = xs.iter().sum::<f64>() / 1000.0;
        assert!((mean - 0.5).abs() < 0.05);
        assert_eq!(b.next(), xs[0]);
        let d = Rng::new(3).direction();
        assert!(((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn boxes_what_is_on_the_panel() {
        let lines = [([10.0, 20.0, 30.0, 40.0], 15), ([-100.0, 5.0, -50.0, 5.0], 15)];
        assert_eq!(around(&lines, 2), Some([8, 18, 33, 43]));
        let mut d = Dirty::default();
        assert_eq!(d.take(Side::Left, Some([0, 0, 5, 5])), Some([0, 0, 5, 5]));
        assert_eq!(d.take(Side::Left, Some([10, 10, 20, 20])), Some([0, 0, 20, 20]));
        assert_eq!(d.take(Side::Right, None), None);
    }

    #[test]
    fn clips_to_a_box() {
        assert_eq!(clip_to([0.0, 50.0, 200.0, 50.0], [10, 0, 101, 100]), Some([10.0, 50.0, 100.0, 50.0]));
        assert_eq!(clip_to([0.0, 150.0, 200.0, 150.0], [10, 0, 101, 100]), None);
    }

    #[test]
    fn cuts_lines_coming_too_near() {
        let eye = Eye { side: Side::Left, head: None };
        assert!(segment(&eye, [0.0, 0.0, 0.1], [0.0, 0.1, 0.2]).is_none());
        let s = segment(&eye, [0.0, 0.0, -1.0], [0.0, 0.0, 2.0]).unwrap();
        assert!(s.iter().all(|v| v.is_finite()));
    }
}
