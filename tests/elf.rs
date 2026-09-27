//! ELF loader integration tests (P1.2): every built guest program and riscv-test parses, and
//! random input never panics.

mod common;

use bridgev::elf::Elf;
use proptest::prelude::*;

#[test]
fn parses_all_guest_programs() {
    let root = common::repo_root().join("guest/build");
    let mut dirs = vec![root.clone(), root.join("riscv-tests")];
    let mut n = 0;
    while let Some(dir) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() || p.extension().is_some_and(|e| e != "elf") {
                continue;
            }
            let data = std::fs::read(&p).unwrap();
            let elf = Elf::parse(&data).unwrap_or_else(|e| panic!("{}: {e:#}", p.display()));
            assert!(elf.loads().count() > 0, "{}: no PT_LOAD", p.display());
            // Only the `-dyn` builds (Phase 10) name a program interpreter.
            let dynamic = p.to_string_lossy().ends_with("-dyn.elf");
            assert_eq!(
                elf.interp.is_some(),
                dynamic,
                "{}: interpreter {:?}",
                p.display(),
                elf.interp
            );
            if p.to_string_lossy().contains("riscv-tests") {
                assert!(elf.symbol("tohost").is_some(), "{}: no tohost", p.display());
                assert_eq!(elf.entry, 0x8000_0000, "{}", p.display());
            }
            n += 1;
        }
    }
    if n == 0 {
        // Same policy as common::guest_elf: skip locally, fail in CI.
        common::guest_elf("hello");
    }
    eprintln!("parsed {n} guest ELFs");
}

proptest! {
    #[test]
    fn random_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = Elf::parse(&data);
    }

    #[test]
    fn corrupted_header_never_panics(pos in 0usize..120, byte in any::<u8>()) {
        let mut data = std::fs::read(common::repo_root().join("guest/build/hello.elf"))
            .unwrap_or_else(|_| vec![0; 128]);
        if pos < data.len() { data[pos] = byte; }
        let _ = Elf::parse(&data);
    }
}
