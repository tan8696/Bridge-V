//! FP differential fuzzer (P6.5): random F/D blocks with operands biased toward special values
//! (NaN payloads, signaling NaNs, ±0, ±inf, subnormals, the extreme normals, the integer
//! conversion boundaries), random rounding modes (static and dynamic, including the reserved
//! ones), random fflags and mstatus.FS. The JIT must match the SoftFloat interpreter
//! bit-exactly: f and x registers, fflags, frm, mstatus, pc, icount, memory.
//!
//! Each block runs under the FP variant the dispatcher would pick for the state (D47), and
//! the fast variant is also forced onto every state: its prologue guard must then either
//! leave before executing anything (FS not Dirty, or frm not RNE with dynamic-rm ops) or,
//! when the guard does not apply, match the interpreter.
//!
//! `PROPTEST_CASES` sets the number of blocks (default 2000).

#[path = "common/rvgen.rs"]
mod rvgen;

use std::cell::RefCell;

use bridgev::interp::{BlockExit, Engine, exec_block};
use bridgev::ir::lift::{LiftOptions, lift_with};
use bridgev::jit::dispatch::fp_slow;
use bridgev::jit::{Jit, JitOptions, RegAlloc};
use proptest::prelude::*;
use rvgen::{CODE, FpState, Harness, Outcome};

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
        ("linear", base.clone()),
        (
            "pinned",
            JitOptions {
                regalloc: RegAlloc::Pinned,
                ..base.clone()
            },
        ),
        (
            "linear, no host features (no FMA3/BMI2)",
            JitOptions {
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

/// Run the block as one TB of the given FP variant, and the interpreter over the same
/// instructions.
fn run(
    h: &mut Harness,
    regs: &[u64; 32],
    st: &FpState,
    jit: &mut Jit,
    slow: bool,
) -> (Outcome, Outcome) {
    jit.flush();
    let mut id = 0;
    let got = h.run_init(
        regs,
        |c| st.apply(c),
        |cpu, mem, _, _| {
            id = jit.tb_for_variant(CODE, mem, slow);
            let n = jit.tb(id).insns.len() as i64;
            jit.exec(cpu, mem, id, n)
        },
    );
    let tb = jit.tb(id);
    let (insns, ff) = (tb.insns.clone(), tb.fetch_fault);
    let want = h.run_init(
        regs,
        |c| st.apply(c),
        |cpu, mem, _, _| exec_block(cpu, mem, &insns, ff, false),
    );
    (got, want)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn fp_blocks_match_softfloat(code in rvgen::fp_block(), regs in rvgen::regs(), st in rvgen::fp_state()) {
        let mut h = Harness::new(&code);
        let listing: Vec<String> = h
            .insns
            .iter()
            .map(|d| bridgev::isa::disasm::disasm(&d.inst))
            .collect();
        // Does the fast variant's guard fire for this state?
        let probe = {
            let mut cpu = bridgev::cpu::state::CpuState::new_user(CODE);
            st.apply(&mut cpu);
            cpu
        };
        let slow = fp_slow(&probe);
        JITS.with(|jits| -> Result<(), TestCaseError> {
            for (name, jit) in jits.borrow_mut().iter_mut() {
                let fma = jit.options().host_features && bridgev::backend::x86::features::host().fma;
                let ir = lift_with(&h.insns, h.fetch_fault, CODE, LiftOptions { inline_fp: true, fma });
                let (has_fp, has_dyn) = (ir.fp_guard, ir.fp_dyn);
                let guard_fires = has_fp && (st.fs != 3 || (has_dyn && st.frm != 0));
                for (variant, forced) in [(slow, false), (false, true)] {
                    if forced && !slow {
                        continue; // the natural variant already was the fast one
                    }
                    let (got, want) = run(&mut h, &regs, &st, jit, variant);
                    let want = if forced && guard_fires && jit.options().regalloc != RegAlloc::None {
                        // Nothing may execute: the state is the initial one.
                        h.run_init(&regs, |c| st.apply(c), |_, _, _, _| BlockExit::Continue)
                    } else {
                        want
                    };
                    prop_assert_eq!(
                        &got,
                        &want,
                        "config `{}`, {} variant{} (FS {}, frm {}), block:\n  {}\nIR:\n{}",
                        name,
                        if variant { "slow" } else { "fast" },
                        if forced { " forced" } else { "" },
                        st.fs,
                        st.frm,
                        listing.join("\n  "),
                        ir
                    );
                }
            }
            Ok(())
        })?;
    }
}
