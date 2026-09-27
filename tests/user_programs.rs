//! Linux user-mode programs under every engine (P1.14–P1.16, P2.7, P2.8): every guest program
//! built by `tools/build-guests.sh` must produce byte-identical stdout and the same exit status
//! as `qemu-riscv64` did when `tests/data/expected/` was recorded (D20).

mod common;

use std::path::Path;

fn expected(key: &str) -> (Vec<u8>, i32, Vec<String>) {
    let dir = common::repo_root().join("tests/data/expected");
    let out = std::fs::read(dir.join(format!("{key}.out"))).unwrap();
    let code = std::fs::read_to_string(dir.join(format!("{key}.code")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let args = std::fs::read_to_string(dir.join(format!("{key}.args")))
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();
    (out, code, args)
}

fn check(engine: &[&str], elf: &Path, key: &str) -> Result<(), String> {
    let (want_out, want_code, args) = expected(key);
    let mut cmd: Vec<String> = vec!["run".to_string()];
    cmd.extend(engine.iter().map(|s| s.to_string()));
    cmd.push(elf.to_string_lossy().into_owned());
    cmd.extend(args);
    let out = common::run_bridgev(&cmd);
    if out.stdout.as_bytes() != want_out.as_slice() || out.code != Some(want_code) {
        return Err(format!(
            "{} {engine:?}: exit {:?} (want {want_code})\n--- stdout ---\n{}\n--- expected ---\n{}\n--- stderr ---\n{}",
            elf.display(),
            out.code,
            out.stdout,
            String::from_utf8_lossy(&want_out),
            out.stderr
        ));
    }
    Ok(())
}

fn run_all(engine: &[&str]) {
    let dir = common::repo_root().join("guest/build");
    let mut elfs: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "elf"))
                .collect()
        })
        .unwrap_or_default();
    if elfs.is_empty() {
        common::guest_elf("hello"); // skip locally, fail in CI
        return;
    }
    elfs.sort();
    let mut failures = Vec::new();
    for elf in &elfs {
        let stem = elf.file_stem().unwrap().to_string_lossy().into_owned();
        // asm programs are "<name>.elf"; C programs are "<name>-O{0,2}[-nc].elf".
        let key = match stem.split_once("-O") {
            Some((name, _)) => name.to_string(),
            None => format!("asm-{stem}"),
        };
        if let Err(e) = check(engine, elf, &key) {
            failures.push(e);
        }
    }
    eprintln!(
        "{engine:?}: {} guest programs run, {} failed",
        elfs.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn guest_programs_match_qemu_reference() {
    run_all(&[]);
}

#[test]
fn guest_programs_match_qemu_reference_jit() {
    run_all(&["--engine", "jit"]);
}

#[test]
fn guest_programs_match_qemu_reference_lockstep() {
    run_all(&["--engine", "lockstep"]);
}

#[test]
fn guest_programs_match_qemu_reference_no_chain() {
    run_all(&["--engine", "jit", "--no-chain"]);
    run_all(&["--engine", "lockstep", "--no-chain"]);
}

/// A 64 KiB code cache fills up repeatedly: full flushes must reset links and the jump cache.
#[test]
fn guest_programs_match_qemu_reference_small_code_cache() {
    run_all(&["--engine", "jit", "--code-cache", "64K"]);
}

#[test]
fn guest_programs_match_qemu_reference_jit_mprotect_baseline() {
    run_all(&[
        "--engine",
        "jit",
        "--wx",
        "mprotect",
        "--no-host-features",
        "--max-block",
        "7",
    ]);
}

/// P1.14: parse the initial stack back and check argc/argv/envp/auxv.
/// P4.7: every `--regalloc` level (linear is the default above), and linear without pinning.
#[test]
fn guest_programs_match_qemu_reference_regalloc_levels() {
    for level in ["none", "pinned"] {
        run_all(&["--engine", "jit", "--regalloc", level]);
        run_all(&["--engine", "lockstep", "--regalloc", level]);
    }
    run_all(&["--engine", "lockstep", "--pin", ""]);
}

/// `--mem=softmmu` (D48): every access through the software TLB, under every engine.
#[test]
fn guest_programs_match_qemu_reference_softmmu() {
    run_all(&["--mem", "softmmu"]);
    run_all(&["--engine", "jit", "--mem", "softmmu"]);
    run_all(&["--engine", "lockstep", "--mem", "softmmu"]);
    run_all(&["--engine", "jit", "--mem", "softmmu", "--regalloc", "none"]);
}

#[test]
fn initial_stack_layout() {
    use bridgev::mem::{GuestVirt, prot};
    use bridgev::user::loader;
    let Some(path) = common::guest_elf("hello-O2") else {
        return;
    };
    let args: Vec<String> = ["prog", "a", "bc"].map(String::from).to_vec();
    let envs = vec!["X=1".to_string()];
    let p = loader::load(&path, &args, &envs).unwrap();
    let sp = p.cpu.x[2];
    assert_eq!(sp % 16, 0, "sp must be 16-byte aligned");
    let word = |a: u64| p.mem.load(a, 8).unwrap();
    // Byte by byte: the strings sit right below the top of the stack mapping.
    let cstr = |a: u64| {
        let bytes: Vec<u8> = (a..)
            .map(|x| p.mem.load(x, 1).unwrap() as u8)
            .take_while(|&b| b != 0)
            .collect();
        String::from_utf8(bytes).unwrap()
    };
    assert_eq!(word(sp), 3, "argc");
    let got: Vec<String> = (0..3).map(|i| cstr(word(sp + 8 + 8 * i))).collect();
    assert_eq!(got, args);
    assert_eq!(word(sp + 32), 0, "argv terminator");
    assert_eq!(cstr(word(sp + 40)), "X=1");
    assert_eq!(word(sp + 48), 0, "envp terminator");
    let mut aux = std::collections::HashMap::new();
    let mut a = sp + 56;
    loop {
        let (k, v) = (word(a), word(a + 8));
        if k == 0 {
            break;
        }
        aux.insert(k, v);
        a += 16;
    }
    let elf_data = std::fs::read(&path).unwrap();
    let elf = bridgev::elf::Elf::parse(&elf_data).unwrap();
    assert_eq!(aux[&6], 4096, "AT_PAGESZ");
    assert_eq!(aux[&9], elf.entry, "AT_ENTRY");
    assert_eq!(aux[&5], elf.phnum as u64, "AT_PHNUM");
    assert_eq!(aux[&16], 0x112d, "AT_HWCAP = IMAFDC");
    // AT_PHDR points at the loaded program headers: the first one's p_type matches.
    let first_type = p.mem.load(aux[&3], 4).unwrap() as u32;
    assert_eq!(first_type, elf.segments[0].p_type, "AT_PHDR");
    assert!(
        p.mem.slice(GuestVirt(aux[&25]), 16, prot::R).is_ok(),
        "AT_RANDOM readable"
    );
    assert!(cstr(aux[&31]).ends_with("hello-O2.elf"), "AT_EXECFN");
    assert_eq!(p.cpu.pc, elf.entry);
}
