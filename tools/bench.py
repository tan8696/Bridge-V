#!/usr/bin/env python3
"""Bridge-V benchmark harness (P5.2, CLAUDE.md §22).

Runs the benchmark matrix: every workload under every configuration (bridgev interpreter,
four JIT levels, qemu-riscv64, native x86-64). For each cell it:
  1. calibrates the amount of work so one run lasts about --target seconds (CoreMark needs
     >= 10 s for a valid result), and recalibrates from the warm-up run; a measured CoreMark
     run that comes out under 10 s is kept on record as discarded and redone with 25% more work,
  2. does 1 warm-up run and --runs measured runs, pinned to one CPU with taskset,
  3. validates every run (CoreMark: "Correct operation validated" and >= 10 s; Dhrystone: every
     final value matches its "should be" line),
  4. records the program's own score (CoreMark iterations/s, Dhrystones/s), and for bridgev the
     --stats counters (guest instructions, MIPS, translation time, code size, dispatcher entries).
Writes results.json (all raw runs + host info) and results.md (tables) to --out.

Build first: cargo build --release && tools/build-bench.sh
"""

import argparse
import datetime
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BENCH = os.path.join(ROOT, "guest", "build", "bench")
BRIDGEV = os.path.join(ROOT, "target", "release", "bridgev")

# name -> (kind, extra bridgev arguments). kind: bridgev | qemu | native
CONFIGS = {
    "interp": ("bridgev", ["--engine", "interp"]),
    "jit-naive": ("bridgev", ["--engine", "jit", "--regalloc", "none", "--no-chain"]),
    "jit+chain": ("bridgev", ["--engine", "jit", "--regalloc", "none"]),
    "jit+pinned": ("bridgev", ["--engine", "jit", "--regalloc", "pinned"]),
    "jit+linear": ("bridgev", ["--engine", "jit"]),
    "qemu": ("qemu", []),
    "native": ("native", []),
}
# Not yet implemented; shown as n/a in the tables.
PENDING = {"softmmu": "Phase 7 (--mem=softmmu)"}


class Workload:
    """A benchmark program: how to run N units of work, parse and validate the output."""

    def __init__(self, name, rv64, native, unit, min_secs, start, target_factor):
        self.name, self.rv64, self.native = name, rv64, native
        self.unit, self.min_secs, self.start = unit, min_secs, start
        # Run length relative to --target (Dhrystone has no minimum; half the time suffices).
        self.target_factor = target_factor

    def binary(self, kind):
        return os.path.join(BENCH, self.native if kind == "native" else self.rv64)


class CoreMark(Workload):
    def __init__(self):
        super().__init__("coremark", "coremark-rv64.elf", "coremark-native", "iterations", 10.0, 20, 1.0)

    def args(self, n):
        return ["0x0", "0x0", "0x66", str(n)]

    def parse(self, out, n):
        score = re.search(r"Iterations/Sec\s*:\s*([\d.]+)", out)
        secs = re.search(r"Total time \(secs\)\s*:\s*([\d.]+)", out)
        iters = re.search(r"^Iterations\s*:\s*(\d+)", out, re.M)
        errors = []
        if not secs or not iters or "seedcrc" not in out:
            return None, None, ["unparsable CoreMark output"]
        if int(iters.group(1)) != n:
            errors.append(f"ran {iters.group(1)} iterations, asked for {n}")
        # CoreMark checks its list/matrix/state CRCs itself ("ERROR! ... crc ... - should be").
        # Its 10 s rule also counts as an error and suppresses "Correct operation validated", so
        # that line is required only when the 10 s minimum applies (not in --quick runs).
        errors += [l.strip() for l in out.splitlines()
                   if l.startswith("ERROR!") and "at least 10 secs" not in l]
        if self.min_secs >= 10 and "Correct operation validated" not in out:
            errors.append("no 'Correct operation validated'")
        return (float(score.group(1)) if score else None), float(secs.group(1)), errors


class Dhrystone(Workload):
    def __init__(self):
        super().__init__("dhrystone", "dhrystone-rv64.elf", "dhrystone-native", "runs", 0.0, 20000, 0.5)

    def args(self, n):
        return [str(n)]

    def parse(self, out, n):
        errors = []
        score = re.search(r"Dhrystones per Second:\s*(-?\d+)", out)
        # Every "name: value" line followed by "should be: expected" (Weicker's self-check).
        lines = out.splitlines()
        checked = 0
        for i in range(len(lines) - 1):
            m = re.match(r"\s*(\S[^:]*):\s*(.*?)\s*$", lines[i])
            e = re.match(r"\s*should be:\s*(.*?)\s*$", lines[i + 1])
            if not m or not e:
                continue
            want = e.group(1)
            if want.startswith("(implementation-dependent"):
                continue
            if want == "Number_Of_Runs + 10":
                want = str(n + 10)
            checked += 1
            if m.group(2) != want:
                errors.append(f"{m.group(1)} = {m.group(2)!r}, should be {want!r}")
        if checked < 15:
            errors.append(f"only {checked} Dhrystone self-check values found")
        if not score:
            errors.append("unparsable Dhrystone output")
            return None, None, errors
        dps = int(score.group(1))
        if dps <= 0:
            errors.append(f"Dhrystones per Second = {dps}")
        return float(dps), (n / dps if dps > 0 else None), errors


WORKLOADS = {w.name: w for w in (CoreMark(), Dhrystone())}


def command(cfg, w, n, cpu):
    kind, extra = CONFIGS[cfg]
    prog = [w.binary(kind)] + w.args(n)
    if kind == "bridgev":
        cmd = [BRIDGEV, "run"] + extra + ["--stats"] + prog
    elif kind == "qemu":
        cmd = ["qemu-riscv64"] + prog
    else:
        cmd = prog
    if cpu is not None:
        cmd = ["taskset", "-c", str(cpu)] + cmd
    return cmd


def bridgev_stats(err):
    s = {}
    m = re.search(r"bridgev: (\d+) guest instructions in ([\d.]+) s \(([\d.]+) MIPS\)", err)
    if m:
        s["guest_insns"], s["secs"], s["mips"] = int(m.group(1)), float(m.group(2)), float(m.group(3))
    m = re.search(r"(\d+) TBs translated \((\d+) guest insns, \d+ KiB host code, ([\d.]+) bytes/insn\)", err)
    if m:
        s["tbs"], s["tb_guest_insns"], s["bytes_per_insn"] = int(m.group(1)), int(m.group(2)), float(m.group(3))
    m = re.search(r"translate time ([\d.]+) ms", err)
    if m:
        s["translate_ms"] = float(m.group(1))
    m = re.search(r"(\d+) dispatcher entries \((\d+) per M guest insns\)", err)
    if m:
        s["dispatcher_entries"], s["entries_per_m"] = int(m.group(1)), int(m.group(2))
    m = re.search(r"interp: (\d+) blocks decoded", err)
    if m:
        s["blocks_decoded"] = int(m.group(1))
    return s


def run_once(cfg, w, n, cpu):
    cmd = command(cfg, w, n, cpu)
    t0 = time.monotonic()
    p = subprocess.run(cmd, capture_output=True, text=True)
    wall = time.monotonic() - t0
    score, secs, errors = w.parse(p.stdout, n)
    if p.returncode != 0:
        errors.append(f"exit status {p.returncode}")
    too_short = secs is not None and secs < w.min_secs
    if too_short:
        errors.append(f"ran {secs:.2f} s < {w.min_secs} s")
    # Too short and nothing else wrong (CoreMark's 10 s rule also withholds its "validated").
    others = [e for e in errors if not e.startswith("ran ") and "Correct operation" not in e]
    r = {"n": n, "wall": wall, "score": score, "secs": secs, "valid": not errors, "errors": errors,
         "too_short": too_short and not others}
    if CONFIGS[cfg][0] == "bridgev":
        r["stats"] = bridgev_stats(p.stderr)
    if errors:
        r["stdout_tail"] = p.stdout[-2000:]
        r["stderr_tail"] = p.stderr[-2000:]
    return r


def calibrate(cfg, w, target, cpu):
    """Work units for one run of about `target` seconds, from the program's own rate."""
    n = w.start
    while True:
        r = run_once(cfg, w, n, cpu)
        if r["score"] is None and r["secs"] == 0.0:
            n *= 100  # too fast to time at all
            continue
        if r["score"] is None:
            raise SystemExit(f"{w.name}/{cfg}: calibration run failed: {r['errors']}\n"
                             f"{r.get('stdout_tail', '')}{r.get('stderr_tail', '')}")
        elapsed = r["secs"] or r["wall"]
        if elapsed >= 0.5:
            return max(1, math.ceil(r["score"] * target))
        n *= max(2, min(100, math.ceil(0.6 / max(elapsed, 1e-3))))


def summarize(runs):
    ok = [r for r in runs if r["valid"]]
    if not ok:
        return None
    scores = [r["score"] for r in ok]
    s = {"median": statistics.median(scores), "min": min(scores), "max": max(scores),
         "valid_runs": len(ok), "runs": len(runs), "n": ok[0]["n"]}
    if "stats" in ok[0]:
        for key in ("mips", "bytes_per_insn", "entries_per_m", "guest_insns", "translate_ms", "secs"):
            vals = [r["stats"][key] for r in ok if key in r["stats"]]
            if vals:
                s[key] = statistics.median(vals)
        if "translate_ms" in s and "secs" in s:
            s["translate_share"] = s["translate_ms"] / 1000.0 / s["secs"]
        if "guest_insns" in s:
            s["insns_per_unit"] = s["guest_insns"] / s["n"]
    return s


def sh(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, shell=True).stdout.strip()
    except OSError:
        return ""


def host_info(cpu):
    model = ""
    with open("/proc/cpuinfo") as f:
        for line in f:
            if line.startswith("model name"):
                model = line.split(":", 1)[1].strip()
                break
    return {
        "date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "cpu_model": model,
        "nproc": os.cpu_count(),
        "pinned_cpu": cpu,
        "kernel": platform.release(),
        "rustc": sh("rustc --version"),
        "qemu": sh("qemu-riscv64 --version | head -1"),
        "commit": sh(f"git -C {ROOT} rev-parse --short HEAD"),
        "dirty": bool(sh(f"git -C {ROOT} status --porcelain --untracked-files=no")),
        "buildinfo": open(os.path.join(BENCH, "BUILDINFO.txt")).read()
        if os.path.exists(os.path.join(BENCH, "BUILDINFO.txt")) else "",
    }


def fmt(x, digits=0):
    return f"{x:,.{digits}f}"


def cell(s, digits=0):
    if s is None:
        return "invalid"
    return f"{fmt(s['median'], digits)} ({fmt(s['min'], digits)}–{fmt(s['max'], digits)})"


def markdown(res):
    h = res["host"]
    out = [f"Host: {h['cpu_model']} ({h['nproc']} vCPU, pinned to CPU {h['pinned_cpu']}), kernel "
           f"{h['kernel']}, {h['rustc']}, {h['qemu']}. Commit `{h['commit']}`"
           f"{' (dirty tree)' if h['dirty'] else ''}, {h['date']}. {res['params']['runs']} measured "
           f"run(s) after {res['params']['warmup']} warm-up, median (min–max)"
           f"{'; QUICK smoke run, not a measurement' if res['params']['quick'] else ''}. "
           f"Shared cloud VM: expect noise of several percent."]
    for wname, cells in res["results"].items():
        w = WORKLOADS[wname]
        score = "iterations/s" if wname == "coremark" else "Dhrystones/s"
        interp = (cells.get("interp") or {}).get("median")
        native = (cells.get("native") or {}).get("median")
        linear_ipu = (cells.get("jit+linear") or {}).get("insns_per_unit")
        out += ["", f"### {wname}", "",
                f"| config | {score} | vs interp | vs native | guest MIPS | {w.unit}/run | valid |",
                "|---|---:|---:|---:|---:|---:|---:|"]
        if wname == "dhrystone":
            out[-2] = out[-2].replace("| guest MIPS |", "| DMIPS | guest MIPS |")
            out[-1] = "|---|---:|---:|---:|---:|---:|---:|---:|"
        for cfg in list(CONFIGS) + list(PENDING):
            if cfg in PENDING:
                cols = [cfg, f"n/a: {PENDING[cfg]}", "", "", ""] + ([""] if wname == "dhrystone" else [])
                out.append("| " + " | ".join(cols + ["", ""]) + " |")
                continue
            if cfg not in cells:
                continue
            s = cells[cfg]
            if s is None:
                out.append(f"| {cfg} | invalid | | | | | 0 |")
                continue
            vi = f"{s['median'] / interp:.1f}×" if interp else ""
            vn = f"{s['median'] / native:.3f}" if native else ""
            if "mips" in s:
                mips = fmt(s["mips"])
            elif linear_ipu and CONFIGS[cfg][0] == "qemu":
                mips = f"{fmt(s['median'] * linear_ipu / 1e6)} (est.)"
            else:
                mips = "—"
            cols = [cfg, cell(s), vi, vn]
            if wname == "dhrystone":
                cols.append(fmt(s["median"] / 1757.0))
            cols += [mips, fmt(s["n"]), f"{s['valid_runs']}/{s['runs']}"]
            out.append("| " + " | ".join(cols) + " |")
        jit = [c for c in cells if c.startswith("jit") and cells[c]]
        if jit:
            out += ["", "| config | translate time (share of run) | host bytes / guest insn | "
                        "dispatcher entries per M insns |", "|---|---:|---:|---:|"]
            for c in jit:
                s = cells[c]
                out.append(f"| {c} | {s.get('translate_ms', 0):.1f} ms "
                           f"({100 * s.get('translate_share', 0):.3f}%) | "
                           f"{s.get('bytes_per_insn', 0):.1f} | {fmt(s.get('entries_per_m', 0))} |")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", default="coremark,dhrystone", help="comma-separated workloads")
    ap.add_argument("--configs", default=",".join(CONFIGS), help="comma-separated configurations")
    ap.add_argument("--runs", type=int, default=5, help="measured runs per cell")
    ap.add_argument("--warmup", type=int, default=1, help="warm-up runs per cell (not measured)")
    ap.add_argument("--target", type=float, default=13.0, help="seconds per run (CoreMark needs >= 10)")
    ap.add_argument("--cpu", type=int, default=2, help="CPU for taskset (-1: no pinning)")
    ap.add_argument("--quick", action="store_true",
                    help="smoke test: 1 s runs, 1 measured run, no warm-up, CoreMark's 10 s rule off")
    ap.add_argument("--out", default=os.path.join(ROOT, "target", "bench"), help="output directory")
    a = ap.parse_args()
    cpu = None if a.cpu < 0 or not shutil.which("taskset") or a.cpu >= (os.cpu_count() or 1) else a.cpu
    runs, warmup, target = (1, 0, 1.0) if a.quick else (a.runs, a.warmup, a.target)
    for w in WORKLOADS.values():
        if a.quick:
            w.min_secs = 0.0
    for path in (BRIDGEV,):
        if not os.path.exists(path):
            raise SystemExit(f"missing {path}: run cargo build --release")
    res = {"host": host_info(cpu), "params": {"runs": runs, "warmup": warmup, "target": target,
                                             "quick": a.quick}, "results": {}, "raw": {}}
    for wname in a.suite.split(","):
        w = WORKLOADS[wname]
        for kind in ("rv64", "native"):
            p = w.binary("native" if kind == "native" else "bridgev")
            if not os.path.exists(p):
                raise SystemExit(f"missing {p}: run tools/build-bench.sh")
        res["results"][wname], res["raw"][wname] = {}, {}
        for cfg in a.configs.split(","):
            if CONFIGS[cfg][0] == "qemu" and not shutil.which("qemu-riscv64"):
                print(f"{wname}/{cfg}: qemu-riscv64 not installed, skipped", file=sys.stderr)
                continue
            n = calibrate(cfg, w, target * w.target_factor, cpu)
            rs, retried = [], []
            i = 0
            while i < warmup + runs:
                r = run_once(cfg, w, n, cpu)
                if i >= warmup and r["too_short"] and len(retried) < 3:
                    # Only CoreMark's 10 s rule failed (the VM got faster mid-batch): keep the
                    # run on record, redo it with 25% more work.
                    retried.append(r)
                    print(f"{wname:9} {cfg:10} run {i - warmup + 1}/{runs} too short "
                          f"({r['secs']:.2f} s), redone with more work", file=sys.stderr)
                    n = math.ceil(n * 1.25)
                    continue
                if i >= warmup:
                    rs.append(r)
                elif r["score"]:
                    # The warm-up is a full-length run: recalibrate from its steady-state rate
                    # (short calibration runs underestimate it, e.g. cold block decoding).
                    n = max(1, math.ceil(r["score"] * target * w.target_factor))
                tag = "warm-up" if i < warmup else f"run {i - warmup + 1}/{runs}"
                status = "ok" if r["valid"] else "INVALID " + "; ".join(r["errors"])
                print(f"{wname:9} {cfg:10} {tag:9} n={r['n']} score={r['score']} wall={r['wall']:.2f}s {status}",
                      file=sys.stderr, flush=True)
                i += 1
            res["raw"][wname][cfg] = rs
            if retried:
                res["raw"][wname][cfg + " (discarded, too short)"] = retried
            res["results"][wname][cfg] = summarize(rs)
    os.makedirs(a.out, exist_ok=True)
    md = markdown(res)
    with open(os.path.join(a.out, "results.json"), "w") as f:
        json.dump(res, f, indent=1)
    with open(os.path.join(a.out, "results.md"), "w") as f:
        f.write(md)
    print(md)
    bad = [f"{w}/{c}" for w, cells in res["results"].items() for c, s in cells.items()
           if s is None or s["valid_runs"] != s["runs"]]
    if bad:
        print("INVALID runs in: " + ", ".join(bad), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
