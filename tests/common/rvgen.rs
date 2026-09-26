//! Random RV64 straight-line blocks for property tests and the block fuzzer (P4.2, P4.3, P4.8).
//!
//! A generated block is 1–40 body instructions followed by one terminator. Body instructions
//! cover every integer ALU form (OP, OP-32, OP-IMM, OP-IMM-32, LUI, AUIPC), loads and stores
//! through x5 (which points into a scratch page), and a few helper-executed instructions (CSR
//! read of `instret`, AMOs through x5, FP moves) that force full register syncs. Terminators
//! are ECALL, JAL, JALR, every branch, or a load/store through x6 (an unmapped address) that
//! faults. x5 and x6 are never written.
//!
//! `Harness` runs a block from identical initial state under any executor and captures the
//! resulting architectural state and scratch memory for comparison.

// Shared by several test crates (`#[path]` include), each using a subset of the generators.
#![allow(dead_code)]

use bridgev::cpu::state::CpuState;
use bridgev::interp::{BlockExit, build_block_max};
use bridgev::isa::Decoded;
use bridgev::mem::direct::DirectMem;
use bridgev::mem::{GuestVirt, prot};
use proptest::prelude::*;

pub const CODE: u64 = 0x10000;
pub const SCRATCH: u64 = 0x20000;
pub const BASE: u64 = SCRATCH + 2048;
pub const UNMAPPED: u64 = 0x7000_0000;

fn r(f7: u32, rs2: u32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    f7 << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | rd << 7 | opc
}
fn i(imm: i32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    ((imm as u32) & 0xFFF) << 20 | rs1 << 15 | f3 << 12 | rd << 7 | opc
}
fn s(imm: i32, rs2: u32, rs1: u32, f3: u32) -> u32 {
    let imm = imm as u32;
    ((imm >> 5) & 0x7F) << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | (imm & 0x1F) << 7 | 0x23
}
fn b(off: i32, rs2: u32, rs1: u32, f3: u32) -> u32 {
    let o = off as u32;
    ((o >> 12) & 1) << 31
        | ((o >> 5) & 0x3F) << 25
        | rs2 << 20
        | rs1 << 15
        | f3 << 12
        | ((o >> 1) & 0xF) << 8
        | ((o >> 11) & 1) << 7
        | 0x63
}
fn j(off: i32, rd: u32) -> u32 {
    let o = off as u32;
    ((o >> 20) & 1) << 31
        | ((o >> 1) & 0x3FF) << 21
        | ((o >> 11) & 1) << 20
        | ((o >> 12) & 0xFF) << 12
        | rd << 7
        | 0x6F
}

/// Destination registers: anything but x5/x6 (x0 allowed: writes are discarded).
fn rd() -> impl Strategy<Value = u32> {
    (0u32..30).prop_map(|x| if x >= 5 { x + 2 } else { x })
}
fn rs() -> impl Strategy<Value = u32> {
    0u32..32
}
fn imm12() -> impl Strategy<Value = i32> {
    prop_oneof![
        -2048i32..2048,
        Just(0),
        Just(-1),
        Just(1),
        Just(2047),
        Just(-2048)
    ]
}

const OP: [(u32, u32); 18] = [
    (0, 0),
    (0x20, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (0, 4),
    (0, 5),
    (0x20, 5),
    (0, 6),
    (0, 7),
    (1, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (1, 4),
    (1, 5),
    (1, 6),
    (1, 7),
];
const OP32: [(u32, u32); 10] = [
    (0, 0),
    (0x20, 0),
    (0, 1),
    (0, 5),
    (0x20, 5),
    (1, 0),
    (1, 4),
    (1, 5),
    (1, 6),
    (1, 7),
];

/// One body instruction.
pub fn body_insn() -> impl Strategy<Value = u32> {
    prop_oneof![
        4 => (0..OP.len(), rs(), rs(), rd()).prop_map(|(k, a, bb, d)| r(OP[k].0, bb, a, OP[k].1, d, 0x33)),
        2 => (0..OP32.len(), rs(), rs(), rd()).prop_map(|(k, a, bb, d)| r(OP32[k].0, bb, a, OP32[k].1, d, 0x3B)),
        4 => (prop::sample::select(vec![0u32, 2, 3, 4, 6, 7]), imm12(), rs(), rd())
            .prop_map(|(f3, imm, a, d)| i(imm, a, f3, d, 0x13)),
        2 => (0u32..64, prop::sample::select(vec![(1u32, 0u32), (5, 0), (5, 0x400)]), rs(), rd())
            .prop_map(|(sh, (f3, hi), a, d)| i((hi | sh) as i32, a, f3, d, 0x13)),
        1 => (imm12(), rs(), rd()).prop_map(|(imm, a, d)| i(imm, a, 0, d, 0x1B)),
        1 => (0u32..32, prop::sample::select(vec![(1u32, 0u32), (5, 0), (5, 0x400)]), rs(), rd())
            .prop_map(|(sh, (f3, hi), a, d)| i((hi | sh) as i32, a, f3, d, 0x1B)),
        1 => (any::<u32>(), rd(), prop::bool::ANY)
            .prop_map(|(imm, d, auipc)| (imm & 0xFFFF_F000) | d << 7 | if auipc { 0x17 } else { 0x37 }),
        3 => (prop::sample::select(vec![0u32, 1, 2, 3, 4, 5, 6]), -2048i32..2040, rd())
            .prop_map(|(f3, off, d)| i(off, 5, f3, d, 0x03)),
        3 => (0u32..4, -2048i32..2040, rs()).prop_map(|(f3, off, v)| s(off, v, 5, f3)),
        1 => rd().prop_map(|d| i(0xC02, 0, 2, d, 0x73)), // csrrs d, instret, x0
        1 => (rs(), rd(), prop::bool::ANY).prop_map(|(v, d, w)| {
            // amoadd.{w,d} d, v, (x5)
            r(0, v, 5, if w { 2 } else { 3 }, d, 0x2F)
        }),
        1 => (rs(), 0u32..32).prop_map(|(a, f)| r(0x79, 0, a, 0, f, 0x53)), // fmv.d.x f, a
        1 => (0u32..32, rd()).prop_map(|(f, d)| r(0x71, 0, f, 0, d, 0x53)), // fmv.x.d d, f
    ]
}

/// One terminator.
pub fn terminator() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(0x0000_0073u32), // ecall
        (rd(), -64i32..64).prop_map(|(d, o)| j(o * 2, d)),
        (rd(), rs(), imm12()).prop_map(|(d, a, imm)| i(imm, a, 0, d, 0x67)),
        (
            prop::sample::select(vec![0u32, 1, 4, 5, 6, 7]),
            rs(),
            rs(),
            -64i32..64
        )
            .prop_map(|(f3, a, bb, o)| b(o * 2, bb, a, f3)),
        (rd(), 0u32..7).prop_map(|(d, f3)| i(8, 6, f3, d, 0x03)), // faulting load
        (rs(), 0u32..4).prop_map(|(v, f3)| s(-8, v, 6, f3)),      // faulting store
    ]
}

/// A whole block: body + terminator. One block in four loops back to its own start (a branch
/// or JAL to CODE), the shape of hot inner loops.
pub fn block() -> impl Strategy<Value = Vec<u32>> {
    let plain = (prop::collection::vec(body_insn(), 1..40), terminator()).prop_map(|(mut v, t)| {
        v.push(t);
        v
    });
    let self_loop = (
        prop::collection::vec(body_insn(), 1..40),
        prop::sample::select(vec![0u32, 1, 4, 5, 6, 7]),
        rs(),
        rs(),
        rd(),
        prop::bool::weighted(0.8),
    )
        .prop_map(|(mut v, f3, a, bb, d, branch)| {
            let back = -4 * v.len() as i32;
            v.push(if branch {
                b(back, bb, a, f3)
            } else {
                j(back, d)
            });
            v
        });
    prop_oneof![3 => plain, 1 => self_loop]
}

// ------------------------------------------------------------------------------ FP (P6.5) ----

fn fr() -> impl Strategy<Value = u32> {
    0u32..32
}

/// Rounding-mode field: mostly RNE and dynamic (the inline paths), sometimes the others and the
/// reserved values 5 and 6.
fn rm() -> impl Strategy<Value = u32> {
    prop_oneof![4 => Just(0u32), 4 => Just(7u32), 2 => 1u32..5, 1 => 5u32..7]
}

fn op_fp(f5: u32, fmt: u32, rs2: u32, rs1: u32, rm: u32, rd: u32) -> u32 {
    r(f5 << 2 | fmt, rs2, rs1, rm, rd, 0x53)
}

/// One F/D instruction (x5 is the memory base, x-register results avoid x5/x6).
pub fn fp_insn() -> impl Strategy<Value = u32> {
    let fmt = || 0u32..2;
    prop_oneof![
        // fadd/fsub/fmul/fdiv
        6 => (0u32..4, fmt(), fr(), fr(), rm(), fr()).prop_map(|(f5, t, b, a, m, d)| op_fp(f5, t, b, a, m, d)),
        // fsqrt
        1 => (fmt(), fr(), rm(), fr()).prop_map(|(t, a, m, d)| op_fp(0x0B, t, 0, a, m, d)),
        // fmadd/fmsub/fnmsub/fnmadd
        4 => (prop::sample::select(vec![0x43u32, 0x47, 0x4B, 0x4F]), fmt(), fr(), fr(), fr(), rm(), fr())
            .prop_map(|(opc, t, c, b, a, m, d)| c << 27 | t << 25 | b << 20 | a << 15 | m << 12 | d << 7 | opc),
        // fsgnj/fsgnjn/fsgnjx
        2 => (fmt(), fr(), fr(), 0u32..3, fr()).prop_map(|(t, b, a, m, d)| op_fp(0x04, t, b, a, m, d)),
        // fmin/fmax
        1 => (fmt(), fr(), fr(), 0u32..2, fr()).prop_map(|(t, b, a, m, d)| op_fp(0x05, t, b, a, m, d)),
        // fcvt.s.d / fcvt.d.s
        2 => (fmt(), fr(), rm(), fr()).prop_map(|(t, a, m, d)| op_fp(0x08, t, 1 - t, a, m, d)),
        // feq/flt/fle → x
        3 => (fmt(), fr(), fr(), 0u32..3, rd()).prop_map(|(t, b, a, m, d)| op_fp(0x14, t, b, a, m, d)),
        // fcvt.{w,wu,l,lu}.fmt → x
        3 => (fmt(), 0u32..4, fr(), rm(), rd()).prop_map(|(t, k, a, m, d)| op_fp(0x18, t, k, a, m, d)),
        // fcvt.fmt.{w,wu,l,lu} ← x
        3 => (fmt(), 0u32..4, rs(), rm(), fr()).prop_map(|(t, k, a, m, d)| op_fp(0x1A, t, k, a, m, d)),
        // fmv.x.{w,d} / fclass → x
        2 => (fmt(), fr(), 0u32..2, rd()).prop_map(|(t, a, m, d)| op_fp(0x1C, t, 0, a, m, d)),
        // fmv.{w,d}.x ← x
        2 => (fmt(), rs(), fr()).prop_map(|(t, a, d)| op_fp(0x1E, t, 0, a, 0, d)),
        // flw/fld, fsw/fsd through x5
        2 => (prop::bool::ANY, -2048i32..2040, fr()).prop_map(|(dbl, off, f)| i(off, 5, if dbl { 3 } else { 2 }, f, 0x07)),
        2 => (prop::bool::ANY, -2048i32..2040, fr()).prop_map(|(dbl, off, f)| {
            let imm = off as u32;
            ((imm >> 5) & 0x7F) << 25 | f << 20 | 5 << 15 | (if dbl { 3 } else { 2 }) << 12 | (imm & 0x1F) << 7 | 0x27
        }),
    ]
}

/// An FP-heavy block: FP and integer body instructions, then a terminator.
pub fn fp_block() -> impl Strategy<Value = Vec<u32>> {
    let body = prop_oneof![4 => fp_insn(), 1 => body_insn()];
    (prop::collection::vec(body, 1..24), terminator()).prop_map(|(mut v, t)| {
        v.push(t);
        v
    })
}

const D_SPECIAL: [u64; 26] = [
    0,
    0x8000_0000_0000_0000,
    0x3ff0_0000_0000_0000, // 1.0
    0xbff0_0000_0000_0000,
    0x4008_0000_0000_0000, // 3.0
    0x3fb9_9999_9999_999a, // 0.1
    0x7ff0_0000_0000_0000, // inf
    0xfff0_0000_0000_0000,
    0x7ff8_0000_0000_0000, // canonical qNaN
    0xfff8_0000_0000_0000, // x86 default NaN
    0x7ff8_0000_0000_1234, // qNaN with payload
    0x7ff0_0000_0000_0001, // sNaN
    0xfff4_0000_0000_0000, // negative sNaN
    0x0000_0000_0000_0001, // min subnormal
    0x000f_ffff_ffff_ffff, // max subnormal
    0x0010_0000_0000_0000, // min normal
    0x7fef_ffff_ffff_ffff, // max normal
    0x41e0_0000_0000_0000, // 2^31
    0xc1e0_0000_0000_0000, // -2^31
    0x41df_ffff_ffe0_0000, // 2^31 - 0.5
    0xc1e0_0000_0010_0000, // -2^31 - 0.5
    0x43e0_0000_0000_0000, // 2^63
    0xc3e0_0000_0000_0000, // -2^63
    0x41f0_0000_0000_0000, // 2^32
    0x7e37_e43c_8800_759c, // 1e300
    0x01a5_6e1f_c2f8_f359, // 1e-300
];

const S_SPECIAL: [u32; 21] = [
    0,
    0x8000_0000,
    0x3f80_0000, // 1.0
    0xbf80_0000,
    0x4040_0000, // 3.0
    0x3dcc_cccd, // 0.1
    0x7f80_0000, // inf
    0xff80_0000,
    0x7fc0_0000, // canonical qNaN
    0xffc0_0000,
    0x7fc0_1234,
    0x7f80_0001, // sNaN
    0x0000_0001, // min subnormal
    0x007f_ffff, // max subnormal
    0x0080_0000, // min normal
    0x7f7f_ffff, // max normal
    0x4f00_0000, // 2^31
    0xcf00_0000, // -2^31
    0x5f00_0000, // 2^63
    0xdf00_0000, // -2^63
    0x4f80_0000, // 2^32
];

fn fval() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => prop::sample::select(D_SPECIAL.to_vec()),
        3 => prop::sample::select(S_SPECIAL.to_vec()).prop_map(|v| 0xffff_ffff_0000_0000 | v as u64),
        1 => any::<u32>().prop_map(|v| 0xffff_ffff_0000_0000 | v as u64),
        1 => any::<u64>(),
    ]
}

/// Initial FP state: f registers, frm, fflags and mstatus.FS (mostly Dirty with frm = RNE,
/// the fast-variant precondition; D47).
#[derive(Clone, Debug)]
pub struct FpState {
    pub f: [u64; 32],
    pub frm: u8,
    pub fflags: u8,
    /// mstatus.FS: 0 Off, 1 Initial, 2 Clean, 3 Dirty.
    pub fs: u64,
}

pub fn fp_state() -> impl Strategy<Value = FpState> {
    (
        prop::collection::vec(fval(), 32),
        prop_oneof![6 => Just(0u8), 2 => 1u8..5, 1 => 5u8..8],
        0u8..32,
        prop_oneof![8 => Just(3u64), 1 => Just(1u64), 1 => Just(2u64), 1 => Just(0u64)],
    )
        .prop_map(|(f, frm, fflags, fs)| {
            let mut a = [0u64; 32];
            a.copy_from_slice(&f);
            FpState {
                f: a,
                frm,
                fflags,
                fs,
            }
        })
}

impl FpState {
    pub fn apply(&self, cpu: &mut CpuState) {
        cpu.f = self.f;
        cpu.frm = self.frm;
        cpu.fflags = self.fflags;
        let fs = bridgev::cpu::csr::mstatus::FS;
        cpu.csr.mstatus = (cpu.csr.mstatus & !fs) | self.fs << bridgev::cpu::csr::mstatus::FS_SHIFT;
    }
}

/// Initial registers (x5, x6 fixed; a mix of edge values and random bits).
pub fn regs() -> impl Strategy<Value = [u64; 32]> {
    let val = prop_oneof![
        any::<u64>(),
        Just(0u64),
        Just(1),
        Just(u64::MAX),
        Just(0x8000_0000_0000_0000),
        Just(0x7FFF_FFFF_FFFF_FFFF),
        Just(0xFFFF_FFFF),
        Just(0x8000_0000),
        (0u64..64),
    ];
    prop::collection::vec(val, 32).prop_map(|v| {
        let mut a = [0u64; 32];
        a.copy_from_slice(&v);
        a[0] = 0;
        a[5] = BASE;
        a[6] = UNMAPPED;
        a
    })
}

/// Scratch-page contents, printed as a short checksum (the full 4 KiB dump is unreadable).
#[derive(PartialEq, Eq)]
pub struct Scratch(pub Vec<u8>);

impl std::fmt::Debug for Scratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let h = self.0.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
        });
        write!(f, "scratch#{h:016x}")
    }
}

/// Everything compared after a run.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub exit: BlockExit,
    pub x: [u64; 32],
    pub f: [u64; 32],
    pub pc: u64,
    pub icount: u64,
    pub res: (u64, u64, u64),
    /// fflags, frm and mstatus (FS).
    pub fcsr: (u8, u8, u64),
    pub scratch: Scratch,
}

/// Guest memory with the code and scratch pages, reset before every run.
pub struct Harness {
    pub mem: DirectMem,
    scratch_init: Vec<u8>,
    pub insns: Vec<Decoded>,
    pub fetch_fault: Option<bridgev::cpu::trap::Exception>,
}

impl Harness {
    pub fn new(code: &[u32]) -> Harness {
        let mut mem = DirectMem::new().unwrap();
        mem.map(GuestVirt(CODE), 0x1000, prot::R | prot::X).unwrap();
        mem.map(GuestVirt(SCRATCH), 0x1000, prot::RW).unwrap();
        let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
        mem.write_bytes(GuestVirt(CODE), &bytes).unwrap();
        let scratch_init: Vec<u8> = (0..4096u32)
            .map(|k| (k.wrapping_mul(0x9E37_79B1) >> 13) as u8)
            .collect();
        let blk = build_block_max(CODE, &mem, 128);
        Harness {
            mem,
            scratch_init,
            insns: blk.insns,
            fetch_fault: blk.fetch_fault,
        }
    }

    /// Fresh CPU with `regs`, memory reset; run `f`; capture the outcome.
    pub fn run(
        &mut self,
        regs: &[u64; 32],
        f: impl FnOnce(
            &mut CpuState,
            &mut DirectMem,
            &[Decoded],
            Option<bridgev::cpu::trap::Exception>,
        ) -> BlockExit,
    ) -> Outcome {
        self.run_init(regs, |_| {}, f)
    }

    /// `run` with extra initial state set by `init` (FP registers, fcsr, mstatus).
    pub fn run_init(
        &mut self,
        regs: &[u64; 32],
        init: impl FnOnce(&mut CpuState),
        f: impl FnOnce(
            &mut CpuState,
            &mut DirectMem,
            &[Decoded],
            Option<bridgev::cpu::trap::Exception>,
        ) -> BlockExit,
    ) -> Outcome {
        self.mem
            .write_bytes(GuestVirt(SCRATCH), &self.scratch_init)
            .unwrap();
        let mut cpu = CpuState::new_user(CODE);
        cpu.x = *regs;
        cpu.csr.deterministic_time = true;
        init(&mut cpu);
        let exit = f(&mut cpu, &mut self.mem, &self.insns, self.fetch_fault);
        Outcome {
            exit,
            x: cpu.x,
            f: cpu.f,
            pc: cpu.pc,
            icount: cpu.icount,
            res: (cpu.res_addr, cpu.res_val, cpu.res_valid),
            fcsr: (cpu.fflags, cpu.frm, cpu.csr.mstatus),
            scratch: Scratch(
                self.mem
                    .slice(GuestVirt(SCRATCH), 4096, prot::R)
                    .unwrap()
                    .to_vec(),
            ),
        }
    }
}
