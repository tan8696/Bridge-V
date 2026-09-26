//! The JIT engine: dispatcher loop, translation, exit handling and host-fault resolution
//! (CLAUDE.md §5, P2.7). Phase 2 has no chaining: every TB returns to the dispatcher.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::backend::x86::features::{self, HostFeatures};
use crate::backend::x86::lower::{self, LowerOptions};
use crate::cpu::state::{CpuState, exit};
use crate::cpu::trap::Exception;
use crate::interp::{BlockExit, Engine, Env, Stop, build_block_max, deliver, tohost_written};
use crate::isa::disasm;
use crate::isa::inst::{Inst, LoadOp, StoreOp};
use crate::mem::direct::DirectMem;
use crate::mem::{Access, MemFault};
use crate::user::signal;

use super::cache::{TbCache, TranslationBlock};
use super::code_mem::{CodeMem, Full, WxMode};
use super::perfmap::PerfMap;
use super::trampoline::Trampolines;

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
    pub entries: u64,
    /// Exits by `exit_reason` (NONE, ECALL, EXCEPTION, FLUSH, HOST_FAULT).
    pub exits: [u64; 5],
    pub translate_time: Duration,
}

pub struct Jit {
    cm: CodeMem,
    tr: Trampolines,
    cache: TbCache,
    opts: JitOptions,
    pub features: HostFeatures,
    perf: Option<PerfMap>,
    pub stats: JitStats,
}

impl Jit {
    pub fn new(opts: JitOptions) -> io::Result<Jit> {
        let mut cm = CodeMem::new(opts.code_cache, opts.wx)?;
        let tr = Trampolines::generate(&mut cm);
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
        if let Some(d) = &opts.dump_x86 {
            std::fs::create_dir_all(d)?;
        }
        signal::install();
        Ok(Jit {
            cm,
            tr,
            cache: TbCache::default(),
            opts,
            features,
            perf,
            stats: JitStats::default(),
        })
    }

    pub fn trampolines(&self) -> &Trampolines {
        &self.tr
    }

    pub fn tb(&self, id: u32) -> &TranslationBlock {
        self.cache.get(id)
    }

    /// Host code of a TB.
    pub fn tb_code(&self, id: u32) -> &[u8] {
        let tb = self.cache.get(id);
        self.cm.read(tb.host, tb.host_len as usize)
    }

    /// The TB for guest `pc`, translating it on a miss.
    pub fn tb_for(&mut self, pc: u64, mem: &DirectMem) -> u32 {
        if let Some(id) = self.cache.lookup(pc) {
            return id;
        }
        let t0 = Instant::now();
        let block = build_block_max(pc, mem, self.opts.max_block);
        let lopts = LowerOptions {
            inject_bug: self.opts.inject_bug,
        };
        let (host, out) = loop {
            let origin = self.cm.next_addr();
            let id = self.cache.next_id();
            let out = lower::translate(
                &block.insns,
                block.fetch_fault,
                pc,
                origin,
                id,
                &self.tr,
                lopts,
            );
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
        let guest_bytes = block.insns.iter().map(|d| d.len as u32).sum();
        self.stats.translated += 1;
        self.stats.code_bytes += out.code.len() as u64;
        self.stats.guest_insns_translated += block.insns.len() as u64;
        if let Some(p) = &mut self.perf {
            p.record(host, out.code.len() as u32, pc);
        }
        if let Some(dir) = &self.opts.dump_x86 {
            let _ = std::fs::write(dir.join(format!("tb_{pc:016x}.bin")), &out.code);
            let _ = std::fs::write(
                dir.join(format!("tb_{pc:016x}.txt")),
                dump_tb_text(pc, &block.insns, host, &out.code),
            );
        }
        let id = self.cache.insert(TranslationBlock {
            guest_pc: pc,
            insns: block.insns,
            fetch_fault: block.fetch_fault,
            guest_bytes,
            host,
            host_len: out.code.len() as u32,
            pcmap: out.pcmap,
        });
        self.stats.translate_time += t0.elapsed();
        id
    }

    /// Execute TB `id` once and report how it ended (`cpu.pc`/`icount` updated).
    pub fn exec_tb(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, id: u32) -> BlockExit {
        let host = self.cache.get(id).host;
        cpu.exit_reason = exit::NONE;
        cpu.mem_base = mem.base() as u64;
        cpu.helper_mem = mem as *mut DirectMem as u64;
        let start = self.cm.rx_base();
        signal::set_jit_range(start, start + self.cm.size() as u64, self.tr.fault_exit);
        self.stats.entries += 1;
        // SAFETY: `host` is a TB placed in this Jit's code buffer (still mapped: we own it),
        // and mem_base/helper_mem were just set to the live guest memory.
        unsafe { self.tr.enter(cpu, host) };
        signal::clear_jit_range();
        let reason = cpu.exit_reason;
        if let Some(n) = self.stats.exits.get_mut(reason as usize) {
            *n += 1;
        }
        match reason {
            exit::NONE => BlockExit::Continue,
            exit::ECALL => BlockExit::Ecall,
            exit::EXCEPTION => BlockExit::Trap(Exception {
                cause: cpu.exc_cause,
                tval: cpu.exc_tval,
            }),
            exit::FLUSH => BlockExit::Flush,
            exit::HOST_FAULT => BlockExit::Trap(self.resolve_host_fault(cpu, mem)),
            r => panic!("JIT exit with unknown reason {r} at pc {:#x}", cpu.pc),
        }
    }

    /// Turn a host SIGSEGV in JIT code into a precise guest exception: find the guest
    /// instruction through the TB's `pcmap`, restore `pc` and `icount`, and compute the
    /// fault address the interpreter would report.
    fn resolve_host_fault(&self, cpu: &mut CpuState, mem: &DirectMem) -> Exception {
        let rip = cpu.fault_rip;
        let tb = self
            .cache
            .find_host(rip)
            .unwrap_or_else(|| panic!("host fault at {rip:#x} outside any TB"));
        let e = tb.entry_for(rip).expect("pcmap covers the TB");
        let d = tb.insns[e.idx as usize];
        cpu.pc = e.guest_pc;
        cpu.icount += e.pending as u64;
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

    fn flush_all(&mut self) {
        self.cache.flush();
        self.cm.reset();
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
            let id = self.tb_for(cpu.pc, mem);
            let exit = self.exec_tb(cpu, mem, id);
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
        let s = &self.stats;
        format!(
            "jit: {} TBs translated ({} guest insns, {} KiB host code, {:.1} bytes/insn), \
             {} full + {} code-change flushes; {} TB entries; exits: none {}, ecall {}, \
             exception {}, flush {}, host-fault {}; translate time {:.1} ms",
            s.translated,
            s.guest_insns_translated,
            s.code_bytes / 1024,
            s.code_bytes as f64 / s.guest_insns_translated.max(1) as f64,
            s.full_flushes,
            s.fence_flushes,
            s.entries,
            s.exits[0],
            s.exits[1],
            s.exits[2],
            s.exits[3],
            s.exits[4],
            s.translate_time.as_secs_f64() * 1e3,
        )
    }
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
