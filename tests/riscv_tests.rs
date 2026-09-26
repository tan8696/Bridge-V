//! Official riscv-tests under every engine (P1.13, P2.7, P2.8): every test of the Phase 1
//! suites must write `tohost = 1`. The ELFs are built by `tools/build-riscv-tests.sh` and
//! validated against QEMU in Phase 0 (D20).

mod common;

use bridgev::jit::{EngineKind, JitOptions, RegAlloc};
use bridgev::system::bare::{self, BareOptions, BareResult};

/// Suites that must pass under the interpreter (`p` = physical-memory environment).
const SUITES: &[&str] = &[
    "rv64ui-p-",
    "rv64um-p-",
    "rv64ua-p-",
    "rv64uc-p-",
    "rv64uf-p-",
    "rv64ud-p-",
];

fn run_suites(engine: EngineKind, jit: JitOptions) {
    let dir = common::repo_root().join("guest/build/riscv-tests");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        common::guest_elf("riscv-tests/missing"); // skip locally, fail in CI
        return;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| SUITES.iter().any(|s| n.starts_with(s)))
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no riscv-tests found in {}",
        dir.display()
    );
    let opts = BareOptions {
        max_insns: 10_000_000,
        engine,
        jit,
        ..BareOptions::default()
    };
    let mut failures = Vec::new();
    for name in &names {
        let data = std::fs::read(dir.join(name)).unwrap();
        match bare::run(&data, &opts) {
            Ok(r) if r.result == BareResult::Pass => {}
            other => failures.push(format!("{name}: {other:?}")),
        }
    }
    eprintln!(
        "{engine:?}: {} riscv-tests run, {} failed",
        names.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

#[test]
fn phase1_suites_pass_under_interpreter() {
    run_suites(EngineKind::Interp, JitOptions::default());
}

#[test]
fn phase1_suites_pass_under_jit() {
    run_suites(EngineKind::Jit, JitOptions::default());
}

#[test]
fn phase1_suites_pass_under_lockstep() {
    run_suites(EngineKind::Lockstep, JitOptions::default());
}

#[test]
fn phase1_suites_pass_without_chaining() {
    let jit = JitOptions {
        chain: false,
        ..JitOptions::default()
    };
    run_suites(EngineKind::Jit, jit.clone());
    run_suites(EngineKind::Lockstep, jit);
}

/// A tiny slice makes budget exits (and chained re-entry) happen everywhere.
#[test]
fn phase1_suites_pass_with_tiny_slices() {
    let jit = JitOptions {
        slice: 3,
        ..JitOptions::default()
    };
    run_suites(EngineKind::Jit, jit);
}

/// The fallback W^X mode and baseline-only host code must be just as correct.
#[test]
fn phase1_suites_pass_under_jit_mprotect_baseline_small_blocks() {
    let jit = JitOptions {
        wx: bridgev::jit::code_mem::WxMode::Mprotect,
        host_features: false,
        max_block: 3,
        ..JitOptions::default()
    };
    run_suites(EngineKind::Lockstep, jit);
}

/// P4.7: the naive backend (`none`) and the IR backend without optimization or caching
/// (`pinned`), under the JIT and lockstep (`linear` is the default, covered above).
#[test]
fn phase1_suites_pass_at_every_regalloc_level() {
    for regalloc in [RegAlloc::None, RegAlloc::Pinned] {
        let jit = JitOptions {
            regalloc,
            ..JitOptions::default()
        };
        run_suites(EngineKind::Jit, jit.clone());
        run_suites(EngineKind::Lockstep, jit);
    }
}

/// P4.7: the linear allocator with the tiny-slice / small-block stress settings and no pinned
/// registers (every guest register goes through the pool).
#[test]
fn phase1_suites_pass_linear_unpinned_small_blocks() {
    let jit = JitOptions {
        pin: Vec::new(),
        max_block: 3,
        slice: 1,
        ..JitOptions::default()
    };
    run_suites(EngineKind::Jit, jit.clone());
    run_suites(EngineKind::Lockstep, jit);
}
