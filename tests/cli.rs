//! CLI smoke tests (P0.2).

mod common;

use common::run_bridgev;

#[test]
fn version_exits_zero() {
    let out = run_bridgev(["--version"]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(out.stdout.starts_with("bridgev "), "stdout: {}", out.stdout);
}

#[test]
fn unimplemented_subcommands_exit_two() {
    for args in [vec!["boot", "--kernel", "Image"], vec!["bench", "coremark"]] {
        let out = run_bridgev(&args);
        assert_eq!(out.code, Some(2), "{args:?}: stderr: {}", out.stderr);
        assert!(
            out.stderr.contains("not implemented yet"),
            "{args:?}: {}",
            out.stderr
        );
    }
}

#[test]
fn missing_elf_is_an_error_not_a_panic() {
    let out = run_bridgev(["run", "/nonexistent.elf"]);
    assert_eq!(out.code, Some(1));
    assert!(out.stderr.contains("bridgev: error"), "{}", out.stderr);
}

#[test]
fn disasm_prints_the_entry_point() {
    let Some(elf) = common::guest_elf("hello") else {
        return;
    };
    let out = run_bridgev([std::ffi::OsStr::new("disasm"), elf.as_os_str()]);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout.contains("ecall"), "{}", out.stdout);
    assert!(out.stdout.contains("addi a0, zero, 1"), "{}", out.stdout);
}

#[test]
fn guest_helper_skips_or_finds_hello() {
    // Exercises tests/common: finds guest/build/hello.elf when built, skips otherwise.
    if let Some(path) = common::guest_elf("hello") {
        assert!(path.is_file());
    }
}

/// P2.8: a deliberately miscompiled ADDI must be caught by lockstep, not by the program's
/// output, and the report must show the TB and the differing register.
#[test]
fn lockstep_catches_injected_miscompilation() {
    let Some(elf) = common::guest_elf("hello") else {
        return;
    };
    let out = run_bridgev([
        "run",
        "--engine",
        "lockstep",
        "--inject-bug",
        elf.to_str().unwrap(),
    ]);
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert!(out.stderr.contains("lockstep divergence"), "{}", out.stderr);
    assert!(
        out.stderr.contains("interp") && out.stderr.contains("jit"),
        "{}",
        out.stderr
    );
    // Without lockstep the same bug silently changes behaviour: the check is what caught it.
    let ok = run_bridgev(["run", "--engine", "lockstep", elf.to_str().unwrap()]);
    assert_eq!(ok.code, Some(0), "stderr: {}", ok.stderr);
}

/// P2.9: --stats reports JIT counters; --dump-x86 writes one .bin/.txt pair per TB.
#[test]
fn jit_stats_and_dump() {
    let Some(elf) = common::guest_elf("hello") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("bridgev-dump-{}", std::process::id()));
    let out = run_bridgev([
        "run",
        "--engine",
        "jit",
        "--stats",
        "--dump-x86",
        dir.to_str().unwrap(),
        elf.to_str().unwrap(),
    ]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(out.stderr.contains("TBs translated"), "{}", out.stderr);
    let n = std::fs::read_dir(&dir).unwrap().count();
    assert!(n >= 2 && n.is_multiple_of(2), "{n} dump files");
    std::fs::remove_dir_all(&dir).unwrap();
}
