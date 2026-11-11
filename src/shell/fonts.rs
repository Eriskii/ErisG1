//! The shell's fonts: the 8 × 8 one (`v5::font`), and Departure Mono, a pixel font that reads better small, with
//! emoji (Noto Emoji, black and white) for the tiles that show messages. `assets/fonts/` holds them as data
//! (`tools/fonts.py` makes it); each size is the 11 px one drawn so many times over, the emoji drawn at each size.
//!
//! Every font has the glyphs in the same order (`v5::text_indices`: digits first, so a digit's glyph is its value),
//! so the programs' clocks and batteries work with either. A font with emoji has them from glyph 128 on: each is two
//! bytes of text (a varint) and two cells wide, so a line's bytes are its cells.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::v5;

const DEPARTURE: &[u8] = include_bytes!("../../assets/fonts/departure_mono.bin");
const EMOJI: [&[u8]; 2] =
    [include_bytes!("../../assets/fonts/emoji_1.bin"), include_bytes!("../../assets/fonts/emoji_2.bin")];
/// Departure Mono's cell at 11 px: 7 wide, 9 above the baseline and 3 below.
const ADVANCE: i32 = 7;
const ASCENT: i32 = 9;
const DESCENT: i32 = 3;
/// The first emoji's glyph: the first that takes two bytes.
const FIRST_EMOJI: u32 = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Face {
    /// The 8 × 8 font, 8 px at size 1.
    #[default]
    Pixel,
    /// Departure Mono, 11 px at size 1 (a cell 7 × 12).
    Departure,
}

/// A font's cell at a size, pixels: how far apart characters are, how far they reach above and below the
/// baseline, and how far apart lines are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Metrics {
    pub advance: i32,
    pub ascent: i32,
    pub descent: i32,
    pub pitch: i32,
}

impl Metrics {
    pub fn height(&self) -> i32 {
        self.ascent + self.descent
    }

    /// Between one line's cell and the next's.
    pub fn gap(&self) -> i32 {
        self.pitch - self.height()
    }
}

impl Face {
    pub fn metrics(self, scale: u8) -> Metrics {
        let s = scale as i32;
        match self {
            Face::Pixel => Metrics { advance: 8 * s, ascent: 7 * s, descent: s, pitch: 10 * s },
            Face::Departure => Metrics {
                advance: ADVANCE * s,
                ascent: ASCENT * s,
                descent: DESCENT * s,
                pitch: (ASCENT + DESCENT + 2) * s,
            },
        }
    }

    /// Its largest size.
    pub fn max_scale(self) -> u8 {
        match self {
            Face::Pixel => 6,
            Face::Departure => 3,
        }
    }

    /// Whether it has emoji at `scale` (Departure Mono at 11 and 22 px; at 33 they'd crowd out other fonts).
    pub fn has_emoji(self, scale: u8) -> bool {
        self == Face::Departure && (1..=EMOJI.len() as u8).contains(&scale)
    }

    /// Its pixel size at `scale`, as the editor names it.
    pub fn px(self, scale: u8) -> u32 {
        match self {
            Face::Pixel => 8 * scale as u32,
            Face::Departure => 11 * scale as u32,
        }
    }
}

/// An emoji's glyph, as `tools/fonts.py` drew it.
struct Emoji {
    codepoint: u32,
    w: u8,
    h: u8,
    x: i8,
    y: i8,
    /// `h` rows of `ceil(w / 8)` bytes.
    rows: Vec<u8>,
}

fn emoji_at(scale: u8) -> &'static [Emoji] {
    static SETS: OnceLock<Vec<Vec<Emoji>>> = OnceLock::new();
    let sets = SETS.get_or_init(|| {
        EMOJI
            .iter()
            .map(|data| {
                let count = u16::from_le_bytes([data[0], data[1]]) as usize;
                let mut at = 2;
                let mut out = Vec::with_capacity(count);
                for _ in 0..count {
                    let codepoint = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
                    let (w, h, x, y) = (data[at + 4], data[at + 5], data[at + 6] as i8, data[at + 7] as i8);
                    let len = h as usize * (w as usize).div_ceil(8);
                    out.push(Emoji { codepoint, w, h, x, y, rows: data[at + 8..at + 8 + len].to_vec() });
                    at += 8 + len;
                }
                out
            })
            .collect()
    });
    &sets[scale as usize - 1]
}

/// The emoji's glyph index, if the fonts have it.
fn emoji_glyph(c: char) -> Option<u32> {
    emoji_at(1).iter().position(|e| e.codepoint == c as u32).map(|i| FIRST_EMOJI + i as u32)
}

/// Whether `c` is an emoji the fonts have.
pub(crate) fn is_emoji(c: char) -> bool {
    emoji_glyph(c).is_some()
}

/// Cells `c` takes: two for an emoji, else one.
pub(crate) fn width(c: char) -> usize {
    if is_emoji(c) { 2 } else { 1 }
}

/// Cells `text` takes.
#[cfg(test)]
fn cells(text: &str) -> usize {
    text.chars().map(width).sum()
}

/// What only joins or styles an emoji (variation selectors, the joiner, skin tones, a keycap): dropped.
pub(crate) fn is_modifier(c: char) -> bool {
    matches!(c, '\u{FE0E}' | '\u{FE0F}' | '\u{200D}' | '\u{20E3}' | '\u{1F3FB}'..='\u{1F3FF}')
}

/// `text` as glyph indices, varints (emoji two bytes, all else one); a character the font lacks is `?`. Text keeps
/// emoji only if the font has them (`ascii` cleans it first).
pub(crate) fn encode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for c in text.chars() {
        match emoji_glyph(c) {
            Some(g) => out.extend([(g & 0x7F) as u8 | 0x80, (g >> 7) as u8]),
            None => out.extend(v5::text_indices(&c.to_string())),
        }
    }
    out
}

/// `bytes` of glyphs cut to at most `cap` bytes, never in the middle of a varint.
pub(crate) fn cut(mut bytes: Vec<u8>, cap: usize) -> Vec<u8> {
    bytes.truncate(cap);
    // A varint's first byte has the top bit; a last byte with it lost its second.
    if bytes.last().is_some_and(|&b| b & 0x80 != 0) {
        bytes.pop();
    }
    bytes
}

/// The font blob (VM.md §4.2) for `face` at `scale`, with emoji or not.
pub(crate) fn blob(face: Face, scale: u8, emoji: bool) -> Vec<u8> {
    match face {
        Face::Pixel => pixel(scale),
        Face::Departure => departure(scale, emoji && face.has_emoji(scale)),
    }
}

/// A character's cell (`rows`, a byte each, the leftmost pixel the top bit; `ascent` of them above the baseline)
/// drawn `s` times over and cropped to its ink.
fn cell(rows: &[u8], width: i32, ascent: i32, s: i32, advance: u32) -> Glyph {
    let ink: Vec<(i32, i32)> = (0..rows.len() as i32)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| rows[y as usize] & (0x80 >> x) != 0)
        .collect();
    if ink.is_empty() {
        return Glyph { w: 0, h: 0, x: 0, y: 0, advance, bits: Vec::new() };
    }
    let (x0, x1) = (ink.iter().map(|p| p.0).min().unwrap(), ink.iter().map(|p| p.0).max().unwrap() + 1);
    let (y0, y1) = (ink.iter().map(|p| p.1).min().unwrap(), ink.iter().map(|p| p.1).max().unwrap() + 1);
    let (w, h) = ((x1 - x0) * s, (y1 - y0) * s);
    let bits = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| rows[(y0 + y / s) as usize] & (0x80 >> (x0 + x / s)) != 0)
        .collect();
    Glyph { w: w as u32, h: h as u32, x: x0 * s, y: (y0 - ascent) * s, advance, bits }
}

/// The 8 × 8 font `scale` times over, as `v5::font` (the same glyphs and spacing) but each glyph cropped to its ink.
fn pixel(scale: u8) -> Vec<u8> {
    use font8x8::{BASIC_FONTS, LATIN_FONTS, UnicodeFonts};
    let s = scale as i32;
    let chars = v5::text_chars();
    let glyphs: Vec<Glyph> = chars
        .iter()
        .map(|&c| {
            // In font8x8 bit 0 is the leftmost pixel.
            let rows = BASIC_FONTS.get(c).or_else(|| LATIN_FONTS.get(c)).unwrap_or([0; 8]).map(u8::reverse_bits);
            cell(&rows, 8, 7, s, 8 * scale as u32)
        })
        .collect();
    let codepoints: Vec<u32> = chars.iter().map(|&c| c as u32).collect();
    pack(&glyphs, &codepoints, 7 * scale, scale, 9 * scale)
}

/// A glyph: its ink box from the pen and the baseline, and its rows of bits.
struct Glyph {
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    advance: u32,
    bits: Vec<bool>,
}

/// Departure Mono `scale` times over, each glyph cropped to its ink; with emoji, empty glyphs up to 128 and the
/// emoji after.
fn departure(scale: u8, emoji: bool) -> Vec<u8> {
    let s = scale as i32;
    let mut glyphs: Vec<Glyph> = DEPARTURE
        .chunks((ASCENT + DESCENT) as usize)
        .map(|rows| cell(rows, ADVANCE, ASCENT, s, (ADVANCE * s) as u32))
        .collect();
    let mut codepoints: Vec<u32> = v5::text_chars().iter().map(|&c| c as u32).collect();
    if emoji {
        while glyphs.len() < FIRST_EMOJI as usize {
            glyphs.push(Glyph { w: 0, h: 0, x: 0, y: 0, advance: 0, bits: Vec::new() });
            codepoints.push(0);
        }
        for e in emoji_at(scale) {
            let stride = (e.w as usize).div_ceil(8);
            let bits = (0..e.h as usize)
                .flat_map(|y| (0..e.w as usize).map(move |x| (x, y)))
                .map(|(x, y)| e.rows[y * stride + x / 8] & (0x80 >> (x % 8)) != 0)
                .collect();
            let advance = (2 * ADVANCE * s) as u32;
            glyphs.push(Glyph { w: e.w as u32, h: e.h as u32, x: e.x as i32, y: e.y as i32, advance, bits });
            codepoints.push(e.codepoint);
        }
    }
    pack(&glyphs, &codepoints, (ASCENT * s) as u8, (DESCENT * s) as u8, ((ASCENT + DESCENT + 2) * s) as u8)
}

/// A font blob of `glyphs`, their bitmaps packed bit after bit.
fn pack(glyphs: &[Glyph], codepoints: &[u32], ascent: u8, descent: u8, line_spacing: u8) -> Vec<u8> {
    let n = glyphs.len();
    let start = 16 + 16 * n;
    let total_bits: usize = glyphs.iter().map(|g| g.bits.len()).sum();
    let total = start + total_bits.div_ceil(8);
    let mut blob = Vec::with_capacity(total);
    blob.extend(b"SKF");
    blob.push(1);
    blob.extend((total as u32).to_le_bytes());
    blob.extend((n as u16).to_le_bytes());
    blob.extend(0u16.to_le_bytes());
    blob.extend([ascent, descent, line_spacing, 0]);
    let mut bit = start * 8;
    for g in glyphs {
        let at = if g.bits.is_empty() { 0 } else { bit as u32 };
        blob.extend(at.to_le_bytes());
        blob.extend([g.w as u8, g.h as u8, g.advance as u8, 0]);
        blob.extend((g.x as i16).to_le_bytes());
        blob.extend((g.y as i16).to_le_bytes());
        bit += g.bits.len();
    }
    for &c in codepoints {
        blob.extend(c.to_le_bytes());
    }
    let mut bitmaps = vec![0u8; total - start];
    let mut at = 0;
    for g in glyphs {
        for &on in &g.bits {
            if on {
                bitmaps[at / 8] |= 0x80 >> (at % 8);
            }
            at += 1;
        }
    }
    blob.extend(bitmaps);
    debug_assert_eq!(blob.len(), total);
    blob
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_emoji_as_two_bytes_and_two_cells() {
        let text = "hi 👍";
        assert_eq!(cells(text), 5);
        let bytes = encode(text);
        assert_eq!(bytes.len(), 5);
        assert!(bytes[3] & 0x80 != 0 && bytes[4] & 0x80 == 0);
        // Cut between an emoji's bytes: the half goes.
        assert_eq!(cut(bytes.clone(), 4).len(), 3);
        assert_eq!(cut(bytes, 5).len(), 5);
        assert!(is_modifier('\u{FE0F}') && !is_emoji('a'));
    }

    #[test]
    fn builds_departure_mono() {
        for (scale, emoji) in [(1, false), (2, true), (3, false)] {
            let blob = blob(Face::Departure, scale, emoji);
            assert_eq!(&blob[..4], b"SKF\x01");
            let n = u16::from_le_bytes([blob[8], blob[9]]) as usize;
            assert_eq!(n, if emoji { 128 + emoji_at(scale).len() } else { v5::text_chars().len() });
            assert_eq!(u32::from_le_bytes(blob[4..8].try_into().unwrap()) as usize, blob.len());
            // Every glyph's bits inside the blob.
            for i in 0..n {
                let e = &blob[16 + 12 * i..28 + 12 * i];
                let bits = u32::from_le_bytes(e[..4].try_into().unwrap()) as usize;
                assert!(bits + e[4] as usize * e[5] as usize <= 8 * blob.len());
            }
        }
        // 'A' at 11 px: ink 5 or 7 wide, 8 high above the baseline.
        let blob = blob(Face::Departure, 1, false);
        let a = 10;
        let e = &blob[16 + 12 * a..28 + 12 * a];
        assert_eq!((e[5], i16::from_le_bytes([e[10], e[11]])), (8, -8));
        assert_eq!(Face::Departure.px(2), 22);
    }

    /// Glyph `i`'s ink in `blob` as the set of pixels from the pen at the baseline, and its advance.
    fn ink(blob: &[u8], i: usize) -> (std::collections::BTreeSet<(i32, i32)>, u8) {
        let e = &blob[16 + 12 * i..28 + 12 * i];
        let bits = u32::from_le_bytes(e[..4].try_into().unwrap()) as usize;
        let (w, h) = (e[4] as usize, e[5] as usize);
        let (x, y) = (i16::from_le_bytes([e[8], e[9]]) as i32, i16::from_le_bytes([e[10], e[11]]) as i32);
        let on = (0..w * h).filter(|k| blob[(bits + k) / 8] & (0x80 >> ((bits + k) % 8)) != 0);
        (on.map(|k| (x + (k % w) as i32, y + (k / w) as i32)).collect(), e[6])
    }

    #[test]
    fn the_cropped_8x8_font_draws_as_the_whole_one() {
        for scale in [1, 3] {
            let (cropped, whole) = (blob(Face::Pixel, scale, false), v5::font(scale));
            assert!(cropped.len() < whole.len());
            for i in 0..v5::text_chars().len() {
                assert_eq!(ink(&cropped, i), ink(&whole, i), "glyph {i} at {scale}");
            }
        }
    }
}
