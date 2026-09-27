//! Milestone B (P9.6): boot Linux to a BusyBox shell and talk to it over the emulated UART.
//! Needs the guest images from `tools/fetch-guest-images.sh`; ignored by default (run with
//! `cargo test --release --test linux_boot -- --ignored`; CI's `linux-boot` job does).

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn images() -> Option<(PathBuf, PathBuf)> {
    let dir = common::repo_root().join("guest/build/linux");
    let (k, i) = (dir.join("Image"), dir.join("rootfs.cpio"));
    if k.is_file() && i.is_file() {
        Some((k, i))
    } else {
        eprintln!("skipping: run tools/fetch-guest-images.sh");
        None
    }
}

/// Wait until the console output contains `pat` (after byte offset `from`); returns the
/// offset just past it.
fn wait_for(out: &Arc<Mutex<Vec<u8>>>, from: usize, pat: &str, timeout: Duration) -> usize {
    let start = Instant::now();
    loop {
        {
            let o = out.lock().unwrap();
            let hay = &o[from.min(o.len())..];
            if let Some(i) = hay.windows(pat.len()).position(|w| w == pat.as_bytes()) {
                return from + i + pat.len();
            }
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {pat:?}; console so far:\n{}",
            String::from_utf8_lossy(&out.lock().unwrap())
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn boot_to_shell(engine: &str) {
    let Some((kernel, initrd)) = images() else {
        return;
    };
    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bridgev"))
        .args(["boot", "--engine", engine, "--stats", "--kernel"])
        .arg(&kernel)
        .arg("--initrd")
        .arg(&initrd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridgev boot");
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut stdout = child.stdout.take().unwrap();
    let o2 = out.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            o2.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let mut stdin = child.stdin.take().unwrap();
    let limit = Duration::from_secs(if engine == "jit" { 300 } else { 900 });
    let mut at = wait_for(&out, 0, "/ # ", limit);
    let to_shell = t0.elapsed();
    let mut cmd = |c: &str, expect: &[&str], at: &mut usize| {
        stdin.write_all(format!("{c}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
        for e in expect {
            *at = wait_for(&out, *at, e, limit);
        }
        *at = wait_for(&out, *at, "/ # ", limit);
    };
    cmd("uname -a", &["Linux", "riscv64"], &mut at);
    cmd(
        "cat /proc/cpuinfo",
        &["processor", "rv64imafdc", "sv39"],
        &mut at,
    );
    cmd("ls /", &["bin", "proc", "sys"], &mut at);
    cmd("echo $((6 * 7))", &["42"], &mut at);
    stdin.write_all(b"poweroff -f\n").unwrap();
    stdin.flush().unwrap();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(t0.elapsed() < limit * 2, "no poweroff");
        std::thread::sleep(Duration::from_millis(50));
    };
    reader.join().unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    eprintln!(
        "{engine}: shell after {:.1} s; {}",
        to_shell.as_secs_f64(),
        err.lines()
            .filter(|l| l.contains("bridgev:"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    assert!(status.success(), "exit {status:?}\n{err}");
    assert!(err.contains("PowerOff"), "{err}");
}

#[test]
#[ignore = "needs tools/fetch-guest-images.sh; slow"]
fn linux_boots_to_busybox_shell_jit() {
    boot_to_shell("jit");
}

#[test]
#[ignore = "needs tools/fetch-guest-images.sh; slow"]
fn linux_boots_to_busybox_shell_interp() {
    boot_to_shell("interp");
}

/// The whole boot and shell session under `--engine lockstep`: every TB is checked against the
/// interpreter, device accesses included (D52).
#[test]
#[ignore = "needs tools/fetch-guest-images.sh; slow"]
fn linux_boots_to_busybox_shell_lockstep() {
    boot_to_shell("lockstep");
}
