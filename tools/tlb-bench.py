#!/usr/bin/env python3
"""TLB microbenchmark harness (P7.9). Three configurations, each pinned to one CPU, --runs
times after one warm-up:
  sv39     guest/build/bench/tlbbench.riscv, bare metal, S-mode with Sv39 (misses walk 3 levels)
  direct   guest/build/bench/tlbuser-rv64.elf, user mode, --mem=direct (raw host loads)
  softmmu  the same, --mem=softmmu (inline TLB; misses fill with identity translation)
Prints the median (min-max) ns per access of each kernel (baseline loop subtracted), plus
cycles at the host's nominal TSC frequency (from /proc/cpuinfo; the core clock may differ).

Build first: cargo build --release && tools/build-bench.sh
"""
import argparse, os, re, statistics, subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CONFIGS = {
    "sv39": ["--mode", "bare", "--max-insns", "10000000000", "guest/build/bench/tlbbench.riscv"],
    "direct": ["--mem", "direct", "guest/build/bench/tlbuser-rv64.elf"],
    "softmmu": ["--mem", "softmmu", "guest/build/bench/tlbuser-rv64.elf"],
}


def run(cfg, cpu):
    args = CONFIGS[cfg]
    cmd = (["taskset", "-c", str(cpu)] if cpu >= 0 else []) + [
        os.path.join(ROOT, "target/release/bridgev"), "run", "--engine", "jit"] + args[:-1] + [
        os.path.join(ROOT, args[-1])]
    p = subprocess.run(cmd, capture_output=True, text=True, check=True)
    out = p.stdout
    if cfg == "sv39" and "PASS" not in out + p.stderr:
        raise SystemExit(f"tlbbench did not pass:\n{out}{p.stderr}")
    res = {}
    for m in re.finditer(r"^(\w+)\s+W=(\d+): (?:.* )?(-?\d+\.\d+) ns/access", out, re.M):
        res[f"{m.group(1)} W={m.group(2)}"] = float(m.group(3))
    if len(res) != 4:
        raise SystemExit(f"unexpected output:\n{out}")
    return res

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--cpu", type=int, default=2)
    a = ap.parse_args()
    res = {}
    for cfg in CONFIGS:
        run(cfg, a.cpu)  # warm-up
        res[cfg] = [run(cfg, a.cpu) for _ in range(a.runs)]
    model = next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), "?")
    ghz = re.search(r"@\s*([\d.]+)GHz", model)
    ghz = float(ghz.group(1)) if ghz else None
    print(f"Host: {model}; {a.runs} runs after 1 warm-up, median (min–max); cycles at {ghz} GHz nominal.")
    print("\n| config | kernel | ns/access | cycles/access |\n|---|---|---:|---:|")
    for cfg, runs in res.items():
        for k in runs[0]:
            v = [r[k] for r in runs]
            med = statistics.median(v)
            cyc = f"{med * ghz:.1f}" if ghz else "?"
            print(f"| {cfg} | {k} | {med:.2f} ({min(v):.2f}–{max(v):.2f}) | {cyc} |")

if __name__ == "__main__":
    main()
