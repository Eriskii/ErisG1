//! `sysmon`: the Mac's vitals on the glasses, about every 2 s: CPU use with a sparkline of the last minute, memory
//! used of the total, network in and out, disk free, the Mac's battery and the busiest process. Tap either
//! TouchBar to put the panel to sleep or wake it.
//!
//! A background thread reads them from macOS's own tools (`top`, `vm_stat`, `sysctl`, `netstat`, `df`, `pmset`, `ps`),
//! so a slow one never holds a frame up; only the rows that changed are redrawn.

use std::process::Command;
use std::sync::mpsc::{self, Receiver};

use super::kit::{Canvas, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;

pub(crate) const APP: App = App {
    name: "sysmon",
    about: "the Mac's vitals every 2 s: CPU with a sparkline, memory, network, disk, battery, busiest process",
    fun: false,
    options: Options { font: 2, big: 4, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0)
}

/// Samples kept for the sparkline: a minute at one every 2 s.
const HISTORY: usize = 30;
const EVERY: f64 = 2.0;
/// The sparkline's box: x, y, w, h.
const SPARK: [i32; 4] = [200, 96, 424, 50];
/// The rows under the CPU: their first top and spacing.
const ROWS_TOP: i32 = 184;
const ROW_STEP: i32 = 38;
/// The memory bar, right of its row: x and width.
const BAR: [i32; 2] = [440, 184];

#[derive(Clone, Debug, Default, PartialEq)]
struct Vitals {
    /// CPU in use, % of all cores.
    cpu: f64,
    mem_used: f64,
    mem_total: f64,
    /// Network, bytes a second.
    net_in: f64,
    net_out: f64,
    disk_free: f64,
    /// The Mac's battery: % and what it's doing; `None` without one.
    battery: Option<(u32, String)>,
    /// The busiest process and its CPU %.
    top: Option<(String, f64)>,
}

pub(crate) fn output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// CPU in use from `top -l 2 -n 0`: 100 less the idle % of its last "CPU usage" line (the first is since boot).
fn parse_top(text: &str) -> Option<f64> {
    let line = text.lines().rev().find(|l| l.contains("CPU usage:"))?;
    let idle = line.split(',').find(|part| part.contains("idle"))?;
    let idle: f64 = idle.trim().split('%').next()?.trim().parse().ok()?;
    Some((100.0 - idle).clamp(0.0, 100.0))
}

/// Memory in use from `vm_stat`, bytes: active, wired and compressed pages, as Activity Monitor counts it.
fn parse_vm_stat(text: &str) -> Option<f64> {
    let page: f64 = text.split("page size of ").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let pages = |name: &str| -> f64 {
        text.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0.0)
    };
    Some((pages("Pages active") + pages("Pages wired down") + pages("Pages occupied by compressor")) * page)
}

/// Bytes in and out since boot over every interface but loopback, from `netstat -ib`: the link rows' Ibytes and
/// Obytes, counted from the end since the Address column is sometimes empty.
fn parse_netstat(text: &str) -> (u64, u64) {
    let (mut inward, mut outward) = (0, 0);
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 8 || cols[0].starts_with("lo") || !cols[2].starts_with("<Link#") {
            continue;
        }
        let n = cols.len();
        inward += cols[n - 5].parse::<u64>().unwrap_or(0);
        outward += cols[n - 2].parse::<u64>().unwrap_or(0);
    }
    (inward, outward)
}

/// Free space on the startup disk from `df -k /`, bytes.
fn parse_df(text: &str) -> Option<f64> {
    let cols: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    Some(cols.get(3)?.parse::<f64>().ok()? * 1024.0)
}

/// The battery from `pmset -g batt`: its % and state ("charging", "discharging", "charged"...), `None` without one.
pub(crate) fn parse_pmset(text: &str) -> Option<(u32, String)> {
    let line = text.lines().find(|l| l.contains("InternalBattery"))?;
    let fields: Vec<&str> = line.split('\t').nth(1)?.split(';').map(str::trim).collect();
    let pct = fields.first()?.trim_end_matches('%').parse().ok()?;
    Some((pct, fields.get(1).unwrap_or(&"").to_string()))
}

/// The busiest process from `ps -Ao %cpu=,comm= -r`: its name and CPU %.
fn parse_ps(text: &str) -> Option<(String, f64)> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (cpu, path) = line.split_once(char::is_whitespace)?;
    let name = path.trim().rsplit('/').next()?.to_string();
    Some((name, cpu.parse().ok()?))
}

/// Reads the vitals every EVERY seconds on a thread of its own.
fn sample(out: mpsc::Sender<Vitals>) {
    std::thread::spawn(move || {
        let total: f64 = output("sysctl", &["-n", "hw.memsize"]).trim().parse().unwrap_or(0.0);
        let mut last: Option<((u64, u64), std::time::Instant)> = None;
        loop {
            let started = std::time::Instant::now();
            // Takes a second: two samples a second apart.
            let cpu = parse_top(&output("top", &["-l", "2", "-n", "0", "-s", "1"])).unwrap_or(0.0);
            let bytes = parse_netstat(&output("netstat", &["-ib"]));
            let (net_in, net_out) = match last {
                Some(((i, o), at)) => {
                    let dt = at.elapsed().as_secs_f64().max(0.1);
                    (bytes.0.saturating_sub(i) as f64 / dt, bytes.1.saturating_sub(o) as f64 / dt)
                }
                None => (0.0, 0.0),
            };
            last = Some((bytes, std::time::Instant::now()));
            let vitals = Vitals {
                cpu,
                mem_used: parse_vm_stat(&output("vm_stat", &[])).unwrap_or(0.0),
                mem_total: total,
                net_in,
                net_out,
                disk_free: parse_df(&output("df", &["-k", "/"])).unwrap_or(0.0),
                battery: parse_pmset(&output("pmset", &["-g", "batt"])),
                top: parse_ps(&output("ps", &["-Ao", "%cpu=,comm=", "-r"])),
            };
            if out.send(vitals).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(EVERY).saturating_sub(started.elapsed()));
        }
    });
}

/// Made-up vitals at `t` seconds, for the preview.
fn made_up(t: f64) -> Vitals {
    let wave = |f: f64, phase: f64| (t * f + phase).sin();
    Vitals {
        cpu: (24.0 + 14.0 * wave(0.7, 0.0) + 6.0 * wave(2.3, 1.0)).clamp(0.0, 100.0),
        mem_used: (11.4 + 0.4 * wave(0.2, 0.5)) * 1e9,
        mem_total: 32.0 * 1024f64.powi(3),
        net_in: 1.2e6 + 0.9e6 * wave(0.9, 2.0),
        net_out: 8.0e4 + 6.0e4 * wave(1.3, 0.3),
        disk_free: 234.5e9,
        battery: Some((87, "charging".into())),
        top: Some(if wave(0.4, 0.0) > 0.0 { ("cargo".into(), 184.0) } else { ("Safari".into(), 34.0) }),
    }
}

/// "1.2 MB/s", "80 KB/s", "312 B/s".
fn rate(bytes: f64) -> String {
    match bytes {
        b if b >= 1e9 => format!("{:.1} GB/s", b / 1e9),
        b if b >= 1e6 => format!("{:.1} MB/s", b / 1e6),
        b if b >= 1e3 => format!("{:.0} KB/s", b / 1e3),
        b => format!("{b:.0} B/s"),
    }
}

/// The rows under the CPU, as text.
fn rows(v: &Vitals) -> [String; 5] {
    let gb = |b: f64| b / 1024f64.powi(3);
    [
        format!("MEM   {:.1} of {:.0} GB", gb(v.mem_used), gb(v.mem_total)),
        format!("NET   in {}  out {}", rate(v.net_in), rate(v.net_out)),
        format!("DISK  {:.0} GB free", v.disk_free / 1e9),
        match &v.battery {
            Some((pct, state)) if !state.is_empty() => format!("BATT  {pct}%, {state}"),
            Some((pct, _)) => format!("BATT  {pct}%"),
            None => "BATT  none, on power".into(),
        },
        match &v.top {
            Some((name, cpu)) => format!("TOP   {}  {cpu:.0}%", name.chars().take(20).collect::<String>()),
            None => "TOP   ?".into(),
        },
    ]
}

/// The sparkline of `history` (CPU %, oldest first) in SPARK, as lines.
fn sparkline(history: &[f64]) -> Vec<([f64; 4], u8)> {
    let [x, y, w, h] = SPARK.map(|v| v as f64);
    let at = |i: usize, v: f64| [x + w * i as f64 / (HISTORY - 1) as f64, y + h - h * v.clamp(0.0, 100.0) / 100.0];
    let mut lines: Vec<([f64; 4], u8)> = vec![([x, y + h, x + w, y + h], 4)];
    let start = HISTORY - history.len().min(HISTORY);
    for (i, pair) in history.windows(2).enumerate() {
        let (a, b) = (at(start + i, pair[0]), at(start + i + 1, pair[1]));
        lines.push(([a[0], a[1], b[0], b[1]], 13));
    }
    lines
}

/// What the panel shows, to redraw only what changed.
#[derive(Default)]
struct Shown {
    clock: String,
    cpu: String,
    rows: [String; 5],
    bar: i32,
}

fn draw_static(c: &mut Canvas) {
    c.clear(0, 56, 640, 344);
    c.text(16, 66, "MAC", 8);
    c.text(16, 92, "CPU", 8);
    c.show(0, 56, 640, 344);
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let (tx, rx): (mpsc::Sender<Vitals>, Receiver<Vitals>) = mpsc::channel();
        let mut history: Vec<f64> = Vec::new();
        if g.preview() {
            // A minute already seen, so the sparkline has something to show.
            history = (0..HISTORY - 1).map(|i| made_up(i as f64 * EVERY - 60.0).cpu).collect();
        } else {
            sample(tx);
        }
        let (mut shown, mut awake) = (Shown::default(), true);
        let mut next_made_up = 0.0;
        g.frame(|_, c| draw_static(c)).await?;
        while g.running() {
            let mut latest = None;
            if g.preview() {
                if g.now() >= next_made_up {
                    latest = Some(made_up(g.now()));
                    next_made_up += EVERY;
                }
            } else {
                while let Ok(v) = rx.try_recv() {
                    latest = Some(v);
                }
            }
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap {
                    awake = !awake;
                    if awake {
                        g.frame(|_, c| draw_static(c)).await?;
                        shown = Shown::default();
                    } else {
                        g.sleep_panel().await?;
                    }
                }
            }
            if let Some(v) = latest {
                history.push(v.cpu);
                if history.len() > HISTORY {
                    history.remove(0);
                }
                if awake {
                    draw(g, &v, &history, &mut shown).await?;
                }
            }
            g.wait(0.1).await?;
        }
        Ok(())
    })
}

async fn draw(g: &mut Glasses, v: &Vitals, history: &[f64], shown: &mut Shown) -> Result<(), Error> {
    let clock = chrono::Local::now().format("%H:%M").to_string();
    let cpu = format!("{:.0}%", v.cpu);
    let rows = rows(v);
    let bar = if v.mem_total > 0.0 { (BAR[1] as f64 * v.mem_used / v.mem_total).round() as i32 } else { 0 };
    let spark = sparkline(history);
    let old = std::mem::take(shown);
    g.frame(|_, c| {
        if clock != old.clock {
            c.clear(520, 66, 104, 16);
            let w = c.width(&clock);
            c.text(624 - w, 66, &clock, 8);
            c.show(520, 66, 104, 16);
        }
        if cpu != old.cpu {
            c.clear(16, 112, 176, 32);
            c.big(16, 112, &cpu, 15);
            c.show(16, 112, 176, 32);
        }
        // The sparkline, always: it moves on with every sample.
        let [x, y, w, h] = SPARK;
        c.clear(x, y - 2, w + 2, h + 4);
        c.lines(&spark);
        c.show(x, y - 2, w + 2, h + 4);
        for (i, (row, before)) in rows.iter().zip(&old.rows).enumerate() {
            if row != before {
                let top = ROWS_TOP + i as i32 * ROW_STEP;
                let w = if i == 0 { BAR[0] - 16 } else { 608 };
                c.clear(16, top, w, 16);
                c.text(16, top, row, if i == 0 { 13 } else { 11 });
                c.show(16, top, w, 16);
            }
        }
        if bar != old.bar {
            let top = ROWS_TOP + 4;
            c.clear(BAR[0], top, BAR[1] + 2, 9);
            c.rect(BAR[0], top, BAR[1] + 2, 9, 6);
            c.fill(BAR[0] + 1, top + 1, bar, 7, 12);
            c.show(BAR[0], top, BAR[1] + 2, 9);
        }
    })
    .await?;
    *shown = Shown { clock, cpu, rows, bar };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_macos_tools() {
        let top = "Processes: 1\nCPU usage: 3.0% user, 5.0% sys, 92.0% idle\nX\nCPU usage: 7.14% user, 10.71% sys, 82.15% idle\n";
        assert!((parse_top(top).unwrap() - 17.85).abs() < 1e-9);
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:     1000.\nPages active:   100.\n\
                  Pages wired down:   50.\nPages occupied by compressor:   10.\n";
        assert_eq!(parse_vm_stat(vm), Some(160.0 * 16384.0));
        let net = "Name  Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll\n\
                   lo0   16384 <Link#1>                        10     0       1000       10     0       1000     0\n\
                   en0   1500  <Link#11>   aa:bb:cc:dd:ee:ff   100    0       5000       90     0       2000     0\n\
                   en0   1500  192.168.1     192.168.1.2      100    -       5000       90     -       2000     -\n\
                   utun0 1380  <Link#18>                        5     0        300        5     0        400     0\n";
        assert_eq!(parse_netstat(net), (5300, 2400));
        let df = "Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted on\n\
                  /dev/disk3s1s1 971350180 10483540 229001880 5% 404167 2290018800 0% /\n";
        assert_eq!(parse_df(df), Some(229001880.0 * 1024.0));
        let batt =
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=123)\t87%; charging; 1:23 remaining present: true\n";
        assert_eq!(parse_pmset(batt), Some((87, "charging".to_string())));
        assert_eq!(parse_pmset("Now drawing from 'AC Power'\n"), None);
        assert_eq!(
            parse_ps("  34.5 /Applications/Safari.app/Contents/MacOS/Safari\n 2.0 x\n"),
            Some(("Safari".into(), 34.5))
        );
    }

    #[test]
    fn formats() {
        assert_eq!(rate(1.25e6), "1.2 MB/s");
        assert_eq!(rate(80_000.0), "80 KB/s");
        let r = rows(&made_up(0.0));
        assert!(r.iter().all(|row| row.chars().count() <= 38), "{r:?}");
        assert_eq!(sparkline(&[10.0, 20.0, 30.0]).len(), 3);
    }
}
