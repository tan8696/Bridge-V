//! Hand-written x86-64 machine-code emitter: REX/ModRM/SIB encoding, immediates, labels and
//! rel32 fixups (CLAUDE.md §11, D3).
//!
//! `Asm` appends instructions to a byte buffer that will be placed at the host address
//! `origin` (the RX view of the code buffer, §12), so absolute branch and RIP-relative targets
//! can be encoded directly. Every encoding rule and gotcha of §11.1 is handled here and nowhere
//! else; `tests/emitter_golden.rs` checks the output against `iced-x86`.

use super::regs::Reg;

/// Operand size of an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    B8,
    B16,
    B32,
    B64,
}

impl Size {
    pub const fn bytes(self) -> u32 {
        match self {
            Size::B8 => 1,
            Size::B16 => 2,
            Size::B32 => 4,
            Size::B64 => 8,
        }
    }
}

/// SIB scale factor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    S1 = 0,
    S2 = 1,
    S4 = 2,
    S8 = 3,
}

impl Scale {
    pub const ALL: [Scale; 4] = [Scale::S1, Scale::S2, Scale::S4, Scale::S8];
    pub const fn factor(self) -> u32 {
        1 << self as u32
    }
}

/// A memory operand `[base + index*scale + disp]`. With neither base nor index it is the
/// absolute address `[disp32]` (sign-extended), encoded through a SIB byte because
/// `mod=00 rm=101` means RIP-relative in 64-bit mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mem {
    pub base: Option<Reg>,
    pub index: Option<(Reg, Scale)>,
    pub disp: i32,
}

impl Mem {
    /// `[base + disp]`
    pub const fn base(base: Reg, disp: i32) -> Mem {
        Mem {
            base: Some(base),
            index: None,
            disp,
        }
    }

    /// `[base + index*scale + disp]`
    pub const fn bi(base: Reg, index: Reg, scale: Scale, disp: i32) -> Mem {
        Mem {
            base: Some(base),
            index: Some((index, scale)),
            disp,
        }
    }

    /// `[disp32]`
    pub const fn abs(disp: i32) -> Mem {
        Mem {
            base: None,
            index: None,
            disp,
        }
    }
}

/// A register-or-memory (ModRM.rm) operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rm {
    Reg(Reg),
    Mem(Mem),
}

impl From<Reg> for Rm {
    fn from(r: Reg) -> Rm {
        Rm::Reg(r)
    }
}

impl From<Mem> for Rm {
    fn from(m: Mem) -> Rm {
        Rm::Mem(m)
    }
}

/// Condition code nibble of Jcc/SETcc/CMOVcc (§11.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    O = 0x0,
    No = 0x1,
    /// Below (unsigned <), CF=1.
    B = 0x2,
    /// Above or equal (unsigned >=).
    Ae = 0x3,
    E = 0x4,
    Ne = 0x5,
    Be = 0x6,
    A = 0x7,
    S = 0x8,
    Ns = 0x9,
    P = 0xA,
    Np = 0xB,
    /// Less (signed <).
    L = 0xC,
    Ge = 0xD,
    Le = 0xE,
    G = 0xF,
}

impl Cond {
    pub const ALL: [Cond; 16] = [
        Cond::O,
        Cond::No,
        Cond::B,
        Cond::Ae,
        Cond::E,
        Cond::Ne,
        Cond::Be,
        Cond::A,
        Cond::S,
        Cond::Ns,
        Cond::P,
        Cond::Np,
        Cond::L,
        Cond::Ge,
        Cond::Le,
        Cond::G,
    ];

    /// The opposite condition (flipping the low bit of the nibble).
    pub fn negate(self) -> Cond {
        Cond::ALL[(self as usize) ^ 1]
    }
}

/// Group-1 ALU operations; the value is the `/digit` and `opcode >> 3`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    Add = 0,
    Or = 1,
    Adc = 2,
    Sbb = 3,
    And = 4,
    Sub = 5,
    Xor = 6,
    Cmp = 7,
}

impl Alu {
    pub const ALL: [Alu; 8] = [
        Alu::Add,
        Alu::Or,
        Alu::Adc,
        Alu::Sbb,
        Alu::And,
        Alu::Sub,
        Alu::Xor,
        Alu::Cmp,
    ];
}

/// Group-2 shift/rotate operations (`/digit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Rol = 0,
    Ror = 1,
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

impl Shift {
    pub const ALL: [Shift; 5] = [Shift::Rol, Shift::Ror, Shift::Shl, Shift::Shr, Shift::Sar];
}

/// Group-3 unary operations (`F7 /digit`). `Mul`/`Imul`/`Div`/`Idiv` use RDX:RAX.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Not = 2,
    Neg = 3,
    Mul = 4,
    Imul = 5,
    Div = 6,
    Idiv = 7,
}

impl Unary {
    pub const ALL: [Unary; 6] = [
        Unary::Not,
        Unary::Neg,
        Unary::Mul,
        Unary::Imul,
        Unary::Div,
        Unary::Idiv,
    ];
}

/// BMI2 three-operand shifts (`VEX.LZ.pp.0F38.W F7 /r`): the count comes from a register,
/// flags are untouched, and no register is fixed (unlike `shl r/m, cl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftX {
    Shlx,
    Shrx,
    Sarx,
}

/// An SSE register (XMM0–XMM15). JIT code uses them only as scratch within one guest FP
/// instruction (Phase 6, D47); they are caller-saved in the SysV ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Xmm(pub u8);

/// An XMM register or a memory operand (the r/m side of an SSE instruction).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XRm {
    X(Xmm),
    M(Mem),
}

impl From<Xmm> for XRm {
    fn from(x: Xmm) -> XRm {
        XRm::X(x)
    }
}

impl From<Mem> for XRm {
    fn from(m: Mem) -> XRm {
        XRm::M(m)
    }
}

impl XRm {
    /// The generic ModRM operand (an XMM register is encoded like the GPR of the same number).
    fn rm(self) -> Rm {
        match self {
            XRm::X(x) => Rm::Reg(Reg::ALL[x.0 as usize]),
            XRm::M(m) => Rm::Mem(m),
        }
    }
}

/// Scalar SSE arithmetic (`F2 0F op` = double, `F3 0F op` = single).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseOp {
    Add = 0x58,
    Mul = 0x59,
    Sub = 0x5C,
    Div = 0x5E,
    Sqrt = 0x51,
}

/// FMA3 `231` forms: `dst = ±(a * b) ± dst` (VEX.66.0F38, W1 = double, W0 = single).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fma {
    /// `vfmadd231`: a*b + dst
    Madd = 0xB9,
    /// `vfmsub231`: a*b - dst
    Msub = 0xBB,
    /// `vfnmadd231`: -(a*b) + dst
    Nmadd = 0xBD,
    /// `vfnmsub231`: -(a*b) - dst
    Nmsub = 0xBF,
}

/// A branch target inside the buffer being assembled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label(u32);

#[derive(Clone, Copy)]
struct Fixup {
    /// Offset of the rel8/rel32 field.
    at: u32,
    label: Label,
    short: bool,
}

/// Encoding flags for `emit_op`.
#[derive(Clone, Copy, Default)]
struct Flags {
    /// REX.W (64-bit operand size).
    w: bool,
    /// 0x66 operand-size prefix (16-bit operand size).
    p66: bool,
    /// ModRM.reg names an 8-bit register (SPL..DIL need REX).
    reg_byte: bool,
    /// A register ModRM.rm operand is 8-bit.
    rm_byte: bool,
}

impl Flags {
    fn size(s: Size) -> Flags {
        Flags {
            w: s == Size::B64,
            p66: s == Size::B16,
            reg_byte: s == Size::B8,
            rm_byte: s == Size::B8,
        }
    }
}

/// Recommended multi-byte NOP forms, lengths 1..=9 (Intel SDM Vol. 2B, NOP).
const NOPS: [&[u8]; 9] = [
    &[0x90],
    &[0x66, 0x90],
    &[0x0F, 0x1F, 0x00],
    &[0x0F, 0x1F, 0x40, 0x00],
    &[0x0F, 0x1F, 0x44, 0x00, 0x00],
    &[0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00],
    &[0x0F, 0x1F, 0x80, 0x00, 0x00, 0x00, 0x00],
    &[0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
    &[0x66, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
];

fn fits_i8(v: i64) -> bool {
    v == v as i8 as i64
}

fn fits_i32(v: i64) -> bool {
    v == v as i32 as i64
}

/// Write `target - (field + 4)` into the rel32 field at `buf[at..at+4]`, where `field` is the
/// host address of that field. Used to link and unlink exits (§13.3).
pub fn write_rel32(buf: &mut [u8], at: usize, field_addr: u64, target: u64) {
    let rel = target.wrapping_sub(field_addr + 4) as i64;
    assert!(
        fits_i32(rel),
        "rel32 out of range: {field_addr:#x} -> {target:#x}"
    );
    buf[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
}

/// The x86-64 assembler.
pub struct Asm {
    buf: Vec<u8>,
    origin: u64,
    labels: Vec<Option<u32>>,
    fixups: Vec<Fixup>,
}

impl Asm {
    /// A new assembler whose first byte will live at host address `origin`.
    pub fn new(origin: u64) -> Asm {
        Asm {
            buf: Vec::with_capacity(256),
            origin,
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    /// Current offset from `origin`.
    #[inline]
    pub fn pos(&self) -> usize {
        self.buf.len()
    }

    /// Host address of the next byte.
    #[inline]
    pub fn here(&self) -> u64 {
        self.origin + self.buf.len() as u64
    }

    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// The bytes emitted so far (fixups to unbound labels are not resolved yet).
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Resolve all label fixups and return the machine code.
    pub fn finish(mut self) -> Vec<u8> {
        for f in std::mem::take(&mut self.fixups) {
            let target = self.labels[f.label.0 as usize].expect("branch to an unbound label");
            let at = f.at as usize;
            if f.short {
                let rel = target as i64 - (at as i64 + 1);
                assert!(fits_i8(rel), "rel8 branch out of range ({rel})");
                self.buf[at] = rel as i8 as u8;
            } else {
                let rel = target as i64 - (at as i64 + 4);
                self.buf[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        self.buf
    }

    #[inline]
    fn byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    #[inline]
    fn bytes_raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    fn imm16(&mut self, v: i16) {
        self.bytes_raw(&v.to_le_bytes());
    }

    fn imm32(&mut self, v: i32) {
        self.bytes_raw(&v.to_le_bytes());
    }

    // ---------------------------------------------------------------- core encoder ----

    /// Emit `[prefixes] [66] [REX] opcode ModRM [SIB] [disp]` with ModRM.reg = `reg`
    /// (a register number or a `/digit`) and ModRM.rm = `rm`. Immediates follow separately.
    fn emit_op(&mut self, prefixes: &[u8], f: Flags, opcode: &[u8], reg: u8, rm: Rm) {
        self.bytes_raw(prefixes);
        if f.p66 {
            self.byte(0x66);
        }
        let (x, b) = match rm {
            Rm::Reg(r) => (0, r.rex_bit()),
            Rm::Mem(m) => (
                m.index.map_or(0, |(i, _)| i.rex_bit()),
                m.base.map_or(0, |b| b.rex_bit()),
            ),
        };
        let r = (reg >> 3) & 1;
        let mut need_rex = f.w || r != 0 || x != 0 || b != 0;
        // §11.1 gotcha 4: byte registers 4..7 without REX are AH/CH/DH/BH.
        if f.reg_byte && (4..8).contains(&reg) {
            need_rex = true;
        }
        if let Rm::Reg(rr) = rm
            && f.rm_byte
            && rr.byte_needs_rex()
        {
            need_rex = true;
        }
        if need_rex {
            self.byte(0x40 | (f.w as u8) << 3 | r << 2 | x << 1 | b);
        }
        self.bytes_raw(opcode);
        self.modrm_rm(reg & 7, rm);
    }

    /// `emit_op` for `/digit` forms: ModRM.reg is an opcode extension, not a register.
    fn emit_digit(&mut self, prefixes: &[u8], f: Flags, opcode: &[u8], digit: u8, rm: Rm) {
        let f = Flags {
            reg_byte: false,
            ..f
        };
        self.emit_op(prefixes, f, opcode, digit, rm);
    }

    /// ModRM (+ SIB + displacement) for `rm`, with `reg3` in ModRM.reg.
    fn modrm_rm(&mut self, reg3: u8, rm: Rm) {
        let m = match rm {
            Rm::Reg(r) => {
                self.byte(0xC0 | reg3 << 3 | r.low3());
                return;
            }
            Rm::Mem(m) => m,
        };
        if let Some((i, _)) = m.index {
            // §11.1 gotcha 3: SIB.index = 100 means "no index", so RSP cannot be an index.
            assert!(i != Reg::Rsp, "RSP cannot be an index register");
        }
        let Some(base) = m.base else {
            // No base: mod=00 rm=100, SIB.base=101 → [index*scale + disp32] or [disp32].
            self.byte(reg3 << 3 | 0b100);
            let (idx, sc) = m.index.map_or((0b100, 0), |(i, s)| (i.low3(), s as u8));
            self.byte(sc << 6 | idx << 3 | 0b101);
            self.imm32(m.disp);
            return;
        };
        // §11.1 gotcha 2: mod=00 with base=101 (RBP/R13) means RIP/disp32-only, so those bases
        // always carry at least a disp8.
        let md = if m.disp == 0 && base.low3() != 0b101 {
            0b00
        } else if fits_i8(m.disp as i64) {
            0b01
        } else {
            0b10
        };
        // §11.1 gotcha 1: rm=100 (RSP/R12) means "SIB follows".
        if m.index.is_some() || base.low3() == 0b100 {
            self.byte(md << 6 | reg3 << 3 | 0b100);
            let (idx, sc) = m.index.map_or((0b100, 0), |(i, s)| (i.low3(), s as u8));
            self.byte(sc << 6 | idx << 3 | base.low3());
        } else {
            self.byte(md << 6 | reg3 << 3 | base.low3());
        }
        match md {
            0b01 => self.byte(m.disp as i8 as u8),
            0b10 => self.imm32(m.disp),
            _ => {}
        }
    }

    /// A REX prefix for an instruction whose only register lives in the opcode or ModRM.rm.
    fn rex_b(&mut self, w: bool, r: Reg) {
        if w || r.rex_bit() != 0 {
            self.byte(0x40 | (w as u8) << 3 | r.rex_bit());
        }
    }

    // ---------------------------------------------------------------- data movement ----

    /// `mov dst, src` (register to register).
    pub fn mov_rr(&mut self, size: Size, dst: Reg, src: Reg) {
        let op = if size == Size::B8 { 0x88 } else { 0x89 };
        self.emit_op(&[], Flags::size(size), &[op], src.num(), dst.into());
    }

    /// `mov dst, [mem]`. A 32-bit load zero-extends into the full register.
    pub fn load(&mut self, size: Size, dst: Reg, mem: Mem) {
        let op = if size == Size::B8 { 0x8A } else { 0x8B };
        self.emit_op(&[], Flags::size(size), &[op], dst.num(), mem.into());
    }

    /// `mov [mem], src`.
    pub fn store(&mut self, size: Size, mem: Mem, src: Reg) {
        let op = if size == Size::B8 { 0x88 } else { 0x89 };
        self.emit_op(&[], Flags::size(size), &[op], src.num(), mem.into());
    }

    /// `mov r32, imm32` (`B8+r id`); zero-extends into the 64-bit register.
    pub fn mov_r32_imm(&mut self, dst: Reg, imm: u32) {
        self.rex_b(false, dst);
        self.byte(0xB8 + dst.low3());
        self.imm32(imm as i32);
    }

    /// `mov r64, imm32` sign-extended (`REX.W C7 /0 id`).
    pub fn mov_r64_simm32(&mut self, dst: Reg, imm: i32) {
        self.emit_digit(&[], Flags::size(Size::B64), &[0xC7], 0, dst.into());
        self.imm32(imm);
    }

    /// `movabs r64, imm64` (`REX.W B8+r io`).
    pub fn movabs(&mut self, dst: Reg, imm: u64) {
        self.rex_b(true, dst);
        self.byte(0xB8 + dst.low3());
        self.bytes_raw(&imm.to_le_bytes());
    }

    /// Load a 64-bit constant with the shortest encoding (§11.1 gotcha 6). Never touches the
    /// flags (so it never uses `xor r,r`).
    pub fn mov_imm(&mut self, dst: Reg, imm: u64) {
        if imm <= u32::MAX as u64 {
            self.mov_r32_imm(dst, imm as u32);
        } else if fits_i32(imm as i64) {
            self.mov_r64_simm32(dst, imm as i32);
        } else {
            self.movabs(dst, imm);
        }
    }

    /// `mov [mem], imm` of `size`; for B64 the imm32 is sign-extended.
    pub fn store_imm(&mut self, size: Size, mem: Mem, imm: i32) {
        match size {
            Size::B8 => {
                assert!(fits_i8(imm as i64) || (0..=255).contains(&imm));
                self.emit_digit(&[], Flags::size(size), &[0xC6], 0, mem.into());
                self.byte(imm as u8);
            }
            Size::B16 => {
                assert!(imm == imm as i16 as i32 || (0..=0xFFFF).contains(&imm));
                self.emit_digit(&[], Flags::size(size), &[0xC7], 0, mem.into());
                self.imm16(imm as i16);
            }
            _ => {
                self.emit_digit(&[], Flags::size(size), &[0xC7], 0, mem.into());
                self.imm32(imm);
            }
        }
    }

    /// `movsxd r64, r/m32` (`REX.W 63 /r`).
    pub fn movsxd(&mut self, dst: Reg, src: impl Into<Rm>) {
        self.emit_op(&[], Flags::size(Size::B64), &[0x63], dst.num(), src.into());
    }

    /// `movsx dst, r/m8|r/m16` with a 32- or 64-bit destination.
    pub fn movsx(&mut self, dst_size: Size, src_size: Size, dst: Reg, src: impl Into<Rm>) {
        let op = match src_size {
            Size::B8 => 0xBE,
            Size::B16 => 0xBF,
            _ => panic!("movsx source must be 8 or 16 bits (use movsxd)"),
        };
        let f = Flags {
            w: dst_size == Size::B64,
            rm_byte: src_size == Size::B8,
            ..Flags::default()
        };
        self.emit_op(&[], f, &[0x0F, op], dst.num(), src.into());
    }

    /// `movzx r32, r/m8|r/m16`; the 32-bit result zero-extends into the full register.
    pub fn movzx(&mut self, src_size: Size, dst: Reg, src: impl Into<Rm>) {
        let op = match src_size {
            Size::B8 => 0xB6,
            Size::B16 => 0xB7,
            _ => panic!("movzx source must be 8 or 16 bits (a 32-bit mov zero-extends)"),
        };
        let f = Flags {
            rm_byte: src_size == Size::B8,
            ..Flags::default()
        };
        self.emit_op(&[], f, &[0x0F, op], dst.num(), src.into());
    }

    /// `lea r64, [mem]`.
    pub fn lea(&mut self, dst: Reg, mem: Mem) {
        self.emit_op(&[], Flags::size(Size::B64), &[0x8D], dst.num(), mem.into());
    }

    /// `xchg r/m, reg` (implicitly locked with a memory operand).
    pub fn xchg(&mut self, size: Size, rm: impl Into<Rm>, reg: Reg) {
        let op = if size == Size::B8 { 0x86 } else { 0x87 };
        self.emit_op(&[], Flags::size(size), &[op], reg.num(), rm.into());
    }

    /// `lock xadd [mem], reg`.
    pub fn lock_xadd(&mut self, size: Size, mem: Mem, reg: Reg) {
        let op = if size == Size::B8 { 0xC0 } else { 0xC1 };
        self.emit_op(
            &[0xF0],
            Flags::size(size),
            &[0x0F, op],
            reg.num(),
            mem.into(),
        );
    }

    /// `lock cmpxchg [mem], reg` (compares with RAX/EAX).
    pub fn lock_cmpxchg(&mut self, size: Size, mem: Mem, reg: Reg) {
        let op = if size == Size::B8 { 0xB0 } else { 0xB1 };
        self.emit_op(
            &[0xF0],
            Flags::size(size),
            &[0x0F, op],
            reg.num(),
            mem.into(),
        );
    }

    // ---------------------------------------------------------------- arithmetic ----

    /// `op dst, src` register-register (`op*8+1 /r`, ModRM.rm = dst).
    pub fn alu_rr(&mut self, size: Size, op: Alu, dst: Reg, src: Reg) {
        let opc = (op as u8) << 3 | if size == Size::B8 { 0 } else { 1 };
        self.emit_op(&[], Flags::size(size), &[opc], src.num(), dst.into());
    }

    /// `op dst, [mem]` (`op*8+3 /r`).
    pub fn alu_rm(&mut self, size: Size, op: Alu, dst: Reg, mem: Mem) {
        let opc = (op as u8) << 3 | if size == Size::B8 { 2 } else { 3 };
        self.emit_op(&[], Flags::size(size), &[opc], dst.num(), mem.into());
    }

    /// `op [mem], src` (`op*8+1 /r`).
    pub fn alu_mr(&mut self, size: Size, op: Alu, mem: Mem, src: Reg) {
        let opc = (op as u8) << 3 | if size == Size::B8 { 0 } else { 1 };
        self.emit_op(&[], Flags::size(size), &[opc], src.num(), mem.into());
    }

    /// `op r/m, imm` with the shortest immediate (`83 /op ib` or `81 /op id`; B8: `80 /op ib`).
    /// For B64 the immediate is sign-extended (§11.1 gotcha 5).
    pub fn alu_ri(&mut self, size: Size, op: Alu, dst: impl Into<Rm>, imm: i32) {
        let rm = dst.into();
        let f = Flags::size(size);
        if size == Size::B8 {
            self.emit_digit(&[], f, &[0x80], op as u8, rm);
            self.byte(imm as u8);
        } else if fits_i8(imm as i64) {
            self.emit_digit(&[], f, &[0x83], op as u8, rm);
            self.byte(imm as i8 as u8);
        } else if size == Size::B16 {
            self.emit_digit(&[], f, &[0x81], op as u8, rm);
            self.imm16(imm as i16);
        } else {
            self.emit_digit(&[], f, &[0x81], op as u8, rm);
            self.imm32(imm);
        }
    }

    /// `test r/m, reg`.
    pub fn test_rr(&mut self, size: Size, a: impl Into<Rm>, b: Reg) {
        let op = if size == Size::B8 { 0x84 } else { 0x85 };
        self.emit_op(&[], Flags::size(size), &[op], b.num(), a.into());
    }

    /// `test r/m, imm` (`F7 /0 id`; B8: `F6 /0 ib`).
    pub fn test_ri(&mut self, size: Size, a: impl Into<Rm>, imm: i32) {
        let rm = a.into();
        match size {
            Size::B8 => {
                self.emit_digit(&[], Flags::size(size), &[0xF6], 0, rm);
                self.byte(imm as u8);
            }
            Size::B16 => {
                self.emit_digit(&[], Flags::size(size), &[0xF7], 0, rm);
                self.imm16(imm as i16);
            }
            _ => {
                self.emit_digit(&[], Flags::size(size), &[0xF7], 0, rm);
                self.imm32(imm);
            }
        }
    }

    /// `shl/shr/sar/rol/ror r/m, imm8` (`C1 /op ib`; B8: `C0`).
    pub fn shift_ri(&mut self, size: Size, op: Shift, dst: impl Into<Rm>, imm: u8) {
        let opc = if size == Size::B8 { 0xC0 } else { 0xC1 };
        self.emit_digit(&[], Flags::size(size), &[opc], op as u8, dst.into());
        self.byte(imm);
    }

    /// `shl/shr/sar/rol/ror r/m, cl` (`D3 /op`; B8: `D2`).
    pub fn shift_cl(&mut self, size: Size, op: Shift, dst: impl Into<Rm>) {
        let opc = if size == Size::B8 { 0xD2 } else { 0xD3 };
        self.emit_digit(&[], Flags::size(size), &[opc], op as u8, dst.into());
    }

    /// BMI2 `shlx/shrx/sarx dst, src, count` (§11.2). 32- or 64-bit; the count is masked to
    /// 5/6 bits like the RISC-V shifts. The caller checks `HostFeatures::bmi2`.
    pub fn shiftx(&mut self, size: Size, op: ShiftX, dst: Reg, src: impl Into<Rm>, count: Reg) {
        assert!(
            matches!(size, Size::B32 | Size::B64),
            "shiftx is 32/64-bit only"
        );
        let rm = src.into();
        // pp: 66 = 01 (SHLX), F3 = 10 (SARX), F2 = 11 (SHRX).
        let pp = match op {
            ShiftX::Shlx => 0b01,
            ShiftX::Sarx => 0b10,
            ShiftX::Shrx => 0b11,
        };
        let (x, b) = match rm {
            Rm::Reg(r) => (0, r.rex_bit()),
            Rm::Mem(m) => (
                m.index.map_or(0, |(i, _)| i.rex_bit()),
                m.base.map_or(0, |b| b.rex_bit()),
            ),
        };
        // 3-byte VEX: C4, [~R ~X ~B m-mmmm=00010 (0F38)], [W ~vvvv L=0 pp].
        self.byte(0xC4);
        self.byte((dst.rex_bit() ^ 1) << 7 | (x ^ 1) << 6 | (b ^ 1) << 5 | 0b00010);
        let w = (size == Size::B64) as u8;
        self.byte(w << 7 | ((!count.num()) & 0xF) << 3 | pp);
        self.byte(0xF7);
        self.modrm_rm(dst.low3(), rm);
    }

    /// `imul dst, r/m` (two-operand, `0F AF /r`).
    pub fn imul_rr(&mut self, size: Size, dst: Reg, src: impl Into<Rm>) {
        assert!(size != Size::B8, "no 8-bit two-operand imul");
        self.emit_op(&[], Flags::size(size), &[0x0F, 0xAF], dst.num(), src.into());
    }

    /// `imul dst, r/m, imm` (`6B /r ib` or `69 /r id`).
    pub fn imul_rri(&mut self, size: Size, dst: Reg, src: impl Into<Rm>, imm: i32) {
        assert!(size != Size::B8, "no 8-bit three-operand imul");
        let f = Flags::size(size);
        if fits_i8(imm as i64) {
            self.emit_op(&[], f, &[0x6B], dst.num(), src.into());
            self.byte(imm as i8 as u8);
        } else if size == Size::B16 {
            self.emit_op(&[], f, &[0x69], dst.num(), src.into());
            self.imm16(imm as i16);
        } else {
            self.emit_op(&[], f, &[0x69], dst.num(), src.into());
            self.imm32(imm);
        }
    }

    /// `not/neg/mul/imul/div/idiv r/m` (`F7 /op`; B8: `F6`).
    pub fn unary(&mut self, size: Size, op: Unary, rm: impl Into<Rm>) {
        let opc = if size == Size::B8 { 0xF6 } else { 0xF7 };
        self.emit_digit(&[], Flags::size(size), &[opc], op as u8, rm.into());
    }

    /// `cqo`: sign-extend RAX into RDX:RAX.
    pub fn cqo(&mut self) {
        self.bytes_raw(&[0x48, 0x99]);
    }

    /// `cdq`: sign-extend EAX into EDX:EAX.
    pub fn cdq(&mut self) {
        self.byte(0x99);
    }

    /// `setcc r/m8`.
    pub fn setcc(&mut self, cond: Cond, dst: impl Into<Rm>) {
        let f = Flags {
            rm_byte: true,
            ..Flags::default()
        };
        self.emit_digit(&[], f, &[0x0F, 0x90 + cond as u8], 0, dst.into());
    }

    /// `cmovcc dst, r/m`.
    pub fn cmov(&mut self, size: Size, cond: Cond, dst: Reg, src: impl Into<Rm>) {
        assert!(size != Size::B8, "no 8-bit cmov");
        self.emit_op(
            &[],
            Flags::size(size),
            &[0x0F, 0x40 + cond as u8],
            dst.num(),
            src.into(),
        );
    }

    // ---------------------------------------------------------------- control flow ----

    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() as u32 - 1)
    }

    /// Bind `label` to the current position.
    pub fn bind(&mut self, label: Label) {
        let slot = &mut self.labels[label.0 as usize];
        assert!(slot.is_none(), "label bound twice");
        *slot = Some(self.buf.len() as u32);
    }

    /// Offset a label was bound to.
    pub fn label_offset(&self, label: Label) -> Option<usize> {
        self.labels[label.0 as usize].map(|o| o as usize)
    }

    fn fixup(&mut self, label: Label, short: bool) {
        let at = self.buf.len() as u32;
        self.fixups.push(Fixup { at, label, short });
        if short {
            self.byte(0);
        } else {
            self.imm32(0);
        }
    }

    /// `lea dst, [rip + disp32]` addressing `label` (`REX.W 8D /r`, ModRM mod=00 rm=101).
    pub fn lea_label(&mut self, dst: Reg, label: Label) {
        self.byte(0x48 | (dst.rex_bit() << 2));
        self.byte(0x8D);
        self.byte((dst.low3() << 3) | 0b101);
        self.fixup(label, false);
    }

    /// `jcc rel32` to `label`. Returns the offset of the rel32 field.
    pub fn jcc(&mut self, cond: Cond, label: Label) -> usize {
        self.bytes_raw(&[0x0F, 0x80 + cond as u8]);
        let at = self.pos();
        self.fixup(label, false);
        at
    }

    /// `jcc rel8` to `label` (must end up within -128..=127 bytes).
    pub fn jcc_short(&mut self, cond: Cond, label: Label) {
        self.byte(0x70 + cond as u8);
        self.fixup(label, true);
    }

    /// `jmp rel32` to `label`. Returns the offset of the rel32 field.
    pub fn jmp(&mut self, label: Label) -> usize {
        self.byte(0xE9);
        let at = self.pos();
        self.fixup(label, false);
        at
    }

    /// `jmp rel8` to `label`.
    pub fn jmp_short(&mut self, label: Label) {
        self.byte(0xEB);
        self.fixup(label, true);
    }

    fn rel32_to(&mut self, target: u64) {
        let field = self.here();
        let rel = target.wrapping_sub(field + 4) as i64;
        assert!(fits_i32(rel), "rel32 target {target:#x} out of range");
        self.imm32(rel as i32);
    }

    /// `jmp rel32` to an absolute host address. Returns the offset of the rel32 field.
    pub fn jmp_abs(&mut self, target: u64) -> usize {
        self.byte(0xE9);
        let at = self.pos();
        self.rel32_to(target);
        at
    }

    /// `jcc rel32` to an absolute host address. Returns the offset of the rel32 field.
    pub fn jcc_abs(&mut self, cond: Cond, target: u64) -> usize {
        self.bytes_raw(&[0x0F, 0x80 + cond as u8]);
        let at = self.pos();
        self.rel32_to(target);
        at
    }

    /// `call rel32` to an absolute host address.
    pub fn call_abs(&mut self, target: u64) {
        self.byte(0xE8);
        self.rel32_to(target);
    }

    /// `call [rip + disp32]`: call through the 8-byte pointer stored at host address `slot`.
    pub fn call_indirect_abs(&mut self, slot: u64) {
        self.bytes_raw(&[0xFF, 0x15]);
        self.rel32_to(slot);
    }

    /// `jmp [rip + disp32]`: jump through the 8-byte pointer stored at host address `slot`.
    pub fn jmp_indirect_abs(&mut self, slot: u64) {
        self.bytes_raw(&[0xFF, 0x25]);
        self.rel32_to(slot);
    }

    /// `call r/m64` (`FF /2`).
    pub fn call_rm(&mut self, rm: impl Into<Rm>) {
        self.emit_digit(&[], Flags::default(), &[0xFF], 2, rm.into());
    }

    /// `jmp r/m64` (`FF /4`).
    pub fn jmp_rm(&mut self, rm: impl Into<Rm>) {
        self.emit_digit(&[], Flags::default(), &[0xFF], 4, rm.into());
    }

    pub fn ret(&mut self) {
        self.byte(0xC3);
    }

    pub fn push(&mut self, r: Reg) {
        self.rex_b(false, r);
        self.byte(0x50 + r.low3());
    }

    pub fn pop(&mut self, r: Reg) {
        self.rex_b(false, r);
        self.byte(0x58 + r.low3());
    }

    /// `n` bytes of padding using the recommended multi-byte NOPs.
    pub fn nop(&mut self, mut n: usize) {
        while n > 0 {
            let k = n.min(NOPS.len());
            self.bytes_raw(NOPS[k - 1]);
            n -= k;
        }
    }

    /// Pad with NOPs until `here() + skew` is a multiple of `align` (a power of two). With
    /// `skew` = the opcode length, this aligns the rel32 field of the next branch (§13.3).
    pub fn align(&mut self, align: u64, skew: u64) {
        debug_assert!(align.is_power_of_two());
        let mis = (self.here() + skew) & (align - 1);
        if mis != 0 {
            self.nop((align - mis) as usize);
        }
    }

    // ---------------------------------------------------------------- SSE / FMA3 (Phase 6) ----

    fn sse_prefix(dbl: bool) -> u8 {
        if dbl { 0xF2 } else { 0xF3 }
    }

    /// `movsd/movss xmm, m64/m32` (or xmm, xmm).
    pub fn movs_load(&mut self, dbl: bool, dst: Xmm, src: impl Into<XRm>) {
        let p = Self::sse_prefix(dbl);
        self.emit_op(
            &[p],
            Flags::default(),
            &[0x0F, 0x10],
            dst.0,
            src.into().rm(),
        );
    }

    /// `movsd/movss m64/m32, xmm`.
    pub fn movs_store(&mut self, dbl: bool, dst: Mem, src: Xmm) {
        let p = Self::sse_prefix(dbl);
        self.emit_op(&[p], Flags::default(), &[0x0F, 0x11], src.0, Rm::Mem(dst));
    }

    /// `addsd/subsd/mulsd/divsd/sqrtsd` (or the `ss` forms): `dst = dst op src` (`sqrt`:
    /// `dst = sqrt(src)`).
    pub fn sse_arith(&mut self, op: SseOp, dbl: bool, dst: Xmm, src: impl Into<XRm>) {
        let p = Self::sse_prefix(dbl);
        self.emit_op(
            &[p],
            Flags::default(),
            &[0x0F, op as u8],
            dst.0,
            src.into().rm(),
        );
    }

    /// `ucomisd/ucomiss a, b` (quiet: invalid only for signaling NaNs); `signaling`: `comisd`
    /// (invalid for any NaN). Unordered sets ZF, PF and CF.
    pub fn comis(&mut self, dbl: bool, signaling: bool, a: Xmm, b: impl Into<XRm>) {
        let op = if signaling { 0x2F } else { 0x2E };
        let pre: &[u8] = if dbl { &[0x66] } else { &[] };
        self.emit_op(pre, Flags::default(), &[0x0F, op], a.0, b.into().rm());
    }

    /// `cvtss2sd` (`to_double`) or `cvtsd2ss`.
    pub fn cvt_fp(&mut self, to_double: bool, dst: Xmm, src: impl Into<XRm>) {
        // cvtss2sd = F3 0F 5A (source single), cvtsd2ss = F2 0F 5A (source double).
        let p = Self::sse_prefix(!to_double);
        self.emit_op(
            &[p],
            Flags::default(),
            &[0x0F, 0x5A],
            dst.0,
            src.into().rm(),
        );
    }

    /// `cvtsi2sd/cvtsi2ss xmm, r/m32|64` (rounds per MXCSR).
    pub fn cvt_int_to_fp(&mut self, dbl: bool, size: Size, dst: Xmm, src: impl Into<Rm>) {
        assert!(matches!(size, Size::B32 | Size::B64));
        let p = Self::sse_prefix(dbl);
        let f = Flags {
            w: size == Size::B64,
            ..Flags::default()
        };
        self.emit_op(&[p], f, &[0x0F, 0x2A], dst.0, src.into());
    }

    /// `cvt(t)sd2si/cvt(t)ss2si r32|64, xmm/m`: `trunc` = round toward zero (`cvtt`), else
    /// per MXCSR. Out of range or NaN gives the "integer indefinite" 0x80…0 and sets IE.
    pub fn cvt_fp_to_int(
        &mut self,
        dbl: bool,
        size: Size,
        trunc: bool,
        dst: Reg,
        src: impl Into<XRm>,
    ) {
        assert!(matches!(size, Size::B32 | Size::B64));
        let p = Self::sse_prefix(dbl);
        let f = Flags {
            w: size == Size::B64,
            ..Flags::default()
        };
        let op = if trunc { 0x2C } else { 0x2D };
        self.emit_op(&[p], f, &[0x0F, op], dst.num(), src.into().rm());
    }

    /// `movq xmm, r64` / `movd xmm, r32` (`66 [REX.W] 0F 6E`).
    pub fn mov_to_xmm(&mut self, size: Size, dst: Xmm, src: Reg) {
        assert!(matches!(size, Size::B32 | Size::B64));
        let f = Flags {
            w: size == Size::B64,
            ..Flags::default()
        };
        self.emit_op(&[0x66], f, &[0x0F, 0x6E], dst.0, Rm::Reg(src));
    }

    /// `movq r64, xmm` / `movd r32, xmm` (`66 [REX.W] 0F 7E`).
    pub fn mov_from_xmm(&mut self, size: Size, dst: Reg, src: Xmm) {
        assert!(matches!(size, Size::B32 | Size::B64));
        let f = Flags {
            w: size == Size::B64,
            ..Flags::default()
        };
        self.emit_op(&[0x66], f, &[0x0F, 0x7E], src.0, Rm::Reg(dst));
    }

    /// `stmxcsr m32` (`0F AE /3`).
    pub fn stmxcsr(&mut self, dst: Mem) {
        self.emit_digit(&[], Flags::default(), &[0x0F, 0xAE], 3, Rm::Mem(dst));
    }

    /// `ldmxcsr m32` (`0F AE /2`).
    pub fn ldmxcsr(&mut self, src: Mem) {
        self.emit_digit(&[], Flags::default(), &[0x0F, 0xAE], 2, Rm::Mem(src));
    }

    /// FMA3 `vf[n]madd/msub231sd/ss dst, a, b`: `dst = ±(a*b) ± dst` with one rounding. The
    /// caller checks `HostFeatures::fma`.
    pub fn fma231(&mut self, op: Fma, dbl: bool, dst: Xmm, a: Xmm, b: impl Into<XRm>) {
        let rm = b.into().rm();
        let (x, bb) = match rm {
            Rm::Reg(r) => (0, r.rex_bit()),
            Rm::Mem(m) => (
                m.index.map_or(0, |(i, _)| i.rex_bit()),
                m.base.map_or(0, |b| b.rex_bit()),
            ),
        };
        let r = (dst.0 >> 3) & 1;
        // 3-byte VEX: C4, [~R ~X ~B m-mmmm=00010 (0F38)], [W ~vvvv L=0 pp=01 (66)].
        self.byte(0xC4);
        self.byte((r ^ 1) << 7 | (x ^ 1) << 6 | (bb ^ 1) << 5 | 0b00010);
        self.byte((dbl as u8) << 7 | ((!a.0) & 0xF) << 3 | 0b01);
        self.byte(op as u8);
        self.modrm_rm(dst.0 & 7, rm);
    }

    pub fn mfence(&mut self) {
        self.bytes_raw(&[0x0F, 0xAE, 0xF0]);
    }

    pub fn int3(&mut self) {
        self.byte(0xCC);
    }

    pub fn ud2(&mut self) {
        self.bytes_raw(&[0x0F, 0x0B]);
    }

    /// Raw 8-byte little-endian data (helper address tables).
    pub fn data64(&mut self, v: u64) {
        self.bytes_raw(&v.to_le_bytes());
    }
}
