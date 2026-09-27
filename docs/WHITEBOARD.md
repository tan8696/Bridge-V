# Whiteboard walkthrough from real translation blocks

This page walks through three translation blocks (TBs) exactly as Bridge-V produced them. They are not hand-written examples. Each one shows a different part of the design:

- a self-loop TB with a chained branch;
- a function return through the jump cache;
- the same self-loop TB under softmmu, with the inline TLB probe and its cold slow path.

Every byte below was copied from `--dump-x86`/`--dump-ir` output (Phase 11, commit `567255a`). Host addresses change from run to run (ASLR), but offsets inside the code buffer don't.

**Reproduce:**
```sh
cargo build --release --features disasm     # iced-x86 text; without it the dump is hex (D26)
D=/tmp/dump; mkdir -p $D
target/release/bridgev run --engine jit --dump-x86 $D --dump-ir $D guest/build/fib-O2.elf 20
target/release/bridgev run --engine jit --mem softmmu --dump-x86 $D-sm --dump-ir $D-sm guest/build/fib-O2.elf 20
#  $D/tb_<pc>.txt (guest + x86), tb_<pc>.ir (IR before/after passes), tb_<pc>.bin, links.txt (every chain patch)
```
One run of `fib-O2.elf 20` translated 933 TBs and patched 904 chain links.

---

## 1. A self-loop, chained to itself (direct memory)

This is glibc start-up code (at 0x10990) that scans for a zero doubleword: `a4` walks the array and `a5` gets each element. It is three compressed instructions.

**Guest code:**
```
0x10990: 631c   c.ld   a5, 0(a4)
0x10992: 0721   c.addi a4, a4, 8
0x10994: fff5   c.bnez a5, 0x10990      (bne a5, zero, -4)
```

**IR** (`--dump-ir`). Each guest register is read once and every value is defined once (D34). `load.u8` means an 8-byte unsigned load.
```
; lifted                                  ; optimized
  --- #0 @0x10990                           --- #0 @0x10990
    v0 = x14                                  v0 = x14
    v1 = load.u8 [v0+0]                       v1 = load.u8 [v0+0]
    x15 = v1                                  x15 = v1
  --- #1 @0x10992                           --- #1 @0x10992
    v2 = x14                  ── forward ──►  v3 = add v0, 8        (x14 re-read → v0)
    v3 = add v2, 8                            x14 = v3
    x14 = v3                                --- #2 @0x10994
  --- #2 @0x10994                             v5 = const 0x0
    v4 = x15                  ── forward ──►  br.Ne v1, v5 ? 0x10990 : 0x10996
    v5 = const 0x0
    br.Ne v4, v5 ? 0x10990 : 0x10996
```
The forwarding pass (D35) replaces both re-reads with values it already has: `x14` becomes `v0`, and `x15` becomes the loaded `v1`.

**x86-64:** 125 bytes at `B+0x3b0`, where `B` is the code buffer's RX base. RBP = `&CpuState + 128`, RBX = guest memory base, R9 = budget, and `a5` (x15) is pinned in R15. `a4` (x14) is not pinned, so it lives in `CpuState.x[14]` at `[rbp-0x10]`.
```
+3b0  49 83 e9 03              sub  r9, 3                   ; budget: charge 3 insns up front (D30, D46)
+3b4  0f 8c 26 00 00 00        jl   budget_stub (+3e0)      ; pre-emption point, nothing executed yet
+3ba  48 8b 45 f0              mov  rax, [rbp-0x10]         ; v0 = x14 (fill from CpuState)
+3be  48 8b 0c 03              mov  rcx, [rbx+rax]          ; v1 = ld [a4]   ← fault site (D37): no check code
+3c2  49 89 cf                 mov  r15, rcx                ; x15 = v1 (pinned: a register move)
+3c5  48 83 c0 08              add  rax, 8                  ; v3 = v0 + 8 (v0 is dead after this, reuse RAX)
+3c9  48 89 45 f0              mov  [rbp-0x10], rax         ; lazy write-back of dirty x14 at the block end (D36)
+3cd  48 85 c9                 test rcx, rcx                ; bne v1, 0: a compare against 0 is lowered to test
+3d0  66 90                    xchg ax, ax                  ; 2-byte NOP: aligns the next rel32 field to 4 (D7)
+3d2  0f 85 2b 00 00 00        jne  stub_1 (+403)           ; slot 1 (taken)   rel32 field at +3d4
+3d8  0f 1f 00                 nop  [rax]                   ; 3-byte NOP: aligns the next rel32 field to 4
+3db  e9 38 00 00 00           jmp  stub_0 (+418)           ; slot 0 (fall)    rel32 field at +3dc
; ---- cold stubs (never on the hot path) ----
+3e0  49 83 c1 03              add  r9, 3                   ; budget_stub: refund, nothing retired
+3e4  48 c7 85 80 00 00 00 90 09 01 00   mov qword [rbp+0x80], 0x10990   ; cpu.pc
+3ef  c7 85 90 00 00 00 05 00 00 00      mov dword [rbp+0x90], 5         ; exit_reason = BUDGET
+3f9  b8 12 00 00 00           mov  eax, 0x12               ; exit code (tb 4 << 2) | slot 2
+3fe  e9 4d fc ff ff           jmp  exit_jit (B+0x50)
+403  48 c7 85 80 00 00 00 90 09 01 00   mov qword [rbp+0x80], 0x10990   ; stub_1: pc = taken target
+40e  b8 11 00 00 00           mov  eax, 0x11               ; (4 << 2) | 1
+413  e9 38 fc ff ff           jmp  exit_jit
+418  48 c7 85 80 00 00 00 96 09 01 00   mov qword [rbp+0x80], 0x10996   ; stub_0: pc = fall-through
+423  b8 10 00 00 00           mov  eax, 0x10               ; (4 << 2) | 0
+428  e9 23 fc ff ff           jmp  exit_jit
```
**Offsets to check on the whiteboard (§8.1):**
- `x[14]` sits at 8·14 − 128 = −0x10, which fits in a disp8.
- `pc` is at 0x100 − 0x80 = 0x80, and `exit_reason` at 0x110 − 0x80 = 0x90.
- Both rel32 fields (+3d4 and +3dc) are 4-byte aligned, so a patch is a single aligned 32-bit store.

**The loop body is 9 hot instructions for 3 guest instructions.** It has one memory load for the guest load, one fill and one write-back of `a4`, and no guest-register traffic for `a5`.

**Chaining** (`links.txt`, written by the dispatcher on each patch):
```
tb_10990 slot 1 @+3d2: 0f 85 d8 ff ff ff -> tb_10990 @+3b0 (stub was +403)
tb_10990 slot 0 @+3db: e9 50 00 00 00    -> tb_10996 @+430 (stub was +418)
```
- The first time the loop branch is taken, execution leaves through `stub_1` with exit code 0x11. The dispatcher then looks up TB `0x10990` (itself) and rewrites the rel32 at +3d4 to `0x3b0 − (0x3d2 + 6) = −0x28 = d8 ff ff ff`.
- From then on, the loop runs `jne +3b0` straight back into its own prologue. The only way out is the `sub r9, 3; jl` budget check, which keeps a chained loop pre-emptible.
- When the loop ends, the fall-through exit takes `stub_0` once and is then patched to the next TB: `0x430 − (0x3db + 5) = 0x50`.
- Unlinking (SMC, D49) writes the old stub displacement back.

## 2. A return through the jump cache

`ret` (`c.jr ra`) at 0x1822a is a whole TB by itself. `ra` (x1) is pinned in R13. The jump cache is `CpuState.jmp_cache[4096]` of `{pc, host}` at 0x300 (D31). The slot index is `(pc >> 1) & 4095`, so the byte offset is `(pc << 3) & 0xFFF0`.
```
IR:  v0 = x1 ; v2 = and v0, -2 ; jump [v2]

+0d0  49 83 e9 01              sub  r9, 1
+0d4  0f 8c 2b 00 00 00        jl   budget_stub
+0da  4c 89 e8                 mov  rax, r13                ; target = ra
+0dd  48 83 e0 fe              and  rax, -2                 ; & ~1 (JALR semantics)
+0e1  49 89 c2                 mov  r10, rax
+0e4  49 c1 e2 03              shl  r10, 3
+0e8  41 81 e2 f0 ff 00 00     and  r10d, 0xfff0            ; entry byte offset
+0ef  4a 3b 84 15 80 02 00 00  cmp  rax, [rbp+r10+0x280]    ; jmp_cache[i].pc   (0x300 − 0x80)
+0f7  0f 85 2b 00 00 00        jne  miss_stub
+0fd  42 ff a4 15 88 02 00 00  jmp  qword [rbp+r10+0x288]   ; jmp_cache[i].host: straight into the caller's TB
; miss_stub:
+128  48 89 85 80 00 00 00     mov  [rbp+0x80], rax         ; pc = target
+12f  c7 85 90 00 00 00 06 00 00 00   mov dword [rbp+0x90], 6   ; exit_reason = LOOKUP
+139  b8 be 05 00 00           mov  eax, 0x5be              ; (tb 367 << 2) | 2
+13e  e9 0d 2f ff ff           jmp  exit_jit
```
- **Hit:** 8 instructions, then an indirect `jmp`. The dispatcher is never involved.
- **Miss:** the dispatcher translates the target (or finds it) and fills the entry before entering it, so the next return to the same place hits.
- This TB was itself reached by a chained `jmp` from `tb_18224` (`e9 58 00 00 00`, from links.txt).
- In system mode, the compared value is tagged with the TB flags (`pc ^ flags << 56`, D51). This is one extra `movabs r11, tag; xor r11, rax`, so an entry filled at one privilege level can never hit at another.

## 3. The same loop under softmmu: the inline TLB probe

With `--mem=softmmu`, the load at 0x10990 no longer uses `[rbx+rax]`. It probes the software TLB inline (D48; §14.4). User mode uses MMU index 0 (U), so the TLB is at `0x10580 − 0x80 = 0x10500` from RBP. Each entry is 32 bytes, `{addr_read, addr_write, addr_code, addend}`.
```
+53a  48 8b 45 f0              mov  rax, [rbp-0x10]           ; v0 = x14
+53e  4c 8d 18                 lea  r11, [rax]                ; guest vaddr (base + 0)
+541  4d 89 da                 mov  r10, r11
+544  49 c1 ea 07              shr  r10, 7                    ; 12 (page) − 5 (log2 32-byte entry)
+548  41 81 e2 e0 1f 00 00     and  r10d, 0x1fe0              ; (vpage & 255) * 32
+54f  49 81 e3 07 f0 ff ff     and  r11, 0xfffffffffffff007   ; page | misalignment bits (8-byte access)
+556  4e 3b 9c 15 00 05 01 00  cmp  r11, [rbp+r10+0x10500]    ; tlb[0][i].addr_read
+55e  0f 85 2c 00 00 00        jne  slow_path (+590)          ; miss, misaligned, MMIO: all take the cold path
+564  4c 8d 18                 lea  r11, [rax]                ; vaddr again (base register untouched)
+567  4e 03 9c 15 18 05 01 00  add  r11, [rbp+r10+0x10518]    ; + tlb[0][i].addend → host address
+56f  49 8b 0b                 mov  rcx, [r11]                ; the load (fault site for the state map)
+572  49 89 cf                 mov  r15, rcx                  ; .Lret: continues exactly as in §1
```
The hit path is 9 instructions (tag check included) plus the access itself. There is no extra memory traffic beyond the one TLB entry.

**Slow path** (cold, at the TB tail):
```
+590..+5ba  mov [rbp+0x10480..], rax/rcx/rdx/rsi/rdi/r8/r9   ; save caller-saved regs to cpu.fault_regs
+5c1  mov  rsi, [rbp+0x10480] ; lea rsi, [rsi]                ; arg va = saved base + offset
+5cb  mov  edx, 8                                              ; arg info = size 8, load, mmu index 0
+5d0  lea  r10, [r9+3] ; mov [rbp+0x88], r10                  ; cpu.budget = R9 + refund of the 3 unretired insns
+5db  lea  rdi, [rbp-0x80]                                     ; arg cpu (unbiased)
+5df  ff 15 23 ea ff ff        call qword [B+0x8]              ; helper table slot 1: helper_mmu_access
+5e5  mov  r11, rax ; restore rax..r9 from fault_regs
+619  83 bd 90 00 00 00 00     cmp  dword [rbp+0x90], 0        ; exit_reason set? (page fault)
+620  0f 85 08 00 00 00        jne  fault
+626  4c 89 d9                 mov  rcx, r11                   ; value → the load's destination register
+629  e9 44 ff ff ff           jmp  +572                       ; back to .Lret
fault: store r12..r15 to fault_regs; fault_rip = +56f (the site); eax = 0x12; jmp exit_jit
```
- The helper walks the Sv39/Sv48 page table on a miss, fills the entry and returns the value.
- On a page fault, the stub records the site's host RIP. The dispatcher then applies that site's **state map** (where every dirty guest register lives at +56f). So the guest sees a precise `scause = 13` at pc 0x10990, and the hot path carries no bookkeeping for it.
- Helpers are called through the absolute-address table at the start of the code buffer (`call [rip+d32]`, §8.4), because Rust code can be more than 2 GiB away from the buffer.

## 4. The code buffer, drawn from these addresses
```
B+0x000  helper table: [0] helper_interp_one  [1] helper_mmu_access     (8 bytes each)
B+0x010  enter_jit      push rbp,rbx,r12–r15; sub rsp,8; lea rbp,[rdi+128]; load rbx, r12–r15, r9, MXCSR (D47); jmp rsi
B+0x050  exit_jit       store r12–r15, r9 → CpuState; add rsp,8; pop …; ret   (every stub jumps here)
B+…      fault_exit     (SIGSEGV handler resumes here)
B+0x3b0  TB 4  (0x10990)  prologue · hot body · 2 exit jumps (rel32 aligned) · cold stubs
B+0x430  TB for 0x10996    ← slot 0 of TB 4 now jumps here directly
…        each TB starts on a 16-byte boundary; bump allocation; full → flush all + generation++
```
**Rust-side metadata for the same TB:**
- `tb_map[(0x10990, flags)] = 4`
- `exits[1] = {patch_off +3d4, target 0x10990, linked 4}`
- `exits[0] = {patch_off +3dc, target 0x10996, linked = the TB at +430}`
- `incoming(4)` contains (TB 0x1094e, slot 1) and (TB 4, slot 1), its own loop branch
- `page_tbs[0x10] ∋ 4`, which is what SMC invalidation walks (D49).

## 5. Encoding checks
`tests/emitter_golden.rs::interview_examples_28_5` runs the CLAUDE.md §28.5 worked examples through the real emitter and checks the bytes:

| Example | Bytes |
|---|---|
| `jne rel32` at B+0x10 → B+0x200 | `0F 85 EA 01 00 00` |
| `cmp r14, rsi` | `49 39 F6` |
| `jmp rel32` at B+0x1040 → B+0x2000 | `E9 BB 0F 00 00` |
| the same jump emitted as a chainable exit | `0F 1F 00 E9 B8 0F 00 00` |
| a chain patch through `write_rel32` | (checked by the same test) |

For the chainable exit, the emitter pads with **one 3-byte NOP**. The `E9` then lands at 0x1043 and the rel32 at 0x1044 (aligned), so rel = 0x2000 − 0x1048 = 0x0FB8.
