//! Built-in SBI (D15, CLAUDE.md §20.2, P9.3): S-mode `ecall`s are serviced here instead of by
//! M-mode firmware. Calling convention: a7 = extension id, a6 = function id, a0–a5 = arguments;
//! returns a0 = error, a1 = value (legacy extensions return a0 only).

use std::collections::VecDeque;

use crate::cpu::state::CpuState;
use crate::mem::direct::DirectMem;
use crate::mem::{GuestVirt, prot};

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

/// What an SBI call asks the machine to do besides returning. Hart sets are bit masks over
/// hart ids (Phase 10 SMP); the machine applies them, the calling hart included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbiAction {
    None,
    /// Shut down: `true` = the guest reported a failure.
    Shutdown(bool),
    Reset,
    /// Set SSIP on these harts.
    Ipi(u64),
    /// FENCE.I on these harts.
    RemoteFenceI(u64),
    /// SFENCE.VMA (all addresses) on these harts.
    RemoteSfence(u64),
    /// HSM hart_start: start `hart` in S-mode at `addr` with a0 = hart, a1 = `opaque`.
    HartStart {
        hart: usize,
        addr: u64,
        opaque: u64,
    },
    /// HSM hart_stop of the calling hart (the call does not return).
    HartStop,
}

/// HSM state of a hart (SBI spec §9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HartStatus {
    Started = 0,
    Stopped = 1,
    StartPending = 2,
}

/// Firmware state the SBI needs.
pub struct SbiState {
    /// S-mode timer deadline per hart (`time` units); `u64::MAX` = disarmed.
    pub stimecmp: [u64; super::plic::MAX_HARTS],
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

/// The harts named by an SBI hart mask (`mask` relative to `base`; `base` = -1: all harts).
fn hart_set(mask: u64, base: u64, harts: usize) -> u64 {
    let all = if harts >= 64 {
        u64::MAX
    } else {
        (1u64 << harts) - 1
    };
    if base == u64::MAX {
        all
    } else if base >= 64 {
        0
    } else {
        (mask << base) & all
    }
}

/// Serve the SBI call in `cpu`'s registers, made by hart `hart` of a machine whose harts are in
/// `status`. `rx` is the console input queue.
pub fn call(
    cpu: &mut CpuState,
    hart: usize,
    status: &[HartStatus],
    mem: &mut DirectMem,
    st: &mut SbiState,
    rx: &mut VecDeque<u8>,
) -> SbiAction {
    st.calls += 1;
    let (eid, fid) = (cpu.x[17], cpu.x[16]);
    let a = [
        cpu.x[10], cpu.x[11], cpu.x[12], cpu.x[13], cpu.x[14], cpu.x[15],
    ];
    let n = status.len();
    let all = hart_set(0, u64::MAX, n);
    let mut action = SbiAction::None;
    let ret = |cpu: &mut CpuState, err: i64, val: u64| {
        cpu.x[10] = err as u64;
        cpu.x[11] = val;
    };
    let set_timer = |cpu: &mut CpuState, st: &mut SbiState, t: u64| {
        st.stimecmp[hart] = t;
        cpu.csr.mip &= !MIP_STIP;
    };
    match eid {
        // Legacy extensions (v0.1): a0 only. Their hart masks are pointers into S-mode virtual
        // memory; Linux uses the v0.2 extensions, so these act on every hart.
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
            action = SbiAction::Ipi(all);
            cpu.x[10] = 0;
        }
        0x05 => {
            action = SbiAction::RemoteFenceI(all);
            cpu.x[10] = 0;
        }
        0x06 | 0x07 => {
            action = SbiAction::RemoteSfence(all);
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
            action = SbiAction::Ipi(hart_set(a[0], a[1], n));
            ret(cpu, SUCCESS, 0)
        }
        EXT_RFENCE => match fid {
            0 => {
                action = SbiAction::RemoteFenceI(hart_set(a[0], a[1], n));
                ret(cpu, SUCCESS, 0)
            }
            1..=6 => {
                action = SbiAction::RemoteSfence(hart_set(a[0], a[1], n));
                ret(cpu, SUCCESS, 0)
            }
            _ => ret(cpu, ERR_NOT_SUPPORTED, 0),
        },
        EXT_HSM => match fid {
            0 => {
                let h = a[0] as usize;
                match status.get(h) {
                    None => ret(cpu, ERR_INVALID_PARAM, 0),
                    Some(HartStatus::Stopped) => {
                        action = SbiAction::HartStart {
                            hart: h,
                            addr: a[1],
                            opaque: a[2],
                        };
                        ret(cpu, SUCCESS, 0)
                    }
                    Some(_) => ret(cpu, ERR_ALREADY_AVAILABLE, 0),
                }
            }
            1 if n > 1 => action = SbiAction::HartStop,
            1 => ret(cpu, ERR_FAILED, 0), // stopping the only hart
            2 => match status.get(a[0] as usize) {
                Some(&s) => ret(cpu, SUCCESS, s as u64),
                None => ret(cpu, ERR_INVALID_PARAM, 0),
            },
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
