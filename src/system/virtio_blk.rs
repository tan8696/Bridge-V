//! virtio-blk over virtio-mmio version 2 (Phase 10, D58; virtio spec v1.2 §4.2 MMIO transport,
//! §2.7 split virtqueues, §5.2 block device). One device at 0x1000_1000, PLIC source 1, backed
//! by a host file.
//!
//! The MMIO registers (`Mmio`) only record the driver's requests. The queue itself is
//! processed by the machine loop between slices (`VirtioBlk::process`), through `DirectMem`.
//! So DMA into guest RAM goes through the same write paths as CPU stores, and code pages it
//! overwrites are invalidated (D49).

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};

use crate::mem::GuestVirt;
use crate::mem::direct::DirectMem;
use crate::mem::phys::Mmio;

pub const VIRTIO_BASE: u64 = 0x1000_1000;
pub const VIRTIO_IRQ: usize = 1;

const MAGIC: u32 = 0x7472_6976; // "virt"
const DEVICE_BLOCK: u32 = 2;
const VENDOR: u32 = 0x5642_5642; // "BVBV"
const QUEUE_MAX: u32 = 128;
/// VIRTIO_F_VERSION_1 (feature bit 32).
const FEATURES: u64 = 1 << 32;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const T_GET_ID: u32 = 8;
const S_OK: u8 = 0;
const S_IOERR: u8 = 1;
const S_UNSUPP: u8 = 2;

/// Device state shared by the MMIO register block and the machine loop.
#[derive(Default)]
pub struct BlkState {
    status: u32,
    dev_features_sel: u32,
    drv_features_sel: u32,
    drv_features: u64,
    queue_num: u32,
    queue_ready: bool,
    desc: u64,
    avail: u64,
    used: u64,
    /// Next avail-ring index to consume.
    last_avail: u16,
    /// The driver wrote QueueNotify since the last `process`.
    notified: bool,
    interrupt_status: u32,
    /// Statistics.
    pub requests: u64,
    pub sectors_read: u64,
    pub sectors_written: u64,
}

impl BlkState {
    pub fn irq(&self) -> bool {
        self.interrupt_status != 0
    }

    pub fn pending(&self) -> bool {
        self.notified && self.queue_ready
    }

    fn reset(&mut self) {
        let (r, sr, sw) = (self.requests, self.sectors_read, self.sectors_written);
        *self = BlkState {
            requests: r,
            sectors_read: sr,
            sectors_written: sw,
            ..Default::default()
        };
    }
}

pub struct VirtioBlk {
    pub state: Arc<Mutex<BlkState>>,
    file: Arc<File>,
    /// Capacity in 512-byte sectors.
    sectors: u64,
}

fn set_half(v: &mut u64, high: bool, x: u64) {
    *v = if high {
        (*v & 0xffff_ffff) | x << 32
    } else {
        (*v & !0xffff_ffff) | (x & 0xffff_ffff)
    };
}

impl VirtioBlk {
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let sectors = file.metadata()?.len() / 512;
        Ok(VirtioBlk {
            state: Arc::new(Mutex::new(BlkState::default())),
            file: Arc::new(file),
            sectors,
        })
    }

    /// The MMIO register block (a second handle on the same device).
    pub fn regs(&self) -> VirtioBlkRegs {
        VirtioBlkRegs {
            state: self.state.clone(),
            sectors: self.sectors,
        }
    }

    /// Serve every request the driver made available, then raise the interrupt.
    pub fn process(&self, mem: &mut DirectMem) {
        let mut s = self.state.lock().unwrap();
        if !s.pending() {
            return;
        }
        s.notified = false;
        let num = s.queue_num.max(1) as u64;
        let ld = |mem: &DirectMem, a: u64, n: u64| mem.load(a, n).unwrap_or(0);
        let avail_idx = ld(mem, s.avail + 2, 2) as u16;
        let mut served = false;
        while s.last_avail != avail_idx {
            let head = ld(mem, s.avail + 4 + 2 * (s.last_avail as u64 % num), 2) as u16;
            let written = self.request(mem, &mut s, head, num);
            let used_idx = ld(mem, s.used + 2, 2) as u16;
            let slot = s.used + 4 + 8 * (used_idx as u64 % num);
            let _ = mem.store(slot, 4, head as u64);
            let _ = mem.store(slot + 4, 4, written as u64);
            // The ring entry must be visible before the index (single hart: program order).
            let _ = mem.store(s.used + 2, 2, used_idx.wrapping_add(1) as u64);
            s.last_avail = s.last_avail.wrapping_add(1);
            served = true;
        }
        if served {
            s.interrupt_status |= 1; // used buffer notification
        }
    }

    /// One descriptor chain: header, data buffers, status byte. Returns the bytes written into
    /// guest memory.
    fn request(&self, mem: &mut DirectMem, s: &mut BlkState, head: u16, num: u64) -> u32 {
        let ld = |mem: &DirectMem, a: u64, n: u64| mem.load(a, n).unwrap_or(0);
        let mut chain = Vec::new();
        let mut i = head as u64;
        for _ in 0..num {
            let d = s.desc + 16 * (i % num);
            let (addr, len) = (ld(mem, d, 8), ld(mem, d + 8, 4));
            let flags = ld(mem, d + 12, 2) as u16;
            chain.push((addr, len, flags));
            if flags & DESC_NEXT == 0 {
                break;
            }
            i = ld(mem, d + 14, 2);
        }
        s.requests += 1;
        let (Some(&(haddr, _, _)), Some(&(saddr, _, _))) = (chain.first(), chain.last()) else {
            return 0;
        };
        if chain.len() < 2 {
            return 0;
        }
        let (typ, sector) = (ld(mem, haddr, 4) as u32, ld(mem, haddr + 8, 8));
        let data = &chain[1..chain.len() - 1];
        let mut written = 0u32;
        let mut off = sector * 512;
        let status = match typ {
            T_IN | T_OUT => {
                let mut st = S_OK;
                for &(addr, len, flags) in data {
                    let ok = if typ == T_IN && flags & DESC_WRITE != 0 {
                        let mut buf = vec![0u8; len as usize];
                        let r = self.file.read_exact_at(&mut buf, off).is_ok()
                            && mem.write_bytes(GuestVirt(addr), &buf).is_ok();
                        written += len as u32;
                        s.sectors_read += len / 512;
                        r
                    } else if typ == T_OUT {
                        let r = mem
                            .slice(GuestVirt(addr), len, 0)
                            .map(|b| b.to_vec())
                            .ok()
                            .is_some_and(|b| self.file.write_all_at(&b, off).is_ok());
                        s.sectors_written += len / 512;
                        r
                    } else {
                        false
                    };
                    if !ok || off / 512 + len / 512 > self.sectors {
                        st = S_IOERR;
                    }
                    off += len;
                }
                st
            }
            T_FLUSH => {
                if self.file.sync_data().is_ok() {
                    S_OK
                } else {
                    S_IOERR
                }
            }
            T_GET_ID => {
                if let Some(&(addr, len, _)) = data.first() {
                    let id = b"bridgev-virtio-blk";
                    let n = id.len().min(len as usize);
                    let _ = mem.write_bytes(GuestVirt(addr), &id[..n]);
                    written += n as u32;
                }
                S_OK
            }
            _ => S_UNSUPP,
        };
        let _ = mem.store(saddr, 1, status as u64);
        written + 1
    }
}

/// The MMIO register block of the device (virtio spec §4.2.2).
pub struct VirtioBlkRegs {
    state: Arc<Mutex<BlkState>>,
    sectors: u64,
}

impl Mmio for VirtioBlkRegs {
    fn name(&self) -> &str {
        "virtio-blk"
    }

    fn read(&mut self, off: u64, size: u64) -> u64 {
        let s = self.state.lock().unwrap();
        let v: u64 = match off & !3 {
            0x000 => MAGIC as u64,
            0x004 => 2, // version 2 (non-legacy)
            0x008 => DEVICE_BLOCK as u64,
            0x00c => VENDOR as u64,
            0x010 => (FEATURES >> (32 * (s.dev_features_sel & 1))) & 0xffff_ffff,
            0x034 => QUEUE_MAX as u64,
            0x044 => s.queue_ready as u64,
            0x060 => s.interrupt_status as u64,
            0x070 => s.status as u64,
            0x0fc => 0, // config generation: the config never changes
            // Config space: virtio_blk_config.capacity (u64), then zeros.
            0x100 => self.sectors & 0xffff_ffff,
            0x104 => self.sectors >> 32,
            _ => 0,
        };
        let v = v >> (8 * (off & 3));
        if size >= 4 {
            v
        } else {
            v & ((1 << (8 * size)) - 1)
        }
    }

    fn write(&mut self, off: u64, _size: u64, val: u64) {
        let mut s = self.state.lock().unwrap();
        let v = val & 0xffff_ffff;
        match off {
            0x014 => s.dev_features_sel = v as u32,
            0x020 => {
                let high = s.drv_features_sel & 1 != 0;
                set_half(&mut s.drv_features, high, v);
            }
            0x024 => s.drv_features_sel = v as u32,
            0x030 => {} // QueueSel: only queue 0 exists
            0x038 => s.queue_num = (v as u32).min(QUEUE_MAX),
            0x044 => s.queue_ready = v & 1 != 0,
            0x050 => s.notified = true,
            0x064 => s.interrupt_status &= !(v as u32),
            0x070 => {
                if v == 0 {
                    s.reset();
                } else {
                    s.status = v as u32;
                }
            }
            0x080 => set_half(&mut s.desc, false, v),
            0x084 => set_half(&mut s.desc, true, v),
            0x090 => set_half(&mut s.avail, false, v),
            0x094 => set_half(&mut s.avail, true, v),
            0x0a0 => set_half(&mut s.used, false, v),
            0x0a4 => set_half(&mut s.used, true, v),
            _ => {}
        }
    }
}
