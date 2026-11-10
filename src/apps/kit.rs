//! The kit the apps (`erisg1 v0 app NAME`) are built on, so an app is only what it shows and how it reacts. The same
//! calls drive the real glasses or two simulated arms (`erisg1 v0 preview NAME`, `sim.rs`).
//!
//! An app either draws from the Mac, frame by frame (`Glasses::frame`, `input`, `wait`, `head`), or uploads a
//! program that runs on the glasses alone (`Glasses::deploy`, programs built with `vmkit.rs`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::sim::{Script, World, head_turn};
use super::vmkit::{Program, TABLE};
use crate::link::{self, Error};
use crate::proto::Side;
use crate::v0_cube::{self, EYES, FOCAL};
use crate::v0_pinned::{Beat, Frame, Head, M, V, apply, sub, transpose};
use crate::v5::{self, prop, trigger};
use crate::v5_link::Arm;

/// Where the big font goes, after the main one (scale 6 at most fits before 0x30000).
pub(crate) const BIG_FONT: u32 = 0x29000;
/// The do-nothing program that keeps the right arm's light sensor reading while the Mac draws.
const IDLE: u32 = 0x3F000;
/// The kit's routine that draws a list of lines (`Canvas::lines`), its constants, and the list's buffer.
const LINES_POOL: u32 = 0x3E000;
const LINES_CODE: u32 = 0x3E040;
const LINES_LIST: u32 = 0x3E200;
/// Lines per list: 9 bytes each in one WRITE that fits a packet.
const LINES_CHUNK: usize = 25;
/// Arena words the kit keeps for drawing: frame, stride, main font, text mode (v5::fixed_args), then the big font.
const W_FRAME: u16 = 16;
const W_STRIDE: u16 = 17;
const W_FONT: u16 = 18;
const W_MASK: u16 = 19;
const W_BIG: u16 = 20;
/// The panel's middle, where the line of sight meets it.
pub(crate) const CX: f64 = 320.0;
pub(crate) const CY: f64 = 240.0;
/// Frames are this far apart at most (30 a second).
const PERIOD: Duration = Duration::from_millis(33);

#[derive(Clone, Copy)]
pub(crate) struct Options {
    /// The main font: the 8 × 8 font this many times over (2: 16 px characters).
    pub font: u8,
    /// A second, big font for numbers and headlines (at most 6), or 0 for none.
    pub big: u8,
    /// Follow the head with the gyroscopes (`Eye::head`, `Glasses::head`): about 1.5 s holding still at the start.
    pub imu: bool,
    /// Levels (of 42) added to Even's light-to-brightness table.
    pub boost: u8,
}

/// A TouchBar gesture (VM.md §6.7), and the arm whose TouchBar it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    Tap,
    DoubleTap,
    TripleTap,
    /// A press held for half a second (the start of a long press).
    Hold,
    Release,
    Other(u32),
}

impl Gesture {
    pub fn from_code(code: u32) -> Gesture {
        match code {
            1 => Gesture::Tap,
            2 => Gesture::DoubleTap,
            3 => Gesture::TripleTap,
            4 => Gesture::Hold,
            5 => Gesture::Release,
            other => Gesture::Other(other),
        }
    }
}

pub(crate) enum Target {
    /// The real glasses: both arms, or one.
    Glasses(Option<Side>),
    /// Two simulated arms following a script, recording a preview to the path given.
    Sim(Script, Option<PathBuf>),
}

enum Backend {
    Real { arms: Vec<Arm>, beat: Beat, head: Option<Head>, light: Light, extra: Duration },
    Sim { world: World, path: Option<PathBuf>, gestures: Vec<(Side, Gesture)> },
}

/// The right arm's light, asked for every few seconds without waiting for the answer.
struct Light {
    value: u32,
    asked: Option<(u8, Instant)>,
    last: Option<Instant>,
}

pub(crate) struct Glasses {
    backend: Backend,
    options: Options,
    started: Instant,
    stop: Arc<AtomicBool>,
    deployed: bool,
}

/// One eye, as a frame is drawn for it.
pub(crate) struct Eye {
    pub side: Side,
    /// The head's turn (head frame to the room's, `v0_pinned::Tracker`) when the frame appears, with `imu`.
    pub head: Option<M>,
}

impl Eye {
    /// Where the eye is across the head, metres: left negative.
    pub fn x(&self) -> f64 {
        v0_cube::eye(Some(self.side))
    }

    /// The sideways shift, in pixels, that puts something drawn flat `metres` away: the left eye's copy right of
    /// where it's drawn, the right eye's left. Comfortable from about 0.8 m out.
    pub fn depth(&self, metres: f64) -> i32 {
        let disparity = FOCAL * EYES / metres;
        let left = (disparity / 2.0).round();
        match self.side {
            Side::Left => left as i32,
            Side::Right => -(disparity - left).round() as i32,
        }
    }

    #[allow(clippy::wrong_self_convention)]
    /// A point given relative to the middle of the head (metres: x right, y up, z ahead) as this eye sees it: x
    /// right, y up, z ahead of the eye.
    pub fn from_head(&self, p: V) -> V {
        [p[0] - self.x(), p[1], p[2]]
    }

    #[allow(clippy::wrong_self_convention)]
    /// A point in the room (the head frame at the start: x right, y up, z back) as this eye sees it now: x right,
    /// y up, z ahead. Without `imu`, the head never turns.
    pub fn from_room(&self, p: V) -> V {
        let turned = match &self.head {
            Some(turn) => apply(&transpose(turn), p),
            None => p,
        };
        let [x, y, z] = sub(turned, [self.x(), 0.0, 0.0]);
        [x, y, -z]
    }

    /// Where a point the eye sees (x right, y up, z ahead) lands on the panel, if it's in front.
    pub fn project(&self, p: V) -> Option<[f64; 2]> {
        (p[2] > 0.05).then(|| [CX + FOCAL * p[0] / p[2], CY - FOCAL * p[1] / p[2]])
    }
}

/// What a frame draws into one eye's frame buffer, and which parts of it go to the panel at the frame's moment.
/// Everything is clipped to the panel. Text is drawn by its top left corner.
pub(crate) struct Canvas {
    code: Vec<Vec<u8>>,
    show: Vec<Vec<u8>>,
    font: u8,
    big: u8,
}

fn regs(values: &[i32]) -> Vec<u8> {
    let bytes: Vec<u8> = values.iter().flat_map(|v| (*v as u32).to_le_bytes()).collect();
    v5::write(0, &bytes)
}

const FILL: u8 = 0x50;
const GLYPHS: u8 = 0x52;
const DRAW: u8 = 0x53;
const LINE: u8 = 0x54;

impl Canvas {
    fn new(font: u8, big: u8) -> Canvas {
        Canvas { code: Vec::new(), show: Vec::new(), font, big }
    }

    /// The part of x, y, w, h on the panel, or `None`.
    fn clamp(x: i32, y: i32, w: i32, h: i32) -> Option<[i32; 4]> {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(v5::WIDTH as i32), (y + h).min(v5::HEIGHT as i32));
        (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, level: u8) {
        if let Some([x, y, w, h]) = Canvas::clamp(x, y, w, h) {
            let mut c = regs(&[x, y, w, h, level.min(15) as i32]);
            c.extend(v5::encode(FILL, &[W_FRAME, W_STRIDE, 0, 1, 2, 3, 4], &[]));
            self.code.push(c);
        }
    }

    pub fn clear(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.fill(x, y, w, h, 0);
    }

    /// Clears the whole frame.
    pub fn clear_all(&mut self) {
        self.fill(0, 0, v5::WIDTH as i32, v5::HEIGHT as i32, 0);
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, level: u8) {
        self.line_f([x0 as f64, y0 as f64], [x1 as f64, y1 as f64], level);
    }

    pub fn line_f(&mut self, a: [f64; 2], b: [f64; 2], level: u8) {
        if !(a.iter().chain(&b).all(|v| v.is_finite() && v.abs() < 30000.0)) {
            return;
        }
        if let Some([x0, y0, x1, y1]) = v0_cube::clip(a, b) {
            let mut c = regs(&[x0, y0, x1, y1, level.min(15) as i32]);
            c.extend(v5::encode(LINE, &[W_FRAME, W_STRIDE, 0, 1, 2, 3, 4], &[]));
            self.code.push(c);
        }
    }

    /// The outline of a rectangle.
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, level: u8) {
        let (x1, y1) = (x + w - 1, y + h - 1);
        self.line(x, y, x1, y, level);
        self.line(x1, y, x1, y1, level);
        self.line(x1, y1, x, y1, level);
        self.line(x, y1, x, y, level);
    }

    /// The main font's character size, pixels.
    pub fn size(&self) -> i32 {
        8 * self.font as i32
    }

    pub fn big_size(&self) -> i32 {
        8 * self.big as i32
    }

    pub fn width(&self, text: &str) -> i32 {
        self.size() * text.chars().count() as i32
    }

    pub fn big_width(&self, text: &str) -> i32 {
        self.big_size() * text.chars().count() as i32
    }

    fn glyphs(&mut self, font_word: u16, scale: u8, x: i32, top: i32, text: &str, level: u8) -> i32 {
        let size = 8 * scale as i32;
        let indices = v5::text_indices(text);
        // Glyph rows must stay on the panel's rows: below them is the rest of the arena.
        if indices.is_empty() || scale == 0 || top < 0 || top + size > v5::HEIGHT as i32 {
            return size * indices.len() as i32;
        }
        if x >= v5::WIDTH as i32 || x + size * (indices.len() as i32) <= 0 {
            return size * indices.len() as i32;
        }
        for (n, chunk) in indices.chunks(200).enumerate() {
            let x = x + n as i32 * 200 * size;
            let mut c = v5::write(v5::TEXT, chunk);
            c.extend(regs(&[x, top + 7 * scale as i32, level.min(15) as i32, v5::TEXT as i32, chunk.len() as i32]));
            c.extend(v5::encode(GLYPHS, &[font_word, W_FRAME, W_STRIDE, 0, 1, W_MASK, 2, 3, 4], &[]));
            self.code.push(c);
        }
        size * indices.len() as i32
    }

    /// Text in the main font, its top left at `x`, `top`; returns its width.
    pub fn text(&mut self, x: i32, top: i32, text: &str, level: u8) -> i32 {
        self.glyphs(W_FONT, self.font, x, top, text, level)
    }

    /// Text in the big font (`Options::big`); returns its width.
    pub fn big(&mut self, x: i32, top: i32, text: &str, level: u8) -> i32 {
        self.glyphs(W_BIG, self.big, x, top, text, level)
    }

    /// Text in the main font centred on `x`.
    pub fn text_centred(&mut self, x: i32, top: i32, text: &str, level: u8) {
        let w = self.width(text);
        self.text(x - w / 2, top, text, level);
    }

    pub fn big_centred(&mut self, x: i32, top: i32, text: &str, level: u8) {
        let w = self.big_width(text);
        self.big(x - w / 2, top, text, level);
    }

    /// Puts x, y, w, h of the frame on the panel at the frame's moment.
    pub fn show(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if let Some([x, y, w, h]) = Canvas::clamp(x, y, w, h) {
            let mut c = regs(&[x, y, w, h]);
            c.extend(v5::encode(DRAW, &[W_FRAME, W_STRIDE, 0, 1, 0, 1, 2, 3], &[]));
            self.show.push(c);
        }
    }

    pub fn show_all(&mut self) {
        self.show(0, 0, v5::WIDTH as i32, v5::HEIGHT as i32);
    }

    /// Many lines at once, each (x0, y0, x1, y1) and a level: 9 bytes a line on the link instead of about 25, so
    /// about 25 lines fit a packet. Clipped to the panel.
    pub fn lines(&mut self, lines: &[([f64; 4], u8)]) {
        let mut list: Vec<u8> = Vec::new();
        let mut n = 0;
        let flush = |c: &mut Canvas, list: &mut Vec<u8>, n: &mut usize| {
            if *n > 0 {
                c.code.push(v5::write(LINES_LIST, list));
                let mut call = regs(&[LINES_LIST as i32, *n as i32]);
                call.extend(v5::call(LINES_CODE));
                c.code.push(call);
            }
            list.clear();
            *n = 0;
        };
        for &([x0, y0, x1, y1], level) in lines {
            if ![x0, y0, x1, y1].iter().all(|v| v.is_finite() && v.abs() < 30000.0) {
                continue;
            }
            let Some(ends) = v0_cube::clip([x0, y0], [x1, y1]) else { continue };
            for v in ends {
                list.extend((v as u16).to_le_bytes());
            }
            list.push(level.min(15));
            n += 1;
            if n == LINES_CHUNK {
                flush(self, &mut list, &mut n);
            }
        }
        flush(self, &mut list, &mut n);
    }

    /// Copies w × h of the frame from x, y to `to` (overlapping is fine): to scroll a part of it.
    pub fn blit(&mut self, x: i32, y: i32, w: i32, h: i32, to: [i32; 2]) {
        let fits = |x: i32, y: i32| Canvas::clamp(x, y, w, h) == Some([x, y, w, h]);
        if w <= 0 || h <= 0 || !fits(x, y) || !fits(to[0], to[1]) {
            return;
        }
        // BLIT src, src_stride, src_x, src_y, dst, dst_stride, dst_x, dst_y, w, h, mode (copy), level.
        let mut c = regs(&[x, y, to[0], to[1], w, h, 0, 15]);
        c.extend(v5::encode(0x51, &[W_FRAME, W_STRIDE, 0, 1, W_FRAME, W_STRIDE, 2, 3, 4, 5, 6, 7], &[]));
        self.code.push(c);
    }

    fn into_frame(self) -> Frame {
        (self.code, self.show)
    }
}

/// Host code that readies an arm for the kit: the fonts, the drawing words, a blank panel.
fn setup(options: &Options) -> Vec<Vec<u8>> {
    let mut code = v5::upload(v5::FONT, &v5::font(options.font.max(1)), 200);
    if options.big > 0 {
        code.extend(v5::upload(BIG_FONT, &v5::font(options.big.min(6)), 200));
    }
    code.push(v5::fixed_args());
    code.push(v5::write(W_BIG as u32 * 4, &BIG_FONT.to_le_bytes()));
    let (lines, pool) = lines_routine();
    code.extend(v5::upload(LINES_POOL, &pool, 200));
    code.extend(v5::upload(LINES_CODE, &lines, 200));
    code.push(v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0));
    code.push(v5::draw(0, 0, v5::WIDTH, v5::HEIGHT));
    code
}

/// The routine `Canvas::lines` calls: r0 the list's address, r1 how many lines (x0, y0, x1, y1 as u16, then a
/// level byte, each).
fn lines_routine() -> (Vec<u8>, Vec<u8>) {
    use crate::v5_asm::*;
    let mut a = Asm::new(LINES_CODE, LINES_POOL);
    let [zero, one, two, three, eight, nine] = [0, 1, 2, 3, 8, 9].map(|v| a.k(v));
    a.label("next");
    a.branch(BEQ, 1, zero, "done");
    a.op(LD16, &[2, 0, zero]);
    a.op(LD16, &[3, 0, one]);
    a.op(LD16, &[4, 0, two]);
    a.op(LD16, &[5, 0, three]);
    a.op(LD8, &[6, 0, eight]);
    a.op(LINE, &[W_FRAME, W_STRIDE, 2, 3, 4, 5, 6]);
    a.op(ADD, &[0, 0, nine]);
    a.op(SUB, &[1, 1, one]);
    a.jump(JMP, "next");
    a.label("done");
    a.op(RET, &[]);
    let (code, pool) = a.finish().expect("the lines routine assembles");
    assert!(LINES_POOL + pool.len() as u32 <= LINES_CODE && LINES_CODE + code.len() as u32 <= LINES_LIST);
    (code, pool)
}

/// The do-nothing program (`v0_sensors::start_idle_program`) as host code and its table.
fn idle_program() -> (Vec<Vec<u8>>, u32) {
    let table_at = IDLE + 16;
    let mut table = [0u32; 16];
    table[0] = IDLE;
    let table: Vec<u8> = table.iter().flat_map(|v| v.to_le_bytes()).collect();
    (
        vec![
            v5::set(prop::AUTO_BRIGHTNESS, 0),
            v5::set(prop::PEER_LINK, 0),
            v5::write(IDLE, &[v5::END]),
            v5::write(table_at, &table),
        ],
        table_at,
    )
}

impl Glasses {
    pub async fn open(target: Target, options: Options) -> Result<Glasses, Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let backend = match target {
            Target::Glasses(side) => {
                let mut arms = crate::v0::arms(side).await?;
                let mut lead = Duration::ZERO;
                let touch = (1u16 << trigger::TOUCH).to_le_bytes();
                for arm in &mut arms {
                    arm.control(v5::ctrl::VM_STOP, &[]).await?;
                    arm.run(setup(&options)).await?;
                    let (idle, table) = idle_program();
                    arm.run(idle).await?;
                    if options.imu {
                        arm.run([v5::set(prop::IMU_RATE, 104), v5::set(prop::GYRO_RATE, 104)]).await?;
                    }
                    arm.flush().await?;
                    arm.control(v5::ctrl::VM_START, &table.to_le_bytes()).await?;
                    arm.control(v5::ctrl::EVENTS, &touch).await?;
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
                let head = if options.imu { Some(Head::calibrate(&mut arms, false).await?) } else { None };
                let beat = Beat::new(lead, &arms);
                let flag = stop.clone();
                tokio::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        flag.store(true, Ordering::SeqCst);
                    }
                });
                Backend::Real {
                    arms,
                    beat,
                    head,
                    light: Light { value: v5::UNKNOWN, asked: None, last: None },
                    extra: Duration::ZERO,
                }
            }
            Target::Sim(script, path) => {
                let mut world = World::new(&[Side::Left, Side::Right], script, 8);
                for i in 0..2 {
                    let code = v5::pack(setup(&options), 240);
                    world.push(i, code);
                    let (idle, table) = idle_program();
                    world.push(i, v5::pack(idle, 240));
                    world.settle(2_000_000);
                    world.start(i, table)?;
                    world.events_mask(i, 1 << trigger::TOUCH);
                }
                Backend::Sim { world, path, gestures: Vec::new() }
            }
        };
        let mut glasses = Glasses { backend, options, started: Instant::now(), stop, deployed: false };
        glasses.check()?;
        Ok(glasses)
    }

    /// Whether this is the simulator, for apps that make up their data there.
    pub fn preview(&self) -> bool {
        matches!(self.backend, Backend::Sim { .. })
    }

    /// Seconds since the app started (in the simulator, its clock).
    pub fn now(&self) -> f64 {
        match &self.backend {
            Backend::Real { .. } => self.started.elapsed().as_secs_f64(),
            Backend::Sim { world, .. } => world.seconds(),
        }
    }

    /// False once it's time to stop: Ctrl-C on the glasses, the end of the script in the simulator.
    pub fn running(&self) -> bool {
        match &self.backend {
            Backend::Real { .. } => !self.stop.load(Ordering::SeqCst),
            Backend::Sim { world, .. } => world.seconds() < world.script().seconds,
        }
    }

    /// The head's turn now (with `imu`): head frame to the room's, the room being the head at the start.
    pub fn head(&self) -> Option<M> {
        match &self.backend {
            Backend::Real { head, .. } => head.as_ref().map(|h| h.tracker.turn),
            Backend::Sim { world, .. } => self.options.imu.then(|| head_turn(world.head())),
        }
    }

    /// Reports errors the arms sent: a failed packet or a stopped program.
    fn check(&mut self) -> Result<(), Error> {
        if let Backend::Sim { world, gestures, .. } = &mut self.backend {
            for arm in &mut world.arms {
                while let Some(event) = arm.events.pop_front() {
                    match event.kind {
                        v5::EventKind::Error { offset, op, code, detail, .. } => {
                            return Err(format!(
                                "{:?} arm: host code failed at offset {offset}, op {op:#04x}: {} ({code}), detail \
                                 {detail:#x}",
                                arm.side,
                                v5::error_name(code)
                            )
                            .into());
                        }
                        v5::EventKind::Halt { address, op, code, detail } => {
                            return Err(format!(
                                "{:?} arm: the program stopped at {address:#x}, op {op:#04x}: {} ({code}), detail \
                                 {detail:#x}",
                                arm.side,
                                v5::error_name(code)
                            )
                            .into());
                        }
                        v5::EventKind::Input { trigger: trigger::TOUCH, value } => {
                            gestures.push((arm.side, Gesture::from_code(value)));
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// The TouchBar gestures since the last call, and (with `imu`) the head followed on.
    pub async fn input(&mut self) -> Result<Vec<(Side, Gesture)>, Error> {
        self.check()?;
        match &mut self.backend {
            Backend::Real { arms, head, light, .. } => {
                if let Some(head) = head {
                    let gestures = head.poll(arms).await?;
                    light.value = head.light;
                    return Ok(gestures.into_iter().map(|(s, g)| (s, Gesture::from_code(g))).collect());
                }
                let mut gestures = Vec::new();
                for arm in arms.iter_mut() {
                    arm.poll().await?;
                    while let Some(event) = arm.events.pop_front() {
                        if let v5::EventKind::Input { trigger: trigger::TOUCH, value } = event.kind {
                            gestures.push((arm.side, Gesture::from_code(value)));
                        }
                    }
                    if arm.side == Side::Right {
                        if let Some((tag, _)) = light.asked
                            && let Some(reply) = arm.reply(tag)
                        {
                            if let Some(v) = reply?.as_chunks::<4>().0.first() {
                                light.value = u32::from_le_bytes(*v);
                            }
                            light.asked = None;
                        }
                        let stale = light.asked.is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2));
                        if (light.asked.is_none() || stale)
                            && light.last.is_none_or(|at| at.elapsed() > Duration::from_secs(2))
                        {
                            light.asked = Some((arm.request(v5::ctrl::PROPS, &[prop::LIGHT]).await?, Instant::now()));
                            light.last = Some(Instant::now());
                        }
                    }
                }
                Ok(gestures)
            }
            Backend::Sim { gestures, .. } => Ok(std::mem::take(gestures)),
        }
    }

    /// Draws a frame: `draw` once for each eye, into its frame buffer; both eyes show it at the same moment.
    pub async fn frame(&mut self, mut draw: impl FnMut(&Eye, &mut Canvas)) -> Result<(), Error> {
        let (font, big, boost) = (self.options.font, self.options.big, self.options.boost);
        match &mut self.backend {
            Backend::Real { arms, beat, head, light, extra } => {
                let due = beat.next_with(arms, *extra).await?;
                let turn = head.as_ref().map(|h| h.at(due).0);
                let mut bytes = 0;
                let frames: Vec<Frame> = arms
                    .iter()
                    .map(|arm| {
                        let mut canvas = Canvas::new(font, big);
                        draw(&Eye { side: arm.side, head: turn }, &mut canvas);
                        bytes = bytes.max(canvas.code.iter().chain(&canvas.show).map(Vec::len).sum::<usize>());
                        canvas.into_frame()
                    })
                    .collect();
                // A frame of several packets takes longer to arrive: the next one is sent earlier.
                let interval = arms.iter().map(|a| a.interval()).max().unwrap_or_default();
                *extra = interval * (bytes / 230) as u32 / 2;
                beat.send(arms, due, light.value, boost, frames).await?;
            }
            Backend::Sim { world, .. } => {
                let due = world.now + 40_000;
                let t = due as f64 / 1e6;
                let turn = self.options.imu.then(|| head_turn((world.script().head)(t)));
                for i in 0..world.arms.len() {
                    let mut canvas = Canvas::new(font, big);
                    draw(&Eye { side: world.arms[i].side, head: turn }, &mut canvas);
                    let (mut code, show) = canvas.into_frame();
                    code.push(v5::sleep_until(world.device_time(i, due)));
                    code.extend(show);
                    world.push(i, v5::pack(code, 240));
                }
                let to = world.now + PERIOD.as_micros() as u64;
                world.run_to(to);
            }
        }
        self.check()
    }

    /// Waits `seconds` (in the simulator, runs its clock on).
    pub async fn wait(&mut self, seconds: f64) -> Result<(), Error> {
        match &mut self.backend {
            Backend::Real { .. } => tokio::time::sleep(Duration::from_secs_f64(seconds.max(0.0))).await,
            Backend::Sim { world, .. } => {
                let to = world.now + (seconds.max(0.0) * 1e6) as u64;
                world.run_to(to);
            }
        }
        self.check()
    }

    /// Properties (`v5::prop`) of each arm, as `PROPS` reads them (VM.md §2.5): sensors, battery, case. On the
    /// glasses it waits for the answers (tens of ms): read a few seconds apart.
    pub async fn props(&mut self, ids: &[u8]) -> Result<Vec<(Side, Vec<u32>)>, Error> {
        match &mut self.backend {
            Backend::Real { arms, .. } => {
                let mut out = Vec::new();
                for arm in arms.iter_mut() {
                    out.push((arm.side, arm.props(ids).await?));
                }
                Ok(out)
            }
            Backend::Sim { world, .. } => {
                Ok(world.arms.iter().map(|a| (a.side, ids.iter().map(|&id| a.get(id)).collect())).collect())
            }
        }
    }

    /// Puts the panel in standby (dark, about 1 mA) until the next frame shows something.
    pub async fn sleep_panel(&mut self) -> Result<(), Error> {
        match &mut self.backend {
            Backend::Real { arms, .. } => {
                for arm in arms.iter_mut() {
                    arm.run([v5::set(prop::PANEL, 2)]).await?;
                }
            }
            Backend::Sim { world, .. } => {
                for i in 0..world.arms.len() {
                    world.push(i, vec![v5::set(prop::PANEL, 2)]);
                }
            }
        }
        Ok(())
    }

    /// Uploads a program to each arm (`program(side)`, built with `vmkit::Prog`) and starts them together: from
    /// then on the glasses run it alone, and the Mac leaves when the app returns. Both arms take the arm-to-arm
    /// link; the left follows the right arm's brightness.
    pub async fn deploy(&mut self, program: impl Fn(Side) -> Result<Program, String>) -> Result<(), Error> {
        // The arms' clocks at one moment a little ahead, for the programs' start words.
        const AHEAD: u32 = 500_000;
        match &mut self.backend {
            Backend::Real { arms, .. } => {
                let host = crate::v5_link::host_us().wrapping_add(AHEAD);
                for arm in arms.iter_mut() {
                    let p = program(arm.side)?;
                    arm.control(v5::ctrl::VM_STOP, &[]).await?;
                    let start = arm.arm_time(host);
                    arm.run(p.writes(start)).await?;
                    arm.run([
                        v5::set(prop::PEER_LINK, 1),
                        v5::set(prop::AUTO_BRIGHTNESS, (arm.side == Side::Left) as u32),
                    ])
                    .await?;
                    arm.flush().await?;
                }
                for arm in arms.iter_mut() {
                    arm.control(v5::ctrl::VM_START, &TABLE.to_le_bytes()).await?;
                }
            }
            Backend::Sim { world, .. } => {
                let at = world.now + AHEAD as u64;
                for i in 0..world.arms.len() {
                    world.stop(i);
                    let p = program(world.arms[i].side)?;
                    let start = world.device_time(i, at);
                    world.push(i, v5::pack(p.writes(start), 240));
                    let left = world.arms[i].side == Side::Left;
                    world.push(
                        i,
                        vec![[v5::set(prop::PEER_LINK, 1), v5::set(prop::AUTO_BRIGHTNESS, left as u32)].concat()],
                    );
                }
                world.settle(5_000_000);
                for i in 0..world.arms.len() {
                    world.start(i, TABLE)?;
                }
            }
        }
        self.deployed = true;
        self.check()
    }

    /// Ends the app. On the glasses: a deployed program goes on running; otherwise the panel goes off. In the
    /// simulator: a deployed program runs to the script's end, then the preview is written.
    pub async fn close(mut self) -> Result<(), Error> {
        if let Backend::Sim { world, .. } = &mut self.backend {
            let end = (world.script().seconds * 1e6) as u64;
            if world.now < end {
                world.run_to(end);
            }
        }
        self.check()?;
        match self.backend {
            Backend::Real { arms, .. } => {
                if self.deployed {
                    for arm in arms {
                        arm.disconnect().await;
                    }
                    println!("running on the glasses; `erisg1 v0 stop` stops it");
                    return Ok(());
                }
                crate::v0_pinned::finish(arms).await
            }
            Backend::Sim { world, path, .. } => {
                for (side, status) in world.statuses() {
                    if status[0] == 2 {
                        return Err(format!("{side:?} arm's program halted: {}", v5::error_name(status[15])).into());
                    }
                }
                if let Some(path) = path {
                    world.write_preview(&path)?;
                    world.write_sheet(&path.with_extension("sheet.png"), 8)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_clips_and_skips_what_is_off_the_panel() {
        let mut c = Canvas::new(2, 0);
        c.fill(-10, -10, 5, 5, 15);
        c.text(10, 470, "below", 15);
        c.text(10, -3, "above", 15);
        c.line(-100, -100, -50, -50, 15);
        assert!(c.code.is_empty());
        c.fill(630, 470, 50, 50, 15);
        c.text(600, 10, "edge", 15);
        assert_eq!(c.code.len(), 2);
    }

    #[tokio::test]
    async fn draws_lists_of_lines_and_scrolls_in_the_simulator() {
        let options = Options { font: 2, big: 0, imu: false, boost: 5 };
        let mut g = Glasses::open(Target::Sim(Script::still(1.0), None), options).await.unwrap();
        let many: Vec<([f64; 4], u8)> = (0..60).map(|i| ([10.0, 10.0 + i as f64, 100.0, 10.0 + i as f64], 9)).collect();
        g.frame(|_, c| {
            c.lines(&many);
            c.blit(10, 10, 91, 60, [200, 300]);
            c.show_all();
        })
        .await
        .unwrap();
        g.wait(0.1).await.unwrap();
        let Backend::Sim { world, .. } = &g.backend else { unreachable!() };
        for arm in &world.arms {
            let panel = arm.panel().unwrap();
            assert_eq!(panel[69 * 640 + 50], 9, "the 60th line");
            assert_eq!(panel[70 * 640 + 50], 0);
            assert_eq!(panel[(300 + 59) * 640 + 240], 9, "its copy");
        }
        g.close().await.unwrap();
    }

    #[test]
    fn depth_splits_between_the_eyes() {
        let left = Eye { side: Side::Left, head: None };
        let right = Eye { side: Side::Right, head: None };
        // 1270 × 0.063 / 1 m ≈ 80 px in all.
        assert_eq!(left.depth(1.0) - right.depth(1.0), 80);
        assert!(left.depth(1.0) > 0 && right.depth(1.0) < 0);
    }
}
