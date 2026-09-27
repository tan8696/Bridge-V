//! User-mode guest address space mapped 1:1 at a host base (CLAUDE.md §14.1, D9).
//!
//! The whole 2^38-byte guest space (the SV39 user half) is reserved up front as `PROT_NONE`
//! host memory, with 4 GiB guard regions on each side. Guest mappings are made inside it with
//! `MAP_FIXED`, so guest address `g` always lives at host address `base + g`; the JIT (Phase 2)
//! will access guest memory as `[rbx + g]`.
//!
//! Guest permissions are tracked in a two-level page table (`PageProt`) and enforced by the
//! checked accessors below, so the interpreter can never fault the host: a bad guest access
//! becomes a `MemFault`. Host page protection mirrors the guest permissions (`host_prot`), so
//! the JIT's unchecked `[rbx + g]` accesses fault on the host exactly where the interpreter
//! would report a fault (D23, D27); the JIT's SIGSEGV handler turns that into a guest
//! exception. Execute-only guest pages stay host-readable (the decoder reads them).

use std::ptr;

use super::phys::{Device, Mmio};
use super::{Access, GuestVirt, MemFault, PAGE_SIZE, page_ceil, page_floor, prot};

/// Size of the guest address space: 2^38 bytes = 256 GiB.
pub const GUEST_SPACE: u64 = 1 << 38;
const GUARD: usize = 4 << 30;
const PAGES_PER_CHUNK: u64 = 4096;
const NUM_CHUNKS: usize = (GUEST_SPACE / PAGE_SIZE / PAGES_PER_CHUNK) as usize;

/// Guest permissions per 4 KiB page; chunks of 4096 pages are allocated lazily.
struct PageProt {
    chunks: Vec<Option<Box<[u8; PAGES_PER_CHUNK as usize]>>>,
}

impl PageProt {
    fn new() -> Self {
        PageProt {
            chunks: (0..NUM_CHUNKS).map(|_| None).collect(),
        }
    }

    #[inline]
    fn get(&self, page: u64) -> u8 {
        match self.chunks.get((page / PAGES_PER_CHUNK) as usize) {
            Some(Some(c)) => c[(page % PAGES_PER_CHUNK) as usize],
            _ => 0,
        }
    }

    fn set(&mut self, page: u64, p: u8) {
        let c = self.chunks[(page / PAGES_PER_CHUNK) as usize]
            .get_or_insert_with(|| Box::new([0; PAGES_PER_CHUNK as usize]));
        c[(page % PAGES_PER_CHUNK) as usize] = p;
    }
}

/// The direct-mapped guest address space.
pub struct DirectMem {
    reserve: *mut u8,
    base: *mut u8,
    prot: PageProt,
    /// Lockstep checking: every successful `store` appends `(addr, size, old value)`.
    pub write_log: Option<Vec<(u64, u64, u64)>>,
    /// System mode: memory-mapped devices outside RAM, sorted by base (P7.3).
    pub devices: Vec<Device>,
    /// Self-modifying code (Phase 8, D49): pages that held translated code and were written
    /// (or remapped) since the engines last drained this list. Their code-page mark is gone.
    pub smc_pages: Vec<u64>,
    /// Lockstep: device accesses of the reference run, replayed by the checked run (D52).
    pub mmio_log: Option<MmioLog>,
    /// Count of code-page writes ever reported (Phase 10): a guest thread whose engine did not
    /// see a write (another thread made it) flushes its translations.
    pub smc_epoch: u64,
    /// Every code-page write, in order (system-mode SMP, Phase 10): each hart's engine is
    /// given the pages written since it last ran. Trimmed by the machine.
    pub smc_log: Vec<u64>,
}

/// One device access (`val` = the value read or written).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmioOp {
    pub write: bool,
    pub pa: u64,
    pub size: u64,
    pub val: u64,
}

/// Lockstep over devices (D52): the reference run performs its MMIO accesses and records them;
/// the checked run replays them (reads return the recorded values, writes are compared and not
/// performed). Every device sees each access once, in the reference order.
#[derive(Debug, Default)]
pub struct MmioLog {
    pub replay: bool,
    pub ops: Vec<MmioOp>,
    pub pos: usize,
    /// The first access of the checked run that differs from the reference run.
    pub mismatch: Option<String>,
}

impl MmioLog {
    fn replay(&mut self, op: MmioOp) -> u64 {
        let want = self.ops.get(self.pos).copied();
        self.pos += 1;
        match want {
            Some(w)
                if (w.write, w.pa, w.size) == (op.write, op.pa, op.size)
                    && (!op.write || w.val == op.val) =>
            {
                w.val
            }
            _ => {
                self.mismatch.get_or_insert_with(|| {
                    format!(
                        "mmio access {}: interp {want:x?}, jit {op:x?}",
                        self.pos - 1
                    )
                });
                0
            }
        }
    }
}

/// Host protection for guest permissions `p`: readable if the guest may read or execute
/// (instruction fetch reads through the host mapping), writable if the guest may write.
fn host_prot(p: u8) -> libc::c_int {
    let mut h = libc::PROT_NONE;
    if p & (prot::R | prot::X | prot::W) != 0 {
        h |= libc::PROT_READ;
    }
    if p & prot::W != 0 {
        h |= libc::PROT_WRITE;
    }
    h
}

// SAFETY: DirectMem owns its mapping exclusively; it is only accessed through &self/&mut self.
unsafe impl Send for DirectMem {}

#[derive(Debug)]
pub struct MapError(pub String);

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for MapError {}

impl DirectMem {
    /// Reserve the guest address space (no memory is committed yet).
    pub fn new() -> Result<Self, MapError> {
        let len = GUEST_SPACE as usize + 2 * GUARD;
        // SAFETY: anonymous PROT_NONE reservation; no existing memory is affected.
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(MapError(format!(
                "cannot reserve {len:#x} bytes of address space: {}",
                std::io::Error::last_os_error()
            )));
        }
        let reserve = p as *mut u8;
        Ok(DirectMem {
            reserve,
            // SAFETY: GUARD < len, so the offset stays inside the reservation.
            base: unsafe { reserve.add(GUARD) },
            prot: PageProt::new(),
            write_log: None,
            devices: Vec::new(),
            smc_pages: Vec::new(),
            mmio_log: None,
            smc_epoch: 0,
            smc_log: Vec::new(),
        })
    }

    /// Host address of guest address 0 (the JIT's RBX in direct mode).
    pub fn base(&self) -> *mut u8 {
        self.base
    }

    fn check_range(addr: u64, len: u64) -> Result<(), MapError> {
        match addr.checked_add(len) {
            Some(end) if end <= GUEST_SPACE => Ok(()),
            _ => Err(MapError(format!(
                "guest range {addr:#x}+{len:#x} is outside the guest address space"
            ))),
        }
    }

    /// Map fresh zero-filled memory at `[addr, addr+len)` (rounded out to pages) with guest
    /// permissions `p`. Replaces anything mapped there before.
    pub fn map(&mut self, addr: GuestVirt, len: u64, p: u8) -> Result<(), MapError> {
        let start = page_floor(addr.0);
        let end = page_ceil(addr.0 + len);
        Self::check_range(start, end - start)?;
        if end == start {
            return Ok(());
        }
        self.forget_code(start, end);
        // SAFETY: the range lies inside our reservation (checked above) and MAP_FIXED only
        // replaces pages of that reservation.
        let r = unsafe {
            libc::mmap(
                self.base.add(start as usize) as *mut libc::c_void,
                (end - start) as usize,
                host_prot(p),
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if r == libc::MAP_FAILED {
            return Err(MapError(format!(
                "mmap of guest {start:#x}..{end:#x} failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        for page in start / PAGE_SIZE..end / PAGE_SIZE {
            self.prot.set(page, p | MAPPED);
        }
        Ok(())
    }

    /// Unmap `[addr, addr+len)`: the pages become inaccessible again.
    pub fn unmap(&mut self, addr: GuestVirt, len: u64) -> Result<(), MapError> {
        let start = page_floor(addr.0);
        let end = page_ceil(addr.0 + len);
        Self::check_range(start, end - start)?;
        if end == start {
            return Ok(());
        }
        self.forget_code(start, end);
        // SAFETY: as in `map`; re-reserves the pages as PROT_NONE, dropping their contents.
        let r = unsafe {
            libc::mmap(
                self.base.add(start as usize) as *mut libc::c_void,
                (end - start) as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if r == libc::MAP_FAILED {
            return Err(MapError(format!(
                "munmap of guest {start:#x}..{end:#x} failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        for page in start / PAGE_SIZE..end / PAGE_SIZE {
            self.prot.set(page, 0);
        }
        Ok(())
    }

    /// Change guest permissions of mapped pages. Fails if any page is unmapped.
    pub fn protect(&mut self, addr: GuestVirt, len: u64, p: u8) -> Result<(), MapError> {
        let start = page_floor(addr.0);
        let end = page_ceil(addr.0 + len);
        Self::check_range(start, end - start)?;
        if (start / PAGE_SIZE..end / PAGE_SIZE).any(|pg| !self.is_mapped(pg * PAGE_SIZE)) {
            return Err(MapError(format!(
                "mprotect of unmapped range {start:#x}..{end:#x}"
            )));
        }
        self.forget_code(start, end);
        self.set_host_prot(start, end, host_prot(p))?;
        for page in start / PAGE_SIZE..end / PAGE_SIZE {
            self.prot.set(page, p | MAPPED);
        }
        Ok(())
    }

    fn set_host_prot(&mut self, start: u64, end: u64, h: libc::c_int) -> Result<(), MapError> {
        // SAFETY: [start, end) is page-aligned, inside the reservation and mapped (callers
        // check), so mprotect only changes pages we own.
        let r = unsafe {
            libc::mprotect(
                self.base.add(start as usize) as *mut libc::c_void,
                (end - start) as usize,
                h,
            )
        };
        if r != 0 {
            return Err(MapError(format!(
                "mprotect of guest {start:#x}..{end:#x} failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    /// Guest permissions of the page containing `addr` (0 if unmapped).
    #[inline]
    pub fn prot_of(&self, addr: u64) -> u8 {
        self.prot.get(addr / PAGE_SIZE) & prot::RWX
    }

    /// Is the page containing `addr` mapped (even if it has no permissions)?
    pub fn is_mapped(&self, addr: u64) -> bool {
        addr < GUEST_SPACE && self.prot.get(addr / PAGE_SIZE) != 0
    }

    /// Check that every page of `[addr, addr+len)` has all permissions in `need`.
    #[inline]
    fn check(&self, addr: u64, len: u64, need: u8, access: Access) -> Result<(), MemFault> {
        self.check_bits(addr, len, need, access).map(|_| ())
    }

    /// `check`, returning the OR of the pages' permission bytes, so a store learns whether it
    /// hits a code page from the same lookup (D49).
    #[inline]
    fn check_bits(&self, addr: u64, len: u64, need: u8, access: Access) -> Result<u8, MemFault> {
        let fault = MemFault { access, addr };
        let last = addr.checked_add(len - 1).ok_or(fault)?;
        if last >= GUEST_SPACE {
            return Err(fault);
        }
        let mut bits = 0;
        for page in addr / PAGE_SIZE..=last / PAGE_SIZE {
            let b = self.prot.get(page);
            if b & need != need {
                return Err(MemFault {
                    access,
                    addr: addr.max(page * PAGE_SIZE),
                });
            }
            bits |= b;
        }
        Ok(bits)
    }

    /// Load `size` (1, 2, 4 or 8) bytes, zero-extended. Misaligned accesses are allowed.
    #[inline]
    pub fn load(&self, addr: u64, size: u64) -> Result<u64, MemFault> {
        self.check(addr, size, prot::R, Access::Load)?;
        // SAFETY: `check` proved [addr, addr+size) lies in mapped guest pages, which are
        // host-readable memory inside our reservation.
        unsafe {
            let p = self.base.add(addr as usize);
            Ok(match size {
                1 => *p as u64,
                2 => ptr::read_unaligned(p as *const u16) as u64,
                4 => ptr::read_unaligned(p as *const u32) as u64,
                _ => ptr::read_unaligned(p as *const u64),
            })
        }
    }

    /// Store the low `size` bytes of `val`. Misaligned accesses are allowed.
    #[inline]
    pub fn store(&mut self, addr: u64, size: u64, val: u64) -> Result<(), MemFault> {
        if self.check_bits(addr, size, prot::W, Access::Store)? & CODE != 0 {
            self.uncode_range(addr, size);
        }
        let old = self.write_log.is_some().then(|| self.peek(addr, size));
        if let (Some(log), Some(old)) = (self.write_log.as_mut(), old) {
            log.push((addr, size, old));
        }
        // SAFETY: as in `load`; guest-writable pages are host-writable (`host_prot`).
        unsafe {
            let p = self.base.add(addr as usize);
            match size {
                1 => *p = val as u8,
                2 => ptr::write_unaligned(p as *mut u16, val as u16),
                4 => ptr::write_unaligned(p as *mut u32, val as u32),
                _ => ptr::write_unaligned(p as *mut u64, val),
            }
        }
        Ok(())
    }

    /// Load for a read-modify-write (AMO): requires both R and W, faults as a store.
    #[inline]
    pub fn load_for_amo(&self, addr: u64, size: u64) -> Result<u64, MemFault> {
        self.check(addr, size, prot::R | prot::W, Access::Store)?;
        self.load(addr, size).map_err(|_| MemFault {
            access: Access::Store,
            addr,
        })
    }

    /// Fetch an instruction halfword (requires X).
    #[inline]
    pub fn fetch16(&self, addr: u64) -> Result<u16, MemFault> {
        self.check(addr, 2, prot::X, Access::Fetch)?;
        // SAFETY: as in `load`.
        Ok(unsafe { ptr::read_unaligned(self.base.add(addr as usize) as *const u16) })
    }

    /// Guest memory as a host slice, if every page has permissions `need`. Used by syscalls
    /// and the loader; the slice borrows `self`, so it cannot outlive a remapping.
    pub fn slice(&self, addr: GuestVirt, len: u64, need: u8) -> Result<&[u8], MemFault> {
        if len == 0 {
            return Ok(&[]);
        }
        self.check(addr.0, len, need, Access::Load)?;
        // SAFETY: range checked as mapped and readable; lifetime tied to &self.
        Ok(unsafe { std::slice::from_raw_parts(self.base.add(addr.0 as usize), len as usize) })
    }

    /// Mutable guest memory as a host slice, if every page has permissions `need`.
    pub fn slice_mut(
        &mut self,
        addr: GuestVirt,
        len: u64,
        need: u8,
    ) -> Result<&mut [u8], MemFault> {
        if len == 0 {
            return Ok(&mut []);
        }
        self.check(addr.0, len, need, Access::Store)?;
        self.uncode_range(addr.0, len);
        // SAFETY: range checked as mapped; host pages are writable; exclusive via &mut self.
        Ok(unsafe { std::slice::from_raw_parts_mut(self.base.add(addr.0 as usize), len as usize) })
    }

    /// Copy `data` into mapped guest memory regardless of guest permissions (loader use).
    /// Pages that are not host-writable are made writable for the copy and restored after.
    pub fn write_bytes(&mut self, addr: GuestVirt, data: &[u8]) -> Result<(), MemFault> {
        if data.is_empty() {
            return Ok(());
        }
        self.check(addr.0, data.len() as u64, MAPPED, Access::Store)?;
        self.uncode_range(addr.0, data.len() as u64);
        let (start, end) = (page_floor(addr.0), page_ceil(addr.0 + data.len() as u64));
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        self.set_host_prot(start, end, rw)
            .expect("mprotect of a mapped guest range");
        // SAFETY: range checked as mapped and made host-writable just above.
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), self.base.add(addr.0 as usize), data.len());
        }
        for page in start / PAGE_SIZE..end / PAGE_SIZE {
            let h = host_prot(self.prot.get(page) & prot::RWX);
            if h != rw {
                let a = page * PAGE_SIZE;
                self.set_host_prot(a, a + PAGE_SIZE, h)
                    .expect("mprotect of a mapped guest page");
            }
        }
        Ok(())
    }

    /// Read `size` (1, 2, 4 or 8) bytes without a permission check. The caller guarantees the
    /// range is host-readable (it was just written, or is guest-readable).
    pub fn peek(&self, addr: u64, size: u64) -> u64 {
        assert!(addr.checked_add(size).is_some_and(|e| e <= GUEST_SPACE));
        // SAFETY: in the reservation; readability is the caller's contract (a violation is a
        // host SIGSEGV, never memory corruption).
        unsafe {
            let p = self.base.add(addr as usize);
            match size {
                1 => *p as u64,
                2 => ptr::read_unaligned(p as *const u16) as u64,
                4 => ptr::read_unaligned(p as *const u32) as u64,
                _ => ptr::read_unaligned(p as *const u64),
            }
        }
    }

    // ------------------------------------------------ self-modifying code (D49) ----

    /// Mark the page containing `addr` as holding translated code. A guest-writable page
    /// becomes read-only on the host, so direct-mode JIT stores to it fault (and are
    /// recognized as SMC); interpreter and helper stores see the mark in `store`. Returns true
    /// if the page was not marked before.
    pub fn mark_code(&mut self, addr: u64) -> bool {
        let page = addr / PAGE_SIZE;
        let b = self.prot.get(page);
        if b & MAPPED == 0 || b & CODE != 0 {
            return false;
        }
        self.prot.set(page, b | CODE);
        if b & prot::W != 0 {
            let a = page * PAGE_SIZE;
            self.set_host_prot(a, a + PAGE_SIZE, host_prot(b & prot::RWX & !prot::W))
                .expect("mprotect of a mapped guest page");
        }
        true
    }

    /// Does the page containing `addr` hold translated code?
    #[inline]
    pub fn is_code(&self, addr: u64) -> bool {
        addr < GUEST_SPACE && self.prot.get(addr / PAGE_SIZE) & CODE != 0
    }

    /// Drop the code mark of page `page` (a page number): restore host write access and
    /// report the page in `smc_pages` so the engines invalidate what they translated from it.
    fn uncode(&mut self, page: u64) {
        let b = self.prot.get(page);
        if b & CODE == 0 {
            return;
        }
        self.prot.set(page, b & !CODE);
        if b & prot::W != 0 {
            let a = page * PAGE_SIZE;
            self.set_host_prot(a, a + PAGE_SIZE, host_prot(b & prot::RWX))
                .expect("mprotect of a mapped guest page");
        }
        self.smc_pages.push(page * PAGE_SIZE);
        self.smc_epoch += 1;
        self.smc_log.push(page * PAGE_SIZE);
    }

    /// A write to `[addr, addr+len)` is about to happen (range already checked).
    #[inline]
    fn uncode_range(&mut self, addr: u64, len: u64) {
        let (first, last) = (addr / PAGE_SIZE, (addr + len - 1) / PAGE_SIZE);
        if (self.prot.get(first) | self.prot.get(last)) & CODE == 0 && last - first <= 1 {
            return;
        }
        for page in first..=last {
            self.uncode(page);
        }
    }

    /// Pages `[start, end)` are about to be remapped or change permissions.
    fn forget_code(&mut self, start: u64, end: u64) {
        for page in start / PAGE_SIZE..end / PAGE_SIZE {
            self.uncode(page);
        }
    }

    /// Re-mark pages (lockstep: the reference run's writes are undone, so the JIT run must see
    /// the same code pages).
    pub fn remark_code(&mut self, pages: &[u64]) {
        for &a in pages {
            self.mark_code(a);
        }
    }

    /// Add a memory-mapped device at physical `[base, base + size)` (outside RAM).
    pub fn add_device(&mut self, base: u64, size: u64, dev: Box<dyn Mmio>) {
        self.devices.push(Device { base, size, dev });
        self.devices.sort_by_key(|d| d.base);
    }

    /// Index of the device decoding physical address `pa`.
    pub fn device_at(&self, pa: u64) -> Option<usize> {
        let i = self
            .devices
            .partition_point(|d| d.base <= pa)
            .checked_sub(1)?;
        let d = &self.devices[i];
        (pa - d.base < d.size).then_some(i)
    }

    /// Device read; `None` if no device decodes `pa` (an access fault).
    pub fn mmio_read(&mut self, pa: u64, size: u64) -> Option<u64> {
        let i = self.device_at(pa)?;
        let mut op = MmioOp {
            write: false,
            pa,
            size,
            val: 0,
        };
        if let Some(log) = self.mmio_log.as_mut().filter(|l| l.replay) {
            return Some(log.replay(op));
        }
        let d = &mut self.devices[i];
        op.val = d.dev.read(pa - d.base, size);
        if let Some(log) = self.mmio_log.as_mut() {
            log.ops.push(op);
        }
        Some(op.val)
    }

    /// Device write; `None` if no device decodes `pa` (an access fault).
    pub fn mmio_write(&mut self, pa: u64, size: u64, val: u64) -> Option<()> {
        let i = self.device_at(pa)?;
        let d = &mut self.devices[i];
        // Devices see exactly the stored bytes (found by tests/softmmu.rs: the interpreter
        // passed the whole register, the JIT a size-truncated constant).
        let val = if size < 8 {
            val & ((1 << (8 * size)) - 1)
        } else {
            val
        };
        let op = MmioOp {
            write: true,
            pa,
            size,
            val,
        };
        if let Some(log) = self.mmio_log.as_mut() {
            if log.replay {
                log.replay(op);
                return Some(());
            }
            log.ops.push(op);
        }
        d.dev.write(pa - d.base, size, val);
        Some(())
    }

    /// Undo stores recorded in a write log (newest first), restoring the old bytes.
    pub fn undo_writes(&mut self, log: &[(u64, u64, u64)]) {
        for &(addr, size, old) in log.iter().rev() {
            // SAFETY: each entry records a store that succeeded, so the range is mapped and
            // guest-writable, hence host-writable.
            unsafe {
                let p = self.base.add(addr as usize);
                match size {
                    1 => *p = old as u8,
                    2 => ptr::write_unaligned(p as *mut u16, old as u16),
                    4 => ptr::write_unaligned(p as *mut u32, old as u32),
                    _ => ptr::write_unaligned(p as *mut u64, old),
                }
            }
        }
    }
}

/// Internal marker bit: the page is mapped (so a page with no guest permissions is still
/// distinguishable from an unmapped one).
const MAPPED: u8 = 0x80;
/// Internal marker bit: the page holds translated code (D49).
const CODE: u8 = 0x40;

impl Drop for DirectMem {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the reservation created in `new`.
        unsafe {
            libc::munmap(
                self.reserve as *mut libc::c_void,
                GUEST_SPACE as usize + 2 * GUARD,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_access_unmap() {
        let mut m = DirectMem::new().unwrap();
        let a = 0x1_0000;
        assert!(m.load(a, 8).is_err(), "unmapped read must fault");
        m.map(GuestVirt(a), 0x2000, prot::RW).unwrap();
        m.store(a + 0xffe, 4, 0xdead_beef).unwrap(); // crosses into the second page
        assert_eq!(m.load(a + 0xffe, 4).unwrap(), 0xdead_beef);
        assert_eq!(m.load(a + 0x1000, 1).unwrap(), 0xad); // LE bytes: ef be | ad de
        assert!(m.fetch16(a).is_err(), "no X permission");
        m.protect(GuestVirt(a), 0x1000, prot::R).unwrap();
        assert_eq!(
            m.store(a, 1, 0),
            Err(MemFault {
                access: Access::Store,
                addr: a
            })
        );
        m.unmap(GuestVirt(a), 0x2000).unwrap();
        assert!(m.load(a + 0x1000, 1).is_err());
    }

    #[test]
    fn page_crossing_fault_reports_first_bad_byte() {
        let mut m = DirectMem::new().unwrap();
        m.map(GuestVirt(0x4000), 0x1000, prot::RW).unwrap();
        let f = m.load(0x4ffc, 8).unwrap_err();
        assert_eq!(f.addr, 0x5000);
    }

    #[test]
    fn out_of_space_is_rejected() {
        let mut m = DirectMem::new().unwrap();
        assert!(
            m.map(GuestVirt(GUEST_SPACE - 0x1000), 0x2000, prot::RW)
                .is_err()
        );
        assert!(m.load(u64::MAX - 3, 8).is_err());
        assert!(m.load(GUEST_SPACE, 1).is_err());
    }

    #[test]
    fn mapped_without_permissions() {
        let mut m = DirectMem::new().unwrap();
        m.map(GuestVirt(0x8000), 0x1000, 0).unwrap();
        assert!(m.is_mapped(0x8000));
        assert_eq!(m.prot_of(0x8000), 0);
        assert!(m.load(0x8000, 1).is_err());
        m.write_bytes(GuestVirt(0x8000), b"ok").unwrap(); // loader ignores guest perms
    }
}
