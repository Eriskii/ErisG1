//! `erisg1 v0 cube`: the time with seconds at the top left, the glasses' battery (both arms' averaged) at the top
//! right, and a wireframe cube spinning in the middle.
//!
//! As in `v0 flight`, the arms draw it: each keeps a routine, built with `v5_asm`, that erases the last cube by
//! drawing it again in black, draws this one from its eight corners and puts the text in. Each frame the Mac sends
//! only the corners, the edges' levels and the text (about 150 bytes, one packet), then has both arms show the
//! result at the same moment.

use std::time::{Duration, Instant};

use crate::link::{self, Error};
use crate::proto::Side;
use crate::v5::{self, prop};
use crate::v5_asm::*;
use crate::v5_link;

// Where things sit in the arena, above the frame and the font.
const POOL: u32 = 0x30000;
/// Written every frame: the state words, then the text slots from TEXTS.
const STATE: u32 = 0x30400;
// The state words, by index.
/// The cube's corners on the panel, x and y for each of the eight.
const S_CORNERS: u32 = 0;
/// Each edge's level: the nearer, the brighter.
const S_LEVELS: u32 = 16;
/// The rectangle (x, y, w, h) that covers this cube and the last one, which `show` copies to the panel.
const S_RECT: u32 = 28;
/// The battery bar's width and level.
const S_BAR: u32 = 32;
const S_BAR_LEVEL: u32 = 33;
const TEXTS: u32 = STATE + 34 * 4;
// The text slots: offset from TEXTS, and length.
const T_CLOCK: (u32, usize) = (0, 11);
const T_BATTERY: (u32, usize) = (12, 4);
const STATE_LEN: usize = (TEXTS - STATE) as usize + 16;
/// The corners the routine drew last, to erase them with. The host zeroes them.
const LAST: u32 = STATE + 0xC0;
/// Where the routines note the arm's clock as each starts and ends: `render`, `show`, then `text`.
const TIMES: u32 = LAST + 16 * 4;
const CODE: u32 = 0x30600;
/// A program that does nothing, which keeps the right arm's light sensor reading for the brightness.
const IDLE: u32 = 0x3F000;

pub(crate) const SCALE: u8 = 3;
pub(crate) const ADVANCE: u32 = 8 * SCALE as u32;
pub(crate) const BASELINE: u32 = 40;
/// The boxes (x, y, w, h) the clock and the battery's percentage are cleared and drawn in each frame, and the one the
/// battery is copied to the panel from, which takes in its outline.
pub(crate) const CLOCK: [u32; 4] = [16, 16, T_CLOCK.1 as u32 * ADVANCE, 30];
pub(crate) const BATTERY_TEXT: [u32; 4] = [466, 16, T_BATTERY.1 as u32 * ADVANCE, 30];
pub(crate) const BATTERY: [u32; 4] = [466, 16, 160, 30];
/// The battery's outline (x0, y0, x1, y1), drawn once, and its nub; the bar inside (x, y, full width, height).
const BODY: [i32; 4] = [572, 18, 620, 42];
const NUB: [i32; 4] = [621, 26, 3, 9];
pub(crate) const BAR: [u32; 4] = [574, 20, 45, 21];

/// The cube is drawn as each eye would see a real one, in metres: the eyes EYES apart, looking ahead (z) through the
/// panel's point CX, CY; the cube HALF_SIDE from its middle to a face, DISTANCE ahead. A corner turned toward you is
/// nearer, and its two copies further apart, than one turned away. FOCAL is pixels per unit of x / z: the pair
/// scouter_kit was calibrated on shows things at 0.8 m with the copies 100 px apart, so FOCAL × EYES = 80.
pub(crate) const CX: f64 = 320.0;
const CY: f64 = 264.0;
pub(crate) const FOCAL: f64 = 1270.0;
pub(crate) const EYES: f64 = 0.063;
pub(crate) const HALF_SIDE: f64 = 0.08;
const DISTANCE: f64 = 1.0;
/// How long the spin takes to come back to where it started, and the turns it makes about the vertical, the
/// horizontal and the line of sight meanwhile: about 0.9, 0.5 and 0.25 radians a second.
pub(crate) const LOOP_SECS: f64 = 48.0;
const TURNS: [f64; 3] = [7.0, 4.0, 2.0];

const PERIOD: Duration = Duration::from_millis(33);
const QUEUE: usize = 4;
const BATTERY_EVERY: Duration = Duration::from_secs(10);
const LIGHT_EVERY: Duration = Duration::from_secs(1);

/// The cube's twelve edges, as pairs of corners. Corner `i` is at x, y, z = ±1 by its bits 0, 1 and 2.
pub(crate) fn edges() -> Vec<(u32, u32)> {
    (0..8).flat_map(|i| [1, 2, 4].into_iter().filter(move |b| i & b == 0).map(move |b| (i, i | b))).collect()
}

/// The routines each arm keeps, from CODE on, and where the ones after the first start.
struct Routines {
    code: Vec<u8>,
    pool: Vec<u8>,
    /// Draws the clock and the battery, which the host has done only when they change: drawing the text takes the
    /// arm about 10 ms.
    text: u32,
    /// Copies the cube's rectangle to the panel, and `show_text` the clock and the battery.
    show: u32,
    show_text: u32,
}

/// At CODE, `render`, which erases the last cube and draws this one; then `text`, `show` and `show_text`.
fn render_code() -> Result<Routines, String> {
    const R0: u16 = 0;
    const R1: u16 = 1;
    let mut a = Asm::new(CODE, POOL);
    let (frame, stride, font, mask) = (a.k(v5::FRAME), a.k(v5::STRIDE), a.k(v5::FONT), a.k(2));
    let (zero, bright) = (a.k(0), a.k(15));
    let time = a.k(0); // TIME_US
    let times = |i: u32| ((TIMES / 4) + i) as u16;
    let state = |i: u32| ((STATE / 4) + i) as u16;
    let last = |i: u32| ((LAST / 4) + i) as u16;

    a.label("render");
    a.op(SYS_READ, &[times(0), time]);
    // The last cube in black, then this one.
    let edges = edges();
    for &(i, j) in &edges {
        a.op(LINE, &[frame, stride, last(2 * i), last(2 * i + 1), last(2 * j), last(2 * j + 1), zero]);
    }
    for (k, &(i, j)) in edges.iter().enumerate() {
        let corner = |n: u32| state(S_CORNERS + n);
        let level = state(S_LEVELS + k as u32);
        a.op(LINE, &[frame, stride, corner(2 * i), corner(2 * i + 1), corner(2 * j), corner(2 * j + 1), level]);
    }
    for i in 0..16 {
        a.op(MOV, &[last(i), state(S_CORNERS + i)]);
    }
    a.op(SYS_READ, &[times(1), time]);
    a.op(RET, &[]);

    // How long a routine took, in µs, tagged with its number in the top byte (1 the cube, 2 the text, 3 copying the
    // cube to the panel), into a register.
    let took = |a: &mut Asm, reg: u16, end: u32, start: u32, tag: u32| {
        let tag = a.k(tag << 24);
        a.op(SUB, &[reg, times(end), times(start)]);
        a.op(ADD, &[reg, reg, tag]);
    };

    // text: the clock, the battery's percentage and its bar.
    a.label("text");
    a.op(SYS_READ, &[times(4), time]);
    let baseline = a.k(BASELINE);
    for ([x, y, w, h], (at, n)) in [(CLOCK, T_CLOCK), (BATTERY_TEXT, T_BATTERY)] {
        let (x, y, w, h, text, n) = (a.k(x), a.k(y), a.k(w), a.k(h), a.k(TEXTS + at), a.k(n as u32));
        a.op(FILL, &[frame, stride, x, y, w, h, zero]);
        a.op(GLYPHS, &[font, frame, stride, x, baseline, mask, bright, text, n]);
    }
    let [x, y, w, h] = BAR.map(|v| a.k(v));
    a.op(FILL, &[frame, stride, x, y, w, h, zero]);
    a.op(FILL, &[frame, stride, x, y, state(S_BAR), h, state(S_BAR_LEVEL)]);
    a.op(SYS_READ, &[times(5), time]);
    took(&mut a, R0, 5, 4, 2);
    a.op(NOTIFY, &[R0, zero]);
    a.op(RET, &[]);

    // show: the cube's rectangle to the panel, then how long rendering and this took.
    a.label("show");
    a.op(SYS_READ, &[times(2), time]);
    let rect = |i: u32| state(S_RECT + i);
    a.op(DRAW, &[frame, stride, rect(0), rect(1), rect(0), rect(1), rect(2), rect(3)]);
    a.op(SYS_READ, &[times(3), time]);
    took(&mut a, R0, 1, 0, 1);
    took(&mut a, R1, 3, 2, 3);
    a.op(NOTIFY, &[R0, R1]);
    a.op(RET, &[]);

    // show_text: the clock and the battery to the panel.
    a.label("show_text");
    for [x, y, w, h] in [CLOCK, BATTERY] {
        let (x, y, w, h) = (a.k(x), a.k(y), a.k(w), a.k(h));
        a.op(DRAW, &[frame, stride, x, y, x, y, w, h]);
    }
    a.op(RET, &[]);

    assert_eq!(a.address("render"), CODE);
    let (text, show, show_text) = (a.address("text"), a.address("show"), a.address("show_text"));
    let (code, pool) = a.finish()?;
    assert!(POOL + pool.len() as u32 <= STATE && CODE + code.len() as u32 <= IDLE);
    Ok(Routines { code, pool, text, show, show_text })
}

/// Where an eye is across the head, in metres: the left eye's to the left. `None`: midway, one view for both.
pub(crate) fn eye(side: Option<Side>) -> f64 {
    match side {
        Some(Side::Left) => -EYES / 2.0,
        Some(Side::Right) => EYES / 2.0,
        None => 0.0,
    }
}

/// The cube `t` seconds into its spin, as seen from an eye at `eye` (from `eye()`): its corners on the panel, and
/// each edge's level, 15 for the nearest down to 5 for the farthest.
pub(crate) fn cube(t: f64, eye: f64) -> ([[i32; 2]; 8], Vec<u32>) {
    let [(sa, ca), (sb, cb), (sc, cc)] = TURNS.map(|n| (std::f64::consts::TAU * n * t / LOOP_SECS).sin_cos());
    let turned: Vec<[f64; 3]> = (0..8)
        .map(|i| {
            let [x, y, z] = [1, 2, 4].map(|b| if i & b == 0 { -1.0 } else { 1.0 });
            // About the vertical, then the horizontal, then the line of sight.
            let (x, z) = (x * ca + z * sa, -x * sa + z * ca);
            let (y, z) = (y * cb - z * sb, y * sb + z * cb);
            let (x, y) = (x * cc - y * sc, x * sc + y * cc);
            [x, y, z]
        })
        .collect();
    let corners = std::array::from_fn(|i| {
        let [x, y, z] = turned[i].map(|v| v * HALF_SIDE);
        project([x, y, DISTANCE + z], eye, CY).map(|v| v.round() as i32)
    });
    // An edge's middle is √2 from the cube's: −√2 nearest, √2 farthest.
    let levels = edges()
        .iter()
        .map(|&(i, j)| {
            let z = (turned[i as usize][2] + turned[j as usize][2]) / 2.0;
            let far = ((z + 2f64.sqrt()) / (2.0 * 2f64.sqrt())).clamp(0.0, 1.0);
            (15.0 - 10.0 * far).round() as u32
        })
        .collect();
    (corners, levels)
}

/// Where the point x, y, z (right, up, ahead, in metres) is on the panel for the eye at `eye`, looking through row
/// `cy`.
pub(crate) fn project([x, y, z]: [f64; 3], eye: f64, cy: f64) -> [f64; 2] {
    [CX + FOCAL * (x - eye) / z, cy - FOCAL * y / z]
}

/// The part of the line from `a` to `b` on the panel, if any.
pub(crate) fn clip(a: [f64; 2], b: [f64; 2]) -> Option<[i32; 4]> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (max_x, max_y) = (v5::WIDTH as f64 - 1.0, v5::HEIGHT as f64 - 1.0);
    // Liang–Barsky: the line is a + t·(b − a), kept where each edge allows.
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a[0]), (dx, max_x - a[0]), (-dy, a[1]), (dy, max_y - a[1])] {
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
    (t0 <= t1).then(|| [a[0] + t0 * dx, a[1] + t0 * dy, a[0] + t1 * dx, a[1] + t1 * dy].map(|v| v.round() as i32))
}

/// The rectangle (x, y, w, h) around `corners`, joined with `before` so copying it also clears the last cube.
pub(crate) fn cube_rect(corners: &[[i32; 2]; 8], before: Option<[i32; 4]>) -> [i32; 4] {
    let mut x0 = corners.iter().map(|c| c[0]).min().unwrap() - 1;
    let mut y0 = corners.iter().map(|c| c[1]).min().unwrap() - 1;
    let mut x1 = corners.iter().map(|c| c[0]).max().unwrap() + 2;
    let mut y1 = corners.iter().map(|c| c[1]).max().unwrap() + 2;
    if let Some([bx, by, bw, bh]) = before {
        (x0, y0, x1, y1) = (x0.min(bx), y0.min(by), x1.max(bx + bw), y1.max(by + bh));
    }
    let (x0, y0) = (x0.clamp(0, v5::WIDTH as i32), y0.clamp(0, v5::HEIGHT as i32));
    let (x1, y1) = (x1.clamp(0, v5::WIDTH as i32), y1.clamp(0, v5::HEIGHT as i32));
    [x0, y0, x1 - x0, y1 - y0]
}

/// The battery's percentage as text, and its bar's width and level: dimmer at 20% and under.
fn battery(percent: Option<u32>) -> (String, u32, u32) {
    match percent {
        Some(p) => (format!("{p:>3}%"), 1 + p.min(100) * (BAR[2] - 1) / 100, if p <= 20 { 8 } else { 15 }),
        None => ("  ?%".into(), 1, 5),
    }
}

/// Host code that clears the frame but for the battery's outline, and shows it.
pub(crate) fn blank_panel() -> Vec<Vec<u8>> {
    let mut code = vec![v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0), v5::fixed_args()];
    let [x0, y0, x1, y1] = BODY;
    for [ax, ay, bx, by] in [[x0, y0, x1, y0], [x1, y0, x1, y1], [x1, y1, x0, y1], [x0, y1, x0, y0]] {
        code.push(v5::line_k(ax, ay, bx, by, 15));
    }
    let [x, y, w, h] = NUB;
    code.push(v5::fill(x, y, w as u32, h as u32, 15));
    code.push(v5::draw(0, 0, v5::WIDTH, v5::HEIGHT));
    code
}

/// Text padded or cut to `n` characters, as glyph indices.
fn slot(text: &str, n: usize) -> Vec<u8> {
    let text: String = format!("{text:<n$}").chars().take(n).collect();
    v5::text_indices(&text)
}

pub async fn cube_hud(side: Option<Side>, boost: u8) -> Result<(), Error> {
    let routines = render_code()?;
    let mut arms = crate::v0::arms(side).await?;
    let mut lead = Duration::ZERO;
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        crate::v0::upload_font(arm, SCALE).await?;
        let max = arm.max_code;
        let mut writes = v5::upload(POOL, &routines.pool, max);
        writes.extend(v5::upload(CODE, &routines.code, max));
        writes.push(v5::write(LAST, &[0; 64]));
        writes.extend(blank_panel());
        arm.run(writes).await?;
        arm.flush().await?;
        crate::v0_sensors::start_idle_program(arm, IDLE).await?;
        for _ in 0..2 {
            match arm.ask_for_fast_link().await {
                Ok(interval) if interval <= Duration::from_millis(15) => break,
                Ok(_) => {}
                Err(e) => link::trace(format_args!("{e}")),
            }
        }
        let round_trip = arm.sync_clock().await?;
        lead = lead.max(2 * arm.interval() + round_trip / 2 + Duration::from_millis(15));
    }
    println!("Ctrl-C to stop");

    let mut percent = vec![v5::UNKNOWN; arms.len()];
    let mut battery_at: Vec<Option<Instant>> = vec![None; arms.len()];
    let mut light = v5::UNKNOWN;
    let mut light_at: Option<Instant> = None;
    let mut brightness: Option<u32> = None;
    let mut reads: Vec<Option<(u8, Vec<u8>, Instant)>> = arms.iter().map(|_| None).collect();
    let mut rect: Option<[i32; 4]> = None;
    // The text and battery bar as last drawn.
    let mut drawn: Option<Vec<u8>> = None;
    // How long the arms take to draw the cube, the text and to put the cube on the panel, from their reports.
    let mut parts = [0u64; 3];
    let mut reports = [0u64; 3];
    let mut queued: Vec<std::collections::VecDeque<u16>> = vec![Default::default(); arms.len()];
    let mut next_due: Option<u32> = None;
    let start = Instant::now();
    let mut frames = 0u64;
    let mut reported = Instant::now();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        frames += 1;
        let frame = async {
            // Replies in; the battery asked for now and then, and the right arm's light for brightness.
            for (i, (arm, read)) in arms.iter_mut().zip(&mut reads).enumerate() {
                arm.poll().await?;
                if let Some((tag, ids, _)) = read
                    && let Some(reply) = arm.reply(*tag)
                {
                    for (&id, value) in ids.iter().zip(reply?.as_chunks::<4>().0) {
                        let value = u32::from_le_bytes(*value);
                        match id {
                            prop::LIGHT => light = value,
                            _ => percent[i] = value,
                        }
                    }
                    *read = None;
                }
                while let Some(event) = arm.events.pop_front() {
                    if let v5::EventKind::Notify { a, b, .. } = event.kind {
                        for part in [a, b] {
                            let (tag, us) = ((part >> 24) as usize, (part & 0xFF_FFFF) as u64);
                            if (1..=3).contains(&tag) {
                                parts[tag - 1] += us;
                                reports[tag - 1] += 1;
                            }
                        }
                    }
                }
                if read.as_ref().is_none_or(|r| r.2.elapsed() > Duration::from_millis(500)) {
                    let mut ids = Vec::new();
                    if battery_at[i].is_none_or(|t| t.elapsed() > BATTERY_EVERY) {
                        ids.push(prop::BATT_PCT);
                        battery_at[i] = Some(Instant::now());
                    }
                    if arm.side == Side::Right && light_at.is_none_or(|t| t.elapsed() > LIGHT_EVERY) {
                        ids.push(prop::LIGHT);
                        light_at = Some(Instant::now());
                    }
                    if !ids.is_empty() {
                        let tag = arm.request(v5::ctrl::PROPS, &ids).await?;
                        *read = Some((tag, ids, Instant::now()));
                    }
                }
            }

            // The moment this frame is shown, on the beat.
            let waits = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| async move {
                while queued.len() >= QUEUE {
                    arm.wait_for(queued.pop_front().unwrap()).await?;
                }
                Ok::<(), Error>(())
            });
            futures::future::try_join_all(waits).await?;
            let interval = arms.iter().map(|a| a.interval()).max().unwrap_or_default();
            let lead = (lead + interval).as_micros() as u32;
            let earliest = v5_link::host_us().wrapping_add(lead);
            let due = match next_due {
                Some(due) if (due.wrapping_sub(earliest) as i32) > 0 => due,
                _ => earliest,
            };
            let send_at = due.wrapping_sub(lead).wrapping_sub(v5_link::host_us()) as i32;
            if send_at > 0 {
                tokio::time::sleep(Duration::from_micros(send_at as u64)).await;
            }
            next_due = Some(due.wrapping_add(PERIOD.as_micros() as u32));

            // The cube spun and the time told as of that moment.
            let ahead = Duration::from_micros((due.wrapping_sub(v5_link::host_us()) as i32).max(0) as u64);
            // One view for both eyes: the state is the same packet for each.
            let (corners, levels) = cube((start.elapsed() + ahead).as_secs_f64(), eye(None));
            let now_rect = cube_rect(&corners, None);
            let [rx, ry, rw, rh] = cube_rect(&corners, rect);
            rect = Some(now_rect);
            let known: Vec<u32> = percent.iter().copied().filter(|&p| p <= 100).collect();
            let average =
                (!known.is_empty()).then(|| (known.iter().sum::<u32>() as f64 / known.len() as f64).round() as u32);
            let (battery_text, bar, bar_level) = battery(average);
            let mut words: Vec<u32> = corners.iter().flat_map(|&[x, y]| [x as u32, y as u32]).collect();
            words.extend(&levels);
            words.extend([rx as u32, ry as u32, rw as u32, rh as u32, bar, bar_level]);
            let mut state: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            state.resize(STATE_LEN, 0);
            let clock = chrono::Local::now() + chrono::TimeDelta::from_std(ahead).unwrap_or_default();
            for ((at, n), text) in [(T_CLOCK, clock.format("%-I:%M:%S %p").to_string()), (T_BATTERY, battery_text)] {
                let at = (TEXTS - STATE + at) as usize;
                state[at..at + n].copy_from_slice(&slot(&text, n));
            }
            // The bar's two words and the text after them.
            let text = state[S_BAR as usize * 4..].to_vec();
            let new_text = drawn.as_ref() != Some(&text);
            drawn = Some(text);

            let mut code = vec![v5::write(STATE, &state), v5::call(CODE)];
            if new_text {
                code.push(v5::call(routines.text));
            }
            if light != v5::UNKNOWN {
                let want = crate::v0_sensors::brightness_for(light, boost);
                if brightness.is_none_or(|b| b.abs_diff(want) >= 2) {
                    code.push(v5::set(prop::BRIGHTNESS, want));
                    brightness = Some(want);
                }
            }
            let sends = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| {
                let mut code = code.clone();
                code.push(v5::sleep_until(arm.arm_time(due)));
                code.push(v5::call(routines.show));
                if new_text {
                    code.push(v5::call(routines.show_text));
                }
                async move {
                    arm.run(code).await?;
                    queued.push_back(arm.sent());
                    Ok::<(), Error>(())
                }
            });
            futures::future::try_join_all(sends).await?;
            Ok::<(), Error>(())
        };
        tokio::select! {
            _ = &mut stop => break,
            done = frame => done?,
        }
        if reported.elapsed() > Duration::from_secs(5) {
            let rate = frames as f64 / reported.elapsed().as_secs_f64();
            let arms_battery: Vec<String> = arms
                .iter()
                .zip(&percent)
                .map(|(arm, &p)| {
                    format!("{} {}", arm.side.letter(), if p <= 100 { format!("{p}%") } else { "?".into() })
                })
                .collect();
            println!("{rate:.1} frames/s, battery {}", arms_battery.join(", "));
            let [cube, text, show] = std::array::from_fn(|i| parts[i].checked_div(reports[i]).unwrap_or(0));
            link::trace(format_args!("arms, µs each time: cube {cube}, text {text}, cube to panel {show}"));
            (parts, reports) = ([0; 3], [0; 3]);
            frames = 0;
            reported = Instant::now();
        }
    }
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        arm.run([v5::set(prop::PANEL, 0)]).await?;
        arm.flush().await?;
    }
    for arm in arms {
        arm.disconnect().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_fits() {
        let r = render_code().unwrap();
        assert!(CODE < r.text && r.text < r.show && r.show < r.show_text);
        assert!(!r.code.is_empty() && !r.pool.is_empty());
        // The state and its text slots, plus the four calls, a brightness change and the sleep, fit one packet.
        const { assert!(5 + STATE_LEN + 4 * 4 + 15 + 11 <= 242) };
        assert!(T_BATTERY.0 + T_BATTERY.1 as u32 + TEXTS <= STATE + STATE_LEN as u32);
        assert!(LAST >= STATE + STATE_LEN as u32 && TIMES + 6 * 4 <= CODE);
        const { assert!(S_BAR_LEVEL * 4 < TEXTS - STATE) };
    }

    #[test]
    fn has_twelve_edges_three_at_each_corner() {
        let edges = edges();
        assert_eq!(edges.len(), 12);
        for corner in 0..8 {
            assert_eq!(edges.iter().filter(|&&(i, j)| i == corner || j == corner).count(), 3);
        }
        // Each joins corners one coordinate apart.
        assert!(edges.iter().all(|&(i, j)| (i ^ j).count_ones() == 1));
    }

    #[test]
    fn stays_between_the_text_and_the_bottom() {
        for side in [Some(Side::Left), None, Some(Side::Right)] {
            for step in 0..3000 {
                let (corners, levels) = cube(step as f64 * 0.05, eye(side));
                for [x, y] in corners {
                    assert!((0..v5::WIDTH as i32).contains(&x), "x {x}");
                    assert!(y > (CLOCK[1] + CLOCK[3]) as i32 + 4 && y < v5::HEIGHT as i32, "y {y}");
                }
                assert!(levels.iter().all(|l| (5..=15).contains(l)));
            }
        }
    }

    #[test]
    fn nearer_corners_are_further_apart_in_the_two_eyes() {
        // At rest, corners 0–3 make the face toward you, 0.92 m away, and 4–7 the one away from you, 1.08 m:
        // 80 / 0.92 and 80 / 1.08 px apart.
        let (left, _) = cube(0.0, eye(Some(Side::Left)));
        let (right, _) = cube(0.0, eye(Some(Side::Right)));
        let apart: Vec<i32> = left.iter().zip(&right).map(|(l, r)| l[0] - r[0]).collect();
        assert_eq!(apart, [87, 87, 87, 87, 74, 74, 74, 74]);
        assert!(left.iter().zip(&right).all(|(l, r)| l[1] == r[1]));
    }

    #[test]
    fn clips_lines_to_the_panel() {
        assert_eq!(clip([-100.0, 10.0], [100.0, 10.0]), Some([0, 10, 100, 10]));
        assert_eq!(clip([10.0, 400.0], [10.0, 600.0]), Some([10, 400, 10, 479]));
        assert_eq!(clip([-10.0, -10.0], [-5.0, 500.0]), None);
    }

    #[test]
    fn nearer_edges_are_brighter() {
        // At rest, the face toward the eye (z = −1) is nearest, the one away from it farthest, and the edges between
        // them halfway.
        let (_, levels) = cube(0.0, 0.0);
        for (&(i, j), &level) in edges().iter().zip(&levels) {
            let expected = match (i & 4, j & 4) {
                (0, 0) => 14,
                (4, 4) => 6,
                _ => 10,
            };
            assert_eq!(level, expected, "edge {i}–{j}");
        }
    }

    #[test]
    fn rect_covers_the_cube_and_the_last_one() {
        let (a, _) = cube(0.0, 0.0);
        let (b, _) = cube(1.0, 0.0);
        let first = cube_rect(&a, None);
        let both = cube_rect(&b, Some(first));
        for [x, y] in a.iter().chain(&b) {
            assert!(x >= &both[0] && x < &(both[0] + both[2]) && y >= &both[1] && y < &(both[1] + both[3]));
        }
    }

    #[test]
    fn shows_the_battery() {
        assert_eq!(battery(Some(84)), (" 84%".into(), 1 + 84 * 44 / 100, 15));
        assert_eq!(battery(Some(100)).1, BAR[2]);
        assert_eq!(battery(Some(15)).2, 8);
        assert_eq!(battery(None).0, "  ?%");
    }
}
