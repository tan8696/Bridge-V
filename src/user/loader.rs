//! Maps an ELF into the guest address space and builds the initial process stack
//! (CLAUDE.md §14.1 layout, §19 stack/auxv; P1.14).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cpu::state::CpuState;
use crate::elf::{ET_EXEC, Elf, PF_R, PF_W, PF_X};
use crate::mem::direct::DirectMem;
use crate::mem::{GuestVirt, PAGE_SIZE, page_ceil, page_floor, prot};

/// Top of the initial stack (just below the SV39 user-space limit).
pub const STACK_TOP: u64 = 0x3f_ffff_f000;
pub const STACK_SIZE: u64 = 8 << 20;
/// mmap allocations grow down from here.
pub const MMAP_TOP: u64 = 0x3f_0000_0000;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;

/// AT_HWCAP: one bit per single-letter extension, `1 << (letter - 'A')`, for IMAFDC.
pub const HWCAP: u64 = {
    let mut bits = 0;
    let letters = *b"IMAFDC";
    let mut i = 0;
    while i < letters.len() {
        bits |= 1 << (letters[i] - b'A');
        i += 1;
    }
    bits
};

/// A loaded guest process.
pub struct Process {
    pub mem: DirectMem,
    pub cpu: Box<CpuState>,
    /// Start of the heap (end of the highest segment, page-aligned).
    pub brk_start: u64,
    /// Current program break.
    pub brk: u64,
    /// Absolute path of the executable (for `/proc/self/exe`).
    pub exe_path: String,
}

fn elf_prot(flags: u32) -> u8 {
    let mut p = 0;
    if flags & PF_R != 0 {
        p |= prot::R;
    }
    if flags & PF_W != 0 {
        p |= prot::W;
    }
    if flags & PF_X != 0 {
        p |= prot::X;
    }
    p
}

/// Load `path` with `args` (argv[0] included) and environment `envs`.
pub fn load(path: &Path, args: &[String], envs: &[String]) -> Result<Process> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let elf = Elf::parse(&data).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(i) = &elf.interp {
        bail!(
            "dynamically linked executables are not supported yet (interpreter {i}); link with -static"
        );
    }
    if elf.e_type != ET_EXEC {
        bail!("position-independent executables are not supported yet; link with -static -no-pie");
    }
    let mut mem = DirectMem::new()?;

    // Union of permissions per page: segments may share a page at their boundaries.
    let mut pages: BTreeMap<u64, u8> = BTreeMap::new();
    let mut end = 0;
    for seg in elf.loads() {
        let p = elf_prot(seg.flags);
        for pg in
            (page_floor(seg.vaddr)..page_ceil(seg.vaddr + seg.memsz)).step_by(PAGE_SIZE as usize)
        {
            *pages.entry(pg).or_default() |= p;
        }
        end = end.max(seg.vaddr + seg.memsz);
    }
    // Map maximal runs of consecutive pages with equal permissions.
    let mut iter = pages.iter().peekable();
    while let Some((&start, &p)) = iter.next() {
        let mut next = start + PAGE_SIZE;
        while let Some(&(&pg, &q)) = iter.peek() {
            if pg != next || q != p {
                break;
            }
            next += PAGE_SIZE;
            iter.next();
        }
        mem.map(GuestVirt(start), next - start, p)?;
    }
    for seg in elf.loads() {
        mem.write_bytes(GuestVirt(seg.vaddr), elf.segment_data(seg))
            .map_err(|f| anyhow::anyhow!("writing segment at {:#x}: {f:?}", seg.vaddr))?;
    }

    mem.map(GuestVirt(STACK_TOP - STACK_SIZE), STACK_SIZE, prot::RW)?;
    let exe_path = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let sp = build_stack(&mut mem, &elf, &exe_path, args, envs)?;

    let mut cpu = CpuState::new_user(elf.entry);
    cpu.x[2] = sp;
    let brk = page_ceil(end);
    Ok(Process {
        mem,
        cpu,
        brk_start: brk,
        brk,
        exe_path,
    })
}

/// Build the initial stack; returns the initial `sp` (16-byte aligned), which points at argc.
///
/// Layout, from low to high addresses: argc, argv[], NULL, envp[], NULL, auxv pairs,
/// AT_NULL, then (at the top) the strings and the 16 AT_RANDOM bytes.
fn build_stack(
    mem: &mut DirectMem,
    elf: &Elf,
    exe: &str,
    args: &[String],
    envs: &[String],
) -> Result<u64> {
    let mut top = STACK_TOP;
    let mut push_bytes = |mem: &mut DirectMem, bytes: &[u8]| -> Result<u64> {
        top -= bytes.len() as u64;
        mem.write_bytes(GuestVirt(top), bytes).map_err(|f| {
            anyhow::anyhow!("stack overflow while building the initial stack: {f:?}")
        })?;
        Ok(top)
    };
    let mut cstr = |mem: &mut DirectMem, s: &str| {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        push_bytes(mem, &v)
    };
    let execfn = cstr(mem, exe)?;
    let argv: Vec<u64> = args.iter().map(|a| cstr(mem, a)).collect::<Result<_>>()?;
    let envp: Vec<u64> = envs.iter().map(|e| cstr(mem, e)).collect::<Result<_>>()?;
    let mut random = [0u8; 16];
    getrandom_host(&mut random);
    let random_ptr = push_bytes(mem, &random)?;

    // SAFETY: plain libc getters without arguments.
    let (uid, euid, gid, egid) = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    let auxv: [(u64, u64); 16] = [
        (AT_PHDR, elf.phdr_vaddr().unwrap_or(0)),
        (AT_PHENT, 56),
        (AT_PHNUM, elf.phnum as u64),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_BASE, 0),
        (AT_FLAGS, 0),
        (AT_ENTRY, elf.entry),
        (AT_UID, uid as u64),
        (AT_EUID, euid as u64),
        (AT_GID, gid as u64),
        (AT_EGID, egid as u64),
        (AT_HWCAP, HWCAP),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_RANDOM, random_ptr),
        (AT_EXECFN, execfn),
    ];
    let words = 1 + argv.len() + 1 + envp.len() + 1 + 2 * (auxv.len() + 1);
    let sp = (top - 8 * words as u64) & !15;
    let mut w = Vec::with_capacity(words);
    w.push(argv.len() as u64);
    w.extend(&argv);
    w.push(0);
    w.extend(&envp);
    w.push(0);
    for (k, v) in auxv {
        w.push(k);
        w.push(v);
    }
    w.push(AT_NULL);
    w.push(0);
    let bytes: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
    mem.write_bytes(GuestVirt(sp), &bytes)
        .map_err(|f| anyhow::anyhow!("writing initial stack: {f:?}"))?;
    Ok(sp)
}

fn getrandom_host(buf: &mut [u8]) {
    // SAFETY: buf is a valid writable buffer of buf.len() bytes.
    let n = unsafe { libc::getrandom(buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
    if n != buf.len() as isize {
        // Fall back to something non-constant; AT_RANDOM only seeds stack protectors.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        buf.copy_from_slice(&t.to_le_bytes()[..buf.len()]);
    }
}
