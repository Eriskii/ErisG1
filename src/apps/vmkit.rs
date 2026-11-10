//! A builder for programs that run on the glasses alone (`Glasses::deploy`), on top of `v5_asm`. It lays out the
//! arena, keeps the program's variables and buffers, and has the pieces most programs need:
//!
//! - `var_start`: a variable holding the arm's clock at a moment shared by both arms, to schedule from, so both
//!   eyes change together.
//! - `events`: TouchBar gestures from either arm, and messages sent with `broadcast`, handled by both arms at the
//!   same moment (200 ms after the touch: the time the arm-to-arm link needs), in a routine the program supplies.
//! - Drawing in the kit's two fonts, digits and minutes:seconds into a text buffer, and the right arm's
//!   brightness from its light sensor (the left follows it).
//!
//! Registers r0–r11 are the program's; the helpers use r12–r14. Registers don't keep their values from one
//! handler to the next safely: keep state in `var`s.

use super::kit::Options;
use crate::proto::Side;
use crate::v5::{self, prop, trigger};
use crate::v5_asm::*;

pub(crate) const POOL: u32 = 0x30000;
pub(crate) const VARS: u32 = 0x31000;
pub(crate) const DATA: u32 = 0x31400;
pub(crate) const CODE: u32 = 0x34000;
/// The trigger table, just below the kit's do-nothing program.
pub(crate) const TABLE: u32 = 0x3EFC0;
/// How long after a gesture or a broadcast both arms act on it.
pub(crate) const EVENT_LEAD_US: u32 = 200_000;

pub(crate) const R0: u16 = 0;
pub(crate) const R1: u16 = 1;
pub(crate) const R2: u16 = 2;
pub(crate) const R3: u16 = 3;
pub(crate) const R4: u16 = 4;
pub(crate) const R5: u16 = 5;
pub(crate) const R6: u16 = 6;
pub(crate) const R7: u16 = 7;
pub(crate) const R8: u16 = 8;
pub(crate) const R9: u16 = 9;
pub(crate) const R10: u16 = 10;
pub(crate) const R11: u16 = 11;
const R12: u16 = 12;
const R13: u16 = 13;
const R14: u16 = 14;
/// The words `v5::fixed_args` and the kit keep: the frame, its stride, the main font, the text mode.
const W_FRAME: u16 = 16;
const W_STRIDE: u16 = 17;
const W_FONT: u16 = 18;
const W_MASK: u16 = 19;
const W_BIG: u16 = 20;

/// What a variable starts as.
pub(crate) enum Init {
    Value(u32),
    /// The arm's clock at the shared start moment, plus this many µs.
    Start(i64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Font {
    Main,
    Big,
}

/// A finished program, ready to upload.
pub(crate) struct Program {
    code: Vec<u8>,
    pool: Vec<u8>,
    data: Vec<(u32, Vec<u8>)>,
    triggers: [u32; 16],
    inits: Vec<Init>,
}

impl Program {
    /// Host code that puts it in the arena, `start` being the arm's clock at the shared start moment.
    pub fn writes(&self, start: u32) -> Vec<Vec<u8>> {
        let mut w = v5::upload(POOL, &self.pool, 240);
        w.extend(v5::upload(CODE, &self.code, 240));
        for (at, bytes) in &self.data {
            w.extend(v5::upload(*at, bytes, 240));
        }
        let vars: Vec<u8> = self
            .inits
            .iter()
            .flat_map(|init| match init {
                Init::Value(v) => v.to_le_bytes(),
                Init::Start(offset) => start.wrapping_add(*offset as u32).to_le_bytes(),
            })
            .collect();
        w.extend(v5::upload(VARS, &vars, 240));
        let table: Vec<u8> = self.triggers.iter().flat_map(|v| v.to_le_bytes()).collect();
        w.push(v5::write(TABLE, &table));
        w
    }
}

pub(crate) struct Prog {
    pub a: Asm,
    pub side: Side,
    font: u8,
    big: u8,
    inits: Vec<Init>,
    data_at: u32,
    data: Vec<(u32, Vec<u8>)>,
    triggers: Vec<(u8, &'static str)>,
    labels: u32,
    message: Option<(u16, u16)>,
    bright: Option<u16>,
}

impl Prog {
    /// A program for the arm on `side`, drawing in the fonts `options` gave the kit.
    pub fn new(side: Side, options: &Options) -> Prog {
        Prog {
            a: Asm::new(CODE, POOL),
            side,
            font: options.font,
            big: options.big,
            inits: Vec::new(),
            data_at: DATA,
            data: Vec::new(),
            triggers: Vec::new(),
            labels: 0,
            message: None,
            bright: None,
        }
    }

    pub fn right(&self) -> bool {
        self.side == Side::Right
    }

    /// A variable (an arena word), starting at `init`.
    pub fn var(&mut self, init: u32) -> u16 {
        self.inits.push(Init::Value(init));
        ((VARS / 4) as usize + self.inits.len() - 1) as u16
    }

    /// A variable starting at the arm's clock at the moment both arms share, plus `offset_us`.
    pub fn var_start(&mut self, offset_us: i64) -> u16 {
        self.inits.push(Init::Start(offset_us));
        ((VARS / 4) as usize + self.inits.len() - 1) as u16
    }

    /// A buffer holding `bytes`; returns its address.
    pub fn data(&mut self, bytes: &[u8]) -> u32 {
        let at = self.data_at;
        self.data.push((at, bytes.to_vec()));
        self.data_at = (at + bytes.len() as u32 + 3) & !3;
        at
    }

    /// A text buffer holding `text` as glyph indices (digits are their own value, so `digits` can write them).
    pub fn text(&mut self, text: &str) -> u32 {
        self.data(&v5::text_indices(text))
    }

    /// The pool word holding `value`.
    pub fn k(&mut self, value: u32) -> u16 {
        self.a.k(value)
    }

    pub fn ki(&mut self, value: i32) -> u16 {
        self.a.k(value as u32)
    }

    pub fn op(&mut self, op: u8, words: &[u16]) {
        self.a.op(op, words);
    }

    pub fn label(&mut self, name: &'static str) {
        self.a.label(name);
    }

    pub fn jump(&mut self, op: u8, target: &'static str) {
        self.a.jump(op, target);
    }

    pub fn branch(&mut self, op: u8, a: u16, b: u16, target: &'static str) {
        self.a.branch(op, a, b, target);
    }

    /// A label name no other has.
    pub fn fresh(&mut self, stem: &str) -> &'static str {
        self.labels += 1;
        Box::leak(format!("_{stem}{}", self.labels).into_boxed_str())
    }

    /// `label` handles `trigger` (`v5::trigger`): START, TIMER, TILT, BATTERY, CASE, WEAR...
    pub fn on(&mut self, trigger: u8, label: &'static str) {
        self.triggers.push((trigger, label));
    }

    pub fn read(&mut self, dst: u16, property: u8) {
        let p = self.k(property as u32);
        self.a.op(SYS_READ, &[dst, p]);
    }

    pub fn set(&mut self, property: u8, value: u16) {
        let p = self.k(property as u32);
        self.a.op(SYS_WRITE, &[p, value]);
    }

    /// The TIMER trigger at `at` (the arm's clock).
    pub fn timer(&mut self, at: u16) {
        self.set(prop::TIMER, at);
    }

    pub fn sleep_until(&mut self, at: u16) {
        self.a.op(SLEEP_UNTIL, &[at]);
    }

    pub fn fill(&mut self, x: u16, y: u16, w: u16, h: u16, level: u16) {
        self.a.op(FILL, &[W_FRAME, W_STRIDE, x, y, w, h, level]);
    }

    /// A rectangle of the frame at `level`; nothing of it off the panel.
    pub fn fill_k(&mut self, x: i32, y: i32, w: i32, h: i32, level: u8) {
        if let Some([x, y, w, h]) = clamp(x, y, w, h) {
            let [x, y, w, h, l] = [x, y, w, h, level as i32].map(|v| self.ki(v));
            self.fill(x, y, w, h, l);
        }
    }

    /// A line of the frame. Coordinates from words must stay on the panel (rows past it are other memory):
    /// clamp them first.
    pub fn line(&mut self, x0: u16, y0: u16, x1: u16, y1: u16, level: u16) {
        self.a.op(LINE, &[W_FRAME, W_STRIDE, x0, y0, x1, y1, level]);
    }

    pub fn line_k(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, level: u8) {
        let ends = [x0, y0, x1, y1];
        assert!(ends.iter().all(|v| (0..v5::WIDTH as i32).contains(v)) && y0 < 480 && y1 < 480, "line off the panel");
        let [x0, y0, x1, y1, l] = [x0, y0, x1, y1, level as i32].map(|v| self.ki(v));
        self.line(x0, y0, x1, y1, l);
    }

    /// x, y, w, h of the frame to the panel.
    pub fn draw(&mut self, x: u16, y: u16, w: u16, h: u16) {
        self.a.op(DRAW, &[W_FRAME, W_STRIDE, x, y, x, y, w, h]);
    }

    pub fn draw_k(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if let Some([x, y, w, h]) = clamp(x, y, w, h) {
            let [x, y, w, h] = [x, y, w, h].map(|v| self.ki(v));
            self.draw(x, y, w, h);
        }
    }

    /// A font's character size, pixels.
    pub fn size(&self, font: Font) -> i32 {
        8 * if font == Font::Main { self.font } else { self.big } as i32
    }

    /// `n` glyphs of the buffer at the address in word `text`, in `font`, the top left at x, `top` (words). `top`
    /// must keep the text on the panel's rows.
    pub fn glyphs(&mut self, font: Font, x: u16, top: u16, text: u16, n: u16, level: u16) {
        let ascent = self.size(font) * 7 / 8;
        let a = self.ki(ascent);
        self.a.op(ADD, &[R14, top, a]);
        let f = if font == Font::Main { W_FONT } else { W_BIG };
        self.a.op(GLYPHS, &[f, W_FRAME, W_STRIDE, x, R14, W_MASK, level, text, n]);
    }

    /// `n` glyphs of the buffer at `buf` (+ `from`), the top left at x, `top`.
    #[allow(clippy::too_many_arguments)]
    pub fn text_k(&mut self, font: Font, x: i32, top: i32, buf: u32, from: u32, n: u32, level: u8) {
        let size = self.size(font);
        assert!(top >= 0 && top + size <= v5::HEIGHT as i32, "text off the panel's rows");
        let baseline = top + size * 7 / 8;
        let f = if font == Font::Main { W_FONT } else { W_BIG };
        let [x, y, t, n, l] = [x, baseline, (buf + from) as i32, n as i32, level as i32].map(|v| self.ki(v));
        self.a.op(GLYPHS, &[f, W_FRAME, W_STRIDE, x, y, W_MASK, l, t, n]);
    }

    /// Writes `value` (a word) into the text buffer at `buf` + `at` as `count` decimal digits, zero-padded (the
    /// value modulo 10^count).
    pub fn digits(&mut self, value: u16, buf: u32, at: u32, count: u32) {
        let (ten, b) = (self.k(10), self.k(buf));
        self.a.op(MOV, &[R12, value]);
        for i in (0..count).rev() {
            let place = self.k(at + i);
            self.a.op(MODU, &[R13, R12, ten]);
            self.a.op(ST8, &[b, place, R13]);
            self.a.op(DIVU, &[R12, R12, ten]);
        }
    }

    /// As `digits`, with the leading zeros (all but the last digit) as spaces.
    pub fn digits_spaced(&mut self, value: u16, buf: u32, at: u32, count: u32) {
        self.digits(value, buf, at, count);
        let (zero, b, space) = (self.k(0), self.k(buf), self.k(v5::text_indices(" ")[0] as u32));
        let done = self.fresh("spaced");
        for i in 0..count.saturating_sub(1) {
            let place = self.k(at + i);
            self.a.op(LD8, &[R13, b, place]);
            self.a.branch(BNE, R13, zero, done);
            self.a.op(ST8, &[b, place, space]);
        }
        self.a.label(done);
    }

    /// Writes `secs` (a word) as "MM:SS" into the buffer at `buf` + `at` (the colon included; minutes modulo 100).
    pub fn mmss(&mut self, secs: u16, buf: u32, at: u32) {
        let (sixty, b, colon, place) =
            (self.k(60), self.k(buf), self.k(v5::text_indices(":")[0] as u32), self.k(at + 2));
        self.a.op(ST8, &[b, place, colon]);
        self.a.op(DIVU, &[R14, secs, sixty]);
        self.digits(R14, buf, at, 2);
        self.a.op(MODU, &[R14, secs, sixty]);
        self.digits(R14, buf, at + 3, 2);
    }

    /// Handles TouchBar gestures from either arm, and `broadcast` messages, in `handler` on both arms at the same
    /// moment: it's called with r0 the gesture (1 tap, 2 double tap, 3 triple tap, 4 hold, 5 release) or the
    /// message (keep messages 16 and up), and r1 that moment (this arm's clock). It ends with `RET`.
    pub fn events(&mut self, handler: &'static str) {
        let (msg, msg_t) = (self.var(0), self.var(0));
        self.message = Some((msg, msg_t));
        let lead = self.k(EVENT_LEAD_US);
        self.on(trigger::TOUCH, "_touch");
        self.on(trigger::PEER, "_peer");
        self.on(trigger::USER0, "_broadcast");
        self.a.label("_touch");
        self.read(R1, prop::TIME_US);
        self.read(R0, prop::TOUCH_GESTURE);
        self.set(prop::PEER_SEND, R0);
        self.a.op(ADD, &[R1, R1, lead]);
        self.a.op(SLEEP_UNTIL, &[R1]);
        self.a.jump(CALL, handler);
        self.a.op(END, &[]);
        self.a.label("_peer");
        self.read(R0, prop::PEER_MSG);
        self.read(R1, prop::PEER_T_US);
        self.a.op(ADD, &[R1, R1, lead]);
        self.a.op(SLEEP_UNTIL, &[R1]);
        self.a.jump(CALL, handler);
        self.a.op(END, &[]);
        self.a.label("_broadcast");
        self.a.op(MOV, &[R0, msg]);
        self.a.op(ADD, &[R1, msg_t, lead]);
        self.a.op(SLEEP_UNTIL, &[R1]);
        self.a.jump(CALL, handler);
        self.a.op(END, &[]);
    }

    /// Sends `value` (a word, 16 and up) to the other arm, and has `events`' handler take it on both arms at the
    /// same moment. Needs `events` first. One message at a time: a second before the first arrives replaces it.
    pub fn broadcast(&mut self, value: u16) {
        let (msg, msg_t) = self.message.expect("broadcast needs events() first");
        self.a.op(MOV, &[msg, value]);
        self.read(msg_t, prop::TIME_US);
        self.set(prop::PEER_SEND, value);
        let user0 = self.k(trigger::USER0 as u32);
        self.a.op(RAISE, &[user0]);
    }

    /// On the right arm, sets the brightness from the light sensor with Even's table plus `boost` (VM.md §6.7),
    /// changing it by two levels or more only; the left arm follows over the arm-to-arm link. Call it every second
    /// or so. Nothing on the left arm.
    pub fn brightness(&mut self, boost: u8) {
        if !self.right() {
            return;
        }
        let bright = match self.bright {
            Some(v) => v,
            None => {
                let v = self.var(u32::MAX);
                self.bright = Some(v);
                v
            }
        };
        let [one, three, k5, ten, k25, k41, k42, k43, k50, hundred, k200, k1000, k2000, boost, no_reading] =
            [1, 3, 5, 10, 25, 41, 42, 43, 50, 100, 200, 1000, 2000, boost as u32, 0x1000_0000].map(|v| self.k(v));
        let [done, u50, u200, u1000, u2000, add, capped] =
            ["bdone", "b50", "b200", "b1000", "b2000", "badd", "bcap"].map(|s| self.fresh(s));
        self.read(R12, prop::LIGHT);
        self.a.branch(BLTU, no_reading, R12, done);
        self.a.op(DIVU, &[R12, R12, hundred]);
        self.a.branch(BLTU, R12, k50, u50);
        self.a.branch(BLTU, R12, k200, u200);
        self.a.branch(BLTU, R12, k1000, u1000);
        self.a.branch(BLTU, R12, k2000, u2000);
        self.a.op(MOV, &[R13, k42]);
        self.a.jump(JMP, add);
        self.a.label(u50);
        self.a.op(DIVU, &[R13, R12, k5]);
        self.a.jump(JMP, add);
        self.a.label(u200);
        self.a.op(SUB, &[R13, R12, k50]);
        self.a.op(DIVU, &[R13, R13, ten]);
        self.a.op(ADD, &[R13, R13, ten]);
        self.a.jump(JMP, add);
        self.a.label(u1000);
        self.a.op(SUB, &[R13, R12, k200]);
        self.a.op(DIVU, &[R13, R13, k50]);
        self.a.op(ADD, &[R13, R13, k25]);
        self.a.jump(JMP, add);
        self.a.label(u2000);
        self.a.op(MOV, &[R13, k41]);
        self.a.label(add);
        self.a.op(ADD, &[R13, R13, boost]);
        self.a.branch(BLTU, R13, k43, capped);
        self.a.op(MOV, &[R13, k42]);
        self.a.label(capped);
        self.a.op(SUB, &[R14, R13, bright]);
        self.a.op(ADD, &[R14, R14, one]);
        self.a.branch(BLTU, R14, three, done);
        self.a.op(MOV, &[bright, R13]);
        self.set(prop::BRIGHTNESS, R13);
        self.a.label(done);
    }

    pub fn finish(self) -> Result<Program, String> {
        let mut triggers = [0u32; 16];
        for &(t, label) in &self.triggers {
            triggers[t as usize] = self.a.address(label);
        }
        let (code, pool) = self.a.finish()?;
        if POOL + pool.len() as u32 > VARS {
            return Err(format!("{} constants: more than fit", pool.len() / 4));
        }
        if VARS + 4 * self.inits.len() as u32 > DATA {
            return Err(format!("{} variables: more than fit", self.inits.len()));
        }
        if self.data_at > CODE {
            return Err(format!("{} bytes of buffers: more than fit", self.data_at - DATA));
        }
        if CODE + code.len() as u32 > TABLE {
            return Err(format!("{} bytes of code: more than fit", code.len()));
        }
        Ok(Program { code, pool, data: self.data, triggers, inits: self.inits })
    }
}

/// The part of x, y, w, h on the panel.
fn clamp(x: i32, y: i32, w: i32, h: i32) -> Option<[i32; 4]> {
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + w).min(v5::WIDTH as i32), (y + h).min(v5::HEIGHT as i32));
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}
