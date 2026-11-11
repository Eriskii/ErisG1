//! With `ERISG1_SHELL_STATS` set, `erisg1 shell` says what the shell takes of the glasses: at the start, its share of
//! each area of the arena; while it runs, every 10 s, each arm's program time (`VM_STATUS` busy_ms: the time its
//! program ran in the last whole 100 ms period, which the firmware caps at 30), and the tasks, instructions, draws
//! and cap pauses since the last report. It only reads.

use std::time::{Duration, Instant};

use super::layout::{FONTS, FONTS_END};
use super::program::Shell;
use crate::link::Error;
use crate::proto::Side;
use crate::v5;
use crate::v5_link::{Arm, side_name};

const REPORT_EVERY: Duration = Duration::from_secs(10);
/// The control request for the arm's diagnostics (VM.md §6.2).
const STATUS: u8 = 0x03;

pub(crate) fn enabled() -> bool {
    std::env::var_os("ERISG1_SHELL_STATS").is_some()
}

/// What the program, its buffers and its fonts take of the arena.
pub(crate) fn print_usage(shell: &Shell) {
    let names = ["code", "constants", "variables", "buffers"];
    let usage = shell.right.program.usage();
    let parts: Vec<String> = names
        .iter()
        .zip(usage)
        .map(|(name, (used, of))| format!("{name} {:.1} of {:.1} KB", used as f64 / 1024.0, of as f64 / 1024.0))
        .collect();
    println!("stats: the program: {}", parts.join(", "));
    let geo = &shell.geo;
    let after_frame: u32 = shell
        .assets()
        .iter()
        .filter(|(at, _)| (FONTS..FONTS_END).contains(at))
        .map(|(_, bytes)| bytes.len() as u32)
        .sum();
    let total: u32 = shell.assets().iter().map(|(_, bytes)| bytes.len() as u32).sum();
    println!(
        "stats: fonts and icons {:.1} KB: {:.1} of {:.1} KB after the frame, {:.1} KB atop the code ({:.1} KB left \
         between them and the code)",
        total as f64 / 1024.0,
        after_frame as f64 / 1024.0,
        (FONTS_END - FONTS) as f64 / 1024.0,
        (total - after_frame) as f64 / 1024.0,
        (geo.low - shell.right.program.code_end()) as f64 / 1024.0,
    );
}

/// Counters from one reading: tasks started, cap pauses, instructions, draws, uptime (ms).
#[derive(Clone, Copy)]
struct Counters {
    tasks: u32,
    pauses: u16,
    instructions: u32,
    draws: u32,
    uptime: u32,
}

struct ArmLoad {
    side: Side,
    busy: Vec<u8>,
    first: Option<Counters>,
    last: Option<Counters>,
}

pub(crate) struct Load {
    arms: Vec<ArmLoad>,
    since: Instant,
}

impl Load {
    pub fn new() -> Load {
        Load { arms: Vec::new(), since: Instant::now() }
    }

    /// One reading of each arm.
    pub async fn sample(&mut self, arms: &mut [Arm]) -> Result<(), Error> {
        for arm in arms.iter_mut() {
            let vm = arm.control(v5::ctrl::VM_STATUS, &[]).await?;
            let status = arm.control(STATUS, &[]).await?;
            if vm.len() < 32 || status.len() < 44 {
                continue;
            }
            let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            let counters = Counters {
                tasks: u32_at(&vm, 20),
                pauses: u16::from_le_bytes([vm[29], vm[30]]),
                instructions: u32_at(&status, 13),
                draws: u32_at(&status, 19),
                uptime: u32_at(&status, 40),
            };
            let entry = match self.arms.iter_mut().position(|a| a.side == arm.side) {
                Some(k) => &mut self.arms[k],
                None => {
                    self.arms.push(ArmLoad { side: arm.side, busy: Vec::new(), first: None, last: None });
                    self.arms.last_mut().unwrap()
                }
            };
            entry.busy.push(vm[31]);
            entry.first.get_or_insert(counters);
            entry.last = Some(counters);
        }
        if self.since.elapsed() >= REPORT_EVERY {
            self.report();
        }
        Ok(())
    }

    fn report(&mut self) {
        for a in &mut self.arms {
            let (Some(first), Some(last)) = (a.first, a.last) else { continue };
            let seconds = (last.uptime.wrapping_sub(first.uptime) as f64 / 1000.0).max(0.001);
            let mean = a.busy.iter().map(|&b| b as f64).sum::<f64>() / a.busy.len().max(1) as f64;
            let max = a.busy.iter().copied().max().unwrap_or(0);
            let busy_windows = a.busy.iter().filter(|&&b| b > 0).count();
            println!(
                "stats: {} arm: busiest 100 ms {max} ms (cap 30), mean {mean:.2} ms ({busy_windows} of {} samples \
                 busy); {:.1} tasks/s, {:.0} instructions/s, {:.1} draws/s, {} cap pauses",
                side_name(a.side),
                a.busy.len(),
                last.tasks.wrapping_sub(first.tasks) as f64 / seconds,
                last.instructions.wrapping_sub(first.instructions) as f64 / seconds,
                last.draws.wrapping_sub(first.draws) as f64 / seconds,
                last.pauses.wrapping_sub(first.pauses),
            );
            a.busy.clear();
            a.first = a.last;
        }
        self.since = Instant::now();
    }
}
