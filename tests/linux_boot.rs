//! Milestone B (P9.6): boot Linux to a BusyBox shell and talk to it over the emulated UART.
//! Needs the guest images from `tools/fetch-guest-images.sh`; ignored by default (run with
//! `cargo test --release --test linux_boot -- --ignored`; CI's `linux-boot` job does).

mod common;

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

/// OpenSBI as shipped with QEMU (`qemu-system-data`, installed by tools/setup.sh).
const OPENSBI: &str = "/usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin";

fn boot_to_shell(engine: &str) {
    boot_to_shell_with(&Boot {
        engine,
        ..Boot::default()
    });
}

/// A machine configuration for `boot_to_shell_with` (Phase 10 options).
#[derive(Default)]
struct Boot<'a> {
    engine: &'a str,
    /// M-mode firmware instead of the built-in SBI.
    firmware: Option<&'a str>,
    /// Offer Sv48; /proc/cpuinfo must then show it.
    sv48: bool,
    /// A virtio-blk disk image with /hello.txt ("hello from the host").
    disk: Option<&'a Path>,
    /// Number of harts (0 = 1); /proc/cpuinfo must list them all.
    harts: usize,
    /// `--tier` (D63): interpreted runs of a block before it is translated.
    tier: u32,
}

/// Boot with the built-in SBI, or with `firmware` in M-mode, and with Sv48 offered (Phase 10);
/// /proc/cpuinfo must show the paging mode Linux chose.
fn boot_to_shell_with(b: &Boot) {
    let (engine, firmware, sv48, disk) = (b.engine, b.firmware, b.sv48, b.disk);
    let harts = b.harts.max(1);
    let Some((kernel, initrd)) = images() else {
        return;
    };
    if let Some(fw) = firmware
        && !std::path::Path::new(fw).is_file()
    {
        eprintln!("skipping: {fw} not installed (tools/setup.sh)");
        return;
    }
    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bridgev"))
        .args(["boot", "--engine", engine, "--stats", "--kernel"])
        .arg(&kernel)
        .arg("--initrd")
        .arg(&initrd)
        .args(firmware.map(|f| ["--firmware", f]).into_iter().flatten())
        .args(if sv48 { &["--mmu", "sv48"][..] } else { &[] })
        .args(
            disk.map(|d| [OsStr::new("--disk"), d.as_os_str()])
                .into_iter()
                .flatten(),
        )
        .args(["--smp", &harts.to_string()])
        .args(["--tier", &b.tier.to_string()])
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
    if firmware.is_some() {
        wait_for(&out, 0, "OpenSBI v", limit);
    }
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
    // Matched in order: the last hart's block, then its ISA and MMU lines.
    let last_cpu = format!("processor\t: {}", harts - 1);
    cmd(
        "cat /proc/cpuinfo",
        &[&last_cpu, "rv64imafdc", if sv48 { "sv48" } else { "sv39" }],
        &mut at,
    );
    cmd("ls /", &["bin", "proc", "sys"], &mut at);
    cmd("echo $((6 * 7))", &["42"], &mut at);
    if disk.is_some() {
        cmd(
            "mkdir /mnt && mount -t ext2 /dev/vda /mnt && cat /mnt/hello.txt",
            &["hello from the host"],
            &mut at,
        );
        cmd(
            "echo written by the guest > /mnt/new.txt && umount /mnt && echo unmounted",
            &["unmounted"],
            &mut at,
        );
    }
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

/// Phase 10: OpenSBI (M-mode firmware) instead of the built-in SBI; the kernel's SBI calls
/// trap to M-mode and are served by guest code.
#[test]
#[ignore = "needs tools/fetch-guest-images.sh and QEMU's OpenSBI; slow"]
fn linux_boots_via_opensbi_jit() {
    boot_to_shell_with(&Boot {
        engine: "jit",
        firmware: Some(OPENSBI),
        ..Boot::default()
    });
}

#[test]
#[ignore = "needs tools/fetch-guest-images.sh and QEMU's OpenSBI; slow"]
fn linux_boots_via_opensbi_lockstep() {
    boot_to_shell_with(&Boot {
        engine: "lockstep",
        firmware: Some(OPENSBI),
        ..Boot::default()
    });
}

/// Phase 10: the machine offers Sv48 (satp mode 9, `mmu-type = "riscv,sv48"`) and Linux uses
/// four-level page tables.
#[test]
#[ignore = "needs tools/fetch-guest-images.sh; slow"]
fn linux_boots_with_sv48_jit() {
    boot_to_shell_with(&Boot {
        engine: "jit",
        sv48: true,
        ..Boot::default()
    });
}

/// Phase 10: a virtio-blk disk (`--disk`) holding an ext2 file system made on the host: the
/// guest mounts it, reads a file, writes one, unmounts; the host then finds the new file.
#[test]
#[ignore = "needs tools/fetch-guest-images.sh and e2fsprogs; slow"]
fn linux_mounts_a_virtio_disk_jit() {
    if images().is_none() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("bridgev-disk-{}", std::process::id()));
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), "hello from the host\n").unwrap();
    let img = dir.join("disk.img");
    let made = Command::new("mke2fs")
        .args(["-q", "-t", "ext2", "-d"])
        .arg(&root)
        .arg(&img)
        .arg("8M")
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: mke2fs (e2fsprogs) not available");
        return;
    }
    boot_to_shell_with(&Boot {
        engine: "jit",
        disk: Some(&img),
        ..Boot::default()
    });
    let out = Command::new("debugfs")
        .args(["-R", "cat /new.txt"])
        .arg(&img)
        .output()
        .expect("debugfs");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "written by the guest\n"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Phase 10: SMP. Linux brings up 4 harts (built-in SBI HSM hart_start), and /proc/cpuinfo
/// lists them all.
#[test]
#[ignore = "needs tools/fetch-guest-images.sh; slow"]
fn linux_boots_with_4_harts_jit() {
    boot_to_shell_with(&Boot {
        engine: "jit",
        harts: 4,
        ..Boot::default()
    });
}

/// D63: the interpreter tier. Most boot code runs only a few times and is never translated;
/// with 4 harts, code writes seen by one hart must also drop the others' decoded blocks.
#[test]
#[ignore = "needs tools/fetch-guest-images.sh; slow"]
fn linux_boots_tiered_jit() {
    for harts in [1, 4] {
        boot_to_shell_with(&Boot {
            engine: "jit",
            harts,
            tier: 16,
            ..Boot::default()
        });
    }
}

/// SMP through OpenSBI: all harts enter the firmware, which starts the secondaries for Linux.
#[test]
#[ignore = "needs tools/fetch-guest-images.sh and QEMU's OpenSBI; slow"]
fn linux_boots_with_4_harts_via_opensbi_jit() {
    boot_to_shell_with(&Boot {
        engine: "jit",
        firmware: Some(OPENSBI),
        harts: 4,
        ..Boot::default()
    });
}
