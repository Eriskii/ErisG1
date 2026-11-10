//! ScouterKit's protocol 5, which the g1v0 firmware speaks in place of Even's (scouter_kit's
//! `g1_firmware/docs/VM.md`, cited by section below). Each arm is a small machine: a 256 KiB arena that is its only
//! memory, a 640 × 480 panel of 16 grey levels, sensors, and an interpreter that runs the instructions the host sends
//! in numbered packets. Nothing here touches Bluetooth.

use uuid::Uuid;

use crate::proto::Side;

/// Packets of instructions.
pub const CMD: Uuid = Uuid::from_u128(0xf3da4d59_277a_4fc9_a1ed_41c9c22d4401);
/// Control requests, answered at once outside the packet order.
pub const CTRL: Uuid = Uuid::from_u128(0xf3da4d59_277a_4fc9_a1ed_41c9c22d4402);
/// Everything the arm sends: acknowledgements, errors, control replies, sensor events.
pub const EVT: Uuid = Uuid::from_u128(0xf3da4d59_277a_4fc9_a1ed_41c9c22d4403);

pub const PROTOCOL: u8 = 5;
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 480;
pub const ARENA: u32 = 262_144;
/// Packets the arm holds ahead of the one it runs next (§6.2).
pub const WINDOW: u16 = 32;

/// Where this program keeps things in the arena. r0–r14 are words 0–14, the operands of every instruction here.
pub const TEXT: u32 = 0x100;
/// A whole-panel 4-bit image that drawing goes into before `DRAW` copies part of it to the panel.
pub const FRAME: u32 = 0x1000;
pub const STRIDE: u32 = WIDTH / 2;
pub const FONT: u32 = FRAME + STRIDE * HEIGHT;

/// Reads an arm's advertised name: `G1v0_L`, or `G1v0S_L` in safe mode. Returns the side and whether it's in safe
/// mode.
pub fn parse_name(name: &str) -> Option<(Side, bool)> {
    let (safe, side) = match name.strip_prefix("G1v0S_") {
        Some(side) => (true, side),
        None => (false, name.strip_prefix("G1v0_")?),
    };
    match side {
        "L" => Some((Side::Left, safe)),
        "R" => Some((Side::Right, safe)),
        _ => None,
    }
}

// Instructions (§2). Operands name arena words; there are no immediate values, so the helpers below write their
// arguments into r0, r1, … first and pass those.

const WRITE: u8 = 0x01;
/// `END`: ends a program's task, or a packet.
pub const END: u8 = 0x33;
const SLEEP_UNTIL: u8 = 0x40;
const SYS_WRITE: u8 = 0x45;
const FILL: u8 = 0x50;
const GLYPHS: u8 = 0x52;
const DRAW: u8 = 0x53;
const LINE: u8 = 0x54;

/// `WRITE`: `data`, at most 255 bytes, to `addr`.
pub fn write(addr: u32, data: &[u8]) -> Vec<u8> {
    assert!(data.len() <= 255 && addr + data.len() as u32 <= ARENA);
    let mut code = vec![WRITE, addr as u8, (addr >> 8) as u8, (addr >> 16) as u8, data.len() as u8];
    code.extend_from_slice(data);
    code
}

/// Instruction `op` with arena words `words` as its operands: one nibble each, words 15 and up escaped to a u16
/// after the nibbles (§2.1).
fn instruction(op: u8, words: &[u16]) -> Vec<u8> {
    encode(op, words, &[])
}

/// Instruction `op`: the operand nibbles for `words`, then `literal` (a jump target), then the escaped words (§2.1).
pub fn encode(op: u8, words: &[u16], literal: &[u8]) -> Vec<u8> {
    let nibbles: Vec<u8> = words.iter().map(|&w| w.min(15) as u8).collect();
    let mut code = vec![op];
    code.extend(nibbles.chunks(2).map(|pair| pair[0] << 4 | pair.get(1).copied().unwrap_or(0)));
    code.extend_from_slice(literal);
    for &w in words.iter().filter(|&&w| w >= 15) {
        code.extend(w.to_le_bytes());
    }
    code
}

/// `op` on `values`, put in r0, r1, … by a `WRITE` just before.
fn with_registers(op: u8, values: &[u32]) -> Vec<u8> {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut code = write(0, &bytes);
    let registers: Vec<u16> = (0..values.len() as u16).collect();
    code.extend(instruction(op, &registers));
    code
}

/// `FILL`: the rectangle at `x`, `y` of the frame to `level` (§3.2).
pub fn fill(x: i32, y: i32, w: u32, h: u32, level: u8) -> Vec<u8> {
    with_registers(FILL, &[FRAME, STRIDE, x as u32, y as u32, w, h, level as u32])
}

/// `GLYPHS`: the text whose glyph indices are at `TEXT` (`n` bytes) in the font at `FONT`, its baseline at `y`,
/// drawn into the frame at `level` where the font has ink (§3.4).
pub fn glyphs(x: i32, y: i32, level: u8, n: usize) -> Vec<u8> {
    const MASK: u32 = 2;
    with_registers(GLYPHS, &[FONT, FRAME, STRIDE, x as u32, y as u32, MASK, level as u32, TEXT, n as u32])
}

/// `DRAW`: the rectangle at `x`, `y` of the frame to the same place on the panel, which then shows it (§3.5).
pub fn draw(x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
    with_registers(DRAW, &[FRAME, STRIDE, x as u32, y as u32, x as u32, y as u32, w, h])
}

// Arguments that never change, kept in words 16–19 by `fixed_args`, so `line_k` and `glyphs_k` send only the ones
// that do: about a quarter less per instruction.
const W_FRAME: u16 = 16;
const W_STRIDE: u16 = 17;
const W_FONT: u16 = 18;
const W_MASK: u16 = 19;

/// Puts the arguments `line_k` and `glyphs_k` take from the arena there. Send once per session.
pub fn fixed_args() -> Vec<u8> {
    let words: Vec<u8> = [FRAME, STRIDE, FONT, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
    write(W_FRAME as u32 * 4, &words)
}

/// `LINE`: the frame's pixels from `x0`, `y0` to `x1`, `y1`, both ends included, to `level` (§3.6), with the frame
/// from `fixed_args`.
pub fn line_k(x0: i32, y0: i32, x1: i32, y1: i32, level: u8) -> Vec<u8> {
    let values: Vec<u8> =
        [x0 as u32, y0 as u32, x1 as u32, y1 as u32, level as u32].iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut code = write(0, &values);
    code.extend(instruction(LINE, &[W_FRAME, W_STRIDE, 0, 1, 2, 3, 4]));
    code
}

/// `GLYPHS` of the `n` glyph indices at `text`, with the font, frame and text mode from `fixed_args`.
pub fn glyphs_k(x: i32, y: i32, level: u8, text: u32, n: usize) -> Vec<u8> {
    let values: Vec<u8> =
        [x as u32, y as u32, level as u32, text, n as u32].iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut code = write(0, &values);
    code.extend(instruction(GLYPHS, &[W_FONT, W_FRAME, W_STRIDE, 0, 1, W_MASK, 2, 3, 4]));
    code
}

/// `SLEEP_UNTIL`: the rest of the packet, and every packet after it, waits until the arm's clock reads `t_us`, at most
/// 2 s ahead (§2.6).
pub fn sleep_until(t_us: u32) -> Vec<u8> {
    with_registers(SLEEP_UNTIL, &[t_us])
}

/// `SYS_WRITE`: property `prop` to `value` (§2.5).
pub fn set(prop: u8, value: u32) -> Vec<u8> {
    with_registers(SYS_WRITE, &[prop as u32, value])
}

/// `CALL`: runs the code stored at `addr` as part of the packet, until its `RET` (§6.2).
pub fn call(addr: u32) -> Vec<u8> {
    vec![0x31, addr as u8, (addr >> 8) as u8, (addr >> 16) as u8]
}

/// Uploads `data` to `addr` as `WRITE`s that each fit in a packet of `max_code` bytes of instructions.
pub fn upload(addr: u32, data: &[u8], max_code: usize) -> Vec<Vec<u8>> {
    let chunk = (max_code - 5).min(255);
    data.chunks(chunk).enumerate().map(|(i, part)| write(addr + (i * chunk) as u32, part)).collect()
}

/// Puts `instructions` into as few packets of at most `max_code` bytes as keep each instruction whole.
pub fn pack(instructions: impl IntoIterator<Item = Vec<u8>>, max_code: usize) -> Vec<Vec<u8>> {
    let mut packets: Vec<Vec<u8>> = Vec::new();
    for code in instructions {
        assert!(code.len() <= max_code, "a {}-byte instruction doesn't fit a packet", code.len());
        match packets.last_mut() {
            Some(packet) if packet.len() + code.len() <= max_code => packet.extend(code),
            _ => packets.push(code),
        }
    }
    packets
}

/// A `CMD` write: the packet's sequence number, then its instructions.
pub fn packet(seq: u16, code: &[u8]) -> Vec<u8> {
    let mut packet = seq.to_le_bytes().to_vec();
    packet.extend_from_slice(code);
    packet
}

/// Control requests (§6.4).
pub mod ctrl {
    pub const HELLO: u8 = 0x01;
    pub const PING: u8 = 0x02;
    pub const LINK: u8 = 0x05;
    pub const REBOOT: u8 = 0x08;
    pub const PROPS: u8 = 0x0A;
    pub const PANEL_READ: u8 = 0x0B;
    pub const ARENA_CRC: u8 = 0x0C;
    pub const VM_START: u8 = 0x10;
    pub const VM_STOP: u8 = 0x11;
    pub const VM_STATUS: u8 = 0x12;
    pub const EVENTS: u8 = 0x14;
}

/// A `CTRL` write.
pub fn control(op: u8, tag: u8, body: &[u8]) -> Vec<u8> {
    let mut request = vec![op, tag];
    request.extend_from_slice(body);
    request
}

/// Properties (§2.5).
pub mod prop {
    pub const TIME_US: u8 = 0x00;
    pub const LINK_INTERVAL: u8 = 0x05;
    pub const BATT_MV: u8 = 0x10;
    pub const BATT_PCT: u8 = 0x11;
    pub const CHARGE: u8 = 0x12;
    pub const IN_CASE: u8 = 0x13;
    pub const WORN: u8 = 0x14;
    pub const BATT_MA: u8 = 0x15;
    pub const BATT_TEMP: u8 = 0x16;
    pub const ACC_X: u8 = 0x18;
    pub const ACC_Y: u8 = 0x19;
    pub const ACC_Z: u8 = 0x1A;
    pub const PITCH: u8 = 0x1B;
    pub const LIGHT: u8 = 0x1C;
    pub const ROLL: u8 = 0x1D;
    pub const BRIGHTNESS: u8 = 0x20;
    pub const PANEL: u8 = 0x21;
    pub const TIMER: u8 = 0x22;
    pub const IMU_RATE: u8 = 0x28;
    pub const BRIGHTNESS_CAP: u8 = 0x2A;
    pub const GYR_X: u8 = 0x30;
    pub const GYR_Y: u8 = 0x31;
    pub const GYR_Z: u8 = 0x32;
    pub const GYRO_RATE: u8 = 0x33;
    pub const TOUCH_GESTURE: u8 = 0x34;
    pub const AUTO_BRIGHTNESS: u8 = 0x38;
    pub const PEER_LINK: u8 = 0x39;
    pub const PEER_SEND: u8 = 0x3A;
    pub const PEER_MSG: u8 = 0x3B;
    pub const PEER_T_US: u8 = 0x3C;
}

/// What a property reads when the arm has no value for it.
pub const UNKNOWN: u32 = 0x8000_0000;

/// Triggers (§5.2): what starts a program's handlers, and what the arm can report as `INPUT` events, by bit in the
/// `EVENTS` mask.
pub mod trigger {
    pub const START: u8 = 0;
    pub const TIMER: u8 = 1;
    pub const MOTION: u8 = 2;
    pub const TILT: u8 = 3;
    pub const LIGHT: u8 = 4;
    pub const BATTERY: u8 = 5;
    pub const CASE: u8 = 6;
    pub const WEAR: u8 = 7;
    pub const TOUCH: u8 = 8;
    pub const PEER: u8 = 9;
    pub const USER0: u8 = 12;

    pub fn name(id: u8) -> &'static str {
        match id {
            MOTION => "motion",
            TILT => "tilt",
            LIGHT => "light",
            BATTERY => "battery",
            CASE => "case",
            WEAR => "wear",
            TOUCH => "touch",
            START => "start",
            TIMER => "timer",
            PEER => "peer",
            _ => "?",
        }
    }
}

pub fn gesture_name(value: u32) -> &'static str {
    match value {
        1 => "tap",
        2 => "double tap",
        3 => "triple tap",
        4 => "long press",
        5 => "long press released",
        6 => "slide forward",
        7 => "slide back",
        _ => "?",
    }
}

/// The error codes of `ERROR`, `HALT` and control replies (§1).
pub fn error_name(code: u8) -> &'static str {
    match code {
        1 => "malformed",
        2 => "invalid op",
        3 => "outside the arena",
        4 => "bad PackBits data",
        5 => "argument out of range",
        6 => "outside the receive window",
        7 => "safe mode",
        8 => "font",
        9 => "stack",
        10 => "budget",
        11 => "no panel",
        12 => "microphone unavailable",
        13 => "asset",
        _ => "?",
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    /// Random and new at every boot of the arm: a change means it restarted and forgot everything.
    pub epoch: u32,
    /// The arm's clock, µs since boot.
    pub t_us: u32,
    pub kind: EventKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EventKind {
    Ack {
        rx_next: u16,
        exec_next: u16,
        credits: u8,
        sack: u32,
    },
    Error {
        seq: u16,
        offset: u8,
        op: u8,
        code: u8,
        detail: u32,
    },
    /// A program stopped on an error.
    Halt {
        address: u32,
        op: u8,
        code: u8,
        detail: u32,
    },
    Notify {
        count: u16,
        a: u32,
        b: u32,
    },
    Notice {
        kind: u8,
        body: Vec<u8>,
    },
    Input {
        trigger: u8,
        value: u32,
    },
    Reply {
        op: u8,
        tag: u8,
        status: u8,
        body: Vec<u8>,
    },
    Other(u8),
}

/// Reads an `EVT` notification (§6.3).
pub fn event(data: &[u8]) -> Option<Event> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (&kind, epoch, t_us) = (data.first()?, u32_at(1)?, u32_at(5)?);
    let kind = match kind {
        0x01 => {
            EventKind::Ack { rx_next: u16_at(9)?, exec_next: u16_at(11)?, credits: *data.get(13)?, sack: u32_at(14)? }
        }
        0x02 => EventKind::Error {
            seq: u16_at(9)?,
            offset: *data.get(11)?,
            op: *data.get(12)?,
            code: *data.get(13)?,
            detail: u32_at(14)?,
        },
        0x03 => EventKind::Halt { address: u32_at(10)?, op: *data.get(14)?, code: *data.get(15)?, detail: u32_at(16)? },
        0x04 => EventKind::Notify { count: u16_at(9)?, a: u32_at(12)?, b: u32_at(16)? },
        0x05 => EventKind::Notice { kind: *data.get(9)?, body: data[10..].to_vec() },
        0x07 => EventKind::Input { trigger: *data.get(11)?, value: u32_at(12)? },
        op @ 0x80.. => EventKind::Reply {
            op: op - 0x80,
            tag: *data.get(9)?,
            status: *data.get(10)?,
            body: data.get(11..)?.to_vec(),
        },
        other => EventKind::Other(other),
    };
    Some(Event { epoch, t_us, kind })
}

/// The arm's answer to `HELLO` (§6.4).
#[derive(Debug, Clone)]
pub struct Hello {
    pub version: String,
    pub flags: u8,
    pub board: u8,
    pub max_packet: u16,
    pub rx_next: u16,
    pub exec_next: u16,
    pub boot_count: u32,
    pub unhealthy_boots: u8,
}

impl Hello {
    pub fn parse(body: &[u8]) -> Result<Hello, String> {
        if body.first() != Some(&PROTOCOL) {
            return Err(format!("the arm speaks protocol {:?}, not {PROTOCOL}", body.first()));
        }
        if body.len() < 35 {
            return Err(format!("HELLO reply is {} bytes, not 35", body.len()));
        }
        let u16_at = |at: usize| u16::from_le_bytes([body[at], body[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
        Ok(Hello {
            version: format!("{}.{}.{}", body[1], body[2], body[3]),
            flags: body[5],
            board: body[6],
            max_packet: u16_at(18),
            rx_next: u16_at(20),
            exec_next: u16_at(22),
            boot_count: u32_at(25),
            unhealthy_boots: body[34],
        })
    }

    pub fn safe_mode(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn panel_ok(&self) -> bool {
        self.flags & 2 != 0
    }

    pub fn encrypted(&self) -> bool {
        self.flags & 64 != 0
    }

    pub fn in_case(&self) -> bool {
        self.flags & 128 != 0
    }

    pub fn bondable(&self) -> bool {
        self.board & 2 != 0
    }
}

/// `seq` comes before `other` in 16-bit sequence arithmetic.
pub fn before(seq: u16, other: u16) -> bool {
    (seq.wrapping_sub(other) as i16) < 0
}

// Text. The glasses draw text from a font blob in the arena (§4), so this program makes one from the same 8 × 8
// font the stock-firmware images use, drawn `scale` times over.

/// The characters of the font, in the protocol's glyph order (§4.1): `0`–`9`, `A`–`Z`, `a`–`z`, then the rest of
/// printable ASCII by codepoint.
fn font_chars() -> Vec<char> {
    let mut chars: Vec<char> = ('0'..='9').chain('A'..='Z').chain('a'..='z').collect();
    chars.extend((' '..='~').filter(|c| !c.is_ascii_alphanumeric()));
    chars
}

/// The glyph `c` draws as: itself, or `?`.
fn glyph_index(c: char) -> u8 {
    let chars = font_chars();
    let find = |c| chars.iter().position(|&f| f == c);
    find(c).or_else(|| find('?')).unwrap() as u8
}

/// `text` as glyph indices, one byte each (all are below 128, so each varint is one byte).
pub fn text_indices(text: &str) -> Vec<u8> {
    text.chars().map(glyph_index).collect()
}

/// The font blob (§4.2): the 8 × 8 font at `scale`, every glyph `8·scale` square and that wide apart.
pub fn font(scale: u8) -> Vec<u8> {
    use font8x8::{BASIC_FONTS, UnicodeFonts};
    let chars = font_chars();
    let size = 8 * scale as usize;
    let n = chars.len();
    let bitmaps_start = 16 + 16 * n;
    let glyph_bytes = size * size / 8;
    let total = bitmaps_start + n * glyph_bytes;

    let mut blob = Vec::with_capacity(total);
    blob.extend(b"SKF");
    blob.push(1);
    blob.extend((total as u32).to_le_bytes());
    blob.extend((n as u16).to_le_bytes());
    blob.extend(0u16.to_le_bytes()); // no kerning
    let ascent = 7 * scale;
    blob.extend([ascent, scale, 9 * scale, 0]);
    for i in 0..n {
        let bits = ((bitmaps_start + i * glyph_bytes) * 8) as u32;
        blob.extend(bits.to_le_bytes());
        blob.extend([size as u8, size as u8, size as u8, 0]);
        blob.extend(0i16.to_le_bytes());
        blob.extend((-(ascent as i16)).to_le_bytes());
    }
    for &c in &chars {
        blob.extend((c as u32).to_le_bytes());
    }
    for &c in &chars {
        let glyph = BASIC_FONTS.get(c).unwrap_or([0; 8]);
        // `size` rows of `size` bits, most significant first. In font8x8, bit 0 is the leftmost pixel.
        let mut bits = vec![0u8; glyph_bytes];
        for y in 0..size {
            for x in 0..size {
                if glyph[y / scale as usize] & (1 << (x / scale as usize)) != 0 {
                    let bit = y * size + x;
                    bits[bit / 8] |= 0x80 >> (bit % 8);
                }
            }
        }
        blob.extend(bits);
    }
    debug_assert_eq!(blob.len(), total);
    blob
}

/// The panel as the arm holds it: a 4-bit image the host keeps in step, to check `PANEL_READ` against.
pub struct Mirror {
    pub pixels: Vec<u8>,
}

impl Mirror {
    pub fn new() -> Mirror {
        Mirror { pixels: vec![0; (STRIDE * HEIGHT) as usize] }
    }

    /// What `fill` does to the frame, clipped the same way.
    pub fn fill(&mut self, x: i32, y: i32, w: u32, h: u32, level: u8) {
        let (x0, y0) = (x.max(0) as u32, y.max(0) as u32);
        let x1 = (x + w as i32).clamp(0, WIDTH as i32) as u32;
        let y1 = (y + h as i32).clamp(0, HEIGHT as i32) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let byte = &mut self.pixels[(y * STRIDE + x / 2) as usize];
                *byte = if x % 2 == 0 { (*byte & 0x0F) | level << 4 } else { (*byte & 0xF0) | level };
            }
        }
    }

    /// zlib's CRC-32 of rows `y0`… as `PANEL_READ` reports it.
    pub fn crc(&self, y0: u32, rows: u32) -> u32 {
        crate::proto::crc32(&self.pixels[(y0 * STRIDE) as usize..((y0 + rows) * STRIDE) as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_spec_example() {
        // §2.1: ADD r3, r1, [0x0120] = 11 31 f0 20 01
        assert_eq!(instruction(0x11, &[3, 1, 0x0120]), [0x11, 0x31, 0xf0, 0x20, 0x01]);
    }

    #[test]
    fn passes_arguments_in_registers() {
        let code = fill(1, 2, 3, 4, 15);
        // WRITE 7 words at 0, then FILL r0–r6: four nibble bytes, the last padded.
        assert_eq!(&code[..5], &[WRITE, 0, 0, 0, 28]);
        assert_eq!(u32::from_le_bytes(code[5..9].try_into().unwrap()), FRAME);
        assert_eq!(&code[33..], &[FILL, 0x01, 0x23, 0x45, 0x60]);
    }

    #[test]
    fn packs_instructions_whole() {
        let packets = pack([vec![1; 100], vec![2; 100], vec![3; 100]], 242);
        assert_eq!(packets.iter().map(Vec::len).collect::<Vec<_>>(), [200, 100]);
        let parts = upload(0x2000, &[7; 600], 242);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.len() <= 242));
        assert_eq!(&parts[1][..4], &[WRITE, 0xED, 0x20, 0x00]);
    }

    #[test]
    fn reads_names_and_events() {
        assert_eq!(parse_name("G1v0_L"), Some((Side::Left, false)));
        assert_eq!(parse_name("G1v0S_R"), Some((Side::Right, true)));
        assert_eq!(parse_name("Even G1_93_L_57DE02"), None);

        let mut ack = vec![0x01, 1, 0, 0, 0, 2, 0, 0, 0, 5, 0, 4, 0, 31, 0b10, 0, 0, 0];
        assert_eq!(
            event(&ack).unwrap(),
            Event { epoch: 1, t_us: 2, kind: EventKind::Ack { rx_next: 5, exec_next: 4, credits: 31, sack: 2 } }
        );
        ack[0] = 0x81;
        ack.truncate(11);
        assert!(matches!(event(&ack).unwrap().kind, EventKind::Reply { op: 1, tag: 5, status: 0, .. }));
        assert!(before(0xFFFF, 1) && !before(1, 0xFFFF));
    }

    #[test]
    fn builds_a_valid_font() {
        let blob = font(3);
        let n = u16::from_le_bytes([blob[8], blob[9]]) as usize;
        assert_eq!(&blob[..4], b"SKF\x01");
        assert_eq!(u32::from_le_bytes(blob[4..8].try_into().unwrap()) as usize, blob.len());
        assert_eq!(n, 95);
        assert_eq!(text_indices("0Aa :"), [0, 10, 36, 62, glyph_index(':')]);
        assert_eq!(glyph_index('é'), glyph_index('?'));
        // Every glyph's bitmap ends inside the blob.
        for i in 0..n {
            let entry = &blob[16 + 12 * i..];
            let bits = u32::from_le_bytes(entry[..4].try_into().unwrap()) as usize;
            assert!(bits + entry[4] as usize * entry[5] as usize <= blob.len() * 8);
        }
    }

    #[test]
    fn mirrors_fills() {
        let mut mirror = Mirror::new();
        mirror.fill(-5, 0, 7, 1, 0xF);
        assert_eq!(&mirror.pixels[..2], &[0xFF, 0x00]);
        mirror.fill(639, 479, 10, 10, 0xA);
        assert_eq!(*mirror.pixels.last().unwrap(), 0x0A);
    }
}
