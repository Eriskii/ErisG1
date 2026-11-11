//! The shell's program, made from a layout for one arm. It runs on the glasses by itself: it keeps the time, turns
//! tiles off when their time is up, follows the head's tilt between scenes, works out which tiles show, draws them
//! and runs the panel. The Mac (later the phone) only sends it content and the time, and can come and go.
//!
//! How it shows tiles: the tiles are in priority order, and `active` and `visible` are masks of them. Whenever
//! `active` or the scene changes, `arbitrate` goes down the list: a tile that's on and in the scene shows unless a
//! tile already showing overlaps it (the layout gives each tile the mask of those above it that overlap). Each tile
//! that changed has its rectangle cleared, and every showing tile that overlaps one of those is drawn again, so the
//! frame always holds exactly what should show. Then the rectangle around all that changed goes to the panel in one
//! `DRAW`.
//!
//! Both eyes together, without the Mac: things happen at moments both arms know. Ticks are on whole seconds, which
//! the Mac sets in each arm's clock and the right arm keeps the left's to by the arm-to-arm link (a message every
//! minute with how late after its tick it was sent, which `PEER_T_US` turns into the left's clock, and the scene). A
//! gesture, a battery reading, a change of scene (the right arm watches the tilt) and the Mac's updates are acted on
//! at a moment after they happen, on both arms, after any tick due before it; drawing that takes a while goes to the
//! panel a little later, at a moment both arms wait for.
//!
//! The Mac's side: it writes a tile's content (lines, icons, values), then its words (generation, on, how long), then
//! when to act, and `RAISE`s USER1. The program takes a tile whose generation changed. The program never writes those
//! words, so the two never race; the Mac never touches r0–r14.

use super::fonts;
use super::icons::{self, IconSet};
use super::layout::{
    Align, DATES, DISTANCE_COLS, Geometry, Kind, Layout, Part, Plan, TILT_HYSTERESIS, one_line, part_width,
};
use super::nav::{self, MAX_ROADS, MAX_ROUTE};
use crate::apps::kit::Options;
use crate::apps::vmkit::{EVENT_LEAD_US, Prog, Program, R0, R1, R2, R3, R4, R5, R6, R8, R9, R10, R11, TABLE};
use crate::proto::Side;
use crate::v5::{self, prop, trigger};
use crate::v5_asm::*;

/// The words the kit keeps for drawing (`v5::fixed_args`).
const W_FRAME: u16 = 16;
const W_STRIDE: u16 = 17;
const W_MASK: u16 = 19;

const PERIOD: u32 = 1_000_000;
const DAY: u32 = 86_400;
/// How long after a gesture, a battery message or a change of scene both arms act on it.
const LEAD: u32 = EVENT_LEAD_US;
/// How long after the moment the panel changes: drawing a notification's text takes a few tenths of a second
/// under the 30 ms in 100 ms cap, and each arm's 100 ms periods start at their own times.
const SHOW_GESTURE: u32 = 120_000;
const SHOW_HOST: u32 = 400_000;
/// A message of its own (battery, clock sync) waits until the arm has sent nothing for this long, so it doesn't take
/// the place of a gesture still on its way.
const QUIET: u32 = 400_000;
/// The right arm sends the left its tick every minute, at this second of it.
const SYNC_EVERY: u32 = 60;
const SYNC_AT: u32 = 7;
/// A sync moves the left's ticks by at most this.
const MAX_CORRECTION: u32 = 250_000;
/// Ticks this far behind are skipped rather than caught up one by one.
const SKIP_AFTER: u32 = 3 * PERIOD;
/// A tilt past a threshold must last this long to change the scene.
const TILT_SETTLE: u32 = 150_000;

/// Arm-to-arm messages: a gesture is its code (1–7); the rest have a class in the high bits.
const MSG_BATTERY: u32 = 0x0001_0000;
const MSG_LEFT: u32 = 0x100;
const MSG_SCENE: u32 = 0x0002_0000;
/// The right arm's tick, sent this many µs after it (the low 20 bits), and its scene (bits 20–21).
const MSG_SYNC: u32 = 0x2000_0000;
const PHASE_MASK: u32 = 0x000F_FFFF;
const SYNC_SCENE_SHIFT: u32 = 20;
const UNKNOWN_PCT: u32 = 0xFF;

/// The Mac's updates come with USER1.
pub(crate) const HOST_TRIGGER: u8 = 13;
/// What `NOTIFY` tells the Mac: a tile turned off (its time was up, or a tap), or one brought back.
pub(crate) const NOTE_EXPIRED: u32 = 1;
pub(crate) const NOTE_DISMISSED: u32 = 2;
pub(crate) const NOTE_RESTORED: u32 = 3;
/// A tile's "on" word: off, on, or as it is (new content only).
pub(crate) const WANT_OFF: u32 = 0;
pub(crate) const WANT_ON: u32 = 1;
pub(crate) const WANT_KEEP: u32 = 2;
/// Where the shell signs the arena: "ERSH", the program's checksum, the side. `ARENA_CRC` reads it back.
pub(crate) const SIGNATURE: u32 = 0x3FF00;
/// Host code's own words: never r0–r14, which are the program's while it runs.
const SCRATCH: u32 = 0x60;
const BORDER_LEVEL: u8 = 7;
const RULE_LEVEL: u8 = 5;
/// BLIT's mode for an icon: 4-bit coverage, drawn as `MASK`; and as `ERASE`, for the dark edge around the position on
/// a map.
const ICON_MODE: u32 = 2;
const ERASE_MODE: u32 = 3;
/// A round map's edge.
const RING_LEVEL: u8 = 7;

/// Where the Mac writes, the same in both arms' programs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Map {
    /// The time: generation, second of the day shown until the tick, the tick (this arm's clock).
    pub time: u32,
    /// When to act on an update, this arm's clock.
    pub due: u32,
    /// The moment to start, this arm's clock.
    pub start: u32,
    /// The masks of tiles on and showing, and the scene (0 level, 1 down, 2 up).
    pub active: u32,
    pub visible: u32,
    pub scene: u32,
    pub tiles: Vec<TileMap>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileMap {
    /// Generation, on (`WANT_*`), how long (µs, 0 for until turned off).
    pub host: u32,
    /// Two words of values: the Mac's battery, %, and whether it charges.
    pub values: u32,
    /// A byte per line, its length; a byte per line, its icon (0 if none); then each line's address and how many
    /// characters it holds. A notification's line 0 is its title; a date tile has a line per day.
    pub lens: u32,
    pub icons: u32,
    pub lines: Vec<(u32, usize)>,
    /// Clock and battery: the text they last drew (glyph indices).
    pub buf: u32,
    /// A map: how many points the route and the roads have (two u16s), then the route's points and the roads' (x, y
    /// as u16s from the map's corner, `nav::BREAK` between lines), `MAX_ROUTE` and `MAX_ROADS` of them; 0 for none.
    pub points: u32,
}

/// What the Mac sends a tile.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TileData {
    pub lines: Vec<String>,
    /// An icon a line, an index into the tile's set (`icons::NONE` for none).
    pub icons: Vec<u8>,
    pub values: Vec<u32>,
    /// A map's lines.
    pub map: Option<nav::Lines>,
}

/// A program for one arm, and where things are in it.
pub(crate) struct Built {
    pub program: Program,
    pub map: Map,
}

/// The part of x, y, w, h on the panel.
fn on_panel([x, y, w, h]: [i32; 4]) -> Option<[i32; 4]> {
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + w).min(v5::WIDTH as i32), (y + h).min(v5::HEIGHT as i32));
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// Where `width` pixels start in `w` from `x` by `align`.
fn aligned(align: Align, x: i32, w: i32, width: i32) -> i32 {
    match align {
        Align::Left => x,
        Align::Center => x + (w - width) / 2,
        Align::Right => x + w - width,
    }
}

fn glyph(c: char) -> u32 {
    v5::text_indices(&c.to_string())[0] as u32
}

/// A tile's lines, their capacities, by kind.
fn capacities(tile: &super::layout::Tile, plan: &Plan) -> Vec<usize> {
    match tile.kind {
        Kind::Text | Kind::NotificationList => vec![plan.cols; plan.rows],
        Kind::Notification if tile.single_line => vec![plan.cols],
        Kind::Notification => std::iter::once(plan.title_cols).chain(vec![plan.cols; plan.rows]).collect(),
        Kind::Date => vec![plan.cols; DATES],
        Kind::Weather => vec![4, 4],
        Kind::Steps => vec![6],
        Kind::HeartRate => vec![3],
        Kind::Navigation if plan.map => Vec::new(),
        Kind::Navigation if plan.rows == 2 => vec![DISTANCE_COLS, plan.cols],
        Kind::Navigation => vec![plan.cols],
        Kind::Clock | Kind::Battery | Kind::DeviceBattery => Vec::new(),
    }
}

/// Whether a tile's lines have icons.
fn has_icons(tile: &super::layout::Tile) -> bool {
    matches!(tile.kind, Kind::Notification | Kind::NotificationList | Kind::Weather) && tile.icon
}

/// The bytes of a map's points buffer.
const POINTS_BYTES: usize = 4 + 4 * (MAX_ROUTE + MAX_ROADS);

/// The bytes of a tile's lines buffer: the lengths and (with `icons`) the icons, each padded to a word, then each
/// line (glyph varints: an emoji's two bytes are its two cells, so a line's capacity in bytes is its cells).
fn lines_buffer(caps: &[usize], icons: bool, data: &TileData) -> Vec<u8> {
    let n = caps.len().next_multiple_of(4);
    let mut lens = vec![0u8; n];
    let mut marks = vec![icons::NONE; if icons { n } else { 0 }];
    let mut text = Vec::new();
    let space = glyph(' ') as u8;
    for (i, &cap) in caps.iter().enumerate() {
        let line = data.lines.get(i).map(|l| fonts::encode(l)).unwrap_or_default();
        let mut line = fonts::cut(line, cap);
        lens[i] = line.len() as u8;
        if icons {
            marks[i] = data.icons.get(i).copied().unwrap_or(icons::NONE);
        }
        line.resize(cap, space);
        text.extend(line);
    }
    lens.extend(marks);
    lens.extend(text);
    lens
}

struct Vars {
    start: u16,
    tick: u16,
    sod: u16,
    day: u16,
    count: u16,
    active: u16,
    visible: u16,
    scene: u16,
    /// The right arm's view of the scene, which it sends.
    tilt_scene: u16,
    timed: u16,
    filled: u16,
    quiet: u16,
    in_case: u16,
    /// 0 dark, 1 lit, 2 unknown (at the start: the panel may show another program's picture).
    lit: u16,
    drawn: u16,
    changed: u16,
    updated: u16,
    moment: u16,
    present_at: u16,
    now_tick: u16,
    ev: u16,
    last_send: u16,
    batt_pending: u16,
    batt_l: u16,
    batt_r: u16,
    time_seen: u16,
    msg: u16,
    msg_t: u16,
    /// The rectangle (x0, y0, x1, y1) drawn since the panel last changed.
    dirty: [u16; 4],
}

struct TileVars {
    seen: u16,
    expires: u16,
    /// Clock: the second (or minute) drawn; date: the day.
    shown: u16,
    /// The Mac's words: generation, on, how long; its values.
    host: u16,
    values: u16,
    /// Clock and battery: the text buffer; a label's.
    buf: u32,
    label: u32,
    draw: &'static str,
}

struct Gen<'a> {
    p: Prog,
    layout: &'a Layout,
    geo: &'a Geometry,
    side: Side,
    v: Vars,
    t: Vec<TileVars>,
    time: u16,
    due: u16,
    scenes: u32,
    map: Map,
}

/// The program for the arm on `side`.
pub(crate) fn build(layout: &Layout, geo: &Geometry, side: Side) -> Result<Built, String> {
    let options = Options { font: 2, big: 0, imu: false, boost: layout.boost };
    let mut p = Prog::new(side, &options);
    let n = layout.tiles.len();

    // What the Mac writes first, so it's at the same place in both arms' programs.
    let time = p.data(&[0; 12]);
    let due = p.data(&[0; 4]);
    let mut tile_maps = Vec::new();
    for (tile, plan) in layout.tiles.iter().zip(&geo.plans) {
        let host = p.data(&[0; 12]);
        let values = p.data(&[UNKNOWN_PCT as u8, 0, 0, 0, 0, 0, 0, 0]);
        let caps = capacities(tile, plan);
        let initial = match tile.kind {
            Kind::Text if !tile.text.trim().is_empty() => {
                let lines = super::layout::wrap(&tile.text, plan.cols, plan.rows, tile.emoji(plan.scale));
                TileData { lines, ..TileData::default() }
            }
            _ => TileData::default(),
        };
        let icons = has_icons(tile);
        let (lens, marks, lines) = if caps.is_empty() {
            (0, 0, Vec::new())
        } else {
            let at = p.data(&lines_buffer(&caps, icons, &initial));
            let pad = caps.len().next_multiple_of(4) as u32;
            let marks = if icons { at + pad } else { 0 };
            let mut line_at = at + pad * if icons { 2 } else { 1 };
            let mut lines = Vec::new();
            for &cap in &caps {
                lines.push((line_at, cap));
                line_at += cap as u32;
            }
            (at, marks, lines)
        };
        let points = if plan.map { p.data(&[0; POINTS_BYTES]) } else { 0 };
        tile_maps.push(TileMap { host, values, lens, icons: marks, lines, buf: 0, points });
    }
    let scenes = p.data(&geo.scenes.iter().flat_map(|m| m.to_le_bytes()).collect::<Vec<u8>>());

    let on: u32 =
        (0..n).filter(|&i| layout.tiles[i].on || !layout.tiles[i].text.trim().is_empty()).fold(0, |m, i| m | 1 << i);
    let filled: u32 = (0..n)
        .filter(|&i| layout.tiles[i].kind == Kind::Text && !layout.tiles[i].text.trim().is_empty())
        .fold(0, |m, i| m | 1 << i);
    let v = Vars {
        start: p.var_start(0),
        tick: p.var_start(PERIOD as i64),
        sod: p.var(0),
        day: p.var(0),
        count: p.var(0),
        active: p.var(on),
        visible: p.var(0),
        scene: p.var(0),
        tilt_scene: p.var(0),
        timed: p.var(0),
        filled: p.var(filled),
        quiet: p.var(0),
        in_case: p.var(0),
        lit: p.var(2),
        drawn: p.var(0),
        changed: p.var(0),
        updated: p.var(0),
        moment: p.var(0),
        present_at: p.var(0),
        now_tick: p.var(0),
        ev: p.var(0),
        last_send: p.var(0),
        batt_pending: p.var(0),
        batt_l: p.var(UNKNOWN_PCT),
        batt_r: p.var(UNKNOWN_PCT),
        time_seen: p.var(0),
        msg: p.var(0),
        msg_t: p.var(0),
        dirty: [p.var(v5::WIDTH), p.var(v5::HEIGHT), p.var(0), p.var(0)],
    };
    let mut t = Vec::new();
    for (i, tile) in layout.tiles.iter().enumerate() {
        let buf = match tile.kind {
            Kind::Clock => p.text(match (tile.h24, tile.seconds) {
                (true, true) => "00:00:00",
                (true, false) => "00:00",
                (false, true) => "12:00:00 AM",
                (false, false) => "12:00 AM",
            }),
            Kind::Battery if tile.both => p.text("L --% R --%"),
            Kind::Battery | Kind::DeviceBattery => p.text(" --%"),
            _ => 0,
        };
        let label = match tile.parts().iter().find_map(|part| if let Part::Label(l) = part { Some(*l) } else { None }) {
            Some(text) => p.text(text),
            None => 0,
        };
        tile_maps[i].buf = buf;
        t.push(TileVars {
            seen: p.var(0),
            expires: p.var(0),
            shown: p.var(u32::MAX),
            host: (tile_maps[i].host / 4) as u16,
            values: (tile_maps[i].values / 4) as u16,
            buf,
            label,
            draw: p.fresh("draw"),
        });
    }
    let map = Map {
        time,
        due,
        start: v.start as u32 * 4,
        active: v.active as u32 * 4,
        visible: v.visible as u32 * 4,
        scene: v.scene as u32 * 4,
        tiles: tile_maps,
    };
    let mut g = Gen { p, layout, geo, side, v, t, time: (time / 4) as u16, due: (due / 4) as u16, scenes, map };
    g.handlers();
    g.step();
    g.arbitrate();
    g.present();
    g.apply_host();
    g.routines();
    if g.p.right() && layout.tilted() {
        g.tilt();
    }
    for i in 0..n {
        g.draw_tile(i);
    }
    let program = g.p.finish()?;
    Ok(Built { program, map: g.map })
}

/// Which icon of a set to draw.
#[derive(Clone, Copy)]
enum Pick {
    /// The set's first, always.
    First,
    /// The index in byte `j` of the tile's icons (none past the set's end).
    Line(usize),
    /// The first, while the tile's second value isn't 0 (the Mac charging).
    WhileCharging,
}

impl Gen<'_> {
    fn k(&mut self, value: u32) -> u16 {
        self.p.k(value)
    }

    fn ki(&mut self, value: i32) -> u16 {
        self.p.ki(value)
    }

    fn bit(&mut self, i: usize) -> u16 {
        self.p.k(1 << i)
    }

    fn not_bit(&mut self, i: usize) -> u16 {
        self.p.k(!(1u32 << i))
    }

    fn tiles(&self) -> usize {
        self.layout.tiles.len()
    }

    /// Tile `i`'s rectangle in this eye (moved sideways for its depth).
    fn rect(&self, i: usize) -> [i32; 4] {
        self.layout.eye_rect(i, self.side)
    }

    fn inner(&self, i: usize) -> [i32; 4] {
        let [x, y, w, h] = self.geo.plans[i].inner;
        [x + self.layout.shift(i, self.side), y, w, h]
    }

    /// The tile's words, generation, on and how long.
    fn host_words(&self, i: usize) -> [u16; 3] {
        let h = self.t[i].host;
        [h, h + 1, h + 2]
    }

    /// Adds the constant rectangle x, y, w, h to the dirty one.
    fn mark_k(&mut self, [x, y, w, h]: [i32; 4]) {
        for (r, v) in [(R8, x), (R9, y), (R10, x + w), (R11, y + h)] {
            let c = self.ki(v);
            self.p.op(MOV, &[r, c]);
        }
        self.p.jump(CALL, "mark");
    }

    /// Tile `i`'s font's cell at `scale`.
    fn metrics(&self, i: usize, scale: u8) -> super::layout::Metrics {
        self.layout.tiles[i].metrics(scale)
    }

    /// GLYPHS of `n` (a word) bytes of glyphs at the address in `text`, in tile `i`'s font at `scale`, the pen at the
    /// words `x`, `baseline`.
    #[allow(clippy::too_many_arguments)]
    fn glyphs(&mut self, i: usize, scale: u8, x: u16, baseline: u16, text: u16, n: u16, level: u16) {
        let font = self.k(self.geo.font(self.layout.tiles[i].face(), scale));
        self.p.op(GLYPHS, &[font, W_FRAME, W_STRIDE, x, baseline, W_MASK, level, text, n]);
    }

    /// Tile `i`'s icon from `set`, `size` px, its top left at x, y. Uses R4.
    fn icon(&mut self, i: usize, set: IconSet, size: u32, pick: Pick, x: i32, y: i32) {
        let base = self.geo.icons_at(set, size);
        let bytes = size * size / 2;
        let [zero, stride, cx, cy, n, mode, level] =
            [0, size / 2, x as u32, y as u32, size, ICON_MODE, self.layout.tiles[i].level as u32].map(|v| self.k(v));
        let skip = self.p.fresh("no_icon");
        let src = match pick {
            Pick::First => self.k(base),
            Pick::WhileCharging => {
                let charging = self.t[i].values + 1;
                self.p.branch(BEQ, charging, zero, skip);
                self.k(base)
            }
            Pick::Line(j) => {
                let [marks, line, count, k_bytes, k_base] =
                    [self.map.tiles[i].icons, j as u32, set.icons().len() as u32, bytes, base].map(|v| self.k(v));
                self.p.op(LD8, &[R4, marks, line]);
                self.p.branch(BGEU, R4, count, skip);
                self.p.op(MUL, &[R4, R4, k_bytes]);
                self.p.op(ADD, &[R4, R4, k_base]);
                R4
            }
        };
        self.p.op(BLIT, &[src, stride, zero, zero, W_FRAME, W_STRIDE, cx, cy, n, n, mode, level]);
        self.p.label(skip);
    }

    /// Into `out`, the pen x for `len` (a word) cells of tile `i`'s font at `scale` aligned in x, w.
    #[allow(clippy::too_many_arguments)]
    fn line_x(&mut self, i: usize, out: u16, len: u16, scale: u8, align: Align, x: i32, w: i32) {
        let advance = self.k(self.metrics(i, scale).advance as u32);
        match align {
            Align::Left => {
                let x = self.ki(x);
                self.p.op(MOV, &[out, x]);
            }
            Align::Center => {
                let (w, one, x) = (self.ki(w), self.k(1), self.ki(x));
                self.p.op(MUL, &[out, len, advance]);
                self.p.op(SUB, &[out, w, out]);
                self.p.op(SAR, &[out, out, one]);
                self.p.op(ADD, &[out, out, x]);
            }
            Align::Right => {
                let end = self.ki(x + w);
                self.p.op(MUL, &[out, len, advance]);
                self.p.op(SUB, &[out, end, out]);
            }
        }
    }

    /// Sends the word `value` (in R2) to the other arm, if the link is on. Uses R3.
    fn send(&mut self) {
        self.p.jump(CALL, "send");
    }

    /// R2 to both arms, acted on by both LEAD later (this one through USER0).
    fn broadcast(&mut self) {
        let (msg, msg_t) = (self.v.msg, self.v.msg_t);
        let user0 = self.k(trigger::USER0 as u32);
        self.p.op(MOV, &[msg, R2]);
        self.p.read(msg_t, prop::TIME_US);
        self.send();
        self.p.op(RAISE, &[user0]);
    }

    fn handlers(&mut self) {
        let v = &self.v;
        let (start, tick, moment, present_at, lit) = (v.start, v.tick, v.moment, v.present_at, v.lit);
        let (batt_pending, msg, msg_t) = (v.batt_pending, v.msg, v.msg_t);
        let [zero, one, two, lead] = [0, 1, 2, LEAD].map(|x| self.k(x));
        let tilted = self.p.right() && self.layout.tilted();
        let p = &mut self.p;
        p.on(trigger::START, "start");
        p.on(trigger::TIMER, "timer");
        p.on(trigger::BATTERY, "battery");
        p.on(trigger::CASE, "case");
        p.on(trigger::TOUCH, "touch");
        p.on(trigger::PEER, "peer");
        p.on(trigger::USER0, "broadcast");
        p.on(HOST_TRIGGER, "host");
        if tilted {
            p.on(trigger::TILT, "tilt");
        }

        // START: a clear frame, what the Mac left (the time, content), then the tiles that show, all at once at
        // the start moment. The right arm looks at the tilt.
        p.label("start");
        p.op(MOV, &[moment, start]);
        p.fill_k(0, 0, v5::WIDTH as i32, v5::HEIGHT as i32, 0);
        p.jump(CALL, "read_case");
        p.jump(CALL, "apply_host");
        p.jump(CALL, "arbitrate");
        p.op(MOV, &[lit, two]);
        p.op(MOV, &[present_at, moment]);
        p.jump(CALL, "present");
        p.timer(tick);
        p.op(MOV, &[batt_pending, one]);
        if tilted {
            p.jump(CALL, "tilt_check");
        }
        p.op(END, &[]);

        // TIMER: the tick due, unless an event has already counted it.
        p.label("timer");
        p.read(R0, prop::TIME_US);
        p.op(SUB, &[R0, R0, tick]);
        p.branch(BLTS, R0, zero, "timer_early");
        p.jump(CALL, "step");
        p.op(END, &[]);
        p.label("timer_early");
        p.timer(tick);
        p.op(END, &[]);

        // BATTERY: the next tick sends the new reading to both arms.
        p.label("battery");
        p.op(MOV, &[batt_pending, one]);
        p.op(END, &[]);

        // CASE: in the case nothing shows and the panel is off.
        p.label("case");
        p.jump(CALL, "read_case");
        p.jump(CALL, "arbitrate");
        p.read(present_at, prop::TIME_US);
        p.jump(CALL, "present");
        p.op(END, &[]);

        // TOUCH: to the other arm, then both act on it LEAD later.
        p.label("touch");
        p.read(R1, prop::TIME_US);
        p.read(R0, prop::TOUCH_GESTURE);
        p.op(MOV, &[R2, R0]);
        self.send();
        let p = &mut self.p;
        p.op(ADD, &[R1, R1, lead]);
        p.jump(JMP, "handle");

        // PEER: the other arm's gesture, battery or scene, acted on LEAD after it was sent; on the left, the right's
        // tick.
        p.label("peer");
        p.read(R0, prop::PEER_MSG);
        p.read(R1, prop::PEER_T_US);
        let (k28, k2) = (p.k(28), p.k(2));
        p.op(SHR, &[R2, R0, k28]);
        p.branch(BNE, R2, k2, "peer_event");
        if !p.right() {
            self.sync();
        }
        let p = &mut self.p;
        p.op(END, &[]);
        p.label("peer_event");
        p.op(ADD, &[R1, R1, lead]);
        p.jump(JMP, "handle");

        // USER0: this arm's own message, acted on when the other arm does.
        p.label("broadcast");
        p.op(MOV, &[R0, msg]);
        p.op(ADD, &[R1, msg_t, lead]);
        p.jump(JMP, "handle");

        // USER1: the Mac's update, at the moment it gave.
        let (due, show) = (self.due, self.k(SHOW_HOST));
        let n = self.tiles();
        let p = &mut self.p;
        p.label("host");
        p.op(MOV, &[moment, due]);
        p.op(SLEEP_UNTIL, &[moment]);
        p.jump(CALL, "catch_up");
        p.jump(CALL, "apply_host");
        p.jump(CALL, "arbitrate");
        let (updated, visible, drawn) = (self.v.updated, self.v.visible, self.v.drawn);
        for i in 0..n {
            let (bit, next, draw) = (self.bit(i), self.p.fresh("host_next"), self.t[i].draw);
            let p = &mut self.p;
            p.op(AND, &[R0, updated, bit]);
            p.branch(BEQ, R0, zero, next);
            p.op(AND, &[R0, visible, bit]);
            p.branch(BEQ, R0, zero, next);
            p.op(AND, &[R0, drawn, bit]);
            p.branch(BNE, R0, zero, next);
            p.jump(CALL, draw);
            p.label(next);
        }
        let p = &mut self.p;
        p.op(ADD, &[present_at, moment, show]);
        p.jump(CALL, "present");
        p.op(END, &[]);

        self.handle();
    }

    /// On the left: the right arm's tick, sent `phase` µs after it, moves this arm's ticks to the nearest whole
    /// second of the right's (by a quarter second at most); and its scene, if a change of scene didn't arrive.
    fn sync(&mut self) {
        let (tick, scene, present_at) = (self.v.tick, self.v.scene, self.v.present_at);
        let [mask, half, period, max, window, shift, three] =
            [PHASE_MASK, PERIOD / 2, PERIOD, MAX_CORRECTION, 2 * MAX_CORRECTION, SYNC_SCENE_SHIFT, 3]
                .map(|x| self.k(x));
        let p = &mut self.p;
        p.op(SHR, &[R6, R0, shift]);
        p.op(AND, &[R6, R6, three]);
        p.op(AND, &[R2, R0, mask]);
        p.op(SUB, &[R3, R1, R2]);
        p.op(SUB, &[R4, tick, R3]);
        p.op(ADD, &[R4, R4, half]);
        p.op(DIVS, &[R5, R4, period]);
        p.op(MUL, &[R5, R5, period]);
        p.op(ADD, &[R5, R5, R3]);
        p.op(SUB, &[R2, R5, tick]);
        p.op(ADD, &[R2, R2, max]);
        p.branch(BGEU, R2, window, "sync_scene");
        p.op(MOV, &[tick, R5]);
        p.timer(tick);
        p.label("sync_scene");
        p.branch(BEQ, R6, scene, "sync_end");
        p.op(MOV, &[scene, R6]);
        p.jump(CALL, "arbitrate");
        p.read(present_at, prop::TIME_US);
        p.jump(CALL, "present");
        p.label("sync_end");
    }

    /// On the right, with tilt scenes: TILT (the head left the range set for its scene) and the start. After a
    /// moment to settle, the scene the pitch is in, with some way back past each threshold before leaving it; the
    /// range around it; and a new scene to both arms.
    fn tilt(&mut self) {
        let (down, up) = (self.layout.tilt_down, self.layout.tilt_up);
        let h = TILT_HYSTERESIS;
        let tilt_scene = self.v.tilt_scene;
        let [zero, one, two, settle, unknown, scene_msg] =
            [0, 1, 2, TILT_SETTLE, v5::UNKNOWN, MSG_SCENE].map(|x| self.k(x));
        let p = &mut self.p;
        p.label("tilt");
        p.read(R0, prop::TIME_US);
        p.op(ADD, &[R0, R0, settle]);
        p.op(SLEEP_UNTIL, &[R0]);
        p.jump(CALL, "tilt_check");
        p.op(END, &[]);

        p.label("tilt_check");
        p.read(R0, prop::PITCH);
        p.branch(BEQ, R0, unknown, "tilt_end");
        p.op(MOV, &[R1, zero]);
        if let Some(d) = down {
            let [k_d, k_back] = [d, d + h].map(|v| self.ki(v));
            let p = &mut self.p;
            p.op(MOV, &[R2, k_d]);
            p.branch(BNE, tilt_scene, one, "tilt_down_cmp");
            p.op(MOV, &[R2, k_back]);
            p.label("tilt_down_cmp");
            p.branch(BGES, R0, R2, "tilt_not_down");
            p.op(MOV, &[R1, one]);
            p.jump(JMP, "tilt_decided");
            p.label("tilt_not_down");
        }
        if let Some(u) = up {
            let [k_u, k_back] = [u, u - h].map(|v| self.ki(v));
            let p = &mut self.p;
            p.op(MOV, &[R2, k_u]);
            p.branch(BNE, tilt_scene, two, "tilt_up_cmp");
            p.op(MOV, &[R2, k_back]);
            p.label("tilt_up_cmp");
            p.branch(BGES, R2, R0, "tilt_decided");
            p.op(MOV, &[R1, two]);
        }
        let ranges = [
            (down.unwrap_or(-90), up.unwrap_or(90)),
            (-90, down.map_or(90, |d| d + h)),
            (up.map_or(-90, |u| u - h), 90),
        ];
        let ks: Vec<[u16; 2]> = ranges.iter().map(|&(lo, hi)| [self.ki(lo), self.ki(hi)]).collect();
        let p = &mut self.p;
        p.label("tilt_decided");
        p.branch(BEQ, R1, one, "tilt_range_down");
        p.branch(BEQ, R1, two, "tilt_range_up");
        for (s, label) in [(0, "tilt_range_level"), (1, "tilt_range_down"), (2, "tilt_range_up")] {
            p.label(label);
            p.set(prop::PITCH_LO, ks[s][0]);
            p.set(prop::PITCH_HI, ks[s][1]);
            p.jump(JMP, "tilt_set");
        }
        p.label("tilt_set");
        p.branch(BEQ, R1, tilt_scene, "tilt_end");
        p.op(MOV, &[tilt_scene, R1]);
        p.op(OR, &[R2, R1, scene_msg]);
        self.broadcast();
        self.p.label("tilt_end");
        self.p.op(RET, &[]);
    }

    /// An event (r0) at a moment both arms share (r1): after the ticks due before it, a battery reading, a scene,
    /// or a gesture. A tap turns off the top dismissable tile that shows; a double tap brings back the top one that's
    /// off and has had content; a hold hides everything, or shows it again.
    fn handle(&mut self) {
        let (ev, moment, present_at, scene) = (self.v.ev, self.v.moment, self.v.present_at, self.v.scene);
        let (batt_l, batt_r, visible, active, timed, filled, quiet) =
            (self.v.batt_l, self.v.batt_r, self.v.visible, self.v.active, self.v.timed, self.v.filled, self.v.quiet);
        let [zero, one, two, three, four, k16, low, left, show] =
            [0, 1, 2, 3, 4, 16, 0xFF, MSG_LEFT, SHOW_GESTURE].map(|x| self.k(x));
        let n = self.tiles();
        let p = &mut self.p;
        p.label("handle");
        p.op(MOV, &[ev, R0]);
        p.op(MOV, &[moment, R1]);
        p.op(SLEEP_UNTIL, &[moment]);
        p.jump(CALL, "catch_up");
        p.op(SHR, &[R0, ev, k16]);
        p.branch(BEQ, R0, two, "scene");
        p.branch(BNE, R0, one, "gesture");
        p.op(AND, &[R1, ev, low]);
        p.op(AND, &[R2, ev, left]);
        p.branch(BEQ, R2, zero, "batt_right");
        p.op(MOV, &[batt_l, R1]);
        p.jump(JMP, "batt_draw");
        p.label("batt_right");
        p.op(MOV, &[batt_r, R1]);
        p.label("batt_draw");
        for i in 0..n {
            if self.layout.tiles[i].kind != Kind::Battery {
                continue;
            }
            let (bit, next, draw) = (self.bit(i), self.p.fresh("batt_next"), self.t[i].draw);
            let p = &mut self.p;
            p.op(AND, &[R0, visible, bit]);
            p.branch(BEQ, R0, zero, next);
            p.jump(CALL, draw);
            p.label(next);
        }
        let p = &mut self.p;
        p.jump(JMP, "handled");

        p.label("scene");
        p.op(AND, &[scene, ev, three]);
        p.jump(JMP, "rearrange");

        p.label("gesture");
        p.branch(BEQ, ev, one, "tap");
        p.branch(BEQ, ev, two, "double");
        p.branch(BEQ, ev, four, "hold");
        p.jump(JMP, "handled");

        let dismissable: Vec<usize> = (0..n).filter(|&i| self.layout.tiles[i].dismiss).collect();
        self.p.label("tap");
        for &i in &dismissable {
            let (bit, not, next, k_note, k_i) =
                (self.bit(i), self.not_bit(i), self.p.fresh("tap_next"), self.k(NOTE_DISMISSED), self.k(i as u32));
            let p = &mut self.p;
            p.op(AND, &[R0, visible, bit]);
            p.branch(BEQ, R0, zero, next);
            p.op(AND, &[active, active, not]);
            p.op(AND, &[timed, timed, not]);
            p.op(NOTIFY, &[k_note, k_i]);
            p.jump(JMP, "rearrange");
            p.label(next);
        }
        self.p.jump(JMP, "handled");

        self.p.label("double");
        for &i in &dismissable {
            let (bit, next, k_note, k_i) =
                (self.bit(i), self.p.fresh("double_next"), self.k(NOTE_RESTORED), self.k(i as u32));
            let timeout = (self.layout.tiles[i].timeout * 1e6).round() as u32;
            let (k_timeout, expires) = (self.k(timeout), self.t[i].expires);
            let p = &mut self.p;
            p.op(AND, &[R0, active, bit]);
            p.branch(BNE, R0, zero, next);
            p.op(AND, &[R0, filled, bit]);
            p.branch(BEQ, R0, zero, next);
            p.op(OR, &[active, active, bit]);
            if timeout > 0 {
                p.op(OR, &[timed, timed, bit]);
                p.op(ADD, &[expires, moment, k_timeout]);
            }
            p.op(NOTIFY, &[k_note, k_i]);
            p.jump(JMP, "rearrange");
            p.label(next);
        }
        let p = &mut self.p;
        p.jump(JMP, "handled");

        p.label("hold");
        p.op(XOR, &[quiet, quiet, one]);
        p.label("rearrange");
        p.jump(CALL, "arbitrate");
        p.label("handled");
        p.op(ADD, &[present_at, moment, show]);
        p.jump(CALL, "present");
        p.op(END, &[]);
    }

    /// A tick: the next second, the brightness, tiles whose time is up, the clocks and dates, messages to the
    /// other arm, then the panel.
    fn step(&mut self) {
        let v = &self.v;
        let (tick, sod, day, count, now_tick, active, visible, timed, drawn) =
            (v.tick, v.sod, v.day, v.count, v.now_tick, v.active, v.visible, v.timed, v.drawn);
        let (batt_pending, last_send, present_at, tilt_scene) =
            (v.batt_pending, v.last_send, v.present_at, v.tilt_scene);
        let [zero, one, period, skip, k_day, k60, quiet_k, k101, unknown] =
            [0, 1, PERIOD, SKIP_AFTER, DAY, 60, QUIET, 101, UNKNOWN_PCT].map(|x| self.k(x));
        let n = self.tiles();
        let right = self.p.right();
        let side_msg = self.k(MSG_BATTERY | if right { 0 } else { MSG_LEFT });
        let p = &mut self.p;
        p.label("step");
        p.op(MOV, &[now_tick, tick]);
        p.op(ADD, &[tick, tick, period]);
        // Seconds behind (the arm was kept busy): skip them.
        p.read(R0, prop::TIME_US);
        p.op(SUB, &[R0, R0, tick]);
        p.branch(BLTS, R0, skip, "on_time");
        p.op(DIVU, &[R1, R0, period]);
        p.op(MUL, &[R2, R1, period]);
        p.op(ADD, &[tick, tick, R2]);
        p.op(ADD, &[sod, sod, R1]);
        p.op(MODU, &[sod, sod, k_day]);
        p.label("on_time");
        p.timer(tick);
        p.op(ADD, &[sod, sod, one]);
        p.branch(BNE, sod, k_day, "same_day");
        p.op(MOV, &[sod, zero]);
        p.op(ADD, &[day, day, one]);
        p.label("same_day");
        p.op(ADD, &[count, count, one]);
        p.brightness(self.layout.boost);

        for i in 0..n {
            let (bit, not, next, note, k_i, expires) = (
                self.bit(i),
                self.not_bit(i),
                self.p.fresh("expiry"),
                self.k(NOTE_EXPIRED),
                self.k(i as u32),
                self.t[i].expires,
            );
            let p = &mut self.p;
            p.op(AND, &[R0, timed, bit]);
            p.branch(BEQ, R0, zero, next);
            p.op(SUB, &[R0, now_tick, expires]);
            p.branch(BLTS, R0, zero, next);
            p.op(AND, &[active, active, not]);
            p.op(AND, &[timed, timed, not]);
            p.op(NOTIFY, &[note, k_i]);
            p.label(next);
        }
        self.p.jump(CALL, "arbitrate");

        // Clocks when what they show changes, dates when the day does.
        for i in 0..n {
            let tile = &self.layout.tiles[i];
            if !matches!(tile.kind, Kind::Clock | Kind::Date) {
                continue;
            }
            let (kind, seconds) = (tile.kind, tile.seconds);
            let (bit, next, draw, shown) = (self.bit(i), self.p.fresh("tick_next"), self.t[i].draw, self.t[i].shown);
            let p = &mut self.p;
            p.op(AND, &[R0, visible, bit]);
            p.branch(BEQ, R0, zero, next);
            p.op(AND, &[R0, drawn, bit]);
            p.branch(BNE, R0, zero, next);
            match kind {
                Kind::Clock if seconds => p.op(MOV, &[R0, sod]),
                Kind::Clock => p.op(DIVU, &[R0, sod, k60]),
                _ => p.op(MOV, &[R0, day]),
            }
            p.branch(BEQ, R0, shown, next);
            p.jump(CALL, draw);
            p.label(next);
        }

        // This arm's battery to both arms, when it changed and nothing else is on its way.
        let p = &mut self.p;
        p.branch(BEQ, batt_pending, zero, "batt_sent");
        p.read(R0, prop::TIME_US);
        p.op(SUB, &[R0, R0, last_send]);
        p.branch(BLTU, R0, quiet_k, "batt_sent");
        p.read(R2, prop::BATT_PCT);
        p.branch(BLTU, R2, k101, "batt_known");
        p.op(MOV, &[R2, unknown]);
        p.label("batt_known");
        p.op(OR, &[R2, R2, side_msg]);
        p.op(MOV, &[batt_pending, zero]);
        self.broadcast();
        self.p.label("batt_sent");

        // On the right, once a minute: this tick and the scene to the left.
        if right {
            let [every, at, limit, sync, shift] =
                [SYNC_EVERY, SYNC_AT, PHASE_MASK, MSG_SYNC, SYNC_SCENE_SHIFT].map(|x| self.k(x));
            let p = &mut self.p;
            p.op(MODU, &[R0, count, every]);
            p.branch(BNE, R0, at, "sync_sent");
            p.read(R0, prop::TIME_US);
            p.op(SUB, &[R1, R0, last_send]);
            p.branch(BLTU, R1, quiet_k, "sync_sent");
            p.op(SUB, &[R2, R0, now_tick]);
            p.branch(BGEU, R2, limit, "sync_sent");
            p.op(OR, &[R2, R2, sync]);
            p.op(SHL, &[R1, tilt_scene, shift]);
            p.op(OR, &[R2, R2, R1]);
            p.jump(CALL, "send");
            p.label("sync_sent");
        }
        let p = &mut self.p;
        p.op(MOV, &[present_at, now_tick]);
        p.jump(CALL, "present");
        p.op(RET, &[]);
    }

    /// Works out which tiles show from those on and in the scene; clears the rectangles of those that changed and
    /// draws again every tile that shows and overlaps one (or is new). `drawn` gets the tiles drawn.
    fn arbitrate(&mut self) {
        let v = &self.v;
        let (active, visible, changed, drawn, in_case, quiet, scene) =
            (v.active, v.visible, v.changed, v.drawn, v.in_case, v.quiet, v.scene);
        let [zero, scenes] = [0, self.scenes].map(|x| self.k(x));
        let n = self.tiles();
        let p = &mut self.p;
        p.label("arbitrate");
        p.op(MOV, &[R0, zero]);
        p.op(OR, &[R1, in_case, quiet]);
        p.branch(BNE, R1, zero, "arb_have");
        p.op(LD32, &[R2, scenes, scene]);
        p.op(AND, &[R2, R2, active]);
        for i in 0..n {
            let (bit, next, above) = (self.bit(i), self.p.fresh("arb_next"), self.geo.above[i]);
            let k_above = self.k(above);
            let p = &mut self.p;
            p.op(AND, &[R1, R2, bit]);
            p.branch(BEQ, R1, zero, next);
            if above != 0 {
                p.op(AND, &[R1, R0, k_above]);
                p.branch(BNE, R1, zero, next);
            }
            p.op(OR, &[R0, R0, bit]);
            p.label(next);
        }
        let p = &mut self.p;
        p.label("arb_have");
        p.op(MOV, &[drawn, zero]);
        p.op(XOR, &[R1, R0, visible]);
        p.branch(BEQ, R1, zero, "arb_end");
        p.op(MOV, &[visible, R0]);
        p.op(MOV, &[changed, R1]);
        for i in 0..n {
            let (bit, next) = (self.bit(i), self.p.fresh("clear_next"));
            let rect = on_panel(self.rect(i));
            let p = &mut self.p;
            p.op(AND, &[R1, changed, bit]);
            p.branch(BEQ, R1, zero, next);
            if let Some([x, y, w, h]) = rect {
                p.fill_k(x, y, w, h, 0);
                self.mark_k([x, y, w, h]);
            }
            self.p.label(next);
        }
        for i in 0..n {
            let (bit, near, next, draw) =
                (self.bit(i), self.k(self.geo.touch[i] | 1 << i), self.p.fresh("redraw_next"), self.t[i].draw);
            let p = &mut self.p;
            p.op(AND, &[R1, visible, bit]);
            p.branch(BEQ, R1, zero, next);
            p.op(AND, &[R1, changed, near]);
            p.branch(BEQ, R1, zero, next);
            p.jump(CALL, draw);
            p.op(OR, &[drawn, drawn, bit]);
            p.label(next);
        }
        let p = &mut self.p;
        p.label("arb_end");
        p.op(RET, &[]);
    }

    /// The panel at `present_at`: off with nothing showing (standby, or off in the case or when hidden), the whole
    /// frame when it was dark, else the rectangle drawn since.
    fn present(&mut self) {
        let v = &self.v;
        let (visible, lit, present_at, in_case, quiet) = (v.visible, v.lit, v.present_at, v.in_case, v.quiet);
        let [dx0, dy0, dx1, dy1] = v.dirty;
        let [zero, one, two, width, height] = [0, 1, 2, v5::WIDTH, v5::HEIGHT].map(|x| self.k(x));
        let p = &mut self.p;
        p.label("present");
        p.branch(BNE, visible, zero, "present_show");
        p.branch(BEQ, lit, zero, "present_reset");
        p.op(MOV, &[lit, zero]);
        p.op(OR, &[R0, in_case, quiet]);
        p.branch(BNE, R0, zero, "present_off");
        p.set(prop::PANEL, two);
        p.jump(JMP, "present_reset");
        p.label("present_off");
        p.set(prop::PANEL, zero);
        p.jump(JMP, "present_reset");
        p.label("present_show");
        p.branch(BEQ, lit, one, "present_part");
        p.op(SLEEP_UNTIL, &[present_at]);
        p.draw_k(0, 0, v5::WIDTH as i32, v5::HEIGHT as i32);
        p.op(MOV, &[lit, one]);
        p.jump(JMP, "present_reset");
        p.label("present_part");
        p.branch(BGES, dx0, dx1, "present_reset");
        p.branch(BGES, dy0, dy1, "present_reset");
        p.op(SLEEP_UNTIL, &[present_at]);
        p.op(SUB, &[R0, dx1, dx0]);
        p.op(SUB, &[R1, dy1, dy0]);
        p.draw(dx0, dy0, R0, R1);
        p.label("present_reset");
        p.op(MOV, &[dx0, width]);
        p.op(MOV, &[dy0, height]);
        p.op(MOV, &[dx1, zero]);
        p.op(MOV, &[dy1, zero]);
        p.op(RET, &[]);
    }

    /// The Mac's time and tiles, those whose generation changed. `updated` gets the tiles taken.
    fn apply_host(&mut self) {
        let v = &self.v;
        let (time_seen, sod, tick, day, moment, updated, filled, active, timed) =
            (v.time_seen, v.sod, v.tick, v.day, v.moment, v.updated, v.filled, v.active, v.timed);
        let (generation, ts_sod, ts_tick) = (self.time, self.time + 1, self.time + 2);
        let [zero, keep, unshown] = [0, WANT_KEEP, u32::MAX].map(|x| self.k(x));
        let n = self.tiles();
        let p = &mut self.p;
        p.label("apply_host");
        p.branch(BEQ, generation, time_seen, "time_kept");
        p.op(MOV, &[time_seen, generation]);
        p.op(MOV, &[sod, ts_sod]);
        p.op(MOV, &[tick, ts_tick]);
        p.timer(tick);
        p.op(MOV, &[day, zero]);
        for i in 0..n {
            if matches!(self.layout.tiles[i].kind, Kind::Clock | Kind::Date) {
                let shown = self.t[i].shown;
                self.p.op(MOV, &[shown, unshown]);
            }
        }
        let p = &mut self.p;
        p.label("time_kept");
        p.op(MOV, &[updated, zero]);
        for i in 0..n {
            let [generation, want, dur] = self.host_words(i);
            let (bit, not, seen, expires) = (self.bit(i), self.not_bit(i), self.t[i].seen, self.t[i].expires);
            let (next, off) = (self.p.fresh("take_next"), self.p.fresh("take_off"));
            let p = &mut self.p;
            p.branch(BEQ, generation, seen, next);
            p.op(MOV, &[seen, generation]);
            p.op(OR, &[updated, updated, bit]);
            p.op(OR, &[filled, filled, bit]);
            p.branch(BEQ, want, keep, next);
            p.op(AND, &[timed, timed, not]);
            p.branch(BEQ, want, zero, off);
            p.op(OR, &[active, active, bit]);
            p.branch(BEQ, dur, zero, next);
            p.op(OR, &[timed, timed, bit]);
            p.op(ADD, &[expires, moment, dur]);
            p.jump(JMP, next);
            p.label(off);
            p.op(AND, &[active, active, not]);
            p.label(next);
        }
        self.p.op(RET, &[]);
    }

    /// Small routines: the ticks due by `moment`, the dirty rectangle, sending to the other arm, the case.
    fn routines(&mut self) {
        let v = &self.v;
        let (moment, tick, last_send, in_case) = (v.moment, v.tick, v.last_send, v.in_case);
        let [dx0, dy0, dx1, dy1] = v.dirty;
        let [zero, one] = [0, 1].map(|x| self.k(x));
        let p = &mut self.p;

        p.label("catch_up");
        p.op(SUB, &[R0, moment, tick]);
        p.branch(BLTS, R0, zero, "caught_up");
        p.jump(CALL, "step");
        p.jump(JMP, "catch_up");
        p.label("caught_up");
        p.op(RET, &[]);

        // R8–R11: x0, y0, x1, y1 to add to the dirty rectangle.
        p.label("mark");
        for (reg, word, keep) in [(R8, dx0, "mark_1"), (R9, dy0, "mark_2")] {
            p.branch(BGES, reg, word, keep);
            p.op(MOV, &[word, reg]);
            p.label(keep);
        }
        for (reg, word, keep) in [(R10, dx1, "mark_3"), (R11, dy1, "mark_4")] {
            p.branch(BGES, word, reg, keep);
            p.op(MOV, &[word, reg]);
            p.label(keep);
        }
        p.op(RET, &[]);

        // R2 to the other arm. Uses R3.
        p.label("send");
        p.read(R3, prop::PEER_LINK);
        p.branch(BEQ, R3, zero, "send_end");
        p.set(prop::PEER_SEND, R2);
        p.read(last_send, prop::TIME_US);
        p.label("send_end");
        p.op(RET, &[]);

        p.label("read_case");
        p.read(R0, prop::IN_CASE);
        p.op(MOV, &[in_case, zero]);
        p.branch(BNE, R0, one, "read_case_end");
        p.op(MOV, &[in_case, one]);
        p.label("read_case_end");
        p.op(RET, &[]);
    }

    /// Tile `i`'s routine: its rectangle cleared and drawn, and added to the dirty one.
    fn draw_tile(&mut self, i: usize) {
        let label = self.t[i].draw;
        self.p.label(label);
        let tile = self.layout.tiles[i].clone();
        if let Some(rect) = on_panel(self.rect(i)) {
            let [x, y, w, h] = rect;
            self.p.fill_k(x, y, w, h, 0);
            if tile.border {
                let [x, y, w, h] = self.rect(i);
                for [bx, by, bw, bh] in [[x, y, w, 1], [x, y + h - 1, w, 1], [x, y, 1, h], [x + w - 1, y, 1, h]] {
                    self.p.fill_k(bx, by, bw, bh, BORDER_LEVEL);
                }
            }
            match tile.kind {
                Kind::Clock => self.clock(i),
                Kind::Date => self.date(i),
                Kind::Battery if tile.both => self.both_batteries(i),
                Kind::Battery | Kind::Weather | Kind::Steps | Kind::DeviceBattery | Kind::HeartRate => self.row(i),
                Kind::Text => self.text(i),
                Kind::Notification => self.notification(i),
                Kind::NotificationList => self.list(i),
                Kind::Navigation if self.geo.plans[i].map => self.map(i),
                Kind::Navigation => self.next_turn(i),
            }
            self.mark_k(rect);
        } else if tile.kind == Kind::Date {
            let (shown, day) = (self.t[i].shown, self.v.day);
            self.p.op(MOV, &[shown, day]);
        }
        self.p.op(RET, &[]);
    }

    /// A one-line tile's left edge and top.
    fn row_origin(&self, i: usize) -> (i32, i32) {
        let plan = &self.geo.plans[i];
        let [ix, iy, iw, ih] = self.inner(i);
        let size = self.metrics(i, plan.scale).height();
        (aligned(self.layout.tiles[i].align, ix, iw, plan.width), iy + (ih - size) / 2)
    }

    /// A one-line tile's baseline, from its top.
    fn baseline(&self, i: usize, top: i32) -> i32 {
        top + self.metrics(i, self.geo.plans[i].scale).ascent
    }

    fn clock(&mut self, i: usize) {
        let tile = self.layout.tiles[i].clone();
        let (s, chars) = (self.geo.plans[i].scale, self.geo.plans[i].cols as i32);
        let (x, top) = self.row_origin(i);
        let (sod, shown, buf) = (self.v.sod, self.t[i].shown, self.t[i].buf);
        let [zero, k3600, k60, k12] = [0, 3600, 60, 12].map(|x| self.k(x));
        let [a, pm, b, pos] = [glyph('A'), glyph('P'), buf, if tile.seconds { 9 } else { 6 }].map(|x| self.k(x));
        let (twelve_ok, am) = (self.p.fresh("twelve"), self.p.fresh("am"));
        let p = &mut self.p;
        p.op(DIVU, &[R0, sod, k3600]);
        if tile.h24 {
            p.digits(R0, buf, 0, 2);
        } else {
            p.op(MODU, &[R1, R0, k12]);
            p.branch(BNE, R1, zero, twelve_ok);
            p.op(MOV, &[R1, k12]);
            p.label(twelve_ok);
            p.digits_spaced(R1, buf, 0, 2);
            p.op(MOV, &[R2, a]);
            p.branch(BLTU, R0, k12, am);
            p.op(MOV, &[R2, pm]);
            p.label(am);
            p.op(ST8, &[b, pos, R2]);
        }
        p.op(DIVU, &[R3, sod, k60]);
        p.op(MODU, &[R3, R3, k60]);
        p.digits(R3, buf, 3, 2);
        if tile.seconds {
            p.op(MODU, &[R3, sod, k60]);
            p.digits(R3, buf, 6, 2);
            p.op(MOV, &[shown, sod]);
        } else {
            p.op(DIVU, &[shown, sod, k60]);
        }
        let [cx, base, n, level] = [x, self.baseline(i, top), chars, tile.level as i32].map(|v| self.ki(v));
        self.glyphs(i, s, cx, base, b, n, level);
    }

    fn date(&mut self, i: usize) {
        let tile = self.layout.tiles[i].clone();
        let plan = self.geo.plans[i].clone();
        let s = plan.scale;
        let [ix, iy, iw, ih] = self.inner(i);
        let size = self.metrics(i, s).height();
        let top = iy + (ih - size) / 2;
        let (day, shown) = (self.v.day, self.t[i].shown);
        let map = &self.map.tiles[i];
        let (lens, text) = (map.lens, map.lines[0].0);
        let [zero, dates, k_lens, cols, k_text] = [0, DATES as u32, lens, plan.cols as u32, text].map(|x| self.k(x));
        let [base, level] = [self.baseline(i, top), tile.level as i32].map(|v| self.ki(v));
        let none = self.p.fresh("no_date");
        let p = &mut self.p;
        p.branch(BGEU, day, dates, none);
        p.op(LD8, &[R0, k_lens, day]);
        p.branch(BEQ, R0, zero, none);
        p.op(MUL, &[R1, day, cols]);
        p.op(ADD, &[R1, R1, k_text]);
        self.line_x(i, R2, R0, s, tile.align, ix, iw);
        self.glyphs(i, s, R2, base, R1, R0, level);
        self.p.label(none);
        self.p.op(MOV, &[shown, day]);
    }

    /// "L 87% R 85%", the glasses' arms.
    fn both_batteries(&mut self, i: usize) {
        let s = self.geo.plans[i].scale;
        let (x, top) = self.row_origin(i);
        let (batt_l, batt_r, buf) = (self.v.batt_l, self.v.batt_r, self.t[i].buf);
        let [k101, b, space, dash] = [101, buf, glyph(' '), glyph('-')].map(|v| self.k(v));
        for (word, at) in [(batt_l, 1u32), (batt_r, 7)] {
            let (unknown, done) = (self.p.fresh("batt_unknown"), self.p.fresh("batt_done"));
            let [p0, p1, p2] = [at, at + 1, at + 2].map(|v| self.k(v));
            let p = &mut self.p;
            p.branch(BGEU, word, k101, unknown);
            p.digits_spaced(word, buf, at, 3);
            p.jump(JMP, done);
            p.label(unknown);
            p.op(ST8, &[b, p0, space]);
            p.op(ST8, &[b, p1, dash]);
            p.op(ST8, &[b, p2, dash]);
            p.label(done);
        }
        let [cx, base, n, level] =
            [x, self.baseline(i, top), 11, self.layout.tiles[i].level as i32].map(|v| self.ki(v));
        self.glyphs(i, s, cx, base, b, n, level);
    }

    /// A one-line tile, part by part (`Tile::parts`).
    fn row(&mut self, i: usize) {
        let tile = self.layout.tiles[i].clone();
        let s = self.geo.plans[i].scale;
        let m = self.metrics(i, s);
        let (mut x, top) = self.row_origin(i);
        let level = self.ki(tile.level as i32);
        let base = self.ki(self.baseline(i, top));
        let parts = tile.parts();
        let mut meter_x = None;
        for part in parts {
            match part {
                Part::Icon(set) => {
                    let pick = match set {
                        IconSet::Weather => Pick::Line(0),
                        IconSet::Bolt => Pick::WhileCharging,
                        _ => Pick::First,
                    };
                    self.icon(i, set, m.height() as u32, pick, x, top);
                }
                Part::Line(n, _) => {
                    let (at, _) = self.map.tiles[i].lines[n];
                    let [zero, lens, index, text, cx] =
                        [0, self.map.tiles[i].lens, n as u32, at, x as u32].map(|v| self.k(v));
                    let skip = self.p.fresh("row_line");
                    self.p.op(LD8, &[R0, lens, index]);
                    self.p.branch(BEQ, R0, zero, skip);
                    self.glyphs(i, s, cx, base, text, R0, level);
                    self.p.label(skip);
                }
                Part::Label(text) => {
                    let [cx, at, len] = [x as u32, self.t[i].label, text.len() as u32].map(|v| self.k(v));
                    self.glyphs(i, s, cx, base, at, len, level);
                }
                Part::Meter => meter_x = Some(x),
                Part::Percent => {
                    let meter = meter_x.take().expect("a meter before its percentage");
                    self.meter(i, meter, x, top);
                }
                Part::Gap(_) => {}
            }
            x += part_width(part, m);
        }
    }

    /// A battery: its outline at `meter`, a bar as full as the charge and the charge as text at `percent`. The
    /// glasses' (both arms averaged) or the Mac's (the tile's first value). In the 8 × 8 font's cell.
    fn meter(&mut self, i: usize, meter: i32, percent: i32, top: i32) {
        let tile = self.layout.tiles[i].clone();
        let s = self.geo.plans[i].scale;
        let (x, buf) = (meter, self.t[i].buf);
        let [one, k101, b, space, dash, hundred] = [1, 101, buf, glyph(' '), glyph('-'), 100].map(|v| self.k(v));
        let [base, level] = [self.baseline(i, top), tile.level as i32].map(|v| self.ki(v));
        // Drawn in eighths of the font's height (exact for the 8 × 8 font): a body 13 wide and 6 high, its knob.
        let height = self.metrics(i, s).height();
        let e = |k: i32| (height * k + 4) / 8;
        let t = ((e(1) + 1) / 2).max(1);
        let outline = |c: &mut Self| {
            c.p.fill_k(x, top + e(1), e(13), e(6), tile.level);
            c.p.fill_k(x + t, top + e(1) + t, e(13) - 2 * t, e(6) - 2 * t, 0);
            c.p.fill_k(x + e(13), top + e(3), e(1), e(2), tile.level);
        };
        let bar = [x + 2 * t, top + e(1) + 2 * t, e(13) - 4 * t, e(6) - 4 * t];
        let [bx, by, bw, bh] = bar.map(|v| self.ki(v));
        let (left_unknown, have, unknown, text) =
            (self.p.fresh("l_unknown"), self.p.fresh("have"), self.p.fresh("unknown"), self.p.fresh("text"));
        let [p0, p1, p2] = [0, 1, 2].map(|v| self.k(v));
        if tile.kind == Kind::Battery {
            let (batt_l, batt_r) = (self.v.batt_l, self.v.batt_r);
            let p = &mut self.p;
            p.op(MOV, &[R0, batt_l]);
            p.op(MOV, &[R1, batt_r]);
            p.branch(BGEU, R0, k101, left_unknown);
            p.branch(BGEU, R1, k101, have);
            p.op(ADD, &[R0, R0, R1]);
            p.op(ADD, &[R0, R0, one]);
            p.op(SHR, &[R0, R0, one]);
            p.jump(JMP, have);
            p.label(left_unknown);
            p.op(MOV, &[R0, R1]);
        } else {
            let value = self.t[i].values;
            self.p.op(MOV, &[R0, value]);
            self.p.label(left_unknown);
        }
        let p = &mut self.p;
        p.branch(BGEU, R0, k101, unknown);
        p.label(have);
        p.digits_spaced(R0, buf, 0, 3);
        outline(self);
        let p = &mut self.p;
        p.op(MUL, &[R2, R0, bw]);
        p.op(DIVU, &[R2, R2, hundred]);
        p.fill(bx, by, R2, bh, level);
        p.jump(JMP, text);
        p.label(unknown);
        p.op(ST8, &[b, p0, space]);
        p.op(ST8, &[b, p1, dash]);
        p.op(ST8, &[b, p2, dash]);
        outline(self);
        self.p.label(text);
        let [cx, n] = [percent, 4].map(|v| self.ki(v));
        self.glyphs(i, s, cx, base, b, n, level);
    }

    /// `count` lines of the tile's buffer from line `first`, at `scale`, the first's top at `top`, each after its
    /// icon (with `icons`), in the inner box's width or `span` (x, w).
    #[allow(clippy::too_many_arguments)]
    fn lines(
        &mut self,
        i: usize,
        first: usize,
        count: usize,
        scale: u8,
        top: i32,
        align: Align,
        icons: bool,
        span: Option<(i32, i32)>,
    ) {
        let [ix, _, iw, _] = self.inner(i);
        let (ix, iw) = span.unwrap_or((ix, iw));
        let m = self.metrics(i, scale);
        // An icon a character high, and the gap between lines after it.
        let indent = if icons { m.height() + m.gap() } else { 0 };
        let lens = self.map.tiles[i].lens;
        let lines = self.map.tiles[i].lines.clone();
        let [zero, k_lens] = [0, lens].map(|v| self.k(v));
        let level = self.ki(self.layout.tiles[i].level as i32);
        for j in 0..count {
            let (at, _) = lines[first + j];
            let skip = self.p.fresh("line_skip");
            let [index, text] = [(first + j) as u32, at].map(|v| self.k(v));
            let row_top = top + j as i32 * m.pitch;
            let base = self.ki(row_top + m.ascent);
            self.p.op(LD8, &[R0, k_lens, index]);
            self.p.branch(BEQ, R0, zero, skip);
            if icons {
                self.icon(i, IconSet::Category, m.height() as u32, Pick::Line(first + j), ix, row_top);
            }
            self.line_x(i, R1, R0, scale, align, ix + indent, iw - indent);
            self.glyphs(i, scale, R1, base, text, R0, level);
            self.p.label(skip);
        }
    }

    fn text(&mut self, i: usize) {
        let plan = self.geo.plans[i].clone();
        let [_, iy, _, _] = self.inner(i);
        self.lines(i, 0, plan.rows, plan.scale, iy, self.layout.tiles[i].align, false, None);
    }

    fn list(&mut self, i: usize) {
        let plan = self.geo.plans[i].clone();
        let [_, iy, _, _] = self.inner(i);
        let tile = &self.layout.tiles[i];
        let (align, icons) = (tile.align, tile.icon);
        self.lines(i, 0, plan.rows, plan.scale, iy, align, icons, None);
    }

    fn notification(&mut self, i: usize) {
        let plan = self.geo.plans[i].clone();
        let tile = self.layout.tiles[i].clone();
        let [ix, iy, iw, ih] = self.inner(i);
        if tile.single_line {
            let top = iy + (ih - self.metrics(i, plan.scale).height()) / 2;
            self.lines(i, 0, 1, plan.scale, top, tile.align, tile.icon, None);
            return;
        }
        // The title, a rule halfway down the gap under it, then the body.
        let t = self.metrics(i, plan.title_scale);
        self.lines(i, 0, 1, plan.title_scale, iy, Align::Left, tile.icon, None);
        self.p.fill_k(ix, iy + t.height() + t.gap() / 2 + 1, iw, 1, RULE_LEVEL);
        self.lines(i, 1, plan.rows, plan.scale, iy + t.height() + t.gap() + 3, tile.align, false, None);
    }

    /// The next turn: its arrow, then the distance over the street, or the two on one line.
    fn next_turn(&mut self, i: usize) {
        let plan = self.geo.plans[i].clone();
        let align = self.layout.tiles[i].align;
        let [ix, iy, iw, ih] = self.inner(i);
        let arrow = plan.icon as i32;
        self.icon(i, IconSet::Turn, plan.icon, Pick::First, ix, iy + (ih - arrow) / 2);
        let x = ix + arrow + (arrow / 6).max(4);
        let span = Some((x, ix + iw - x));
        let s = self.metrics(i, plan.scale).height();
        if plan.rows == 2 {
            let d = self.metrics(i, plan.title_scale).height();
            let top = iy + (ih - (d + 3 + s)) / 2;
            self.lines(i, 0, 1, plan.title_scale, top, align, false, span);
            self.lines(i, 1, 1, plan.scale, top + d + 3, align, false, span);
        } else {
            self.lines(i, 0, 1, plan.scale, iy + (ih - s) / 2, align, false, span);
        }
    }

    /// A map: a round one's edge, the roads, the route over them, and the position's arrow with a dark edge.
    fn map(&mut self, i: usize) {
        let plan = self.geo.plans[i].clone();
        let tile = self.layout.tiles[i].clone();
        let view = tile.map_view();
        let [ix, iy, _, _] = self.inner(i);
        if let Some((cx, cy, r)) = view.circle {
            let n = ((std::f64::consts::TAU * r as f64 / 10.0).round() as usize).clamp(24, 96);
            let at = |k: usize| {
                let a = std::f64::consts::TAU * k as f64 / n as f64;
                [ix + cx + (r as f64 * a.cos()).round() as i32, iy + cy + (r as f64 * a.sin()).round() as i32]
            };
            let level = self.k(RING_LEVEL.min(tile.level) as u32);
            for k in 0..n {
                let [x0, y0, x1, y1] = [at(k)[0], at(k)[1], at(k + 1)[0], at(k + 1)[1]].map(|v| self.ki(v));
                self.p.op(LINE, &[W_FRAME, W_STRIDE, x0, y0, x1, y1, level]);
            }
        }
        let points = self.map.tiles[i].points;
        let roads = (tile.level as u32 * 2 / 5).max(1);
        self.polyline(i, 1, points + 4 + 4 * MAX_ROUTE as u32, MAX_ROADS, roads, false);
        self.polyline(i, 0, points + 4, MAX_ROUTE, tile.level as u32, true);
        let size = plan.icon;
        let (px, py) = (ix + view.origin.0 - size as i32 / 2, iy + view.origin.1 - size as i32 / 2);
        let [src, stride, zero, n, erase, full] =
            [self.geo.icons_at(IconSet::Position, size), size / 2, 0, size, ERASE_MODE, 15].map(|v| self.k(v));
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let [x, y] = [px + dx, py + dy].map(|v| self.ki(v));
            self.p.op(BLIT, &[src, stride, zero, zero, W_FRAME, W_STRIDE, x, y, n, n, erase, full]);
        }
        self.icon(i, IconSet::Position, size, Pick::First, px, py);
    }

    /// The lines of a map's points at `at` (the route's, `which` 0, or the roads', 1, of at most `max`), 3 px wide
    /// when `thick`, from the inner box's corner. A segment with an end outside the box (or `nav::BREAK`) is
    /// skipped, so nothing draws outside it. Uses R0–R6.
    fn polyline(&mut self, i: usize, which: u32, at: u32, max: usize, level: u32, thick: bool) {
        let [ix, iy, iw, ih] = self.inner(i);
        let counts = self.map.tiles[i].points;
        let [k_counts, k_which, k_at, k_max, zero, one, two, k_level] =
            [counts, which, at, max as u32, 0, 1, 2, level].map(|v| self.k(v));
        let [k_ix, k_iy, k_iw, k_ih] = [ix, iy, iw, ih].map(|v| self.ki(v));
        let (counted, top, next, done) =
            (self.p.fresh("counted"), self.p.fresh("segment"), self.p.fresh("segment_next"), self.p.fresh("segments"));
        let p = &mut self.p;
        p.op(LD16, &[R6, k_counts, k_which]);
        p.branch(BLTU, R6, k_max, counted);
        p.op(MOV, &[R6, k_max]);
        p.label(counted);
        p.op(MOV, &[R5, zero]);
        p.label(top);
        p.op(ADD, &[R0, R5, one]);
        p.branch(BGEU, R0, R6, done);
        // R2, R3 to R4, R1: the segment from point R5 to the next.
        p.op(SHL, &[R1, R5, one]);
        p.op(LD16, &[R2, k_at, R1]);
        p.op(ADD, &[R1, R1, one]);
        p.op(LD16, &[R3, k_at, R1]);
        p.op(ADD, &[R1, R1, one]);
        p.op(LD16, &[R4, k_at, R1]);
        p.op(ADD, &[R1, R1, one]);
        p.op(LD16, &[R1, k_at, R1]);
        for (v, limit) in [(R2, k_iw), (R3, k_ih), (R4, k_iw), (R1, k_ih)] {
            p.branch(BGEU, v, limit, next);
        }
        p.op(ADD, &[R2, R2, k_ix]);
        p.op(ADD, &[R3, R3, k_iy]);
        p.op(ADD, &[R4, R4, k_ix]);
        p.op(ADD, &[R1, R1, k_iy]);
        let line = |p: &mut Prog| p.op(LINE, &[W_FRAME, W_STRIDE, R2, R3, R4, R1, k_level]);
        line(p);
        if thick {
            // Again a pixel right and left, then down and up.
            for (op, by) in [(ADD, one), (SUB, two)] {
                p.op(op, &[R2, R2, by]);
                p.op(op, &[R4, R4, by]);
                line(p);
            }
            p.op(ADD, &[R2, R2, one]);
            p.op(ADD, &[R4, R4, one]);
            for (op, by) in [(ADD, one), (SUB, two)] {
                p.op(op, &[R3, R3, by]);
                p.op(op, &[R1, R1, by]);
                line(p);
            }
        }
        p.label(next);
        p.op(MOV, &[R5, R0]);
        p.jump(JMP, top);
        p.label(done);
    }
}

/// The shell for both arms, from a checked layout.
pub(crate) struct Shell {
    pub layout: Layout,
    pub geo: Geometry,
    pub left: Built,
    pub right: Built,
}

impl Shell {
    pub fn new(layout: Layout) -> Result<Shell, String> {
        let layout = layout.settle();
        let geo = layout.check()?;
        let left = build(&layout, &geo, Side::Left)?;
        let right = build(&layout, &geo, Side::Right)?;
        // The fonts at the top of the code's area start above the code.
        let end = left.program.code_end().max(right.program.code_end());
        if end > geo.low {
            return Err(geo.too_big(end));
        }
        assert_eq!(left.map, right.map, "both arms' programs keep the Mac's words in the same places");
        Ok(Shell { layout, geo, left, right })
    }

    pub fn built(&self, side: Side) -> &Built {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub fn map(&self) -> &Map {
        &self.right.map
    }

    /// The fonts and icons, each with its place in the arena.
    pub fn assets(&self) -> Vec<(u32, Vec<u8>)> {
        let fonts = self.geo.fonts.iter().map(|&(face, scale, emoji, at)| (at, fonts::blob(face, scale, emoji)));
        let icons = self.geo.icons.iter().map(|&((set, size), at)| (at, set.render(size)));
        fonts.chain(icons).collect()
    }

    /// What the arm on `side` holds when it runs this shell: "ERSH", a checksum of the program and assets, the side.
    pub fn signature(&self, side: Side) -> [u8; 16] {
        let mut bytes = Vec::new();
        for code in self.built(side).program.writes(0) {
            bytes.extend(code);
        }
        for (at, asset) in self.assets() {
            bytes.extend(at.to_le_bytes());
            bytes.extend(asset);
        }
        let mut sig = [0u8; 16];
        sig[..4].copy_from_slice(b"ERSH");
        sig[4..8].copy_from_slice(&crate::proto::crc32(&bytes).to_le_bytes());
        sig[8] = side as u8;
        sig[9] = 1;
        sig
    }

    /// Tiles of `kind`.
    pub fn of(&self, kind: Kind) -> Vec<usize> {
        (0..self.layout.tiles.len()).filter(|&i| self.layout.tiles[i].kind == kind).collect()
    }

    /// Host code that draws `turn`'s arrow into the next-turn tiles' arrow (one for each size in use); before their
    /// content, so they draw the new one.
    pub fn arrows(&self, turn: nav::Turn) -> Vec<Vec<u8>> {
        let slots = self.geo.icons.iter().filter(|((set, _), _)| *set == IconSet::Turn);
        slots.flat_map(|&((_, size), at)| v5::upload(at, &icons::render(turn.icon(), size), 240)).collect()
    }

    /// `text` for tile `i`, as the Mac sends it: for a notification "TITLE: text" (or just text, titled "Note") with
    /// `icon` (a category), wrapped to fit, or on one line; for a text tile, wrapped. Empty for no text.
    pub fn note(&self, i: usize, text: &str, icon: u8) -> TileData {
        let (tile, plan) = (&self.layout.tiles[i], &self.geo.plans[i]);
        let text = text.trim();
        if text.is_empty() {
            return TileData::default();
        }
        let emoji = tile.emoji(plan.scale);
        match tile.kind {
            Kind::Notification => {
                let (title, body) = match text.split_once(": ") {
                    Some((title, rest)) if title.chars().count() <= plan.title_cols.max(20) => {
                        (title.trim(), rest.trim())
                    }
                    _ => ("", text),
                };
                let lines = if tile.single_line {
                    let line = if title.is_empty() { body.to_string() } else { format!("{title}: {body}") };
                    vec![one_line(&line, plan.cols, emoji)]
                } else {
                    let title = if title.is_empty() { "Note" } else { title };
                    let mut lines = vec![one_line(title, plan.title_cols, tile.emoji(plan.title_scale))];
                    lines.extend(super::layout::wrap(body, plan.cols, plan.rows, emoji));
                    lines
                };
                TileData { lines, icons: vec![icon], ..TileData::default() }
            }
            Kind::Text => {
                TileData { lines: super::layout::wrap(text, plan.cols, plan.rows, emoji), ..TileData::default() }
            }
            _ => TileData::default(),
        }
    }

    /// A notification list's rows, newest first: an icon (a category) and a line each.
    pub fn list(&self, i: usize, items: &[(u8, String)]) -> TileData {
        let plan = &self.geo.plans[i];
        let emoji = self.layout.tiles[i].emoji(plan.scale);
        let rows = &items[..items.len().min(plan.rows)];
        TileData {
            lines: rows.iter().map(|(_, line)| one_line(line, plan.cols, emoji)).collect(),
            icons: rows.iter().map(|(icon, _)| *icon).collect(),
            ..TileData::default()
        }
    }

    /// The weather for tile `i`: its icon, " 12°" and " 65%" (degrees Celsius, shown as the tile's unit says).
    pub fn weather(&self, i: usize, icon: u8, celsius: f64, humidity: f64, fahrenheit_mac: bool) -> TileData {
        let tile = &self.layout.tiles[i];
        let fahrenheit = match tile.unit {
            super::layout::Unit::Auto => fahrenheit_mac,
            super::layout::Unit::C => false,
            super::layout::Unit::F => true,
        };
        let t = if fahrenheit { celsius * 9.0 / 5.0 + 32.0 } else { celsius };
        TileData {
            lines: vec![
                format!("{:>3}°", t.round() as i32),
                format!("{:>3}%", humidity.round().clamp(0.0, 100.0) as i32),
            ],
            icons: vec![icon],
            ..TileData::default()
        }
    }

    /// A step count in six characters, " 6,284" (from 100,000 without the comma).
    pub fn steps(&self, steps: u32) -> TileData {
        let digits = steps.min(999_999).to_string();
        let mut grouped = String::new();
        for (k, c) in digits.chars().enumerate() {
            if k > 0 && (digits.len() - k).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(c);
        }
        let shown = if grouped.len() <= 6 { grouped } else { digits };
        TileData { lines: vec![format!("{shown:>6}")], ..TileData::default() }
    }

    /// The Mac's battery: % (none: unknown) and whether it charges.
    pub fn device(&self, percent: Option<u32>, charging: bool) -> TileData {
        TileData { values: vec![percent.map_or(UNKNOWN_PCT, |p| p.min(100)), charging as u32], ..TileData::default() }
    }

    /// Beats a minute in three characters, " 72".
    pub fn heart(&self, bpm: u32) -> TileData {
        TileData { lines: vec![format!("{:>3}", bpm.min(999))], ..TileData::default() }
    }

    /// A route for navigation tile `i`: the map's lines, or the next turn's arrow, distance (`metric` or not) and
    /// street.
    pub fn navigation(&self, i: usize, route: &nav::Route, metric: bool) -> TileData {
        let (tile, plan) = (&self.layout.tiles[i], &self.geo.plans[i]);
        if plan.map {
            return TileData { map: Some(nav::project(route, &tile.map_view())), ..TileData::default() };
        }
        let distance = nav::distance(route.distance, metric);
        let emoji = tile.emoji(plan.scale);
        let lines = if plan.rows == 2 {
            vec![distance, one_line(&route.street, plan.cols, emoji)]
        } else {
            vec![one_line(&format!("{distance}  {}", route.street), plan.cols, emoji)]
        };
        TileData { lines, ..TileData::default() }
    }

    /// Host code that readies a stopped arm: the drawing words and a clear frame (the assets go separately).
    pub fn prepare(&self) -> Vec<Vec<u8>> {
        let mut words = v5::fixed_args();
        // The main font word: the first font in use (nothing draws with it; GLYPHS name their font).
        let first = self.geo.fonts.first().map_or(v5::FONT, |f| f.3);
        words.extend(v5::write(18 * 4, &first.to_le_bytes()));
        vec![words, v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0)]
    }

    /// Host code that loads the program on a stopped arm, to start at `start` (this arm's clock), and signs it.
    pub fn load(&self, side: Side, start: u32) -> Vec<Vec<u8>> {
        let mut code = self.built(side).program.writes(start);
        code.push(v5::write(SIGNATURE, &self.signature(side)));
        code
    }
}

/// Host code that puts `data` in tile `tile`'s buffers (cut to fit).
pub(crate) fn content(tile: &TileMap, data: &TileData) -> Vec<Vec<u8>> {
    let mut code = Vec::new();
    if !tile.lines.is_empty() {
        let caps: Vec<usize> = tile.lines.iter().map(|l| l.1).collect();
        let icons = tile.icons != 0;
        let buffer = lines_buffer(&caps, icons, data);
        let pad = caps.len().next_multiple_of(4);
        code.push(v5::write(tile.lens, &buffer[..caps.len()]));
        if icons {
            code.push(v5::write(tile.icons, &buffer[pad..pad + caps.len()]));
        }
        let mut at = pad * if icons { 2 } else { 1 };
        for (i, &(addr, cap)) in tile.lines.iter().enumerate() {
            if i < data.lines.len() {
                code.extend(v5::upload(addr, &buffer[at..at + cap], 240));
            }
            at += cap;
        }
    }
    if !data.values.is_empty() {
        let words: Vec<u8> = data.values.iter().take(2).flat_map(|v| v.to_le_bytes()).collect();
        code.push(v5::write(tile.values, &words));
    }
    if let Some(lines) = data.map.as_ref().filter(|_| tile.points != 0) {
        let bytes = |points: &[[u16; 2]], max: usize| -> Vec<u8> {
            points.iter().take(max).flat_map(|p| p.iter().flat_map(|v| v.to_le_bytes())).collect()
        };
        let (route, roads) = (bytes(&lines.route, MAX_ROUTE), bytes(&lines.roads, MAX_ROADS));
        for (at, part) in [(tile.points + 4, &route), (tile.points + 4 + 4 * MAX_ROUTE as u32, &roads)] {
            if !part.is_empty() {
                code.extend(v5::upload(at, part, 240));
            }
        }
        let counts: Vec<u8> =
            [route.len() / 4, roads.len() / 4].iter().flat_map(|&n| (n as u16).to_le_bytes()).collect();
        code.push(v5::write(tile.points, &counts));
    }
    code
}

/// Host code that turns tile `tile` on (for `duration` µs, or until turned off: 0), off, or leaves it (`WANT_*`), as
/// generation `generation`.
pub(crate) fn turn(tile: &TileMap, generation: u32, want: u32, duration: u32) -> Vec<u8> {
    let words: Vec<u8> = [generation, want, duration].iter().flat_map(|v| v.to_le_bytes()).collect();
    v5::write(tile.host, &words)
}

/// Host code that sets the time: `sod` the second of the day shown until the tick at `tick` (this arm's clock).
pub(crate) fn time(map: &Map, generation: u32, sod: u32, tick: u32) -> Vec<u8> {
    let words: Vec<u8> = [generation, sod, tick].iter().flat_map(|v| v.to_le_bytes()).collect();
    v5::write(map.time, &words)
}

/// Host code that has the program take what was written, at `due` (this arm's clock).
pub(crate) fn raise(map: &Map, due: u32) -> Vec<Vec<u8>> {
    let mut code = v5::write(map.due, &due.to_le_bytes());
    code.extend(v5::write(SCRATCH, &(HOST_TRIGGER as u32).to_le_bytes()));
    code.extend(v5::encode(RAISE, &[(SCRATCH / 4) as u16], &[]));
    vec![code]
}

/// The trigger table's place, for `VM_START`.
pub(crate) const TRIGGERS: u32 = TABLE;

#[cfg(test)]
mod tests {
    use super::super::layout::{Layout, Scene, Shape, Tile};
    use super::super::sim::SimShell;
    use super::*;
    use crate::apps::sim::Script;

    #[test]
    fn builds_the_same_map_for_both_arms() {
        let shell = Shell::new(Layout::default()).unwrap();
        assert_eq!(shell.map().tiles.len(), Layout::default().tiles.len());
        assert_ne!(shell.signature(Side::Left), shell.signature(Side::Right));
        // A depth only moves things: the same words in the same places.
        let deep = Layout { depth: 2.0, ..Layout::default() };
        assert_eq!(Shell::new(deep).unwrap().map(), shell.map());
    }

    #[test]
    fn shifts_split_the_disparity() {
        use super::super::layout::eye_shift;
        assert_eq!(eye_shift(0.0, Side::Left), 0);
        assert_eq!(eye_shift(1.0, Side::Left) - eye_shift(1.0, Side::Right), 80);
    }

    #[test]
    fn content_fills_and_cuts_lines() {
        let tile = TileMap {
            host: 0x100,
            values: 0x10c,
            lens: 0x200,
            icons: 0x204,
            lines: vec![(0x208, 4), (0x20c, 4)],
            buf: 0,
            points: 0,
        };
        let data = TileData { lines: vec!["ab".into(), "toolong".into()], icons: vec![3], ..TileData::default() };
        let code = content(&tile, &data);
        // The lengths: 2 and 4 (cut); the icons: 3 and none; then each line.
        assert_eq!(&code[0][..7], &[0x01, 0x00, 0x02, 0x00, 2, 2, 4]);
        assert_eq!(&code[1][..7], &[0x01, 0x04, 0x02, 0x00, 2, 3, icons::NONE]);
        assert_eq!(code.len(), 4);
    }

    #[test]
    fn formats_what_the_mac_sends() {
        let shell = Shell::new(Layout::default()).unwrap();
        let w = shell.of(Kind::Weather)[0];
        assert_eq!(shell.weather(w, 2, 11.6, 64.4, false).lines, [" 12°", " 64%"]);
        assert_eq!(shell.weather(w, 2, 0.0, 50.0, true).lines[0], " 32°");
        assert_eq!(shell.steps(6284).lines, [" 6,284"]);
        assert_eq!(shell.steps(42).lines, ["    42"]);
        assert_eq!(shell.steps(123_456).lines, ["123456"]);
        let n = shell.of(Kind::Notification)[0];
        let note = shell.note(n, "Messages: Lunch at 12:30?", 1);
        assert_eq!(note.lines, ["Messages", "Lunch at 12:30?"]);
        assert_eq!(note.icons, [1]);
        let list = shell.of(Kind::NotificationList)[0];
        let rows = shell.list(list, &[(1, "Alex: hi".into()), (2, "x".repeat(100))]);
        assert_eq!(rows.lines[0], "Alex: hi");
        assert!(rows.lines[1].ends_with("...") && rows.lines[1].len() == shell.geo.plans[list].cols);
    }

    #[test]
    fn a_tile_with_everything_builds() {
        let mut layout = Layout::default();
        let mut both = Tile::new("both", Kind::Battery, [300, 300, 300, 40]);
        both.both = true;
        layout.tiles.push(both);
        let mut clock = Tile::new("clock24", Kind::Clock, [300, 360, 300, 40]);
        clock.h24 = true;
        clock.seconds = false;
        clock.align = Align::Center;
        layout.tiles.push(clock);
        let mut one = Tile::new("ticker", Kind::Notification, [0, 440, 640, 36]);
        one.single_line = true;
        layout.tiles.push(one);
        layout.depth = 1.5;
        Shell::new(layout).unwrap();
    }

    const BOTH: [Side; 2] = [Side::Left, Side::Right];
    /// 10:59:59 at the first tick (0.8 s), 11:00:00 at the next.
    const ELEVEN: u32 = 10 * 3600 + 59 * 60 + 59;

    fn sim(script: Script) -> SimShell {
        SimShell::new(Layout::default(), script, &BOTH, 10, ELEVEN).unwrap()
    }

    fn bits(sim: &SimShell, names: &[&str]) -> u32 {
        names.iter().fold(0, |m, n| m | 1 << sim.tile(n))
    }

    /// Whether any pixel in x, y, w, h is lit on arm `arm`'s panel.
    fn lit(sim: &SimShell, arm: usize, [x, y, w, h]: [i32; 4]) -> bool {
        let Some(panel) = sim.world.arms[arm].panel() else { return false };
        (y..y + h).any(|y| (x..x + w).any(|x| panel[(y * 640 + x) as usize] != 0))
    }

    /// The text tile `name` last drew (clock, battery), as characters.
    fn drawn_text(sim: &SimShell, arm: usize, name: &str) -> String {
        let buf = sim.shell.map().tiles[sim.tile(name)].buf as usize;
        let chars: Vec<char> = ('0'..='9')
            .chain('A'..='Z')
            .chain('a'..='z')
            .chain((' '..='~').filter(|c| !c.is_ascii_alphanumeric()))
            .collect();
        let arena = sim.world.arms[arm].arena();
        let text: String =
            arena[buf..buf + 11].iter().map(|&g| chars.get(g as usize).copied().unwrap_or('#')).collect();
        text.trim_end().to_string()
    }

    #[test]
    fn shows_tiles_and_lets_a_notification_take_its_area() {
        let mut sim = sim(Script::still(30.0).touch(5.0, Side::Left, 1));
        sim.run(1.5).unwrap();
        let level = bits(&sim, &["clock", "date", "battery", "weather"]);
        for arm in 0..2 {
            assert_eq!(sim.visible(arm), level);
            assert!(lit(&sim, arm, [16, 16, 288, 40]), "the clock is drawn");
            assert!(!lit(&sim, arm, [16, 412, 608, 52]), "the status line has nothing");
        }
        let (note, status) = (sim.tile("notification"), sim.tile("status"));
        sim.send(note, "Messages: Lunch at 12:30?", 8.0);
        sim.send(status, "Now playing: a song", 0.0);
        sim.run(1.0).unwrap();
        for arm in 0..2 {
            assert_eq!(sim.visible(arm), bits(&sim, &["notification", "status"]));
            assert!(lit(&sim, arm, [16, 412, 608, 52]));
        }
        // A tap on the left TouchBar at 5 s dismisses it on both arms; the clock comes back.
        sim.run(3.0).unwrap();
        for arm in 0..2 {
            assert_eq!(sim.visible(arm), level | bits(&sim, &["status"]));
        }
        let panels: Vec<Vec<u8>> = sim.world.arms.iter().map(|a| a.panel().unwrap().to_vec()).collect();
        assert_eq!(panels[0], panels[1], "both eyes show the same with no depth");
    }

    #[test]
    fn keeps_time_and_expires_tiles() {
        let mut sim = sim(Script::still(30.0));
        sim.run(1.0).unwrap();
        assert_eq!(drawn_text(&sim, 0, "clock"), "10:59:59 AM");
        sim.send(sim.tile("notification"), "Timer: Done", 2.0);
        sim.run(1.0).unwrap();
        assert_eq!(sim.visible(1) & 1, 1);
        sim.run(2.5).unwrap();
        assert_eq!(sim.visible(1), bits(&sim, &["clock", "date", "battery", "weather"]), "its two seconds are up");
        // Ticks at 0.8, 1.8, ... 3.8 s: 11:00:02 at 4.5 s.
        assert_eq!(drawn_text(&sim, 1, "clock"), "11:00:02 AM");
        let events: Vec<_> = sim.world.arms[1].events.drain(..).collect();
        assert!(events.iter().any(|e| matches!(e.kind, v5::EventKind::Notify { a: NOTE_EXPIRED, b: 0, .. })));
    }

    #[test]
    fn a_double_tap_brings_back_the_last_notification_and_a_hold_hides_all() {
        let script =
            Script::still(30.0).touch(2.5, Side::Right, 1).touch(4.5, Side::Right, 2).touch(7.0, Side::Left, 4);
        let mut sim = sim(script);
        sim.run(1.0).unwrap();
        sim.send(sim.tile("notification"), "Hello: there", 0.0);
        sim.run(1.0).unwrap();
        assert_eq!(sim.visible(0) & 1, 1);
        sim.run(1.5).unwrap();
        assert_eq!(sim.visible(0) & 1, 0, "tapped away");
        sim.run(2.0).unwrap();
        assert_eq!(sim.visible(0) & 1, 1, "double-tapped back");
        sim.run(2.5).unwrap();
        for arm in 0..2 {
            assert_eq!(sim.visible(arm), 0, "held: hidden");
            assert!(sim.world.arms[arm].panel().is_none(), "and the panel off");
        }
    }

    #[test]
    fn both_arms_show_the_average_battery() {
        let mut script = Script::still(10.0);
        script.battery = [80, 91];
        let mut sim = sim(script);
        sim.run(3.0).unwrap();
        for arm in 0..2 {
            assert_eq!(&drawn_text(&sim, arm, "battery")[..4], " 86%", "(80 + 91) / 2, rounded");
        }
        let panels: Vec<Vec<u8>> = sim.world.arms.iter().map(|a| a.panel().unwrap().to_vec()).collect();
        assert_eq!(panels[0], panels[1]);
    }

    #[test]
    fn the_head_tilt_picks_the_scene_on_both_arms() {
        // Level, then looking down at 3 s, up at 6 s, level again at 9 s; a wobble back to -21° (inside the 6°
        // way back from -25°) at 4.5 s changes nothing.
        let script = Script::still(12.0).head(|t| {
            let pitch = match t {
                t if t < 3.0 => 0.0,
                t if t < 4.5 => -40.0,
                t if t < 5.0 => -21.0,
                t if t < 6.0 => -40.0,
                t if t < 9.0 => 35.0,
                _ => 0.0,
            };
            [0.0, pitch, 0.0]
        });
        let mut sim = sim(script);
        let level = bits(&sim, &["clock", "date", "battery", "weather"]);
        let down = bits(&sim, &["notifications", "heart", "steps", "mac_battery"]);
        let up = bits(&sim, &["big_clock", "sky"]);
        for (at, want, scene) in [(2.5, level, 0), (4.0, down, 1), (5.5, down, 1), (8.0, up, 2), (11.0, level, 0)] {
            let to = at - sim.world.seconds();
            sim.run(to).unwrap();
            for arm in 0..2 {
                assert_eq!(sim.word(arm, sim.shell.map().scene), scene, "scene at {at} s, arm {arm}");
                assert_eq!(sim.visible(arm), want, "tiles at {at} s, arm {arm}");
            }
        }
    }

    #[test]
    fn draws_the_new_tiles() {
        let mut sim = sim(Script::still(10.0).head(|_| [0.0, -45.0, 0.0]));
        let (list, steps, mac) = (sim.tile("notifications"), sim.tile("steps"), sim.tile("mac_battery"));
        let heart = sim.tile("heart");
        let rows: Vec<(u8, String)> = (0..20).map(|k| (k as u8 % 8, format!("Someone {k}: a message"))).collect();
        let data = sim.shell.list(list, &rows);
        sim.send_data(list, data);
        let data = sim.shell.steps(6284);
        sim.send_data(steps, data);
        let data = sim.shell.heart(72);
        sim.send_data(heart, data);
        let data = sim.shell.device(Some(76), true);
        sim.send_data(mac, data);
        sim.run(2.0).unwrap();
        assert_eq!(sim.word(1, sim.shell.map().scene), Scene::Down as u32);
        let plan = sim.shell.geo.plans[list].clone();
        // Every row has its icon and text: lit pixels in each row's band.
        let [x, y, w, _] = sim.shell.layout.tiles[list].rect;
        for row in 0..plan.rows as i32 {
            let band = [x, y + 2 + row * 10 * plan.scale as i32, w, 8 * plan.scale as i32];
            assert!(lit(&sim, 1, band), "row {row}");
        }
        assert_eq!(drawn_text(&sim, 1, "mac_battery")[..4].to_string(), " 76%");
        assert!(lit(&sim, 1, sim.shell.layout.tiles[steps].rect));
        assert!(lit(&sim, 1, sim.shell.layout.tiles[heart].rect));
    }

    #[test]
    fn draws_departure_mono_and_emoji() {
        // The notification and the clock in Departure Mono; the notification's emoji drawn, the status line's
        // (the 8 × 8 font) a "?".
        let mut layout = Layout::default();
        for name in ["notification", "clock"] {
            let i = layout.tiles.iter().position(|t| t.name == name).unwrap();
            layout.tiles[i].font = super::super::fonts::Face::Departure;
        }
        let mut sim = SimShell::new(layout, Script::still(10.0), &BOTH, 10, ELEVEN).unwrap();
        sim.run(1.0).unwrap();
        let (note, status) = (sim.tile("notification"), sim.tile("status"));
        let data = sim.shell.note(note, "Alex 🍜: lunch? 👍️ see you at 12:30 🎉🎉", 1);
        assert_eq!(data.lines[0], "Alex 🍜");
        assert!(data.lines[1].contains('👍'));
        sim.send_data(note, data);
        sim.send_on(note);
        sim.send(status, "done 👍", 0.0);
        sim.run(1.5).unwrap();
        for arm in 0..2 {
            assert_eq!(sim.visible(arm) & 1, 1);
        }
        assert_eq!(sim.shell.note(status, "done 👍", 0).lines, ["done ?"]);
        // The notification's text, at 11 px, is in its first body line's band.
        let plan = sim.shell.geo.plans[note].clone();
        let [x, y, w, _] = plan.inner;
        assert!(lit(&sim, 1, [x, y + 31, w, 12]));
    }

    #[test]
    fn a_tile_with_its_own_depth_moves_apart_in_the_eyes() {
        // The glasses' battery at 2 m (40 px between the eyes), the rest flat.
        let mut layout = Layout::default();
        let battery = layout.tiles.iter().position(|t| t.name == "battery").unwrap();
        layout.tiles[battery].depth = Some(2.0);
        let mut sim = SimShell::new(layout, Script::still(10.0), &BOTH, 10, ELEVEN).unwrap();
        sim.run(1.5).unwrap();
        // The leftmost lit column from x0 on in a band, on arm `arm` (0 left, 1 right).
        let first = |sim: &SimShell, arm: usize, x0: i32, y0: i32, y1: i32| {
            let panel = sim.world.arms[arm].panel().unwrap();
            (x0..640).find(|&x| (y0..y1).any(|y| panel[(y * 640 + x) as usize] != 0)).unwrap()
        };
        assert_eq!(first(&sim, 0, 400, 16, 56) - first(&sim, 1, 400, 16, 56), 40, "the battery");
        assert_eq!(first(&sim, 0, 0, 16, 56), first(&sim, 1, 0, 16, 56), "the clock");
    }

    #[test]
    fn draws_navigation_while_a_route_is_followed() {
        let mut layout = Layout::default();
        let mut round = Tile::new("round", Kind::Navigation, [380, 110, 240, 240]);
        round.shape = Shape::Round;
        layout.tiles.push(round);
        layout.tiles.push(Tile::new("square", Kind::Navigation, [16, 110, 300, 240]));
        let mut sim = SimShell::new(layout, Script::still(10.0), &BOTH, 10, ELEVEN).unwrap();
        sim.run(1.0).unwrap();
        let tiles = sim.shell.of(Kind::Navigation);
        let all = tiles.iter().fold(0, |m, i| m | 1 << i);
        assert_eq!(sim.visible(0) & all, 0, "off until a route");
        sim.feed_samples();
        for &i in &tiles {
            sim.send_on(i);
        }
        sim.run(2.0).unwrap();
        for arm in 0..2 {
            assert_eq!(sim.visible(arm) & all, all);
        }
        // The next turn's arrow; the square map's route straight up from the position (166, 269), and nothing
        // between the maps; the round map's corners dark.
        assert!(lit(&sim, 1, [18, 412, 48, 48]));
        assert!(lit(&sim, 1, [164, 190, 5, 50]));
        assert!(!lit(&sim, 1, [316, 110, 64, 240]));
        let [x, y, w, h] = sim.shell.layout.tiles[sim.tile("round")].rect;
        assert!(lit(&sim, 1, [x, y, w, h]));
        for [cx, cy] in [[x + 2, y + 2], [x + w - 14, y + 2], [x + 2, y + h - 14], [x + w - 14, y + h - 14]] {
            assert!(!lit(&sim, 1, [cx, cy, 12, 12]), "the round map's corner at {cx}, {cy}");
        }
        // Points outside the map draw nothing and stop nothing.
        let square = sim.tile("square");
        let wild = nav::Lines { route: vec![[0, 0], [5000, 5000], [20, 20], nav::BREAK, [1, 300]], roads: vec![] };
        sim.send_data(square, TileData { map: Some(wild), ..TileData::default() });
        sim.run(1.0).unwrap();
        assert!(!lit(&sim, 1, [316, 110, 64, 240]) && !lit(&sim, 1, [16, 352, 300, 50]));
        // The route ends: they go.
        for &i in &tiles {
            sim.send(i, "", 0.0);
        }
        sim.run(1.0).unwrap();
        assert_eq!(sim.visible(0) & all, 0);
    }

    #[test]
    fn renders_a_layout() {
        let mut layout = Layout::default();
        layout.tiles[0].preview = true;
        let render = super::super::sim::render(&layout, Scene::Level).unwrap();
        assert_eq!(render.visible & 1, 1);
        assert!(render.png.starts_with(b"\x89PNG"));
        let down = super::super::sim::render(&layout, Scene::Down).unwrap();
        let list = layout.tiles.iter().position(|t| t.kind == Kind::NotificationList).unwrap();
        // The notification covers the list's top, so the list hides while it's previewed.
        assert_eq!(down.visible & (1 << list), 0);
        assert_eq!(down.visible & 1, 1);
    }
}
