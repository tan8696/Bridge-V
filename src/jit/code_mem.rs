//! Executable code memory under a strict W^X discipline (CLAUDE.md §12, D6).
//!
//! * `DualMap` (default): a `memfd` region mapped twice, RW for emitting and patching and RX
//!   for executing. No `mprotect` per block; the two views alias the same physical pages.
//! * `Mprotect` (`--wx=mprotect`): one private mapping, RX except while the dispatcher writes
//!   to it (RW for the duration of the copy, then RX again).
//!
//! No mapping is ever RWX. All addresses handed out are RX-view addresses, and all rel32 math
//! uses them. Allocation is a bump pointer; each allocation starts on a 16-byte boundary. A
//! prefix (trampolines, helper table) survives `reset`.

use std::io;
use std::ptr;

/// How W^X is enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WxMode {
    DualMap,
    Mprotect,
}

/// The code buffer.
pub struct CodeMem {
    mode: WxMode,
    rw: *mut u8,
    rx: *mut u8,
    size: usize,
    used: usize,
    prefix: usize,
    fd: libc::c_int,
}

/// `place` failed: the buffer has no room left (the caller flushes and retries).
#[derive(Debug, PartialEq, Eq)]
pub struct Full;

/// Largest code buffer: every rel32 inside it must reach every other byte (§11.2).
pub const MAX_SIZE: usize = 1 << 30;
const PAGE: usize = 4096;
const ALIGN: usize = 16;

fn os_err(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

impl CodeMem {
    /// Map a code buffer of `size` bytes (rounded up to pages, at most 1 GiB).
    pub fn new(size: usize, mode: WxMode) -> io::Result<CodeMem> {
        if size == 0 || size > MAX_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("code cache size must be 1..=1 GiB, got {size}"),
            ));
        }
        let size = size.div_ceil(PAGE) * PAGE;
        match mode {
            WxMode::DualMap => {
                // SAFETY: plain syscalls; every result is checked, and the mappings are owned
                // by the returned CodeMem (released in Drop).
                unsafe {
                    let fd = libc::memfd_create(c"bridgev-jit".as_ptr(), libc::MFD_CLOEXEC);
                    if fd < 0 {
                        return Err(os_err("memfd_create"));
                    }
                    if libc::ftruncate(fd, size as libc::off_t) != 0 {
                        let e = os_err("ftruncate");
                        libc::close(fd);
                        return Err(e);
                    }
                    let map =
                        |prot| libc::mmap(ptr::null_mut(), size, prot, libc::MAP_SHARED, fd, 0);
                    let rw = map(libc::PROT_READ | libc::PROT_WRITE);
                    if rw == libc::MAP_FAILED {
                        let e = os_err("mmap RW view");
                        libc::close(fd);
                        return Err(e);
                    }
                    let rx = map(libc::PROT_READ | libc::PROT_EXEC);
                    if rx == libc::MAP_FAILED {
                        let e = os_err("mmap RX view");
                        libc::munmap(rw, size);
                        libc::close(fd);
                        return Err(e);
                    }
                    Ok(CodeMem {
                        mode,
                        rw: rw as *mut u8,
                        rx: rx as *mut u8,
                        size,
                        used: 0,
                        prefix: 0,
                        fd,
                    })
                }
            }
            WxMode::Mprotect => {
                // SAFETY: anonymous private mapping, checked; owned by the returned CodeMem.
                unsafe {
                    let p = libc::mmap(
                        ptr::null_mut(),
                        size,
                        libc::PROT_READ | libc::PROT_EXEC,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                    if p == libc::MAP_FAILED {
                        return Err(os_err("mmap code buffer"));
                    }
                    Ok(CodeMem {
                        mode,
                        rw: p as *mut u8,
                        rx: p as *mut u8,
                        size,
                        used: 0,
                        prefix: 0,
                        fd: -1,
                    })
                }
            }
        }
    }

    pub fn mode(&self) -> WxMode {
        self.mode
    }

    /// RX address of the first byte.
    pub fn rx_base(&self) -> u64 {
        self.rx as u64
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Bytes allocated so far (including alignment padding and the prefix).
    pub fn used(&self) -> usize {
        self.used
    }

    /// Is `addr` (an RX address) inside the buffer?
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.rx_base() && addr < self.rx_base() + self.size as u64
    }

    fn aligned_used(&self) -> usize {
        self.used.next_multiple_of(ALIGN)
    }

    /// RX address where the next `place` will put its code.
    pub fn next_addr(&self) -> u64 {
        self.rx_base() + self.aligned_used() as u64
    }

    /// Copy `code` to the next free position. `origin` must equal `next_addr()` (the code
    /// was assembled for that address). Returns the RX address of the code.
    pub fn place(&mut self, origin: u64, code: &[u8]) -> Result<u64, Full> {
        assert_eq!(
            origin,
            self.next_addr(),
            "code assembled for a different address"
        );
        let off = self.aligned_used();
        if off + code.len() > self.size {
            return Err(Full);
        }
        self.write_at(off, code);
        self.used = off + code.len();
        Ok(origin)
    }

    /// Overwrite already-placed code at RX address `addr` (chain patching, Phase 3).
    pub fn patch(&mut self, addr: u64, bytes: &[u8]) {
        assert!(self.contains(addr) && self.contains(addr + bytes.len() as u64 - 1));
        self.write_at((addr - self.rx_base()) as usize, bytes);
    }

    /// Placed code at RX address `addr`.
    pub fn read(&self, addr: u64, len: usize) -> &[u8] {
        assert!(addr >= self.rx_base() && addr + len as u64 <= self.rx_base() + self.size as u64);
        // SAFETY: the range lies inside our mapping, which is always readable (both views).
        unsafe { std::slice::from_raw_parts(self.rx.add((addr - self.rx_base()) as usize), len) }
    }

    fn write_at(&mut self, off: usize, bytes: &[u8]) {
        debug_assert!(off + bytes.len() <= self.size);
        if bytes.is_empty() {
            return;
        }
        match self.mode {
            // SAFETY: in bounds of the RW view (checked by callers); no JIT code runs while
            // the dispatcher writes (§12), so no instruction is being fetched from these bytes.
            WxMode::DualMap => unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), self.rw.add(off), bytes.len());
            },
            WxMode::Mprotect => {
                let start = off / PAGE * PAGE;
                let end = (off + bytes.len()).div_ceil(PAGE) * PAGE;
                // SAFETY: the page range is inside our mapping. It is RW only between the two
                // mprotect calls, during which no code in it executes.
                unsafe {
                    let p = self.rx.add(start) as *mut libc::c_void;
                    let rc = libc::mprotect(p, end - start, libc::PROT_READ | libc::PROT_WRITE);
                    assert_eq!(rc, 0, "mprotect RW: {}", io::Error::last_os_error());
                    ptr::copy_nonoverlapping(bytes.as_ptr(), self.rx.add(off), bytes.len());
                    let rc = libc::mprotect(p, end - start, libc::PROT_READ | libc::PROT_EXEC);
                    assert_eq!(rc, 0, "mprotect RX: {}", io::Error::last_os_error());
                }
            }
        }
    }

    /// Make everything placed so far permanent: `reset` keeps it.
    pub fn seal_prefix(&mut self) {
        self.prefix = self.used;
    }

    /// Drop all code after the prefix (full flush, §12). Old code bytes stay in memory but
    /// will be overwritten by later placements.
    pub fn reset(&mut self) {
        self.used = self.prefix;
    }
}

impl Drop for CodeMem {
    fn drop(&mut self) {
        // SAFETY: releasing exactly the mappings and fd created in `new`.
        unsafe {
            libc::munmap(self.rx as *mut libc::c_void, self.size);
            if self.mode == WxMode::DualMap {
                libc::munmap(self.rw as *mut libc::c_void, self.size);
                libc::close(self.fd);
            }
        }
    }
}

/// Lines of `/proc/self/maps` whose permissions are both writable and executable.
pub fn rwx_mappings() -> Vec<String> {
    std::fs::read_to_string("/proc/self/maps")
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            let perms = l.split_whitespace().nth(1).unwrap_or("");
            perms.as_bytes().get(1) == Some(&b'w') && perms.as_bytes().get(2) == Some(&b'x')
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x86::emit::Asm;
    use crate::backend::x86::regs::Reg;

    fn hello(mode: WxMode) {
        let mut cm = CodeMem::new(1 << 20, mode).unwrap();
        let mut a = Asm::new(cm.next_addr());
        a.mov_r32_imm(Reg::Rax, 42);
        a.ret();
        let origin = a.origin();
        let code = a.finish();
        assert_eq!(code, [0xB8, 42, 0, 0, 0, 0xC3]);
        let addr = cm.place(origin, &code).unwrap();
        assert!(
            rwx_mappings().is_empty(),
            "RWX mapping: {:?}",
            rwx_mappings()
        );
        // SAFETY: `addr` holds a complete `mov eax, 42; ret` function in RX memory.
        let f: extern "sysv64" fn() -> u64 = unsafe { std::mem::transmute(addr as *const u8) };
        assert_eq!(f(), 42);
        // Patch the immediate through the write path and observe it through the RX view.
        cm.patch(addr + 1, &[99]);
        assert_eq!(cm.read(addr, 2), &[0xB8, 99]);
        assert_eq!(f(), 99);
    }

    #[test]
    fn hello_jit_dualmap() {
        hello(WxMode::DualMap);
    }

    #[test]
    fn hello_jit_mprotect() {
        hello(WxMode::Mprotect);
    }

    #[test]
    fn bump_alignment_full_and_reset() {
        let mut cm = CodeMem::new(8192, WxMode::DualMap).unwrap();
        let a0 = cm.next_addr();
        assert_eq!(cm.place(a0, &[0x90; 3]).unwrap(), a0);
        cm.seal_prefix();
        let a1 = cm.next_addr();
        assert_eq!(a1 - a0, 16);
        cm.place(a1, &[0xC3; 100]).unwrap();
        let a2 = cm.next_addr();
        assert_eq!(a2 % 16, 0);
        assert_eq!(cm.place(a2, &vec![0xCC; 9000]), Err(Full));
        cm.reset();
        assert_eq!(cm.next_addr(), a1);
        assert!(cm.contains(a0) && !cm.contains(a0 + 8192));
    }

    #[test]
    fn rejects_bad_sizes() {
        assert!(CodeMem::new(0, WxMode::DualMap).is_err());
        assert!(CodeMem::new(MAX_SIZE + 1, WxMode::DualMap).is_err());
    }
}
