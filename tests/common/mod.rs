//! Shared helpers for integration tests.

// Each integration test crate compiles this module separately and uses a different subset of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository root (the crate manifest directory).
pub fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Path of a guest program built by `tools/build-guests.sh`, e.g. `guest_elf("hello")`.
///
/// Returns `None` when the file is missing and `BRIDGEV_REQUIRE_GUESTS` is unset, so a local
/// `cargo test` without a RISC-V toolchain skips instead of failing. In CI (where the variable
/// is set to `1`) a missing guest is a hard failure.
pub fn guest_elf(name: &str) -> Option<PathBuf> {
    let path = repo_root().join("guest/build").join(format!("{name}.elf"));
    if path.is_file() {
        return Some(path);
    }
    if std::env::var("BRIDGEV_REQUIRE_GUESTS").as_deref() == Ok("1") {
        panic!(
            "missing guest program {}: run tools/build-guests.sh",
            path.display()
        );
    }
    eprintln!(
        "skipping: {} not built (run tools/build-guests.sh)",
        path.display()
    );
    None
}

/// Output of one `bridgev` invocation.
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

/// Run the `bridgev` binary built by Cargo for this test run.
pub fn run_bridgev<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::new(env!("CARGO_BIN_EXE_bridgev"))
        .args(args)
        .output()
        .expect("failed to spawn bridgev");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
    }
}
