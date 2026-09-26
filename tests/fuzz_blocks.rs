//! Random-block differential fuzzer (P4.8, CLAUDE.md §21.5): random RV64 straight-line blocks
//! (`common/rvgen.rs`) with random initial registers run once under the interpreter and once
//! as a JIT translation block under several backend configurations; registers, FP registers,
//! pc, icount, the LR/SC reservation, the block exit and the scratch page must be identical.
//!
//! Configurations cover every `--regalloc` level, the linear allocator without pinned
//! registers, and the linear allocator without BMI2 (RCX-constrained shifts) with the fuzzed
//! memory base x5 pinned. Failing cases are shrunk and persisted by proptest's regression file
//! (`proptest-regressions`), which is replayed first on every later run.
//!
//! `PROPTEST_CASES` sets the number of blocks per run (default 2000, the CI smoke run; every
//! block runs under all configurations).

#[path = "common/rvgen.rs"]
mod rvgen;

use std::cell::RefCell;

use bridgev::backend::x86::disasm::disasm_x86;
use bridgev::interp::{Engine, exec_block};
use bridgev::ir::lift::lift;
use bridgev::ir::opt;
use bridgev::jit::{Jit, JitOptions, RegAlloc};
use proptest::prelude::*;
use rvgen::{CODE, Harness, Outcome};

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000)
}

fn configs() -> Vec<(&'static str, JitOptions)> {
    let base = JitOptions {
        code_cache: 1 << 20,
        ..JitOptions::default()
    };
    vec![
        (
            "none",
            JitOptions {
                regalloc: RegAlloc::None,
                ..base.clone()
            },
        ),
        (
            "pinned",
            JitOptions {
                regalloc: RegAlloc::Pinned,
                ..base.clone()
            },
        ),
        ("linear", base.clone()),
        (
            "linear, no pins",
            JitOptions {
                pin: Vec::new(),
                ..base.clone()
            },
        ),
        (
            "linear, no BMI2, pin x5,x7,x8,x9",
            JitOptions {
                pin: vec![5, 7, 8, 9],
                host_features: false,
                ..base
            },
        ),
    ]
}

thread_local! {
    static JITS: RefCell<Vec<(&'static str, Jit)>> = RefCell::new(
        configs()
            .into_iter()
            .map(|(name, o)| (name, Jit::new(o).expect("JIT")))
            .collect(),
    );
}

/// Run the block at `CODE` as one TB under `jit` (fresh translation: the previous case's code
/// was different); returns the JIT outcome and the interpreter's.
fn run_jit(h: &mut Harness, regs: &[u64; 32], jit: &mut Jit) -> (Outcome, Outcome) {
    jit.flush();
    let mut id = 0;
    let got = h.run(regs, |cpu, mem, _, _| {
        id = jit.tb_for(CODE, mem);
        let n = jit.tb(id).insns.len() as i64;
        jit.exec(cpu, mem, id, n)
    });
    // Reference over exactly the translated instructions (a TB may be shorter than the
    // interpreter's block if it ran out of spill slots and was retranslated).
    let tb = jit.tb(id);
    let (insns, ff) = (tb.insns.clone(), tb.fetch_fault);
    let want = h.run(regs, |cpu, mem, _, _| {
        exec_block(cpu, mem, &insns, ff, false)
    });
    (got, want)
}

/// Divergence report (§21.4): the optimized IR and the host code of the failing TB.
fn report(h: &Harness, jit: &mut Jit, regalloc: RegAlloc) -> String {
    let mut ir = lift(&h.insns, h.fetch_fault, CODE);
    if regalloc == RegAlloc::Linear {
        opt::optimize(&mut ir);
    }
    let id = jit.tb_for(CODE, &h.mem);
    let host = jit.tb(id).host;
    let x86 = disasm_x86(jit.tb_code(id), host);
    format!("--- IR ---\n{ir}\n--- x86 ---\n{x86}")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn jit_blocks_match_the_interpreter(code in rvgen::block(), regs in rvgen::regs()) {
        let mut h = Harness::new(&code);
        let listing: Vec<String> = h
            .insns
            .iter()
            .map(|d| bridgev::isa::disasm::disasm(&d.inst))
            .collect();
        JITS.with(|jits| -> Result<(), TestCaseError> {
            for (name, jit) in jits.borrow_mut().iter_mut() {
                let (got, want) = run_jit(&mut h, &regs, jit);
                if got != want {
                    let regalloc = jit.options().regalloc;
                    let r = report(&h, jit, regalloc);
                    prop_assert_eq!(
                        &got,
                        &want,
                        "config `{}`, block:\n  {}\n{}",
                        name,
                        listing.join("\n  "),
                        r
                    );
                }
            }
            Ok(())
        })?;
    }
}
