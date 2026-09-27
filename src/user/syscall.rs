//! RISC-V Linux syscall emulation (CLAUDE.md §19, P1.15).
//!
//! Syscall number in a7, arguments in a0–a5, result in a0 (negative errno on failure). riscv64
//! uses the asm-generic syscall table; errno values, open(2) flags, mmap(2) flags/prot bits,
//! `timespec`, `iovec`, `statx`, `rlimit` and `utsname` have the same layout on x86-64, so those
//! are passed through with guest pointers translated. `struct stat` differs and is converted.

use std::collections::HashSet;
use std::ffi::CString;

use crate::interp::Engine;
use crate::mem::{GuestVirt, PAGE_SIZE, page_ceil, prot};

use super::loader::{MMAP_TOP, Process};

const EPERM: i64 = 1;
const ENOMEM: i64 = 12;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;
const ENOTTY: i64 = 25;
const ENAMETOOLONG: i64 = 36;
const ENOSYS: i64 = 38;

/// Result of servicing one syscall.
pub enum SysOut {
    /// Return this value in a0 and continue.
    Ret(i64),
    /// The process exits with this status (exit_group, or a fatal signal).
    Exit(i32),
    /// This thread exits (exit); the process ends with its last thread (Phase 10).
    ThreadExit(i32),
    /// Create a thread (clone with CLONE_VM | CLONE_THREAD); a0 of the parent gets its tid.
    Clone(CloneArgs),
    /// A host syscall that may block (futex wait, sleeps): the thread layer runs it without
    /// holding the guest lock. Pointer arguments are already host addresses.
    Block(i64, [u64; 6]),
    /// The registers were set by the syscall itself (rt_sigreturn): no a0, no pc advance.
    NoRet,
}

/// `clone(flags, newsp, parent_tid, tls, child_tid)` (asm-generic argument order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloneArgs {
    pub flags: u64,
    pub stack: u64,
    pub parent_tid: u64,
    pub tls: u64,
    pub child_tid: u64,
}

pub const CLONE_VM: u64 = 0x100;
pub const CLONE_THREAD: u64 = 0x10000;
pub const CLONE_SETTLS: u64 = 0x80000;
pub const CLONE_PARENT_SETTID: u64 = 0x100000;
pub const CLONE_CHILD_CLEARTID: u64 = 0x200000;
pub const CLONE_CHILD_SETTID: u64 = 0x1000000;

/// Host syscall return value → guest return value (`-errno` on failure).
fn host_ret(r: i64) -> i64 {
    if r == -1 {
        -(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(EINVAL as i32) as i64)
    } else {
        r
    }
}

/// Per-process syscall state.
#[derive(Default)]
pub struct Syscalls {
    warned: HashSet<u64>,
    /// Emit a line per syscall on stderr.
    pub strace: bool,
}

/// Read a NUL-terminated guest string (at most 4096 bytes).
fn guest_cstr(p: &Process, addr: u64) -> Result<CString, i64> {
    let mut v = Vec::new();
    for i in 0..4096 {
        let b = p.mem.load(addr.wrapping_add(i), 1).map_err(|_| -EFAULT)? as u8;
        if b == 0 {
            return CString::new(v).map_err(|_| -EINVAL);
        }
        v.push(b);
    }
    Err(-ENAMETOOLONG)
}

/// A path the guest reads through: an absolute path is looked up in the sysroot first, as
/// qemu's `-L` does (Phase 10: dynamically linked programs find ld.so's libraries there).
fn guest_path(p: &Process, addr: u64) -> Result<CString, i64> {
    use std::os::unix::ffi::OsStringExt;
    let path = guest_cstr(p, addr)?;
    let (Some(root), Ok(s)) = (&p.sysroot, path.to_str()) else {
        return Ok(path);
    };
    let host = super::loader::in_sysroot(Some(root), s);
    CString::new(host.into_os_string().into_vec()).map_err(|_| -EINVAL)
}

/// Host pointer to `len` guest bytes with permissions `need`, or -EFAULT.
fn gptr(p: &Process, addr: u64, len: u64, need: u8) -> Result<*mut libc::c_void, i64> {
    if len == 0 {
        return Ok(std::ptr::null_mut());
    }
    p.mem
        .slice(GuestVirt(addr), len, need)
        .map(|s| s.as_ptr() as *mut libc::c_void)
        .map_err(|_| -EFAULT)
}

/// Convert a host (x86-64) `struct stat` to the riscv64 asm-generic 128-byte layout.
fn stat_to_guest(st: &libc::stat) -> [u8; 128] {
    let mut b = [0u8; 128];
    let mut put = |off: usize, bytes: &[u8]| b[off..off + bytes.len()].copy_from_slice(bytes);
    put(0, &st.st_dev.to_le_bytes());
    put(8, &st.st_ino.to_le_bytes());
    put(16, &st.st_mode.to_le_bytes());
    put(20, &(st.st_nlink as u32).to_le_bytes());
    put(24, &st.st_uid.to_le_bytes());
    put(28, &st.st_gid.to_le_bytes());
    put(32, &st.st_rdev.to_le_bytes());
    put(48, &st.st_size.to_le_bytes());
    put(56, &(st.st_blksize as i32).to_le_bytes());
    put(64, &st.st_blocks.to_le_bytes());
    put(72, &st.st_atime.to_le_bytes());
    put(80, &(st.st_atime_nsec as u64).to_le_bytes());
    put(88, &st.st_mtime.to_le_bytes());
    put(96, &(st.st_mtime_nsec as u64).to_le_bytes());
    put(104, &st.st_ctime.to_le_bytes());
    put(112, &(st.st_ctime_nsec as u64).to_le_bytes());
    b
}

fn write_guest(p: &mut Process, addr: u64, bytes: &[u8]) -> i64 {
    match p
        .mem
        .slice_mut(GuestVirt(addr), bytes.len() as u64, prot::W)
    {
        Ok(s) => {
            s.copy_from_slice(bytes);
            0
        }
        Err(_) => -EFAULT,
    }
}

impl Syscalls {
    /// Service the ECALL at `p.cpu.pc`.
    pub fn dispatch(&mut self, p: &mut Process, interp: &mut dyn Engine) -> SysOut {
        let nr = p.cpu.x[17];
        let a: [u64; 6] = std::array::from_fn(|i| p.cpu.x[10 + i]);
        let r = self.handle(p, interp, nr, a);
        if self.strace {
            match r {
                SysOut::Ret(v) => eprintln!(
                    "[syscall {nr}({:#x}, {:#x}, {:#x}) = {v}]",
                    a[0], a[1], a[2]
                ),
                SysOut::Exit(c) => eprintln!("[syscall {nr}: exit {c}]"),
                SysOut::ThreadExit(c) => eprintln!("[syscall {nr}: thread exit {c}]"),
                SysOut::Clone(c) => eprintln!("[syscall {nr}: clone {c:x?}]"),
                SysOut::Block(h, a) => eprintln!("[syscall {nr}: blocking host {h} {a:x?}]"),
                SysOut::NoRet => eprintln!("[syscall {nr}: registers restored]"),
            }
        }
        r
    }

    fn handle(&mut self, p: &mut Process, interp: &mut dyn Engine, nr: u64, a: [u64; 6]) -> SysOut {
        use SysOut::Ret;
        let ret = |v: Result<i64, i64>| Ret(v.unwrap_or_else(|e| e));
        match nr {
            // getcwd(buf, size)
            17 => ret((|| {
                let buf = gptr(p, a[0], a[1], prot::W)?;
                // SAFETY: buf covers a[1] writable guest bytes.
                Ok(host_ret(unsafe {
                    libc::syscall(libc::SYS_getcwd, buf, a[1])
                }))
            })()),
            // dup(fd), dup3(old, new, flags)
            // SAFETY: integer-only arguments.
            23 => Ret(host_ret(unsafe { libc::dup(a[0] as i32) } as i64)),
            // SAFETY: integer-only arguments.
            24 => Ret(host_ret(
                unsafe { libc::dup3(a[0] as i32, a[1] as i32, a[2] as i32) } as i64,
            )),
            // fcntl(fd, cmd, arg): integer-argument commands only.
            25 => match a[1] {
                0..=4 | 1030 => Ret(host_ret(unsafe {
                    // SAFETY: these commands take an integer argument.
                    libc::fcntl(a[0] as i32, a[1] as i32, a[2] as libc::c_long)
                } as i64)),
                _ => Ret(-EINVAL),
            },
            // ioctl(fd, req, arg): terminal queries used by stdio (same struct layouts).
            29 => {
                let size = match a[1] {
                    0x5401 => 36, // TCGETS: struct termios (asm-generic, NCCS = 19)
                    0x5413 => 8,  // TIOCGWINSZ: struct winsize
                    _ => return Ret(-ENOTTY),
                };
                ret((|| {
                    let buf = gptr(p, a[2], size, prot::W)?;
                    // SAFETY: buf covers `size` writable bytes, the size of the ioctl's struct.
                    Ok(host_ret(
                        unsafe { libc::ioctl(a[0] as i32, a[1], buf) } as i64
                    ))
                })())
            }
            // mkdirat, unlinkat, faccessat
            34 | 35 | 48 => ret((|| {
                let path = if nr == 48 {
                    guest_path(p, a[1])?
                } else {
                    guest_cstr(p, a[1])?
                };
                // SAFETY: path is a valid C string; the other arguments are integers.
                Ok(host_ret(unsafe {
                    match nr {
                        34 => libc::mkdirat(a[0] as i32, path.as_ptr(), a[2] as u32) as i64,
                        35 => libc::unlinkat(a[0] as i32, path.as_ptr(), a[2] as i32) as i64,
                        _ => libc::syscall(
                            libc::SYS_faccessat,
                            a[0] as i32,
                            path.as_ptr(),
                            a[2] as i32,
                        ),
                    }
                }))
            })()),
            // openat(dirfd, path, flags, mode): asm-generic O_* flags equal x86-64's.
            56 => ret((|| {
                let path = guest_path(p, a[1])?;
                // SAFETY: path is a valid C string.
                Ok(host_ret(unsafe {
                    libc::openat(
                        a[0] as i32,
                        path.as_ptr(),
                        a[2] as i32,
                        a[3] as libc::c_uint,
                    )
                } as i64))
            })()),
            // SAFETY: integer-only arguments.
            57 => Ret(host_ret(unsafe { libc::close(a[0] as i32) } as i64)),
            // getdents64: struct linux_dirent64 is architecture-independent.
            61 => ret((|| {
                let buf = gptr(p, a[1], a[2], prot::W)?;
                // SAFETY: buf covers a[2] writable bytes.
                Ok(host_ret(unsafe {
                    libc::syscall(libc::SYS_getdents64, a[0] as i32, buf, a[2])
                }))
            })()),
            // SAFETY: integer-only arguments.
            62 => Ret(host_ret(unsafe {
                libc::lseek(a[0] as i32, a[1] as i64, a[2] as i32)
            })),
            // read(fd, buf, count)
            63 => ret((|| {
                let buf = gptr(p, a[1], a[2], prot::W)?;
                // SAFETY: buf covers a[2] writable guest bytes.
                Ok(host_ret(
                    unsafe { libc::read(a[0] as i32, buf, a[2] as usize) } as i64,
                ))
            })()),
            // write(fd, buf, count)
            64 => ret((|| {
                let buf = gptr(p, a[1], a[2], prot::R)?;
                // SAFETY: buf covers a[2] readable guest bytes.
                Ok(host_ret(
                    unsafe { libc::write(a[0] as i32, buf, a[2] as usize) } as i64,
                ))
            })()),
            // pread64 / pwrite64(fd, buf, count, offset)
            67 | 68 => ret((|| {
                let need = if nr == 67 { prot::W } else { prot::R };
                let buf = gptr(p, a[1], a[2], need)?;
                // SAFETY: buf covers a[2] guest bytes with the needed access.
                Ok(host_ret(unsafe {
                    if nr == 67 {
                        libc::pread(a[0] as i32, buf, a[2] as usize, a[3] as i64) as i64
                    } else {
                        libc::pwrite(a[0] as i32, buf, a[2] as usize, a[3] as i64) as i64
                    }
                }))
            })()),
            // readv / writev(fd, iov, iovcnt): struct iovec {base, len} is identical.
            65 | 66 => ret(self.rw_vec(p, nr == 65, a[0] as i32, a[1], a[2])),
            // readlinkat(dirfd, path, buf, size)
            78 => ret((|| {
                let path = guest_cstr(p, a[1])?;
                if path.as_bytes() == b"/proc/self/exe" {
                    let exe = p.exe_path.clone().into_bytes();
                    let n = exe.len().min(a[3] as usize);
                    let r = write_guest(p, a[2], &exe[..n]);
                    return if r < 0 { Err(r) } else { Ok(n as i64) };
                }
                let path = guest_path(p, a[1])?;
                let buf = gptr(p, a[2], a[3], prot::W)?;
                // SAFETY: path is a C string; buf covers a[3] writable bytes.
                Ok(host_ret(unsafe {
                    libc::readlinkat(
                        a[0] as i32,
                        path.as_ptr(),
                        buf as *mut libc::c_char,
                        a[3] as usize,
                    )
                } as i64))
            })()),
            // newfstatat(dirfd, path, statbuf, flags) / fstat(fd, statbuf)
            79 | 80 => ret((|| {
                // SAFETY: an all-zero stat is a valid value to be overwritten.
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                let r = if nr == 79 {
                    let path = guest_path(p, a[1])?;
                    // SAFETY: path is a C string; st is a valid out-pointer.
                    unsafe { libc::fstatat(a[0] as i32, path.as_ptr(), &mut st, a[3] as i32) }
                } else {
                    // SAFETY: st is a valid out-pointer.
                    unsafe { libc::fstat(a[0] as i32, &mut st) }
                };
                let r = host_ret(r as i64);
                if r < 0 {
                    return Err(r);
                }
                let buf = if nr == 79 { a[2] } else { a[1] };
                let w = write_guest(p, buf, &stat_to_guest(&st));
                if w < 0 { Err(w) } else { Ok(0) }
            })()),
            // exit (this thread) / exit_group (the process)
            93 => SysOut::ThreadExit((a[0] & 0xff) as i32),
            94 => SysOut::Exit((a[0] & 0xff) as i32),
            // set_tid_address: remembered for the thread's exit (CLONE_CHILD_CLEARTID).
            96 => {
                p.clear_tid = a[0];
                Ret(p.tid)
            }
            // futex: guest memory is host memory, so the host futex does the work (threads are
            // host threads). Waits block, so they run without the guest lock.
            98 => (|| -> Result<SysOut, i64> {
                const WAIT: u64 = 0;
                const WAKE: u64 = 1;
                const REQUEUE: u64 = 3;
                const CMP_REQUEUE: u64 = 4;
                const WAIT_BITSET: u64 = 9;
                const WAKE_BITSET: u64 = 10;
                let uaddr = gptr(p, a[0], 4, prot::R)? as u64;
                Ok(match a[1] & 0x7f {
                    WAIT | WAIT_BITSET => {
                        let ts = if a[3] == 0 {
                            0
                        } else {
                            gptr(p, a[3], 16, prot::R)? as u64
                        };
                        SysOut::Block(libc::SYS_futex, [uaddr, a[1], a[2], ts, 0, a[5]])
                    }
                    // SAFETY (both host calls): the addresses are mapped guest words.
                    WAKE | WAKE_BITSET => Ret(host_ret(unsafe {
                        libc::syscall(libc::SYS_futex, uaddr, a[1], a[2], 0, 0, a[5])
                    })),
                    REQUEUE | CMP_REQUEUE => {
                        let uaddr2 = gptr(p, a[4], 4, prot::R)? as u64;
                        Ret(host_ret(unsafe {
                            libc::syscall(libc::SYS_futex, uaddr, a[1], a[2], a[3], uaddr2, a[5])
                        }))
                    }
                    _ => Ret(-ENOSYS),
                })
            })()
            .unwrap_or_else(Ret),
            // set_robust_list
            99 => Ret(0),
            // nanosleep / clock_gettime / clock_getres / gettimeofday: timespec/timeval match.
            // Sleeps block: they run without the guest lock (Phase 10).
            101 => (|| -> Result<SysOut, i64> {
                let req = gptr(p, a[0], 16, prot::R)? as u64;
                let rem = if a[1] == 0 {
                    0
                } else {
                    gptr(p, a[1], 16, prot::W)? as u64
                };
                Ok(SysOut::Block(libc::SYS_nanosleep, [req, rem, 0, 0, 0, 0]))
            })()
            .unwrap_or_else(Ret),
            // clock_nanosleep(clock, flags, req, rem)
            115 => (|| -> Result<SysOut, i64> {
                let req = gptr(p, a[2], 16, prot::R)? as u64;
                let rem = if a[3] == 0 {
                    0
                } else {
                    gptr(p, a[3], 16, prot::W)? as u64
                };
                Ok(SysOut::Block(
                    libc::SYS_clock_nanosleep,
                    [a[0], a[1], req, rem, 0, 0],
                ))
            })()
            .unwrap_or_else(Ret),
            113 | 114 | 169 => ret((|| {
                let buf = gptr(p, if nr == 169 { a[0] } else { a[1] }, 16, prot::W)?;
                // SAFETY: buf covers a struct timespec/timeval.
                Ok(host_ret(unsafe {
                    match nr {
                        113 => libc::syscall(libc::SYS_clock_gettime, a[0] as i32, buf),
                        114 => libc::syscall(libc::SYS_clock_getres, a[0] as i32, buf),
                        _ => libc::syscall(libc::SYS_gettimeofday, buf, 0),
                    }
                }))
            })()),
            124 => Ret(0), // sched_yield
            // kill(pid, sig) / tkill(tid, sig) / tgkill(tgid, tid, sig) (Phase 10: signals to
            // this process are delivered to its handlers; others go to the host).
            129..=131 => {
                use super::guest_signal as gs;
                let pid = std::process::id() as i64;
                let (target, tid, sig) = match nr {
                    129 => (a[0] as i64, None, a[1]),
                    130 => (pid, Some(a[0] as i64), a[1]),
                    _ => (a[0] as i64, Some(a[1] as i64), a[2]),
                };
                if sig > gs::NSIG as u64 {
                    return Ret(-EINVAL);
                }
                if target != pid && target != 0 {
                    // SAFETY: plain host kill of another process.
                    return Ret(host_ret(
                        unsafe { libc::kill(target as i32, sig as i32) } as i64
                    ));
                }
                if sig != 0 {
                    match tid {
                        Some(t) if t != p.tid => {
                            *p.thread_pending.entry(t).or_default() |= 1 << (sig - 1);
                        }
                        _ => gs::raise(p, sig as u32),
                    }
                }
                Ret(0)
            }
            // sigaltstack(ss, old): stack_t {ss_sp, ss_flags (i32), ss_size}.
            132 => {
                let old = p.altstack;
                if a[1] != 0 {
                    let flags = if old[2] == 0 { 2 } else { old[1] }; // SS_DISABLE
                    let mut b = [0u8; 24];
                    b[..8].copy_from_slice(&old[0].to_le_bytes());
                    b[8..12].copy_from_slice(&(flags as u32).to_le_bytes());
                    b[16..].copy_from_slice(&old[2].to_le_bytes());
                    let w = write_guest(p, a[1], &b);
                    if w < 0 {
                        return Ret(w);
                    }
                }
                if a[0] != 0 {
                    let rd = |o| p.mem.load(a[0] + o, 8);
                    let (Ok(sp), Ok(fl), Ok(sz)) = (rd(0), rd(8), rd(16)) else {
                        return Ret(-EFAULT);
                    };
                    let fl = fl & 0xffff_ffff;
                    p.altstack = if fl & 2 != 0 { [0; 3] } else { [sp, fl, sz] };
                }
                Ret(0)
            }
            // rt_sigaction(sig, act, oldact): struct sigaction {handler, flags, mask}.
            134 => {
                use super::guest_signal as gs;
                let sig = a[0] as usize;
                if sig == 0 || sig > gs::NSIG {
                    return Ret(-EINVAL);
                }
                if a[2] != 0 {
                    let o = p.sigactions[sig];
                    let mut b = [0u8; 24];
                    b[..8].copy_from_slice(&o.handler.to_le_bytes());
                    b[8..16].copy_from_slice(&o.flags.to_le_bytes());
                    b[16..].copy_from_slice(&o.mask.to_le_bytes());
                    let w = write_guest(p, a[2], &b);
                    if w < 0 {
                        return Ret(w);
                    }
                }
                if a[1] != 0 {
                    if sig as u32 == gs::SIGKILL || sig as u32 == gs::SIGSTOP {
                        return Ret(-EINVAL);
                    }
                    let rd = |o| p.mem.load(a[1] + o, 8);
                    let (Ok(handler), Ok(flags), Ok(mask)) = (rd(0), rd(8), rd(16)) else {
                        return Ret(-EFAULT);
                    };
                    p.sigactions[sig] = gs::SigAction {
                        handler,
                        flags,
                        mask: mask & !gs::UNBLOCKABLE,
                    };
                }
                Ret(0)
            }
            // rt_sigprocmask(how, set, oldset)
            135 => {
                use super::guest_signal as gs;
                if a[2] != 0 {
                    let w = write_guest(p, a[2], &p.sigmask.to_le_bytes());
                    if w < 0 {
                        return Ret(w);
                    }
                }
                if a[1] != 0 {
                    let Ok(set) = p.mem.load(a[1], 8) else {
                        return Ret(-EFAULT);
                    };
                    p.sigmask = match a[0] {
                        0 => p.sigmask | set,
                        1 => p.sigmask & !set,
                        2 => set,
                        _ => return Ret(-EINVAL),
                    } & !gs::UNBLOCKABLE;
                }
                Ret(0)
            }
            // rt_sigreturn: registers come back from the signal frame.
            139 => {
                if super::guest_signal::sigreturn(p) {
                    SysOut::NoRet
                } else {
                    eprintln!("bridgev: bad signal frame at sp {:#x}", p.cpu.x[2]);
                    SysOut::Exit(128 + 11)
                }
            }
            // uname: struct utsname is 6 × 65 bytes on both architectures.
            160 => {
                // SAFETY: an all-zero utsname is valid; uname fills it.
                let mut u: libc::utsname = unsafe { std::mem::zeroed() };
                // SAFETY: u is a valid out-pointer.
                unsafe { libc::uname(&mut u) };
                let mut buf = [0u8; 390];
                let fields: [&[libc::c_char]; 6] = [
                    &u.sysname,
                    &u.nodename,
                    &u.release,
                    &u.version,
                    &u.machine,
                    &u.domainname,
                ];
                for (i, f) in fields.iter().enumerate() {
                    for (j, &c) in f.iter().take(64).enumerate() {
                        buf[i * 65 + j] = c as u8;
                    }
                }
                buf[4 * 65..5 * 65].fill(0);
                buf[4 * 65..4 * 65 + 7].copy_from_slice(b"riscv64");
                Ret(write_guest(p, a[0], &buf))
            }
            // getpid, getppid, getuid, geteuid, getgid, getegid, gettid
            172 => Ret(std::process::id() as i64),
            178 => Ret(p.tid), // gettid
            // clone: threads only (fork-style clones are not supported).
            220 => {
                let c = CloneArgs {
                    flags: a[0],
                    stack: a[1],
                    parent_tid: a[2],
                    tls: a[3],
                    child_tid: a[4],
                };
                if c.flags & (CLONE_VM | CLONE_THREAD) == CLONE_VM | CLONE_THREAD {
                    SysOut::Clone(c)
                } else {
                    if self.warned.insert(nr) {
                        eprintln!(
                            "bridgev: clone without CLONE_VM|CLONE_THREAD (fork) unsupported"
                        );
                    }
                    Ret(-ENOSYS)
                }
            }
            435 => Ret(-ENOSYS), // clone3: glibc falls back to clone
            // SAFETY (all four): argument-less libc getters.
            173 => Ret(unsafe { libc::getppid() } as i64),
            174 => Ret(unsafe { libc::getuid() } as i64),
            175 => Ret(unsafe { libc::geteuid() } as i64),
            176 => Ret(unsafe { libc::getgid() } as i64),
            177 => Ret(unsafe { libc::getegid() } as i64),
            214 => Ret(self.brk(p, a[0])),
            215 => Ret(self.munmap(p, interp, a[0], a[1])),
            222 => Ret(self.mmap(p, interp, a)),
            // mprotect(addr, len, prot): PROT_* bits equal our R/W/X bits.
            226 => {
                if !a[0].is_multiple_of(PAGE_SIZE) {
                    return Ret(-EINVAL);
                }
                match p.mem.protect(GuestVirt(a[0]), a[1], (a[2] & 7) as u8) {
                    Ok(()) => Ret(0),
                    Err(_) => Ret(-ENOMEM),
                }
            }
            // madvise: MADV_DONTNEED zero-fills anonymous memory; other advice is ignored.
            233 => {
                if a[2] == 4 {
                    match gptr(p, a[0], a[1], prot::R) {
                        // SAFETY: the range is mapped guest memory inside our reservation.
                        Ok(ptr) => Ret(host_ret(
                            unsafe { libc::madvise(ptr, a[1] as usize, 4) } as i64
                        )),
                        Err(e) => Ret(e),
                    }
                } else {
                    Ret(0)
                }
            }
            258 => Ret(-ENOSYS), // riscv_hwprobe: glibc falls back without it
            259 => {
                interp.fence_i(); // riscv_flush_icache
                Ret(0)
            }
            // prlimit64(pid, resource, new, old): struct rlimit {cur, max} is identical.
            261 => ret((|| {
                if a[0] != 0 && a[0] != std::process::id() as u64 {
                    return Err(-EPERM);
                }
                let new = gptr(p, a[2], if a[2] == 0 { 0 } else { 16 }, prot::R)?;
                let old = gptr(p, a[3], if a[3] == 0 { 0 } else { 16 }, prot::W)?;
                // SAFETY: pointers cover a struct rlimit each (or are null).
                Ok(host_ret(unsafe {
                    libc::syscall(libc::SYS_prlimit64, 0, a[1] as i32, new, old)
                }))
            })()),
            // getrandom(buf, len, flags)
            278 => ret((|| {
                let buf = gptr(p, a[0], a[1], prot::W)?;
                // SAFETY: buf covers a[1] writable bytes.
                Ok(host_ret(
                    unsafe { libc::getrandom(buf, a[1] as usize, a[2] as u32) } as i64,
                ))
            })()),
            // statx: struct statx is architecture-independent (256 bytes).
            291 => ret((|| {
                let path = guest_path(p, a[1])?;
                let buf = gptr(p, a[4], 256, prot::W)?;
                // SAFETY: path is a C string; buf covers a struct statx.
                Ok(host_ret(unsafe {
                    libc::syscall(
                        libc::SYS_statx,
                        a[0] as i32,
                        path.as_ptr(),
                        a[2] as i32,
                        a[3] as u32,
                        buf,
                    )
                }))
            })()),
            293 => Ret(-ENOSYS), // rseq: glibc works without it
            _ => {
                if self.warned.insert(nr) {
                    eprintln!("bridgev: unsupported syscall {nr} (returning -ENOSYS)");
                }
                Ret(-ENOSYS)
            }
        }
    }

    fn rw_vec(&self, p: &Process, read: bool, fd: i32, iov: u64, cnt: u64) -> Result<i64, i64> {
        if cnt > 1024 {
            return Err(-EINVAL);
        }
        let mut host = Vec::with_capacity(cnt as usize);
        for i in 0..cnt {
            let base = p.mem.load(iov + 16 * i, 8).map_err(|_| -EFAULT)?;
            let len = p.mem.load(iov + 16 * i + 8, 8).map_err(|_| -EFAULT)?;
            let ptr = gptr(p, base, len, if read { prot::W } else { prot::R })?;
            host.push(libc::iovec {
                iov_base: ptr,
                iov_len: len as usize,
            });
        }
        // SAFETY: every iovec points at checked guest memory of the given length.
        Ok(host_ret(unsafe {
            if read {
                libc::readv(fd, host.as_ptr(), host.len() as i32)
            } else {
                libc::writev(fd, host.as_ptr(), host.len() as i32)
            }
        } as i64))
    }

    fn brk(&self, p: &mut Process, want: u64) -> i64 {
        if want < p.brk_start {
            return p.brk as i64;
        }
        let (old_end, new_end) = (page_ceil(p.brk), page_ceil(want));
        let ok = if new_end > old_end {
            let free = (old_end..new_end)
                .step_by(PAGE_SIZE as usize)
                .all(|pg| !p.mem.is_mapped(pg));
            free && p
                .mem
                .map(GuestVirt(old_end), new_end - old_end, prot::RW)
                .is_ok()
        } else {
            new_end == old_end || p.mem.unmap(GuestVirt(new_end), old_end - new_end).is_ok()
        };
        if ok {
            p.brk = want;
        }
        p.brk as i64
    }

    /// Search downwards from MMAP_TOP for `len` bytes of unmapped space (highest fit first).
    fn find_free(&self, p: &Process, len: u64) -> Option<u64> {
        let mut cand = MMAP_TOP.checked_sub(len)?;
        'search: loop {
            for i in (0..len / PAGE_SIZE).rev() {
                let pg = cand + i * PAGE_SIZE;
                if p.mem.is_mapped(pg) {
                    cand = pg.checked_sub(len)?;
                    continue 'search;
                }
            }
            return Some(cand);
        }
    }

    fn mmap(&self, p: &mut Process, interp: &mut dyn Engine, a: [u64; 6]) -> i64 {
        const MAP_FIXED: u64 = 0x10;
        const MAP_ANONYMOUS: u64 = 0x20;
        const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;
        let (addr, len, pr, flags, fd, off) =
            (a[0], a[1], (a[2] & 7) as u8, a[3], a[4] as i32, a[5]);
        if len == 0 || !off.is_multiple_of(PAGE_SIZE) {
            return -EINVAL;
        }
        let len = page_ceil(len);
        let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
        let start = if fixed {
            if !addr.is_multiple_of(PAGE_SIZE) {
                return -EINVAL;
            }
            if flags & MAP_FIXED_NOREPLACE != 0
                && (addr..addr + len)
                    .step_by(PAGE_SIZE as usize)
                    .any(|pg| p.mem.is_mapped(pg))
            {
                return -17; // EEXIST
            }
            addr
        } else {
            match self.find_free(p, len) {
                Some(s) => s,
                None => return -ENOMEM,
            }
        };
        let file = flags & MAP_ANONYMOUS == 0;
        // A file mapping is filled through the host mapping: writable until then.
        let map_prot = if file { pr | prot::W } else { pr };
        if p.mem.map(GuestVirt(start), len, map_prot).is_err() {
            return -ENOMEM;
        }
        if file {
            // File mapping: copy the contents (MAP_PRIVATE semantics; MAP_SHARED writes are
            // not written back — acceptable for the static programs Phase 1 targets).
            let Ok(dst) = p.mem.slice_mut(GuestVirt(start), len, 0) else {
                return -EFAULT;
            };
            // SAFETY: dst covers `len` writable bytes of freshly mapped guest memory.
            let n = unsafe {
                libc::pread(
                    fd,
                    dst.as_mut_ptr() as *mut libc::c_void,
                    len as usize,
                    off as i64,
                )
            };
            if n < 0 {
                let e = host_ret(-1);
                let _ = p.mem.unmap(GuestVirt(start), len);
                return e;
            }
            if map_prot != pr && p.mem.protect(GuestVirt(start), len, pr).is_err() {
                return -ENOMEM;
            }
        }
        if pr & prot::X != 0 {
            interp.flush();
        }
        start as i64
    }

    fn munmap(&self, p: &mut Process, interp: &mut dyn Engine, addr: u64, len: u64) -> i64 {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
            return -EINVAL;
        }
        match p.mem.unmap(GuestVirt(addr), len) {
            Ok(()) => {
                interp.flush();
                0
            }
            Err(_) => -EINVAL,
        }
    }
}
