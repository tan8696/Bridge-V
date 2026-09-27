//! CLINT: machine timer and software interrupts (CLAUDE.md §20.1, P9.1). msip @ 0x0,
//! mtimecmp @ 0x4000, mtime @ 0xBFF8 (single hart). The machine loop turns `msip` and
//! `mtime >= mtimecmp` into mip.MSIP/MTIP between slices; `mtime` is the `time` CSR's clock
//! (host 10 MHz, or icount / 10 with `--deterministic`), read from a shared snapshot.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::mem::phys::Mmio;

#[derive(Debug)]
pub struct ClintState {
    pub msip: bool,
    pub mtimecmp: u64,
}

pub struct Clint {
    pub state: Arc<Mutex<ClintState>>,
    /// Current `mtime`, updated by the machine loop.
    pub mtime: Arc<AtomicU64>,
}

impl Clint {
    pub fn new(mtime: Arc<AtomicU64>) -> Self {
        Clint {
            state: Arc::new(Mutex::new(ClintState {
                msip: false,
                mtimecmp: u64::MAX,
            })),
            mtime,
        }
    }
}

fn merge(old: u64, off: u64, size: u64, val: u64) -> u64 {
    let sh = 8 * (off & 7);
    let mask = if size >= 8 {
        u64::MAX
    } else {
        ((1u64 << (8 * size)) - 1) << sh
    };
    (old & !mask) | ((val << sh) & mask)
}

impl Mmio for Clint {
    fn name(&self) -> &str {
        "clint"
    }

    fn read(&mut self, off: u64, size: u64) -> u64 {
        let s = self.state.lock().unwrap();
        let word = match off & !7 {
            0x0 => s.msip as u64,
            0x4000 => s.mtimecmp,
            0xbff8 => self.mtime.load(Ordering::Relaxed),
            _ => 0,
        };
        let v = word >> (8 * (off & 7));
        if size >= 8 {
            v
        } else {
            v & ((1 << (8 * size)) - 1)
        }
    }

    fn write(&mut self, off: u64, size: u64, val: u64) {
        let mut s = self.state.lock().unwrap();
        match off & !7 {
            0x0 if off == 0 => s.msip = val & 1 != 0,
            0x4000 => s.mtimecmp = merge(s.mtimecmp, off, size, val),
            _ => {} // mtime is read-only here (writes ignored)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_partial_writes() {
        let mtime = Arc::new(AtomicU64::new(0x1122_3344_5566_7788));
        let mut c = Clint::new(mtime);
        assert_eq!(c.read(0x4000, 8), u64::MAX, "mtimecmp resets disarmed");
        // RV32-style split write of mtimecmp: low word, then high word.
        c.write(0x4000, 4, 0xdead_beef);
        c.write(0x4004, 4, 0x0000_0001);
        assert_eq!(c.read(0x4000, 8), 0x1_dead_beef);
        assert_eq!(c.read(0x4004, 4), 1);
        assert_eq!(c.read(0xbff8, 8), 0x1122_3344_5566_7788);
        assert_eq!(c.read(0xbffc, 4), 0x1122_3344);
        c.write(0xbff8, 8, 0); // mtime is not writable through the CLINT
        assert_eq!(c.read(0xbff8, 8), 0x1122_3344_5566_7788);
        c.write(0, 4, 1);
        assert!(c.state.lock().unwrap().msip);
        assert_eq!(c.read(0, 4), 1);
        c.write(0, 4, 2); // only bit 0 is implemented
        assert!(!c.state.lock().unwrap().msip);
    }
}
