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
        vec!["run", "x.elf"],
        vec!["boot", "--kernel", "Image"],
        vec!["disasm", "x.elf"],
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
fn guest_helper_skips_or_finds_hello() {
    // Exercises tests/common: finds guest/build/hello.elf when built, skips otherwise.
    if let Some(path) = common::guest_elf("hello") {
        assert!(path.is_file());
    }
}
