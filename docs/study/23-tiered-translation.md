# 23 · Tiered translation: don't translate code that barely runs

## What you will learn

- why translating a block has a real cost, and why **most code in a Linux boot runs only a handful of times**
- the fix: run new code in the **interpreter** first, and translate a block only after it has proven it is "hot"
- the small piece of math that tells you *when* translating pays off (the **break-even point** and the **ski-rental problem**)
- how the feature is built in Bridge-V, function by function (`--tier N`, decision **D63**)
- why it cannot break correctness, how it was tested, and what it measured
- how to explain it in an interview, honestly

Read files 05 (code flow), 06 (interpreter), 07 (JIT pipeline) and 10 (chaining) first. This file builds on all four.

---

## 1. The problem: translation is not free

Remember the JIT pipeline from file 07: **decode → lift to IR → optimize → allocate registers → emit x86 bytes → place in the code cache.** That is a lot of work for one block. Bridge-V measured it: about **8–9 microseconds (µs) per block** (1 µs = one millionth of a second).

After a block is translated, running it is almost free: about **1 nanosecond (ns) per guest instruction** (1 ns = one thousandth of a µs).

So a JIT is a deal: *pay a big price once, then run fast forever.* The deal is great for code that runs millions of times, like the inner loop of CoreMark. There, translation is **0.1%** of the run time (file 18).

But what about code that runs **once**? Then you paid the big price and got almost nothing back.

**How much code runs only once?** When Linux boots, it runs a huge amount of set-up code exactly once: driver initialisation, parsing the devicetree, mounting file systems, starting each BusyBox command. The Phase 9 report measured the damage:

| Linux boot to a shell (Phase 9/11, before this change) | |
|---|---:|
| blocks translated | 66,669 |
| average block size | 6.3 guest instructions |
| time spent translating | **590 ms of 1.48 s** (about 40%) |

Almost half of the boot was spent *preparing* code, not *running* it.

> **Analogy.** Think of a translator who is given a book to read aloud in another language. For a chapter that will be read aloud 1,000 times, it is worth writing a careful, polished translation first. For a footnote that will be read once, it is faster to translate it on the fly, word by word, while reading. A good translator decides *per passage*. Bridge-V used to write a polished translation of every footnote.

---

## 2. The idea: interpret first, translate what proves hot

Bridge-V already has **two** ways to run a block:

| | Interpreter (file 06) | JIT (files 07–10) |
|---|---|---|
| cost to prepare a block | decode only, about 0.5 µs | decode + IR + optimize + allocate + emit, about **8 µs** |
| cost to run a block once | about **0.1 µs** (it executes each instruction in Rust) | about **0.006 µs** (native x86 code) |

Tiered translation uses **both**, one after the other ("tiers", like floors of a building):

```
        cpu.pc = start of a block
               │
               ▼
     ┌──────────────────────┐   yes   ┌──────────────────────────────┐
     │ already translated?  │ ───────►│ run its x86 code (JIT, fast) │
     └──────────────────────┘         └──────────────────────────────┘
               │ no
               ▼
     runs[block] = runs[block] + 1
               │
               ▼
     ┌──────────────────────┐   yes   ┌──────────────────────────────┐
     │ runs[block] ≤ N ?    │ ───────►│ run it in the interpreter    │   ← tier 1: "cold"
     └──────────────────────┘         └──────────────────────────────┘
               │ no (this is run N+1)
               ▼
     translate it, then run its x86 code                               ← tier 2: "hot"
```

- **N** is the command-line option **`--tier N`**. The default is **32**.
- **`--tier 0`** means "translate every block on its first run", which is exactly the old behaviour.
- A block that runs only 5 times is **never translated**. It costs 5 × 0.1 µs = 0.5 µs instead of 8 µs.
- A hot loop that runs a million times pays 32 interpreted runs (about 3 µs, nothing), then gets translated and runs at full JIT speed forever after.

**Vocabulary:**
- A block that has not been translated yet is **cold**. One that has run often enough to be translated is **hot**.
- **Tier-up** (or **promotion**) is the moment a cold block becomes hot and gets translated.

---

## 3. The math: when does translating pay off?

Use three costs (rounded from Bridge-V's own measurements):

- **T** = cost to translate one block ≈ **8 µs**
- **I** = cost to interpret one run of a block ≈ **0.1 µs**
- **J** = cost to run one block as JIT code ≈ **0.006 µs**

A block that will run **k** times costs:

| strategy | total cost |
|---|---|
| translate at once | T + k·J |
| never translate | k·I |

They are equal when T + k·J = k·I, that is when

```
k = T / (I − J) ≈ 8 / (0.1 − 0.006) ≈ 85 runs
```

This number is the **break-even point**. With Bridge-V's measured costs it lies somewhere between about **50 and 85 runs**, depending on the machine and block size. Below it, interpreting is cheaper. Above it, translating is cheaper.

**The catch:** when a block runs for the first time, you do not know whether it will run 1 time or 1 million times. You must decide *without knowing the future*.

### 3.1 The ski-rental problem

Computer science has a famous puzzle for exactly this situation:

> You are going skiing, but you don't know for how many days. Renting skis costs 1 per day. Buying costs 10. What should you do?

- If you buy on day 1 and it rains after one day, you paid 10 instead of 1. That is 10× too much.
- If you rent forever and ski 100 days, you paid 100 instead of 10. That is also 10× too much.
- The clever rule: **rent until the rent you have paid equals the price of buying, then buy.** Rent for 10 days; if you are still skiing on day 11, buy.

With that rule you *never* pay more than about **2×** what you would have paid had you known the future. (Worst case: you rent 10 days, buy, and then stop. You paid 20, while the best choice, buying on day 1, cost 10.) Computer scientists call this a **2-competitive** algorithm.

Tiered translation is ski rental:

| skiing | Bridge-V |
|---|---|
| renting for a day | interpreting one run of a block (I) |
| buying skis | translating the block (T) |
| "rent until rent = price, then buy" | "interpret N ≈ T/I times, then translate" |

So theory says: pick **N near the break-even point** (about 50–85), and no block can cost much more than twice its ideal cost. Blocks that run only a few times (most of the boot) become far cheaper.

### 3.2 What the real data showed

Theory is nice; the project's rule is *measure* (file 18). Two measurements answered the question.

**1. How often does each block run during a boot?** Run the boot with a tier so high that nothing is ever translated (`--tier 1000000000`). Then every block stays in the interpreter tier, and `--stats` prints how often each one ran:

| times a block ran | number of blocks | share | cumulative share |
|---|---:|---:|---:|
| exactly 1 | 20,659 | 31.4% | 31.4% |
| 2–3 | 7,952 | 12.1% | 43.6% |
| 4–7 | 5,356 | 8.2% | 51.7% |
| 8–15 | 7,926 | 12.1% | 63.8% |
| 16–31 | 5,382 | 8.2% | 72.0% |
| 32–63 | 4,659 | 7.1% | 79.1% |
| 64–127 | 3,109 | 4.7% | 83.8% |
| 128 or more | 10,647 | 16.2% | 100% |
| **total** | **65,690** | | |

- **Almost a third of all blocks run exactly once.** Translating them was pure waste.
- **Almost two thirds run fewer than 16 times.**
- A small group of very hot blocks (a few thousand that run thousands of times or more) does almost all the actual work.

This pattern, where a few things are used enormously and most things barely at all, is so common in computing that it has a name: a **long-tail** or **power-law** distribution.

**2. Which N boots Linux fastest?** Boot to the shell prompt with different tiers. Every configuration was booted 5 times, taking turns so machine noise hit all of them equally, and the median is shown:

| `--tier` | time to shell | blocks translated | host code | translate time |
|---:|---:|---:|---:|---:|
| 0 (old behaviour) | 1.32 s | 65,544 | 43.4 MiB | 515 ms |
| 1 | 1.20 s | 44,939 | 30.1 MiB | 363 ms |
| 4 | 1.16 s | 34,621 | 23.2 MiB | 297 ms |
| **16** | **1.11 s** | 23,167 | 15.8 MiB | 201 ms |
| **64** | **1.11 s** | 13,341 | 9.1 MiB | 116 ms |
| 256 | 1.32 s | 7,452 | 5.1 MiB | 71 ms |
| QEMU 8.2 (for comparison) | 1.41 s | | | |

*Host: a GitHub Actions runner (AMD EPYC 7763, shared cloud VM), commit `53f9030`. Numbers from a different machine than file 18's Xeon, so compare only within this table.*

Look at the shape:
- **Too small N** (1, 4): many blocks that run only a few times still get translated.
- **Too big N** (256): the few hot blocks are interpreted hundreds of times each before they get translated, and those interpreted runs add up. Tier 256 interpreted 20.7 million instructions, against 3.3 million at tier 16.
- **In between** (16 to 64) is a flat valley: the best of both. The default **32** sits in the middle of that valley and close to the break-even estimate. That is the ski-rental rule and the measurement agreeing.

This kind of U-shaped curve, where too little and too much are both bad, appears whenever you trade a one-time cost against a per-use cost. Caches, batching and garbage-collector tuning all look like this.

**3. Confirming the default.** A second run on a fresh runner (commit `2e498ee`, 7 boots per configuration, taking turns) zoomed in on the valley:

| `--tier` | time to shell | blocks translated | host code | translate time |
|---:|---:|---:|---:|---:|
| 0 (old behaviour) | 1.30 s | 65,634 | 43.5 MiB | 561 ms |
| 16 | 1.07 s | 23,163 | 15.8 MiB | 197 ms |
| **32 (the default)** | **1.06 s** | **18,023** | **12.3 MiB** | **154 ms** |
| 64 | 1.09 s | 13,391 | 9.1 MiB | 119 ms |
| QEMU 8.2 | 1.41 s | | | |

**Result with the default tier: the boot got 18% faster (1.30 s → 1.06 s), from 1.08× to 1.33× QEMU's speed on this machine.** The JIT also translated **72% fewer blocks** and produced **72% less machine code** (43.5 MiB → 12.3 MiB). "MiB" is a mebibyte, 1,048,576 bytes. Less code also means the CPU's instruction cache is used better.

> The user-mode benchmarks (CoreMark, Dhrystone, fpbench) with and without the tier, and all the raw data, are in the phase report: [`docs/phase-reports/phase-12-tiered-translation.md`](../phase-reports/phase-12-tiered-translation.md).

---

## 4. How it is built

All of it lives in [`src/jit/dispatch.rs`](../../src/jit/dispatch.rs), the dispatcher from file 05: about 190 changed lines, many of them comments and the `--stats` report. Nothing in the translator (IR, allocator, emitter) changed. The whole phase, with tests and tools, is about 425 added lines.

### 4.1 The option

```rust
pub const DEFAULT_TIER: u32 = 32;          // dispatch.rs:58

pub struct JitOptions {
    ...
    /// `--tier N` (D63): a block runs N times in the interpreter before it is translated; 0
    /// translates it on its first run.
    pub tier: u32,                         // dispatch.rs:101
}
```

`main.rs` adds `--tier N` to both `bridgev run` and `bridgev boot`, with `DEFAULT_TIER` as the default.

### 4.2 Remembering cold blocks

```rust
/// A block of the interpreter tier (D63): how often it ran, and its decoded instructions
/// (dropped once it is translated, or when its page is written).
struct Cold {                              // dispatch.rs:200
    key: TbKey,
    runs: u32,
    block: Option<Block>,
}
```

The `Jit` struct gets three new fields:
- `cold: FxHashMap<TbKey, u32>`: a hash map from "which block" to its position in the next list.
- `cold_blocks: Vec<Cold>`: the counters and decoded instructions.
- `cold_pages: FxHashMap<u64, Vec<u32>>`: which cold blocks came from which memory page (for self-modifying code, §5.3).

### 4.3 `select()`: the decision point

`select()` is the function that decides what runs next (file 05, §7). Before this change it always returned "run TB number `id`", translating first if needed. Now it has one more possible answer:

```rust
pub enum Next {
    Tb(u32),        // run this translated block
    Cold(u32),      // NEW: run cold block number i in the interpreter
    Straddle,       // an instruction crosses a page boundary: interpret it
    Fault(Exception),
}
```

In `select()` (dispatch.rs:528), right before the place where a missing translation would be created:

```rust
if let Some(i) = self.cold_run(self.soft_key(cpu, ppage)) {
    return Next::Cold(i);
}
let id = self.tb_for_soft(cpu, mem, ppage);   // translate (or find) as before
```

### 4.4 `cold_run()`: count, and decide

```rust
fn cold_run(&mut self, key: TbKey) -> Option<u32> {       // dispatch.rs:572
    if self.opts.tier == 0 || self.cache.lookup_key(&key).is_some() {
        return None;                     // tier off, or already translated: use the JIT
    }
    let next = self.cold_blocks.len() as u32;
    let i = *self.cold.entry(key).or_insert(next);   // find, or create, this block's counter
    if i == next {
        self.cold_blocks.push(Cold { key, runs: 0, block: None });
    }
    let c = &mut self.cold_blocks[i as usize];
    c.runs = c.runs.saturating_add(1);   // count this run
    if c.runs > self.opts.tier {
        c.block = None;                  // hot now: free the decoded copy, translate it
        return None;
    }
    self.last_exit = None;               // nothing to link the previous exit to
    Some(i)                              // still cold: interpret it
}
```

Line by line:
- `saturating_add(1)` adds 1 but stops at the largest `u32` instead of overflowing. It only matters for silly tiers like 10⁹.
- `self.last_exit = None`: the dispatcher remembers the last unlinked exit so it can **chain** it to the next TB (file 10). A cold block has no x86 code to jump to, so there is nothing to link.

### 4.5 `exec_cold()`: run it in the interpreter

```rust
fn exec_cold(&mut self, cpu, mem, i: u32) -> BlockExit {     // dispatch.rs:599
    let key = self.cold_blocks[i as usize].key;
    if self.cold_blocks[i as usize].block.is_none() {
        // First run (or its page was written): decode it, like build_block in file 06.
        let block = if soft { build_block_soft(key.pc, cpu, mem, max) }
                    else    { build_block_max(key.pc, mem, max) };
        // Mark its page(s) as code pages, so a write to them is noticed (§5.3).
        for p in code_pages(key, soft, bytes) {
            self.cold_pages.entry(p).or_default().push(i);
            self.new_code.push(p);
        }
        self.mark_new_code(cpu, mem);
        self.cold_blocks[i as usize].block = Some(block);
    }
    let block = ...;                                           // the cached decoded block
    let exit = exec_block(cpu, mem, &block.insns, block.fetch_fault, false);
    self.stats.cold_blocks += 1;                               // for --stats
    ...
    exit
}
```

`exec_block` is **the interpreter's own function** (file 06). The JIT engine borrows it. The main loop `Jit::run` just gained one line:

```rust
Next::Cold(i) => self.exec_cold(cpu, mem, i),              // dispatch.rs:1006
```

After that, the loop continues exactly as before: deliver the ECALL, trap or interrupt, then call `select()` again.

### 4.6 `--stats`

With a tier, `--stats` prints an extra part (`tier_counters`, dispatch.rs:1098), for example from the boot at tier 16:

```
tier 16: 525589 blocks (3267963 guest insns) interpreted, 23090 blocks then translated,
42052 still cold (by runs: 1: 20505, 2-3: 7923, 4-7: 5106, 8-15: 7996, 16: 522)
```

Read it as:
- 525,589 block runs happened in the interpreter.
- 23,090 blocks became hot and were translated.
- 42,052 blocks never became hot (they ran at most 16 times), grouped by how often they ran.

---

## 5. The tricky parts (the design decisions)

### 5.1 Why cache the decoded instructions?

Decoding a block costs about 0.5 µs, which is **five times** more than interpreting it once. If a cold block that runs 20 times were decoded every time, the tier would waste most of what it saves. So each cold block is decoded once and kept (`Cold::block`) until it is translated.

### 5.2 Why is a cold block keyed exactly like a translated block?

The key is `TbKey { pc, slow, flags, ppage }`: the guest pc, the FP variant, the MMU flags and the **physical** page (file 11). There are two reasons.

1. **The counter must count runs of the thing that will be translated.** A translation depends on the whole key (for example, user-mode and kernel-mode code at the same address translate differently), so the count must too.
2. **It survives TLB flushes.** The interpreter engine's own block cache is keyed by the *virtual* pc and is thrown away at every TLB flush, which Linux does very often. Translated blocks survive flushes because they are keyed by the physical page (decision D48). Cold blocks use the same key, so they survive too.

### 5.3 Self-modifying code (file 13)

What if the guest overwrites code that is sitting in the cold cache? The answer copies the TB mechanism exactly:
- When a cold block is decoded, its page is marked as a **code page** (`mark_new_code`), just like a translated block's.
- Any write to a code page lands in `mem.smc_pages`.
- `drain_smc` (dispatch.rs:638) now also drops the decoded copy of every cold block from that page:

```rust
for i in self.cold_pages.remove(&p).unwrap_or_default() {
    self.cold_blocks[i as usize].block = None;     // decode again on the next run
}
```

This works even when a cold block overwrites *its own* page. The interpreter stops right after such a store (`Flow::Smc`, file 13), and the next `select()` drains the page.

### 5.4 Lockstep always translates

The lockstep engine (file 17) exists to check that every translation matches the interpreter. If it used the tier, most boot code would never be translated, so never checked. So lockstep forces `tier: 0` (`src/jit/lockstep.rs:43`).

### 5.5 Chaining still works

A hot block whose exit leads to a cold block cannot be chained to it, because there is no x86 code to jump to yet. That exit just returns to the dispatcher. When the cold block becomes hot and is translated, the dispatcher links the exit the next time it is taken. Hot code ends up chained exactly as before (file 10).

### 5.6 Full flushes

When the code cache is full, or the guest maps new executable memory, `flush_all` also clears the cold tables (dispatch.rs:971). Everything starts from zero, like the translations.

---

## 6. Why it cannot break correctness

1. **The interpreter is the golden model.** It boots Linux on its own and passes every test (file 17). The cold tier runs the *same* `exec_block` function.
2. **The hand-off happens only at block boundaries.** The block-boundary rule (file 09, CLAUDE.md §8.3) says that between blocks, `CpuState` holds the *entire* guest state: all registers, pc and flags. JIT code writes everything back before it exits. So at a boundary the interpreter and the JIT can take turns freely. Neither has hidden state the other needs.
3. **Same block boundaries.** Cold blocks are built with the same maximum length (`--max-block`) and stop at the same places as translated blocks.
4. **Every subtle case copies an existing, tested mechanism:** the TB key, page marking for self-modifying code, flushes.

**Tests added** (all pass in CI):

| test | what it checks |
|---|---|
| `phase1_suites_pass_tiered` (tests/riscv_tests.rs) | all 244 riscv-tests with tiers 1, 3 and 32: every test mixes interpreted and translated blocks |
| `guest_programs_match_qemu_reference_tiered` (tests/user_programs.rs) | every guest program at tier 2, with direct and software-TLB memory, byte-identical to QEMU. This includes the self-modifying-code program `smc.c` |
| `icount_is_exact_under_every_engine` (tests/cli.rs) | the retired-instruction count is exactly the interpreter's, with and without the tier |
| `tier_translates_only_hot_blocks` (tests/cli.rs) | a small tier translates fewer blocks than tier 0; a huge tier translates none; the count never changes |
| Linux boot tests (tests/linux_boot.rs) | every JIT boot (built-in SBI, OpenSBI, Sv48, virtio disk, 4 harts) now runs with the default tier; `linux_boots_translating_every_block_jit` keeps a full-translation boot |

**A trap that was avoided.** With the tier on by default, several old tests would have kept passing *without testing anything*. For example, `jit_lowering.rs` compares every ALU instruction run by the JIT against the interpreter. But its tiny test programs run each block once, so under the default tier the "JIT" run would actually have been the interpreter, compared with itself. Tests of translated code now ask for `--tier 0` explicitly.

> **Lesson:** when you change a default, check which tests *silently change meaning*, not just which ones fail.

---

## 7. Is this unique? (be honest in interviews)

- **The idea is old.** Language virtual machines have done it for decades: Java's HotSpot interprets a method before compiling it, and JavaScript's V8 runs new code in its Ignition interpreter first. In binary translation, HP's **Dynamo** (1999) interpreted code while it looked for hot paths.
- **QEMU does not do it.** QEMU's TCG, the most widely used translator, translates every block the first time it runs. Its "TCI" is a separate *build option* that interprets instead of translating; it is not a tier.
- **So the honest claim is:** *"Bridge-V adds interpreter-first tiering, which QEMU's TCG does not have, chose the threshold from the ski-rental break-even, and measured that it makes the Linux boot 18% faster and cuts the generated code by 72%."*

Do **not** claim "no translator does this". An interviewer who knows HotSpot or Dynamo will catch it. Saying where the idea comes from *shows* you know the field.

---

## 8. How to explain it in an interview

**30 seconds:**
> "A JIT pays a big one-time cost to translate each block. I measured that in a Linux boot almost a third of the blocks run only once, so that cost was wasted. I added a tier: new code runs in my interpreter first, and a block is translated only after it has run 32 times. That's the ski-rental rule, where you rent until renting has cost as much as buying. It made the boot 18% faster, now 1.33× QEMU, and cut the generated code by 72%."

**Likely follow-up questions:**

- *Why 32?* The break-even is translate cost ÷ interpret cost per run, about 8 µs ÷ 0.1 µs ≈ 50–85 runs. The measured optimum was flat from 16 to 64, so I took the middle. Ski rental says a threshold near break-even is never worse than about 2× the best choice for any block.
- *Doesn't interpreting hot code first slow down benchmarks?* A hot block pays 32 interpreted runs, about 3 µs, once. CoreMark runs about 1,700 blocks, so that's a few milliseconds out of a 10-second run. The user-mode benchmark A/B in the phase report checks it.
- *How do the interpreter and JIT hand over state?* Only at block boundaries, where `CpuState` holds all guest state by the block-boundary ABI. Pinned host registers are written back when JIT code exits, so the interpreter always sees an up-to-date state.
- *What about self-modifying code?* Cold blocks mark their pages as code pages like translated blocks do, so a write drops the decoded copy through the same `drain_smc` path.
- *How did you make sure it's correct?* Same interpreter function as the golden model. Tiered runs of all riscv-tests, all guest programs and Linux boots on 1 and 4 harts. Lockstep still checks every translation because it runs with tier 0. And I audited which old tests would silently stop testing the JIT under the new default.
- *What would you do next?* Make the interpreter tier cheaper per run, so a higher tier pays. Or save translations across runs (a persistent code cache), so even hot code isn't retranslated on every boot.

---

## 9. Try it yourself

(On a Linux machine or WSL2: see file 22.)

```sh
# CoreMark-style hot code barely notices the tier:
target/release/bridgev run --engine jit --stats guest/build/fib-O2.elf
target/release/bridgev run --engine jit --tier 0 --stats guest/build/fib-O2.elf

# Short programs: most blocks run once, so few get translated.
target/release/bridgev run --engine jit --stats guest/build/hello-O2.elf        # look for "tier 32:"
target/release/bridgev run --engine jit --tier 0 --stats guest/build/hello-O2.elf

# How often do blocks run during a Linux boot? (slow: everything is interpreted)
target/release/bridgev boot --kernel guest/build/linux/Image --initrd guest/build/linux/rootfs.cpio \
    --stats --tier 1000000000

# The boot-time sweep, like the table in §3.2:
python3 tools/boot-bench.py --configs jit-tier0,jit-tier16,jit-tier32,jit-tier64,qemu --detail
```

Without a Linux machine, push a commit to a branch named `bench/<anything>` in your GitHub repository. The workflow `.github/workflows/bench.yml` runs these benchmarks on a GitHub runner and shows the tables in the run's summary page.

---

## Check yourself

1. In one sentence each: what is a *cold* block, a *hot* block and *tier-up*?
2. Write the break-even formula. With T = 8 µs, I = 0.1 µs and J ≈ 0, how many runs is it?
3. Explain the ski-rental rule and why it is "2-competitive". What plays the role of renting and of buying in Bridge-V?
4. The boot table shows tier 256 as slow as tier 0. Explain why, using the words *translation* and *interpreted runs*.
5. Why are cold blocks cached decoded instead of decoded on every run?
6. Why is a cold block keyed by the *physical* page, like a TB, and not by the virtual pc like the interpreter engine's cache?
7. Why does lockstep force `--tier 0`? What would go wrong otherwise?
8. A hot block's exit leads to a cold block. Can it be chained? What happens when the cold block becomes hot?
9. Why can the interpreter and the JIT take turns at block boundaries but not in the middle of a block?
10. Name one test that would have passed *without testing anything* after the default changed, and explain how it was fixed.
11. What is the honest way to answer "Did you invent this?"
