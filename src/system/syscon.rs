//! SiFive test finisher / syscon (`sifive,test0`, CLAUDE.md §20.1): a 32-bit write of 0x5555
//! powers off, `code << 16 | 0x3333` fails with `code`, 0x7777 resets. The machine loop polls
//! the request.

use std::sync::{Arc, Mutex};

use crate::mem::phys::Mmio;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    Pass,
    Fail(u32),
    Reset,
}

pub struct Syscon {
    pub request: Arc<Mutex<Option<Finish>>>,
}

impl Syscon {
    pub fn new() -> Self {
        Syscon {
            request: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for Syscon {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmio for Syscon {
    fn name(&self) -> &str {
        "syscon"
    }

    fn read(&mut self, _off: u64, _size: u64) -> u64 {
        0
    }

    fn write(&mut self, off: u64, _size: u64, val: u64) {
        if off != 0 {
            return;
        }
        let v = val as u32;
        let req = match v & 0xffff {
            0x5555 => Some(Finish::Pass),
            0x3333 => Some(Finish::Fail(v >> 16)),
            0x7777 => Some(Finish::Reset),
            _ => None,
        };
        if req.is_some() {
            *self.request.lock().unwrap() = req;
        }
    }
}
