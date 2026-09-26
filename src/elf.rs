//! ELF64 loader for RISC-V guest executables (CLAUDE.md §19): header validation, program
//! headers, entry point, `e_flags`, and `.symtab` lookup (`tohost`/`fromhost` for riscv-tests).
//!
//! Parsing never panics: every read is bounds-checked and malformed input returns an error.

use anyhow::{Context, Result, bail, ensure};

pub const EM_RISCV: u16 = 243;
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const PT_LOAD: u32 = 1;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;
/// `e_flags`: the binary may contain compressed instructions.
pub const EF_RISCV_RVC: u32 = 0x1;
/// `e_flags` float-ABI field (0 soft, 2 single, 4 double, 6 quad).
pub const EF_RISCV_FLOAT_ABI: u32 = 0x6;

const SHT_SYMTAB: u32 = 2;
const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const SHDR_SIZE: usize = 64;
const SYM_SIZE: usize = 24;

/// One program header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub p_type: u32,
    /// `PF_R | PF_W | PF_X`
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub paddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

/// A parsed ELF64 RISC-V executable. Borrows the file contents.
#[derive(Debug)]
pub struct Elf<'a> {
    data: &'a [u8],
    pub e_type: u16,
    pub entry: u64,
    pub flags: u32,
    pub phoff: u64,
    pub phnum: u16,
    pub segments: Vec<Segment>,
    /// Program interpreter path (dynamically linked executables).
    pub interp: Option<String>,
    /// `(name, value)` of every symbol in `.symtab`, if present.
    symbols: Vec<(String, u64)>,
}

fn slice(d: &[u8], off: u64, len: u64) -> Result<&[u8]> {
    let end = off.checked_add(len).context("offset overflow")?;
    d.get(off as usize..end as usize).with_context(|| {
        format!(
            "range {off:#x}+{len:#x} is outside the file ({:#x} bytes)",
            d.len()
        )
    })
}

fn u16_at(d: &[u8], off: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(slice(d, off as u64, 2)?.try_into()?))
}
fn u32_at(d: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(slice(d, off as u64, 4)?.try_into()?))
}
fn u64_at(d: &[u8], off: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(slice(d, off as u64, 8)?.try_into()?))
}

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Elf<'a>> {
        ensure!(data.len() >= EHDR_SIZE, "file too small for an ELF header");
        ensure!(data[0..4] == *b"\x7fELF", "not an ELF file (bad magic)");
        ensure!(data[4] == 2, "not a 64-bit ELF (EI_CLASS = {})", data[4]);
        ensure!(
            data[5] == 1,
            "not a little-endian ELF (EI_DATA = {})",
            data[5]
        );
        ensure!(data[6] == 1, "unsupported ELF version {}", data[6]);
        let e_type = u16_at(data, 16)?;
        let machine = u16_at(data, 18)?;
        ensure!(
            machine == EM_RISCV,
            "not a RISC-V ELF (e_machine = {machine})"
        );
        ensure!(
            e_type == ET_EXEC || e_type == ET_DYN,
            "not an executable (e_type = {e_type})"
        );
        let entry = u64_at(data, 24)?;
        let phoff = u64_at(data, 32)?;
        let shoff = u64_at(data, 40)?;
        let flags = u32_at(data, 48)?;
        let phentsize = u16_at(data, 54)? as usize;
        let phnum = u16_at(data, 56)?;
        let shentsize = u16_at(data, 58)? as usize;
        let shnum = u16_at(data, 60)?;
        ensure!(
            phnum == 0 || phentsize == PHDR_SIZE,
            "bad e_phentsize {phentsize}"
        );

        let mut segments = Vec::with_capacity(phnum as usize);
        let mut interp = None;
        for i in 0..phnum as u64 {
            let off = phoff
                .checked_add(i * PHDR_SIZE as u64)
                .context("phoff overflow")?;
            let ph = slice(data, off, PHDR_SIZE as u64).context("program header table")?;
            let seg = Segment {
                p_type: u32_at(ph, 0)?,
                flags: u32_at(ph, 4)?,
                offset: u64_at(ph, 8)?,
                vaddr: u64_at(ph, 16)?,
                paddr: u64_at(ph, 24)?,
                filesz: u64_at(ph, 32)?,
                memsz: u64_at(ph, 40)?,
                align: u64_at(ph, 48)?,
            };
            if seg.p_type == PT_LOAD {
                ensure!(seg.filesz <= seg.memsz, "segment {i}: p_filesz > p_memsz");
                slice(data, seg.offset, seg.filesz)
                    .with_context(|| format!("segment {i} file contents"))?;
                seg.vaddr
                    .checked_add(seg.memsz)
                    .with_context(|| format!("segment {i}: address overflow"))?;
            } else if seg.p_type == PT_INTERP {
                let raw = slice(data, seg.offset, seg.filesz).context("PT_INTERP")?;
                let s = raw.split(|&b| b == 0).next().unwrap_or_default();
                interp = Some(String::from_utf8_lossy(s).into_owned());
            }
            segments.push(seg);
        }
        let loads: Vec<&Segment> = segments.iter().filter(|s| s.p_type == PT_LOAD).collect();
        for (i, a) in loads.iter().enumerate() {
            for b in &loads[i + 1..] {
                if a.memsz > 0
                    && b.memsz > 0
                    && a.vaddr < b.vaddr + b.memsz
                    && b.vaddr < a.vaddr + a.memsz
                {
                    bail!(
                        "overlapping PT_LOAD segments at {:#x} and {:#x}",
                        a.vaddr,
                        b.vaddr
                    );
                }
            }
        }

        let symbols = if shnum > 0 && shentsize == SHDR_SIZE {
            parse_symbols(data, shoff, shnum).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Elf {
            data,
            e_type,
            entry,
            flags,
            phoff,
            phnum,
            segments,
            interp,
            symbols,
        })
    }

    /// `PT_LOAD` segments, in file order.
    pub fn loads(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().filter(|s| s.p_type == PT_LOAD)
    }

    /// File bytes of a segment (`p_filesz` bytes; validated by `parse`).
    pub fn segment_data(&self, seg: &Segment) -> &'a [u8] {
        &self.data[seg.offset as usize..(seg.offset + seg.filesz) as usize]
    }

    /// Value of the first `.symtab` symbol with this name.
    pub fn symbol(&self, name: &str) -> Option<u64> {
        self.symbols
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, v)| v)
    }

    /// Does `e_flags` declare compressed instructions?
    pub fn has_rvc(&self) -> bool {
        self.flags & EF_RISCV_RVC != 0
    }

    /// Virtual address of the program headers in the loaded image (for `AT_PHDR`), if they
    /// are covered by a `PT_PHDR` or a loaded segment.
    pub fn phdr_vaddr(&self) -> Option<u64> {
        if let Some(p) = self.segments.iter().find(|s| s.p_type == PT_PHDR) {
            return Some(p.vaddr);
        }
        self.loads()
            .find(|s| s.offset <= self.phoff && self.phoff < s.offset + s.filesz)
            .map(|s| s.vaddr + (self.phoff - s.offset))
    }
}

fn parse_symbols(data: &[u8], shoff: u64, shnum: u16) -> Result<Vec<(String, u64)>> {
    let shdr = |i: u64| slice(data, shoff + i * SHDR_SIZE as u64, SHDR_SIZE as u64);
    let mut out = Vec::new();
    for i in 0..shnum as u64 {
        let sh = shdr(i)?;
        if u32_at(sh, 4)? != SHT_SYMTAB {
            continue;
        }
        let (off, size, link) = (u64_at(sh, 24)?, u64_at(sh, 32)?, u32_at(sh, 40)?);
        let strsh = shdr(link as u64)?;
        let strtab = slice(data, u64_at(strsh, 24)?, u64_at(strsh, 32)?)?;
        let syms = slice(data, off, size)?;
        for sym in syms.chunks_exact(SYM_SIZE) {
            let name_off = u32_at(sym, 0)? as usize;
            let value = u64_at(sym, 8)?;
            let Some(rest) = strtab.get(name_off..) else {
                continue;
            };
            let name = rest.split(|&b| b == 0).next().unwrap_or_default();
            if !name.is_empty() {
                out.push((String::from_utf8_lossy(name).into_owned(), value));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid ELF: header + one PT_LOAD program header covering 4 code bytes.
    pub(crate) fn tiny_elf() -> Vec<u8> {
        let mut d = vec![0u8; EHDR_SIZE + PHDR_SIZE + 4];
        d[0..4].copy_from_slice(b"\x7fELF");
        d[4] = 2;
        d[5] = 1;
        d[6] = 1;
        d[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        d[18..20].copy_from_slice(&EM_RISCV.to_le_bytes());
        d[24..32].copy_from_slice(&0x10078u64.to_le_bytes()); // entry
        d[32..40].copy_from_slice(&(EHDR_SIZE as u64).to_le_bytes()); // phoff
        d[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        d[56..58].copy_from_slice(&1u16.to_le_bytes());
        let ph = EHDR_SIZE;
        d[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        d[ph + 4..ph + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
        d[ph + 16..ph + 24].copy_from_slice(&0x10000u64.to_le_bytes()); // vaddr
        d[ph + 32..ph + 40].copy_from_slice(&((EHDR_SIZE + PHDR_SIZE + 4) as u64).to_le_bytes());
        d[ph + 40..ph + 48].copy_from_slice(&0x1000u64.to_le_bytes()); // memsz
        d
    }

    #[test]
    fn parses_minimal_elf() {
        let d = tiny_elf();
        let e = Elf::parse(&d).unwrap();
        assert_eq!(e.entry, 0x10078);
        assert_eq!(e.loads().count(), 1);
        assert_eq!(e.phdr_vaddr(), Some(0x10000 + EHDR_SIZE as u64));
        assert_eq!(e.symbol("tohost"), None);
    }

    #[test]
    fn rejects_malformed() {
        let good = tiny_elf();
        type Mutation = Box<dyn Fn(&mut Vec<u8>)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("truncated", Box::new(|d| d.truncate(40))),
            ("bad magic", Box::new(|d| d[1] = b'X')),
            ("32-bit", Box::new(|d| d[4] = 1)),
            ("big endian", Box::new(|d| d[5] = 2)),
            ("wrong machine", Box::new(|d| d[18] = 62)),
            ("relocatable", Box::new(|d| d[16] = 1)),
            (
                "filesz > memsz",
                Box::new(|d| d[EHDR_SIZE + 40..EHDR_SIZE + 48].fill(0)),
            ),
            ("phdr outside file", Box::new(|d| d[32] = 0xf0)),
            ("segment past EOF", Box::new(|d| d[EHDR_SIZE + 8] = 0x80)),
        ];
        for (name, mutate) in cases {
            let mut d = good.clone();
            mutate(&mut d);
            assert!(Elf::parse(&d).is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn rejects_overlapping_segments() {
        let mut d = tiny_elf();
        d[56..58].copy_from_slice(&2u16.to_le_bytes());
        let ph1: Vec<u8> = d[EHDR_SIZE..EHDR_SIZE + PHDR_SIZE].to_vec();
        d.splice(EHDR_SIZE + PHDR_SIZE..EHDR_SIZE + PHDR_SIZE, ph1);
        assert!(Elf::parse(&d).is_err());
    }
}
