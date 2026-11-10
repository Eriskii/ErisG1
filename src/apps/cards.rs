//! `cards`: flashcards. `erisg1 v0 app cards FILE` reads one card a line, "front | back" or front and back
//! separated by a tab (blank lines and lines starting with # are skipped), and shows them a card at a time, about
//! 2 m away. Tap either TouchBar to turn the card over. Double tap: you knew it, it comes back at the end of the
//! pile, and a card known twice is learned and leaves it. Triple tap: again, it comes back three cards on. Hold to
//! shuffle and start over.

use std::collections::VecDeque;

use super::kit::{Canvas, Eye, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;

pub(crate) const APP: App = App {
    name: "cards",
    about: "flashcards from a file: tap turns over, double tap knew it, triple tap again, hold starts over",
    fun: false,
    options: Options { font: 2, big: 3, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0)
        .touch(1.5, Side::Right, 1)
        .touch(3.0, Side::Left, 2)
        .touch(4.5, Side::Right, 1)
        .touch(6.0, Side::Left, 3)
        .touch(7.5, Side::Right, 1)
        .touch(9.0, Side::Right, 2)
        .touch(10.5, Side::Left, 1)
}

const DECK: &str = "France | Paris
Japan | Tokyo
Kenya | Nairobi
Canada | Ottawa
Peru | Lima
Norway | Oslo";

/// The card: its box, and how far away it shows.
const CARD: [i32; 4] = [70, 110, 500, 230];
const DEPTH: f64 = 2.0;
/// Characters a line holds: the front in the 24 px font, the back in the 16 px one.
const FRONT: usize = 19;
const BACK: usize = 29;
/// Times a card must be known to be learned, and how far "again" puts it back.
const LEARNED: u8 = 2;
const AGAIN: usize = 3;

/// Words into lines of at most `width` characters, at most `lines` of them.
fn wrap(text: &str, width: usize, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word: String = word.chars().take(width).collect();
        match out.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(&word);
            }
            _ => out.push(word),
        }
    }
    out.truncate(lines);
    out
}

struct Deck {
    cards: Vec<(String, String)>,
    /// The pile, top first, as indices into `cards`.
    pile: VecDeque<usize>,
    known: Vec<u8>,
    learned: usize,
}

impl Deck {
    fn parse(text: &str) -> Deck {
        let cards: Vec<(String, String)> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let (front, back) = l.split_once('\t').or_else(|| l.split_once('|'))?;
                Some((front.trim().to_string(), back.trim().to_string()))
            })
            .collect();
        let n = cards.len();
        Deck { cards, pile: (0..n).collect(), known: vec![0; n], learned: 0 }
    }

    /// Puts the pile in a new order (a small generator seeded by `seed`), everything unlearned.
    fn shuffle(&mut self, seed: u64) {
        let mut state = seed | 1;
        let mut order: Vec<usize> = (0..self.cards.len()).collect();
        for i in (1..order.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            order.swap(i, (state % (i as u64 + 1)) as usize);
        }
        self.pile = order.into();
        self.known = vec![0; self.cards.len()];
        self.learned = 0;
    }

    fn top(&self) -> Option<&(String, String)> {
        self.pile.front().map(|&i| &self.cards[i])
    }

    /// Knew it: to the bottom of the pile, or out of it once known LEARNED times.
    fn knew(&mut self) {
        if let Some(i) = self.pile.pop_front() {
            self.known[i] += 1;
            if self.known[i] >= LEARNED {
                self.learned += 1;
            } else {
                self.pile.push_back(i);
            }
        }
    }

    /// Again: back in AGAIN cards on, and no longer known.
    fn again(&mut self) {
        if let Some(i) = self.pile.pop_front() {
            self.known[i] = 0;
            self.pile.insert(AGAIN.min(self.pile.len()), i);
        }
    }
}

/// The card as it is: its front, and its back when turned over; or the end of the pile.
fn card(eye: &Eye, c: &mut Canvas, deck: &Deck, turned: bool) {
    let [x, y, w, h] = CARD;
    let shift = eye.depth(DEPTH);
    c.clear(x - 40, y - 46, w + 80, h + 96);
    let progress = format!("{} left   {} learned", deck.pile.len(), deck.learned);
    c.text_centred(320 + shift, y - 36, &progress, 8);
    let x = x + shift;
    c.rect(x, y, w, h, 12);
    match deck.top() {
        Some((front, back)) => {
            let front = wrap(front, FRONT, 2);
            if turned {
                // The front small at the top, the back under it: big if it's short.
                for (i, line) in front.iter().enumerate() {
                    c.big_centred(x + w / 2, y + 22 + 30 * i as i32, line, 9);
                }
                c.line(x + 20, y + 92, x + w - 21, y + 92, 6);
                // Wrapped to three lines, two or fewer means it all fits two (no word cut short).
                let short = wrap(back, FRONT, 3);
                if short.len() <= 2 && back.split_whitespace().all(|w| w.chars().count() <= FRONT) {
                    for (i, line) in short.iter().enumerate() {
                        c.big_centred(x + w / 2, y + 120 + 34 * i as i32, line, 15);
                    }
                } else {
                    for (i, line) in wrap(back, BACK, 4).iter().enumerate() {
                        c.text_centred(x + w / 2, y + 108 + 26 * i as i32, line, 15);
                    }
                }
            } else {
                // The front alone, in the middle.
                let top = y + h / 2 - 22 - 15 * (front.len() as i32 - 1);
                for (i, line) in front.iter().enumerate() {
                    c.big_centred(x + w / 2, top + 30 * i as i32, line, 15);
                }
                c.text_centred(x + w / 2, y + h - 34, "tap to turn over", 6);
            }
        }
        None => {
            c.big_centred(x + w / 2, y + 70, "All learned!", 15);
            c.text_centred(x + w / 2, y + 120, "hold to start over", 8);
        }
    }
    c.text_centred(320 + shift, y + h + 14, "tap: turn   2: knew it   3: again", 6);
    c.show(CARD[0] - 40, CARD[1] - 46, CARD[2] + 80, CARD[3] + 96);
}

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let text = match (args.first(), g.preview()) {
            (Some(path), _) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
            (None, true) => DECK.to_string(),
            (None, false) => {
                return Err(Error::from("usage: erisg1 v0 app cards FILE (one card a line: front | back)"));
            }
        };
        let mut deck = Deck::parse(&text);
        if deck.cards.is_empty() {
            return Err("no cards in the file: one a line, front | back".into());
        }
        let seed =
            || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        if !g.preview() {
            deck.shuffle(seed());
        }
        let mut turned = false;
        g.frame(|eye, c| card(eye, c, &deck, turned)).await?;
        while g.running() {
            let mut changed = false;
            for (_, gesture) in g.input().await? {
                changed = true;
                match gesture {
                    Gesture::Tap => turned = !turned,
                    Gesture::DoubleTap => {
                        deck.knew();
                        turned = false;
                    }
                    Gesture::TripleTap => {
                        deck.again();
                        turned = false;
                    }
                    Gesture::Hold => {
                        deck.shuffle(if g.preview() { 7 } else { seed() });
                        turned = false;
                    }
                    _ => changed = false,
                }
            }
            if changed {
                g.frame(|eye, c| card(eye, c, &deck, turned)).await?;
            } else {
                g.wait(0.05).await?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_separators_and_skips_the_rest() {
        let deck = Deck::parse("# capitals\nFrance | Paris\n\nJapan\tTokyo\nno separator");
        assert_eq!(deck.cards, [("France".into(), "Paris".into()), ("Japan".into(), "Tokyo".into())]);
    }

    #[test]
    fn known_cards_leave_and_again_comes_back_soon() {
        let mut deck = Deck::parse(DECK);
        // France known once: to the bottom; known again: learned.
        deck.knew();
        assert_eq!(deck.pile.back(), Some(&0));
        assert_eq!(deck.pile.len(), 6);
        // Japan again: three cards on.
        deck.again();
        assert_eq!(deck.pile[AGAIN], 1);
        while deck.pile.front() != Some(&0) {
            deck.knew();
        }
        deck.knew();
        assert!(!deck.pile.contains(&0));
        assert!(deck.learned >= 1);
    }

    #[test]
    fn shuffles_every_card_once() {
        let mut deck = Deck::parse(DECK);
        deck.shuffle(12345);
        let mut seen: Vec<usize> = deck.pile.iter().copied().collect();
        seen.sort();
        assert_eq!(seen, (0..6).collect::<Vec<_>>());
    }
}
