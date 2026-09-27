# 22 · Run it yourself

## What you will learn

- why Bridge-V can't run directly on Windows, and the three ways around that
- how to set up **WSL2** (a real Linux inside Windows) and build Bridge-V there, step by step
- how to build, test and benchmark with **no Linux at all**, using GitHub Actions (this is how Phase 12 was done)
- a dozen small experiments, each showing one idea from files 01–23 with real output to look for
- how to run the tests, and what to do when something goes wrong

---

## 0. Why not just run it on Windows?

Bridge-V uses features that only Linux has:
- `memfd_create`, to map the code buffer twice (file 09);
- a `SIGSEGV` signal handler, to turn crashes inside generated code into guest exceptions (file 12);
- Linux system calls, which it passes on for the guest program (file 15);
- a 256 GiB address-space reservation for guest memory (file 11).

Windows has different versions of all of these, so it needs its own port (CLAUDE.md §2 lists it as a non-goal). You have three options:

| Option | What you need | Good for |
|---|---|---|
| **A. WSL2** | Windows 10/11, admin rights once, about 10 GB of disk | running everything yourself, interactively (recommended) |
| **B. GitHub Actions** | nothing but git and your GitHub account | building, testing and benchmarking with no Linux machine at all |
| **C. Claude Code in the cloud** | cloud credit | letting Claude work in the Linux container the project was built in |

---

## 1. Option A: WSL2, a real Linux inside Windows

**WSL2** ("Windows Subsystem for Linux, version 2") runs a real Linux kernel in a small virtual machine that Windows manages for you. It runs on the same x86-64 processor, so Bridge-V runs at full speed inside it.

**Step 1: install Ubuntu.** Open PowerShell **as Administrator** and run:
```powershell
wsl --install -d Ubuntu-24.04
```
Restart when asked. An "Ubuntu" window then opens and asks you to choose a Linux user name and password (they are separate from your Windows login).

**Step 2: basic tools.** In the Ubuntu window:
```bash
sudo apt update && sudo apt install -y git curl build-essential
```

**Step 3: Rust.**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```
You don't need to choose a Rust version. The repository's `rust-toolchain.toml` tells rustup to fetch 1.94 automatically.

**Step 4: get the code.** Clone it **inside the Linux file system** (your home folder `~`), not under `/mnt/c` or `/mnt/d`:
```bash
cd ~
git clone --recursive https://github.com/tan8696/Bridge-V
cd Bridge-V
```
- *Why not `/mnt/d/github/Bridge-V`?* That is your Windows disk, reached through a slow translation layer. Builds there are many times slower, and file permissions behave differently.
- *Why `--recursive`?* It also downloads the three **submodules**: riscv-tests, CoreMark and the SoftFloat library. A submodule is a git repository inside another one.

**Step 5: the project's tools and the build.**
```bash
tools/setup.sh                 # apt: RISC-V cross compiler, QEMU, dtc, ... (asks for your password)
cargo build --release          # a few minutes the first time
tools/build-guests.sh          # the RISC-V test programs → guest/build/*.elf
```

You now have `target/release/bridgev`.

---

## 2. Option B: no Linux at all, through GitHub Actions

GitHub can run commands for you on its own Linux machines, called **runners**, every time you push. Bridge-V has two **workflows** (lists of commands) in `.github/workflows/`:

| Workflow | Runs when | What it does | Where to see results |
|---|---|---|---|
| `ci.yml` | every push to any branch | `cargo fmt --check`, `clippy`, builds the guest programs, all tests, riscv-tests, Linux boots | the repository's **Actions** tab on github.com, or `gh run list` |
| `bench.yml` | a push to a branch named `bench/<anything>` | Linux boot times at several `--tier` values and QEMU, a fully interpreted boot, CoreMark/Dhrystone/fpbench | the run's **Summary** page (tables); `results.json` as a downloadable artifact |

To benchmark your current commit without touching your main branch:
```bash
git push origin HEAD:bench/my-experiment
gh run list --limit 3            # find the run
gh run view <run-id> --web       # open it in the browser
```
To try other configurations, edit the `CONFIGS` lines in `.github/workflows/bench.yml` on that bench branch.

**This is exactly how Phase 12 (file 23) was built.** The code was written on a Windows PC without Linux. Every compile, test and measurement ran on GitHub's runners. Public repositories get these minutes for free.

**Things to know:**
- Runners are **shared virtual machines**. Their speed changes from run to run, so only compare numbers from the same run. `boot-bench.py` takes turns between configurations for this reason.
- Each round trip takes about 5–15 minutes, so check your code carefully before pushing.
- `cargo fmt --check` runs first. If your formatting differs from `rustfmt`'s by a single line, the job stops before the tests. With Rust installed locally, run `cargo fmt` before pushing.

---

## 3. Option C: Claude Code in the cloud

The project was developed in Claude Code cloud sessions: Ubuntu containers with Rust and the tools already present (CLAUDE.md §4, §26). In the Claude desktop app, "Continue in cloud" moves a session there. It uses cloud credit, and a fresh container needs `tools/setup.sh` again.

---

## 4. Experiments (each one shows an idea from the study files)

All commands run from the `Bridge-V` folder in WSL2. `bridgev` means `target/release/bridgev`.

**1. Three engines, same answer** (files 06, 07, 17)
```bash
bridgev run guest/build/hello-O2.elf                    # interpreter (the default engine)
bridgev run --engine jit --stats guest/build/hello-O2.elf
bridgev run --engine lockstep guest/build/hello-O2.elf  # JIT checked block by block
```
Look for the same "Hello" line from all three, and for `TBs translated` in the JIT's `--stats` line.

**2. How much faster is the JIT?** (file 18)
```bash
time bridgev run guest/build/fib-O2.elf 32
time bridgev run --engine jit guest/build/fib-O2.elf 32
```

**3. Chaining on and off** (file 10)
```bash
bridgev run --engine jit --stats guest/build/fib-O2.elf
bridgev run --engine jit --no-chain --stats guest/build/fib-O2.elf
```
Compare "dispatcher entries (… per M guest insns)": about 10 per million with chaining, orders of magnitude more without.

**4. Tiered translation** (file 23)
```bash
bridgev run --engine jit --stats guest/build/hello-O2.elf              # default --tier 32
bridgev run --engine jit --tier 0 --stats guest/build/hello-O2.elf     # translate everything
bridgev run --engine jit --tier 1000000000 --stats guest/build/hello-O2.elf  # translate nothing
```
Compare "TBs translated", and read the `tier 32:` part. It shows how many blocks ran only once.

**5. See the generated x86 code and the IR** (files 03, 07, `docs/WHITEBOARD.md`)
```bash
cargo build --release --features disasm       # readable x86 in the dumps (optional)
bridgev run --engine jit --tier 0 --dump-x86 /tmp/tbs --dump-ir /tmp/tbs guest/build/hello.elf
ls /tmp/tbs | head; cat /tmp/tbs/tb_*.txt | head -40
```
`--tier 0` makes sure every block is translated, even ones that run once.

**6. Every memory access through the software TLB** (file 11)
```bash
bridgev run --engine jit --stats guest/build/fib-O2.elf 30
bridgev run --engine jit --mem softmmu --stats guest/build/fib-O2.elf 30
```

**7. Self-modifying code** (file 13)
```bash
bridgev run --engine jit --stats guest/build/smc-O2.elf
```
Look for code-page writes and invalidated TBs in the `SMC:` part of `--stats`.

**8. A crash, reported exactly** (file 12)
```bash
bridgev run --engine jit --tier 0 --stats guest/build/fault.elf; echo "exit code $?"
```
It exits with code 139 (a segmentation fault) and reports cause 7, a store access fault. It is the same report the interpreter gives. `--tier 0` makes the faulting store run as JIT code, so the fault really happens inside generated code.

**9. Boot Linux** (file 16)
```bash
tools/fetch-guest-images.sh      # downloads Ubuntu's riscv64 kernel and busybox (pinned checksums)
bridgev boot --kernel guest/build/linux/Image --initrd guest/build/linux/rootfs.cpio --stats
```
At the `/ #` prompt, type `uname -a`, `cat /proc/cpuinfo`, `ls /`, then `poweroff -f`. Try `--smp 4`, or `--engine interp` to feel the difference.

**10. Debug a RISC-V program with gdb** (file 15)
```bash
sudo apt install -y gdb-multiarch
bridgev run --gdb 1234 guest/build/fib-O2.elf &
gdb-multiarch -ex "set architecture riscv:rv64" -ex "file guest/build/fib-O2.elf" \
    -ex "target remote :1234" -ex "break main" -ex "continue"
```

**11. Benchmarks** (file 18)
```bash
tools/build-bench.sh
python3 tools/bench.py --quick                               # a short smoke run of the matrix
python3 tools/boot-bench.py --configs jit-tier0,jit,qemu     # boot time: no tier, default tier, QEMU
```

---

## 5. Running the tests

```bash
tools/build-riscv-tests.sh                 # the official riscv-tests (once)
cargo test                                 # everything except the slow Linux boots
cargo test --test cli                      # one test file
cargo test --test riscv_tests tiered       # tests whose name contains "tiered"
cargo test --release --test linux_boot -- --ignored --test-threads 1   # the boots (after fetch-guest-images)
```
Some tests skip themselves locally if their guest programs are missing (CI sets `BRIDGEV_REQUIRE_GUESTS=1` to turn that into a failure).

---

## 6. When something goes wrong

| Symptom | Fix |
|---|---|
| `SoftFloat submodule missing: run git submodule update --init --recursive` | Run that command. You cloned without `--recursive`. Phase 12's first benchmark run on GitHub hit exactly this. |
| `tools/setup.sh` fails | `sudo apt update` first; check the internet connection. |
| Everything is very slow | Is the clone under `/mnt/c` or `/mnt/d`? Move it to `~`. |
| `--trace is only supported with --engine=interp` | Tracing each instruction is an interpreter feature. |
| CI fails at `cargo fmt` | Run `cargo fmt` locally and commit the result. |
| Git shows every line as changed | Windows line endings (CRLF). Work inside WSL2 and set `git config core.autocrlf input` there. |

---

## Check yourself

1. Name two Linux features Bridge-V depends on, and which study file explains each.
2. Why should you clone the repository into `~` rather than `/mnt/d` in WSL2?
3. What does pushing to a branch called `bench/test` do? Where do you read the results?
4. Why must numbers from two different GitHub runs not be compared directly?
5. In experiment 4, what do you expect "TBs translated" to show for the three `--tier` values, and why?
6. Why do experiments 5 and 8 add `--tier 0`?
