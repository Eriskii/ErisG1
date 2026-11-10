//! A builder for code that runs on a g1v0 arm (VM.md §2, §5): Rust calls, one per instruction, that come out as the
//! arm's bytecode. Operands are arena words: r0–r14 are words 0–14, and every constant lives in a pool the builder
//! fills as it goes, since the instructions take no immediate values.
// The whole opcode table (VM.md §2.2) is here, whether a program uses each one yet or not.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::v5;

pub const COPY: u8 = 0x03;
pub const MOV: u8 = 0x10;
pub const ADD: u8 = 0x11;
pub const SUB: u8 = 0x12;
pub const MUL: u8 = 0x13;
pub const DIVU: u8 = 0x14;
pub const MODU: u8 = 0x15;
pub const DIVS: u8 = 0x16;
pub const MODS: u8 = 0x17;
pub const AND: u8 = 0x18;
pub const OR: u8 = 0x19;
pub const XOR: u8 = 0x1A;
pub const SHL: u8 = 0x1B;
pub const SHR: u8 = 0x1C;
pub const SAR: u8 = 0x1D;
pub const LD32: u8 = 0x20;
pub const LD16: u8 = 0x21;
pub const LD8: u8 = 0x22;
pub const ST32: u8 = 0x24;
pub const ST16: u8 = 0x25;
pub const ST8: u8 = 0x26;
pub const JMP: u8 = 0x30;
pub const CALL: u8 = 0x31;
pub const RET: u8 = 0x32;
pub const END: u8 = 0x33;
pub const HALT: u8 = 0x34;
pub const BEQ: u8 = 0x35;
pub const BNE: u8 = 0x36;
pub const BLTU: u8 = 0x37;
pub const BGEU: u8 = 0x38;
pub const BLTS: u8 = 0x39;
pub const BGES: u8 = 0x3A;
pub const SLEEP_UNTIL: u8 = 0x40;
pub const WAIT_EVENT: u8 = 0x41;
pub const RAISE: u8 = 0x42;
pub const NOTIFY: u8 = 0x43;
pub const SYS_READ: u8 = 0x44;
pub const SYS_WRITE: u8 = 0x45;
pub const FILL: u8 = 0x50;
pub const BLIT: u8 = 0x51;
pub const GLYPHS: u8 = 0x52;
pub const DRAW: u8 = 0x53;
pub const LINE: u8 = 0x54;

pub struct Asm {
    /// Where the code goes in the arena.
    base: u32,
    code: Vec<u8>,
    labels: HashMap<&'static str, u32>,
    /// Places in `code` that hold a label's address, once it's known.
    fixups: Vec<(usize, &'static str)>,
    pool_addr: u32,
    pool: Vec<u32>,
}

impl Asm {
    /// Code assembled for `base`, its constants kept from `pool_addr` on.
    pub fn new(base: u32, pool_addr: u32) -> Asm {
        assert!(pool_addr.is_multiple_of(4));
        Asm { base, code: Vec::new(), labels: HashMap::new(), fixups: Vec::new(), pool_addr, pool: Vec::new() }
    }

    /// The pool word that holds `value`.
    pub fn k(&mut self, value: u32) -> u16 {
        let i = match self.pool.iter().position(|&v| v == value) {
            Some(i) => i,
            None => {
                self.pool.push(value);
                self.pool.len() - 1
            }
        };
        (self.pool_addr / 4) as u16 + i as u16
    }

    /// An instruction on arena words.
    pub fn op(&mut self, op: u8, words: &[u16]) {
        self.code.extend(v5::encode(op, words, &[]));
    }

    pub fn label(&mut self, name: &'static str) {
        let at = self.base + self.code.len() as u32;
        assert!(self.labels.insert(name, at).is_none(), "label {name} twice");
    }

    /// `JMP` or `CALL` to a label.
    pub fn jump(&mut self, op: u8, target: &'static str) {
        self.code.push(op);
        self.fixups.push((self.code.len(), target));
        self.code.extend([0; 3]);
    }

    /// A conditional branch on `a` and `b` to a label.
    pub fn branch(&mut self, op: u8, a: u16, b: u16, target: &'static str) {
        // The opcode, one byte of operand nibbles, then the target.
        self.fixups.push((self.code.len() + 2, target));
        self.code.extend(v5::encode(op, &[a, b], &[0; 3]));
    }

    pub fn address(&self, label: &str) -> u32 {
        self.labels[label]
    }

    /// The code with every label filled in, and the pool's bytes.
    pub fn finish(mut self) -> Result<(Vec<u8>, Vec<u8>), String> {
        for &(at, label) in &self.fixups {
            let target = *self.labels.get(label).ok_or(format!("no label {label}"))?;
            self.code[at..at + 3].copy_from_slice(&target.to_le_bytes()[..3]);
        }
        let pool = self.pool.iter().flat_map(|v| v.to_le_bytes()).collect();
        Ok((self.code, pool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_and_jumps_reach_their_labels() {
        let mut a = Asm::new(0x1000, 0x800);
        a.label("top");
        let one = a.k(1);
        a.op(ADD, &[0, 0, one]);
        a.branch(BNE, 0, one, "top");
        a.jump(CALL, "end");
        a.label("end");
        a.op(RET, &[]);
        let (code, pool) = a.finish().unwrap();
        // ADD r0, r0, [0x200]: 11 00 f0 00 02
        assert_eq!(&code[..5], &[0x11, 0x00, 0xf0, 0x00, 0x02]);
        // BNE r0, [0x200], 0x1000: 36 0f 00 10 00 00 02 — the target before the escape.
        assert_eq!(&code[5..12], &[0x36, 0x0f, 0x00, 0x10, 0x00, 0x00, 0x02]);
        // CALL 0x1010
        assert_eq!(&code[12..16], &[0x31, 0x10, 0x10, 0x00]);
        assert_eq!(code[16], RET);
        assert_eq!(pool, 1u32.to_le_bytes());
    }

    #[test]
    fn shares_constants() {
        let mut a = Asm::new(0, 0x400);
        assert_eq!(a.k(7), 0x100);
        assert_eq!(a.k(9), 0x101);
        assert_eq!(a.k(7), 0x100);
    }
}
