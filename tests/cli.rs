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
    for args in [
        vec!["run", "--engine", "jit", "x.elf"],
        vec!["boot", "--kernel", "Image"],
        vec!["bench", "coremark"],
    ] {
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
