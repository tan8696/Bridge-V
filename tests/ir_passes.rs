//! Lifter and optimizer property tests (P4.2, P4.3): on random RV64 blocks, lifting to IR and
//! evaluating it must reproduce the interpreter exactly, before and after every optimizer
//! pass. This isolates front-end (IR) bugs from backend bugs.

#[path = "common/rvgen.rs"]
mod rvgen;

use bridgev::interp::exec_block;
use bridgev::ir::eval::eval;
use bridgev::ir::lift::{LiftOptions, lift, lift_with};
use bridgev::ir::ops::Block;
use bridgev::ir::opt;
use proptest::prelude::*;
use rvgen::{CODE, Harness};

type Pass = fn(&mut Block);

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn lift_and_every_pass_match_the_interpreter(code in rvgen::block(), regs in rvgen::regs()) {
        let mut h = Harness::new(&code);
        let want = h.run(&regs, |cpu, mem, insns, ff| exec_block(cpu, mem, insns, ff, false));
        let lifted = lift(&h.insns, h.fetch_fault, CODE);
        let got = h.run(&regs, |cpu, mem, _, _| eval(&lifted, cpu, mem));
        prop_assert_eq!(&got, &want, "lifted IR:\n{}", lifted);
        let passes: [(&str, Pass); 5] = [
            ("forward", opt::forward),
            ("dead_writes", opt::dead_writes),
            ("fold", opt::fold),
            ("dce", opt::dce),
            ("optimize", opt::optimize),
        ];
        let mut cumulative = lifted.clone();
        for (name, pass) in passes {
            // Each pass alone, and all passes cumulatively.
            let mut one = lifted.clone();
            pass(&mut one);
            let got = h.run(&regs, |cpu, mem, _, _| eval(&one, cpu, mem));
            prop_assert_eq!(&got, &want, "after {} alone:\n{}", name, one);
            pass(&mut cumulative);
            let got = h.run(&regs, |cpu, mem, _, _| eval(&cumulative, cpu, mem));
            prop_assert_eq!(&got, &want, "after passes up to {}:\n{}", name, cumulative);
        }
    }

    /// P6.5: the same with F/D instructions lifted inline (D47), under the fast-variant
    /// precondition (FS Dirty, frm = RNE) that the TB guard establishes.
    #[test]
    fn inline_fp_lift_and_every_pass_match_the_interpreter(
        code in rvgen::fp_block(),
        regs in rvgen::regs(),
        st in rvgen::fp_state(),
    ) {
        let st = rvgen::FpState { fs: 3, frm: 0, ..st };
        let mut h = Harness::new(&code);
        let init = |c: &mut bridgev::cpu::state::CpuState| st.apply(c);
        let want = h.run_init(&regs, init, |cpu, mem, insns, ff| exec_block(cpu, mem, insns, ff, false));
        let lifted = lift_with(&h.insns, h.fetch_fault, CODE, LiftOptions { inline_fp: true, fma: true });
        let got = h.run_init(&regs, init, |cpu, mem, _, _| eval(&lifted, cpu, mem));
        prop_assert_eq!(&got, &want, "lifted IR:\n{}", lifted);
        let mut optimized = lifted.clone();
        opt::optimize(&mut optimized);
        let got = h.run_init(&regs, init, |cpu, mem, _, _| eval(&optimized, cpu, mem));
        prop_assert_eq!(&got, &want, "optimized IR:\n{}", optimized);
    }
}
