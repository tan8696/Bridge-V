//! Official riscv-tests under the interpreter (P1.13): every test of the Phase 1 suites must
//! write `tohost = 1`. The ELFs are built by `tools/build-riscv-tests.sh` and validated against
//! QEMU in Phase 0 (D20).

mod common;

use bridgev::system::bare::{self, BareResult};

/// Suites that must pass under the interpreter (`p` = physical-memory environment).
const SUITES: &[&str] = &["rv64ui-p-", "rv64um-p-", "rv64ua-p-", "rv64uc-p-"];

#[test]
fn phase1_suites_pass_under_interpreter() {
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
    let mut failures = Vec::new();
    for name in &names {
        let data = std::fs::read(dir.join(name)).unwrap();
        match bare::run(&data, 10_000_000, false) {
            Ok((BareResult::Pass, _)) => {}
            other => failures.push(format!("{name}: {other:?}")),
        }
    }
    eprintln!("{} riscv-tests run, {} failed", names.len(), failures.len());
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
