//! `--engine=lockstep`: differential testing of the JIT against the interpreter, one TB at a
//! time (CLAUDE.md §21 item 4, P2.8).
//!
//! For every TB: snapshot the CPU, run the interpreter over exactly the TB's instructions with
//! a memory write log, record the resulting state and the new value of every written address,
//! undo the writes and restore the CPU, then run the JIT's translation of the same TB and
//! compare registers, pc, icount, fcsr, privilege, reservation, CSRs, exit kind and memory at
//! every logged address. The first divergence prints the guest disassembly, the host code and
//! the differences, and stops the run. The `time` CSR is made deterministic (icount-based) so
//! both runs read the same value.
//!
//! The JIT runs with a budget of exactly the TB's length: if the TB's exit is chained (or its
//! JALR hits the jump cache), control reaches the successor's prologue, which finds the budget
//! used up and exits before executing anything (D12). So linked exits and jump-cache hits are
//! exercised, yet every comparison covers exactly one TB.

use std::io;

use crate::cpu::csr::Csrs;
use crate::cpu::state::CpuState;
use crate::interp::{
    BlockExit, Engine, Env, Stop, deliver, deliver_interrupt, exec_block, tohost_written,
};

use super::dispatch::{Jit, JitOptions, Next, dump_tb_text};

pub struct Lockstep {
    jit: Jit,
    /// TBs executed and found identical.
    pub checked: u64,
    /// Reused buffers: the write log and the interpreter's new values.
    log: Vec<(u64, u64, u64)>,
    writes: Vec<(u64, u64, u64)>,
}

impl Lockstep {
    pub fn new(opts: JitOptions) -> io::Result<Lockstep> {
        Ok(Lockstep {
            jit: Jit::new(opts)?,
            checked: 0,
            log: Vec::new(),
            writes: Vec::new(),
        })
    }
}

/// The architectural part of `CpuState` that lockstep snapshots and compares. The jump cache
/// and JIT scratch fields are not architectural (and cloning the 64 KiB jump cache per TB would
/// dominate the run time).
#[derive(Clone)]
pub struct ArchState {
    x: [u64; 32],
    f: [u64; 32],
    pc: u64,
    icount: u64,
    fflags: u8,
    frm: u8,
    prv: u8,
    res: (u64, u64, u64),
    csr: Csrs,
}

impl ArchState {
    pub fn capture(c: &CpuState) -> ArchState {
        ArchState {
            x: c.x,
            f: c.f,
            pc: c.pc,
            icount: c.icount,
            fflags: c.fflags,
            frm: c.frm,
            prv: c.prv,
            res: (c.res_addr, c.res_val, c.res_valid),
            csr: c.csr.clone(),
        }
    }

    pub fn restore(&self, c: &mut CpuState) {
        c.x = self.x;
        c.f = self.f;
        c.pc = self.pc;
        c.icount = self.icount;
        c.fflags = self.fflags;
        c.frm = self.frm;
        c.prv = self.prv;
        (c.res_addr, c.res_val, c.res_valid) = self.res;
        c.csr = self.csr.clone();
    }

    /// Differences between this (interpreter) state and the JIT's state `j`.
    pub fn diff(&self, j: &CpuState) -> Vec<String> {
        let i = self;
        // Fast path (runs once per TB): no formatting unless something differs.
        if i.x == j.x
            && i.f == j.f
            && (i.pc, i.icount, i.res) == (j.pc, j.icount, (j.res_addr, j.res_val, j.res_valid))
            && (i.fflags, i.frm, i.prv) == (j.fflags, j.frm, j.prv)
            && i.csr == j.csr
        {
            return Vec::new();
        }
        let mut d = Vec::new();
        let mut cmp = |name: &str, a: u64, b: u64| {
            if a != b {
                d.push(format!("{name}: interp {a:#x}, jit {b:#x}"));
            }
        };
        for r in 0..32 {
            cmp(&format!("x{r}"), i.x[r], j.x[r]);
        }
        for r in 0..32 {
            cmp(&format!("f{r}"), i.f[r], j.f[r]);
        }
        cmp("pc", i.pc, j.pc);
        cmp("icount", i.icount, j.icount);
        cmp("fflags", i.fflags as u64, j.fflags as u64);
        cmp("frm", i.frm as u64, j.frm as u64);
        cmp("prv", i.prv as u64, j.prv as u64);
        cmp("res_addr", i.res.0, j.res_addr);
        cmp("res_val", i.res.1, j.res_val);
        cmp("res_valid", i.res.2, j.res_valid);
        let (a, b) = (&i.csr, &j.csr);
        for (name, x, y) in [
            ("mstatus", a.mstatus, b.mstatus),
            ("mepc", a.mepc, b.mepc),
            ("mcause", a.mcause, b.mcause),
            ("mtval", a.mtval, b.mtval),
            ("mtvec", a.mtvec, b.mtvec),
            ("mscratch", a.mscratch, b.mscratch),
            ("sepc", a.sepc, b.sepc),
            ("scause", a.scause, b.scause),
            ("stval", a.stval, b.stval),
            ("stvec", a.stvec, b.stvec),
            ("sscratch", a.sscratch, b.sscratch),
            ("satp", a.satp, b.satp),
            ("mie", a.mie, b.mie),
            ("mip", a.mip, b.mip),
            ("medeleg", a.medeleg, b.medeleg),
            ("mideleg", a.mideleg, b.mideleg),
            ("instret_offset", a.instret_offset, b.instret_offset),
            ("cycle_offset", a.cycle_offset, b.cycle_offset),
        ] {
            cmp(name, x, y);
        }
        if d.is_empty() && i.csr != j.csr {
            d.push("other CSR state differs".into());
        }
        d
    }
}

impl Engine for Lockstep {
    fn run(
        &mut self,
        cpu: &mut CpuState,
        mem: &mut crate::mem::direct::DirectMem,
        env: &Env,
        max_insns: u64,
    ) -> Stop {
        cpu.csr.deterministic_time = true;
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
            let id = match self.jit.select(cpu, mem) {
                Next::Tb(id) => id,
                // Not compared: the interpreter runs it in both engines.
                Next::Straddle => {
                    let exit = self.jit.interpret_one(cpu, mem);
                    if let Err(stop) = deliver(exit, env, cpu) {
                        return stop;
                    }
                    continue;
                }
                Next::Fault(e) => {
                    if let Err(stop) = deliver(BlockExit::Trap(e), env, cpu) {
                        return stop;
                    }
                    continue;
                }
            };

            // Reference run. The TLB is not architectural, but whether an access walks decides
            // whether A/D bits get written. If the interpreter filled entries (walked), the JIT
            // runs from an empty TLB: it then walks at least where the interpreter did, and a
            // walk of a PTE whose A/D bits are already set writes nothing, so memory matches.
            let snapshot = ArchState::capture(cpu);
            let fills = (cpu.tlb_fills, cpu.mmu_gen);
            self.log.clear();
            mem.write_log = Some(std::mem::take(&mut self.log));
            let tb = self.jit.tb(id);
            let iexit = exec_block(cpu, mem, &tb.insns, tb.fetch_fault, false);
            self.log = mem.write_log.take().expect("write log enabled above");
            self.writes.clear();
            self.writes
                .extend(self.log.iter().map(|&(a, s, _)| (a, s, mem.peek(a, s))));
            let icpu = ArchState::capture(cpu);
            snapshot.restore(cpu);
            mem.undo_writes(&self.log);
            // Code pages the reference run wrote (D49): the JIT run must find them marked too.
            let smc = std::mem::take(&mut mem.smc_pages);
            mem.remark_code(&smc);
            if cpu.softmmu != 0 {
                for &p in &smc {
                    crate::mem::tlb::set_code_flag(cpu, mem.base() as u64, p, true);
                }
            }
            if cpu.softmmu != 0 && (cpu.tlb_fills, cpu.mmu_gen) != fills {
                crate::mem::tlb::flush_all(cpu);
            }

            // JIT run from the same state, with a budget of exactly this TB.
            let n = self.jit.tb(id).insns.len() as i64;
            let jexit = self.jit.exec(cpu, mem, id, n);
            let mut diffs = icpu.diff(cpu);
            if iexit != jexit {
                diffs.push(format!("exit: interp {iexit:?}, jit {jexit:?}"));
            }
            for &(a, s, v) in &self.writes {
                let j = mem.peek(a, s);
                if j != v {
                    diffs.push(format!("mem[{a:#x}; {s}]: interp {v:#x}, jit {j:#x}"));
                }
            }
            if !diffs.is_empty() {
                let tb = self.jit.tb(id);
                eprintln!(
                    "bridgev: lockstep divergence after {} identical TBs\n{}differences:",
                    self.checked,
                    dump_tb_text(tb.guest_pc, &tb.insns, tb.host, self.jit.tb_code(id))
                );
                for d in &diffs {
                    eprintln!("  {d}");
                }
                return Stop::Diverged;
            }
            self.checked += 1;
            if jexit == BlockExit::Flush {
                self.jit.fence_i();
            }
            if let Err(stop) = deliver(jexit, env, cpu) {
                return stop;
            }
        }
    }

    fn flush(&mut self) {
        self.jit.flush();
    }

    fn fence_i(&mut self) {
        self.jit.fence_i();
    }

    fn stats(&self) -> String {
        format!(
            "lockstep: {} TBs identical; {}",
            self.checked,
            self.jit.stats()
        )
    }
}
