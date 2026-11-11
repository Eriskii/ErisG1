//! A layout: the shell's tiles, highest priority first. Each tile is a rectangle of the panel with a kind (what it
//! shows) and is on or off. A tile that is on shows when no tile above it that shows overlaps it, and then it has its
//! whole rectangle: every tile below that overlaps it, by even a pixel, hides until it goes.
//!
//! The layout is JSON (`~/.config/erisg1/shell.json`), written by the editor.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::fonts;
pub(crate) use super::fonts::{Face, Metrics};
use super::icons::IconSet;
use crate::apps::vmkit;
use crate::proto::Side;
use crate::v0_cube::{EYES, FOCAL};
use crate::v5;

pub(crate) const MAX_TILES: usize = 24;
/// The fonts and icons sit after the frame, up to the program's constants (`vmkit::POOL`).
pub(crate) const FONTS: u32 = v5::FONT;
pub(crate) const FONTS_END: u32 = 0x30000;
/// More go at the top of the program's code area, down from its end, as far as the code leaves room; it takes at
/// least this much.
const SPARE_END: u32 = vmkit::TABLE;
const SPARE_START: u32 = vmkit::CODE + 0x4000;
/// Days of dates a date tile holds, from the day the Mac last sent them.
pub(crate) const DATES: usize = 32;
/// The longest line a text tile takes, in characters.
const MAX_COLS: usize = 80;
const DEFAULT_DATE: &str = "%a %d %b";
/// How far past a tilt threshold the head comes back before the scene changes back, degrees.
pub(crate) const TILT_HYSTERESIS: i32 = 6;
/// Navigation: the turn arrow's largest size, pixels (it sits with the fonts); the longest distance, "1.2 km".
const MAX_TURN: i32 = 64;
pub(crate) const DISTANCE_COLS: usize = 7;
/// Notification: the room kept at the right of its first line for how many more wait, "+12".
pub(crate) const BADGE_COLS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// The time, kept by the glasses.
    Clock,
    /// The date, from a table the Mac sends.
    Date,
    /// Both arms' battery, averaged or side by side.
    Battery,
    /// A title and a few lines from the Mac, or one line; a tap hides it, a double tap brings it back.
    Notification,
    /// The Mac's notifications, an icon and a line each, as many as fit.
    NotificationList,
    /// Lines of text from the Mac.
    Text,
    /// An icon, the temperature and the humidity, from the Mac.
    Weather,
    /// A step count from the host (for now a placeholder).
    Steps,
    /// The Mac's own battery.
    DeviceBattery,
    /// Beats a minute from the host (for now a placeholder).
    HeartRate,
    /// Directions while a route is followed: the next turn when the tile is wide (twice as wide as high or more),
    /// else a map around the position, heading up.
    Navigation,
}

/// How a tile comes and goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Transition {
    /// At once.
    None,
    /// In from the top of its rectangle, out of the bottom.
    Slide,
    /// Brighter and brighter, and dimmer and dimmer.
    Fade,
}

/// Notification: what's above its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Header {
    /// The icon and the title on a line of their own, a rule under them.
    #[default]
    Title,
    /// No title line: the icon before the text, "title: text".
    Icon,
    /// Only "title: text".
    None,
}

/// Navigation: the map's shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Shape {
    #[default]
    Square,
    Round,
}

/// Which way the head is tilted: each tile shows in some of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Scene {
    Level,
    Down,
    Up,
}

pub(crate) const SCENES: [Scene; 3] = [Scene::Level, Scene::Down, Scene::Up];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Weather: the temperature's unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Unit {
    /// The Mac's own setting.
    #[default]
    Auto,
    C,
    F,
}

fn yes() -> bool {
    true
}

fn full() -> u8 {
    15
}

fn boost() -> u8 {
    5
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Tile {
    pub name: String,
    pub kind: Kind,
    /// x, y, width, height on the panel.
    pub rect: [i32; 4],
    /// On from the start (until something turns it off).
    #[serde(default)]
    pub on: bool,
    /// The scenes it shows in; missing, its kind's (a notification all, the rest level).
    #[serde(default)]
    pub scenes: Option<Vec<Scene>>,
    /// Text size: the 8 × 8 font this many times over, or 0 for the largest that fits (a notification's body:
    /// 2, its title one more).
    #[serde(default)]
    pub size: u8,
    #[serde(default)]
    pub align: Align,
    /// How bright its text is, 1–15.
    #[serde(default = "full")]
    pub level: u8,
    /// A thin frame around it.
    #[serde(default)]
    pub border: bool,
    /// A tap on either TouchBar turns it off (the top one that shows first); a double tap brings it back.
    #[serde(default)]
    pub dismiss: bool,
    /// Seconds it stays on after something is sent to it (0: until it's turned off).
    #[serde(default)]
    pub timeout: f64,
    /// An icon: a notification's kind, the weather, a pair of footprints, the Mac.
    #[serde(default = "yes")]
    pub icon: bool,
    /// Clock: with seconds.
    #[serde(default = "yes")]
    pub seconds: bool,
    /// Clock: 24-hour rather than AM/PM.
    #[serde(default)]
    pub h24: bool,
    /// Date: a strftime format ("%a %d %b" when empty).
    #[serde(default)]
    pub format: String,
    /// Battery: each arm's rather than their average.
    #[serde(default)]
    pub both: bool,
    /// Notification: the icon and "title: text" on one line.
    #[serde(default)]
    pub single_line: bool,
    /// Notification: the Mac's own new notifications pop up in it.
    #[serde(default = "yes")]
    pub mirror: bool,
    /// Notification: what's above its text, and the title's size (0: one size up from the text).
    #[serde(default)]
    pub header: Header,
    #[serde(default)]
    pub title_size: u8,
    /// Weather: the temperature, the humidity, the unit.
    #[serde(default = "yes")]
    pub temp: bool,
    #[serde(default = "yes")]
    pub humidity: bool,
    #[serde(default)]
    pub unit: Unit,
    /// Steps, heart rate: the unit after the number.
    #[serde(default = "yes")]
    pub label: bool,
    /// Navigation: the map's shape.
    #[serde(default)]
    pub shape: Shape,
    /// Text: what it shows from the start.
    #[serde(default)]
    pub text: String,
    /// How far away it looks, metres (0 as far as can be), as the layout's `depth`; none, the layout's.
    #[serde(default)]
    pub depth: Option<f64>,
    /// How it comes and goes; none, its kind's way (a notification slides).
    #[serde(default)]
    pub transition: Option<Transition>,
    /// What older layouts said instead: it slides, or not.
    #[serde(default, skip_serializing)]
    pub slide: Option<bool>,
    /// Its font (its icons and battery bars are a character high); `size` counts in its sizes.
    #[serde(default)]
    pub font: Face,
    /// The editor's preview shows it on, with sample content.
    #[serde(default)]
    pub preview: bool,
}

/// The sideways shift in the eye on `side` that puts something `depth` metres away (0: none), as `kit::Eye::depth`.
pub(crate) fn eye_shift(depth: f64, side: Side) -> i32 {
    if depth == 0.0 {
        return 0;
    }
    let disparity = FOCAL * EYES / depth;
    let left = (disparity / 2.0).round();
    match side {
        Side::Left => left as i32,
        Side::Right => -(disparity - left).round() as i32,
    }
}

/// A depth in range: 0, or 0.5 to 20 m.
fn depth_ok(depth: f64) -> bool {
    depth == 0.0 || (0.5..=20.0).contains(&depth)
}

fn rects_overlap([ax, ay, aw, ah]: [i32; 4], [bx, by, bw, bh]: [i32; 4]) -> bool {
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

/// What a one-line tile shows, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// An icon of a set: for a set of several the Mac picks which; the bolt shows only while charging.
    Icon(IconSet),
    /// Line `n` of the tile's buffer, so many characters wide.
    Line(usize, usize),
    /// Fixed text.
    Label(&'static str),
    /// A battery: an outline with a bar as full as the charge.
    Meter,
    /// The charge, "100%".
    Percent,
    /// Space, in eighths of a character.
    Gap(i32),
}

/// A part's width in a font of metrics `m`, pixels: an icon a character high and square, a battery two of them,
/// text so many characters, a gap so many eighths of the height.
pub(crate) fn part_width(part: Part, m: Metrics) -> i32 {
    match part {
        Part::Icon(_) => m.height(),
        Part::Line(_, chars) => m.advance * chars as i32,
        Part::Label(text) => m.advance * text.len() as i32,
        Part::Meter => 2 * m.height(),
        Part::Percent => 4 * m.advance,
        Part::Gap(n) => n * m.height() / 8,
    }
}

impl Tile {
    pub fn new(name: &str, kind: Kind, rect: [i32; 4]) -> Tile {
        let popup = kind == Kind::Notification;
        Tile {
            name: name.into(),
            kind,
            rect,
            on: !matches!(kind, Kind::Notification | Kind::Text | Kind::Navigation),
            scenes: Some(kind_scenes(kind)),
            size: 0,
            align: Align::Left,
            level: 15,
            border: popup,
            dismiss: popup,
            timeout: if popup { 8.0 } else { 0.0 },
            icon: true,
            seconds: true,
            h24: false,
            format: String::new(),
            both: false,
            single_line: false,
            mirror: true,
            header: Header::Title,
            title_size: 0,
            temp: true,
            humidity: true,
            unit: Unit::Auto,
            label: true,
            shape: Shape::Square,
            text: String::new(),
            depth: None,
            transition: None,
            slide: None,
            font: Face::Pixel,
            preview: false,
        }
    }

    /// How it comes and goes.
    pub fn transition(&self) -> Transition {
        let older = self.slide.map(|s| if s { Transition::Slide } else { Transition::None });
        let kind = if self.kind == Kind::Notification { Transition::Slide } else { Transition::None };
        self.transition.or(older).unwrap_or(kind)
    }

    /// The font it draws in.
    pub fn face(&self) -> Face {
        self.font
    }

    /// Whether its text at `scale` takes emoji: a tile that shows messages, in a font with them at that size.
    pub fn emoji(&self, scale: u8) -> bool {
        let messages = matches!(self.kind, Kind::Notification | Kind::NotificationList | Kind::Text | Kind::Navigation);
        messages && self.face().has_emoji(scale)
    }

    /// Its font's cell at `scale`.
    pub fn metrics(&self, scale: u8) -> Metrics {
        self.face().metrics(scale)
    }

    /// The scenes it shows in.
    pub fn scenes(&self) -> Vec<Scene> {
        self.scenes.clone().unwrap_or_else(|| kind_scenes(self.kind))
    }

    /// The box the content goes in: the rectangle less its margin.
    fn inner(&self) -> [i32; 4] {
        let m = if self.border { 6 } else { 2 };
        let [x, y, w, h] = self.rect;
        [x + m, y + m, (w - 2 * m).max(0), (h - 2 * m).max(0)]
    }

    pub fn date_format(&self) -> &str {
        if self.format.trim().is_empty() { DEFAULT_DATE } else { &self.format }
    }

    /// What a one-line tile shows, left to right: a clock, date or battery's text as one line.
    pub fn parts(&self) -> Vec<Part> {
        let mut parts = Vec::new();
        match self.kind {
            Kind::Clock => {
                let chars = match (self.h24, self.seconds) {
                    (true, true) => 8,
                    (true, false) => 5,
                    (false, true) => 11,
                    (false, false) => 8,
                };
                parts.push(Part::Line(0, chars));
            }
            // Every date of a year, the longest.
            Kind::Date => {
                let first = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
                let chars = (0..366)
                    .map(|d| format_date(self.date_format(), first + chrono::Days::new(d)).len())
                    .max()
                    .unwrap_or(1);
                parts.push(Part::Line(0, chars));
            }
            Kind::Battery if self.both => parts.push(Part::Line(0, 11)),
            Kind::Battery => parts.extend([Part::Meter, Part::Percent]),
            Kind::Weather => {
                if self.icon {
                    parts.push(Part::Icon(IconSet::Weather));
                    if self.temp || self.humidity {
                        parts.push(Part::Gap(4));
                    }
                }
                if self.temp {
                    parts.push(Part::Line(0, 4));
                }
                if self.temp && self.humidity {
                    parts.push(Part::Gap(8));
                }
                if self.humidity {
                    parts.extend([Part::Icon(IconSet::Droplet), Part::Gap(1), Part::Line(1, 4)]);
                }
            }
            Kind::Steps => {
                if self.icon {
                    parts.extend([Part::Icon(IconSet::Steps), Part::Gap(4)]);
                }
                parts.push(Part::Line(0, 6));
                if self.label {
                    parts.push(Part::Label(" steps"));
                }
            }
            Kind::DeviceBattery => {
                if self.icon {
                    parts.extend([Part::Icon(IconSet::Laptop), Part::Gap(4)]);
                }
                parts.extend([Part::Meter, Part::Percent, Part::Icon(IconSet::Bolt)]);
            }
            Kind::HeartRate => {
                if self.icon {
                    parts.extend([Part::Icon(IconSet::Heart), Part::Gap(4)]);
                }
                parts.push(Part::Line(0, 3));
                if self.label {
                    parts.push(Part::Label(" bpm"));
                }
            }
            Kind::Notification | Kind::NotificationList | Kind::Text | Kind::Navigation => {}
        }
        parts
    }

    /// Navigation: whether it's a map (else the next turn).
    pub fn is_map(&self) -> bool {
        let [_, _, w, h] = self.inner();
        w < 2 * h
    }

    /// Navigation as a map: where things go in its box.
    pub fn map_view(&self) -> MapView {
        let [_, _, w, h] = self.inner();
        match self.shape {
            Shape::Square => MapView { w, h, origin: (w / 2, h * 2 / 3), circle: None },
            Shape::Round => {
                let r = w.min(h) / 2 - 1;
                let (cx, cy) = (w / 2, h / 2);
                MapView { w, h, origin: (cx, cy + r / 3), circle: Some((cx, cy, r)) }
            }
        }
    }

    /// Where its text goes and how much it holds.
    pub fn plan(&self) -> Result<Plan, String> {
        let inner = self.inner();
        let [_, _, w, h] = inner;
        let name = &self.name;
        let face = self.face();
        let most = face.max_scale();
        let m = |s: u8| face.metrics(s);
        if self.size > most {
            return Err(format!("{name}: size {}: at most {most} in its font", self.size));
        }
        // The largest size up to `top` at which a line `width(s)` wide fits across, and a line high.
        let fit = |width: &dyn Fn(u8) -> i32, top: u8| (1..=top).rev().find(|&s| width(s) <= w && m(s).height() <= h);
        let advance = |s: u8| m(s).advance;
        // Characters in `room` px across at `s`; lines in `h` px (the last needs no gap after it).
        let cols = |room: i32, s: u8| ((room / m(s).advance).max(0) as usize).min(MAX_COLS);
        let rows = |h: i32, s: u8| ((h + m(s).gap()).max(0) / m(s).pitch) as usize;
        // An icon before each line: a character high, and the gap between lines after it.
        let indent = |s: u8| if self.icon { m(s).height() + m(s).gap() } else { 0 };
        let icon = |s: u8| if self.icon { m(s).height() as u32 } else { 0 };
        // Text at its auto size: the 8 × 8 font as big as fits up to `top`, Departure Mono at 11 px.
        let auto = |top: u8| if face == Face::Pixel { fit(&advance, top).unwrap_or(1) } else { 1 };
        let none =
            Plan { inner, scale: 0, title_scale: 0, title_cols: 0, cols: 0, rows: 0, icon: 0, width: 0, map: false };
        match self.kind {
            Kind::Clock
            | Kind::Date
            | Kind::Battery
            | Kind::Weather
            | Kind::Steps
            | Kind::DeviceBattery
            | Kind::HeartRate => {
                let parts = self.parts();
                let width = |s: u8| parts.iter().map(|&p| part_width(p, m(s))).sum::<i32>();
                if width(1) == 0 {
                    return Err(format!("{name}: shows nothing; turn something on"));
                }
                let scale = if self.size == 0 {
                    fit(&width, most).ok_or(format!(
                        "{name}: too small (at least {} × {} px inside)",
                        width(1),
                        m(1).height()
                    ))?
                } else if width(self.size) <= w && m(self.size).height() <= h {
                    self.size
                } else {
                    return Err(format!(
                        "{name}: at size {} it needs {} × {} px inside; it has {w} × {h}",
                        self.size,
                        width(self.size),
                        m(self.size).height()
                    ));
                };
                let cols = match parts.first() {
                    Some(Part::Line(_, chars)) => *chars,
                    _ => 0,
                };
                let has_icon = parts.iter().any(|p| matches!(p, Part::Icon(_)));
                Ok(Plan {
                    scale,
                    cols,
                    rows: 1,
                    icon: if has_icon { m(scale).height() as u32 } else { 0 },
                    width: width(scale),
                    ..none
                })
            }
            Kind::Text => {
                let scale = if self.size > 0 { self.size } else { auto(2) };
                let (cols, rows) = (cols(w, scale), rows(h, scale));
                if cols == 0 || rows == 0 {
                    return Err(format!("{name}: too small for one character at size {scale}"));
                }
                Ok(Plan { scale, cols, rows, ..none })
            }
            Kind::NotificationList => {
                let scale = if self.size > 0 { self.size } else { auto(2) };
                let (cols, rows) = (cols(w - indent(scale), scale), rows(h, scale));
                if cols < 4 || rows == 0 {
                    return Err(format!("{name}: too small for a line at size {scale}"));
                }
                Ok(Plan { scale, cols, rows, icon: icon(scale), ..none })
            }
            Kind::Notification if self.single_line => {
                let scale = if self.size > 0 { self.size } else { auto(3) };
                // The line less the counter's room.
                let cols = cols(w - indent(scale), scale).saturating_sub(BADGE_COLS);
                if cols < 4 || m(scale).height() > h {
                    return Err(format!("{name}: too small for a line at size {scale}"));
                }
                Ok(Plan { scale, cols, rows: 1, icon: icon(scale), ..none })
            }
            Kind::Notification => {
                let body = if self.size > 0 {
                    self.size
                } else if face == Face::Pixel {
                    2
                } else {
                    1
                };
                // No title line: the text from the top, after the icon (its first line's) or not.
                if self.header != Header::Title {
                    let with_icon = self.header == Header::Icon && self.icon;
                    let indent = if with_icon { m(body).height() + m(body).gap() } else { 0 };
                    // (The first line less the counter's room.)
                    let (cols, rows) = (cols(w - indent, body), rows(h, body));
                    if cols < 4 + BADGE_COLS || rows == 0 {
                        return Err(format!("{name}: too small for a line at size {body}"));
                    }
                    let icon = if with_icon { m(body).height() as u32 } else { 0 };
                    return Ok(Plan { scale: body, cols, rows, icon, ..none });
                }
                if self.title_size > most {
                    return Err(format!("{name}: title size {}: at most {most} in its font", self.title_size));
                }
                let title = if self.title_size > 0 { self.title_size } else { (body + 1).min(most) };
                let body_top = m(title).height() + m(title).gap() + 3;
                // The title less the icon's and the counter's room.
                let (cols, title_cols) = (cols(w, body), cols(w - indent(title), title).saturating_sub(BADGE_COLS));
                let rows = rows(h - body_top, body);
                if cols == 0 || title_cols == 0 || rows == 0 {
                    return Err(format!(
                        "{name}: too small for a title and a line at size {body} (at least {} × {} px inside)",
                        m(title).advance + indent(title),
                        body_top + m(body).height()
                    ));
                }
                Ok(Plan { scale: body, title_scale: title, title_cols, cols, rows, icon: icon(title), ..none })
            }
            // A map: the position's arrow an eighth of it.
            Kind::Navigation if self.is_map() => {
                if w.min(h) < 48 {
                    return Err(format!("{name}: a map needs 48 × 48 px inside; it has {w} × {h}"));
                }
                Ok(Plan { icon: (w.min(h) / 8).clamp(12, 32) as u32 & !1, map: true, ..none })
            }
            // The next turn: its arrow as high as the tile (up to 64 px), then the distance over the street (the
            // distance one size up), or the two on one line.
            Kind::Navigation => {
                let arrow = h.min(MAX_TURN) & !1;
                if arrow < 16 {
                    return Err(format!("{name}: the next turn needs 16 px of height inside; it has {h}"));
                }
                let room = w - arrow - (arrow / 6).max(4);
                let distance = |s: u8| (s + 1).min(most);
                let two = |s: u8| {
                    let d = distance(s);
                    m(d).height() + 3 + m(s).height() <= h
                        && m(d).advance * DISTANCE_COLS as i32 <= room
                        && 8 * m(s).advance <= room
                };
                let one = |s: u8| m(s).height() <= h && 16 * m(s).advance <= room;
                let s = self.size;
                let icon = arrow as u32;
                if (s > 0 && two(s)) || (s == 0 && two(1)) {
                    let s = if s > 0 { s } else { (1..most).rev().find(|&s| two(s)).unwrap_or(1) };
                    Ok(Plan {
                        scale: s,
                        title_scale: distance(s),
                        title_cols: DISTANCE_COLS,
                        cols: cols(room, s),
                        rows: 2,
                        icon,
                        ..none
                    })
                } else if (s > 0 && one(s)) || (s == 0 && one(1)) {
                    let s = if s > 0 { s } else { (1..=most.min(3)).rev().find(|&s| one(s)).unwrap() };
                    Ok(Plan { scale: s, cols: cols(room, s), rows: 1, icon, ..none })
                } else {
                    Err(format!("{name}: too small for the next turn at size {} ({w} × {h} inside)", self.size))
                }
            }
        }
    }

    /// The icon sets it draws, each at its size in pixels.
    fn icon_sets(&self, plan: &Plan) -> Vec<(IconSet, u32)> {
        match self.kind {
            Kind::Notification | Kind::NotificationList if plan.icon > 0 => vec![(IconSet::Category, plan.icon)],
            Kind::Notification | Kind::NotificationList => Vec::new(),
            Kind::Navigation if plan.map => vec![(IconSet::Position, plan.icon)],
            Kind::Navigation => vec![(IconSet::Turn, plan.icon)],
            _ => self
                .parts()
                .into_iter()
                .filter_map(|p| match p {
                    Part::Icon(set) => Some((set, plan.icon)),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// The scenes a kind shows in unless told otherwise: a notification all of them, so it pops up however the head is.
fn kind_scenes(kind: Kind) -> Vec<Scene> {
    if kind == Kind::Notification { SCENES.to_vec() } else { vec![Scene::Level] }
}

/// Where a tile's text goes: its sizes and how much it holds.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Plan {
    /// The content's box (x, y, w, h).
    pub inner: [i32; 4],
    pub scale: u8,
    /// Notification: the title's size and width in characters.
    pub title_scale: u8,
    pub title_cols: usize,
    /// Characters a line holds, and lines (one for a one-line tile, whose first part is its line).
    pub cols: usize,
    pub rows: usize,
    /// The icons' size, pixels (0: none).
    pub icon: u32,
    /// A one-line tile's width, pixels.
    pub width: i32,
    /// Navigation: a map (its `icon` the position's arrow), else the next turn (its `icon` the turn's arrow, `rows` 2
    /// for the distance over the street or 1 for both on a line).
    pub map: bool,
}

/// A map's box, relative to its tile's inner box: the position (where the head is, heading up) and, for a round map,
/// the circle (centre and radius).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MapView {
    pub w: i32,
    pub h: i32,
    pub origin: (i32, i32),
    pub circle: Option<(i32, i32, i32)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Layout {
    /// How far away everything looks, metres, by the gap between the eyes' copies; 0 for the same place in both
    /// eyes (as far as can be).
    #[serde(default)]
    pub depth: f64,
    /// Levels (of 42) added to Even's light-to-brightness table.
    #[serde(default = "boost")]
    pub boost: u8,
    /// Looking down further than this (degrees, below 0) shows the "down" scene; none, no such scene.
    #[serde(default)]
    pub tilt_down: Option<i32>,
    /// Looking up further than this (degrees, above 0) shows the "up" scene.
    #[serde(default)]
    pub tilt_up: Option<i32>,
    /// Strips of the panel kept clear, pixels: top, right, bottom, left. No tile reaches into them.
    #[serde(default)]
    pub margins: [i32; 4],
    /// Highest priority first.
    pub tiles: Vec<Tile>,
}

impl Default for Layout {
    /// Level: the clock, the date, the glasses' battery and the weather at the top, a line of text at the bottom
    /// that the next turn takes over while a route is followed, and a notification across the top that hides them
    /// while it's up. Looking down: the Mac's notifications, the heart rate, the steps and the Mac's battery. Looking
    /// up: a big clock and the weather.
    fn default() -> Layout {
        let mut note = Tile::new("notification", Kind::Notification, [16, 16, 608, 200]);
        note.preview = false;
        let mut clock = Tile::new("clock", Kind::Clock, [16, 16, 288, 40]);
        clock.size = 3;
        let date = Tile::new("date", Kind::Date, [16, 60, 288, 32]);
        let mut battery = Tile::new("battery", Kind::Battery, [456, 16, 168, 40]);
        battery.align = Align::Right;
        let mut weather = Tile::new("weather", Kind::Weather, [416, 60, 208, 36]);
        weather.align = Align::Right;
        let mut status = Tile::new("status", Kind::Text, [16, 412, 608, 52]);
        status.timeout = 0.0;
        let directions = Tile::new("directions", Kind::Navigation, [16, 404, 608, 64]);
        let mut list = Tile::new("notifications", Kind::NotificationList, [16, 16, 608, 340]);
        list.scenes = Some(vec![Scene::Down]);
        let mut heart = Tile::new("heart", Kind::HeartRate, [16, 364, 200, 40]);
        heart.scenes = Some(vec![Scene::Down]);
        heart.size = 2;
        let mut steps = Tile::new("steps", Kind::Steps, [16, 412, 300, 40]);
        steps.scenes = Some(vec![Scene::Down]);
        steps.size = 2;
        let mut mac = Tile::new("mac_battery", Kind::DeviceBattery, [380, 412, 244, 40]);
        mac.scenes = Some(vec![Scene::Down]);
        mac.align = Align::Right;
        mac.size = 2;
        let mut big_clock = Tile::new("big_clock", Kind::Clock, [96, 168, 448, 48]);
        big_clock.scenes = Some(vec![Scene::Up]);
        big_clock.align = Align::Center;
        big_clock.size = 4;
        let mut sky = Tile::new("sky", Kind::Weather, [96, 232, 448, 40]);
        sky.scenes = Some(vec![Scene::Up]);
        sky.align = Align::Center;
        sky.size = 3;
        Layout {
            depth: 0.0,
            boost: 5,
            tilt_down: Some(-25),
            tilt_up: Some(20),
            margins: [0; 4],
            tiles: vec![
                note, clock, date, battery, weather, directions, status, list, heart, steps, mac, big_clock, sky,
            ],
        }
    }
}

/// What the program needs from a checked layout: each tile's plan, which tiles overlap, the scenes, the fonts and
/// icons.
#[derive(Clone, Debug)]
pub(crate) struct Geometry {
    pub plans: Vec<Plan>,
    /// For each tile, the tiles above it (higher priority) that overlap it, as a mask.
    pub above: Vec<u32>,
    /// For each tile, every other tile that overlaps it.
    pub touch: Vec<u32>,
    /// Each scene's tiles (level, down, up), as masks.
    pub scenes: [u32; 3],
    /// The fonts in use, each at a size, whether with emoji, and where each goes in the arena.
    pub fonts: Vec<(Face, u8, bool, u32)>,
    /// The icon sets in use, each at a size, and where each goes.
    pub icons: Vec<((IconSet, u32), u32)>,
    /// Where the fonts and icons that didn't fit after the frame start, at the top of the code's area (`SPARE_END`
    /// for none): the program's code must end below.
    pub low: u32,
}

impl Geometry {
    /// What's wrong when the fonts and icons reach `end` (the code's end, or the least room it's left): which there
    /// are, and how much too much.
    pub fn too_big(&self, end: u32) -> String {
        let named: Vec<String> = self
            .fonts
            .iter()
            .map(|&(face, s, emoji, _)| {
                let name = if face == Face::Pixel { "8 × 8" } else { "Departure Mono" };
                format!("{name} at {} px{}", face.px(s), if emoji { " with emoji" } else { "" })
            })
            .collect();
        format!(
            "the fonts ({}) with the icons don't fit with the program: {} KB too much. Use fewer different sizes",
            named.join(", "),
            (end - self.low).div_ceil(1024)
        )
    }

    pub fn font(&self, face: Face, scale: u8) -> u32 {
        self.fonts.iter().find(|f| (f.0, f.1) == (face, scale)).expect("a font for every size in use").3
    }

    pub fn icons_at(&self, set: IconSet, size: u32) -> u32 {
        self.icons.iter().find(|i| i.0 == (set, size)).expect("every icon set in use").1
    }
}

impl Layout {
    /// Checks everything and works out the geometry, or says what's wrong.
    pub fn check(&self) -> Result<Geometry, String> {
        if self.tiles.len() > MAX_TILES {
            return Err(format!("{} tiles; at most {MAX_TILES}", self.tiles.len()));
        }
        if !depth_ok(self.depth) {
            return Err(format!("depth {} m: 0, or 0.5 to 20 m", self.depth));
        }
        if self.boost > 42 {
            return Err(format!("boost {}: at most 42", self.boost));
        }
        if self.tilt_down.is_some_and(|d| !(-80..=-5).contains(&d)) {
            return Err("looking down: between -80 and -5 degrees".into());
        }
        if self.tilt_up.is_some_and(|u| !(5..=80).contains(&u)) {
            return Err("looking up: between 5 and 80 degrees".into());
        }
        let [top, right, bottom, left] = self.margins;
        let (x0, y0, x1, y1) = (left, top, v5::WIDTH as i32 - right, v5::HEIGHT as i32 - bottom);
        if self.margins.iter().any(|&m| m < 0) || x1 - x0 < 64 || y1 - y0 < 64 {
            return Err("the cut-off strips leave less than 64 × 64 px".into());
        }
        let mut names = BTreeSet::new();
        let mut plans = Vec::new();
        for t in &self.tiles {
            if t.name.trim().is_empty() || t.name.len() > 32 || t.name.contains(char::is_whitespace) {
                return Err(format!("{:?}: a name is 1–32 characters without spaces", t.name));
            }
            if !names.insert(t.name.as_str()) {
                return Err(format!("two tiles named {}", t.name));
            }
            let [x, y, w, h] = t.rect;
            if x < 0 || y < 0 || w < 8 || h < 8 || x + w > v5::WIDTH as i32 || y + h > v5::HEIGHT as i32 {
                return Err(format!("{}: {x}, {y}, {w} × {h} isn't inside the 640 × 480 panel", t.name));
            }
            if x < x0 || y < y0 || x + w > x1 || y + h > y1 {
                return Err(format!(
                    "{}: {x}, {y}, {w} × {h} reaches into a cut-off strip (the usable area is {x0}, {y0} to {x1}, {y1})",
                    t.name
                ));
            }
            if t.depth.is_some_and(|d| !depth_ok(d)) {
                return Err(format!("{}: depth {} m: 0, or 0.5 to 20 m", t.name, t.depth.unwrap_or(0.0)));
            }
            if !(1..=15).contains(&t.level) {
                return Err(format!("{}: level {}: 1 to 15", t.name, t.level));
            }
            if !(0.0..=3600.0).contains(&t.timeout) {
                return Err(format!("{}: timeout {} s: 0 to 3600", t.name, t.timeout));
            }
            if t.kind == Kind::Date {
                use chrono::format::{Item, StrftimeItems};
                if StrftimeItems::new(t.date_format()).any(|i| matches!(i, Item::Error)) {
                    return Err(format!("{}: {:?} isn't a date format", t.name, t.format));
                }
            }
            plans.push(t.plan()?);
        }
        let n = self.tiles.len();
        let mut above = vec![0u32; n];
        let mut touch = vec![0u32; n];
        for i in 0..n {
            for j in 0..n {
                // Overlapping in either eye: tiles at different depths move apart differently.
                let overlap = [Side::Left, Side::Right]
                    .into_iter()
                    .any(|side| rects_overlap(self.eye_rect(i, side), self.eye_rect(j, side)));
                if i != j && overlap {
                    touch[i] |= 1 << j;
                    if j < i {
                        above[i] |= 1 << j;
                    }
                }
            }
        }
        let mut scenes = [0u32; 3];
        for (i, t) in self.tiles.iter().enumerate() {
            for scene in t.scenes() {
                scenes[scene as usize] |= 1 << i;
            }
        }
        // Each font and size in use, with emoji if any tile's text there takes them (the others share it).
        let mut font_sizes: Vec<(Face, u8, bool)> = Vec::new();
        for (t, p) in self.tiles.iter().zip(&plans) {
            for s in [p.scale, p.title_scale].into_iter().filter(|&s| s > 0) {
                match font_sizes.iter_mut().find(|f| (f.0, f.1) == (t.face(), s)) {
                    Some(f) => f.2 |= t.emoji(s),
                    None => font_sizes.push((t.face(), s, t.emoji(s))),
                }
            }
        }
        font_sizes.sort();
        let icon_sets: BTreeSet<(IconSet, u32)> =
            self.tiles.iter().zip(&plans).flat_map(|(t, p)| t.icon_sets(p)).collect();
        // Each after the frame while there's room, the rest down from the top of the code's area.
        let (mut at, mut low) = (FONTS, SPARE_END);
        let mut place = |size: u32| {
            if at + size <= FONTS_END {
                let here = at;
                at = (at + size + 3) & !3;
                here
            } else {
                low = (low - size) & !3;
                low
            }
        };
        let fonts: Vec<(Face, u8, bool, u32)> = font_sizes
            .into_iter()
            .map(|(face, s, emoji)| (face, s, emoji, place(fonts::blob(face, s, emoji).len() as u32)))
            .collect();
        let icons: Vec<((IconSet, u32), u32)> =
            icon_sets.into_iter().map(|(set, size)| ((set, size), place(set.bytes(size)))).collect();
        let geo = Geometry { plans, above, touch, scenes, fonts, icons, low };
        if low < SPARE_START {
            return Err(geo.too_big(SPARE_START));
        }
        Ok(geo)
    }

    /// Tile `i`'s sideways shift in the eye on `side`, for its depth (its own or the layout's).
    pub fn shift(&self, i: usize, side: Side) -> i32 {
        eye_shift(self.tiles[i].depth.unwrap_or(self.depth), side)
    }

    /// Tile `i`'s rectangle in the eye on `side`.
    pub fn eye_rect(&self, i: usize, side: Side) -> [i32; 4] {
        let [x, y, w, h] = self.tiles[i].rect;
        [x + self.shift(i, side), y, w, h]
    }

    /// Whether any tilt scene is on.
    pub fn tilted(&self) -> bool {
        self.tilt_down.is_some() || self.tilt_up.is_some()
    }

    /// Which tiles show when those in `on` (a mask) are on, in `scene`: in priority order, each that's on and in the
    /// scene unless one already showing overlaps it. The program on the glasses does the same.
    #[cfg(test)]
    pub fn showing(&self, geo: &Geometry, on: u32, scene: Scene) -> u32 {
        let on = on & geo.scenes[scene as usize];
        let mut shown = 0;
        for i in 0..self.tiles.len() {
            if on & 1 << i != 0 && geo.above[i] & shown == 0 {
                shown |= 1 << i;
            }
        }
        shown
    }

    /// Each tile's scenes written out (a layout from before scenes, or from the editor, may leave them to the kind).
    pub fn settle(mut self) -> Layout {
        for t in &mut self.tiles {
            t.scenes = Some(t.scenes());
            t.transition = Some(t.transition());
            t.slide = None;
        }
        self
    }

    pub fn load(path: &std::path::Path) -> Result<Layout, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str::<Layout>(&text).map(Layout::settle).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `date` in `format`, in the font's characters.
pub(crate) fn format_date(format: &str, date: NaiveDate) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = write!(out, "{}", date.format(format));
    ascii(&out)
}

/// `DATES` days of dates from `from` on, in `format`.
pub(crate) fn dates(format: &str, from: NaiveDate) -> Vec<String> {
    (0..DATES as u64).map(|d| format_date(format, from + chrono::Days::new(d))).collect()
}

/// `text` in the characters the font has: typography to its ASCII look-alike, accents dropped, anything else `?`.
pub(crate) fn ascii(text: &str) -> String {
    clean(text, false)
}

/// `text` in the characters the font has, as `ascii`, keeping the emoji it has (`emoji`) less what only joins or
/// styles them.
pub(crate) fn clean(text: &str, emoji: bool) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            ' '..='~' => out.push(c),
            c if emoji && fonts::is_emoji(c) => out.push(c),
            c if fonts::is_modifier(c) => {}
            '\n' | '\t' => out.push(' '),
            '‘' | '’' | '‚' | '′' => out.push('\''),
            '“' | '”' | '„' | '″' => out.push('"'),
            '–' | '—' | '−' | '‐' => out.push('-'),
            '…' => out.push_str("..."),
            '•' | '·' => out.push('*'),
            '×' => out.push('x'),
            '°' => out.push('°'),
            '\u{a0}' | '\u{2009}' | '\u{202f}' => out.push(' '),
            'à'..='å' => out.push('a'),
            'À'..='Å' => out.push('A'),
            'è'..='ë' => out.push('e'),
            'È'..='Ë' => out.push('E'),
            'ì'..='ï' => out.push('i'),
            'Ì'..='Ï' => out.push('I'),
            'ò'..='ö' | 'ø' => out.push('o'),
            'Ò'..='Ö' | 'Ø' => out.push('O'),
            'ù'..='ü' => out.push('u'),
            'Ù'..='Ü' => out.push('U'),
            'ç' => out.push('c'),
            'Ç' => out.push('C'),
            'ñ' => out.push('n'),
            'Ñ' => out.push('N'),
            'ß' => out.push_str("ss"),
            c if c.is_control() => {}
            _ => out.push('?'),
        }
    }
    out
}

/// Cells `chars` take (an emoji two).
fn cells(chars: &[char]) -> usize {
    chars.iter().map(|&c| fonts::width(c)).sum()
}

/// How many of `chars`, from the first, fit in `n` cells.
fn fitting(chars: &[char], n: usize) -> usize {
    let mut used = 0;
    chars
        .iter()
        .take_while(|&&c| {
            used += fonts::width(c);
            used <= n
        })
        .count()
}

/// `text` (cleaned) cut to `cols` cells, "..." where it's cut.
pub(crate) fn one_line(text: &str, cols: usize, emoji: bool) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let line: Vec<char> = clean(&words.join(" "), emoji).chars().collect();
    if cells(&line) <= cols {
        line.into_iter().collect()
    } else {
        line[..fitting(&line, cols.saturating_sub(3))].iter().collect::<String>() + "..."
    }
}

/// Words into at most `rows` lines of at most `cols` cells (an emoji takes two, with `emoji`; else it's `?`): a word
/// too long for a line is broken, and "..." ends the last line when text is left out.
pub(crate) fn wrap(text: &str, cols: usize, rows: usize, emoji: bool) -> Vec<String> {
    wrap_first(text, cols, cols, rows, emoji)
}

/// The same, the first line `first` cells at most.
pub(crate) fn wrap_first(text: &str, first: usize, cols: usize, rows: usize, emoji: bool) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line: Vec<char> = Vec::new();
    for word in clean(text, emoji).split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while !word.is_empty() {
            let cols = if lines.is_empty() { first } else { cols };
            let need = if line.is_empty() { cells(&word) } else { cells(&line) + 1 + cells(&word) };
            if need <= cols {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.append(&mut word);
            } else if !line.is_empty() {
                lines.push(std::mem::take(&mut line).into_iter().collect());
            } else {
                // Longer than a whole line: as much as fits, the rest on the next.
                let rest = word.split_off(fitting(&word, cols).max(1));
                lines.push(word.into_iter().collect());
                word = rest;
            }
        }
    }
    if !line.is_empty() {
        lines.push(line.into_iter().collect());
    }
    if lines.len() > rows {
        lines.truncate(rows);
        let room = if rows == 1 { first } else { cols };
        if let Some(last) = lines.last_mut() {
            let chars: Vec<char> = last.chars().collect();
            *last = chars[..fitting(&chars, room.saturating_sub(3))].iter().collect::<String>() + "...";
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_checks_and_round_trips() {
        let layout = Layout::default();
        let geo = layout.check().unwrap();
        let json = serde_json::to_string(&layout).unwrap();
        assert_eq!(serde_json::from_str::<Layout>(&json).unwrap(), layout);
        let i = |name: &str| layout.tiles.iter().position(|t| t.name == name).unwrap();
        let bits = |names: &[&str]| names.iter().fold(0u32, |m, n| m | 1 << i(n));
        let all = (1u32 << layout.tiles.len()) - 1;
        // Level: the notification on top hides the clock, date, battery and weather under it; the next turn, while
        // a route is followed, takes the status line's place.
        let level = bits(&["clock", "date", "battery", "weather"]);
        let turn = bits(&["directions"]);
        assert_eq!(layout.showing(&geo, all & !1 & !turn, Scene::Level), level | bits(&["status"]));
        assert_eq!(layout.showing(&geo, all & !1, Scene::Level), level | turn);
        assert_eq!(layout.showing(&geo, all & !turn, Scene::Level), bits(&["notification", "status"]));
        // Looking down: the list, heart rate, steps and the Mac's battery; the notification over the list.
        let down = bits(&["notifications", "heart", "steps", "mac_battery"]);
        assert_eq!(layout.showing(&geo, all & !1, Scene::Down), down);
        // Looking up: the big clock and the weather; the notification covers the clock's top, so it hides it.
        assert_eq!(layout.showing(&geo, all & !1, Scene::Up), bits(&["big_clock", "sky"]));
        assert_eq!(layout.showing(&geo, all, Scene::Up), bits(&["notification", "sky"]));
    }

    #[test]
    fn an_old_layout_loads_with_its_kinds_scenes() {
        let json = r#"{"depth":0.0,"boost":5,"tiles":[
            {"name":"n","kind":"notification","rect":[16,160,608,200]},
            {"name":"c","kind":"clock","rect":[16,136,188,20],"on":true,"size":2,"slide":true}]}"#;
        let layout = serde_json::from_str::<Layout>(json).unwrap().settle();
        assert_eq!(layout.tiles[0].scenes.as_deref(), Some(&SCENES[..]));
        // "slide" as layouts said it before: a notification slides by its kind, the clock as told.
        assert_eq!(layout.tiles.iter().map(|t| t.transition).collect::<Vec<_>>(), [Some(Transition::Slide); 2]);
        assert!(!serde_json::to_string(&layout).unwrap().contains("\"slide\":"));
        assert_eq!(layout.tiles[1].scenes.as_deref(), Some(&[Scene::Level][..]));
        assert!(layout.tiles[0].icon && layout.tilt_down.is_none());
        layout.check().unwrap();
    }

    #[test]
    fn a_hidden_tile_hides_nothing() {
        // a over b, b over c, a and c apart: with all on, a shows, b hides, and c shows.
        let mut layout =
            Layout { depth: 0.0, boost: 5, tilt_down: None, tilt_up: None, margins: [0; 4], tiles: Vec::new() };
        for (name, x) in [("a", 0), ("b", 90), ("c", 180)] {
            layout.tiles.push(Tile::new(name, Kind::Text, [x, 0, 100, 40]));
        }
        let geo = layout.check().unwrap();
        assert_eq!(layout.showing(&geo, 0b111, Scene::Level), 0b101);
        assert_eq!(layout.showing(&geo, 0b110, Scene::Level), 0b010);
    }

    #[test]
    fn tiles_at_different_depths_overlap_if_they_do_in_either_eye() {
        // 20 px apart; at 1 m the second moves 40 px each way, onto the first in the right eye.
        let mut layout = Layout { tiles: Vec::new(), ..Layout::default() };
        layout.tiles.push(Tile::new("a", Kind::Text, [100, 0, 100, 40]));
        layout.tiles.push(Tile::new("b", Kind::Text, [220, 0, 100, 40]));
        assert_eq!(layout.check().unwrap().above[1], 0);
        layout.tiles[1].depth = Some(1.0);
        assert_eq!(layout.eye_rect(1, Side::Left)[0], 260);
        assert_eq!(layout.eye_rect(1, Side::Right)[0], 180);
        assert_eq!(layout.check().unwrap().above[1], 1);
        layout.tiles[1].depth = Some(0.2);
        assert!(layout.check().unwrap_err().contains("depth"));
    }

    #[test]
    fn plans_fit_their_text() {
        let clock = Tile::new("c", Kind::Clock, [0, 0, 300, 40]);
        let plan = clock.plan().unwrap();
        // 11 characters, 296 px inside: 24 px each is 264.
        assert_eq!((plan.scale, plan.cols, plan.width), (3, 11, 264));
        let mut small = clock.clone();
        small.rect = [0, 0, 60, 12];
        assert!(small.plan().is_err());
        let note = Tile::new("n", Kind::Notification, [0, 0, 600, 200]);
        let plan = note.plan().unwrap();
        // The title less its 24 px icon and a gap, and 4 characters for the counter.
        assert_eq!((plan.title_scale, plan.scale, plan.title_cols, plan.cols, plan.icon), (3, 2, 19, 36, 24));
        // A 33 px title band, then 20 px lines in the 188 px left.
        assert_eq!(plan.rows, 7);
        let mut one = note.clone();
        one.single_line = true;
        // Framed: 588 × 28 inside, 24 px text after a 24 px icon and a gap, less the counter.
        one.rect = [0, 0, 600, 40];
        let plan = one.plan().unwrap();
        assert_eq!((plan.scale, plan.rows, plan.icon, plan.cols), (3, 1, 24, 19));
        let date = Tile::new("d", Kind::Date, [0, 0, 300, 30]);
        assert_eq!(date.plan().unwrap().cols, 10);
        // The weather: icon, " 12°", a drop and " 65%": 8 + 4 + 32 + 8 + 8 + 1 + 32 = 93 units.
        let weather = Tile::new("w", Kind::Weather, [0, 0, 200, 40]);
        assert_eq!(weather.plan().unwrap().width, 186);
        let mut bare = weather.clone();
        (bare.icon, bare.temp, bare.humidity) = (false, false, false);
        assert!(bare.plan().is_err());
        let list = Tile::new("l", Kind::NotificationList, [0, 0, 400, 104]);
        let plan = list.plan().unwrap();
        assert_eq!((plan.rows, plan.cols, plan.icon), (5, 23, 16));
        let heart = Tile::new("h", Kind::HeartRate, [0, 0, 200, 40]);
        // A heart, a gap, " 72" and " bpm": 8 + 4 + 24 + 32 = 68 units, at 2 px each.
        assert_eq!(heart.plan().unwrap().width, 136);
    }

    #[test]
    fn navigation_is_a_turn_when_wide_and_a_map_when_not() {
        // 604 × 60 inside: a 60 px arrow, then 32 px distance over 24 px street (59 px), 22 characters across.
        let turn = Tile::new("t", Kind::Navigation, [0, 0, 608, 64]);
        let plan = turn.plan().unwrap();
        assert!(!turn.is_map() && !plan.map);
        assert_eq!((plan.icon, plan.rows, plan.title_scale, plan.scale, plan.cols), (60, 2, 4, 3, 22));
        // Too low for two lines: both on one.
        let mut low = turn.clone();
        low.rect = [0, 0, 608, 24];
        let plan = low.plan().unwrap();
        assert_eq!((plan.icon, plan.rows, plan.scale), (20, 1, 2));
        let mut map = Tile::new("m", Kind::Navigation, [0, 0, 200, 200]);
        let plan = map.plan().unwrap();
        assert!(plan.map && plan.icon == 24);
        assert_eq!(map.map_view(), MapView { w: 196, h: 196, origin: (98, 130), circle: None });
        map.shape = Shape::Round;
        assert_eq!(map.map_view().circle, Some((98, 98, 97)));
        let mut tiny = map.clone();
        tiny.rect = [0, 0, 40, 40];
        assert!(tiny.plan().is_err());
    }

    #[test]
    fn checks_what_is_wrong() {
        let mut layout = Layout::default();
        layout.tiles[1].rect = [600, 0, 100, 40];
        assert!(layout.check().unwrap_err().contains("inside"));
        let mut layout = Layout::default();
        layout.tiles[2].name = "clock".into();
        assert!(layout.check().unwrap_err().contains("two tiles"));
        let mut layout = Layout::default();
        layout.tiles[2].format = "%Q".into();
        assert!(layout.check().is_err());
        let layout = Layout { tilt_down: Some(10), ..Layout::default() };
        assert!(layout.check().is_err());
        // Cut-off strips: the default's tiles keep 16 px from the top and 12 from the bottom.
        let layout = Layout { margins: [10, 0, 10, 0], ..Layout::default() };
        layout.check().unwrap();
        let layout = Layout { margins: [30, 0, 30, 0], ..Layout::default() };
        assert!(layout.check().unwrap_err().contains("cut-off strip"));
        let layout = Layout { margins: [200, 0, 220, 0], ..Layout::default() };
        assert!(layout.check().unwrap_err().contains("64 × 64"));
    }

    #[test]
    fn wraps_cuts_and_cleans_text() {
        assert_eq!(wrap("one two three four five six seven", 9, 2, false), ["one two", "three..."]);
        assert_eq!(wrap("abcdefghijkl", 5, 3, false), ["abcde", "fghij", "kl"]);
        assert_eq!(wrap("a  b\nc", 10, 2, false), ["a b c"]);
        assert_eq!(wrap("", 10, 2, false), Vec::<String>::new());
        assert_eq!(ascii("“Café” — 5×2…"), "\"Cafe\" - 5x2...");
        assert_eq!(format_date("%a %d %b", NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()), "Tue 10 Nov");
        // Emoji take two cells, lose their styling, and are "?" in a font without them.
        assert_eq!(wrap("lunch? 👍️ 🍜 ok", 9, 2, true), ["lunch? 👍", "🍜 ok"]);
        assert_eq!(wrap("lunch? 👍️🍜 ok", 9, 2, true), ["lunch?", "👍🍜 ok"]);
        assert_eq!(wrap("lunch? 👍️🍜 ok", 9, 2, false), ["lunch? ??", "ok"]);
        assert_eq!(one_line("🎉🎉🎉🎉 party", 7, true), "🎉🎉...");
    }

    #[test]
    fn a_notification_header_can_be_smaller_an_icon_or_none() {
        // 588 × 188 inside (framed), 16 px text.
        let mut note = Tile::new("n", Kind::Notification, [0, 0, 600, 200]);
        note.title_size = 2;
        let plan = note.plan().unwrap();
        // A 16 px title: a band of 16 + 4 + 3 px, then (188 - 23 + 4) / 20 lines.
        assert_eq!((plan.title_scale, plan.icon, plan.rows), (2, 16, 8));
        note.header = Header::Icon;
        let plan = note.plan().unwrap();
        // No title line: 9 lines of 16 px characters, each after a 16 px icon and a gap (the first has it).
        assert_eq!((plan.title_scale, plan.icon, plan.rows, plan.cols), (0, 16, 9, 35));
        note.header = Header::None;
        let plan = note.plan().unwrap();
        assert_eq!((plan.icon, plan.rows, plan.cols), (0, 9, 36));
        note.header = Header::Title;
        note.title_size = 7;
        assert!(note.plan().unwrap_err().contains("title size"));
    }

    #[test]
    fn plans_in_departure_mono() {
        // 596 × 196 inside: the body at 11 px (7 × 12 cells, 14 apart: 85 across, but 80 at most), the title at 22.
        let mut note = Tile::new("n", Kind::Notification, [0, 0, 608, 208]);
        note.font = Face::Departure;
        let plan = note.plan().unwrap();
        assert_eq!((plan.scale, plan.title_scale, plan.cols, plan.icon), (1, 2, 80, 24));
        // The title band: 24 + 4 + 3 px; then (196 - 31 + 2) / 14 lines.
        assert_eq!(plan.rows, 11);
        assert!(note.emoji(1) && note.emoji(2) && !note.emoji(3));
        // A clock and a battery in Departure Mono: 296 × 36 inside, 33 px fits.
        let mut clock = Tile::new("c", Kind::Clock, [0, 0, 300, 40]);
        clock.font = Face::Departure;
        // 11 characters 21 px apart.
        assert_eq!(clock.plan().unwrap().width, 11 * 21);
        let mut battery = Tile::new("b", Kind::Battery, [0, 0, 300, 40]);
        battery.font = Face::Departure;
        // The battery two characters' height wide, then "100%".
        let plan = battery.plan().unwrap();
        assert_eq!((plan.scale, plan.width), (3, 2 * 36 + 4 * 21));
        assert!(!battery.emoji(1));
        clock.size = 4;
        assert!(clock.plan().unwrap_err().contains("at most 3"));
    }
}
