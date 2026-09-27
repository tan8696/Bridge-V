#!/usr/bin/env python3
"""Linux boot benchmark (P9.7): time from start to the BusyBox `/ # ` prompt, for bridgev
engines and qemu-system-riscv64 (TCG) on the same kernel and initramfs
(guest/build/linux/, from tools/fetch-guest-images.sh). Each run types `poweroff -f` at the
prompt; bridgev's `--stats` line gives the instruction count and MIPS of the whole run.

Configurations:
  jit, interp, lockstep   bridgev boot --engine <e> (built-in SBI)
  <e>-opensbi             with QEMU's OpenSBI fw_dynamic in M-mode (--firmware)
  <e>-sv48, <e>-opensbi-sv48   also offering Sv48 (--mmu sv48)
  <e>-tierN               with --tier N (D63; combines with the options above)
  qemu                    qemu-system-riscv64 -M virt (OpenSBI, QEMU's own devicetree)
  qemu-bvdtb              the same with bridgev's devicetree (same device set as bridgev)
--runs N (default 5) after 1 warm-up per configuration, pinned to --cpu; the configurations
take turns run by run, so host drift affects them alike. Median (min-max). --detail prints
bridgev's statistics line of each configuration's last run. BRIDGEV_BIN selects another build.
"""
import argparse, os, re, select, shutil, statistics, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
IMG = os.path.join(ROOT, "guest/build/linux")
BRIDGEV = os.environ.get("BRIDGEV_BIN") or os.path.join(ROOT, "target/release/bridgev")
OPENSBI = "/usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin"  # qemu-system-data


def cmd_for(cfg):
    kernel, initrd = os.path.join(IMG, "Image"), os.path.join(IMG, "rootfs.cpio")
    if cfg.startswith("qemu"):
        c = ["qemu-system-riscv64", "-M", "virt", "-m", "512M", "-smp", "1", "-nographic",
             "-kernel", kernel, "-initrd", initrd, "-append", "console=ttyS0 earlycon=sbi"]
        if cfg == "qemu-bvdtb":
            dtb = os.path.join(IMG, "bridgev.dtb")
            subprocess.run([BRIDGEV, "boot", "--kernel", kernel, "--initrd", initrd, "--max-insns",
                            "1", "--dump-dtb", dtb], stdin=subprocess.DEVNULL,
                           capture_output=True)
            c += ["-dtb", dtb]
        return c
    engine, *opts = cfg.split("-")
    extra = (["--firmware", OPENSBI] if "opensbi" in opts else []) + \
        (["--mmu", "sv48"] if "sv48" in opts else []) + \
        [arg for o in opts if o.startswith("tier") for arg in ("--tier", o[4:])]
    return [BRIDGEV, "boot", "--engine", engine, "--stats", "--kernel", kernel, "--initrd", initrd] + extra


def run_once(cfg, cpu, timeout):
    cmd = (["taskset", "-c", str(cpu)] if cpu >= 0 else []) + cmd_for(cfg)
    t0 = time.time()
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE)
    os.set_blocking(p.stdout.fileno(), False)
    out, shell = b"", None
    while time.time() - t0 < timeout:
        r, _, _ = select.select([p.stdout], [], [], 0.02)
        if r:
            out += p.stdout.read(65536) or b""
        if shell is None and b"/ # " in out:
            shell = time.time() - t0
            p.stdin.write(b"poweroff -f\n")
            p.stdin.flush()
        if p.poll() is not None:
            break
    if p.poll() is None:
        p.kill()
    err = p.stderr.read().decode(errors="replace")
    m = re.search(r"(\d+) guest instructions in [\d.]+ s \(([\d.]+) MIPS\)", err)
    if shell is None:
        raise SystemExit(f"{cfg}: no shell prompt\n{out.decode(errors='replace')[-3000:]}\n{err}")
    stats = next((l for l in err.splitlines() if "SBI calls" in l), "")
    return {"shell": shell, "total": time.time() - t0,
            "insns": int(m.group(1)) if m else None, "mips": float(m.group(2)) if m else None,
            "exit": p.returncode, "stats": stats}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--configs", default="jit,interp,qemu,qemu-bvdtb")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--cpu", type=int, default=2)
    ap.add_argument("--timeout", type=float, default=600)
    ap.add_argument("--detail", action="store_true", help="print bridgev's statistics lines")
    a = ap.parse_args()
    cpu = a.cpu if shutil.which("taskset") else -1
    model = next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), "?")
    commit = subprocess.run(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    qemu = subprocess.run(["qemu-system-riscv64", "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    print(f"Host: {model}; commit {commit}; {qemu}; {a.runs} runs after 1 warm-up, median (min–max).\n")
    print("| config | time to shell (s) | instructions (whole run) | MIPS |\n|---|---:|---:|---:|")
    cfgs = a.configs.split(",")
    for cfg in cfgs:
        run_once(cfg, cpu, a.timeout)
    runs = {cfg: [] for cfg in cfgs}
    for _ in range(a.runs):
        for cfg in cfgs:
            runs[cfg].append(run_once(cfg, cpu, a.timeout))
    for cfg, rs in runs.items():
        sh = [r["shell"] for r in rs]
        insns = [r["insns"] for r in rs if r["insns"]]
        mips = [r["mips"] for r in rs if r["mips"]]
        ins = f"{statistics.median(insns):,.0f}" if insns else "—"
        mp = f"{statistics.median(mips):.0f}" if mips else "—"
        print(f"| {cfg} | {statistics.median(sh):.2f} ({min(sh):.2f}–{max(sh):.2f}) | {ins} | {mp} |",
              flush=True)
    if a.detail:
        for cfg, rs in runs.items():
            if rs[-1]["stats"]:
                print(f"\n{cfg}: {rs[-1]['stats']}")


if __name__ == "__main__":
    sys.exit(main())
