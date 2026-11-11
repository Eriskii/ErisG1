//! The shell's icons: weather, kinds of notification, turns, and a few for tiles (a drop, footprints, a laptop, a bolt,
//! a heart, the position on a map). Each
//! is a few shapes on a unit square, drawn here at whatever size a tile needs as 4-bit coverage, so edges are smooth
//! on the panel: the arm `BLIT`s one in `MASK` mode at the tile's level.

/// A group of icons a tile picks from by index (the Mac sends the index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum IconSet {
    Weather,
    Category,
    Droplet,
    Steps,
    Laptop,
    Bolt,
    Heart,
    Position,
    Turn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Sun,
    Moon,
    SunCloud,
    MoonCloud,
    Cloud,
    Fog,
    Rain,
    Snow,
    Storm,
    App,
    Message,
    Mail,
    Calendar,
    Call,
    Music,
    Alert,
    Bell,
    Droplet,
    Steps,
    Laptop,
    Bolt,
    Heart,
    Position,
    Straight,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    UTurn,
    Roundabout,
    Arrive,
}

use Icon::*;

pub(crate) const WEATHER: [Icon; 9] = [Sun, Moon, SunCloud, MoonCloud, Cloud, Fog, Rain, Snow, Storm];
pub(crate) const CATEGORY: [Icon; 8] = [App, Message, Mail, Calendar, Call, Music, Alert, Bell];
#[cfg(test)]
const TURNS: [Icon; 10] =
    [Straight, SlightLeft, Left, SharpLeft, SlightRight, Right, SharpRight, UTurn, Roundabout, Arrive];
/// "No icon", as an index.
pub(crate) const NONE: u8 = 0xFF;

impl IconSet {
    pub fn icons(self) -> &'static [Icon] {
        match self {
            IconSet::Weather => &WEATHER,
            IconSet::Category => &CATEGORY,
            IconSet::Droplet => &[Droplet],
            IconSet::Steps => &[Steps],
            IconSet::Laptop => &[Laptop],
            IconSet::Bolt => &[Bolt],
            IconSet::Heart => &[Heart],
            IconSet::Position => &[Position],
            // One arrow: the host draws the next turn's into it (all ten would crowd out the fonts).
            IconSet::Turn => &[Straight],
        }
    }

    /// Bytes of the whole set at `size` px.
    pub fn bytes(self, size: u32) -> u32 {
        self.icons().len() as u32 * size * size / 2
    }

    pub fn render(self, size: u32) -> Vec<u8> {
        self.icons().iter().flat_map(|&icon| render(icon, size)).collect()
    }
}

/// The weather icon for a WMO weather code (as open-meteo gives it), by day or night.
pub(crate) fn weather(code: u32, day: bool) -> u8 {
    let icon = match code {
        0 | 1 if day => Sun,
        0 | 1 => Moon,
        2 if day => SunCloud,
        2 => MoonCloud,
        45 | 48 => Fog,
        51..=67 | 80..=82 => Rain,
        71..=77 | 85 | 86 => Snow,
        95..=99 => Storm,
        _ => Cloud,
    };
    WEATHER.iter().position(|&i| i == icon).unwrap() as u8
}

/// The kind of notification an app sends, by its bundle ID; `None` for the Mac's own (`erisg1 shell say`).
pub(crate) fn category(app: Option<&str>) -> u8 {
    let Some(app) = app else { return CATEGORY.iter().position(|&i| i == Bell).unwrap() as u8 };
    let app = app.to_ascii_lowercase();
    let any = |names: &[&str]| names.iter().any(|n| app.contains(n));
    let icon = if any(&[
        "mobilesms",
        "imessage",
        "signal",
        "whatsapp",
        "telegram",
        "discord",
        "vesktop",
        "slack",
        "messenger",
        "teams",
        "element",
        "beeper",
        "wechat",
        "line.",
    ]) {
        Message
    } else if any(&["mail", "outlook", "spark", "superhuman", "airmail", "fastmail"]) {
        Mail
    } else if any(&["ical", "calendar", "fantastical", "reminders", "busycal"]) {
        Calendar
    } else if any(&["facetime", "zoom", "webex", "skype", "phone"]) {
        Call
    } else if any(&["music", "spotify", "podcasts", "tidal", "soundcloud"]) {
        Music
    } else if any(&["security", "alert", "warning", "backup"]) {
        Alert
    } else {
        App
    };
    CATEGORY.iter().position(|&i| i == icon).unwrap() as u8
}

/// The category's name, for `erisg1 shell say --icon NAME`.
pub(crate) fn category_named(name: &str) -> Option<u8> {
    let icon = match name {
        "app" => App,
        "message" => Message,
        "mail" => Mail,
        "calendar" => Calendar,
        "call" => Call,
        "music" => Music,
        "alert" => Alert,
        "bell" => Bell,
        _ => return None,
    };
    CATEGORY.iter().position(|&i| i == icon).map(|i| i as u8)
}

pub(crate) const CATEGORY_NAMES: &str = "app, message, mail, calendar, call, music, alert, bell";

// Shapes on the unit square, y down.

fn disk(x: f64, y: f64, cx: f64, cy: f64, r: f64) -> bool {
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn ellipse(x: f64, y: f64, cx: f64, cy: f64, rx: f64, ry: f64) -> bool {
    ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
}

fn ring(x: f64, y: f64, cx: f64, cy: f64, r: f64, w: f64) -> bool {
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    d <= r && d >= r - w
}

/// A stroke `w` wide from a to b, round at the ends.
fn seg(x: f64, y: f64, a: (f64, f64), b: (f64, f64), w: f64) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (x - a.0 - t * dx).powi(2) + (y - a.1 - t * dy).powi(2) <= w * w / 4.0
}

fn rect(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64) -> bool {
    x >= x0 && x <= x1 && y >= y0 && y <= y1
}

fn rounded(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> bool {
    let (cx, cy) = (x.clamp(x0 + r, x1 - r), y.clamp(y0 + r, y1 - r));
    rect(x, y, x0, y0, x1, y1) && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

#[allow(clippy::too_many_arguments)]
fn outline(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64, r: f64, w: f64) -> bool {
    rounded(x, y, x0, y0, x1, y1, r) && !rounded(x, y, x0 + w, y0 + w, x1 - w, y1 - w, (r - w).max(0.0))
}

fn triangle(x: f64, y: f64, a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> bool {
    let side = |p: (f64, f64), q: (f64, f64)| (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0);
    let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// A cloud in the square at `ox`, `oy`, `s` across.
fn cloud(x: f64, y: f64, ox: f64, oy: f64, s: f64) -> bool {
    let (x, y) = ((x - ox) / s, (y - oy) / s);
    disk(x, y, 0.28, 0.6, 0.17)
        || disk(x, y, 0.5, 0.46, 0.24)
        || disk(x, y, 0.73, 0.59, 0.18)
        || rect(x, y, 0.28, 0.6, 0.73, 0.77)
}

fn sun(x: f64, y: f64, cx: f64, cy: f64, s: f64) -> bool {
    if disk(x, y, cx, cy, 0.2 * s) {
        return true;
    }
    (0..8).any(|k| {
        let a = k as f64 * std::f64::consts::FRAC_PI_4;
        let (c, n) = (a.cos(), a.sin());
        seg(x, y, (cx + 0.3 * s * c, cy + 0.3 * s * n), (cx + 0.44 * s * c, cy + 0.44 * s * n), 0.08 * s)
    })
}

fn moon(x: f64, y: f64, cx: f64, cy: f64, s: f64) -> bool {
    disk(x, y, cx, cy, 0.36 * s) && !disk(x, y, cx + 0.17 * s, cy - 0.13 * s, 0.3 * s)
}

fn bolt(x: f64, y: f64, ox: f64, oy: f64, s: f64) -> bool {
    let p = |a: f64, b: f64| (ox + a * s, oy + b * s);
    triangle(x, y, p(0.62, 0.0), p(0.22, 0.56), p(0.52, 0.56))
        || triangle(x, y, p(0.38, 1.0), p(0.78, 0.44), p(0.48, 0.44))
        || rect(x, y, ox + 0.4 * s, oy + 0.44 * s, ox + 0.6 * s, oy + 0.56 * s)
}

/// A thick arrow along `path`, its head past the last point the way the last stretch goes.
fn arrow(x: f64, y: f64, path: &[(f64, f64)]) -> bool {
    const W: f64 = 0.16;
    if path.windows(2).any(|p| seg(x, y, p[0], p[1], W)) {
        return true;
    }
    let ((ax, ay), (bx, by)) = (path[path.len() - 2], path[path.len() - 1]);
    let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
    let (dx, dy) = ((bx - ax) / len, (by - ay) / len);
    let tip = (bx + 0.24 * dx, by + 0.24 * dy);
    let (left, right) = ((bx - 0.22 * dy, by + 0.22 * dx), (bx + 0.22 * dy, by - 0.22 * dx));
    triangle(x, y, tip, left, right)
}

/// A turn to the right (`r`), or the same turn mirrored to the left.
fn turn(x: f64, y: f64, r: bool, path: &[(f64, f64)]) -> bool {
    let x = if r { x } else { 1.0 - x };
    arrow(x, y, path)
}

fn inside(icon: Icon, x: f64, y: f64) -> bool {
    match icon {
        Sun => sun(x, y, 0.5, 0.5, 1.0),
        Moon => moon(x, y, 0.48, 0.52, 1.1),
        SunCloud | MoonCloud => {
            let (ox, oy, s) = (0.12, 0.14, 0.88);
            if cloud(x, y, ox, oy, s) {
                return true;
            }
            // The sun or moon behind it, kept clear of its edge.
            let halo = cloud(x, y, ox - 0.05, oy - 0.05, s + 0.1);
            let behind = if icon == SunCloud { sun(x, y, 0.36, 0.36, 0.72) } else { moon(x, y, 0.38, 0.38, 0.75) };
            behind && !halo
        }
        Cloud => cloud(x, y, 0.0, 0.04, 1.0),
        Fog => {
            seg(x, y, (0.18, 0.3), (0.82, 0.3), 0.1)
                || seg(x, y, (0.12, 0.5), (0.74, 0.5), 0.1)
                || seg(x, y, (0.26, 0.7), (0.88, 0.7), 0.1)
        }
        Rain => {
            cloud(x, y, 0.0, -0.14, 1.0)
                || [0.33, 0.52, 0.71].iter().any(|&cx| seg(x, y, (cx, 0.7), (cx - 0.07, 0.9), 0.08))
        }
        Snow => cloud(x, y, 0.0, -0.14, 1.0) || [0.32, 0.52, 0.72].iter().any(|&cx| disk(x, y, cx, 0.8, 0.065)),
        Storm => cloud(x, y, 0.0, -0.16, 1.0) || bolt(x, y, 0.3, 0.6, 0.4),
        App => outline(x, y, 0.16, 0.16, 0.84, 0.84, 0.16, 0.09) || disk(x, y, 0.5, 0.5, 0.11),
        Message => {
            let bubble =
                rounded(x, y, 0.1, 0.16, 0.9, 0.68, 0.14) || triangle(x, y, (0.26, 0.6), (0.46, 0.6), (0.22, 0.88));
            bubble && ![0.32, 0.5, 0.68].iter().any(|&cx| disk(x, y, cx, 0.42, 0.06))
        }
        Mail => {
            outline(x, y, 0.1, 0.22, 0.9, 0.78, 0.05, 0.08)
                || (seg(x, y, (0.12, 0.26), (0.5, 0.54), 0.08) || seg(x, y, (0.88, 0.26), (0.5, 0.54), 0.08))
        }
        Calendar => {
            outline(x, y, 0.14, 0.18, 0.86, 0.88, 0.08, 0.08)
                || rect(x, y, 0.14, 0.18, 0.86, 0.38)
                || [0.3, 0.5, 0.7].iter().any(|&cx| disk(x, y, cx, 0.54, 0.05) || disk(x, y, cx, 0.72, 0.05))
        }
        Call => {
            (ring(x, y, 0.72, 0.28, 0.5, 0.15) && x <= 0.72 && y >= 0.28)
                || ellipse(x, y, 0.27, 0.3, 0.1, 0.13)
                || ellipse(x, y, 0.7, 0.73, 0.13, 0.1)
        }
        Music => {
            ellipse(x, y, 0.35, 0.74, 0.15, 0.11)
                || seg(x, y, (0.47, 0.74), (0.47, 0.14), 0.08)
                || seg(x, y, (0.47, 0.16), (0.74, 0.34), 0.1)
        }
        Alert => {
            let mark = seg(x, y, (0.5, 0.38), (0.5, 0.6), 0.09) || disk(x, y, 0.5, 0.73, 0.055);
            triangle(x, y, (0.5, 0.1), (0.94, 0.86), (0.06, 0.86)) && !mark
        }
        Bell => {
            (disk(x, y, 0.5, 0.46, 0.26) && y <= 0.46)
                || rect(x, y, 0.24, 0.46, 0.76, 0.7)
                || rounded(x, y, 0.13, 0.66, 0.87, 0.76, 0.04)
                || disk(x, y, 0.5, 0.84, 0.08)
                || disk(x, y, 0.5, 0.18, 0.06)
        }
        Droplet => disk(x, y, 0.5, 0.63, 0.25) || triangle(x, y, (0.5, 0.1), (0.27, 0.56), (0.73, 0.56)),
        Steps => {
            ellipse(x, y, 0.33, 0.42, 0.11, 0.17)
                || ellipse(x, y, 0.33, 0.7, 0.08, 0.07)
                || ellipse(x, y, 0.67, 0.27, 0.11, 0.17)
                || ellipse(x, y, 0.67, 0.55, 0.08, 0.07)
        }
        Laptop => outline(x, y, 0.2, 0.2, 0.8, 0.64, 0.04, 0.08) || rounded(x, y, 0.06, 0.7, 0.94, 0.8, 0.03),
        Bolt => bolt(x, y, 0.1, 0.06, 0.88),
        Heart => {
            disk(x, y, 0.32, 0.36, 0.2)
                || disk(x, y, 0.68, 0.36, 0.2)
                || triangle(x, y, (0.13, 0.42), (0.87, 0.42), (0.5, 0.86))
        }
        // An arrowhead pointing up, notched at the back.
        Position => {
            triangle(x, y, (0.5, 0.06), (0.9, 0.92), (0.5, 0.7)) || triangle(x, y, (0.5, 0.06), (0.5, 0.7), (0.1, 0.92))
        }
        Straight => arrow(x, y, &[(0.5, 0.96), (0.5, 0.3)]),
        SlightLeft | SlightRight => turn(x, y, icon == SlightRight, &[(0.38, 0.96), (0.38, 0.58), (0.62, 0.32)]),
        Left | Right => turn(x, y, icon == Right, &[(0.3, 0.96), (0.3, 0.46), (0.64, 0.46)]),
        SharpLeft | SharpRight => turn(x, y, icon == SharpRight, &[(0.3, 0.96), (0.3, 0.2), (0.56, 0.5)]),
        UTurn => {
            (ring(x, y, 0.5, 0.4, 0.23, 0.14) && y <= 0.4)
                || seg(x, y, (0.66, 0.94), (0.66, 0.4), 0.14)
                || arrow(x, y, &[(0.34, 0.4), (0.34, 0.6)])
        }
        Roundabout => {
            ring(x, y, 0.5, 0.44, 0.22, 0.11)
                || seg(x, y, (0.5, 0.94), (0.5, 0.66), 0.14)
                || arrow(x, y, &[(0.64, 0.3), (0.72, 0.22)])
        }
        Arrive => {
            (disk(x, y, 0.5, 0.36, 0.28) && !disk(x, y, 0.5, 0.36, 0.11))
                || triangle(x, y, (0.25, 0.48), (0.75, 0.48), (0.5, 0.95))
        }
    }
}

/// `icon` at `size` px (even): 4-bit coverage from 4 × 4 samples a pixel, two pixels a byte (left high), `size / 2`
/// bytes a row.
pub(crate) fn render(icon: Icon, size: u32) -> Vec<u8> {
    assert!(size >= 2 && size.is_multiple_of(2), "icons are an even number of pixels across");
    let mut out = vec![0u8; (size * size / 2) as usize];
    let n = size as f64;
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let (x, y) = ((px as f64 + (sx as f64 + 0.5) / 4.0) / n, (py as f64 + (sy as f64 + 0.5) / 4.0) / n);
                    hits += inside(icon, x, y) as u32;
                }
            }
            let level = ((hits * 15 + 8) / 16) as u8;
            let at = (py * size / 2 + px / 2) as usize;
            out[at] |= if px % 2 == 0 { level << 4 } else { level };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_ink_and_room() {
        for set in [
            IconSet::Weather,
            IconSet::Category,
            IconSet::Droplet,
            IconSet::Steps,
            IconSet::Laptop,
            IconSet::Bolt,
            IconSet::Heart,
            IconSet::Position,
            IconSet::Turn,
        ] {
            for &icon in set.icons() {
                let px = render(icon, 24);
                let lit = px.iter().map(|b| ((b >> 4) > 7) as u32 + ((b & 15) > 7) as u32).sum::<u32>();
                // Between a tenth and two thirds of the square.
                assert!((57..384).contains(&lit), "{icon:?}: {lit} of 576 pixels lit");
            }
            assert_eq!(set.render(16).len() as u32, set.bytes(16));
        }
        for icon in TURNS {
            let lit = render(icon, 24).iter().map(|b| ((b >> 4) > 7) as u32 + ((b & 15) > 7) as u32).sum::<u32>();
            assert!((57..384).contains(&lit), "{icon:?}: {lit} of 576 pixels lit");
        }
    }

    #[test]
    fn picks_icons() {
        assert_eq!(WEATHER[weather(0, true) as usize], Sun);
        assert_eq!(WEATHER[weather(1, false) as usize], Moon);
        assert_eq!(WEATHER[weather(63, true) as usize], Rain);
        assert_eq!(WEATHER[weather(95, false) as usize], Storm);
        assert_eq!(CATEGORY[category(Some("org.whispersystems.signal-desktop")) as usize], Message);
        assert_eq!(CATEGORY[category(Some("com.apple.mail")) as usize], Mail);
        assert_eq!(CATEGORY[category(Some("us.zoom.xos")) as usize], Call);
        assert_eq!(CATEGORY[category(Some("com.example.thing")) as usize], App);
        assert_eq!(CATEGORY[category(None) as usize], Bell);
        assert_eq!(category_named("mail"), Some(2));
    }
}
