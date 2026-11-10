//! Two simulated arms on a virtual clock: scouter_kit's VM core (built by `build.rs` with `sim.c`) for each, a
//! script for the head, the TouchBars and the sensors, the arm-to-arm link between them, and a recording of what
//! both panels show, written as an animated PNG. `erisg1 v0 preview` runs the apps on it.

use std::collections::VecDeque;
use std::ffi::c_int;

use crate::proto::Side;
use crate::v0_pinned::{M, V, exp, log, mul, scale, transpose};
use crate::v5::{self, Event, prop};

#[repr(C)]
struct RawSim {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn sim_new(left: c_int) -> *mut RawSim;
    fn sim_free(s: *mut RawSim);
    fn sim_set_time(s: *mut RawSim, now: u32);
    fn sim_push(s: *mut RawSim, code: *const u8, len: u32);
    fn sim_waiting(s: *const RawSim) -> u32;
    fn sim_run(s: *mut RawSim) -> u32;
    fn sim_set_prop(s: *mut RawSim, id: u32, value: u32);
    fn sim_get_prop(s: *const RawSim, id: u32, value: *mut u32) -> c_int;
    fn sim_touch(s: *mut RawSim, gesture: u32);
    fn sim_peer(s: *mut RawSim, value: u32, sent: u32);
    fn sim_take_peer(s: *mut RawSim, value: *mut u32, time: *mut u32) -> c_int;
    fn sim_start(s: *mut RawSim, table: u32) -> c_int;
    fn sim_stop(s: *mut RawSim);
    fn sim_status(s: *const RawSim, out: *mut u8);
    fn sim_events(s: *mut RawSim, mask: u16);
    fn sim_next_event(s: *mut RawSim, out: *mut u8) -> u32;
    fn sim_panel(s: *const RawSim) -> *const u8;
    fn sim_panel_on(s: *const RawSim) -> c_int;
}

/// What happens to the simulated glasses, by the second.
pub(crate) struct Script {
    pub seconds: f64,
    /// TouchBar gestures: when, on which arm, and which (1 tap, 2 double, 3 triple, 4 long press).
    pub touches: Vec<(f64, Side, u32)>,
    /// The head's heading, pitch and roll in degrees at a time (left, up and right ear down positive).
    pub head: fn(f64) -> [f64; 3],
    pub lux: f64,
    /// Each arm's battery, %.
    pub battery: [u32; 2],
}

impl Script {
    /// `seconds` with the head still and nothing touched.
    pub fn still(seconds: f64) -> Script {
        Script { seconds, touches: Vec::new(), head: |_| [0.0; 3], lux: 300.0, battery: [84, 87] }
    }

    pub fn touch(mut self, at: f64, side: Side, gesture: u32) -> Script {
        self.touches.push((at, side, gesture));
        self
    }

    pub fn head(mut self, head: fn(f64) -> [f64; 3]) -> Script {
        self.head = head;
        self
    }
}

/// The head's turn (head frame to the room's, as `v0_pinned::Tracker`) for heading, pitch and roll in degrees.
pub(crate) fn head_turn([heading, pitch, roll]: [f64; 3]) -> M {
    let r = |axis: V, degrees: f64| exp(scale(axis, degrees.to_radians()));
    mul(&mul(&r([0.0, 1.0, 0.0], heading), &r([1.0, 0.0, 0.0], pitch)), &r([0.0, 0.0, 1.0], -roll))
}

pub(crate) struct SimArm {
    raw: *mut RawSim,
    pub side: Side,
    /// Added to the world's time for this arm's clock: the arms' clocks differ, as real ones do.
    offset: u32,
    /// When the VM wants to run again, world µs.
    wake: u64,
    pub events: VecDeque<Event>,
}

impl Drop for SimArm {
    fn drop(&mut self) {
        unsafe { sim_free(self.raw) }
    }
}

impl SimArm {
    fn new(side: Side, offset: u32) -> SimArm {
        let raw = unsafe { sim_new((side == Side::Left) as c_int) };
        assert!(!raw.is_null(), "out of memory for a simulated arm");
        SimArm { raw, side, offset, wake: 0, events: VecDeque::new() }
    }

    pub fn set(&mut self, id: u8, value: u32) {
        unsafe { sim_set_prop(self.raw, id as u32, value) }
    }

    pub fn get(&self, id: u8) -> u32 {
        let mut value = v5::UNKNOWN;
        unsafe { sim_get_prop(self.raw, id as u32, &mut value) };
        value
    }

    pub fn status(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        unsafe { sim_status(self.raw, out.as_mut_ptr()) };
        out
    }

    pub fn panel(&self) -> Option<&[u8]> {
        unsafe {
            (sim_panel_on(self.raw) != 0)
                .then(|| std::slice::from_raw_parts(sim_panel(self.raw), (v5::WIDTH * v5::HEIGHT) as usize))
        }
    }
}

/// The panel's 16 levels in its green.
fn palette() -> Vec<u8> {
    (0..16u32)
        .flat_map(|level| {
            let g = level * 255 / 15;
            [(g / 5) as u8, g as u8, (g / 3) as u8]
        })
        .collect()
}

/// One recorded moment: each eye's panel, left first, at half size (each pixel the brightest of four).
struct Still {
    pixels: Vec<u8>,
}

pub(crate) struct World {
    pub arms: Vec<SimArm>,
    /// World time, µs since the start.
    pub now: u64,
    script: Script,
    touched: usize,
    /// Messages on the arm-to-arm link: when they arrive, to which arm, the value, and when they were sent (world).
    link: Vec<(u64, usize, u32, u64)>,
    stills: Vec<Still>,
    next_still: u64,
    still_every: u64,
    head_at: f64,
}

/// How long a message takes from one arm to the other (VM.md §6.9: 6–150 ms).
const LINK_DELAY: u64 = 60_000;
/// The longest step the world takes between running the VMs: the sensors change no faster than this.
const STEP: u64 = 5_000;

impl World {
    pub fn new(sides: &[Side], script: Script, fps: u32) -> World {
        let arms = sides
            .iter()
            .map(|&side| SimArm::new(side, if side == Side::Left { 1_234_567 } else { 98_765_432 }))
            .collect();
        let mut world = World {
            arms,
            now: 0,
            script,
            touched: 0,
            link: Vec::new(),
            stills: Vec::new(),
            next_still: 0,
            still_every: 1_000_000 / fps as u64,
            head_at: f64::NAN,
        };
        world.sense();
        world
    }

    pub fn seconds(&self) -> f64 {
        self.now as f64 / 1e6
    }

    pub fn script(&self) -> &Script {
        &self.script
    }

    /// The arm's clock at world time `t` (µs).
    pub fn device_time(&self, arm: usize, t: u64) -> u32 {
        (t as u32).wrapping_add(self.arms[arm].offset)
    }

    /// The world time of `device` on arm `arm`, taken as the nearest to now.
    pub fn world_time(&self, arm: usize, device: u32) -> u64 {
        let ahead = device.wrapping_sub(self.device_time(arm, self.now)) as i32;
        (self.now as i64 + ahead as i64).max(0) as u64
    }

    pub fn push(&mut self, arm: usize, packets: Vec<Vec<u8>>) {
        for p in packets {
            unsafe { sim_push(self.arms[arm].raw, p.as_ptr(), p.len() as u32) };
        }
        self.arms[arm].wake = self.now;
    }

    pub fn waiting(&self, arm: usize) -> u32 {
        unsafe { sim_waiting(self.arms[arm].raw) }
    }

    pub fn start(&mut self, arm: usize, table: u32) -> Result<(), String> {
        let code = unsafe { sim_start(self.arms[arm].raw, table) };
        self.arms[arm].wake = self.now;
        if code == 0 { Ok(()) } else { Err(format!("VM_START refused: {}", v5::error_name(code as u8))) }
    }

    pub fn stop(&mut self, arm: usize) {
        unsafe { sim_stop(self.arms[arm].raw) }
    }

    pub fn events_mask(&mut self, arm: usize, mask: u16) {
        unsafe { sim_events(self.arms[arm].raw, mask) }
    }

    /// The head's pose now, from the script.
    pub fn head(&self) -> [f64; 3] {
        (self.script.head)(self.seconds())
    }

    /// Sets the sensors from the script for now.
    fn sense(&mut self) {
        let t = self.seconds();
        if t == self.head_at {
            return;
        }
        let turn = head_turn((self.script.head)(t));
        // The accelerometer reads the way up, in the head frame; the gyroscope the turn since a moment ago.
        let up = crate::v0_pinned::apply(&transpose(&turn), [0.0, 1000.0, 0.0]);
        let dt = 0.01;
        let before = head_turn((self.script.head)((t - dt).max(0.0)));
        let rate = scale(log(&mul(&transpose(&before), &turn)), (180.0 / std::f64::consts::PI) * 1000.0 / dt);
        let pitch = (-up[2]).atan2(up[1]).to_degrees();
        let roll = (-up[0]).atan2(up[1]).to_degrees();
        let (lux, battery) = (self.script.lux, self.script.battery);
        for arm in &mut self.arms {
            let right = arm.side == Side::Right;
            for (id, v) in [(prop::ACC_X, up[0]), (prop::ACC_Y, up[1]), (prop::ACC_Z, up[2])] {
                arm.set(id, v.round() as i32 as u32);
            }
            if arm.get(prop::GYRO_RATE) != 0 {
                for (id, v) in [(prop::GYR_X, rate[0]), (prop::GYR_Y, rate[1]), (prop::GYR_Z, rate[2])] {
                    arm.set(id, v.round() as i32 as u32);
                }
            }
            arm.set(prop::PITCH, pitch.round() as i32 as u32);
            arm.set(prop::ROLL, roll.round() as i32 as u32);
            arm.set(prop::LIGHT, if right { (lux * 100.0) as u32 } else { v5::UNKNOWN });
            let pct = battery[right as usize];
            arm.set(prop::BATT_PCT, pct);
            arm.set(prop::BATT_MV, 3400 + pct * 8);
            arm.set(prop::BATT_MA, 38);
            arm.set(prop::BATT_TEMP, 284);
            arm.set(prop::CHARGE, 0);
            arm.set(prop::IN_CASE, 0);
            arm.set(prop::WORN, 1);
        }
        self.head_at = t;
    }

    /// Runs the world on to `until` (µs): the scripted touches, the arm-to-arm link, the sensors and both VMs,
    /// recording the panels as it goes.
    pub fn run_to(&mut self, until: u64) {
        loop {
            // Everything due now.
            while let Some(&(at, side, gesture)) = self.script.touches.get(self.touched)
                && (at * 1e6) as u64 <= self.now
            {
                if let Some(arm) = self.arms.iter_mut().find(|a| a.side == side) {
                    unsafe { sim_touch(arm.raw, gesture) };
                    arm.wake = self.now;
                }
                self.touched += 1;
            }
            let now = self.now;
            let (due, later): (Vec<_>, Vec<_>) = self.link.drain(..).partition(|m| m.0 <= now);
            self.link = later;
            for (_, to, value, sent) in due {
                let sent = self.device_time(to, sent);
                unsafe { sim_peer(self.arms[to].raw, value, sent) };
                self.arms[to].wake = self.now;
            }
            self.sense();
            for i in 0..self.arms.len() {
                let device = self.device_time(i, self.now);
                let arm = &mut self.arms[i];
                unsafe { sim_set_time(arm.raw, device) };
                if arm.wake <= self.now {
                    let wait = unsafe { sim_run(arm.raw) };
                    arm.wake = if wait == u32::MAX { u64::MAX } else { self.now + wait.max(1) as u64 };
                }
                let mut buf = [0u8; 64];
                loop {
                    let len = unsafe { sim_next_event(arm.raw, buf.as_mut_ptr()) } as usize;
                    if len == 0 {
                        break;
                    }
                    // Back into the wire format: type, epoch, time, body.
                    let mut wire = vec![buf[0]];
                    wire.extend(1u32.to_le_bytes());
                    wire.extend(device.to_le_bytes());
                    wire.extend(&buf[1..len]);
                    if let Some(event) = v5::event(&wire) {
                        arm.events.push_back(event);
                    }
                }
                let (mut value, mut at) = (0u32, 0u32);
                if unsafe { sim_take_peer(arm.raw, &mut value, &mut at) } != 0 {
                    let sent = self.world_time(i, at);
                    for other in 0..self.arms.len() {
                        if other != i {
                            self.link.push((self.now + LINK_DELAY, other, value, sent));
                        }
                    }
                }
            }
            while self.next_still <= self.now {
                self.record();
                self.next_still += self.still_every;
            }
            if self.now >= until {
                break;
            }
            let mut next = (self.now + STEP).min(until).min(self.next_still);
            for arm in &self.arms {
                next = next.min(arm.wake.max(self.now + 1));
            }
            for m in &self.link {
                next = next.min(m.0.max(self.now + 1));
            }
            if let Some(&(at, _, _)) = self.script.touches.get(self.touched) {
                next = next.min(((at * 1e6) as u64).max(self.now + 1));
            }
            self.now = next;
        }
    }

    /// Runs until the arms have taken in everything sent to them, at most `limit` µs.
    pub fn settle(&mut self, limit: u64) {
        let end = self.now + limit;
        while (0..self.arms.len()).any(|i| self.waiting(i) > 0) && self.now < end {
            let to = self.now + 1_000;
            self.run_to(to);
        }
    }

    fn record(&mut self) {
        let (w, h) = ((v5::WIDTH / 2) as usize, (v5::HEIGHT / 2) as usize);
        let gap = 8;
        let mut pixels = vec![0u8; (2 * w + gap) * h];
        for (n, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            let Some(panel) = self.arms.iter().find(|a| a.side == side).and_then(|a| a.panel()) else { continue };
            for y in 0..h {
                for x in 0..w {
                    let at = |dx: usize, dy: usize| panel[(2 * y + dy) * v5::WIDTH as usize + 2 * x + dx];
                    let v = at(0, 0).max(at(1, 0)).max(at(0, 1)).max(at(1, 1));
                    pixels[y * (2 * w + gap) + n * (w + gap) + x] = v;
                }
            }
        }
        self.stills.push(Still { pixels });
    }

    /// Writes `count` moments of what was recorded, evenly spread, as one still PNG: a grid two wide, each moment
    /// both eyes side by side, with a thin line between moments.
    pub fn write_sheet(&self, path: &std::path::Path, count: usize) -> Result<(), String> {
        let (w, h) = ((v5::WIDTH + 8) as usize, (v5::HEIGHT / 2) as usize);
        let picks: Vec<&Still> = if self.stills.is_empty() {
            Vec::new()
        } else {
            (0..count).map(|i| &self.stills[(i * (self.stills.len() - 1)) / (count - 1).max(1)]).collect()
        };
        let (cols, rows) = (2, picks.len().div_ceil(2).max(1));
        let (sw, sh) = (cols * w + 8, rows * (h + 8));
        let mut sheet = vec![3u8; sw * sh];
        for (i, still) in picks.iter().enumerate() {
            let (ox, oy) = ((i % cols) * (w + 8), (i / cols) * (h + 8));
            for y in 0..h {
                sheet[(oy + y) * sw + ox..(oy + y) * sw + ox + w].copy_from_slice(&still.pixels[y * w..(y + 1) * w]);
            }
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), sw as u32, sh as u32);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(palette());
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&sheet).map_err(|e| e.to_string())
    }

    /// Writes what was recorded as an animated PNG, both eyes side by side in the panel's green.
    pub fn write_preview(&self, path: &std::path::Path) -> Result<(), String> {
        let (w, h) = (v5::WIDTH + 8, v5::HEIGHT / 2);
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Four);
        encoder.set_palette(palette());
        let frames = self.stills.len().max(1) as u32;
        encoder.set_animated(frames, 0).map_err(|e| e.to_string())?;
        encoder.set_frame_delay(1, (1_000_000 / self.still_every) as u16).map_err(|e| e.to_string())?;
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let blank = Still { pixels: vec![0; (w * h) as usize] };
        let stills: Vec<&Still> = if self.stills.is_empty() { vec![&blank] } else { self.stills.iter().collect() };
        for still in stills {
            let packed: Vec<u8> = still.pixels.chunks(2).map(|p| p[0] << 4 | p.get(1).copied().unwrap_or(0)).collect();
            writer.write_image_data(&packed).map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())
    }

    /// Each arm's VM_STATUS, to check that its program runs.
    pub fn statuses(&self) -> Vec<(Side, [u8; 32])> {
        self.arms.iter().map(|a| (a.side, a.status())).collect()
    }

    pub fn stop_all(&mut self) {
        for i in 0..self.arms.len() {
            self.stop(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_and_passes_messages() {
        let mut world = World::new(&[Side::Left, Side::Right], Script::still(1.0), 10);
        // A filled square on the left arm.
        world.push(0, vec![[v5::fill(10, 10, 20, 20, 15), v5::draw(0, 0, 640, 480)].concat()]);
        world.run_to(100_000);
        let panel = world.arms[0].panel().expect("the panel is on");
        assert_eq!(panel[15 * 640 + 15], 15);
        assert_eq!(panel[5 * 640 + 5], 0);
        assert!(world.arms[1].panel().is_none());
        // A PEER_SEND on the right reaches the left with its time in the left's clock.
        world.push(1, vec![[v5::set(prop::PEER_LINK, 1), v5::set(prop::PEER_SEND, 77)].concat()]);
        world.push(0, vec![v5::set(prop::PEER_LINK, 1)]);
        let sent = world.now;
        world.run_to(world.now + 200_000);
        assert_eq!(world.arms[0].get(prop::PEER_MSG), 77);
        let t = world.arms[0].get(prop::PEER_T_US);
        assert!((world.world_time(0, t) as i64 - sent as i64).abs() < 10_000);
    }

    #[test]
    fn the_sensors_follow_the_head() {
        let mut world = World::new(&[Side::Right], Script::still(1.0).head(|t| [0.0, 20.0 * t, 0.0]), 10);
        world.run_to(500_000);
        assert_eq!(world.arms[0].get(prop::PITCH) as i32, 10);
        let acc_z = world.arms[0].get(prop::ACC_Z) as i32;
        assert!(acc_z < -150, "{acc_z}");
    }
}
