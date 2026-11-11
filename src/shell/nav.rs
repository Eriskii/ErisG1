//! Navigation: a route being followed, as the host knows it (later from the phone's maps app), and what a navigation
//! tile shows of it: the next turn's arrow, distance and street, or a map around the position, heading up, with the
//! roads near and the route drawn over them. The map is lines: the Mac projects and clips them to the tile, and the
//! glasses draw them.

use super::icons::Icon;
use super::layout::MapView;

/// The most points a map holds: the route's, and the roads' (`BREAK` between lines counts as one).
pub(crate) const MAX_ROUTE: usize = 48;
pub(crate) const MAX_ROADS: usize = 112;
/// Between two lines of a map's points.
pub(crate) const BREAK: [u16; 2] = [0xFFFF, 0xFFFF];
/// How far apart the map's lines keep from its edge, pixels (the route is 3 px wide).
const INSET: f64 = 3.0;
/// Metres a pixel: the map zooms so the next turn is in view, within these.
const CLOSEST: f64 = 0.5;
const FARTHEST: f64 = 25.0;

/// The turns a maps app gives (only the demo's comes from the Mac so far).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    Straight,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    /// A U-turn.
    Around,
    Roundabout,
    Arrive,
}

impl Turn {
    /// Its arrow.
    pub fn icon(self) -> Icon {
        match self {
            Turn::Straight => Icon::Straight,
            Turn::SlightLeft => Icon::SlightLeft,
            Turn::Left => Icon::Left,
            Turn::SharpLeft => Icon::SharpLeft,
            Turn::SlightRight => Icon::SlightRight,
            Turn::Right => Icon::Right,
            Turn::SharpRight => Icon::SharpRight,
            Turn::Around => Icon::UTurn,
            Turn::Roundabout => Icon::Roundabout,
            Turn::Arrive => Icon::Arrive,
        }
    }
}

/// A route being followed, from where the head is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Route {
    /// The next turn, how far it is (metres) and the street it turns into.
    pub turn: Turn,
    pub distance: f64,
    pub street: String,
    /// Which way the head is going, degrees clockwise from north.
    pub heading: f64,
    /// The route ahead from the position, and the roads near: metres east and north of the position.
    pub path: Vec<(f64, f64)>,
    pub roads: Vec<Vec<(f64, f64)>>,
}

/// "180 m", "1.2 km", "500 ft", "0.3 mi": at most `DISTANCE_COLS` characters.
pub(crate) fn distance(metres: f64, metric: bool) -> String {
    let m = metres.max(0.0);
    if metric {
        match m {
            m if m < 995.0 => format!("{} m", ((m / 10.0).round() * 10.0) as u32),
            m if m < 9_950.0 => format!("{:.1} km", m / 1000.0),
            m => format!("{} km", ((m / 1000.0).round() as u32).min(9999)),
        }
    } else {
        let feet = m / 0.3048;
        match feet {
            f if f < 995.0 => format!("{} ft", ((f / 10.0).round() * 10.0) as u32),
            _ => match m / 1609.344 {
                mi if mi < 9.95 => format!("{mi:.1} mi"),
                mi => format!("{} mi", (mi.round() as u32).min(9999)),
            },
        }
    }
}

/// A map's lines in its box's pixels: the route's points and the roads', `BREAK` between lines, each cut to fit.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Lines {
    pub route: Vec<[u16; 2]>,
    pub roads: Vec<[u16; 2]>,
}

/// The route on a map of `view`, heading up, zoomed so the next turn shows.
pub(crate) fn project(route: &Route, view: &MapView) -> Lines {
    let (ox, oy) = (view.origin.0 as f64, view.origin.1 as f64);
    // The pixels above the position, and the zoom that puts the turn three quarters of the way up them.
    let above = match view.circle {
        Some((_, cy, r)) => oy - (cy - r) as f64,
        None => oy,
    };
    let mpp = (route.distance / (0.75 * above)).clamp(CLOSEST, FARTHEST);
    let (sin, cos) = route.heading.to_radians().sin_cos();
    let to_px = |&(e, n): &(f64, f64)| {
        let ahead = e * sin + n * cos;
        let right = e * cos - n * sin;
        (ox + right / mpp, oy - ahead / mpp)
    };
    let mut lines = Lines::default();
    let path: Vec<(f64, f64)> = route.path.iter().map(to_px).collect();
    put(&mut lines.route, &clip(&path, view), MAX_ROUTE);
    for road in &route.roads {
        let road: Vec<(f64, f64)> = road.iter().map(to_px).collect();
        put(&mut lines.roads, &clip(&road, view), MAX_ROADS);
    }
    lines
}

/// Lines into `out` as points with `BREAK` between, as many whole lines as fit in `max` points.
fn put(out: &mut Vec<[u16; 2]>, lines: &[Vec<(f64, f64)>], max: usize) {
    for line in lines {
        let mut points: Vec<[u16; 2]> = Vec::new();
        for &(x, y) in line {
            let p = [x.round() as u16, y.round() as u16];
            if points.last() != Some(&p) {
                points.push(p);
            }
        }
        if points.len() < 2 {
            continue;
        }
        let need = points.len() + !out.is_empty() as usize;
        if out.len() + need > max {
            return;
        }
        if !out.is_empty() {
            out.push(BREAK);
        }
        out.extend(points);
    }
}

/// The parts of a line inside the view (less the inset): a line for each stretch inside.
fn clip(line: &[(f64, f64)], view: &MapView) -> Vec<Vec<(f64, f64)>> {
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut open = false;
    for pair in line.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let Some((t0, t1)) = inside(a, b, view) else {
            open = false;
            continue;
        };
        let at = |t: f64| (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
        if !open || t0 > 0.0 {
            out.push(vec![at(t0)]);
        }
        out.last_mut().unwrap().push(at(t1));
        open = t1 >= 1.0;
    }
    out
}

/// The stretch of a to b inside the view, as parameters along it.
fn inside(a: (f64, f64), b: (f64, f64), view: &MapView) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    match view.circle {
        Some((cx, cy, r)) => {
            let (fx, fy, r) = (a.0 - cx as f64, a.1 - cy as f64, r as f64 - INSET);
            let (qa, qb, qc) = (dx * dx + dy * dy, 2.0 * (fx * dx + fy * dy), fx * fx + fy * fy - r * r);
            if qa == 0.0 {
                return (qc <= 0.0).then_some((0.0, 1.0));
            }
            let disc = qb * qb - 4.0 * qa * qc;
            if disc < 0.0 {
                return None;
            }
            let root = disc.sqrt();
            t0 = t0.max((-qb - root) / (2.0 * qa));
            t1 = t1.min((-qb + root) / (2.0 * qa));
        }
        None => {
            // Liang–Barsky, against each edge.
            let (x0, y0) = (INSET, INSET);
            let (x1, y1) = (view.w as f64 - 1.0 - INSET, view.h as f64 - 1.0 - INSET);
            for (p, q) in [(-dx, a.0 - x0), (dx, x1 - a.0), (-dy, a.1 - y0), (dy, y1 - a.1)] {
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
        }
    }
    (t0 < t1).then_some((t0, t1))
}

/// A route for previews and the demo: north up a street for `walked` metres of 180, then right into Harbour St; a
/// grid of streets and an avenue across them.
pub(crate) fn sample(walked: f64) -> Route {
    let walked = walked.clamp(0.0, 180.0);
    let at = |e: f64, n: f64| (e, n - walked);
    let mut roads = Vec::new();
    for e in [-160.0, 0.0, 120.0, 280.0] {
        roads.push(vec![at(e, -400.0), at(e, 900.0)]);
    }
    for n in [-90.0, 180.0, 340.0, 520.0] {
        roads.push(vec![at(-500.0, n), at(600.0, n)]);
    }
    roads.push(vec![at(-500.0, -300.0), at(-60.0, 120.0), at(600.0, 760.0)]);
    Route {
        turn: Turn::Right,
        distance: 180.0 - walked,
        street: "Harbour St".into(),
        heading: 0.0,
        path: vec![at(0.0, walked), at(0.0, 180.0), at(120.0, 180.0), at(120.0, 340.0), at(280.0, 340.0)],
        roads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_distances() {
        assert_eq!(distance(183.0, true), "180 m");
        assert_eq!(distance(1240.0, true), "1.2 km");
        assert_eq!(distance(25_000.0, true), "25 km");
        assert_eq!(distance(152.4, false), "500 ft");
        assert_eq!(distance(800.0, false), "0.5 mi");
        assert!(distance(9e9, true).len() <= super::super::layout::DISTANCE_COLS);
    }

    #[test]
    fn projects_heading_up_and_clips() {
        let view = MapView { w: 196, h: 196, origin: (98, 130), circle: None };
        let lines = project(&sample(0.0), &view);
        // The route starts at the position and goes straight up; the turn, 180 m on, three quarters up.
        assert_eq!(lines.route[0], [98, 130]);
        assert_eq!(lines.route[1][0], 98);
        assert!((30..36).contains(&lines.route[1][1]), "{:?}", lines.route[1]);
        for &p in lines.route.iter().chain(&lines.roads).filter(|&&p| p != BREAK) {
            assert!((3..=192).contains(&p[0]) && (3..=192).contains(&p[1]), "{p:?} inside");
        }
        assert!(lines.roads.len() <= MAX_ROADS && lines.roads.contains(&BREAK));
        // Facing east, the route goes left: north is on the left.
        let east = Route { heading: 90.0, ..sample(0.0) };
        let lines = project(&east, &view);
        assert!(lines.route[1][0] < 98 && lines.route[1][1] == 130);
        // A round map keeps everything in its circle.
        let round = MapView { w: 196, h: 196, origin: (98, 130), circle: Some((98, 98, 97)) };
        for &p in project(&sample(60.0), &round).roads.iter().filter(|&&p| p != BREAK) {
            let (dx, dy) = (p[0] as f64 - 98.0, p[1] as f64 - 98.0);
            // 94 px out at most, and rounding.
            assert!((dx * dx + dy * dy).sqrt() <= 95.0, "{p:?}");
        }
    }
}
