//! The commands for arms running g1v0 (`erisg1 v0 …`). Each arm is driven on its own; the arms find each other's
//! timing only through this program.

use std::time::Duration;

use crate::link::{self, Error};
use crate::proto::Side;
use crate::v5::{self, prop, trigger};
use crate::v5_link::{self, Arm, side_name};

/// Connects to the arm on `side`, or to every g1v0 arm in range.
async fn arms(side: Option<Side>) -> Result<Vec<Arm>, Error> {
    let adapter = link::adapter().await?;
    let wanted = match side {
        Some(side) => vec![side],
        None => vec![Side::Left, Side::Right],
    };
    let mut found = v5_link::scan(&adapter, Duration::from_secs(10), &wanted).await?;
    found.retain(|f| wanted.contains(&f.side));
    if found.is_empty() {
        return Err("no arm running g1v0 in range. Out of the charging case an arm only talks to a computer it has \
                    paired with"
            .into());
    }
    let mut arms = Vec::new();
    for f in found {
        if f.safe {
            eprintln!("{} arm is in safe mode: drawing is off, updates work", side_name(f.side));
        }
        arms.push(Arm::open(f).await?);
    }
    Ok(arms)
}

async fn disconnect(arms: Vec<Arm>) {
    for arm in arms {
        arm.disconnect().await;
    }
}

fn show(value: u32, unit: &str) -> String {
    if value == v5::UNKNOWN { "?".into() } else { format!("{}{unit}", value as i32) }
}

pub async fn scan() -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let found = v5_link::scan(&adapter, Duration::from_secs(10), &[Side::Left, Side::Right]).await?;
    if found.is_empty() {
        println!("no g1v0 arms in range");
    }
    for f in found {
        let safe = if f.safe { ", safe mode" } else { "" };
        println!("{} {}  rssi {}{safe}", f.side.letter(), f.name, f.rssi.unwrap_or(0));
    }
    Ok(())
}

pub async fn hello(side: Option<Side>) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    for arm in &mut arms {
        let h = arm.hello.clone();
        println!(
            "{} arm: g1v0 {}, boot {}, unhealthy boots {}",
            side_name(arm.side),
            h.version,
            h.boot_count,
            h.unhealthy_boots
        );
        let flags = [
            (h.safe_mode(), "safe mode"),
            (!h.panel_ok(), "no panel"),
            (h.in_case(), "in the case"),
            (h.bondable(), "pairable now"),
            (h.encrypted(), "paired link"),
        ];
        let flags: Vec<&str> = flags.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
        if !flags.is_empty() {
            println!("  {}", flags.join(", "));
        }
        let ids = [
            prop::BATT_PCT,
            prop::BATT_MV,
            prop::CHARGE,
            prop::BATT_TEMP,
            prop::WORN,
            prop::PITCH,
            prop::ROLL,
            prop::BRIGHTNESS,
            prop::LIGHT,
        ];
        let p = arm.props(&ids).await?;
        let charge = ["not charging", "charging", "charged"].get(p[2] as usize).copied().unwrap_or("?");
        println!("  battery {} ({} mV, {charge}, {} °C)", show(p[0], "%"), show(p[1], ""), show_tenths(p[3]));
        println!("  worn {}, pitch {}, roll {}", show(p[4], ""), show(p[5], "°"), show(p[6], "°"));
        println!("  brightness {}, light {}", show(p[7], ""), show_hundredths(p[8], " lux"));
    }
    disconnect(arms).await;
    Ok(())
}

fn show_tenths(value: u32) -> String {
    if value == v5::UNKNOWN { "?".into() } else { format!("{:.1}", value as i32 as f64 / 10.0) }
}

fn show_hundredths(value: u32, unit: &str) -> String {
    if value == v5::UNKNOWN { "?".into() } else { format!("{:.0}{unit}", value as f64 / 100.0) }
}

/// The test pattern: a frame round the panel, a grid every 64 pixels, and the 16 grey levels across the middle.
fn test_pattern() -> Vec<(i32, i32, u32, u32, u8)> {
    let (w, h) = (v5::WIDTH, v5::HEIGHT);
    let mut fills = vec![(0, 0, w, h, 0)];
    for x in (64..w).step_by(64) {
        fills.push((x as i32, 0, 1, h, 5));
    }
    for y in (64..h).step_by(64) {
        fills.push((0, y as i32, w, 1, 5));
    }
    fills.extend([(0, 0, w, 3, 15), (0, h as i32 - 3, w, 3, 15), (0, 0, 3, h, 15), (w as i32 - 3, 0, 3, h, 15)]);
    for level in 0..16 {
        fills.push((32 + level as i32 * 36, 208, 36, 64, level));
    }
    fills.extend([(w as i32 / 2 - 20, h as i32 / 2, 41, 1, 15), (w as i32 / 2, h as i32 / 2 - 20, 1, 41, 15)]);
    fills
}

/// Draws the test pattern on each arm and reads the panel back to check every pixel arrived.
pub async fn test(side: Option<Side>) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    let pattern = test_pattern();
    let mut mirror = v5::Mirror::new();
    for &(x, y, w, h, level) in &pattern {
        mirror.fill(x, y, w, h, level);
    }
    for arm in &mut arms {
        let started = std::time::Instant::now();
        let mut code: Vec<Vec<u8>> = pattern.iter().map(|&(x, y, w, h, level)| v5::fill(x, y, w, h, level)).collect();
        code.push(v5::draw(0, 0, v5::WIDTH, v5::HEIGHT));
        arm.run(code).await?;
        arm.flush().await?;
        println!("{} arm: drawn in {} ms", side_name(arm.side), started.elapsed().as_millis());

        // PANEL_READ reads rows back from the panel's own memory, so this also checks the panel took them.
        let mut y0 = 0;
        let mut mismatched = Vec::new();
        while y0 < v5::HEIGHT {
            let mut body = (y0 as u16).to_le_bytes().to_vec();
            body.extend(((v5::HEIGHT - y0) as u16).to_le_bytes());
            let reply = match arm.control(v5::ctrl::PANEL_READ, &body).await {
                Ok(reply) => reply,
                Err(e) => {
                    println!("  can't read the panel back: {e}");
                    break;
                }
            };
            let crc = u32::from_le_bytes(reply[..4].try_into()?);
            let rows = u16::from_le_bytes(reply[4..6].try_into()?) as u32;
            if rows == 0 {
                break;
            }
            if crc != mirror.crc(y0, rows) {
                mismatched.push(format!("{y0}–{}", y0 + rows - 1));
            }
            y0 += rows;
        }
        if y0 >= v5::HEIGHT {
            if mismatched.is_empty() {
                println!("  read back from the panel: all {} rows match", v5::HEIGHT);
            } else {
                println!("  read back from the panel: rows {} differ", mismatched.join(", "));
            }
        }
    }
    println!("in the charging case the panel's LEDs stay dark while the case talks to the arm");
    disconnect(arms).await;
    Ok(())
}

async fn upload_font(arm: &mut Arm, scale: u8) -> Result<(), Error> {
    let font = v5::font(scale);
    let parts = v5::upload(v5::FONT, &font, arm.max_code);
    arm.run(parts).await?;
    arm.flush().await
}

/// Instructions that clear a box at `x`, `y` and draw `text` in it, `scale` times the 8 × 8 font, then show it.
fn text_box(x: i32, y: i32, scale: u8, text: &str, level: u8) -> Vec<Vec<u8>> {
    let indices = v5::text_indices(text);
    let size = 8 * scale as u32;
    let w = size * indices.len() as u32;
    vec![
        v5::fill(x, y, w, size, 0),
        v5::write(v5::TEXT, &indices),
        v5::glyphs(x, y + 7 * scale as i32, level, indices.len()),
        v5::draw(x, y, w, size),
    ]
}

/// Text in the 8 × 8 font, `scale` times over, with its top left corner at `x`, `y`.
pub async fn text(side: Option<Side>, text: &str, scale: u8, x: i32, y: i32) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    for arm in &mut arms {
        upload_font(arm, scale).await?;
        let mut code = vec![v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0)];
        for (i, line) in text.split("\\n").enumerate() {
            code.extend(text_box(x, y + i as i32 * 9 * scale as i32, scale, line, 15));
        }
        code.push(v5::draw(0, 0, v5::WIDTH, v5::HEIGHT));
        arm.run(code).await?;
        arm.flush().await?;
    }
    disconnect(arms).await;
    Ok(())
}

const HUD_MARGIN: i32 = 12;
/// How often the HUD reads head pitch and roll. The accelerometer gives 13 readings a second unless asked for more.
const HUD_POSE_EVERY: Duration = Duration::from_millis(150);
/// Frames sent ahead of the one each arm is drawing, so the link never waits on a round trip.
const HUD_QUEUE: usize = 3;

/// Instructions that clear the full-width band at `y` and draw `left` at its left end and `right` at its right, but
/// don't show it yet.
fn band(y: i32, scale: u8, left: &str, right: &str, level: u8) -> Vec<Vec<u8>> {
    let size = 8 * scale as i32;
    let mut code = vec![v5::fill(0, y, v5::WIDTH, size as u32, 0)];
    let right_x = v5::WIDTH as i32 - HUD_MARGIN - size * right.chars().count() as i32;
    for (x, text) in [(HUD_MARGIN, left), (right_x, right)] {
        let indices = v5::text_indices(text);
        code.push(v5::write(v5::TEXT, &indices));
        code.push(v5::glyphs(x, y + 7 * scale as i32, level, indices.len()));
    }
    code
}

/// A clock top left, both arms' batteries top right, head pitch and roll (the arms' mean) bottom left and a frame
/// counter bottom right, the same in both eyes, until Ctrl-C. Each frame goes to both arms ahead of time with the moment to show it, in each arm's own clock, so both
/// eyes change together (VM.md §7).
pub async fn hud(side: Option<Side>, scale: u8) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    let mut lead = Duration::ZERO;
    for arm in &mut arms {
        upload_font(arm, scale).await?;
        arm.run([v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0), v5::draw(0, 0, v5::WIDTH, v5::HEIGHT)]).await?;
        arm.flush().await?;
        // macOS sometimes grants it only on the second request.
        for _ in 0..2 {
            match arm.ask_for_fast_link().await {
                Ok(interval) if interval <= Duration::from_millis(15) => break,
                Ok(_) => {}
                Err(e) => link::trace(format_args!("{e}")),
            }
        }
        let round_trip = arm.sync_clock().await?;
        // Time for a frame to reach the arm before it's due: two connection intervals, the clock's error (at most
        // half the round trip) and 15 ms, as §7 suggests.
        lead = lead.max(2 * arm.interval() + round_trip / 2 + Duration::from_millis(15));
        link::trace(format_args!("{}: round trip {round_trip:?}, interval {:?}", arm.side.letter(), arm.interval()));
    }
    println!("running; Ctrl-C to stop");
    let size = 8 * scale as i32;
    let bottom = v5::HEIGHT as i32 - HUD_MARGIN - size;
    let mut battery = vec![v5::UNKNOWN; arms.len()];
    let mut battery_read: Option<std::time::Instant> = None;
    let mut pose = vec![(v5::UNKNOWN, v5::UNKNOWN); arms.len()];
    let mut pose_read: Option<std::time::Instant> = None;
    let mut queued: Vec<std::collections::VecDeque<u16>> = vec![Default::default(); arms.len()];
    let mut frames = 0u64;
    let mut recent = std::collections::VecDeque::new();
    let mut reported = std::time::Instant::now();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
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
            reported = now;
        }
        let frame = async {
            if battery_read.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                for (level, arm) in battery.iter_mut().zip(&mut arms) {
                    *level = arm.props(&[prop::BATT_PCT]).await?[0];
                }
                battery_read = Some(std::time::Instant::now());
            }
            if pose_read.is_none_or(|t| t.elapsed() > HUD_POSE_EVERY) {
                let reads = futures::future::join_all(arms.iter_mut().map(|a| a.props(&[prop::PITCH, prop::ROLL])));
                for (p, read) in pose.iter_mut().zip(reads.await) {
                    let read = read?;
                    *p = (read[0], read[1]);
                }
                pose_read = Some(std::time::Instant::now());
            }
            // Room on both arms first, then one due time for both.
            let waits = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| async move {
                while queued.len() >= HUD_QUEUE {
                    arm.wait_for(queued.pop_front().unwrap()).await?;
                }
                Ok::<(), Error>(())
            });
            futures::future::try_join_all(waits).await?;
            let due = v5_link::host_us().wrapping_add(lead.as_micros() as u32);
            let clock = chrono::Local::now().format("%H:%M:%S").to_string();
            let counter = format!("#{frames} {rate:.0}/s");
            // Both eyes show the same: anything that differs between them won't fuse into one image.
            let power: Vec<String> =
                arms.iter().zip(&battery).map(|(arm, &b)| format!("{}{}", arm.side.letter(), show(b, "%"))).collect();
            let power = power.join(" ");
            let mean = |values: &mut dyn Iterator<Item = u32>| {
                let known: Vec<i32> = values.filter(|&v| v != v5::UNKNOWN).map(|v| v as i32).collect();
                if known.is_empty() { v5::UNKNOWN } else { (known.iter().sum::<i32>() / known.len() as i32) as u32 }
            };
            let pitch = mean(&mut pose.iter().map(|p| p.0));
            let roll = mean(&mut pose.iter().map(|p| p.1));
            let head = format!("{:>4} {:>4}", show(pitch, ""), show(roll, ""));
            let sends = arms.iter_mut().zip(&mut queued).map(|(arm, queued)| {
                let mut code = band(HUD_MARGIN, scale, &clock, &power, 15);
                code.extend(band(bottom, scale, &head, &counter, 10));
                code.push(v5::sleep_until(arm.arm_time(due)));
                code.push(v5::draw(0, HUD_MARGIN, v5::WIDTH, size as u32));
                code.push(v5::draw(0, bottom, v5::WIDTH, size as u32));
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
    }
    for arm in &mut arms {
        arm.run([v5::set(prop::PANEL, 0)]).await?;
        arm.flush().await?;
    }
    disconnect(arms).await;
    Ok(())
}

/// Prints the head's pitch and roll, the accelerometer and, with `gyro`, the gyroscope, `hz` times a second.
pub async fn imu(side: Option<Side>, hz: u32, gyro: bool) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    let rate = [13, 26, 52, 104].into_iter().find(|&r| r >= hz).unwrap_or(104);
    for arm in &mut arms {
        arm.run([v5::set(prop::IMU_RATE, rate), v5::set(prop::GYRO_RATE, if gyro { rate } else { 0 })]).await?;
        arm.flush().await?;
    }
    println!("arm   pitch  roll    acc x     y     z (mg){}", if gyro { "    gyro x      y      z (°/s)" } else { "" });
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / hz.max(1) as f64));
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tick.tick() => {}
        }
        for arm in &mut arms {
            let p = arm
                .props(&[
                    prop::PITCH,
                    prop::ROLL,
                    prop::ACC_X,
                    prop::ACC_Y,
                    prop::ACC_Z,
                    prop::GYR_X,
                    prop::GYR_Y,
                    prop::GYR_Z,
                ])
                .await?;
            let mut line = format!(
                "{}   {:>5} {:>5}  {:>6} {:>5} {:>5}",
                arm.side.letter(),
                show(p[0], ""),
                show(p[1], ""),
                show(p[2], ""),
                show(p[3], ""),
                show(p[4], "")
            );
            if gyro {
                let dps =
                    |v: u32| if v == v5::UNKNOWN { "?".into() } else { format!("{:.1}", v as i32 as f64 / 1000.0) };
                line += &format!("  {:>7} {:>6} {:>6}", dps(p[5]), dps(p[6]), dps(p[7]));
            }
            println!("{line}");
        }
    }
    for arm in &mut arms {
        arm.run([v5::set(prop::IMU_RATE, 13), v5::set(prop::GYRO_RATE, 0)]).await?;
        arm.flush().await?;
    }
    disconnect(arms).await;
    Ok(())
}

/// Prints the TouchBar, wear, case and battery events the arms report, until Ctrl-C.
pub async fn events(side: Option<Side>) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    let mask: u16 = [trigger::TOUCH, trigger::WEAR, trigger::CASE, trigger::BATTERY].iter().map(|t| 1 << t).sum();
    for arm in &mut arms {
        arm.control(v5::ctrl::EVENTS, &mask.to_le_bytes()).await?;
    }
    println!("listening; tap the arms, put them on, open the case. Ctrl-C to stop");
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    'listen: loop {
        for arm in &mut arms {
            let event = tokio::select! {
                _ = &mut stop => break 'listen,
                event = arm.next_event(Duration::from_millis(200)) => event?,
            };
            if let Some(v5::Event { kind: v5::EventKind::Input { trigger: t, value }, .. }) = event {
                let what = match t {
                    trigger::TOUCH => v5::gesture_name(value).to_string(),
                    trigger::WEAR => {
                        if value == 1 {
                            "worn".into()
                        } else {
                            "taken off".into()
                        }
                    }
                    trigger::CASE => {
                        format!("in case {}, lid {}", value & 1, if value & 2 != 0 { "closed" } else { "open" })
                    }
                    trigger::BATTERY => "battery changed".into(),
                    _ => value.to_string(),
                };
                println!("{}: {} {what}", arm.side.letter(), trigger::name(t));
            }
        }
    }
    disconnect(arms).await;
    Ok(())
}

pub async fn panel(side: Option<Side>, state: &str) -> Result<(), Error> {
    let value = match state {
        "off" => 0,
        "on" => 1,
        _ => 2,
    };
    let mut arms = arms(side).await?;
    for arm in &mut arms {
        arm.run([v5::set(prop::PANEL, value)]).await?;
        arm.flush().await?;
    }
    disconnect(arms).await;
    Ok(())
}

/// Restarts each arm. `clear` also clears its count of bad boots, which takes an arm out of safe mode.
pub async fn reboot(side: Option<Side>, clear: bool) -> Result<(), Error> {
    let mut arms = arms(side).await?;
    for arm in &mut arms {
        // The arm restarts as it answers, so the reply may never come.
        let _ = arm.control(v5::ctrl::REBOOT, &[if clear { 1 } else { 0 }]).await;
        println!("{} arm restarting", side_name(arm.side));
    }
    disconnect(arms).await;
    Ok(())
}

pub async fn brightness(side: Option<Side>, level: Option<u8>, auto: bool) -> Result<(), Error> {
    if level.is_some_and(|l| l > 42) {
        return Err("brightness runs from 0 to 42".into());
    }
    let mut arms = arms(side).await?;
    for arm in &mut arms {
        if let Some(level) = level {
            arm.run([v5::set(prop::AUTO_BRIGHTNESS, 0), v5::set(prop::BRIGHTNESS, level as u32)]).await?;
        }
        if auto {
            // Only the right arm has a light sensor; the left follows it over the arm-to-arm link.
            arm.run([v5::set(prop::AUTO_BRIGHTNESS, 1)]).await?;
        }
        if let Err(e) = arm.flush().await {
            eprintln!("{} arm: {e}", side_name(arm.side));
        }
        let p = arm.props(&[prop::BRIGHTNESS, prop::AUTO_BRIGHTNESS, prop::BRIGHTNESS_CAP]).await?;
        let auto = if p[1] == 1 { ", auto" } else { "" };
        println!("{} arm: brightness {}/42{auto}, capped at {}", side_name(arm.side), show(p[0], ""), show(p[2], ""));
    }
    disconnect(arms).await;
    Ok(())
}
