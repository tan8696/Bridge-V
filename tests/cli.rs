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
    for args in [vec!["boot", "--kernel", "Image"]] {
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
/// output, and the report must show the TB and the differing register. Runs at every
/// `--regalloc` level; `fib-O2` (C) keeps non-constant ADDIs after IR constant folding, which
/// the asm `hello` does not.
#[test]
fn lockstep_catches_injected_miscompilation() {
    let Some(elf) = common::guest_elf("fib-O2") else {
        return;
    };
    let elf = elf.to_str().unwrap();
    for level in ["none", "pinned", "linear"] {
        let out = run_bridgev([
            "run",
            "--engine",
            "lockstep",
            "--regalloc",
            level,
            "--inject-bug",
            elf,
            "21",
        ]);
        assert_eq!(out.code, Some(1), "{level}: stderr: {}", out.stderr);
        assert!(
            out.stderr.contains("lockstep divergence"),
            "{level}: {}",
            out.stderr
        );
        assert!(
            out.stderr.contains("interp") && out.stderr.contains("jit"),
            "{level}: {}",
            out.stderr
        );
        // Without the injected bug the same run is clean: the check is what caught it.
        let ok = run_bridgev([
            "run",
            "--engine",
            "lockstep",
            "--regalloc",
            level,
            elf,
            "21",
        ]);
        assert_eq!(ok.code, Some(0), "{level}: stderr: {}", ok.stderr);
    }
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

/// P2.7: a guest store to its read-only text faults on the host inside JIT code; the SIGSEGV
/// handler and pcmap must report exactly what the interpreter reports (cause, tval, pc, and the
/// retired-instruction count), and the JIT must have taken the host-fault path.
#[test]
fn jit_host_fault_is_precise() {
    let Some(elf) = common::guest_elf("fault") else {
        return;
    };
    let run = |engine: &str| {
        let out = run_bridgev(["run", "--engine", engine, "--stats", elf.to_str().unwrap()]);
        assert_eq!(out.code, Some(139), "{engine}: {}", out.stderr);
        let lines: Vec<String> = out.stderr.lines().map(String::from).collect();
        (
            lines[0].clone(),
            lines[1].split(" in ").next().unwrap().to_string(),
            out.stderr,
        )
    };
    let (fault_i, count_i, _) = run("interp");
    assert!(fault_i.contains("cause 7"), "{fault_i}");
    for engine in ["jit", "lockstep"] {
        let (fault_j, count_j, all) = run(engine);
        assert_eq!(fault_j, fault_i, "{engine}");
        assert_eq!(count_j, count_i, "{engine}");
        assert!(all.contains("host-fault 1"), "{engine}: {all}");
    }
}

/// Retired-instruction count and statistics from `--stats`.
fn icount_and_stats(args: &[&str]) -> (u64, String) {
    let out = run_bridgev(args);
    assert!(out.code.is_some(), "{args:?}: {}", out.stderr);
    let line = out
        .stderr
        .lines()
        .find(|l| l.contains("guest instructions in"))
        .unwrap_or_else(|| panic!("{args:?}: no stats in {}", out.stderr));
    let n = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    (n, out.stderr)
}

/// P3.3: the budget-derived instruction count is exactly the interpreter's, with and without
/// chaining and with tiny code caches (every exit, flush and helper refund is accounted for).
#[test]
fn icount_is_exact_under_every_engine() {
    for prog in [
        "hello",
        "qsort-O2",
        "printf_float-O0",
        "fib-O2",
        "fault",
        "setjmp-O2-nc",
    ] {
        let Some(elf) = common::guest_elf(prog) else {
            return;
        };
        let e = elf.to_str().unwrap();
        let (want, _) = icount_and_stats(&["run", "--stats", e]);
        for cfg in [
            &["--engine", "jit"][..],
            &["--engine", "jit", "--no-chain"],
            &["--engine", "jit", "--code-cache", "64K"],
            &["--engine", "lockstep"],
        ] {
            let mut args = vec!["run", "--stats"];
            args.extend_from_slice(cfg);
            args.push(e);
            assert_eq!(icount_and_stats(&args).0, want, "{prog} {cfg:?}");
        }
    }
}

/// P3.4: recursive fib returns through the jump cache; the hit rate must exceed 90%, and
/// chaining must cut dispatcher entries by orders of magnitude.
#[test]
fn fib_jump_cache_hit_rate_and_dispatcher_entries() {
    let Some(elf) = common::guest_elf("fib-O2") else {
        return;
    };
    let e = elf.to_str().unwrap();
    let entries = |s: &str| -> u64 {
        let i = s.find(" dispatcher entries").unwrap();
        s[..i].rsplit(' ').next().unwrap().parse().unwrap()
    };
    let (_, chained) = icount_and_stats(&["run", "--engine", "jit", "--profile-jit", "--stats", e]);
    let rate: f64 = chained
        .split("hit rate ")
        .nth(1)
        .and_then(|r| r.split('%').next())
        .unwrap()
        .parse()
        .unwrap();
    assert!(rate > 90.0, "jump-cache hit rate {rate}%");
    let (_, unchained) = icount_and_stats(&["run", "--engine", "jit", "--no-chain", "--stats", e]);
    assert!(
        entries(&chained) * 100 < entries(&unchained),
        "{chained}\n{unchained}"
    );
}

/// P4.9: `--stats=regs` prints the register-use histogram under the interpreter, and is
/// rejected by engines that cannot collect it.
#[test]
fn stats_regs_histogram() {
    let Some(elf) = common::guest_elf("fib-O2") else {
        return;
    };
    let e = elf.to_str().unwrap();
    let out = run_bridgev(["run", "--stats=regs", e, "22"]);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stderr.contains("| x2 (sp) |"), "{}", out.stderr);
    assert!(
        out.stderr.contains("dynamic share of x2,x1,x10,x15"),
        "{}",
        out.stderr
    );
    let jit = run_bridgev(["run", "--engine", "jit", "--stats=regs", e, "22"]);
    assert_eq!(jit.code, Some(1));
    assert!(
        jit.stderr.contains("--stats=regs needs --engine interp"),
        "{}",
        jit.stderr
    );
    // Plain --stats still works and takes no value from the next argument.
    let plain = run_bridgev(["run", "--stats", e, "22"]);
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    assert!(!plain.stderr.contains("register uses"));
}
