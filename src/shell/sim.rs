//! The shell on simulated arms (`apps::sim`): the editor's picture of a layout, `erisg1 shell preview`, and the tests.

use std::path::Path;

use chrono::Timelike;

use super::layout::{self, Kind, Layout, Scene};
use super::program::{self, Shell, TileData, WANT_KEEP, WANT_OFF, WANT_ON};
use super::{icons, nav};
use crate::apps::sim::{Script, World};
use crate::proto::Side;
use crate::v5::{self, prop};

/// What the preview shows in tiles marked `preview`, and the Mac's data for the rest; in a font with emoji, the
/// emoji too.
const SAMPLE_NOTE: &str = "Messages: Alex: are we still on for lunch at 12:30? I can book the table by the river.";
const SAMPLE_NOTE_EMOJI: &str =
    "Messages: Alex: are we still on for lunch at 12:30? 🍜 I can book the table by the river 👍";
const SAMPLE_TEXT: &str = "Now playing: Bonobo - Kerala";
const SAMPLE_LIST: [(&str, &str, &str); 6] = [
    ("org.whispersystems.signal-desktop", "Alex: are we still on for lunch?", " 🍜"),
    ("com.apple.mail", "Invoice #1042 from Fastmail", ""),
    ("com.apple.iCal", "Standup in 5 minutes, room 4B", " ⏰"),
    ("us.zoom.xos", "Missed call from Sam", ""),
    ("com.spotify.client", "Bonobo - Kerala", " 🎵"),
    ("com.example.app", "Your build finished in 42 s", " ✅"),
];
pub(crate) const SAMPLE_STEPS: u32 = 6284;
pub(crate) const SAMPLE_HEART: u32 = 72;
/// The preview's route: this many metres along the sample.
const SAMPLE_WALKED: f64 = 40.0;

pub(crate) struct SimShell {
    pub world: World,
    pub shell: Shell,
    generation: u32,
}

impl SimShell {
    /// The shell loaded on simulated arms (`sides`), started 0.5 s in, the clock at `sod` (second of the day) then.
    pub fn new(layout: Layout, script: Script, sides: &[Side], fps: u32, sod: u32) -> Result<SimShell, String> {
        let shell = Shell::new(layout)?;
        let mut world = World::new(sides, script, fps);
        let start_at = 500_000;
        for i in 0..sides.len() {
            let side = world.arms[i].side;
            let mut code = shell.prepare();
            for (at, asset) in shell.assets() {
                code.extend(v5::upload(at, &asset, 240));
            }
            code.extend(shell.load(side, world.device_time(i, start_at)));
            code.push(v5::set(prop::PEER_LINK, 1));
            code.push(v5::set(prop::AUTO_BRIGHTNESS, (side == Side::Left) as u32));
            world.push(i, v5::pack(code, 240));
        }
        let mut sim = SimShell { world, shell, generation: 0 };
        sim.set_time(sod.wrapping_sub(1) % 86_400, start_at + 300_000);
        let today = chrono::Local::now().date_naive();
        for i in sim.shell.of(Kind::Date) {
            let lines = layout::dates(sim.shell.layout.tiles[i].date_format(), today);
            let data = TileData { lines, ..TileData::default() };
            for arm in 0..sides.len() {
                let code = program::content(&sim.shell.map().tiles[i], &data);
                sim.world.push(arm, v5::pack(code, 240));
            }
        }
        sim.world.settle(5_000_000);
        for i in 0..sides.len() {
            sim.world.start(i, program::TRIGGERS)?;
        }
        Ok(sim)
    }

    fn set_time(&mut self, sod: u32, tick_at: u64) {
        self.generation += 1;
        for i in 0..self.world.arms.len() {
            let code = program::time(self.shell.map(), self.generation, sod, self.world.device_time(i, tick_at));
            self.world.push(i, vec![code]);
        }
    }

    /// Writes `data` (if any) and turns tile `i` on (for `seconds`, 0 until turned off), off, or leaves it
    /// (`WANT_*`), acted on 0.3 s from now.
    fn update(&mut self, i: usize, data: Option<&TileData>, want: u32, seconds: f64) {
        self.generation += 1;
        let due = self.world.now + 300_000;
        for arm in 0..self.world.arms.len() {
            let tile = &self.shell.map().tiles[i];
            let mut code = data.map(|d| program::content(tile, d)).unwrap_or_default();
            code.push(program::turn(tile, self.generation, want, (seconds * 1e6) as u32));
            code.extend(program::raise(self.shell.map(), self.world.device_time(arm, due)));
            self.world.push(arm, v5::pack(code, 240));
        }
    }

    /// Sends tile `i` `text` (as the Mac would, wrapped to fit) and turns it on for `seconds` (0: until turned
    /// off), or off for empty text.
    pub fn send(&mut self, i: usize, text: &str, seconds: f64) {
        let data = self.shell.note(i, text, icons::category(None));
        let want = if data.lines.is_empty() { WANT_OFF } else { WANT_ON };
        self.update(i, Some(&data), want, seconds);
    }

    /// New content for tile `i`, on or off as it is.
    pub fn send_data(&mut self, i: usize, data: TileData) {
        self.update(i, Some(&data), WANT_KEEP, 0.0);
    }

    /// Turns tile `i` on, its content as it is.
    pub fn send_on(&mut self, i: usize) {
        self.update(i, None, WANT_ON, 0.0);
    }

    #[cfg(test)]
    pub fn tile(&self, name: &str) -> usize {
        self.shell.layout.tiles.iter().position(|t| t.name == name).expect("a tile of that name")
    }

    /// Runs on `seconds`; an error if a program stopped.
    pub fn run(&mut self, seconds: f64) -> Result<(), String> {
        let to = self.world.now + (seconds.max(0.0) * 1e6) as u64;
        self.world.run_to(to);
        for (side, status) in self.world.statuses() {
            if status[0] == 2 {
                let address = u32::from_le_bytes(status[8..12].try_into().unwrap());
                return Err(format!("{side:?} arm's shell stopped at {address:#x}: {}", v5::error_name(status[15])));
            }
        }
        Ok(())
    }

    pub fn word(&self, arm: usize, addr: u32) -> u32 {
        let a = self.world.arms[arm].arena();
        u32::from_le_bytes(a[addr as usize..addr as usize + 4].try_into().unwrap())
    }

    /// The mask of tiles showing on arm `arm`.
    pub fn visible(&self, arm: usize) -> u32 {
        self.word(arm, self.shell.map().visible)
    }

    /// Sample data for every tile the host feeds (weather, steps, heart rate, the Mac's battery and notifications, a
    /// route).
    pub fn feed_samples(&mut self) {
        let route = nav::sample(SAMPLE_WALKED);
        for arm in 0..self.world.arms.len() {
            let code = self.shell.arrows(route.turn);
            self.world.push(arm, v5::pack(code, 240));
        }
        for i in 0..self.shell.layout.tiles.len() {
            let data = match self.shell.layout.tiles[i].kind {
                Kind::Weather => Some(self.shell.weather(i, icons::weather(2, true), 17.6, 62.0, false)),
                Kind::Steps => Some(self.shell.steps(SAMPLE_STEPS)),
                Kind::HeartRate => Some(self.shell.heart(SAMPLE_HEART)),
                Kind::Navigation => Some(self.shell.navigation(i, &route, true)),
                Kind::DeviceBattery => Some(self.shell.device(Some(76), true)),
                Kind::NotificationList => {
                    let tile = &self.shell.layout.tiles[i];
                    let emoji = tile.emoji(self.shell.geo.plans[i].scale);
                    let rows: Vec<(u8, String)> = SAMPLE_LIST
                        .iter()
                        .map(|(app, text, extra)| {
                            (icons::category(Some(app)), format!("{text}{}", if emoji { *extra } else { "" }))
                        })
                        .collect();
                    Some(self.shell.list(i, &rows))
                }
                _ => None,
            };
            if let Some(data) = data {
                self.send_data(i, data);
            }
        }
    }
}

/// The second of the day now, local time.
pub(crate) fn second_of_day() -> u32 {
    chrono::Local::now().num_seconds_from_midnight()
}

/// The editor's picture of a layout: one eye's panel as a PNG, which tiles show, each tile's plan.
pub(crate) struct Render {
    pub png: Vec<u8>,
    pub visible: u32,
    pub plans: Vec<layout::Plan>,
}

fn level(_: f64) -> [f64; 3] {
    [0.0; 3]
}

fn looking_down(_: f64) -> [f64; 3] {
    [0.0, -85.0, 0.0]
}

fn looking_up(_: f64) -> [f64; 3] {
    [0.0, 85.0, 0.0]
}

/// Runs `layout` (flat, the tiles marked `preview` on with sample content, sample data from the Mac) on a simulated
/// right arm, the head tilted for `scene`.
pub(crate) fn render(layout: &Layout, scene: Scene) -> Result<Render, String> {
    let mut layout = layout.clone();
    layout.depth = 0.0;
    for t in &mut layout.tiles {
        t.depth = None;
    }
    let head = match scene {
        Scene::Level => level,
        Scene::Down => looking_down,
        Scene::Up => looking_up,
    };
    let previewed: Vec<usize> = (0..layout.tiles.len()).filter(|&i| layout.tiles[i].preview).collect();
    let mut sim = SimShell::new(layout, Script::still(10.0).head(head), &[Side::Right], 1, second_of_day())?;
    sim.feed_samples();
    for &i in &previewed {
        let tile = &sim.shell.layout.tiles[i];
        let emoji = tile.emoji(sim.shell.geo.plans[i].scale);
        let text = match tile.kind {
            Kind::Notification if emoji => SAMPLE_NOTE_EMOJI.to_string(),
            Kind::Notification => SAMPLE_NOTE.to_string(),
            Kind::Text if !tile.text.trim().is_empty() => tile.text.clone(),
            Kind::Text => SAMPLE_TEXT.to_string(),
            _ => String::new(),
        };
        if text.is_empty() {
            sim.send_on(i);
        } else {
            let data = sim.shell.note(i, &text, icons::category(Some("com.apple.MobileSMS")));
            sim.update(i, Some(&data), WANT_ON, 0.0);
        }
    }
    sim.run(1.6)?;
    let visible = sim.visible(0);
    let blank = vec![0u8; (v5::WIDTH * v5::HEIGHT) as usize];
    let panel = sim.world.arms[0].panel().unwrap_or(&blank);
    let png = panel_png(panel)?;
    Ok(Render { png, visible, plans: sim.shell.geo.plans.clone() })
}

/// A panel (a level a pixel) as a PNG in the panel's green.
pub(crate) fn panel_png(panel: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, v5::WIDTH, v5::HEIGHT);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Four);
    let palette: Vec<u8> = (0..16u32)
        .flat_map(|level| {
            let g = level * 255 / 15;
            [(g / 5) as u8, g as u8, (g / 3) as u8]
        })
        .collect();
    encoder.set_palette(palette);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let packed: Vec<u8> = panel.chunks(2).map(|p| p[0] << 4 | p[1]).collect();
    writer.write_image_data(&packed).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// The head in `erisg1 shell preview`: level, down at 11 s, up at 14 s, level from 17 s.
fn preview_head(t: f64) -> [f64; 3] {
    let pitch = match t {
        t if t < 11.0 => 0.0,
        t if t < 14.0 => -40.0,
        t if t < 17.0 => 35.0,
        _ => 0.0,
    };
    [0.0, pitch, 0.0]
}

/// `erisg1 shell preview`: `layout` on two simulated arms for 20 s, as an animated PNG and a sheet in `out`. A
/// notification comes in at 2 s, a tap dismisses it at 5.6 s, a double tap brings it back at 7.6 s and a tap takes it
/// away at 9 s; the status line gets text at 10 s; the head looks down at 11 s and up at 14 s; a hold hides
/// everything at 18 s and shows it again at 19 s.
pub(crate) fn preview(layout: &Layout, out: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let script = Script::still(20.0)
        .head(preview_head)
        .touch(5.6, Side::Right, 1)
        .touch(7.6, Side::Left, 2)
        .touch(9.0, Side::Right, 1)
        .touch(18.0, Side::Right, 4)
        .touch(19.0, Side::Right, 4);
    let mut sim = SimShell::new(layout.clone(), script, &[Side::Left, Side::Right], 10, 9 * 3600 + 41 * 60 + 55)?;
    sim.feed_samples();
    let note = sim.shell.of(Kind::Notification).first().copied();
    let text = sim.shell.of(Kind::Text).first().copied();
    sim.run(2.0)?;
    if let Some(i) = note {
        let data = sim.shell.note(i, "Calendar: Standup in 5 minutes, room 4B. Bring the release notes.", 3);
        sim.update(i, Some(&data), WANT_ON, 0.0);
    }
    sim.run(8.0)?;
    if let Some(i) = text {
        sim.send(i, SAMPLE_TEXT, 0.0);
    }
    sim.run(10.0)?;
    let path = out.join("shell.png");
    sim.world.write_preview(&path)?;
    sim.world.write_sheet(&out.join("shell.sheet.png"), 10)?;
    println!("{}", path.display());
    Ok(())
}
