//! `power`: the glasses' own batteries, every 5 s: for each arm its charge, voltage, current, temperature and
//! whether it's charging, how long its charge lasts at the present current (an 80 mAh cell), and a graph of both
//! charges since the app started. Tap either TouchBar to put the panel to sleep or wake it.

use super::kit::{Canvas, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;
use crate::v5::{self, prop};

pub(crate) const APP: App = App {
    name: "power",
    about: "the glasses' batteries: each arm's charge, volts, current, temperature, time left, and a graph",
    fun: false,
    options: Options { font: 2, big: 4, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0)
}

const EVERY: f64 = 5.0;
/// Each arm's cell, mAh.
const CELL: f64 = 80.0;
const IDS: [u8; 6] = [prop::BATT_PCT, prop::BATT_MV, prop::BATT_MA, prop::BATT_TEMP, prop::CHARGE, prop::IN_CASE];
/// The columns' left edges: the left arm's, the right's.
const COLUMNS: [i32; 2] = [40, 360];
/// The graph: x, y, w, h.
const GRAPH: [i32; 4] = [56, 274, 560, 88];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Reading {
    pct: Option<u32>,
    mv: Option<u32>,
    /// Positive while discharging.
    ma: Option<i32>,
    /// 0.1 °C.
    temp: Option<i32>,
    /// 0 not charging, 1 charging, 2 full.
    charge: Option<u32>,
    in_case: bool,
}

impl Reading {
    fn from(values: &[u32]) -> Reading {
        let known = |i: usize| values.get(i).copied().filter(|&v| v != v5::UNKNOWN);
        Reading {
            pct: known(0).filter(|&p| p <= 100),
            mv: known(1),
            ma: known(2).map(|v| v as i32),
            temp: known(3).map(|v| v as i32),
            charge: known(4),
            in_case: known(5) == Some(1),
        }
    }

    /// The column's lines under the big %: volts and current, temperature, state, time left (17 characters at
    /// most, to fit a column).
    fn lines(&self) -> [String; 4] {
        let volts = self.mv.map_or("? V".into(), |mv| format!("{:.2} V", mv as f64 / 1000.0));
        let amps = self.ma.map_or("? mA".into(), |ma| format!("{ma} mA"));
        let temp = self.temp.map_or("? C".into(), |t| format!("{:.1} C", t as f64 / 10.0));
        let state = match (self.charge, self.in_case) {
            (Some(1), _) => "charging",
            (Some(2), _) => "full",
            (_, true) => "in the case",
            _ => "on battery",
        };
        [format!("{volts}  {amps}"), temp, state.to_string(), left(self.pct, self.ma)]
    }
}

/// How long a charge of `pct` % lasts at `ma` mA from an 80 mAh cell.
fn left(pct: Option<u32>, ma: Option<i32>) -> String {
    match (pct, ma) {
        (Some(pct), Some(ma)) if ma > 0 => {
            let minutes = (CELL * pct as f64 / 100.0 / ma as f64 * 60.0).round() as u32;
            format!("{} h {:02} min left", minutes / 60, minutes % 60)
        }
        (_, Some(ma)) if ma < 0 => "taking charge".into(),
        _ => "time left ?".into(),
    }
}

/// Points drawn on the graph at most, however long the session: each costs the link 9 bytes a line.
const POINTS: usize = 60;

/// The graph of each arm's charge (left dim, right bright) over the session, as lines (from POINTS samples at
/// most, evenly spread); and its scale's ends.
fn graph(history: &[(f64, [Option<u32>; 2])]) -> (Vec<([f64; 4], u8)>, u32, u32) {
    let n = history.len();
    let picked: Vec<(f64, [Option<u32>; 2])> =
        if n > POINTS { (0..POINTS).map(|i| history[i * (n - 1) / (POINTS - 1)]).collect() } else { history.to_vec() };
    let history = &picked[..];
    let known = history.iter().flat_map(|(_, p)| p.iter().flatten().copied());
    let (lo, hi) = known.fold((100u32, 0u32), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let (lo, hi) = if lo > hi { (0, 100) } else { (lo.saturating_sub(5), (hi + 5).min(100)) };
    let [x, y, w, h] = GRAPH.map(|v| v as f64);
    let (t0, t1) = (history.first().map_or(0.0, |s| s.0), history.last().map_or(1.0, |s| s.0));
    let span = (t1 - t0).max(1.0);
    let at = |t: f64, p: u32| [x + w * (t - t0) / span, y + h * (1.0 - (p - lo) as f64 / (hi - lo).max(1) as f64)];
    let mut lines = vec![([x, y + h, x + w, y + h], 4)];
    for (arm, level) in [(0, 8), (1, 14)] {
        for pair in history.windows(2) {
            if let (Some(a), Some(b)) = (pair[0].1[arm], pair[1].1[arm]) {
                let (a, b) = (at(pair[0].0, a), at(pair[1].0, b));
                lines.push(([a[0], a[1], b[0], b[1]], level));
            }
        }
    }
    (lines, lo, hi)
}

/// "12 min", "1 h 05 min".
fn session(seconds: f64) -> String {
    let m = (seconds / 60.0) as u32;
    if m >= 60 { format!("{} h {:02} min", m / 60, m % 60) } else { format!("{m} min") }
}

fn draw_static(c: &mut Canvas) {
    c.clear(0, 56, 640, 344);
    c.text(16, 66, "GLASSES POWER", 8);
    c.text(COLUMNS[0], 92, "LEFT", 8);
    c.text(COLUMNS[1], 92, "RIGHT", 8);
    c.line(320, 96, 320, 246, 4);
    c.show(0, 56, 640, 344);
}

/// What the panel shows, to redraw only what changed.
#[derive(Default)]
struct Shown {
    columns: [Option<(String, [String; 4])>; 2],
}

fn run<'a>(g: &'a mut Glasses, _args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let started = g.now();
        let mut history: Vec<(f64, [Option<u32>; 2])> = Vec::new();
        let (mut shown, mut awake, mut next) = (Shown::default(), true, g.now());
        g.frame(|_, c| draw_static(c)).await?;
        while g.running() {
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap {
                    awake = !awake;
                    if awake {
                        g.frame(|_, c| draw_static(c)).await?;
                        shown = Shown::default();
                        next = g.now();
                    } else {
                        g.sleep_panel().await?;
                    }
                }
            }
            if g.now() >= next {
                next += EVERY;
                let mut readings = [Reading::default(); 2];
                for (side, values) in g.props(&IDS).await? {
                    readings[(side == Side::Right) as usize] = Reading::from(&values);
                }
                history.push((g.now() - started, readings.map(|r| r.pct)));
                if awake {
                    draw(g, &readings, &history, g.now() - started, &mut shown).await?;
                }
            }
            g.wait(0.2).await?;
        }
        Ok(())
    })
}

async fn draw(
    g: &mut Glasses,
    readings: &[Reading; 2],
    history: &[(f64, [Option<u32>; 2])],
    elapsed: f64,
    shown: &mut Shown,
) -> Result<(), Error> {
    let columns = readings.map(|r| Some((r.pct.map_or("?".into(), |p| format!("{p}%")), r.lines())));
    let (lines, lo, hi) = graph(history);
    let old = std::mem::take(&mut shown.columns);
    let clock = chrono::Local::now().format("%H:%M").to_string();
    g.frame(|_, c| {
        c.clear(520, 66, 104, 16);
        let w = c.width(&clock);
        c.text(624 - w, 66, &clock, 8);
        c.show(520, 66, 104, 16);
        for (i, (column, before)) in columns.iter().zip(&old).enumerate() {
            if column == before {
                continue;
            }
            let Some((pct, rows)) = column else { continue };
            let x = COLUMNS[i];
            c.clear(x, 110, 272, 138);
            c.big(x, 112, pct, 15);
            for (j, row) in rows.iter().enumerate() {
                c.text(x, 152 + 24 * j as i32, row, [12, 10, 10, 13][j]);
            }
            c.show(x, 110, 272, 138);
        }
        // The graph, with its scale and how long it covers.
        let [x, y, w, h] = GRAPH;
        c.clear(0, y - 10, 640, h + 40);
        c.lines(&lines);
        c.text(4, y - 6, &hi.to_string(), 7);
        c.text(4, y + h - 10, &lo.to_string(), 7);
        c.text(x, y + h + 10, "left", 8);
        c.text(x + 80, y + h + 10, "right", 14);
        let span = format!("over {}", session(elapsed));
        let sw = c.width(&span);
        c.text(x + w - sw, y + h + 10, &span, 7);
        c.show(0, y - 10, 640, h + 40);
    })
    .await?;
    shown.columns = columns;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_estimates() {
        let r = Reading::from(&[84, 3972, 38, 284, 0, 0]);
        assert_eq!(r.lines(), ["3.97 V  38 mA", "28.4 C", "on battery", "1 h 46 min left"]);
        assert!(r.lines().iter().all(|l| l.len() <= 17));
        assert_eq!(Reading::from(&[v5::UNKNOWN; 6]).pct, None);
        assert_eq!(left(Some(50), Some(-20)), "taking charge");
        assert_eq!(session(3900.0), "1 h 05 min");
    }

    #[test]
    fn graphs_the_charge() {
        let history = vec![(0.0, [Some(84), Some(87)]), (5.0, [Some(83), Some(87)]), (10.0, [None, Some(86)])];
        let (lines, lo, hi) = graph(&history);
        assert_eq!((lo, hi), (78, 92));
        // The axis, the left arm's one step and the right's two.
        assert_eq!(lines.len(), 4);
        // An hour of samples still draws at most POINTS of each arm's.
        let long: Vec<_> = (0..720).map(|i| (i as f64 * 5.0, [Some(90 - i / 20), Some(91 - i / 20)])).collect();
        assert!(graph(&long).0.len() <= 1 + 2 * (POINTS - 1));
    }
}
