# 11 · Memory, the MMU and the software TLB

## What you will learn

- **direct memory** (user mode): how guest address `g` becomes host address `base + g`, and why that needs no checks
- how host page protection is used to catch bad guest accesses for free
- **physical memory and devices** in system mode
- **virtual memory on RISC-V**: the Sv39 page-table format and the page-walk algorithm, step by step
- the **software TLB**: its entry layout, the flag-bit trick, and the 9-instruction inline check in generated code
- the **slow path**, TLB **flushes**, and the measured costs

Files: [`src/mem/direct.rs`](../../src/mem/direct.rs), [`src/mem/mmu.rs`](../../src/mem/mmu.rs), [`src/mem/tlb.rs`](../../src/mem/tlb.rs), `tlb_probe()` and `emit_soft_slow()` in [`src/backend/x86/lower_ir.rs`](../../src/backend/x86/lower_ir.rs).

---

## 1. Two memory backends

| Backend | Used in | Guest address → host address | Cost per access |
|---|---|---|---|
| **direct** | user mode (default) | `host = base + guest` | zero extra instructions |
| **softmmu** | system mode (always); user mode with `--mem=softmmu` | through emulated page tables, cached in a software TLB | ~9 instructions on a TLB hit |

Measuring both is part of the project's story: CoreMark keeps 56% of its speed when every load and store goes through the software TLB (file 18).

---

## 2. Direct memory (user mode)

### 2.1 One big reservation

`DirectMem::new()` reserves the guest's whole address space in one call:

```rust
mmap(NULL, 2^38 + 2 * 4 GiB, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
base = reservation + 4 GiB
```

- **2³⁸ bytes = 256 GiB**: the user half of an Sv39 address space, which is what RISC-V Linux programs use.
- **`PROT_NONE` + `MAP_NORESERVE`**: this only reserves *addresses*. No RAM is used until pages are actually mapped.
- **4 GiB guard regions** on both sides: a guest address slightly out of range lands in a guard and faults, instead of touching Bridge-V's own memory.

When the guest (or the loader) maps memory at guest address `g`, `DirectMem::map()` calls `mmap(base + g, len, prot, MAP_FIXED, ...)` inside the reservation. So guest address `g` **always** lives at host address `base + g`.

### 2.2 Loads and stores cost nothing

In generated code, RBX holds `base`. A guest `ld a5, 16(a4)` with `a4` in RAX becomes a single instruction:

```
mov rdi, [rbx + rax + 16]
```

There is no range check and no permission check. How can that be safe?

### 2.3 The host MMU does the checking (decision D27)

Every guest page has permissions (R, W, X). `DirectMem` gives the host page **matching host protection**:
- host readable if the guest may read, write or execute it
- host writable only if the guest may write it
- unmapped guest pages stay `PROT_NONE`

So if the guest accesses memory it isn't allowed to, the **real CPU's MMU** raises a page fault, the host kernel sends **SIGSEGV**, and Bridge-V's handler turns it into a precise guest exception (file 12). The check costs nothing in the normal case, because the hardware does it anyway.

(Execute-only guest pages are kept host-readable, because the decoder has to read their bytes. A load from such a page therefore doesn't fault under the JIT. QEMU's user mode behaves the same way.)

### 2.4 The guest permission table

`DirectMem` also keeps its own record of guest permissions: a two-level table with one byte per 4 KiB page (`PageProt`, chunks of 4,096 pages allocated lazily). The interpreter and the syscall layer use **checked** accessors (`load`, `store`, `fetch16`, `slice`, `slice_mut`) that consult this table and return a `MemFault` instead of ever touching an invalid host address. The same byte also stores two extra flags: `MAPPED` and `CODE` (the page holds translated code, file 13).

---

## 3. System mode: physical memory and devices

In system mode the guest runs its own operating system with its own page tables. Bridge-V emulates **physical** memory:
- **RAM** (default 512 MiB) is mapped in `DirectMem` at its physical address, `0x8000_0000`. So a physical RAM address `pa` lives at host address `base + pa`.
- **Devices** (timer, interrupt controller, serial port, …) are in a sorted list, `DirectMem::devices`. An access to a device address calls the device's `read`/`write` method instead of touching memory. This is **MMIO** (memory-mapped I/O).
- Any other physical address raises an **access fault**.

---

## 4. Virtual memory on RISC-V: Sv39

### 4.1 The idea

The guest OS gives each process its own virtual address space. The CPU translates every virtual address to a physical one using a **page table** in memory. Pages are 4 KiB. Bridge-V supports **Sv39** (39-bit virtual addresses, 3-level table; the default) and **Sv48** (48-bit, 4 levels; `--mmu sv48`).

The `satp` CSR controls it:
```
satp: MODE[63:60] | ASID[59:44] | PPN[43:0]
      0 = Bare (no translation), 8 = Sv39, 9 = Sv48      PPN = root table's physical page number
```

### 4.2 Splitting a virtual address

```
Sv39 virtual address (39 bits used; bits 63:39 must all equal bit 38):
 38        30 29        21 20        12 11           0
[  VPN[2]   ][  VPN[1]   ][  VPN[0]   ][   offset    ]
   9 bits       9 bits       9 bits       12 bits
```

Each **VPN** (virtual page number) piece indexes one level of the tree. Each table is one 4 KiB page holding 512 eight-byte entries (2⁹ = 512).

### 4.3 A page-table entry (PTE)

```
 63   62:61  60:54      53:28     27:19     18:10    9:8  7 6 5 4 3 2 1 0
[ N ][PBMT][reserved][ PPN[2] ][ PPN[1] ][ PPN[0] ][RSW][D A G U X W R V]
```

| Bit | Meaning |
|---|---|
| V | valid |
| R, W, X | readable, writable, executable. If all three are 0, the entry points to the next table level. |
| U | accessible from U-mode (user) |
| G | global (ignored by Bridge-V) |
| A | accessed: set when the page is used |
| D | dirty: set when the page is written |
| PPN | physical page number (of the page, or of the next-level table) |

### 4.4 The walk algorithm (`walk()` in `mmu.rs`)

```
walk(va, access, mmu_index):
    if mmu_index == M or satp.MODE == Bare:  return va (identity, all permissions)
    if va's top bits aren't a sign-extension of bit 38:  PAGE FAULT
    a = satp.PPN * 4096 ; level = 2
    loop:
        pte = physical_load(a + VPN[level] * 8)       // a RAM read; outside RAM → ACCESS FAULT
        if !V  or  (!R and W)  or  reserved bits set:  PAGE FAULT
        if R or X:  break                              // a leaf: this entry maps the page
        if level == 0:  PAGE FAULT                     // no more levels
        level -= 1 ; a = pte.PPN * 4096                // go down one level
    check privilege:
        U-mode needs U = 1
        S-mode may use U pages only for data, and only with SUM = 1; never execute them
    if level > 0 (a superpage: 2 MiB or 1 GiB) and the low PPN bits aren't 0: PAGE FAULT (misaligned)
    check permission: load needs R (or X with MXR), store needs W, fetch needs X
    set A (and D for a store) in the PTE if not already set
    return physical page = PPN (+ the low VPN bits for a superpage)
```

Faults: an instruction fetch page fault is cause 12, a load is 13, a store is 15; `tval` holds the bad virtual address.

**Setting A/D in the walker** (instead of raising a fault so the OS sets them) is allowed by the spec and saves a trap. It is like the "Svadu" extension.

---

## 5. The software TLB

### 5.1 Why

Walking three levels (three memory reads, lots of checks) on every load and store would make everything very slow. Real CPUs cache recent translations in a **TLB**. Bridge-V does the same in software (decisions D10, D48), and puts the TLB inside `CpuState` so generated code can read it directly.

### 5.2 Layout

```rust
pub tlb: [[TlbEntry; 256]; 4],          // CpuState offset 0x10580: 4 × 256 × 32 B = 32 KiB

#[repr(C)]
pub struct TlbEntry {
    pub addr_read:  u64,   // tag for loads
    pub addr_write: u64,   // tag for stores
    pub addr_code:  u64,   // tag for instruction fetch
    pub addend:     u64,   // host_address = virtual_address + addend
}
```

- **Direct-mapped, 256 entries.** Virtual page `vp` can only go in slot `vp & 255`. Finding the slot is a shift and a mask; there is no search.
- **One TLB per MMU index** (4 of them): U, S, S with SUM, and M/Bare. Linux switches SUM on and off very often (to copy data to and from user memory). With separate TLBs, that needs no flush.
- **A tag** is the virtual page address (low 12 bits zero), or `u64::MAX` (invalid).
- **`addend`** = host address of the page − virtual page address. Adding it to any virtual address in the page gives the host address in one instruction.

### 5.3 Permissions are folded in at fill time

When `fill()` walks and fills an entry, it writes a valid tag **only for the kinds of access that are allowed**:
- a read-only page gets `addr_write = INVALID`
- a page with D = 0 gets `addr_write = INVALID` too, so the first store misses, walks, and sets D
- so a tag match means "translation found **and** this access is permitted". The inline code needs no separate permission check.

### 5.4 The flag-bit trick

Bits 3–11 of a tag are always 0 in a real page address, and the value being compared always has them clear too. Bridge-V puts **flags** there:
- `TLB_MMIO` (bit 3): this page is a device
- `TLB_CODE` (bit 4, on `addr_write` only): this page holds translated code (file 13)

A tag with a flag set **never matches**, so the access automatically takes the slow path, which handles devices and self-modifying code properly. The fast path doesn't need to know these cases exist.

---

## 6. The inline fast path in generated code

For `ld a5, 16(s1)` with `s1` in RSI and the result going to RDI (`tlb_probe()` in `lower_ir.rs`):

```
lea   r11, [rsi + 16]                   ; r11 = guest virtual address
mov   r10, r11
shr   r10, 7                            ; va >> 12 gives the page; << 5 for 32-byte entries → >> 7
and   r10d, 0xFF << 5                   ; r10 = byte offset of slot (page & 255)
and   r11, -4096 | 7                    ; keep the page bits AND the low 3 "misalignment" bits
cmp   r11, [rbp + r10 + tlb_S + 0]      ; compare with addr_read (+8 for addr_write)
jne   .slow                             ; miss, not permitted, misaligned, MMIO, code page: slow path
lea   r11, [rsi + 16]                   ; the address again
add   r11, [rbp + r10 + tlb_S + 24]     ; + addend = host address
mov   rdi, [r11]                        ; the actual load  (fault site recorded here)
.ret:
```

Details worth understanding:
- **`and r11, -4096 | (size − 1)`** keeps the page number *and* the low bits that would make this access misaligned (for an 8-byte access, bits 0–2). A tag always has those bits clear, so a **misaligned access never matches** and goes to the slow path. That guarantees the fast path never crosses a page boundary.
- The TLB's offset for the MMU index is a **constant baked into the code**. A TB is keyed by its MMU flags, so it always knows which TLB to use.
- **9 instructions on a hit**, touching one 32-byte TLB entry.

---

## 7. The slow path

Each softmmu access has its own **cold stub** at the end of the TB (`emit_soft_slow()`):

1. Save RAX, RCX, RDX, RSI, RDI, R8 and R9 to `cpu.fault_regs`.
2. Put the arguments in place: `va`, `info` = size | signed<<4 | store<<5 | mmu_index<<8, and the value (for stores). Refund the budget of not-yet-retired instructions (so the helper can keep `icount` exact).
3. `call helper_mmu_access(cpu, va, info, val)` → in Rust: `soft_load` / `soft_store` in `mmu.rs`:
   - look up the TLB; on a miss, `walk()` and `fill()`
   - RAM → read/write it; device → `mmio_read`/`mmio_write`
   - a misaligned access that crosses a page is split byte by byte (a store checks **both** pages first, so a faulting store changes nothing)
4. Restore the registers. Check `exit_reason`:
   - 0: move the loaded value into the destination and jump back to `.ret`
   - otherwise (a page fault): also save R12–R15, set `fault_rip` to the access's address, and exit with reason `MMU_FAULT`. The dispatcher applies that access's **state map** (file 08, §5), so the guest sees a precise fault.

---

## 8. Keeping the TLB correct: flushes

A cached translation must be thrown away when the page tables it came from might have changed:

| Event | What is flushed |
|---|---|
| write to `satp` (a new address space) | everything (`tlb::flush_all`) |
| `sfence.vma` with rs1 = x0 | everything |
| `sfence.vma` with an address | that page, in all 4 TLBs, plus its jump-cache entries (`tlb::flush_page`) |
| … if that address lies in a cached **superpage** range | everything (a superpage fills many 4 KiB slots; `cpu.tlb_super` tracks the range) |
| `MXR` changes (make-executable-readable) | everything |
| privilege change, `SUM` change | nothing (separate TLBs) |
| user mode `--mem=softmmu`: `brk`, `mmap`, `munmap`, `mremap`, `mprotect` | everything |

Every flush bumps `cpu.mmu_gen`. The interpreter drops its decoded-block cache when `mmu_gen` changes (its cache is keyed by virtual pc). A full flush also bumps `cpu.jc_gen`, which resets the JIT's jump cache. ASIDs (address-space IDs) are ignored: flushing is always correct, just sometimes slower.

---

## 9. How translations stay valid for code (system mode)

In system mode, a TB is keyed by `TbKey { pc (virtual), flags (MMU indices), ppage (physical page) }`. Before running a TB, `select()` translates the current `pc` through the fetch TLB to get the physical page, so if the OS remapped the page, a different key is looked up and the old translation isn't used. A 32-bit instruction that straddles two pages is never translated; `interpret_one()` runs it in the interpreter.

---

## 10. Measured costs (Phase 7, `tools/tlb-bench.py`)

| | Cost |
|---|---|
| TLB hit, throughput | about **0.34 ns** per access (the CPU overlaps the check with other work) |
| TLB hit, added load-to-use latency | about **3 ns** |
| TLB miss (walk + fill) | about **28 ns** (~59 cycles) |
| CoreMark with every access through the TLB | 7,526 it/s = **56%** of direct mode (13,409) |

---

## Check yourself

1. How does direct mode make guest memory accesses cost zero extra instructions, and what catches bad accesses?
2. Why reserve 256 GiB with `PROT_NONE` + `MAP_NORESERVE`? How much RAM does that use?
3. Split an Sv39 virtual address into its fields. How many levels does the walk have?
4. Walk through the page-walk algorithm. What makes an entry a "leaf"?
5. What are the A and D bits, and who sets them in Bridge-V?
6. What does a TLB entry contain? Why are there 4 TLBs?
7. How do permissions get checked without extra instructions on a TLB hit?
8. Explain the flag-bit trick. What two flags use it?
9. Why does a misaligned access always take the slow path?
10. When must the TLB be flushed?
