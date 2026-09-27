//! The JIT engine: dispatcher loop, translation, chaining decisions, the jump cache, exit
//! handling and host-fault resolution (CLAUDE.md §5, P2.7, Phase 3).
//!
//! The dispatcher enters JIT code with an instruction budget (D12). Chained TBs and jump-cache
//! hits run without returning; control comes back on an unlinked exit, a jump-cache miss, an
//! exhausted budget, or a special exit (ECALL, exception, FENCE.I, host fault). After every
//! return `icount += budget_ref - budget` (D30). A direct exit (slot 0/1) that came back
//! unlinked is linked to its successor on the next dispatch.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::backend::x86::features::{self, HostFeatures};
use crate::backend::x86::lower::{self, ExitInfo, LowerOptions};
use crate::backend::x86::lower_ir::{self, FaultSite, IrOptions};
use crate::backend::x86::regs::Reg;
use crate::ir::lift::{LiftOptions, lift_with};
use crate::ir::opt::optimize;
use crate::regalloc::linear_scan::{DLoc, OutOfSlots};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cpu::state::{CpuState, JcEntry, exit, jc_index};
use crate::cpu::trap::Exception;
use crate::interp::{
    Block, BlockExit, Engine, Env, Stop, build_block_max, build_block_soft, deliver,
    deliver_interrupt, exec_block, tohost_written,
};
use crate::isa::disasm;
use crate::isa::inst::{Inst, LoadOp, StoreOp};
use crate::mem::direct::DirectMem;
use crate::mem::{Access, MemFault, mmu, tlb};
use crate::user::signal;

use super::cache::{ExitSlot, TbCache, TbKey, TranslationBlock};
use super::chain;
use super::code_mem::{CodeMem, Full, WxMode};
use super::perfmap::PerfMap;
use super::trampoline::Trampolines;

/// `--regalloc` (§10): how guest registers are mapped to host registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegAlloc {
    /// Phase 3 lowering: every guest register in `CpuState`, no IR.
    None,
    /// IR without optimization passes; pinned R12–R15; every other guest register is stored
    /// as soon as it is written.
    Pinned,
    /// IR + optimizer passes + linear-scan allocation with lazy write-back.
    Linear,
}

/// JIT configuration (CLI flags of §23).
#[derive(Clone, Debug)]
pub struct JitOptions {
    /// `--max-block`: instruction limit per TB.
    pub max_block: usize,
    /// `--code-cache`: code buffer size in bytes (≤ 1 GiB).
    pub code_cache: usize,
    /// `--wx`
    pub wx: WxMode,
    /// `--no-host-features` clears this: only baseline x86-64 is used.
    pub host_features: bool,
    /// `--dump-x86 DIR`: write every TB's host code to DIR.
    pub dump_x86: Option<PathBuf>,
    /// `--perf-map`
    pub perf_map: bool,
    /// Test-only miscompilation (see `LowerOptions::inject_bug`).
    pub inject_bug: bool,
    /// `--no-chain` clears this: no exit is ever linked and the jump cache is never filled,
    /// so every TB returns to the dispatcher (the Phase 2 behaviour, for measurements).
    pub chain: bool,
    /// `--profile-jit`: JIT code counts JALR executions (jump-cache hit rate).
    pub profile: bool,
    /// Instructions per dispatcher slice (D12).
    pub slice: u64,
    /// `--regalloc`
    pub regalloc: RegAlloc,
    /// `--pin`: guest registers pinned to R12, R13, R14, R15 in order (at most 4).
    pub pin: Vec<u8>,
    /// `--dump-ir DIR`: write every TB's IR before and after the passes.
    pub dump_ir: Option<PathBuf>,
    /// `--profile-tbs`: sample the host RIP (SIGPROF, 1 kHz of CPU time) and report the
    /// hottest TBs in `stats` (D44).
    pub profile_tbs: bool,
    /// `--no-inline-fp` clears this: every FP instruction runs through `helper_interp_one`
    /// (the Phase 5 behaviour, for measurements; D47).
    pub inline_fp: bool,
}

impl Default for JitOptions {
    fn default() -> Self {
        JitOptions {
            max_block: 128,
            code_cache: 256 << 20,
            wx: WxMode::DualMap,
            host_features: true,
            dump_x86: None,
            perf_map: false,
            inject_bug: false,
            chain: true,
            profile: false,
            slice: 100_000,
            regalloc: RegAlloc::Linear,
            pin: vec![2, 1, 10, 15],
            dump_ir: None,
            profile_tbs: false,
            inline_fp: true,
        }
    }
}

/// Counters for `--stats`.
#[derive(Clone, Debug, Default)]
pub struct JitStats {
    pub translated: u64,
    pub code_bytes: u64,
    pub guest_insns_translated: u64,
    pub full_flushes: u64,
    pub fence_flushes: u64,
    /// Dispatcher → JIT entries.
    pub entries: u64,
    /// Softmmu: page-straddling instructions run in the interpreter.
    pub interpreted: u64,
    /// Guest instructions retired in JIT code.
    pub retired: u64,
    /// Exits by `exit_reason` (`exit::*`).
    pub exits: [u64; exit::COUNT],
    pub chain_links: u64,
    pub chain_unlinks: u64,
    /// Jump-cache entries written by the dispatcher.
    pub jc_fills: u64,
    /// JALR executions (`--profile-jit` only).
    pub jalr: u64,
    /// Register-allocator activity summed over all translations.
    pub fills: u64,
    pub spills: u64,
    pub writebacks: u64,
    pub moves: u64,
    /// TBs retranslated shorter because they needed too many spill slots.
    pub retranslations: u64,
    pub translate_time: Duration,
}

/// Distinguishes Jit instances in `CpuState::jc_tag`.
static NEXT_JIT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Jit {
    cm: CodeMem,
    tr: Trampolines,
    cache: TbCache,
    opts: JitOptions,
    pub features: HostFeatures,
    perf: Option<PerfMap>,
    pub stats: JitStats,
    id: u64,
    /// Bumped whenever jump-cache contents may be stale (flush, invalidation).
    jc_version: u64,
    /// The last exit, if it was an unlinked direct exit: (tb, slot, cache generation).
    last_exit: Option<(u32, u8, u64)>,
    /// Guest register → pinned host register (empty with `RegAlloc::None`).
    pinned: [Option<Reg>; 32],
    /// Softmmu: (TB flags, `CpuState::mmu_gen`) the jump-cache contents were filled under.
    jc_ctx: (u8, u64),
}

/// `TbKey::flags` bit marking a softmmu translation (D48).
pub const FLAG_SOFT: u8 = 0x80;

/// Where the dispatcher continues at `cpu.pc` (`Jit::select`).
pub enum Next {
    /// Run this TB.
    Tb(u32),
    /// A 32-bit instruction straddles a page boundary (softmmu): interpret it.
    Straddle,
    /// Fetching at `cpu.pc` faults.
    Fault(Exception),
}

/// Output of either back end.
struct Translated {
    code: Vec<u8>,
    pcmap: Vec<crate::jit::cache::PcEntry>,
    exits: [Option<ExitInfo>; 2],
    fault_sites: Vec<FaultSite>,
    stats: crate::regalloc::linear_scan::AllocStats,
}

impl Jit {
    pub fn new(opts: JitOptions) -> io::Result<Jit> {
        let mut cm = CodeMem::new(opts.code_cache, opts.wx)?;
        const PIN_REGS: [Reg; 4] = [Reg::R12, Reg::R13, Reg::R14, Reg::R15];
        if opts.pin.len() > 4 || opts.pin.iter().any(|&g| g == 0 || g >= 32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--pin takes at most 4 registers from x1..x31",
            ));
        }
        let mut pinned = [None; 32];
        let mut pairs = Vec::new();
        if opts.regalloc != RegAlloc::None {
            for (&g, &r) in opts.pin.iter().zip(PIN_REGS.iter()) {
                if pinned[g as usize].is_none() {
                    pinned[g as usize] = Some(r);
                    pairs.push((g, r));
                }
            }
        }
        // The IR back end keeps the budget in a register (D46); the naive one in memory.
        let budget_reg =
            (opts.regalloc != RegAlloc::None).then_some(crate::backend::x86::regs::BUDGET_REG);
        let tr = Trampolines::generate(&mut cm, &pairs, budget_reg);
        let features = if opts.host_features {
            features::host()
        } else {
            HostFeatures::baseline()
        };
        let perf = if opts.perf_map {
            Some(PerfMap::open()?)
        } else {
            None
        };
        for d in opts.dump_x86.iter().chain(opts.dump_ir.iter()) {
            std::fs::create_dir_all(d)?;
        }
        signal::install();
        if opts.profile_tbs {
            signal::start_sampling(1000);
        }
        Ok(Jit {
            cm,
            tr,
            cache: TbCache::default(),
            opts,
            features,
            perf,
            stats: JitStats::default(),
            id: NEXT_JIT_ID.fetch_add(1, Ordering::Relaxed),
            jc_version: 0,
            last_exit: None,
            pinned,
            jc_ctx: (0, 0),
        })
    }

    pub fn trampolines(&self) -> &Trampolines {
        &self.tr
    }

    pub fn options(&self) -> &JitOptions {
        &self.opts
    }

    pub fn tb(&self, id: u32) -> &TranslationBlock {
        self.cache.get(id)
    }

    /// Host code of a TB.
    pub fn tb_code(&self, id: u32) -> &[u8] {
        let tb = self.cache.get(id);
        self.cm.read(tb.host, tb.host_len as usize)
    }

    /// Host bytes at RX address `addr` (tests, dumps).
    pub fn code_at(&self, addr: u64, len: usize) -> &[u8] {
        self.cm.read(addr, len)
    }

    /// The fast-FP-variant TB for guest `pc`, translating it on a miss.
    pub fn tb_for(&mut self, pc: u64, mem: &DirectMem) -> u32 {
        self.tb_for_variant(pc, mem, false)
    }

    /// The TB for guest `pc` in the given FP variant (D47), translating it on a miss (direct
    /// memory).
    pub fn tb_for_variant(&mut self, pc: u64, mem: &DirectMem, fp_slow: bool) -> u32 {
        let key = TbKey::direct(pc, self.variant(fp_slow));
        self.tb_for_key(key, None, |max| build_block_max(pc, mem, max))
    }

    /// Only the IR back end has two FP variants; without inline FP only the slow one is used.
    fn variant(&self, fp_slow: bool) -> bool {
        (fp_slow || !self.opts.inline_fp) && self.opts.regalloc != RegAlloc::None
    }

    /// Softmmu (D48): the TB for `cpu.pc` under the current privilege, MMU state and FP state,
    /// whose code was fetched from physical page `ppage`.
    pub fn tb_for_soft(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, ppage: u64) -> u32 {
        let didx = tlb::data_idx(cpu);
        let key = TbKey {
            pc: cpu.pc,
            slow: self.variant(fp_slow(cpu)),
            flags: FLAG_SOFT | tlb::fetch_idx(cpu) | didx << 2,
            ppage,
        };
        let pc = cpu.pc;
        self.tb_for_key(key, Some(didx), |max| build_block_soft(pc, cpu, mem, max))
    }

    fn tb_for_key(
        &mut self,
        key: TbKey,
        soft: Option<u8>,
        build: impl FnOnce(usize) -> Block,
    ) -> u32 {
        if let Some(id) = self.cache.lookup_key(&key) {
            return id;
        }
        let pc = key.pc;
        let t0 = Instant::now();
        let block = build(self.opts.max_block);
        let (mut insns, mut fetch_fault) = (block.insns, block.fetch_fault);
        let (host, out) = loop {
            let origin = self.cm.next_addr();
            let id = self.cache.next_id();
            let out = match self.translate_insns(&insns, fetch_fault, key, origin, id, soft) {
                Ok(out) => out,
                Err(OutOfSlots) => {
                    // Too many values live at once for the spill area: translate a shorter
                    // prefix (one instruction always fits).
                    assert!(
                        insns.len() > 1,
                        "a single instruction ran out of spill slots"
                    );
                    insns.truncate(insns.len() / 2);
                    fetch_fault = None;
                    self.stats.retranslations += 1;
                    continue;
                }
            };
            match self.cm.place(origin, &out.code) {
                Ok(host) => break (host, out),
                Err(Full) => {
                    assert!(
                        !self.cache.is_empty(),
                        "code cache too small for a single TB ({} bytes)",
                        out.code.len()
                    );
                    self.flush_all();
                    self.stats.full_flushes += 1;
                }
            }
        };
        let guest_bytes = insns.iter().map(|d| d.len as u32).sum();
        self.stats.translated += 1;
        self.stats.code_bytes += out.code.len() as u64;
        self.stats.guest_insns_translated += insns.len() as u64;
        self.stats.fills += out.stats.fills as u64;
        self.stats.spills += out.stats.spills as u64;
        self.stats.writebacks += out.stats.writebacks as u64;
        self.stats.moves += out.stats.moves as u64;
        if let Some(p) = &mut self.perf {
            p.record(host, out.code.len() as u32, pc);
        }
        if let Some(dir) = &self.opts.dump_x86 {
            let _ = std::fs::write(dir.join(format!("tb_{pc:016x}.bin")), &out.code);
            let _ = std::fs::write(
                dir.join(format!("tb_{pc:016x}.txt")),
                dump_tb_text(pc, &insns, host, &out.code),
            );
        }
        let exits = out.exits.map(|e| {
            e.map(|e| {
                let patch_at = host + e.patch_off as u64;
                // D7: the patch must be a single aligned 32-bit store.
                assert!(patch_at.is_multiple_of(4), "unaligned exit slot");
                ExitSlot {
                    patch_at,
                    stub: host + e.stub_off as u64,
                    target_pc: e.target_pc,
                    linked: None,
                }
            })
        });
        // A CSR write may change the MMU flags the successor must be looked up with (D48).
        let chainable = soft.is_none()
            || !insns
                .last()
                .is_some_and(|d| matches!(d.inst, Inst::Csr { .. }));
        let id = self.cache.insert(TranslationBlock {
            guest_pc: pc,
            insns,
            fetch_fault,
            guest_bytes,
            host,
            host_len: out.code.len() as u32,
            pcmap: out.pcmap,
            exits,
            incoming: Vec::new(),
            valid: true,
            fault_sites: out.fault_sites,
            key,
            chainable,
        });
        self.stats.translate_time += t0.elapsed();
        id
    }

    /// Run the selected back end over `insns` for placement at `origin`.
    fn translate_insns(
        &self,
        insns: &[crate::isa::Decoded],
        fetch_fault: Option<Exception>,
        key: TbKey,
        origin: u64,
        id: u32,
        soft: Option<u8>,
    ) -> Result<Translated, OutOfSlots> {
        let (pc, fp_slow) = (key.pc, key.slow);
        if self.opts.regalloc == RegAlloc::None {
            let lopts = LowerOptions {
                inject_bug: self.opts.inject_bug,
                profile: self.opts.profile,
                softmmu: soft.is_some(),
            };
            let o = lower::translate(insns, fetch_fault, pc, origin, id, &self.tr, lopts);
            return Ok(Translated {
                code: o.code,
                pcmap: o.pcmap,
                exits: o.exits,
                fault_sites: Vec::new(),
                stats: Default::default(),
            });
        }
        let lopts = LiftOptions {
            inline_fp: !fp_slow,
            fma: self.features.fma,
        };
        let mut ir = lift_with(insns, fetch_fault, pc, lopts);
        let before = self.opts.dump_ir.as_ref().map(|_| ir.to_string());
        if self.opts.regalloc == RegAlloc::Linear {
            optimize(&mut ir);
        }
        if let (Some(dir), Some(before)) = (&self.opts.dump_ir, before) {
            let _ = std::fs::write(
                dir.join(format!("tb_{pc:016x}.ir")),
                format!("; lifted\n{before}\n; optimized\n{ir}"),
            );
        }
        let iopts = IrOptions {
            lazy: self.opts.regalloc == RegAlloc::Linear,
            bmi2: self.features.bmi2,
            profile: self.opts.profile,
            pinned: self.pinned,
            inject_bug: self.opts.inject_bug,
            softmmu: soft,
        };
        let o = lower_ir::translate(&ir, origin, id, &self.tr, iopts)?;
        Ok(Translated {
            code: o.code,
            pcmap: o.pcmap,
            exits: o.exits,
            fault_sites: o.fault_sites,
            stats: o.stats,
        })
    }

    /// The TB to run next at guest `pc` (direct memory): `tb_for_variant`, then link the
    /// previous unlinked direct exit to it (§13.3).
    pub fn next_tb(&mut self, pc: u64, mem: &DirectMem, fp_slow: bool) -> u32 {
        let id = self.tb_for_variant(pc, mem, fp_slow);
        self.link_last(id);
        id
    }

    /// What to run at `cpu.pc`, in either memory mode, linking the previous exit to it.
    pub fn select(&mut self, cpu: &mut CpuState, mem: &mut DirectMem) -> Next {
        if cpu.softmmu == 0 {
            return Next::Tb(self.next_tb(cpu.pc, mem, fp_slow(cpu)));
        }
        // The jump cache maps virtual pcs to TBs of one flags value and one translation
        // regime: start over when either changes (D48).
        let flags = FLAG_SOFT | tlb::fetch_idx(cpu) | tlb::data_idx(cpu) << 2;
        if self.jc_ctx != (flags, cpu.mmu_gen) {
            self.jc_ctx = (flags, cpu.mmu_gen);
            self.jc_version += 1;
        }
        let pc = cpu.pc;
        let ppage = match mmu::fetch_page(cpu, mem, pc) {
            Ok(p) => p,
            Err(e) => {
                self.last_exit = None;
                return Next::Fault(e);
            }
        };
        if pc & 0xfff == 0xffe && mmu::fetch16(cpu, mem, pc).is_ok_and(|h| h & 3 == 3) {
            self.last_exit = None;
            return Next::Straddle;
        }
        let id = self.tb_for_soft(cpu, mem, ppage);
        self.link_last(id);
        Next::Tb(id)
    }

    /// Link the previous unlinked direct exit to TB `id` if allowed (§13.3): in softmmu mode
    /// only within one virtual page, between TBs of the same flags, and never after a CSR
    /// instruction (D48).
    fn link_last(&mut self, id: u32) {
        let Some((from, slot, generation)) = self.last_exit.take() else {
            return;
        };
        // A flush inside `tb_for` renumbers TBs: `from` would be stale.
        if generation != self.cache.generation || !self.opts.chain {
            return;
        }
        let (f, t) = (self.cache.get(from), self.cache.get(id));
        let soft = f.key.flags & FLAG_SOFT != 0;
        if !f.chainable
            || f.key.flags != t.key.flags
            || (soft && f.guest_pc >> 12 != t.guest_pc >> 12)
        {
            return;
        }
        if chain::link(&mut self.cache, &mut self.cm, from, slot, id) {
            self.stats.chain_links += 1;
            if self.opts.dump_x86.is_some() {
                self.log_link(from, slot, id);
            }
        }
    }

    /// `--dump-x86`: record a chain patch in `links.txt` (the patched instruction's bytes).
    fn log_link(&self, from: u32, slot: u8, to: u32) {
        use std::io::Write;
        let (f, t) = (self.cache.get(from), self.cache.get(to));
        let ex = f.exits[slot as usize].expect("linked slot");
        let op_len = if slot == 0 { 1 } else { 2 };
        let bytes: Vec<String> = self
            .cm
            .read(ex.patch_at - op_len, op_len as usize + 4)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let dir = self.opts.dump_x86.as_ref().expect("checked by caller");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("links.txt"))
        {
            let _ = writeln!(
                file,
                "tb_{:x} slot {slot} @{:#x}: {} -> tb_{:x} @{:#x} (stub was {:#x})",
                f.guest_pc,
                ex.patch_at - op_len,
                bytes.join(" "),
                t.guest_pc,
                t.host,
                ex.stub
            );
        }
    }

    fn jc_tag(&self) -> u64 {
        self.id << 48 | (self.jc_version & 0xFFFF_FFFF_FFFF)
    }

    /// Link every unlinked direct exit of TB `id` that targets the TB's own start to itself,
    /// as the dispatcher does the first time such an exit is taken. Tests use it to run a
    /// self-looping TB for several iterations in one `exec`.
    pub fn link_self_exits(&mut self, id: u32) {
        let pc = self.cache.get(id).guest_pc;
        for slot in 0..2u8 {
            let own = self.cache.get(id).exits[slot as usize]
                .is_some_and(|e| e.target_pc == pc && e.linked.is_none());
            if own && chain::link(&mut self.cache, &mut self.cm, id, slot, id) {
                self.stats.chain_links += 1;
            }
        }
    }

    /// Enter JIT code at TB `id` with `budget` instructions (D12) and run until control comes
    /// back. Returns how execution ended; `cpu.pc` and `cpu.icount` are exact.
    pub fn exec(
        &mut self,
        cpu: &mut CpuState,
        mem: &mut DirectMem,
        id: u32,
        budget: i64,
    ) -> BlockExit {
        let (host, guest_pc) = {
            let tb = self.cache.get(id);
            (tb.host, tb.guest_pc)
        };
        // The jump cache holds host addresses: drop it if it belongs to another Jit or to code
        // that has since been flushed or invalidated.
        let tag = self.jc_tag();
        if cpu.jc_tag != tag {
            cpu.clear_jump_cache();
            cpu.jc_tag = tag;
        }
        if self.opts.chain {
            let e = &mut cpu.jmp_cache[jc_index(guest_pc)];
            if e.pc != guest_pc || e.host != host {
                *e = JcEntry { pc: guest_pc, host };
                self.stats.jc_fills += 1;
            }
        }
        cpu.budget = budget;
        cpu.budget_ref = budget;
        cpu.exit_reason = exit::NONE;
        cpu.mem_base = mem.base() as u64;
        cpu.helper_mem = mem as *mut DirectMem as u64;
        let start = self.cm.rx_base();
        signal::set_jit_range(start, start + self.cm.size() as u64, self.tr.fault_exit);
        self.stats.entries += 1;
        let icount_before = cpu.icount;
        // SAFETY: `host` is a TB placed in this Jit's code buffer (still mapped: we own it),
        // every chained target and jump-cache entry points into the current generation of
        // that buffer (tag check above), and mem_base/helper_mem were just set to the live
        // guest memory.
        let code = unsafe { self.tr.enter(cpu, host) };
        signal::clear_jit_range();
        let reason = cpu.exit_reason;
        if let Some(n) = self.stats.exits.get_mut(reason as usize) {
            *n += 1;
        }
        let exit = match reason {
            exit::NONE | exit::BUDGET | exit::LOOKUP | exit::FP_VARIANT => BlockExit::Continue,
            exit::ECALL => BlockExit::Ecall,
            exit::EXCEPTION => BlockExit::Trap(Exception {
                cause: cpu.exc_cause,
                tval: cpu.exc_tval,
            }),
            exit::FLUSH => BlockExit::Flush,
            exit::HOST_FAULT => BlockExit::Trap(self.resolve_host_fault(cpu, mem)),
            exit::MMU_FAULT => BlockExit::Trap(self.resolve_mmu_fault(cpu)),
            r => panic!("JIT exit with unknown reason {r} at pc {:#x}", cpu.pc),
        };
        // After the host-fault refund: everything charged and not refunded has retired.
        // helper_interp_one folds its share into icount on the way (D30), so count the delta.
        cpu.icount += (cpu.budget_ref - cpu.budget) as u64;
        self.stats.retired += cpu.icount - icount_before;
        self.stats.jalr += std::mem::take(&mut cpu.prof_jalr);
        let slot = (code & 3) as u8;
        self.last_exit = (slot < 2 && reason == exit::NONE).then_some((
            (code >> 2) as u32,
            slot,
            self.cache.generation,
        ));
        exit
    }

    /// A softmmu slow path raised an exception (`exc_cause`/`exc_tval`): make the guest state
    /// precise at the faulting access from its fault site (D48).
    fn resolve_mmu_fault(&self, cpu: &mut CpuState) -> Exception {
        let rip = cpu.fault_rip;
        let tb = self
            .cache
            .find_host(rip)
            .unwrap_or_else(|| panic!("softmmu fault at {rip:#x} outside any TB"));
        apply_site(tb, cpu, rip);
        Exception {
            cause: cpu.exc_cause,
            tval: cpu.exc_tval,
        }
    }

    /// Turn a host SIGSEGV in JIT code into a precise guest exception: find the guest
    /// instruction through the TB's `pcmap`, restore `pc`, refund the unexecuted part of the
    /// TB's budget charge, and compute the fault address the interpreter would report.
    fn resolve_host_fault(&self, cpu: &mut CpuState, mem: &DirectMem) -> Exception {
        let rip = cpu.fault_rip;
        let tb = self
            .cache
            .find_host(rip)
            .unwrap_or_else(|| panic!("host fault at {rip:#x} outside any TB"));
        if !tb.fault_sites.is_empty() {
            return resolve_site(tb, cpu, mem, rip);
        }
        let e = tb.entry_for(rip).expect("pcmap covers the TB");
        let d = tb.insns[e.idx as usize];
        cpu.pc = e.guest_pc;
        // Instructions idx.. were charged by the prologue but did not retire.
        cpu.budget += (tb.insns.len() as u32 - e.idx) as i64;
        let (rs1, imm, size, access) = match d.inst {
            Inst::Load { op, rs1, imm, .. } => {
                let size = match op {
                    LoadOp::Lb | LoadOp::Lbu => 1,
                    LoadOp::Lh | LoadOp::Lhu => 2,
                    LoadOp::Lw | LoadOp::Lwu => 4,
                    LoadOp::Ld => 8,
                };
                (rs1, imm, size, Access::Load)
            }
            Inst::Store { op, rs1, imm, .. } => {
                let size = match op {
                    StoreOp::Sb => 1,
                    StoreOp::Sh => 2,
                    StoreOp::Sw => 4,
                    StoreOp::Sd => 8,
                };
                (rs1, imm, size, Access::Store)
            }
            _ => panic!(
                "host fault at {rip:#x} in non-memory instruction {} (pc {:#x})",
                disasm(&d.inst),
                e.guest_pc
            ),
        };
        let addr = cpu.x[rs1 as usize].wrapping_add(imm as u64);
        // A split access faults at the first inaccessible byte, like DirectMem::check.
        let g = cpu.fault_addr.wrapping_sub(mem.base() as u64);
        let tval = if g.wrapping_sub(addr) < size { g } else { addr };
        Exception::from(MemFault { access, addr: tval })
    }

    /// Invalidate the TB for guest `pc` (Phase 8 uses this for self-modifying code): unlink
    /// every exit chained into it, drop it from the map and stale the jump cache. Returns
    /// false if no TB exists for `pc`.
    pub fn invalidate_pc(&mut self, pc: u64) -> bool {
        let mut any = false;
        for slow in [false, true] {
            let Some(id) = self.cache.lookup(pc, slow) else {
                continue;
            };
            self.stats.chain_unlinks +=
                chain::unlink_incoming(&mut self.cache, &mut self.cm, id) as u64;
            self.cache.invalidate(id);
            if self.last_exit.is_some_and(|(from, _, _)| from == id) {
                self.last_exit = None;
            }
            any = true;
        }
        if any {
            self.jc_version += 1;
        }
        any
    }

    /// Softmmu: run the single instruction at `cpu.pc` in the interpreter (one that straddles
    /// a page boundary, whose translation would depend on two pages).
    pub fn interpret_one(&mut self, cpu: &mut CpuState, mem: &mut DirectMem) -> BlockExit {
        let pc = cpu.pc;
        let b = build_block_soft(pc, cpu, mem, 1);
        self.stats.interpreted += 1;
        exec_block(cpu, mem, &b.insns, b.fetch_fault, false)
    }

    fn flush_all(&mut self) {
        self.cache.flush();
        self.cm.reset();
        self.jc_version += 1;
        self.last_exit = None;
    }
}

impl Engine for Jit {
    fn run(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, env: &Env, max_insns: u64) -> Stop {
        let limit = cpu.icount.saturating_add(max_insns);
        loop {
            if cpu.icount >= limit {
                return Stop::Limit;
            }
            if let Some(v) = tohost_written(mem, env) {
                return Stop::Tohost(v);
            }
            if cpu.softmmu != 0 {
                deliver_interrupt(cpu, env);
            }
            let exit = match self.select(cpu, mem) {
                Next::Tb(id) => {
                    // The first TB always runs whole, like an interpreter block, so a budget
                    // smaller than it cannot stall progress; chained TBs then respect the
                    // remaining budget.
                    let n = self.cache.get(id).insns.len() as u64;
                    let budget = (limit - cpu.icount).min(self.opts.slice).max(n);
                    self.exec(cpu, mem, id, budget as i64)
                }
                Next::Straddle => self.interpret_one(cpu, mem),
                Next::Fault(e) => BlockExit::Trap(e),
            };
            if exit == BlockExit::Flush {
                self.flush();
            }
            if let Err(stop) = deliver(exit, env, cpu) {
                return stop;
            }
        }
    }

    fn flush(&mut self) {
        self.stats.fence_flushes += 1;
        self.flush_all();
    }

    fn stats(&self) -> String {
        let profile = if self.opts.profile_tbs {
            format!("; {}", self.profile_report())
        } else {
            String::new()
        };
        self.stats_counters() + &profile
    }
}

/// Which FP variant (D47) matches the current state: the fast one needs FS = Dirty and frm = RNE.
pub fn fp_slow(cpu: &CpuState) -> bool {
    let fs = crate::cpu::csr::mstatus::FS;
    cpu.csr.mstatus & fs != fs || cpu.frm != 0
}

impl Jit {
    /// `--profile-tbs`: stop sampling and attribute the samples (D44). Samples in the code
    /// buffer but in no TB are the trampolines; samples outside it are Rust (dispatcher,
    /// translator, helpers) and the kernel (syscalls).
    fn profile_report(&self) -> String {
        let (rips, dropped) = signal::stop_sampling();
        let (lo, hi) = (self.cm.rx_base(), self.cm.rx_base() + self.cm.size() as u64);
        let mut by_tb: rustc_hash::FxHashMap<u64, (u64, usize)> = Default::default();
        let (mut in_tb, mut tramp) = (0u64, 0u64);
        for &rip in &rips {
            if let Some(tb) = self.cache.find_host(rip) {
                in_tb += 1;
                by_tb.entry(tb.guest_pc).or_insert((0, tb.insns.len())).0 += 1;
            } else if (lo..hi).contains(&rip) {
                tramp += 1;
            }
        }
        let n = rips.len().max(1) as f64;
        let pct = |x: u64| 100.0 * x as f64 / n;
        let mut top: Vec<(u64, (u64, usize))> = by_tb.into_iter().collect();
        top.sort_by_key(|&(pc, (c, _))| (std::cmp::Reverse(c), pc));
        let list: Vec<String> = top
            .iter()
            .take(12)
            .map(|&(pc, (c, len))| format!("{pc:#x} {:.1}% ({len} insns)", pct(c)))
            .collect();
        format!(
            "profile: {} samples ({dropped} dropped{}), {:.1}% in translated code, {:.1}% in \
             trampolines, {:.1}% elsewhere (dispatcher, translator, helpers, kernel); hottest \
             TBs: {}",
            rips.len(),
            if self.stats.full_flushes > 0 {
                "; code was flushed, attribution approximate"
            } else {
                ""
            },
            pct(in_tb),
            pct(tramp),
            pct(rips.len() as u64 - in_tb - tramp),
            list.join(", ")
        )
    }

    fn stats_counters(&self) -> String {
        let s = &self.stats;
        let per_m = |x: u64| x as f64 * 1e6 / s.retired.max(1) as f64;
        let jc = if self.opts.profile {
            let misses = s.exits[exit::LOOKUP as usize];
            format!(
                "{} JALR, jump-cache hit rate {:.2}%",
                s.jalr,
                100.0 * s.jalr.saturating_sub(misses) as f64 / s.jalr.max(1) as f64
            )
        } else {
            "JALR count needs --profile-jit".into()
        };
        format!(
            "jit: {} TBs translated ({} guest insns, {} KiB host code, {:.1} bytes/insn), \
             {} full + {} code-change flushes, translate time {:.1} ms; \
             {} dispatcher entries ({:.0} per M guest insns); \
             exits: none {}, ecall {}, exception {}, flush {}, host-fault {}, budget {}, \
             jump-cache miss {}, fp-variant {}, mmu-fault {}, straddle {}; chain: {}, {} links, {} unlinks, {} jump-cache fills; {}; \
             regalloc {:?} (emitted code, all TBs): {} fills, {} spills, {} write-backs, \
             {} moves, {} retranslations",
            s.translated,
            s.guest_insns_translated,
            s.code_bytes / 1024,
            s.code_bytes as f64 / s.guest_insns_translated.max(1) as f64,
            s.full_flushes,
            s.fence_flushes,
            s.translate_time.as_secs_f64() * 1e3,
            s.entries,
            per_m(s.entries),
            s.exits[exit::NONE as usize],
            s.exits[exit::ECALL as usize],
            s.exits[exit::EXCEPTION as usize],
            s.exits[exit::FLUSH as usize],
            s.exits[exit::HOST_FAULT as usize],
            s.exits[exit::BUDGET as usize],
            s.exits[exit::LOOKUP as usize],
            s.exits[exit::FP_VARIANT as usize],
            s.exits[exit::MMU_FAULT as usize],
            s.interpreted,
            if self.opts.chain { "on" } else { "off" },
            s.chain_links,
            s.chain_unlinks,
            s.jc_fills,
            jc,
            self.opts.regalloc,
            s.fills,
            s.spills,
            s.writebacks,
            s.moves,
            s.retranslations,
        )
    }
}

/// Host fault in an IR-lowered TB: the fault site's state map says where every dirty guest
/// register was (host registers from the SIGSEGV snapshot, spill slots, constants or another
/// register's home); pinned registers were already stored by `exit_jit` (§15).
fn resolve_site(tb: &TranslationBlock, cpu: &mut CpuState, mem: &DirectMem, rip: u64) -> Exception {
    let site = apply_site(tb, cpu, rip);
    let regs = cpu.fault_regs;
    let addr = regs[site.addr.num() as usize].wrapping_add(site.off as i64 as u64);
    let size = site.size as u64;
    let g = cpu.fault_addr.wrapping_sub(mem.base() as u64);
    let tval = if g.wrapping_sub(addr) < size { g } else { addr };
    let access = if site.store {
        Access::Store
    } else {
        Access::Load
    };
    Exception::from(MemFault { access, addr: tval })
}

/// Make the guest state precise at the fault site at host `rip` (§15, D37): write the dirty
/// guest registers home from `cpu.fault_regs`/slots/constants, set `pc`, refund the budget of
/// the instructions that did not retire.
fn apply_site<'a>(tb: &'a TranslationBlock, cpu: &mut CpuState, rip: u64) -> &'a FaultSite {
    let off = (rip - tb.host) as u32;
    let site = tb
        .fault_sites
        .iter()
        .find(|s| s.rip_off == off)
        .unwrap_or_else(|| {
            panic!("host fault at {rip:#x} (TB offset {off:#x}) is not a memory access")
        });
    let regs = cpu.fault_regs;
    let vals: Vec<(u8, u64)> = site
        .dirty
        .iter()
        .map(|&(g, loc)| {
            let v = match loc {
                DLoc::Reg(r) => regs[r.num() as usize],
                DLoc::Slot(k) => cpu.spill[k as usize],
                DLoc::Const(c) => c,
                DLoc::Home(h) => cpu.x[h as usize],
            };
            (g, v)
        })
        .collect();
    for (g, v) in vals {
        cpu.set_x(g, v);
    }
    cpu.pc = site.pc;
    // Instructions idx.. were charged by the prologue but did not retire.
    cpu.budget += (tb.insns.len() as u32 - site.idx) as i64;
    site
}

/// Guest disassembly plus host code of one TB (`--dump-x86`, lockstep reports).
pub fn dump_tb_text(pc: u64, insns: &[crate::isa::Decoded], host: u64, code: &[u8]) -> String {
    let mut s = format!("TB guest {pc:#x}, host {host:#x}, {} bytes\n", code.len());
    let mut a = pc;
    for d in insns {
        s += &format!("  {a:#012x}: {:08x}  {}\n", d.raw, disasm(&d.inst));
        a = a.wrapping_add(d.len as u64);
    }
    s += &crate::backend::x86::disasm::disasm_x86(code, host);
    s
}
