//! Execution statistics: guest instruction count, TBs translated, chain patches, jump-cache and TLB
//! hit/miss counters, exit reasons, translate-vs-execute time, MIPS. Grows with each phase.

use std::fmt::Write;

use crate::isa::Decoded;
use crate::isa::disasm::XNAMES;

/// Guest integer-register use histogram (`--stats=regs`, P4.9): every read or write of
/// x1..x31, counted once per distinct decoded block (static) and once per retired instruction
/// (dynamic). It drives the choice of the pinned set (§8.2).
#[derive(Clone, Debug, Default)]
pub struct RegStats {
    pub static_uses: [u64; 32],
    pub dynamic_uses: [u64; 32],
}

fn count(h: &mut [u64; 32], insns: &[Decoded]) {
    for d in insns {
        let (reads, write) = d.inst.int_regs();
        for r in reads.into_iter().chain([write]).flatten() {
            if r != 0 {
                h[r as usize] += 1;
            }
        }
    }
}

impl RegStats {
    /// A block was decoded.
    pub fn block(&mut self, insns: &[Decoded]) {
        count(&mut self.static_uses, insns);
    }

    /// The first `retired` instructions of a block retired.
    pub fn retired(&mut self, insns: &[Decoded], retired: usize) {
        count(&mut self.dynamic_uses, &insns[..retired.min(insns.len())]);
    }

    /// The four registers with the most dynamic uses (the best pinned set by this measure).
    pub fn best_pins(&self) -> Vec<u8> {
        let mut r: Vec<u8> = (1..32).collect();
        r.sort_by_key(|&g| std::cmp::Reverse(self.dynamic_uses[g as usize]));
        r.truncate(4);
        r
    }

    /// Markdown table sorted by dynamic uses, plus the dynamic share a pinned set covers.
    pub fn report(&self) -> String {
        let st: u64 = self.static_uses.iter().sum();
        let dy: u64 = self.dynamic_uses.iter().sum();
        let pct = |n: u64, d: u64| {
            if d == 0 {
                0.0
            } else {
                100.0 * n as f64 / d as f64
            }
        };
        let mut regs: Vec<usize> = (1..32).collect();
        regs.sort_by_key(|&g| std::cmp::Reverse((self.dynamic_uses[g], self.static_uses[g])));
        let mut s = String::from(
            "| reg | static uses | % | dynamic uses | % |\n|---|---:|---:|---:|---:|\n",
        );
        for g in regs {
            let (a, b) = (self.static_uses[g], self.dynamic_uses[g]);
            if a == 0 && b == 0 {
                continue;
            }
            let _ = writeln!(
                s,
                "| x{g} ({}) | {a} | {:.1} | {b} | {:.1} |",
                XNAMES[g],
                pct(a, st),
                pct(b, dy)
            );
        }
        let share = |set: &[u8]| pct(set.iter().map(|&g| self.dynamic_uses[g as usize]).sum(), dy);
        let best = self.best_pins();
        let names: Vec<String> = best.iter().map(|g| format!("x{g}")).collect();
        let _ = writeln!(
            s,
            "dynamic share of x2,x1,x10,x15 (default pins): {:.1}%; best set {}: {:.1}%",
            share(&[2, 1, 10, 15]),
            names.join(","),
            share(&best)
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::decode_parts;

    fn dec(w: u32) -> Decoded {
        decode_parts::<()>(w as u16, || Ok((w >> 16) as u16)).unwrap()
    }

    #[test]
    fn counts_reads_and_writes_but_not_x0() {
        let add = dec(0x0031_00B3); // add x1, x2, x3
        let sd = dec(0x0011_3423); // sd x1, 8(x2)
        let li = dec(0x0050_0293); // addi x5, x0, 5
        let mut r = RegStats::default();
        r.block(&[add, sd, li]);
        r.retired(&[add, sd, li], 2);
        assert_eq!(r.static_uses[..6], [0, 2, 2, 1, 0, 1]);
        assert_eq!(r.dynamic_uses[..6], [0, 2, 2, 1, 0, 0]);
        assert_eq!(r.best_pins()[..2], [1, 2]);
    }
}
