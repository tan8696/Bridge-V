//! JIT lowering and translation-cache tests (P2.5, P2.6).
//!
//! * Every integer ALU instruction (OP, OP-32, OP-IMM, OP-IMM-32) is executed by the JIT on
//!   edge-case operands and compared with the interpreter's `alu`/`aluw` (division by zero,
//!   MIN/-1, shift masking, MULH* signs, W sign-extension).
//! * Block formation: a TB ends at a branch, at a page end, at `--max-block`, and records a
//!   fetch fault when the next page is unmapped.
//! * A tiny code cache fills up and is flushed without breaking execution.

use bridgev::cpu::state::CpuState;
use bridgev::interp::{Engine, Env, Stop, alu, aluw};
use bridgev::isa::inst::{AluOp, AluWOp};
use bridgev::jit::{Jit, JitOptions};
use bridgev::mem::direct::DirectMem;
use bridgev::mem::{GuestVirt, prot};

const CODE: u64 = 0x10000;

const VALS: [u64; 14] = [
    0,
    1,
    2,
    3,
    u64::MAX,     // -1
    u64::MAX - 1, // -2
    0x8000_0000_0000_0000,
    0x7FFF_FFFF_FFFF_FFFF,
    0x8000_0000,
    0x7FFF_FFFF,
    0xFFFF_FFFF,
    0xFFFF_FFFF_8000_0000, // sext(INT32_MIN)
    0x1234_5678_9ABC_DEF0,
    63,
];

fn r_type(f7: u32, rs2: u32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    f7 << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | rd << 7 | opc
}

fn i_type(imm: i32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    ((imm as u32) & 0xFFF) << 20 | rs1 << 15 | f3 << 12 | rd << 7 | opc
}

const ECALL: u32 = 0x0000_0073;

struct Rig {
    jit: Jit,
    mem: DirectMem,
    env: Env,
}

impl Rig {
    fn new(opts: JitOptions) -> Rig {
        let mut mem = DirectMem::new().unwrap();
        mem.map(GuestVirt(CODE), 0x2000, prot::R | prot::X).unwrap();
        Rig {
            jit: Jit::new(opts).unwrap(),
            mem,
            env: Env {
                user_mode: true,
                ..Env::default()
            },
        }
    }

    /// Run `insns` (followed by ECALL) at CODE with x5 = a, x6 = b; return x7.
    fn run(&mut self, insns: &[u32], a: u64, b: u64) -> u64 {
        let mut bytes: Vec<u8> = insns.iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.extend(ECALL.to_le_bytes());
        self.mem.write_bytes(GuestVirt(CODE), &bytes).unwrap();
        self.jit.flush();
        let mut cpu = CpuState::new_user(CODE);
        cpu.x[5] = a;
        cpu.x[6] = b;
        cpu.x[7] = 0xDEAD;
        let stop = self.jit.run(&mut cpu, &mut self.mem, &self.env, 1000);
        assert_eq!(stop, Stop::Ecall);
        assert_eq!(cpu.pc, CODE + 4 * insns.len() as u64);
        assert_eq!(cpu.icount, insns.len() as u64);
        assert_eq!((cpu.x[5], cpu.x[6]), (a, b), "sources clobbered");
        cpu.x[7]
    }
}

#[test]
fn register_alu_ops_match_interpreter() {
    let mut rig = Rig::new(JitOptions::default());
    // (op, funct7, funct3) for OP (opcode 0x33).
    let ops = [
        (AluOp::Add, 0x00, 0),
        (AluOp::Sub, 0x20, 0),
        (AluOp::Sll, 0x00, 1),
        (AluOp::Slt, 0x00, 2),
        (AluOp::Sltu, 0x00, 3),
        (AluOp::Xor, 0x00, 4),
        (AluOp::Srl, 0x00, 5),
        (AluOp::Sra, 0x20, 5),
        (AluOp::Or, 0x00, 6),
        (AluOp::And, 0x00, 7),
        (AluOp::Mul, 0x01, 0),
        (AluOp::Mulh, 0x01, 1),
        (AluOp::Mulhsu, 0x01, 2),
        (AluOp::Mulhu, 0x01, 3),
        (AluOp::Div, 0x01, 4),
        (AluOp::Divu, 0x01, 5),
        (AluOp::Rem, 0x01, 6),
        (AluOp::Remu, 0x01, 7),
    ];
    let mut n = 0;
    for (op, f7, f3) in ops {
        let insn = r_type(f7, 6, 5, f3, 7, 0x33);
        for a in VALS {
            for b in VALS {
                assert_eq!(
                    rig.run(&[insn], a, b),
                    alu(op, a, b),
                    "{op:?} {a:#x} {b:#x}"
                );
                n += 1;
            }
        }
    }
    let wops = [
        (AluWOp::Addw, 0x00, 0),
        (AluWOp::Subw, 0x20, 0),
        (AluWOp::Sllw, 0x00, 1),
        (AluWOp::Srlw, 0x00, 5),
        (AluWOp::Sraw, 0x20, 5),
        (AluWOp::Mulw, 0x01, 0),
        (AluWOp::Divw, 0x01, 4),
        (AluWOp::Divuw, 0x01, 5),
        (AluWOp::Remw, 0x01, 6),
        (AluWOp::Remuw, 0x01, 7),
    ];
    for (op, f7, f3) in wops {
        let insn = r_type(f7, 6, 5, f3, 7, 0x3B);
        for a in VALS {
            for b in VALS {
                assert_eq!(
                    rig.run(&[insn], a, b),
                    aluw(op, a, b),
                    "{op:?} {a:#x} {b:#x}"
                );
                n += 1;
            }
        }
    }
    // rd == rs1 == rs2 aliasing: x5 = x5 op x5 then copy to x7 with `addi x7, x5, 0`.
    for (op, f7, f3) in ops {
        for a in VALS {
            let insns = [r_type(f7, 5, 5, f3, 5, 0x33), i_type(0, 5, 0, 7, 0x13)];
            let mut bytes: Vec<u8> = insns.iter().flat_map(|w| w.to_le_bytes()).collect();
            bytes.extend(ECALL.to_le_bytes());
            rig.mem.write_bytes(GuestVirt(CODE), &bytes).unwrap();
            rig.jit.flush();
            let mut cpu = CpuState::new_user(CODE);
            cpu.x[5] = a;
            assert_eq!(
                rig.jit.run(&mut cpu, &mut rig.mem, &rig.env, 100),
                Stop::Ecall
            );
            assert_eq!(cpu.x[7], alu(op, a, a), "{op:?} aliased {a:#x}");
            n += 1;
        }
    }
    eprintln!("{n} register ALU cases");
}

#[test]
fn immediate_alu_ops_match_interpreter() {
    let mut rig = Rig::new(JitOptions::default());
    let imms = [0, 1, -1, 2047, -2048, 31, 32, 63, 0x7F, -0x80];
    let ops = [
        (AluOp::Add, 0),
        (AluOp::Slt, 2),
        (AluOp::Sltu, 3),
        (AluOp::Xor, 4),
        (AluOp::Or, 6),
        (AluOp::And, 7),
    ];
    for (op, f3) in ops {
        for a in VALS {
            for imm in imms {
                let got = rig.run(&[i_type(imm, 5, f3, 7, 0x13)], a, 0);
                assert_eq!(got, alu(op, a, imm as i64 as u64), "{op:?}i {a:#x} {imm}");
            }
        }
    }
    for sh in 0..64u32 {
        for a in VALS {
            let slli = i_type(sh as i32, 5, 1, 7, 0x13);
            let srli = i_type(sh as i32, 5, 5, 7, 0x13);
            let srai = i_type((0x400 | sh) as i32, 5, 5, 7, 0x13);
            assert_eq!(rig.run(&[slli], a, 0), alu(AluOp::Sll, a, sh as u64));
            assert_eq!(rig.run(&[srli], a, 0), alu(AluOp::Srl, a, sh as u64));
            assert_eq!(rig.run(&[srai], a, 0), alu(AluOp::Sra, a, sh as u64));
            if sh < 32 {
                let slliw = i_type(sh as i32, 5, 1, 7, 0x1B);
                let srliw = i_type(sh as i32, 5, 5, 7, 0x1B);
                let sraiw = i_type((0x400 | sh) as i32, 5, 5, 7, 0x1B);
                assert_eq!(rig.run(&[slliw], a, 0), aluw(AluWOp::Sllw, a, sh as u64));
                assert_eq!(rig.run(&[srliw], a, 0), aluw(AluWOp::Srlw, a, sh as u64));
                assert_eq!(rig.run(&[sraiw], a, 0), aluw(AluWOp::Sraw, a, sh as u64));
            }
        }
    }
    for a in VALS {
        for imm in imms {
            let got = rig.run(&[i_type(imm, 5, 0, 7, 0x1B)], a, 0);
            assert_eq!(
                got,
                aluw(AluWOp::Addw, a, imm as i64 as u64),
                "addiw {a:#x} {imm}"
            );
        }
    }
}

fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn block_formation() {
    let nop = i_type(0, 0, 0, 0, 0x13); // addi x0, x0, 0
    let beq = 0x0000_0063; // beq x0, x0, 0
    let opts = JitOptions {
        max_block: 5,
        ..JitOptions::default()
    };
    let mut rig = Rig::new(opts);
    // Ends at a branch.
    rig.mem
        .write_bytes(GuestVirt(CODE), &words(&[nop, nop, beq, nop]))
        .unwrap();
    let id = rig.jit.tb_for(CODE, &rig.mem);
    assert_eq!(rig.jit.tb(id).insns.len(), 3);
    // Ends at max_block.
    rig.mem
        .write_bytes(GuestVirt(CODE + 0x100), &words(&[nop; 8]))
        .unwrap();
    let id = rig.jit.tb_for(CODE + 0x100, &rig.mem);
    assert_eq!(rig.jit.tb(id).insns.len(), 5);
    // Ends at the page end, even with room for more.
    let mut big = Rig::new(JitOptions::default());
    big.mem
        .write_bytes(GuestVirt(CODE + 0xFF0), &words(&[nop; 8]))
        .unwrap();
    let id = big.jit.tb_for(CODE + 0xFF0, &big.mem);
    assert_eq!(big.jit.tb(id).insns.len(), 4);
    assert!(big.jit.tb(id).fetch_fault.is_none());
    // Falls into an unmapped page: the TB records the fetch fault, executing raises it.
    big.mem
        .write_bytes(GuestVirt(CODE + 0x1FF8), &words(&[nop, nop]))
        .unwrap();
    let id = big.jit.tb_for(CODE + 0x1FF8, &big.mem);
    assert_eq!(big.jit.tb(id).insns.len(), 2);
    let id2 = big.jit.tb_for(CODE + 0x2000, &big.mem);
    assert!(big.jit.tb(id2).insns.is_empty() && big.jit.tb(id2).fetch_fault.is_some());
    let mut cpu = CpuState::new_user(CODE + 0x1FF8);
    match big.jit.run(&mut cpu, &mut big.mem, &big.env, 100) {
        Stop::Fault(e) => assert_eq!((e.cause, e.tval), (1, CODE + 0x2000)),
        other => panic!("{other:?}"),
    }
    assert_eq!((cpu.pc, cpu.icount), (CODE + 0x2000, 2));
}

#[test]
fn tiny_code_cache_flushes_and_keeps_running() {
    // 256 blocks of `addi x7, x7, 1; jal x0, +4`, then ECALL: x7 = 256. With a 4 KiB cache the
    // buffer fills repeatedly; every flush must leave execution correct.
    let addi = i_type(1, 7, 0, 7, 0x13);
    let jal4 = 0x0040_006F; // jal x0, +4
    let mut code = Vec::new();
    for _ in 0..256 {
        code.extend([addi, jal4]);
    }
    code.push(ECALL);
    let opts = JitOptions {
        code_cache: 4096,
        ..JitOptions::default()
    };
    let mut rig = Rig::new(opts);
    rig.mem.write_bytes(GuestVirt(CODE), &words(&code)).unwrap();
    for round in 0..3 {
        let mut cpu = CpuState::new_user(CODE);
        assert_eq!(
            rig.jit.run(&mut cpu, &mut rig.mem, &rig.env, 10_000),
            Stop::Ecall
        );
        assert_eq!(cpu.x[7], 256, "round {round}");
    }
    assert!(rig.jit.stats.full_flushes > 0, "cache never filled");
}
