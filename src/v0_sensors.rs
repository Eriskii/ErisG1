//! `erisg1 v0 sensors`: every sensor reading of both arms and a level line, drawn from the Mac into both eyes alike
//! over the whole panel. The left arm's readings fill a column on the left and the right arm's a column on the right,
//! in both eyes; the level line between them comes from both arms' accelerometers averaged. Frames reach both arms
//! ahead of time with the moment to show them, as `v0 hud`'s do, so the eyes change together.

use std::time::Duration;

use crate::link::{self, Error};
use crate::proto::Side;
use crate::v5::{self, prop};
use crate::v5_link;

const SCALE: u8 = 2;
const ADVANCE: i32 = 8 * SCALE as i32;
const ASCENT: i32 = 7 * SCALE as i32;
/// 16 rows of 30 pixels fill the 480-line panel.
const ROW: i32 = 30;
const TOP: i32 = 8;
const MARGIN: i32 = 4;
const COL_W: u32 = 200;
/// Where values start in a column, after the labels (five characters), and how wide they can be (seven).
const VALUE_X: i32 = 88;
const VALUE_W: u32 = COL_W - VALUE_X as u32;
/// The level line's band between the two columns, the panel's full height.
const BOX_X: i32 = 210;
const BOX_W: u32 = 220;
const CX: i32 = BOX_X + BOX_W as i32 / 2;
const CY: i32 = v5::HEIGHT as i32 / 2;
/// Pixels the line moves per radian of pitch (about ±30° fills the panel), and its half length.
const PITCH_PX: f64 = 460.0;
const HALF_LINE: f64 = 100.0;
/// The columns are redrawn every this many frames; the line every frame.
const TEXT_EVERY: u64 = 4;
const QUEUE: usize = 4;
/// The beat frames are shown on.
const PERIOD: Duration = Duration::from_millis(50);

/// The readings in a column: a label, the property, and the decimals its raw value carries.
const READINGS: [(&str, u8, u32); 12] = [
    ("ACC X", prop::ACC_X, 0),
    ("ACC Y", prop::ACC_Y, 0),
    ("ACC Z", prop::ACC_Z, 0),
    ("GYR X", prop::GYR_X, 0),
    ("GYR Y", prop::GYR_Y, 0),
    ("GYR Z", prop::GYR_Z, 0),
    ("PITCH", prop::PITCH, 0),
    ("ROLL", prop::ROLL, 0),
    ("LUX", prop::LIGHT, 2),
    ("mV", prop::BATT_MV, 0),
    ("mA", prop::BATT_MA, 0),
    ("TEMP", prop::BATT_TEMP, 1),
];
const MORE: [(&str, u8, u32); 2] = [("WORN", prop::WORN, 0), ("TOUCH", prop::TOUCH_GESTURE, 0)];

/// Where the do-nothing program sits: one `END`, then its trigger table.
const IDLE: u32 = 0x30000;

/// Starts a program at `at` that does nothing: a single `END`, run once. A running program keeps the right arm's
/// light sensor taking readings (VM.md §6.6) without the firmware's automatic brightness, and this one never runs
/// anything that could get in the way of the host's drawing. Turns automatic brightness off.
pub(crate) async fn start_idle_program(arm: &mut crate::v5_link::Arm, at: u32) -> Result<(), Error> {
    let table_at = at + 16;
    let mut table = [0u32; 16];
    table[0] = at;
    let table: Vec<u8> = table.iter().flat_map(|v| v.to_le_bytes()).collect();
    arm.run([
        v5::set(prop::AUTO_BRIGHTNESS, 0),
        v5::set(prop::PEER_LINK, 0),
        v5::write(at, &[v5::END]),
        v5::write(table_at, &table),
    ])
    .await?;
    arm.flush().await?;
    arm.control(v5::ctrl::VM_START, &table_at.to_le_bytes()).await?;
    Ok(())
}

/// Even's light-to-brightness table (VM.md §6.7), for `lux_hundredths` of ambient light, plus `boost` levels.
pub(crate) fn brightness_for(lux_hundredths: u32, boost: u8) -> u32 {
    let lux = lux_hundredths / 100;
    let level = match lux {
        0..50 => lux / 5,
        50..200 => 10 + (lux - 50) / 10,
        200..1000 => 25 + (lux - 200) / 50,
        1000..2000 => 41,
        _ => 42,
    };
    (level + boost as u32).min(42)
}

/// A raw reading as text: signed, with `decimals` moved behind a point, `?` for none.
fn format(value: u32, decimals: u32) -> String {
    if value == v5::UNKNOWN {
        return "?".into();
    }
    let value = value as i32;
    if decimals == 0 {
        return value.to_string();
    }
    let scale = 10i64.pow(decimals);
    let magnitude = (value as i64).abs();
    let sign = if value < 0 { "-" } else { "" };
    format!("{sign}{}.{:0width$}", magnitude / scale, magnitude % scale, width = decimals as usize)
}

fn text(code: &mut Vec<Vec<u8>>, x: i32, baseline: i32, level: u8, text: &str) {
    let indices = v5::text_indices(text);
    code.push(v5::write(v5::TEXT, &indices));
    code.push(v5::glyphs(x, baseline, level, indices.len()));
}

/// The labels of a column, after its title.
fn labels() -> impl Iterator<Item = &'static str> {
    READINGS.iter().chain(&MORE).map(|r| r.0).chain(["FPS"])
}

fn baseline(row: usize) -> i32 {
    TOP + (row as i32 + 1) * ROW + ASCENT
}

/// Where the column of the arm on `side` sits, and its title.
fn column_x(side: Side) -> (i32, &'static str) {
    match side {
        Side::Left => (MARGIN, "LEFT ARM"),
        Side::Right => (v5::WIDTH as i32 - MARGIN - COL_W as i32, "RIGHT ARM"),
    }
}

/// Instructions that draw a column's title and labels, which don't change, into the frame.
fn column_labels(code: &mut Vec<Vec<u8>>, side: Side) {
    let (x, title) = column_x(side);
    text(code, x, TOP + ASCENT, 15, title);
    for (i, label) in labels().enumerate() {
        text(code, x, baseline(i), 8, label);
    }
}

/// Instructions that draw a column's values into the frame, right-aligned, without showing them.
fn column_values(code: &mut Vec<Vec<u8>>, side: Side, values: &[String]) {
    let x = column_x(side).0 + VALUE_X;
    code.push(v5::fill(x, 0, VALUE_W, v5::HEIGHT, 0));
    // All the values' glyphs in one write, then one GLYPHS each.
    let mut all = Vec::new();
    let mut at = Vec::new();
    for value in values {
        at.push((v5::TEXT + all.len() as u32, value.chars().count()));
        all.extend(v5::text_indices(value));
    }
    code.push(v5::write(v5::TEXT, &all));
    for (i, &(text, n)) in at.iter().enumerate() {
        let right = x + VALUE_W as i32 - ADVANCE * n as i32;
        code.push(v5::glyphs_k(right, baseline(i), 15, text, n));
    }
}

/// The marks in the middle to read the level line against, as lines at `level`.
fn marks(code: &mut Vec<Vec<u8>>, level: u8) {
    code.push(v5::line_k(CX - 60, CY, CX - 20, CY, level));
    code.push(v5::line_k(CX + 20, CY, CX + 60, CY, level));
}

/// The level line, kept so each frame only erases the last one and draws the next.
#[derive(Default)]
struct LevelLine {
    last: Option<[i32; 4]>,
}

impl LevelLine {
    /// Instructions that move the line to where gravity `acc` (mg, head frame) puts it, without showing it. Returns
    /// the rows that changed, to show.
    fn draw(&mut self, code: &mut Vec<Vec<u8>>, acc: Option<[f64; 3]>) -> Option<(i32, i32)> {
        let next = acc.and_then(line_for);
        if next == self.last {
            return None;
        }
        let mut rows: Vec<i32> = Vec::new();
        if let Some([x0, y0, x1, y1]) = self.last {
            code.push(v5::line_k(x0, y0, x1, y1, 0));
            rows.extend([y0, y1]);
            // The erased line may have crossed the marks.
            if y0.min(y1) <= CY && y0.max(y1) >= CY {
                marks(code, 6);
                rows.push(CY);
            }
        }
        if let Some([x0, y0, x1, y1]) = next {
            code.push(v5::line_k(x0, y0, x1, y1, 15));
            rows.extend([y0, y1]);
        }
        self.last = next;
        let top = rows.iter().min()?.clamp(&0, &(v5::HEIGHT as i32 - 1));
        let bottom = rows.iter().max()?.clamp(&0, &(v5::HEIGHT as i32 - 1));
        Some((*top, *bottom))
    }
}

/// Where the level line goes for gravity `acc`: across the band's middle when the head is level.
fn line_for([ax, ay, az]: [f64; 3]) -> Option<[i32; 4]> {
    let across = ax.hypot(ay);
    if across < 1.0 {
        return None;
    }
    // Looking up (acc_z below 0) moves the horizon down; tilting the head right turns it the other way, along
    // (acc_y, acc_x) on the panel, y down.
    let pitch = (-az).atan2(across);
    let cy = (CY as f64 + pitch * PITCH_PX).clamp(-2000.0, 2000.0);
    let (dx, dy) = (ay / across * HALF_LINE, ax / across * HALF_LINE);
    Some([CX as f64 - dx, cy - dy, CX as f64 + dx, cy + dy].map(|v| v.round() as i32))
}

/// The readings asked for in turn, one request a frame: the accelerometer is in both, so it's fresh every frame.
fn groups() -> [Vec<u8>; 2] {
    let acc = [prop::ACC_X, prop::ACC_Y, prop::ACC_Z];
    let rest: Vec<u8> = READINGS.iter().chain(&MORE).map(|r| r.1).filter(|p| !acc.contains(p)).collect();
    let (first, second) = rest.split_at(9);
    [[&acc[..], first].concat(), [&acc[..], second].concat()]
}

/// A read in flight on one arm: its tag, what it asked for, and when.
struct Read {
    tag: u8,
    ids: Vec<u8>,
    sent: std::time::Instant,
}

/// The index in a column's readings of property `id`.
fn reading_index(id: u8) -> usize {
    READINGS.iter().chain(&MORE).position(|r| r.1 == id).unwrap()
}

/// `boost` adds that many levels to Even's automatic brightness.
pub async fn sensors(side: Option<Side>, boost: u8) -> Result<(), Error> {
    let mut arms = crate::v0::arms(side).await?;
    let mut lead = Duration::ZERO;
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        crate::v0::upload_font(arm, SCALE).await?;
        arm.run([
            v5::set(prop::IMU_RATE, 104),
            v5::set(prop::GYRO_RATE, 104),
            v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0),
        ])
        .await?;
        let sides: Vec<Side> = crate::v0::SIDES.into_iter().filter(|s| side.is_none_or(|only| only == *s)).collect();
        let mut code = vec![v5::fixed_args()];
        marks(&mut code, 6);
        for &s in &sides {
            column_labels(&mut code, s);
        }
        code.push(v5::draw(0, 0, v5::WIDTH, v5::HEIGHT));
        arm.run(code).await?;
        arm.flush().await?;
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
    // Brightness follows the right arm's light sensor, set from here so it can be boosted and both eyes match. The
    // sensor only takes readings while something uses it, which a running program does: this one is a single END,
    // so it never runs anything that could get in the way of the drawing.
    for arm in &mut arms {
        start_idle_program(arm, IDLE).await?;
    }
    let mut brightness: Option<u32> = None;
    let max_code = arms.iter().map(|a| a.max_code).min().unwrap_or(64);
    println!("running; Ctrl-C to stop");

    let mut next_due: Option<u32> = None;
    let mut level = LevelLine::default();
    // The columns take turns being redrawn.
    let sides: Vec<Side> = arms.iter().map(|a| a.side).collect();
    let mut columns = sides.into_iter().cycle();
    let mut readings: Vec<Vec<u32>> = vec![vec![v5::UNKNOWN; READINGS.len() + MORE.len()]; arms.len()];
    let mut reads: Vec<Option<Read>> = arms.iter().map(|_| None).collect();
    let groups = groups();
    let mut queued: Vec<std::collections::VecDeque<u16>> = vec![Default::default(); arms.len()];
    let mut frames = 0u64;
    let mut recent = std::collections::VecDeque::new();
    let mut reported = std::time::Instant::now();
    // Time spent reading, waiting for room and sending, for the progress log.
    let mut spent = [Duration::ZERO; 3];
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        let text_frame = frames.is_multiple_of(TEXT_EVERY);
        frames += 1;
        let now = std::time::Instant::now();
        recent.push_back(now);
        while recent.len() > 30 {
            recent.pop_front();
        }
        let rate = match (recent.front(), recent.len()) {
            (Some(first), n) if n > 1 => (n - 1) as f64 / now.duration_since(*first).as_secs_f64(),
            _ => 0.0,
        };
        if reported.elapsed() > Duration::from_secs(5) {
            println!("frame {frames}, {rate:.1}/s");
            let total: Duration = spent.iter().sum();
            link::trace(format_args!("read {:?}, wait {:?}, send {:?} of {total:?}", spent[0], spent[1], spent[2]));
            spent = [Duration::ZERO; 3];
            for (arm, values) in arms.iter().zip(&readings) {
                link::trace(format_args!("{}: {}, light {}", arm.side.letter(), arm.state(), format(values[8], 2)));
            }
            reported = now;
        }
        let frame = async {
            // Take in the readings that have arrived, and ask for the next ones. Waiting for them would cost a
            // round trip behind the frames in flight every frame; instead each frame shows the latest in.
            let mut phase = std::time::Instant::now();
            for ((arm, read), have) in arms.iter_mut().zip(&mut reads).zip(&mut readings) {
                arm.poll().await?;
                if let Some(r) = read
                    && let Some(reply) = arm.reply(r.tag)
                {
                    for (&id, value) in r.ids.iter().zip(reply?.as_chunks::<4>().0) {
                        have[reading_index(id)] = u32::from_le_bytes(*value);
                    }
                    *read = None;
                }
                if read.as_ref().is_none_or(|r| r.sent.elapsed() > Duration::from_millis(500)) {
                    let ids = groups[(frames % 2) as usize].clone();
                    let tag = arm.request(v5::ctrl::PROPS, &ids).await?;
                    *read = Some(Read { tag, ids, sent: std::time::Instant::now() });
                }
            }
            spent[0] += phase.elapsed();
            phase = std::time::Instant::now();

            let mut code = Vec::new();
            let column = text_frame.then(|| columns.next()).flatten();
            if let Some((arm, values)) = column.and_then(|c| arms.iter().zip(&readings).find(|(a, _)| a.side == c)) {
                let mut shown: Vec<String> =
                    READINGS.iter().chain(&MORE).zip(values).map(|(r, &v)| format(v, r.2)).collect();
                shown.push(format!("{rate:.1}"));
                column_values(&mut code, arm.side, &shown);
            }
            // Both arms' accelerometers averaged, so both eyes get the same line.
            let known: Vec<[f64; 3]> = readings
                .iter()
                .filter(|r| r[..3].iter().all(|&v| v != v5::UNKNOWN))
                .map(|r| [0, 1, 2].map(|i| r[i] as i32 as f64))
                .collect();
            let acc = (!known.is_empty())
                .then(|| [0, 1, 2].map(|i| known.iter().map(|a| a[i]).sum::<f64>() / known.len() as f64));
            if column.is_some() {
                let lux = readings.iter().map(|r| r[reading_index(prop::LIGHT)]).find(|&l| l != v5::UNKNOWN);
                if let Some(lux) = lux {
                    let want = brightness_for(lux, boost);
                    if brightness.is_none_or(|b| b.abs_diff(want) >= 2) {
                        code.push(v5::set(prop::BRIGHTNESS, want));
                        brightness = Some(want);
                        link::trace(format_args!("brightness {want} for {} lux", format(lux, 2)));
                    }
                }
            }
            let changed = level.draw(&mut code, acc);
            if code.is_empty() {
                // Nothing moved: wait for the next beat.
                tokio::time::sleep(PERIOD).await;
                return Ok::<(), Error>(());
            }

            let waits = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| async move {
                while queued.len() >= QUEUE {
                    arm.wait_for(queued.pop_front().unwrap()).await?;
                }
                Ok::<(), Error>(())
            });
            futures::future::try_join_all(waits).await?;
            spent[1] += phase.elapsed();
            phase = std::time::Instant::now();
            // Time for this frame's packets too, two per connection event, so both arms have it before it's due.
            let packets = v5::pack(code.clone(), max_code).len() as u32 + 1;
            let interval = arms.iter().map(|a| a.interval()).max().unwrap_or_default();
            let lead = (lead + interval * packets / 2).as_micros() as u32;
            // On the beat if this frame can make it there in time, else as soon as it can, and the beat moves.
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
            let sends = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| {
                let mut code = code.clone();
                code.push(v5::sleep_until(arm.arm_time(due)));
                if let Some((top, bottom)) = changed {
                    code.push(v5::draw(BOX_X, top, BOX_W, (bottom - top + 1) as u32));
                }
                if let Some(side) = column {
                    code.push(v5::draw(column_x(side).0 + VALUE_X, 0, VALUE_W, v5::HEIGHT));
                }
                async move {
                    arm.run(code).await?;
                    queued.push_back(arm.sent());
                    Ok::<(), Error>(())
                }
            });
            futures::future::try_join_all(sends).await?;
            spent[2] += phase.elapsed();
            Ok::<(), Error>(())
        };
        tokio::select! {
            _ = &mut stop => break,
            done = frame => done?,
        }
    }
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

/// Stops a program running on the arms and switches the panel off.
pub async fn stop(side: Option<Side>) -> Result<(), Error> {
    let mut arms = crate::v0::arms(side).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_evens_brightness_table() {
        assert_eq!(brightness_for(0, 0), 0);
        assert_eq!(brightness_for(0, 5), 5);
        assert_eq!(brightness_for(100 * 100, 0), 15);
        assert_eq!(brightness_for(5000 * 100, 5), 42);
    }

    #[test]
    fn formats_readings() {
        assert_eq!(format(339, 1), "33.9");
        assert_eq!(format(5, 1), "0.5");
        assert_eq!(format((-1234i32) as u32, 2), "-12.34");
        assert_eq!(format((-7i32) as u32, 0), "-7");
        assert_eq!(format(v5::UNKNOWN, 1), "?");
    }

    #[test]
    fn level_line_tilts_with_the_head() {
        // Head upright: level, through the middle.
        assert_eq!(line_for([0.0, 1000.0, 0.0]), Some([CX - 100, CY, CX + 100, CY]));
        // Head tilted right (acc_x below 0): the right end goes up.
        let [_, y0, _, y1] = line_for([-500.0, 866.0, 0.0]).unwrap();
        assert!(y1 < y0);
        // Looking up (acc_z below 0): the line goes down.
        assert!(line_for([0.0, 900.0, -400.0]).unwrap()[1] > CY);
    }

    #[test]
    fn level_line_redraws_only_what_moved() {
        let mut level = LevelLine::default();
        let mut code = Vec::new();
        assert_eq!(level.draw(&mut code, Some([0.0, 1000.0, 0.0])), Some((CY, CY)));
        assert_eq!(code.len(), 1);
        // The same reading again changes nothing.
        let mut code = Vec::new();
        assert_eq!(level.draw(&mut code, Some([0.0, 1000.0, 0.0])), None);
        assert!(code.is_empty());
        // Moving off the marks erases the old line and puts the marks back.
        let mut code = Vec::new();
        let (top, bottom) = level.draw(&mut code, Some([0.0, 900.0, -400.0])).unwrap();
        assert_eq!(code.len(), 4);
        assert!(top == CY && bottom > CY);
    }
}
