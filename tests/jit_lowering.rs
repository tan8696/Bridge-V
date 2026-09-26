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
use bridgev::jit::{Jit, JitOptions, RegAlloc};
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

// ------------------------------------------------------------------ Phase 3: chaining ----

/// P3.1: every chainable exit is a `jmp rel32` (slot 0) or `jcc rel32` (slot 1) whose rel32
/// field is 4-byte aligned and initially targets the TB's own stub.
#[test]
fn exit_slots_are_aligned_and_target_their_stubs() {
    let nop = i_type(0, 0, 0, 0, 0x13);
    let bne = 0x0073_1463; // bne x6, x7, +8 (distinct registers: not folded to a jump)
    let jal = 0x0080_006F; // jal x0, +8
    for regalloc in [RegAlloc::None, RegAlloc::Pinned, RegAlloc::Linear] {
        let mut rig = Rig::new(JitOptions {
            regalloc,
            ..JitOptions::default()
        });
        let mut checked = 0;
        for pre in 0..6 {
            // Vary the code before the exits so every alignment case occurs.
            let base = CODE + 0x100 * pre as u64;
            let mut code = vec![nop; pre];
            code.push(bne);
            rig.mem.write_bytes(GuestVirt(base), &words(&code)).unwrap();
            rig.mem
                .write_bytes(GuestVirt(base + 0x80), &words(&[nop, jal]))
                .unwrap();
            for pc in [base, base + 0x80] {
                let id = rig.jit.tb_for(pc, &rig.mem);
                for (slot, ex) in rig.jit.tb(id).exits.iter().enumerate() {
                    let Some(ex) = ex else { continue };
                    assert_eq!(ex.patch_at % 4, 0, "slot {slot} of TB {pc:#x}");
                    let op = rig.jit.code_at(ex.patch_at - 2, 2);
                    if slot == 0 {
                        assert_eq!(op[1], 0xE9);
                    } else {
                        assert_eq!((op[0], op[1] & 0xF0), (0x0F, 0x80));
                    }
                    let rel =
                        i32::from_le_bytes(rig.jit.code_at(ex.patch_at, 4).try_into().unwrap());
                    assert_eq!(
                        ex.patch_at.wrapping_add(4).wrapping_add(rel as u64),
                        ex.stub
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 6 * 3, "{regalloc:?}");
    }
}

/// P3.2: link A→B by running, then invalidate B: A's exit must go back to its stub (no jump
/// into stale code), and the retranslated B must be linked again.
#[test]
fn link_then_invalidate_unlinks() {
    let a = [i_type(1, 7, 0, 7, 0x13), 0x0100_006F]; // addi x7,x7,1 ; jal x0,+16
    let b_old = [i_type(10, 7, 0, 7, 0x13), ECALL]; // addi x7,x7,10 ; ecall
    let b_new = [i_type(100, 7, 0, 7, 0x13), ECALL];
    let mut rig = Rig::new(JitOptions::default());
    rig.mem.write_bytes(GuestVirt(CODE), &words(&a)).unwrap();
    rig.mem
        .write_bytes(GuestVirt(CODE + 20), &words(&b_old))
        .unwrap();
    let run = |rig: &mut Rig| {
        let mut cpu = CpuState::new_user(CODE);
        assert_eq!(
            rig.jit.run(&mut cpu, &mut rig.mem, &rig.env, 100),
            Stop::Ecall
        );
        (cpu.x[7], cpu.icount)
    };
    // First run: A exits through its stub; the dispatcher then links A → B.
    assert_eq!(run(&mut rig), (11, 3));
    // new_user starts with FS = Initial: the dispatcher used the slow FP variant (D47).
    let ida = rig.jit.tb_for_variant(CODE, &rig.mem, true);
    let idb = rig.jit.tb_for_variant(CODE + 20, &rig.mem, true);
    let ex = rig.jit.tb(ida).exits[0].unwrap();
    assert_eq!(ex.linked, Some(idb));
    assert_eq!(rig.jit.tb(idb).incoming, vec![(ida, 0)]);
    let rel = i32::from_le_bytes(rig.jit.code_at(ex.patch_at, 4).try_into().unwrap());
    assert_eq!(ex.patch_at + 4 + rel as u64, rig.jit.tb(idb).host);
    // Second run goes A → B without the dispatcher.
    let entries = rig.jit.stats.entries;
    assert_eq!(run(&mut rig), (11, 3));
    assert_eq!(rig.jit.stats.entries - entries, 1);
    // Change B's code and invalidate it.
    rig.mem
        .write_bytes(GuestVirt(CODE + 20), &words(&b_new))
        .unwrap();
    assert!(rig.jit.invalidate_pc(CODE + 20));
    let ex = rig.jit.tb(ida).exits[0].unwrap();
    assert_eq!(ex.linked, None);
    let rel = i32::from_le_bytes(rig.jit.code_at(ex.patch_at, 4).try_into().unwrap());
    assert_eq!(ex.patch_at + 4 + rel as u64, ex.stub);
    assert_eq!(rig.jit.stats.chain_unlinks, 1);
    // A now exits through its stub again and reaches the new B, which gets linked.
    let entries = rig.jit.stats.entries;
    assert_eq!(run(&mut rig), (101, 3));
    assert_eq!(rig.jit.stats.entries - entries, 2);
    let idb2 = rig.jit.tb_for_variant(CODE + 20, &rig.mem, true);
    assert_ne!(idb2, idb);
    assert_eq!(rig.jit.tb(ida).exits[0].unwrap().linked, Some(idb2));
}

/// P3.3: a chained infinite loop (`jal x0, 0` linked to itself) still returns to the
/// dispatcher every slice, so an instruction limit stops it; icount is exact. Runs at every
/// regalloc level (the IR levels keep the budget in R9, D46).
#[test]
fn infinite_chained_loop_is_preempted() {
    for (slice, regalloc) in [1u64, 7, 1000, 100_000].into_iter().flat_map(|s| {
        [
            (s, RegAlloc::None),
            (s, RegAlloc::Pinned),
            (s, RegAlloc::Linear),
        ]
    }) {
        let opts = JitOptions {
            slice,
            regalloc,
            ..JitOptions::default()
        };
        let mut rig = Rig::new(opts);
        rig.mem
            .write_bytes(
                GuestVirt(CODE),
                &words(&[i_type(1, 7, 0, 7, 0x13), 0xFFDF_F06F]),
            ) // addi; jal x0,-4
            .unwrap();
        let mut cpu = CpuState::new_user(CODE);
        let limit = 1_000_001;
        assert_eq!(
            rig.jit.run(&mut cpu, &mut rig.mem, &rig.env, limit),
            Stop::Limit
        );
        // TBs are [addi, jal] (2 insns) after the first; the limit is checked per slice.
        assert!(
            cpu.icount >= limit && cpu.icount < limit + 2,
            "{}",
            cpu.icount
        );
        assert_eq!(
            cpu.x[7],
            cpu.icount.div_ceil(2),
            "slice {slice} {regalloc:?}"
        );
        let budget_exits = rig.jit.stats.exits[bridgev::cpu::state::exit::BUDGET as usize];
        assert!(
            budget_exits >= (limit / slice.max(2)).saturating_sub(1),
            "slice {slice}"
        );
        assert!(rig.jit.stats.chain_links >= 1);
    }
}

/// P3.4: JALR through the jump cache: a call/return loop runs correctly, returns hit the jump
/// cache after the first miss, and a full flush of a tiny code cache (which invalidates every
/// jump-cache entry) keeps it correct.
#[test]
fn jump_cache_calls_and_returns_survive_flushes() {
    // loop: jal ra, f ; addi t1, t1, -1 ; bnez t1, loop ; ecall
    // f:    addi x7, x7, 1 ; ret                      (encodings from llvm-mc)
    let code = [
        0x0100_00EF,
        0xFFF3_0313,
        0xFE03_1CE3,
        ECALL,
        i_type(1, 7, 0, 7, 0x13),
        0x0000_8067,
    ];
    for (cache, chain) in [(256 << 20, true), (4096, true), (256 << 20, false)] {
        let opts = JitOptions {
            code_cache: cache,
            chain,
            ..JitOptions::default()
        };
        let mut rig = Rig::new(opts);
        rig.mem.write_bytes(GuestVirt(CODE), &words(&code)).unwrap();
        for round in 0..3 {
            let mut cpu = CpuState::new_user(CODE);
            cpu.x[6] = 50;
            assert_eq!(
                rig.jit.run(&mut cpu, &mut rig.mem, &rig.env, 10_000),
                Stop::Ecall
            );
            assert_eq!(cpu.x[7], 50, "cache {cache} chain {chain} round {round}");
            assert_eq!(cpu.icount, 250);
            if cache == 4096 {
                // Force a full flush between rounds: fill the buffer with other TBs.
                for k in 0..200u64 {
                    rig.jit.tb_for(CODE + 0x800 + 4 * k, &rig.mem);
                }
            }
        }
        let misses = rig.jit.stats.exits[bridgev::cpu::state::exit::LOOKUP as usize];
        if !chain {
            assert_eq!(misses, 150, "no-chain: every return exits");
        } else {
            // One miss per round: the first return; later returns hit.
            assert_eq!(misses, 3, "cache {cache}");
        }
        if cache == 4096 {
            assert!(rig.jit.stats.full_flushes > 0);
        }
    }
}

/// P4.6: sixteen guest values live at once (more than the 7-register pool), consumed in a
/// different order, plus back-to-back DIV/REM chains that reuse their own sources (the
/// RAX/RDX constraint). Every level must agree with the reference model; the linear allocator
/// must actually have evicted something.
#[test]
fn register_pressure_and_division_chains() {
    let mut code = Vec::new();
    for k in 0..16u32 {
        code.push(i_type(k as i32 * 3 + 1, 5, 0, 8 + k, 0x13)); // addi x(8+k), x5, 3k+1
        code.push(r_type(1, 6, 8 + k, 0, 8 + k, 0x33)); // mul x(8+k), x(8+k), x6
    }
    code.push(r_type(0, 23, 8, 4, 7, 0x33)); // xor x7, x8, x23
    for k in (1..15u32).rev() {
        code.push(r_type(0, 8 + k, 7, 0, 7, 0x33)); // add x7, x7, x(8+k)
    }
    let divs = [
        r_type(1, 6, 7, 4, 7, 0x33),  // div  x7, x7, x6
        r_type(1, 7, 6, 6, 8, 0x33),  // rem  x8, x6, x7
        r_type(1, 8, 7, 5, 7, 0x33),  // divu x7, x7, x8
        r_type(1, 7, 7, 7, 9, 0x33),  // remu x9, x7, x7
        r_type(1, 9, 8, 4, 10, 0x3B), // divw x10, x8, x9
        r_type(1, 10, 7, 0, 7, 0x33), // mul  x7, x7, x10
        r_type(0, 9, 7, 0, 7, 0x33),  // add  x7, x7, x9
        r_type(0, 8, 7, 0, 7, 0x33),  // add  x7, x7, x8
    ];
    code.extend(divs);
    let model = |a: u64, b: u64| -> u64 {
        let v: Vec<u64> = (0..16u64)
            .map(|k| a.wrapping_add(3 * k + 1).wrapping_mul(b))
            .collect();
        let mut x7 = v[0] ^ v[15];
        for k in (1..15).rev() {
            x7 = x7.wrapping_add(v[k]);
        }
        x7 = alu(AluOp::Div, x7, b);
        let x8 = alu(AluOp::Rem, b, x7);
        x7 = alu(AluOp::Divu, x7, x8);
        let x9 = alu(AluOp::Remu, x7, x7);
        let x10 = aluw(AluWOp::Divw, x8, x9);
        x7.wrapping_mul(x10).wrapping_add(x9).wrapping_add(x8)
    };
    for regalloc in [RegAlloc::None, RegAlloc::Pinned, RegAlloc::Linear] {
        for pin in [vec![2, 1, 10, 15], vec![7, 8, 6, 5], vec![]] {
            let mut rig = Rig::new(JitOptions {
                regalloc,
                pin: pin.clone(),
                ..JitOptions::default()
            });
            for &a in &VALS {
                for &b in &VALS {
                    assert_eq!(
                        rig.run(&code, a, b),
                        model(a, b),
                        "{regalloc:?} pin {pin:?}: a={a:#x} b={b:#x}"
                    );
                }
            }
            if regalloc == RegAlloc::Linear {
                let s = &rig.jit.stats;
                assert!(
                    s.writebacks > 0 && s.fills > 0,
                    "{regalloc:?} {pin:?}: {s:?}"
                );
            }
        }
    }
}
