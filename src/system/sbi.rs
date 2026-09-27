//! Built-in SBI (D15, CLAUDE.md §20.2, P9.3): S-mode `ecall`s are serviced here instead of by
//! M-mode firmware. Calling convention: a7 = extension id, a6 = function id, a0–a5 = arguments;
//! returns a0 = error, a1 = value (legacy extensions return a0 only).

use std::collections::VecDeque;

use crate::cpu::state::CpuState;
use crate::interp::Engine;
use crate::mem::direct::DirectMem;
use crate::mem::{GuestVirt, prot, tlb};

use super::uart16550::Sink;

pub const EXT_BASE: u64 = 0x10;
pub const EXT_TIME: u64 = 0x5449_4D45;
pub const EXT_IPI: u64 = 0x73_5049;
pub const EXT_RFENCE: u64 = 0x5246_4E43;
pub const EXT_HSM: u64 = 0x48_534D;
pub const EXT_SRST: u64 = 0x5352_5354;
pub const EXT_DBCN: u64 = 0x4442_434E;

const SUCCESS: i64 = 0;
const ERR_FAILED: i64 = -1;
const ERR_NOT_SUPPORTED: i64 = -2;
const ERR_INVALID_PARAM: i64 = -3;
const ERR_ALREADY_AVAILABLE: i64 = -6;

const MIP_SSIP: u64 = 1 << 1;
const MIP_STIP: u64 = 1 << 5;

/// What an SBI call asks the machine to do besides returning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbiAction {
    None,
    /// Shut down: `true` = the guest reported a failure.
    Shutdown(bool),
    Reset,
}

/// Firmware state the SBI needs.
pub struct SbiState {
    /// S-mode timer deadline (`time` units); `u64::MAX` = disarmed.
    pub stimecmp: u64,
    pub console: Sink,
    /// Calls served, by extension (statistics).
    pub calls: u64,
}

fn write_console(sink: &Sink, bytes: &[u8]) {
    use std::io::Write;
    match sink {
        Sink::Stdout => {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(bytes);
            let _ = o.flush();
        }
        Sink::Buffer(b) => b.lock().unwrap().extend_from_slice(bytes),
    }
}

/// Serve the SBI call in `cpu`'s registers. `rx` is the console input queue.
pub fn call(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    engine: &mut dyn Engine,
    st: &mut SbiState,
    rx: &mut VecDeque<u8>,
) -> SbiAction {
    st.calls += 1;
    let (eid, fid) = (cpu.x[17], cpu.x[16]);
    let a = [
        cpu.x[10], cpu.x[11], cpu.x[12], cpu.x[13], cpu.x[14], cpu.x[15],
    ];
    let mut action = SbiAction::None;
    let ret = |cpu: &mut CpuState, err: i64, val: u64| {
        cpu.x[10] = err as u64;
        cpu.x[11] = val;
    };
    let set_timer = |cpu: &mut CpuState, st: &mut SbiState, t: u64| {
        st.stimecmp = t;
        cpu.csr.mip &= !MIP_STIP;
    };
    match eid {
        // Legacy extensions (v0.1): a0 only.
        0x00 => {
            set_timer(cpu, st, a[0]);
            cpu.x[10] = 0;
        }
        0x01 => {
            write_console(&st.console, &[a[0] as u8]);
            cpu.x[10] = 0;
        }
        0x02 => cpu.x[10] = rx.pop_front().map_or(u64::MAX, |b| b as u64),
        0x03 => {
            cpu.csr.mip &= !MIP_SSIP;
            cpu.x[10] = 0;
        }
        0x04 => {
            cpu.csr.mip |= MIP_SSIP; // single hart: any IPI targets us
            cpu.x[10] = 0;
        }
        0x05 => {
            engine.fence_i();
            cpu.x[10] = 0;
        }
        0x06 | 0x07 => {
            tlb::flush_all(cpu);
            cpu.x[10] = 0;
        }
        0x08 => action = SbiAction::Shutdown(false),
        EXT_BASE => match fid {
            0 => ret(cpu, SUCCESS, 2 << 24), // SBI spec v2.0
            1 => ret(cpu, SUCCESS, 0xb5),    // implementation id (unregistered)
            2 => ret(cpu, SUCCESS, 1),
            3 => {
                let known = [
                    EXT_BASE, EXT_TIME, EXT_IPI, EXT_RFENCE, EXT_HSM, EXT_SRST, EXT_DBCN,
                ];
                let legacy = a[0] <= 0x08;
                ret(cpu, SUCCESS, (known.contains(&a[0]) || legacy) as u64)
            }
            4..=6 => ret(cpu, SUCCESS, 0), // mvendorid, marchid, mimpid
            _ => ret(cpu, ERR_NOT_SUPPORTED, 0),
        },
        EXT_TIME if fid == 0 => {
            set_timer(cpu, st, a[0]);
            ret(cpu, SUCCESS, 0)
        }
        EXT_IPI if fid == 0 => {
            // hart_mask a0 relative to hart_mask_base a1 (u64::MAX = all harts).
            if a[1] == u64::MAX || (a[1] == 0 && a[0] & 1 == 1) {
                cpu.csr.mip |= MIP_SSIP;
            }
            ret(cpu, SUCCESS, 0)
        }
        EXT_RFENCE => match fid {
            0 => {
                engine.fence_i();
                ret(cpu, SUCCESS, 0)
            }
            1..=6 => {
                tlb::flush_all(cpu);
                ret(cpu, SUCCESS, 0)
            }
            _ => ret(cpu, ERR_NOT_SUPPORTED, 0),
        },
        EXT_HSM => match fid {
            0 => ret(cpu, ERR_ALREADY_AVAILABLE, 0), // hart_start: only hart 0, running
            1 => ret(cpu, ERR_FAILED, 0),            // hart_stop of the only hart
            2 if a[0] == 0 => ret(cpu, SUCCESS, 0),  // hart_get_status: STARTED
            2 => ret(cpu, ERR_INVALID_PARAM, 0),
            _ => ret(cpu, ERR_NOT_SUPPORTED, 0),
        },
        EXT_SRST if fid == 0 => match a[0] {
            0 => action = SbiAction::Shutdown(a[1] == 1),
            1 | 2 => action = SbiAction::Reset,
            _ => ret(cpu, ERR_INVALID_PARAM, 0),
        },
        EXT_DBCN => match fid {
            0 | 1 => {
                let (n, addr) = (a[0], a[1] | a[2] << 32);
                if fid == 0 {
                    match mem.slice(GuestVirt(addr), n, prot::R) {
                        Ok(bytes) => {
                            let bytes = bytes.to_vec();
                            write_console(&st.console, &bytes);
                            ret(cpu, SUCCESS, n)
                        }
                        Err(_) => ret(cpu, ERR_INVALID_PARAM, 0),
                    }
                } else {
                    let k = (n as usize).min(rx.len());
                    let data: Vec<u8> = rx.drain(..k).collect();
                    match mem.slice_mut(GuestVirt(addr), k as u64, prot::W) {
                        Ok(dst) => {
                            dst.copy_from_slice(&data);
                            ret(cpu, SUCCESS, k as u64)
                        }
                        Err(_) => ret(cpu, ERR_INVALID_PARAM, 0),
                    }
                }
            }
            2 => {
                write_console(&st.console, &[a[0] as u8]);
                ret(cpu, SUCCESS, 0)
            }
            _ => ret(cpu, ERR_NOT_SUPPORTED, 0),
        },
        _ => ret(cpu, ERR_NOT_SUPPORTED, 0),
    }
    action
}
