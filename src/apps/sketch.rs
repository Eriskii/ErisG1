//! `sketch`: draw with your head. A cursor follows where you look (turning left and right, up and down, from where
//! you looked at the start) and leaves a line while the pen is down. Tap to lift or lower the pen, double tap for
//! the next brightness, hold to clear. When it ends (Ctrl-C), the drawing is saved as `sketch-DATE-TIME.png` in the
//! current directory.
//!
//! Each frame sends only the new piece of line and the cursor, moved: the old cursor drawn again in black, the lines
//! it covered drawn again, then the new one (`Canvas::lines`), and shows just that part of the panel. The drawing
//! sits about 1.5 m away.

use super::fun::{around, join, xywh};
use super::kit::{Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;
use crate::v0_pinned::{M, apply, exp, mul, transpose};
use crate::v5;

pub(crate) const APP: App = App {
    name: "sketch",
    about: "draw with your head: tap lifts or lowers the pen, double tap changes brightness, hold clears",
    fun: true,
    options: Options { font: 2, big: 0, imu: true, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(11.0)
        .head(|t| {
            // Where to look, as degrees right and up; the script wants left and up.
            let [right, up] = if t < 5.0 {
                // A five-pointed star, corner to corner.
                let s = (t - 0.5).clamp(0.0, 4.0) / 4.0 * 5.0;
                let (k, f) = (s.floor(), s.fract());
                let corner = |k: f64| {
                    let a = std::f64::consts::FRAC_PI_2 + k * 2.0 * std::f64::consts::TAU / 5.0;
                    [8.0 + 9.0 * a.cos(), 2.0 + 9.0 * a.sin()]
                };
                let (a, b) = (corner(k), corner(k + 1.0));
                [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
            } else if t < 6.2 {
                // Pen up: over to the left.
                let f = (t - 5.0) / 1.2;
                [8.0 - 21.0 * f, 11.0 - 11.0 * f]
            } else {
                // A spiral.
                let u = t - 6.2;
                let r = 1.0 + 1.6 * u;
                [-12.0 + r * (u * 4.0).cos(), r * (u * 4.0).sin()]
            };
            [-right, up, 0.0]
        })
        .touch(5.0, Side::Right, 1)
        .touch(6.2, Side::Right, 1)
        .touch(8.0, Side::Left, 2)
}

/// Pixels the cursor moves per degree of head turn.
const GAIN: f64 = 11.0;
/// Where the cursor may go (x0, y0, x1, y1).
const AREA: [i32; 4] = [16, 64, 624, 416];
/// How quickly the cursor follows the head (s): it takes the jitter out.
const SMOOTH: f64 = 0.05;
/// A new piece of line once the cursor has moved this far, pixels.
const STEP: f64 = 2.0;
const LEVELS: [u8; 3] = [15, 11, 7];
const DEPTH: f64 = 1.5;

/// The head's turn as left and up angles, degrees, from `zero`.
fn angles(zero: &M, head: &M) -> [f64; 2] {
    let ahead = apply(&mul(&transpose(zero), head), [0.0, 0.0, -1.0]);
    [(-ahead[0]).atan2(-ahead[2]).to_degrees(), ahead[1].clamp(-1.0, 1.0).asin().to_degrees()]
}

/// Where the cursor heads for: looking left moves it left, up moves it up.
fn target([left, up]: [f64; 2]) -> [f64; 2] {
    let [x0, y0, x1, y1] = AREA.map(|v| v as f64);
    [(320.0 - left * GAIN).clamp(x0, x1 - 1.0), (240.0 - up * GAIN).clamp(y0, y1 - 1.0)]
}

/// The cursor's lines at `at`: a cross with a gap, bright with the pen down, dim with it up.
fn cursor(at: [f64; 2], down: bool) -> Vec<([f64; 4], u8)> {
    let [x, y] = at.map(f64::round);
    let level = if down { 15 } else { 6 };
    [(-9.0, 0.0, -4.0, 0.0), (4.0, 0.0, 9.0, 0.0), (0.0, -9.0, 0.0, -4.0), (0.0, 4.0, 0.0, 9.0)]
        .iter()
        .map(|&(a, b, c, d)| ([x + a, y + b, x + c, y + d], level))
        .collect()
}

/// Lines moved by `shift` pixels sideways (one eye's copy).
fn shifted(lines: &[([f64; 4], u8)], shift: i32) -> Vec<([f64; 4], u8)> {
    let s = shift as f64;
    lines.iter().map(|&([a, b, c, d], l)| ([a + s, b, c + s, d], l)).collect()
}

/// Whether a line (x0, y0, x1, y1) passes through a box, roughly: their bounding boxes meet.
fn touches([a, b, c, d]: [f64; 4], [x0, y0, x1, y1]: [i32; 4]) -> bool {
    a.min(c) < x1 as f64 && a.max(c) >= x0 as f64 && b.min(d) < y1 as f64 && b.max(d) >= y0 as f64
}

/// Draws the lines into a 640 × 480 image, a level a pixel, as the panel would.
fn rasterise(lines: &[([f64; 4], u8)]) -> Vec<u8> {
    let (w, h) = (v5::WIDTH as i32, v5::HEIGHT as i32);
    let mut image = vec![0u8; (w * h) as usize];
    for &([x0, y0, x1, y1], level) in lines {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0) as i32;
        for i in 0..=steps {
            let f = i as f64 / steps as f64;
            let (x, y) = ((x0 + (x1 - x0) * f).round() as i32, (y0 + (y1 - y0) * f).round() as i32);
            if (0..w).contains(&x) && (0..h).contains(&y) {
                image[(y * w + x) as usize] = level;
            }
        }
    }
    image
}

/// Saves the drawing as a grey PNG in the current directory; returns its name.
fn save(lines: &[([f64; 4], u8)]) -> Result<String, Error> {
    let name = format!("sketch-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let file = std::fs::File::create(&name).map_err(|e| format!("{name}: {e}"))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), v5::WIDTH, v5::HEIGHT);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let grey: Vec<u8> = rasterise(lines).iter().map(|&l| l * 17).collect();
    writer.write_image_data(&grey).map_err(|e| e.to_string())?;
    Ok(name)
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let identity = exp([0.0; 3]);
        let zero = g.head().unwrap_or(identity);
        let mut drawing: Vec<([f64; 4], u8)> = Vec::new();
        let (mut down, mut level) = (true, 0usize);
        let mut at = target([0.0, 0.0]);
        let mut pen = at;
        let mut was = cursor(at, down);
        let mut fresh_start = true;
        let mut last = g.now();
        while g.running() {
            let mut clear = false;
            for (_, gesture) in g.input().await? {
                match gesture {
                    Gesture::Tap => down = !down,
                    Gesture::DoubleTap => level = (level + 1) % LEVELS.len(),
                    Gesture::Hold => clear = true,
                    _ => {}
                }
            }
            let now = g.now();
            let dt = (now - last).clamp(0.0, 0.2);
            last = now;
            let to = target(angles(&zero, &g.head().unwrap_or(identity)));
            let follow = 1.0 - (-dt / SMOOTH).exp();
            at = [at[0] + (to[0] - at[0]) * follow, at[1] + (to[1] - at[1]) * follow];
            // A new piece of line from where the pen was.
            let mut new = Vec::new();
            if (at[0] - pen[0]).hypot(at[1] - pen[1]) >= STEP {
                if down {
                    new.push(([pen[0], pen[1], at[0], at[1]], LEVELS[level]));
                }
                pen = at;
            }
            if clear {
                drawing.clear();
                new.clear();
            }
            let now_cursor = cursor(pen, down);
            // Lines the old cursor covered, drawn again after it's erased (the newest, if there are many).
            let was_box = around(&was, 1).unwrap_or([0; 4]);
            let under: Vec<([f64; 4], u8)> =
                drawing.iter().rev().filter(|l| touches(l.0, was_box)).take(24).copied().collect();
            let erase: Vec<([f64; 4], u8)> = was.iter().map(|&(l, _)| (l, 0)).collect();
            let list: Vec<([f64; 4], u8)> =
                erase.iter().chain(under.iter().rev()).chain(&new).chain(&now_cursor).copied().collect();
            let region = join(around(&was, 1), join(around(&new, 1), around(&now_cursor, 1)));
            let wipe = clear || fresh_start;
            g.frame(|eye, c| {
                let shift = eye.depth(DEPTH);
                if wipe {
                    c.clear_all();
                    c.lines(&shifted(&now_cursor, shift));
                    c.show_all();
                    return;
                }
                c.lines(&shifted(&list, shift));
                if let Some([x0, y0, x1, y1]) = region {
                    let [x, y, w, h] = xywh([x0 + shift.min(0) - 1, y0, x1 + shift.max(0) + 1, y1]);
                    c.show(x, y, w, h);
                }
            })
            .await?;
            drawing.extend(new);
            was = now_cursor;
            fresh_start = false;
        }
        if !g.preview() && !drawing.is_empty() {
            println!("saved {}", save(&drawing)?);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_left_and_up_moves_the_cursor_left_and_up() {
        let zero = exp([0.0; 3]);
        let left = exp([0.0, 5f64.to_radians(), 0.0]);
        let [x, y] = target(angles(&zero, &left));
        assert!((x - (320.0 - 5.0 * GAIN)).abs() < 1e-6 && (y - 240.0).abs() < 1e-6);
        let up = exp([5f64.to_radians(), 0.0, 0.0]);
        assert!(target(angles(&zero, &up))[1] < 240.0);
        assert_eq!(target([-90.0, -90.0]), [AREA[2] as f64 - 1.0, AREA[3] as f64 - 1.0]);
    }

    #[test]
    fn rasterises_like_the_panel() {
        let image = rasterise(&[([10.0, 10.0, 20.0, 10.0], 9)]);
        assert_eq!(image[10 * 640 + 15], 9);
        assert_eq!(image[11 * 640 + 15], 0);
        assert!(touches([10.0, 10.0, 20.0, 10.0], [15, 5, 30, 12]));
        assert!(!touches([10.0, 10.0, 20.0, 10.0], [25, 5, 30, 12]));
    }
}
