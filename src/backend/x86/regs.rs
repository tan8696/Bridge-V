//! Host register enum, allocatable pool, scratch registers and the pinned guest map
//! (CLAUDE.md §8.2).

/// An x86-64 general-purpose register, numbered as in the ModRM/SIB/REX encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Reg {
    pub const ALL: [Reg; 16] = [
        Reg::Rax,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rbx,
        Reg::Rsp,
        Reg::Rbp,
        Reg::Rsi,
        Reg::Rdi,
        Reg::R8,
        Reg::R9,
        Reg::R10,
        Reg::R11,
        Reg::R12,
        Reg::R13,
        Reg::R14,
        Reg::R15,
    ];

    /// Encoding number 0..16.
    #[inline]
    pub const fn num(self) -> u8 {
        self as u8
    }

    /// Low three bits, the part that goes into ModRM.reg/rm, SIB.base/index or the opcode.
    #[inline]
    pub const fn low3(self) -> u8 {
        self as u8 & 7
    }

    /// Bit 3, the part that goes into REX.R/X/B.
    #[inline]
    pub const fn rex_bit(self) -> u8 {
        (self as u8 >> 3) & 1
    }

    /// As an 8-bit operand, SPL/BPL/SIL/DIL exist only with a REX prefix (without one,
    /// encodings 4..7 mean AH/CH/DH/BH). CLAUDE.md §11.1 gotcha 4.
    #[inline]
    pub const fn byte_needs_rex(self) -> bool {
        matches!(self, Reg::Rsp | Reg::Rbp | Reg::Rsi | Reg::Rdi)
    }
}

/// RBP holds `&CpuState + CPU_BIAS` in JIT code, so `x[0..32]` sit in disp8 range (§8.1).
pub const CPU: Reg = Reg::Rbp;
pub const CPU_BIAS: i32 = 128;
/// Direct mode: RBX holds the host address of guest address 0.
pub const MEM_BASE: Reg = Reg::Rbx;
/// Emitter scratch registers, never allocated (§8.2).
pub const SCRATCH0: Reg = Reg::R10;
pub const SCRATCH1: Reg = Reg::R11;

/// The IR back end keeps the instruction budget (D12, D30) in this register instead of
/// `CpuState.budget` (D46): `enter_jit` loads it, `exit_jit` stores it back, helper calls sync
/// it. It is therefore not allocatable at the `pinned`/`linear` levels.
pub const BUDGET_REG: Reg = Reg::R9;

/// Caller-saved registers available to the allocator (Phase 4).
pub const POOL: [Reg; 7] = [
    Reg::Rax,
    Reg::Rcx,
    Reg::Rdx,
    Reg::Rsi,
    Reg::Rdi,
    Reg::R8,
    Reg::R9,
];

/// Callee-saved registers, saved and restored by `enter_jit`/`exit_jit`.
pub const CALLEE_SAVED: [Reg; 6] = [Reg::Rbp, Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15];

/// Default pinned guest registers (§8.2): (guest register, host register). Used from Phase 4.
pub const PINNED: [(u8, Reg); 4] = [(2, Reg::R12), (1, Reg::R13), (10, Reg::R14), (15, Reg::R15)];
