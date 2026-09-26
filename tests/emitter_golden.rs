//! Emitter golden tests (P2.1, CLAUDE.md §21 item 2).
//!
//! Every form is encoded by `bridgev::backend::x86::emit`, decoded by `iced-x86`, and the
//! decoded instruction is compared structurally with what was requested: mnemonic, every
//! operand (register, memory base/index/scale/displacement/size, immediate, branch target)
//! and the length (so no trailing bytes are left undecoded). Sweeps cover all 16 registers in
//! every position and the displacement boundaries of §11.1; named tests pin the exact bytes of
//! each encoding gotcha.

use std::sync::atomic::{AtomicU64, Ordering};

use bridgev::backend::x86::emit::*;
use bridgev::backend::x86::regs::Reg;
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

const ORIGIN: u64 = 0x7f00_1234_0000;
const DISPS: [i32; 8] = [0, 1, -128, 127, 128, -129, i32::MIN, i32::MAX];

static CHECKED: AtomicU64 = AtomicU64::new(0);

fn r64(r: Reg) -> Register {
    use Register::*;
    [
        RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8, R9, R10, R11, R12, R13, R14, R15,
    ][r.num() as usize]
}

fn r32(r: Reg) -> Register {
    use Register::*;
    [
        EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI, R8D, R9D, R10D, R11D, R12D, R13D, R14D, R15D,
    ][r.num() as usize]
}

fn r16(r: Reg) -> Register {
    use Register::*;
    [
        AX, CX, DX, BX, SP, BP, SI, DI, R8W, R9W, R10W, R11W, R12W, R13W, R14W, R15W,
    ][r.num() as usize]
}

fn r8(r: Reg) -> Register {
    use Register::*;
    [
        AL, CL, DL, BL, SPL, BPL, SIL, DIL, R8L, R9L, R10L, R11L, R12L, R13L, R14L, R15L,
    ][r.num() as usize]
}

fn rs(r: Reg, s: Size) -> Register {
    match s {
        Size::B8 => r8(r),
        Size::B16 => r16(r),
        Size::B32 => r32(r),
        Size::B64 => r64(r),
    }
}

/// An expected operand.
#[derive(Clone, Copy, Debug)]
enum E {
    R(Register),
    /// Memory operand of `bytes` size.
    M(usize, Mem),
    /// Immediate as iced reports it (sign-extended to the operand size where applicable).
    I(u64),
    /// Near-branch target (absolute).
    T(u64),
}

const SIZES: [Size; 4] = [Size::B8, Size::B16, Size::B32, Size::B64];

fn decode(bytes: &[u8]) -> Instruction {
    let mut d = Decoder::with_ip(64, bytes, ORIGIN, DecoderOptions::NONE);
    let i = d.decode();
    assert!(!i.is_invalid(), "invalid encoding {bytes:02x?}");
    i
}

#[track_caller]
fn check(bytes: &[u8], mnem: Mnemonic, ops: &[E]) {
    let i = decode(bytes);
    let ctx = || format!("bytes {bytes:02x?} decoded as {i} (expected {mnem:?} {ops:?})");
    assert_eq!(i.len(), bytes.len(), "length mismatch: {}", ctx());
    assert_eq!(i.mnemonic(), mnem, "{}", ctx());
    assert_eq!(i.op_count() as usize, ops.len(), "{}", ctx());
    for (n, e) in ops.iter().enumerate() {
        let n = n as u32;
        match *e {
            E::R(r) => {
                assert_eq!(i.op_kind(n), OpKind::Register, "{}", ctx());
                assert_eq!(i.op_register(n), r, "{}", ctx());
            }
            E::M(sz, m) => {
                assert_eq!(i.op_kind(n), OpKind::Memory, "{}", ctx());
                assert_eq!(
                    i.memory_base(),
                    m.base.map_or(Register::None, r64),
                    "{}",
                    ctx()
                );
                let (idx, sc) = m
                    .index
                    .map_or((Register::None, 1), |(r, s)| (r64(r), s.factor()));
                assert_eq!(i.memory_index(), idx, "{}", ctx());
                assert_eq!(i.memory_index_scale(), sc, "{}", ctx());
                assert_eq!(i.memory_displacement64(), m.disp as i64 as u64, "{}", ctx());
                assert_eq!(i.memory_size().size(), sz, "{}", ctx());
            }
            E::I(v) => {
                // Compare at the operand's width: iced returns sign-extended 64-bit values
                // for the widening kinds (e.g. imm8→16 of -1 is 0xFFFF_FFFF_FFFF_FFFF).
                let bits = match i.op_kind(n) {
                    OpKind::Immediate8 => 8,
                    OpKind::Immediate16 | OpKind::Immediate8to16 => 16,
                    OpKind::Immediate32 | OpKind::Immediate8to32 => 32,
                    OpKind::Immediate64 | OpKind::Immediate8to64 | OpKind::Immediate32to64 => 64,
                    k => panic!("operand {n} is {k:?}, not an immediate: {}", ctx()),
                };
                let mask = if bits == 64 {
                    u64::MAX
                } else {
                    (1u64 << bits) - 1
                };
                assert_eq!(i.immediate(n) & mask, v & mask, "{}", ctx());
            }
            E::T(t) => {
                assert!(
                    matches!(
                        i.op_kind(n),
                        OpKind::NearBranch64 | OpKind::NearBranch32 | OpKind::NearBranch16
                    ),
                    "{}",
                    ctx()
                );
                assert_eq!(i.near_branch_target(), t, "{}", ctx());
            }
        }
    }
    CHECKED.fetch_add(1, Ordering::Relaxed);
}

fn asm(f: impl FnOnce(&mut Asm)) -> Vec<u8> {
    let mut a = Asm::new(ORIGIN);
    f(&mut a);
    a.finish()
}

/// Every memory operand shape: all bases (incl. none), all indexes (incl. none, excl. RSP),
/// all scales, all displacement boundaries.
fn all_mems() -> Vec<Mem> {
    let mut v = Vec::new();
    for &disp in &DISPS {
        v.push(Mem::abs(disp));
        for &i in Reg::ALL.iter().filter(|&&r| r != Reg::Rsp) {
            for s in Scale::ALL {
                v.push(Mem {
                    base: None,
                    index: Some((i, s)),
                    disp,
                });
            }
        }
        for &b in &Reg::ALL {
            v.push(Mem::base(b, disp));
            for &i in Reg::ALL.iter().filter(|&&r| r != Reg::Rsp) {
                for s in Scale::ALL {
                    v.push(Mem::bi(b, i, s, disp));
                }
            }
        }
    }
    v
}

/// A smaller set that still hits every ModRM/SIB special case: each register as base (with
/// several displacements) and as index, plus the no-base forms.
fn some_mems() -> Vec<Mem> {
    let mut v = vec![Mem::abs(0x1234), Mem::abs(-8)];
    for &b in &Reg::ALL {
        for d in [0, -128, 127, 128, i32::MIN] {
            v.push(Mem::base(b, d));
        }
        if b != Reg::Rsp {
            v.push(Mem::bi(Reg::Rax, b, Scale::S8, 16));
            v.push(Mem::bi(Reg::R13, b, Scale::S1, 0));
            v.push(Mem {
                base: None,
                index: Some((b, Scale::S4)),
                disp: -4,
            });
        }
    }
    v
}

// ------------------------------------------------------------------------------ sweeps ----

#[test]
fn mem_operand_full_sweep() {
    let mems = all_mems();
    for &dst in &Reg::ALL {
        for &m in &mems {
            let b = asm(|a| a.load(Size::B64, dst, m));
            check(&b, Mnemonic::Mov, &[E::R(r64(dst)), E::M(8, m)]);
        }
    }
    println!("mem_operand_full_sweep: {} forms", 16 * mems.len());
}

#[test]
fn mov_forms_all_sizes() {
    let mems = some_mems();
    for s in SIZES {
        let n = s.bytes() as usize;
        for &d in &Reg::ALL {
            for &r in &Reg::ALL {
                let b = asm(|a| a.mov_rr(s, d, r));
                check(&b, Mnemonic::Mov, &[E::R(rs(d, s)), E::R(rs(r, s))]);
            }
            for &m in &mems {
                let b = asm(|a| a.load(s, d, m));
                check(&b, Mnemonic::Mov, &[E::R(rs(d, s)), E::M(n, m)]);
                let b = asm(|a| a.store(s, m, d));
                check(&b, Mnemonic::Mov, &[E::M(n, m), E::R(rs(d, s))]);
            }
        }
        for &m in &mems {
            for imm in [0, 1, -1, 127, -128] {
                let b = asm(|a| a.store_imm(s, m, imm));
                let v = match s {
                    Size::B8 => imm as u8 as u64,
                    Size::B16 => imm as u16 as u64,
                    Size::B32 => imm as u32 as u64,
                    Size::B64 => imm as i64 as u64,
                };
                check(&b, Mnemonic::Mov, &[E::M(n, m), E::I(v)]);
            }
        }
    }
    for s in [Size::B32, Size::B64] {
        for &m in &mems {
            for imm in [i32::MIN, i32::MAX, 0x1234_5678] {
                let b = asm(|a| a.store_imm(s, m, imm));
                let v = if s == Size::B64 {
                    imm as i64 as u64
                } else {
                    imm as u32 as u64
                };
                check(&b, Mnemonic::Mov, &[E::M(s.bytes() as usize, m), E::I(v)]);
            }
        }
    }
}

#[test]
fn mov_immediates() {
    let imms: [u64; 12] = [
        0,
        1,
        0x7FFF_FFFF,
        0x8000_0000,
        0xFFFF_FFFF,
        0x1_0000_0000,
        u64::MAX,
        (-128i64) as u64,
        i32::MIN as i64 as u64,
        (i32::MIN as i64 - 1) as u64,
        0x8000_0000_0000_0000,
        0x0123_4567_89AB_CDEF,
    ];
    for &d in &Reg::ALL {
        for &v in &imms {
            let b = asm(|a| a.mov_imm(d, v));
            if v <= u32::MAX as u64 {
                assert_eq!(
                    b.len(),
                    5 + (d.rex_bit() as usize),
                    "mov r32,imm32 expected"
                );
                check(&b, Mnemonic::Mov, &[E::R(r32(d)), E::I(v)]);
            } else if v as i64 == v as i32 as i64 {
                assert_eq!(b.len(), 7, "mov r/m64,imm32 expected");
                check(&b, Mnemonic::Mov, &[E::R(r64(d)), E::I(v)]);
            } else {
                assert_eq!(b.len(), 10, "movabs expected");
                check(&b, Mnemonic::Mov, &[E::R(r64(d)), E::I(v)]);
            }
            let b = asm(|a| a.movabs(d, v));
            check(&b, Mnemonic::Mov, &[E::R(r64(d)), E::I(v)]);
        }
    }
}

#[test]
fn extensions_and_lea() {
    let mems = some_mems();
    for &d in &Reg::ALL {
        for &r in &Reg::ALL {
            let b = asm(|a| a.movsxd(d, r));
            check(&b, Mnemonic::Movsxd, &[E::R(r64(d)), E::R(r32(r))]);
            for (ds, src) in [
                (Size::B32, Size::B8),
                (Size::B32, Size::B16),
                (Size::B64, Size::B8),
                (Size::B64, Size::B16),
            ] {
                let b = asm(|a| a.movsx(ds, src, d, r));
                check(&b, Mnemonic::Movsx, &[E::R(rs(d, ds)), E::R(rs(r, src))]);
            }
            for src in [Size::B8, Size::B16] {
                let b = asm(|a| a.movzx(src, d, r));
                check(&b, Mnemonic::Movzx, &[E::R(r32(d)), E::R(rs(r, src))]);
            }
        }
        for &m in &mems {
            let b = asm(|a| a.movsxd(d, m));
            check(&b, Mnemonic::Movsxd, &[E::R(r64(d)), E::M(4, m)]);
            for src in [Size::B8, Size::B16] {
                let b = asm(|a| a.movsx(Size::B64, src, d, m));
                check(
                    &b,
                    Mnemonic::Movsx,
                    &[E::R(r64(d)), E::M(src.bytes() as usize, m)],
                );
                let b = asm(|a| a.movzx(src, d, m));
                check(
                    &b,
                    Mnemonic::Movzx,
                    &[E::R(r32(d)), E::M(src.bytes() as usize, m)],
                );
            }
            let b = asm(|a| a.lea(d, m));
            // iced reports LEA's memory size as "unknown" (0 bytes): the operand is not accessed.
            check(&b, Mnemonic::Lea, &[E::R(r64(d)), E::M(0, m)]);
        }
    }
}

fn alu_mnem(op: Alu) -> Mnemonic {
    match op {
        Alu::Add => Mnemonic::Add,
        Alu::Or => Mnemonic::Or,
        Alu::Adc => Mnemonic::Adc,
        Alu::Sbb => Mnemonic::Sbb,
        Alu::And => Mnemonic::And,
        Alu::Sub => Mnemonic::Sub,
        Alu::Xor => Mnemonic::Xor,
        Alu::Cmp => Mnemonic::Cmp,
    }
}

#[test]
fn alu_forms() {
    let mems = some_mems();
    for op in Alu::ALL {
        let mn = alu_mnem(op);
        for s in SIZES {
            let n = s.bytes() as usize;
            for &d in &Reg::ALL {
                for &r in &Reg::ALL {
                    let b = asm(|a| a.alu_rr(s, op, d, r));
                    check(&b, mn, &[E::R(rs(d, s)), E::R(rs(r, s))]);
                }
                for &m in &mems {
                    let b = asm(|a| a.alu_rm(s, op, d, m));
                    check(&b, mn, &[E::R(rs(d, s)), E::M(n, m)]);
                    let b = asm(|a| a.alu_mr(s, op, m, d));
                    check(&b, mn, &[E::M(n, m), E::R(rs(d, s))]);
                }
                let imms: &[i32] = match s {
                    Size::B8 => &[0, 1, -1, 127, -128],
                    Size::B16 => &[0, 1, -1, 127, -128, 128, -129, 0x7FFF, -0x8000],
                    _ => &[0, 1, -1, 127, -128, 128, -129, i32::MAX, i32::MIN],
                };
                for &imm in imms {
                    let b = asm(|a| a.alu_ri(s, op, d, imm));
                    let v = match s {
                        Size::B8 => imm as u8 as u64,
                        Size::B16 => imm as u16 as u64,
                        Size::B32 => imm as u32 as u64,
                        Size::B64 => imm as i64 as u64,
                    };
                    check(&b, mn, &[E::R(rs(d, s)), E::I(v)]);
                    if imm == -1 {
                        let b = asm(|a| a.alu_ri(s, op, mems[d.num() as usize], imm));
                        check(&b, mn, &[E::M(n, mems[d.num() as usize]), E::I(v)]);
                    }
                }
            }
        }
    }
}

#[test]
fn test_shift_mul_div() {
    let mems = some_mems();
    for s in SIZES {
        let n = s.bytes() as usize;
        for &d in &Reg::ALL {
            for &r in &Reg::ALL {
                let b = asm(|a| a.test_rr(s, d, r));
                check(&b, Mnemonic::Test, &[E::R(rs(d, s)), E::R(rs(r, s))]);
            }
            let b = asm(|a| a.test_ri(s, d, -1));
            let v = match s {
                Size::B64 => u64::MAX,
                _ => (1u64 << (8 * n)) - 1,
            };
            check(&b, Mnemonic::Test, &[E::R(rs(d, s)), E::I(v)]);
            for op in Shift::ALL {
                let mn = match op {
                    Shift::Rol => Mnemonic::Rol,
                    Shift::Ror => Mnemonic::Ror,
                    Shift::Shl => Mnemonic::Shl,
                    Shift::Shr => Mnemonic::Shr,
                    Shift::Sar => Mnemonic::Sar,
                };
                for imm in [1u8, 5, 31, 63] {
                    let b = asm(|a| a.shift_ri(s, op, d, imm));
                    check(&b, mn, &[E::R(rs(d, s)), E::I(imm as u64)]);
                }
                let b = asm(|a| a.shift_cl(s, op, d));
                check(&b, mn, &[E::R(rs(d, s)), E::R(Register::CL)]);
            }
            for op in Unary::ALL {
                let mn = match op {
                    Unary::Not => Mnemonic::Not,
                    Unary::Neg => Mnemonic::Neg,
                    Unary::Mul => Mnemonic::Mul,
                    Unary::Imul => Mnemonic::Imul,
                    Unary::Div => Mnemonic::Div,
                    Unary::Idiv => Mnemonic::Idiv,
                };
                let b = asm(|a| a.unary(s, op, d));
                check(&b, mn, &[E::R(rs(d, s))]);
                let m = mems[d.num() as usize * 3 % mems.len()];
                let b = asm(|a| a.unary(s, op, m));
                check(&b, mn, &[E::M(n, m)]);
            }
            if s != Size::B8 {
                for &r in &Reg::ALL {
                    let b = asm(|a| a.imul_rr(s, d, r));
                    check(&b, Mnemonic::Imul, &[E::R(rs(d, s)), E::R(rs(r, s))]);
                    for c in Cond::ALL {
                        let b = asm(|a| a.cmov(s, c, d, r));
                        check(&b, cmov_mnem(c), &[E::R(rs(d, s)), E::R(rs(r, s))]);
                    }
                }
                for imm in [3, -100, 1000] {
                    let b = asm(|a| a.imul_rri(s, d, Reg::R12, imm));
                    let v = match s {
                        Size::B16 => imm as u16 as u64,
                        Size::B32 => imm as u32 as u64,
                        _ => imm as i64 as u64,
                    };
                    check(
                        &b,
                        Mnemonic::Imul,
                        &[E::R(rs(d, s)), E::R(rs(Reg::R12, s)), E::I(v)],
                    );
                }
            }
        }
    }
    let b = asm(|a| a.cqo());
    check(&b, Mnemonic::Cqo, &[]);
    let b = asm(|a| a.cdq());
    check(&b, Mnemonic::Cdq, &[]);
}

fn cmov_mnem(c: Cond) -> Mnemonic {
    use Mnemonic::*;
    [
        Cmovo, Cmovno, Cmovb, Cmovae, Cmove, Cmovne, Cmovbe, Cmova, Cmovs, Cmovns, Cmovp, Cmovnp,
        Cmovl, Cmovge, Cmovle, Cmovg,
    ][c as usize]
}

fn setcc_mnem(c: Cond) -> Mnemonic {
    use Mnemonic::*;
    [
        Seto, Setno, Setb, Setae, Sete, Setne, Setbe, Seta, Sets, Setns, Setp, Setnp, Setl, Setge,
        Setle, Setg,
    ][c as usize]
}

fn jcc_mnem(c: Cond) -> Mnemonic {
    use Mnemonic::*;
    [
        Jo, Jno, Jb, Jae, Je, Jne, Jbe, Ja, Js, Jns, Jp, Jnp, Jl, Jge, Jle, Jg,
    ][c as usize]
}

#[test]
fn setcc_all_regs_and_conds() {
    for c in Cond::ALL {
        for &d in &Reg::ALL {
            let b = asm(|a| a.setcc(c, d));
            check(&b, setcc_mnem(c), &[E::R(r8(d))]);
        }
        let m = Mem::base(Reg::R13, 0);
        let b = asm(|a| a.setcc(c, m));
        check(&b, setcc_mnem(c), &[E::M(1, m)]);
        assert_eq!(c.negate().negate(), c);
        assert_ne!(c.negate(), c);
    }
}

#[test]
fn atomics_stack_misc() {
    let mems = some_mems();
    for s in SIZES {
        let n = s.bytes() as usize;
        for &r in &Reg::ALL {
            for &m in mems.iter().step_by(7) {
                let b = asm(|a| a.lock_xadd(s, m, r));
                check(&b, Mnemonic::Xadd, &[E::M(n, m), E::R(rs(r, s))]);
                assert!(decode(&b).has_lock_prefix());
                let b = asm(|a| a.lock_cmpxchg(s, m, r));
                check(&b, Mnemonic::Cmpxchg, &[E::M(n, m), E::R(rs(r, s))]);
                assert!(decode(&b).has_lock_prefix());
                let b = asm(|a| a.xchg(s, m, r));
                check(&b, Mnemonic::Xchg, &[E::M(n, m), E::R(rs(r, s))]);
            }
        }
    }
    for &r in &Reg::ALL {
        let b = asm(|a| a.push(r));
        check(&b, Mnemonic::Push, &[E::R(r64(r))]);
        let b = asm(|a| a.pop(r));
        check(&b, Mnemonic::Pop, &[E::R(r64(r))]);
        let b = asm(|a| a.call_rm(r));
        check(&b, Mnemonic::Call, &[E::R(r64(r))]);
        let b = asm(|a| a.jmp_rm(r));
        check(&b, Mnemonic::Jmp, &[E::R(r64(r))]);
    }
    for &m in &mems {
        let b = asm(|a| a.call_rm(m));
        check(&b, Mnemonic::Call, &[E::M(8, m)]);
        let b = asm(|a| a.jmp_rm(m));
        check(&b, Mnemonic::Jmp, &[E::M(8, m)]);
    }
    check(&asm(|a| a.ret()), Mnemonic::Ret, &[]);
    check(&asm(|a| a.mfence()), Mnemonic::Mfence, &[]);
    check(&asm(|a| a.int3()), Mnemonic::Int3, &[]);
    check(&asm(|a| a.ud2()), Mnemonic::Ud2, &[]);
    // The 3..=9 byte forms decode with a (never accessed) memory operand, e.g.
    // `nop dword ptr [rax+rax]`, so only mnemonic and length are compared.
    for n in 1..=9 {
        let b = asm(|a| a.nop(n));
        let i = decode(&b);
        assert_eq!(
            (i.mnemonic(), i.len()),
            (Mnemonic::Nop, n),
            "{n}-byte nop {b:02x?}"
        );
    }
    // Longer padding is a sequence of NOPs.
    let b = asm(|a| a.nop(23));
    let mut d = Decoder::with_ip(64, &b, ORIGIN, DecoderOptions::NONE);
    let mut total = 0;
    for i in &mut d {
        assert_eq!(i.mnemonic(), Mnemonic::Nop);
        total += i.len();
    }
    assert_eq!(total, 23);
}

// ------------------------------------------------------------------------------ branches ----

#[test]
fn labels_forward_backward_near_far() {
    for c in Cond::ALL {
        // Backward short and near.
        let mut a = Asm::new(ORIGIN);
        let top = a.new_label();
        a.bind(top);
        a.nop(5);
        let short_at = a.pos();
        a.jcc_short(c, top);
        a.nop(200);
        let near_at = a.pos();
        a.jcc(c, top);
        // Forward near (far: more than 127 bytes away) and forward short.
        let fwd = a.new_label();
        let fwd_at = a.pos();
        a.jcc(c, fwd);
        let fwd_short_at = a.pos();
        let fwd2 = a.new_label();
        a.jcc_short(c, fwd2);
        a.nop(100);
        a.bind(fwd2);
        a.nop(300);
        a.bind(fwd);
        let fwd_off = a.pos();
        let fwd2_off = a.label_offset(fwd2).unwrap();
        let b = a.finish();
        let t = |o: usize| ORIGIN + o as u64;
        let i = Decoder::with_ip(64, &b[short_at..], t(short_at), DecoderOptions::NONE).decode();
        assert_eq!(
            (i.mnemonic(), i.near_branch_target(), i.len()),
            (jcc_mnem(c), t(0), 2)
        );
        let i = Decoder::with_ip(64, &b[near_at..], t(near_at), DecoderOptions::NONE).decode();
        assert_eq!(
            (i.mnemonic(), i.near_branch_target(), i.len()),
            (jcc_mnem(c), t(0), 6)
        );
        let i = Decoder::with_ip(64, &b[fwd_at..], t(fwd_at), DecoderOptions::NONE).decode();
        assert_eq!((i.near_branch_target(), i.len()), (t(fwd_off), 6));
        let i = Decoder::with_ip(
            64,
            &b[fwd_short_at..],
            t(fwd_short_at),
            DecoderOptions::NONE,
        )
        .decode();
        assert_eq!((i.near_branch_target(), i.len()), (t(fwd2_off), 2));
    }
    // Unconditional jumps.
    let mut a = Asm::new(ORIGIN);
    let l = a.new_label();
    let j32 = a.pos();
    a.jmp(l);
    let j8 = a.pos();
    a.jmp_short(l);
    a.nop(50);
    a.bind(l);
    let lo = a.pos();
    let b = a.finish();
    let i = Decoder::with_ip(64, &b[j32..], ORIGIN + j32 as u64, DecoderOptions::NONE).decode();
    assert_eq!(
        (i.mnemonic(), i.near_branch_target(), i.len()),
        (Mnemonic::Jmp, ORIGIN + lo as u64, 5)
    );
    let i = Decoder::with_ip(64, &b[j8..], ORIGIN + j8 as u64, DecoderOptions::NONE).decode();
    assert_eq!(
        (i.mnemonic(), i.near_branch_target(), i.len()),
        (Mnemonic::Jmp, ORIGIN + lo as u64, 2)
    );
}

#[test]
#[should_panic(expected = "rel8 branch out of range")]
fn short_branch_out_of_range_panics() {
    let mut a = Asm::new(ORIGIN);
    let l = a.new_label();
    a.jmp_short(l);
    a.nop(128);
    a.bind(l);
    a.finish();
}

#[test]
fn absolute_branches_and_rip_relative() {
    for delta in [0i64, 5, -5, 0x1000, -0x1000, 0x7FFF_0000, -0x7FFF_0000] {
        let target = (ORIGIN as i64 + delta) as u64;
        check(
            &asm(|a| {
                a.jmp_abs(target);
            }),
            Mnemonic::Jmp,
            &[E::T(target)],
        );
        check(
            &asm(|a| a.call_abs(target)),
            Mnemonic::Call,
            &[E::T(target)],
        );
        for c in Cond::ALL {
            check(
                &asm(|a| {
                    a.jcc_abs(c, target);
                }),
                jcc_mnem(c),
                &[E::T(target)],
            );
        }
        for (b, mn) in [
            (asm(|a| a.call_indirect_abs(target)), Mnemonic::Call),
            (asm(|a| a.jmp_indirect_abs(target)), Mnemonic::Jmp),
        ] {
            let i = decode(&b);
            assert_eq!((i.mnemonic(), i.len()), (mn, 6));
            assert!(i.is_ip_rel_memory_operand());
            assert_eq!(i.ip_rel_memory_address(), target);
        }
    }
}

#[test]
fn lea_rip_relative_label() {
    for r in Reg::ALL {
        for back in [false, true] {
            let b = asm(|a| {
                let l = a.new_label();
                if back {
                    a.bind(l);
                    a.nop(3);
                    a.lea_label(r, l);
                } else {
                    a.lea_label(r, l);
                    a.nop(5);
                    a.bind(l);
                }
            });
            let at = if back { 3 } else { 0 };
            let i = decode(&b[at..]);
            assert_eq!(
                (i.mnemonic(), i.len(), i.op0_register()),
                (Mnemonic::Lea, 7, r64(r))
            );
            assert!(i.is_ip_rel_memory_operand());
            let want = if back { ORIGIN } else { ORIGIN + 12 };
            // decode() places the instruction at ORIGIN; re-base for the backward case.
            let got = i.ip_rel_memory_address() + at as u64;
            assert_eq!(got, want, "{r:?} back={back}");
        }
    }
}

#[test]
fn write_rel32_patches() {
    // A jmp rel32 at ORIGIN+0x40 (field at +0x41) retargeted to ORIGIN+0x2000 (§28.5).
    let mut buf = asm(|a| {
        a.nop(0x40);
        a.jmp_abs(ORIGIN);
    });
    write_rel32(&mut buf, 0x41, ORIGIN + 0x41, ORIGIN + 0x2000);
    assert_eq!(&buf[0x40..0x45], &[0xE9, 0xBB, 0x1F, 0x00, 0x00]);
    let i = Decoder::with_ip(64, &buf[0x40..], ORIGIN + 0x40, DecoderOptions::NONE).decode();
    assert_eq!(i.near_branch_target(), ORIGIN + 0x2000);
}

#[test]
fn align_pads_rel32_field() {
    for pre in 0..16 {
        let mut a = Asm::new(ORIGIN);
        a.nop(pre);
        a.align(4, 1); // `jmp rel32`: 1 opcode byte
        let at = a.jmp_abs(ORIGIN);
        assert_eq!((ORIGIN + at as u64) % 4, 0, "pre={pre}");
        let mut a = Asm::new(ORIGIN);
        a.nop(pre);
        a.align(4, 2); // `jcc rel32`: 2 opcode bytes
        let at = a.jcc_abs(Cond::Ne, ORIGIN);
        assert_eq!((ORIGIN + at as u64) % 4, 0, "pre={pre}");
    }
}

// ---------------------------------------------------------------- named gotcha bytes ----

fn bytes(f: impl FnOnce(&mut Asm)) -> Vec<u8> {
    asm(f)
}

#[test]
fn gotcha_r12_rsp_need_sib() {
    assert_eq!(
        bytes(|a| a.load(Size::B64, Reg::Rax, Mem::base(Reg::R12, 0))),
        [0x49, 0x8B, 0x04, 0x24]
    );
    assert_eq!(
        bytes(|a| a.load(Size::B64, Reg::Rax, Mem::base(Reg::Rsp, 8))),
        [0x48, 0x8B, 0x44, 0x24, 0x08]
    );
}

#[test]
fn gotcha_r13_rbp_need_disp8() {
    assert_eq!(
        bytes(|a| a.load(Size::B64, Reg::Rax, Mem::base(Reg::R13, 0))),
        [0x49, 0x8B, 0x45, 0x00]
    );
    assert_eq!(
        bytes(|a| a.load(Size::B64, Reg::Rax, Mem::base(Reg::Rbp, 0))),
        [0x48, 0x8B, 0x45, 0x00]
    );
    // SIB base=101 with an index also needs mod=01.
    assert_eq!(
        bytes(|a| a.load(
            Size::B64,
            Reg::Rax,
            Mem::bi(Reg::R13, Reg::Rcx, Scale::S2, 0)
        )),
        [0x49, 0x8B, 0x44, 0x4D, 0x00]
    );
}

#[test]
fn gotcha_r12_is_a_valid_index() {
    // [rax + r12*4]: SIB.index=100 with REX.X=1 is R12, not "no index".
    assert_eq!(
        bytes(|a| a.load(
            Size::B64,
            Reg::Rax,
            Mem::bi(Reg::Rax, Reg::R12, Scale::S4, 0)
        )),
        [0x4A, 0x8B, 0x04, 0xA0]
    );
}

#[test]
#[should_panic(expected = "RSP cannot be an index")]
fn gotcha_rsp_index_rejected() {
    bytes(|a| {
        a.load(
            Size::B64,
            Reg::Rax,
            Mem::bi(Reg::Rax, Reg::Rsp, Scale::S1, 0),
        )
    });
}

#[test]
fn gotcha_byte_regs_need_rex() {
    // mov sil, al → 40 88 C6 (without REX, C6 would be DH).
    assert_eq!(
        bytes(|a| a.mov_rr(Size::B8, Reg::Rsi, Reg::Rax)),
        [0x40, 0x88, 0xC6]
    );
    assert_eq!(
        bytes(|a| a.mov_rr(Size::B8, Reg::Rax, Reg::Rdi)),
        [0x40, 0x88, 0xF8]
    );
    assert_eq!(
        bytes(|a| a.mov_rr(Size::B8, Reg::Rax, Reg::Rcx)),
        [0x88, 0xC8]
    );
    assert_eq!(
        bytes(|a| a.setcc(Cond::L, Reg::Rsi)),
        [0x40, 0x0F, 0x9C, 0xC6]
    );
    assert_eq!(bytes(|a| a.setcc(Cond::L, Reg::Rdx)), [0x0F, 0x9C, 0xC2]);
    assert_eq!(
        bytes(|a| a.store(Size::B8, Mem::base(Reg::Rbx, 0), Reg::Rdi)),
        [0x40, 0x88, 0x3B]
    );
    assert_eq!(
        bytes(|a| a.movzx(Size::B8, Reg::Rax, Reg::Rsp)),
        [0x40, 0x0F, 0xB6, 0xC4]
    );
}

#[test]
fn gotcha_rbp_absolute_and_rip() {
    // [disp32] without base must use a SIB (mod=00 rm=101 alone is RIP-relative).
    assert_eq!(
        bytes(|a| a.load(Size::B64, Reg::Rax, Mem::abs(0x10))),
        [0x48, 0x8B, 0x04, 0x25, 0x10, 0, 0, 0]
    );
}

#[test]
fn gotcha_immediates() {
    // mov eax, 0xFFFFFFFF zero-extends; mov rax, -1 sign-extends imm32.
    assert_eq!(
        bytes(|a| a.mov_imm(Reg::Rax, 0xFFFF_FFFF)),
        [0xB8, 0xFF, 0xFF, 0xFF, 0xFF]
    );
    assert_eq!(
        bytes(|a| a.mov_imm(Reg::Rax, u64::MAX)),
        [0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF]
    );
    assert_eq!(
        bytes(|a| a.mov_imm(Reg::R9, 1 << 40)),
        [0x49, 0xB9, 0, 0, 0, 0, 0, 1, 0, 0]
    );
    assert_eq!(
        bytes(|a| a.alu_ri(Size::B64, Alu::Add, Reg::Rax, -1)),
        [0x48, 0x83, 0xC0, 0xFF]
    );
    assert_eq!(
        bytes(|a| a.alu_ri(Size::B64, Alu::And, Reg::Rax, -2)),
        [0x48, 0x83, 0xE0, 0xFE]
    );
}

#[test]
fn worked_examples_from_claude_md() {
    // §28.5: cmp r14, rsi = 49 39 F6.
    assert_eq!(
        bytes(|a| a.alu_rr(Size::B64, Alu::Cmp, Reg::R14, Reg::Rsi)),
        [0x49, 0x39, 0xF6]
    );
    // jne rel32 at +0x10 to +0x200 = 0F 85 EA 01 00 00.
    let b = asm(|a| {
        a.nop(0x10);
        a.jcc_abs(Cond::Ne, ORIGIN + 0x200);
    });
    assert_eq!(&b[0x10..], &[0x0F, 0x85, 0xEA, 0x01, 0x00, 0x00]);
    // §8.4 prologue pieces.
    assert_eq!(
        bytes(|a| a.lea(Reg::Rbp, Mem::base(Reg::Rdi, 128))),
        [0x48, 0x8D, 0xAF, 0x80, 0, 0, 0]
    );
    assert_eq!(bytes(|a| a.push(Reg::R15)), [0x41, 0x57]);
    assert_eq!(bytes(|a| a.jmp_rm(Reg::Rsi)), [0xFF, 0xE6]);
}

#[test]
fn zz_report_count() {
    // Informational total; accurate with `--test-threads=1 --nocapture` (libtest runs tests
    // in name order, so this one is last).
    println!(
        "emitter golden checks so far: {}",
        CHECKED.load(Ordering::Relaxed)
    );
}

#[test]
fn bmi2_shifts_all_registers() {
    let mems = some_mems();
    for s in [Size::B32, Size::B64] {
        for op in [ShiftX::Shlx, ShiftX::Shrx, ShiftX::Sarx] {
            let mn = match op {
                ShiftX::Shlx => Mnemonic::Shlx,
                ShiftX::Shrx => Mnemonic::Shrx,
                ShiftX::Sarx => Mnemonic::Sarx,
            };
            for &d in &Reg::ALL {
                for &a in &Reg::ALL {
                    for &c in &Reg::ALL {
                        let b = asm(|x| x.shiftx(s, op, d, a, c));
                        check(&b, mn, &[E::R(rs(d, s)), E::R(rs(a, s)), E::R(rs(c, s))]);
                    }
                }
                for &m in mems.iter().step_by(5) {
                    let b = asm(|x| x.shiftx(s, op, d, m, Reg::R9));
                    check(
                        &b,
                        mn,
                        &[
                            E::R(rs(d, s)),
                            E::M(s.bytes() as usize, m),
                            E::R(rs(Reg::R9, s)),
                        ],
                    );
                }
            }
        }
    }
    // Spot check against llvm-mc: shlx rax, rcx, rdx = c4 e2 e9 f7 c1.
    assert_eq!(
        asm(|x| x.shiftx(Size::B64, ShiftX::Shlx, Reg::Rax, Reg::Rcx, Reg::Rdx)),
        [0xC4, 0xE2, 0xE9, 0xF7, 0xC1]
    );
}

// ------------------------------------------------------------------------------ SSE / FMA3 ----

fn xr(x: Xmm) -> Register {
    use Register::*;
    [
        XMM0, XMM1, XMM2, XMM3, XMM4, XMM5, XMM6, XMM7, XMM8, XMM9, XMM10, XMM11, XMM12, XMM13,
        XMM14, XMM15,
    ][x.0 as usize]
}

fn xmms() -> impl Iterator<Item = Xmm> {
    (0..16).map(Xmm)
}

/// P6.1: scalar SSE moves, arithmetic, compares and conversions for every XMM register, XMM
/// and memory sources, and both precisions.
#[test]
fn sse_scalar_forms() {
    use Mnemonic::*;
    let mems = some_mems();
    for dbl in [true, false] {
        let w = if dbl { 8 } else { 4 };
        for x in xmms() {
            for m in &mems {
                check(
                    &asm(|a| a.movs_load(dbl, x, *m)),
                    if dbl { Movsd } else { Movss },
                    &[E::R(xr(x)), E::M(w, *m)],
                );
                check(
                    &asm(|a| a.movs_store(dbl, *m, x)),
                    if dbl { Movsd } else { Movss },
                    &[E::M(w, *m), E::R(xr(x))],
                );
            }
            for y in xmms() {
                for (op, md, ms) in [
                    (SseOp::Add, Addsd, Addss),
                    (SseOp::Sub, Subsd, Subss),
                    (SseOp::Mul, Mulsd, Mulss),
                    (SseOp::Div, Divsd, Divss),
                    (SseOp::Sqrt, Sqrtsd, Sqrtss),
                ] {
                    check(
                        &asm(|a| a.sse_arith(op, dbl, x, y)),
                        if dbl { md } else { ms },
                        &[E::R(xr(x)), E::R(xr(y))],
                    );
                }
                check(
                    &asm(|a| a.comis(dbl, false, x, y)),
                    if dbl { Ucomisd } else { Ucomiss },
                    &[E::R(xr(x)), E::R(xr(y))],
                );
                check(
                    &asm(|a| a.comis(dbl, true, x, y)),
                    if dbl { Comisd } else { Comiss },
                    &[E::R(xr(x)), E::R(xr(y))],
                );
                check(
                    &asm(|a| a.cvt_fp(dbl, x, y)),
                    if dbl { Cvtss2sd } else { Cvtsd2ss },
                    &[E::R(xr(x)), E::R(xr(y))],
                );
            }
            let m = Mem::base(Reg::Rbp, 0xC0 + 8 * x.0 as i32);
            check(
                &asm(|a| a.sse_arith(SseOp::Add, dbl, x, m)),
                if dbl { Addsd } else { Addss },
                &[E::R(xr(x)), E::M(w, m)],
            );
            check(
                &asm(|a| a.comis(dbl, true, x, m)),
                if dbl { Comisd } else { Comiss },
                &[E::R(xr(x)), E::M(w, m)],
            );
            for &r in &Reg::ALL {
                for size in [Size::B32, Size::B64] {
                    check(
                        &asm(|a| a.cvt_int_to_fp(dbl, size, x, r)),
                        if dbl { Cvtsi2sd } else { Cvtsi2ss },
                        &[E::R(xr(x)), E::R(rs(r, size))],
                    );
                    for trunc in [true, false] {
                        let mn = match (dbl, trunc) {
                            (true, true) => Cvttsd2si,
                            (true, false) => Cvtsd2si,
                            (false, true) => Cvttss2si,
                            (false, false) => Cvtss2si,
                        };
                        check(
                            &asm(|a| a.cvt_fp_to_int(dbl, size, trunc, r, x)),
                            mn,
                            &[E::R(rs(r, size)), E::R(xr(x))],
                        );
                    }
                }
                check(
                    &asm(|a| a.mov_to_xmm(Size::B64, x, r)),
                    Movq,
                    &[E::R(xr(x)), E::R(r64(r))],
                );
                check(
                    &asm(|a| a.mov_to_xmm(Size::B32, x, r)),
                    Movd,
                    &[E::R(xr(x)), E::R(r32(r))],
                );
                check(
                    &asm(|a| a.mov_from_xmm(Size::B64, r, x)),
                    Movq,
                    &[E::R(r64(r)), E::R(xr(x))],
                );
                check(
                    &asm(|a| a.mov_from_xmm(Size::B32, r, x)),
                    Movd,
                    &[E::R(r32(r)), E::R(xr(x))],
                );
            }
        }
    }
    for m in &mems {
        check(&asm(|a| a.stmxcsr(*m)), Stmxcsr, &[E::M(4, *m)]);
        check(&asm(|a| a.ldmxcsr(*m)), Ldmxcsr, &[E::M(4, *m)]);
    }
}

/// P6.2: FMA3 231 forms, every register triple and a memory source, both precisions.
#[test]
fn fma3_forms() {
    use Mnemonic::*;
    for dbl in [true, false] {
        for (op, md, ms) in [
            (Fma::Madd, Vfmadd231sd, Vfmadd231ss),
            (Fma::Msub, Vfmsub231sd, Vfmsub231ss),
            (Fma::Nmadd, Vfnmadd231sd, Vfnmadd231ss),
            (Fma::Nmsub, Vfnmsub231sd, Vfnmsub231ss),
        ] {
            let mn = if dbl { md } else { ms };
            for d in xmms() {
                for x in xmms() {
                    for y in xmms() {
                        check(
                            &asm(|a| a.fma231(op, dbl, d, x, y)),
                            mn,
                            &[E::R(xr(d)), E::R(xr(x)), E::R(xr(y))],
                        );
                    }
                    for m in some_mems() {
                        check(
                            &asm(|a| a.fma231(op, dbl, d, x, m)),
                            mn,
                            &[E::R(xr(d)), E::R(xr(x)), E::M(if dbl { 8 } else { 4 }, m)],
                        );
                    }
                }
            }
        }
    }
}
