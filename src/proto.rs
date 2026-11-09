//! The G1's wire format: what to write to an arm and what its notifications mean. Nothing here touches Bluetooth, so
//! the Android app can share it.
//!
//! Each arm is its own peripheral with a Nordic UART service. Commands are written to TX and replies arrive as
//! notifications on RX, with the command byte echoed first. Byte values come from Even's EvenDemoApp and
//! Gadgetbridge's G1 driver.

use uuid::Uuid;

pub const TX: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e);
pub const RX: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e);

/// The glasses never send a packet longer than this, though the MTU is 251 and bitmap packets run to 200 bytes.
pub const MAX_PACKET: usize = 180;

/// The display, in pixels. Bitmaps are always this size, one bit per pixel.
pub const WIDTH: usize = 576;
pub const HEIGHT: usize = 136;

pub const ACK: u8 = 0xC9;

pub const INIT: u8 = 0x4D;
pub const SILENT_MODE_GET: u8 = 0x2B;
pub const BATTERY_GET: u8 = 0x2C;
pub const BRIGHTNESS_SET: u8 = 0x01;
pub const BRIGHTNESS_GET: u8 = 0x29;
pub const BRIGHTNESS_MAX: u8 = 42;
pub const HEAD_UP_ANGLE_SET: u8 = 0x0B;
pub const HEAD_UP_ANGLE_GET: u8 = 0x32;
pub const HEAD_UP_ANGLE_MAX: u8 = 60;
pub const EXIT: u8 = 0x18;
pub const HARDWARE_SET: u8 = 0x26;
pub const DISPLAY_GET: u8 = 0x3B;
pub const DISPLAY_HEIGHT_MAX: u8 = 8;
pub const SYSTEM: u8 = 0x23;
pub const FIRMWARE_REPLY: u8 = 0x6E;
pub const TEXT: u8 = 0x4E;
pub const BITMAP_DATA: u8 = 0x15;
pub const BITMAP_CRC: u8 = 0x16;
pub const BITMAP_END: u8 = 0x20;
pub const EVENT: u8 = 0xF5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn letter(self) -> char {
        match self {
            Side::Left => 'L',
            Side::Right => 'R',
        }
    }
}

/// Reads an arm's advertised name, like `Even G1_93_L_57DE02`, as its channel and side. The two arms of one pair share
/// a channel.
pub fn parse_name(name: &str) -> Option<(u32, Side)> {
    let start = name.find("G1_")?;
    let mut parts = name[start + 3..].split('_');
    let channel = parts.next()?.parse().ok()?;
    let side = match parts.next()? {
        "L" => Side::Left,
        "R" => Side::Right,
        _ => return None,
    };
    Some((channel, side))
}

/// Puts an arm back in its normal work mode. An arm whose ATT MTU is negotiated to exactly 498 switches itself to
/// firmware-update mode, which turns off the link between the arms, the IMU, touch and the mic, and stops bitmaps
/// redrawing. macOS always negotiates 498. (Firmware 1.66, `mtu_updated` in ble_ancs.c; `0x23` is `#`, the prefix of
/// the firmware's text debug commands.)
pub fn normal_mode() -> Vec<u8> {
    b"#s0".to_vec()
}

/// The first thing every app writes to each arm. Gadgetbridge calls it setting the MTU.
pub fn init() -> Vec<u8> {
    vec![INIT, 0x01]
}

/// The official app's heartbeat. The glasses drop the link after about 30 seconds of silence, and any command will do.
pub fn silent_mode_get() -> Vec<u8> {
    vec![SILENT_MODE_GET]
}

pub fn brightness_get() -> Vec<u8> {
    vec![BRIGHTNESS_GET]
}

/// `level` runs from 0 to `BRIGHTNESS_MAX`. With `auto` on, the glasses adjust from there by ambient light.
pub fn brightness_set(level: u8, auto: bool) -> Vec<u8> {
    vec![BRIGHTNESS_SET, level.min(BRIGHTNESS_MAX), auto as u8]
}

/// The level and whether auto brightness is on, from a `brightness_get` reply.
pub fn brightness(reply: &[u8]) -> Option<(u8, bool)> {
    match reply {
        [BRIGHTNESS_GET, _, level, rest @ ..] => Some((*level, rest.first() == Some(&1))),
        _ => None,
    }
}

pub fn head_up_angle_get() -> Vec<u8> {
    vec![HEAD_UP_ANGLE_GET]
}

/// How far, in degrees from 0 to `HEAD_UP_ANGLE_MAX`, the head tilts up before the glasses report `Event::HeadUp`. The
/// official app only sends this to the right arm, which reports head movement.
pub fn head_up_angle_set(degrees: u8) -> Vec<u8> {
    // Gadgetbridge doesn't know what the trailing 1 means either.
    vec![HEAD_UP_ANGLE_SET, degrees.min(HEAD_UP_ANGLE_MAX), 0x01]
}

pub fn head_up_angle(reply: &[u8]) -> Option<u8> {
    match reply {
        [HEAD_UP_ANGLE_GET, _, degrees, ..] => Some(*degrees),
        _ => None,
    }
}

pub fn display_get() -> Vec<u8> {
    vec![DISPLAY_GET]
}

/// Where the image sits: `height` from 0 (Gadgetbridge's lowest slider step) to `DISPLAY_HEIGHT_MAX`, and `depth`,
/// how far away it appears. `preview` is how the official app sends it while you drag the slider. `seq` is echoed in
/// the reply.
pub fn display_set(seq: u8, preview: bool, height: u8, depth: u8) -> Vec<u8> {
    vec![HARDWARE_SET, 0x08, 0x00, seq, 0x02, preview as u8, height.min(DISPLAY_HEIGHT_MAX), depth]
}

/// Height and depth from a `display_get` reply.
pub fn display(reply: &[u8]) -> Option<(u8, u8)> {
    match reply {
        [DISPLAY_GET, _, height, depth, ..] => Some((*height, *depth)),
        _ => None,
    }
}

/// Restarts an arm. It doesn't answer.
pub fn reboot() -> Vec<u8> {
    vec![SYSTEM, 0x72]
}

/// Leaves whatever is on screen, ours or the firmware's.
pub fn exit() -> Vec<u8> {
    vec![EXIT]
}

pub fn battery_get() -> Vec<u8> {
    vec![BATTERY_GET, 0x01]
}

pub fn firmware_get() -> Vec<u8> {
    vec![SYSTEM, 0x74]
}

/// The packets that put `text` on the display as a new screen. The glasses don't wrap lines, so break them with `\n`.
pub fn text(seq: u8, text: &str) -> Vec<Vec<u8>> {
    const HEADER: usize = 9;
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() || chunks.is_empty() {
        let mut end = rest.len().min(MAX_PACKET - HEADER);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&rest[..end]);
        rest = &rest[end..];
    }
    let total = chunks.len() as u8;
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            // 0x71: text mode, new content. Then character position, page and page count.
            let mut packet = vec![TEXT, seq, total, i as u8, 0x71, 0x00, 0x00, 0x01, 0x01];
            packet.extend_from_slice(chunk.as_bytes());
            packet
        })
        .collect()
}

/// Where bitmap data goes in the glasses' memory. It leads the first packet and the CRC.
const BITMAP_ADDRESS: [u8; 4] = [0x00, 0x1C, 0x00, 0x00];

/// A full-screen image as a 1-bit BMP file, laid out exactly like the ones in Even's demo: the glasses refuse files
/// that are two bytes shorter. `lit(x, y)` says which pixels glow; y = 0 is the top row.
pub fn bitmap(lit: impl Fn(usize, usize) -> bool) -> Vec<u8> {
    const HEADER: u32 = 14 + 40 + 8;
    const TRAILER: u32 = 2;
    let row_bytes = WIDTH.div_ceil(32) * 4;
    let pixel_bytes = (row_bytes * HEIGHT) as u32;
    let mut bmp = Vec::with_capacity((HEADER + pixel_bytes + TRAILER) as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(HEADER + pixel_bytes + TRAILER).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&HEADER.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(WIDTH as i32).to_le_bytes());
    bmp.extend_from_slice(&(HEIGHT as i32).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&1u16.to_le_bytes()); // bits per pixel
    bmp.extend_from_slice(&0u32.to_le_bytes()); // uncompressed
    bmp.extend_from_slice(&(pixel_bytes + TRAILER).to_le_bytes());
    bmp.extend_from_slice(&2834i32.to_le_bytes());
    bmp.extend_from_slice(&2834i32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    // Colour 0 is lit: the glasses light clear bits whatever the table says.
    bmp.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00]);
    // BMP rows run bottom to top.
    for y in (0..HEIGHT).rev() {
        let mut row = vec![0xFFu8; row_bytes];
        for x in 0..WIDTH {
            if lit(x, y) {
                row[x / 8] &= !(0x80 >> (x % 8));
            }
        }
        bmp.extend_from_slice(&row);
    }
    bmp.extend_from_slice(&[0; TRAILER as usize]);
    bmp
}

/// The data packets that carry `bmp`, then the end marker. Send them in order to one arm; the end marker is answered.
pub fn bitmap_packets(bmp: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let packets = bmp
        .chunks(194)
        .enumerate()
        .map(|(i, chunk)| {
            let mut packet = vec![BITMAP_DATA, i as u8];
            if i == 0 {
                packet.extend_from_slice(&BITMAP_ADDRESS);
            }
            packet.extend_from_slice(chunk);
            packet
        })
        .collect();
    (packets, vec![BITMAP_END, 0x0D, 0x0E])
}

/// An end marker that isn't one: the arm answers with an error but resets its count of bitmap packets.
pub fn bitmap_reset_counter() -> Vec<u8> {
    vec![BITMAP_END, 0x00, 0x00]
}

/// A CRC for no file: the arm answers with an error and drops whatever it had half stored.
pub fn bitmap_reset_file() -> Vec<u8> {
    vec![BITMAP_CRC, 0, 0, 0, 0]
}

/// The check that makes the glasses show a sent bitmap. Its reply has `ACK` at byte 5 when the image arrived whole.
pub fn bitmap_crc(bmp: &[u8]) -> Vec<u8> {
    let mut crc = Crc32::default();
    crc.update(&BITMAP_ADDRESS);
    crc.update(bmp);
    let mut packet = vec![BITMAP_CRC];
    packet.extend_from_slice(&crc.finish().to_be_bytes());
    packet
}

/// CRC-32 as zlib computes it, which crclib (used by Even's demo) calls CRC-32/XZ.
struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Crc32(0xFFFF_FFFF)
    }
}

impl Crc32 {
    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.0 ^= byte as u32;
            for _ in 0..8 {
                self.0 = if self.0 & 1 == 1 { (self.0 >> 1) ^ 0xEDB8_8320 } else { self.0 >> 1 };
            }
        }
    }

    fn finish(&self) -> u32 {
        !self.0
    }
}

pub fn battery_percent(reply: &[u8]) -> Option<u8> {
    (reply.first() == Some(&BATTERY_GET)).then(|| reply.get(2).copied()).flatten()
}

pub fn firmware_string(reply: &[u8]) -> Option<String> {
    (reply.first() == Some(&FIRMWARE_REPLY)).then(|| String::from_utf8_lossy(reply).trim().to_string())
}

/// Something the glasses reported without being asked.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Tap,
    DoubleTap,
    LongPressStart,
    LongPressEnd,
    HeadUp,
    HeadDown,
    SilentMode(bool),
    Worn,
    NotWorn,
    InCase { lid_open: bool },
    Charging(bool),
    Battery(u8),
    CaseCharging(bool),
    CaseBattery(u8),
    Other(Vec<u8>),
}

/// Reads an `EVENT` notification. Taps arrive from the arm that was tapped.
pub fn event(data: &[u8]) -> Option<Event> {
    if data.first() != Some(&EVENT) {
        return None;
    }
    let arg = data.get(2).copied();
    Some(match (data.get(1).copied()?, arg) {
        (0x01, _) => Event::Tap,
        (0x00 | 0x20, _) => Event::DoubleTap,
        (0x17, _) => Event::LongPressStart,
        (0x18, _) => Event::LongPressEnd,
        (0x02, _) => Event::HeadUp,
        (0x03, _) => Event::HeadDown,
        (0x04, _) => Event::SilentMode(true),
        (0x05, _) => Event::SilentMode(false),
        (0x06, _) => Event::Worn,
        (0x07, _) => Event::NotWorn,
        (0x08, _) => Event::InCase { lid_open: true },
        (0x0B, _) => Event::InCase { lid_open: false },
        (0x09, Some(on)) => Event::Charging(on != 0),
        (0x0A, Some(level)) => Event::Battery(level),
        (0x0E, Some(on)) => Event::CaseCharging(on != 0),
        (0x0F, Some(level)) => Event::CaseBattery(level),
        _ => Event::Other(data.to_vec()),
    })
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(parse_name("Even G1_93_L_57DE02"), Some((93, Side::Left)));
        assert_eq!(parse_name("G1_45_R_92333"), Some((45, Side::Right)));
        assert_eq!(parse_name("Nothing Headphone"), None);
        assert_eq!(parse_name("Even G1_93_X_57DE02"), None);
    }

    #[test]
    fn short_text_is_one_packet() {
        let packets = text(3, "hi");
        assert_eq!(packets, vec![vec![TEXT, 3, 1, 0, 0x71, 0, 0, 1, 1, b'h', b'i']]);
    }

    #[test]
    fn long_text_splits_on_char_boundaries() {
        let long = "é".repeat(200);
        let packets = text(0, &long);
        assert!(packets.iter().all(|p| p.len() <= MAX_PACKET));
        assert_eq!(packets.len(), 3);
        assert!(packets.iter().enumerate().all(|(i, p)| p[2] == 3 && p[3] == i as u8));
        let joined: Vec<u8> = packets.iter().flat_map(|p| p[9..].to_vec()).collect();
        assert_eq!(String::from_utf8(joined).unwrap(), long);
    }

    #[test]
    fn crc_matches_the_standard_check_value() {
        let mut crc = Crc32::default();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    #[test]
    fn bitmap_layout() {
        // Only the top-left pixel lit: it lands in the last row of the file, first bit, cleared.
        let bmp = bitmap(|x, y| x == 0 && y == 0);
        assert_eq!(bmp.len(), 9856);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp[62 + 72 * (HEIGHT - 1)], 0x7F);
        assert!(bmp[62..62 + 72 * (HEIGHT - 1)].iter().all(|&b| b == 0xFF));
        let (packets, end) = bitmap_packets(&bmp);
        assert_eq!(&packets[0][..6], &[BITMAP_DATA, 0, 0x00, 0x1C, 0x00, 0x00]);
        assert_eq!(packets.iter().map(|p| p.len() - 2).sum::<usize>(), bmp.len() + 4);
        assert_eq!(end, vec![BITMAP_END, 0x0D, 0x0E]);
    }

    #[test]
    fn brightness_round_trip() {
        assert_eq!(brightness_set(99, true), vec![BRIGHTNESS_SET, BRIGHTNESS_MAX, 1]);
        assert_eq!(brightness(&[BRIGHTNESS_GET, 0x65, 3, 0, 0]), Some((3, false)));
        assert_eq!(brightness(&[BRIGHTNESS_GET, 0x65, 20, 1]), Some((20, true)));
        assert_eq!(brightness(&[BATTERY_GET, 0x65, 20, 1]), None);
    }

    #[test]
    fn head_up_angle_round_trip() {
        assert_eq!(head_up_angle_set(90), vec![HEAD_UP_ANGLE_SET, HEAD_UP_ANGLE_MAX, 1]);
        assert_eq!(head_up_angle(&[HEAD_UP_ANGLE_GET, 0x6D, 20, 1]), Some(20));
        assert_eq!(head_up_angle(&[BRIGHTNESS_GET, 0x6D, 20, 1]), None);
    }

    #[test]
    fn display_round_trip() {
        assert_eq!(display_set(7, true, 9, 3), vec![HARDWARE_SET, 8, 0, 7, 2, 1, DISPLAY_HEIGHT_MAX, 3]);
        assert_eq!(display(&[DISPLAY_GET, ACK, 3, 3]), Some((3, 3)));
    }

    #[test]
    fn events() {
        assert_eq!(event(&[EVENT, 0x01]), Some(Event::Tap));
        assert_eq!(event(&[EVENT, 0x0A, 0x40]), Some(Event::Battery(64)));
        assert_eq!(event(&[EVENT, 0x99]), Some(Event::Other(vec![EVENT, 0x99])));
        assert_eq!(event(&[TEXT, ACK]), None);
    }
}
