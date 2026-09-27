//! PLIC (CLAUDE.md §20.1, P9.1): `riscv,ndev = 53` level-triggered sources, two contexts
//! (0 = hart 0 M-mode, 1 = hart 0 S-mode). Registers: priority @ 4·src, pending @ 0x1000,
//! enable @ 0x2000 + 0x80·ctx, threshold @ 0x200000 + 0x1000·ctx, claim/complete @ +4.
//! A source is pending while its line is high and it is not being serviced (claimed and not
//! yet completed). A context's interrupt output is high when a pending, enabled source has a
//! priority above the context's threshold.

use std::sync::{Arc, Mutex};

use crate::mem::phys::Mmio;

pub const NDEV: usize = 53;
const NCTX: usize = 2;

#[derive(Debug)]
pub struct PlicState {
    pub priority: [u32; NDEV + 1],
    level: [bool; NDEV + 1],
    in_service: [bool; NDEV + 1],
    enable: [u64; NCTX],
    threshold: [u32; NCTX],
}

impl Default for PlicState {
    fn default() -> Self {
        PlicState {
            priority: [0; NDEV + 1],
            level: [false; NDEV + 1],
            in_service: [false; NDEV + 1],
            enable: [0; NCTX],
            threshold: [0; NCTX],
        }
    }
}

impl PlicState {
    fn pending(&self, src: usize) -> bool {
        self.level[src] && !self.in_service[src]
    }

    /// Drive source `src`'s interrupt line.
    pub fn set_level(&mut self, src: usize, high: bool) {
        if (1..=NDEV).contains(&src) {
            self.level[src] = high;
        }
    }

    /// The best claimable source of context `ctx`, if any.
    fn best(&self, ctx: usize) -> Option<usize> {
        let mut best: Option<(u32, usize)> = None;
        for src in 1..=NDEV {
            let p = self.priority[src];
            if self.pending(src)
                && self.enable[ctx] >> src & 1 == 1
                && p > self.threshold[ctx]
                && best.is_none_or(|(bp, _)| p > bp)
            {
                best = Some((p, src));
            }
        }
        best.map(|(_, s)| s)
    }

    /// Interrupt outputs: (M-mode context, S-mode context).
    pub fn outputs(&self) -> (bool, bool) {
        (self.best(0).is_some(), self.best(1).is_some())
    }
}

pub struct Plic {
    pub state: Arc<Mutex<PlicState>>,
}

impl Plic {
    pub fn new() -> Self {
        Plic {
            state: Arc::new(Mutex::new(PlicState::default())),
        }
    }
}

impl Default for Plic {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmio for Plic {
    fn name(&self) -> &str {
        "plic"
    }

    fn read(&mut self, off: u64, _size: u64) -> u64 {
        let mut s = self.state.lock().unwrap();
        let off = off as usize;
        match off {
            0..0x1000 => s.priority.get(off / 4).copied().unwrap_or(0) as u64,
            0x1000..0x1080 => {
                let w = (off - 0x1000) / 4;
                let mut bits = 0u64;
                for i in 0..32 {
                    let src = w * 32 + i;
                    if (1..=NDEV).contains(&src) && s.pending(src) {
                        bits |= 1 << i;
                    }
                }
                bits
            }
            0x2000..0x2100 => {
                let (ctx, w) = ((off - 0x2000) / 0x80, (off - 0x2000) % 0x80 / 4);
                if ctx < NCTX && w < 2 {
                    (s.enable[ctx] >> (32 * w)) & 0xffff_ffff
                } else {
                    0
                }
            }
            0x20_0000.. => {
                let (ctx, reg) = ((off - 0x20_0000) / 0x1000, (off - 0x20_0000) % 0x1000);
                if ctx >= NCTX {
                    return 0;
                }
                match reg {
                    0 => s.threshold[ctx] as u64,
                    4 => match s.best(ctx) {
                        Some(src) => {
                            s.in_service[src] = true;
                            src as u64
                        }
                        None => 0,
                    },
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    fn write(&mut self, off: u64, _size: u64, val: u64) {
        let mut s = self.state.lock().unwrap();
        let off = off as usize;
        let val32 = val as u32;
        match off {
            0..0x1000 => {
                if let Some(p) = s.priority.get_mut(off / 4)
                    && off >= 4
                {
                    *p = val32 & 7;
                }
            }
            0x2000..0x2100 => {
                let (ctx, w) = ((off - 0x2000) / 0x80, (off - 0x2000) % 0x80 / 4);
                if ctx < NCTX && w < 2 {
                    let sh = 32 * w;
                    let mut e = s.enable[ctx] & !(0xffff_ffff << sh);
                    e |= (val32 as u64) << sh;
                    s.enable[ctx] = e & !1; // source 0 does not exist
                }
            }
            0x20_0000.. => {
                let (ctx, reg) = ((off - 0x20_0000) / 0x1000, (off - 0x20_0000) % 0x1000);
                if ctx >= NCTX {
                    return;
                }
                match reg {
                    0 => s.threshold[ctx] = val32 & 7,
                    4 => {
                        let src = val32 as usize;
                        if (1..=NDEV).contains(&src) {
                            s.in_service[src] = false;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_complete_priority_threshold() {
        let mut p = Plic::new();
        p.write(4 * 10, 4, 1); // priority(10) = 1
        p.write(4 * 3, 4, 2); // priority(3) = 2
        p.write(0x2080, 4, 1 << 10 | 1 << 3); // context 1 enables 3 and 10
        p.state.lock().unwrap().set_level(10, true);
        assert_eq!(p.state.lock().unwrap().outputs(), (false, true));
        assert_eq!(p.read(0x1000, 4), 1 << 10);
        p.state.lock().unwrap().set_level(3, true);
        assert_eq!(p.read(0x20_1004, 4), 3, "highest priority first");
        assert_eq!(p.read(0x20_1004, 4), 10);
        assert_eq!(p.read(0x20_1004, 4), 0, "both in service");
        assert_eq!(p.state.lock().unwrap().outputs(), (false, false));
        p.write(0x20_1004, 4, 10); // complete: the line is still high, so pending again
        assert_eq!(p.state.lock().unwrap().outputs(), (false, true));
        p.write(0x20_1000, 4, 1); // threshold 1 masks priority 1
        assert_eq!(p.state.lock().unwrap().outputs(), (false, false));
        assert_eq!(p.read(0x20_1004, 4), 0);
    }
}
