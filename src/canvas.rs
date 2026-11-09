//! A screen-sized 1-bit picture to draw into before sending it as a bitmap.

use font8x8::{BASIC_FONTS, UnicodeFonts};

use crate::proto;

pub struct Canvas {
    lit: Vec<bool>,
}

impl Canvas {
    pub fn new() -> Canvas {
        Canvas { lit: vec![false; proto::WIDTH * proto::HEIGHT] }
    }

    pub fn set(&mut self, x: usize, y: usize) {
        if x < proto::WIDTH && y < proto::HEIGHT {
            self.lit[y * proto::WIDTH + x] = true;
        }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        self.lit[y * proto::WIDTH + x]
    }

    /// How wide `text` is drawn at `scale`.
    pub fn text_width(text: &str, scale: usize) -> usize {
        text.chars().count() * 8 * scale
    }

    /// Draws `text` in an 8×8 font blown up `scale` times, with its top left corner at `x`, `y`.
    pub fn text(&mut self, x: usize, y: usize, scale: usize, text: &str) {
        for (i, c) in text.chars().enumerate() {
            let Some(glyph) = BASIC_FONTS.get(c) else { continue };
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    // Bit 0 is the leftmost pixel.
                    if bits & (1 << col) != 0 {
                        self.fill(x + (i * 8 + col) * scale, y + row * scale, scale, scale);
                    }
                }
            }
        }
    }

    pub fn fill(&mut self, x: usize, y: usize, width: usize, height: usize) {
        for y in y..y + height {
            for x in x..x + width {
                self.set(x, y);
            }
        }
    }

    pub fn bmp(&self) -> Vec<u8> {
        proto::bitmap(|x, y| self.get(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_lands_where_asked() {
        let mut canvas = Canvas::new();
        canvas.text(10, 20, 2, "|");
        let lit: Vec<(usize, usize)> = (0..proto::HEIGHT)
            .flat_map(|y| (0..proto::WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.get(x, y))
            .collect();
        assert!(!lit.is_empty());
        assert!(lit.iter().all(|&(x, y)| (10..26).contains(&x) && (20..36).contains(&y)));
        assert_eq!(Canvas::text_width("12:00", 3), 120);
    }

    #[test]
    fn drawing_off_the_edge_is_clipped() {
        let mut canvas = Canvas::new();
        canvas.text(proto::WIDTH - 4, proto::HEIGHT - 4, 3, "8");
        canvas.fill(proto::WIDTH + 10, 0, 5, 5);
    }
}
