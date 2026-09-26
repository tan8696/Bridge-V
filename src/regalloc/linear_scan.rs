//! Register allocation for one translation block (CLAUDE.md §10, P4.6).
//!
//! A TB is straight-line SSA code, so linear scan degenerates to one walk over the ops in order
//! (Poletto & Sarkar with intervals `[def, last_use]` from `ir::liveness`). The allocator runs
//! interleaved with code generation and emits its own fills, spills and moves:
//!
//! * **Pool:** RAX, RCX, RDX, RSI, RDI, R8. R9 holds the budget (D46); R10/R11 are never
//!   allocated (op scratch).
//! * **Pinned guest registers** (§8.2) live in R12–R15 for the whole block and across chained
//!   blocks. A `WriteReg` to a pinned register moves the value there immediately, so pinned
//!   state is exact at every fault site.
//! * **Guest-register caching:** a value knows where it can be recovered without a register
//!   (`Back`: its guest register's memory home, a spill slot, or a constant). With `lazy`
//!   write-back, `WriteReg` of a non-pinned register only marks the value *dirty*; it is stored
//!   home at the next exit, helper call or eviction, so a register overwritten within the block
//!   is stored at most once (dead write-back elimination). Fault sites record where every
//!   dirty guest register currently is (`dirty_state`, the §15 state map).
//! * **Spill choice:** when the pool is full, evict the value whose next use is furthest away.
//!   A clean value is dropped for free (refilled from its backing), a dirty guest value is
//!   written home, and a pure temporary goes to a `CpuState::spill` slot.

use std::mem::offset_of;

use crate::backend::x86::emit::{Asm, Mem, Size};
use crate::backend::x86::regs::{BUDGET_REG, CPU, CPU_BIAS, POOL, Reg, SCRATCH1};
use crate::cpu::state::{CpuState, SPILL_SLOTS};
use crate::ir::liveness::Liveness;
use crate::ir::ops::{Block, V};

/// Where a value can be recovered when it is not in a register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Back {
    None,
    /// Guest register `g`'s `CpuState` home holds it.
    Home(u8),
    Slot(u16),
    Const(u64),
}

/// Location of a dirty guest register's value at a fault site (§15 state map).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DLoc {
    Reg(Reg),
    Slot(u16),
    Const(u64),
    /// Equal to guest register `g`'s memory home.
    Home(u8),
}

#[derive(Clone, Copy, Debug)]
struct Val {
    /// Pool register holding the value.
    reg: Option<Reg>,
    back: Back,
    /// The pinned host register of guest `g` holds it.
    pin: Option<u8>,
    /// Non-pinned guest registers whose current value this is, not yet stored home.
    dirty: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocStats {
    pub fills: u32,
    pub spills: u32,
    pub writebacks: u32,
    pub moves: u32,
}

/// The block needs more spill slots than `CpuState` has (the caller retranslates it shorter).
#[derive(Debug, PartialEq, Eq)]
pub struct OutOfSlots;

pub type Result<T> = std::result::Result<T, OutOfSlots>;

pub fn home(g: u8) -> Mem {
    Mem::base(CPU, 8 * g as i32 - CPU_BIAS)
}

pub fn slot_mem(k: u16) -> Mem {
    Mem::base(
        CPU,
        (offset_of!(CpuState, spill) + 8 * k as usize) as i32 - CPU_BIAS,
    )
}

/// `[mem] = v` (64-bit), through R11 when `v` does not fit a sign-extended imm32.
pub fn store_const(a: &mut Asm, mem: Mem, v: u64) {
    if v as i64 == v as i32 as i64 {
        a.store_imm(Size::B64, mem, v as i32);
    } else {
        a.movabs(SCRATCH1, v);
        a.store(Size::B64, mem, SCRATCH1);
    }
}

pub struct Alloc {
    /// Sorted use positions per value (aliases merge theirs into the representative).
    uses: Vec<Vec<u32>>,
    /// Representative of each value (a `ReadReg` of a register whose value is already known
    /// becomes an alias of that value).
    rep: Vec<V>,
    vals: Vec<Val>,
    owner: [Option<V>; 16],
    pinned: [Option<Reg>; 32],
    /// Allocatable registers: `POOL` minus the pinned hosts and the budget register.
    pool: Vec<Reg>,
    pin_val: [Option<V>; 32],
    dirty: [Option<V>; 32],
    home_owner: [Option<V>; 32],
    free_slots: Vec<u16>,
    /// Current op position.
    pub pos: u32,
    pub lazy: bool,
    pub stats: AllocStats,
}

impl Alloc {
    /// `pinned`: guest registers pinned to host registers; `lazy`: cache non-pinned guest
    /// registers and write them back lazily (otherwise every `WriteReg` stores immediately).
    pub fn new(b: &Block, pinned: [Option<Reg>; 32], lazy: bool) -> Alloc {
        let live = Liveness::compute(b);
        let n = b.nvals as usize;
        Alloc {
            uses: live.into_uses(),
            rep: (0..n as u32).map(V).collect(),
            vals: vec![
                Val {
                    reg: None,
                    back: Back::None,
                    pin: None,
                    dirty: 0,
                };
                n
            ],
            owner: [None; 16],
            pinned,
            pool: POOL
                .iter()
                .copied()
                .filter(|r| !pinned.contains(&Some(*r)) && *r != BUDGET_REG)
                .collect(),
            pin_val: [None; 32],
            dirty: [None; 32],
            home_owner: [None; 32],
            free_slots: (0..SPILL_SLOTS as u16).rev().collect(),
            pos: 0,
            lazy,
            stats: AllocStats::default(),
        }
    }

    /// Canonical value for `v`.
    pub fn r(&self, v: V) -> V {
        self.rep[v.0 as usize]
    }

    /// First use of `v` at or after position `from`.
    fn use_from(&self, v: V, from: u32) -> Option<u32> {
        let u = &self.uses[v.0 as usize];
        let i = u.partition_point(|&p| p < from);
        u.get(i).copied()
    }

    /// `v` is used by the current op or a later one. While an op is being lowered its
    /// operands count as live: evicting or overwriting the backing of one it has not loaded
    /// yet must preserve it.
    fn live_now(&self, v: V) -> bool {
        self.use_from(v, self.pos).is_some()
    }

    /// `v` is used after the current op (the op itself has consumed it).
    fn live_after(&self, v: V) -> bool {
        self.use_from(v, self.pos + 1).is_some()
    }

    /// `v`'s value must be preserved: still to be used, or still to be written home.
    fn needed(&self, v: V) -> bool {
        self.live_now(v) || self.vals[v.0 as usize].dirty != 0
    }

    fn val(&mut self, v: V) -> &mut Val {
        &mut self.vals[v.0 as usize]
    }

    pub fn pinned_reg(&self, g: u8) -> Option<Reg> {
        self.pinned[g as usize]
    }

    /// The register currently holding `v`, if any (pool or pinned).
    pub fn reg_of(&self, v: V) -> Option<Reg> {
        let x = &self.vals[self.r(v).0 as usize];
        x.reg.or(x.pin.and_then(|g| self.pinned[g as usize]))
    }

    /// Constant value of `v`, if it is one.
    pub fn const_of(&self, v: V) -> Option<u64> {
        match self.vals[self.r(v).0 as usize].back {
            Back::Const(c) => Some(c),
            _ => None,
        }
    }

    fn alias(&mut self, dst: V, to: V) {
        let (d, t) = (dst.0 as usize, self.r(to).0 as usize);
        let du = std::mem::take(&mut self.uses[d]);
        let mut merged = std::mem::take(&mut self.uses[t]);
        merged.extend(du);
        merged.sort_unstable();
        merged.dedup();
        self.uses[t] = merged;
        self.rep[d] = V(t as u32);
    }

    fn slot(&mut self) -> Result<u16> {
        self.free_slots.pop().ok_or(OutOfSlots)
    }

    /// Load `v` (not in a register) from its backing into `r`.
    fn fill(&mut self, a: &mut Asm, v: V, r: Reg) {
        match self.vals[v.0 as usize].back {
            Back::Home(g) => a.load(Size::B64, r, home(g)),
            Back::Slot(k) => a.load(Size::B64, r, slot_mem(k)),
            Back::Const(c) => a.mov_imm(r, c),
            Back::None => panic!("regalloc: value {v} has no location"),
        }
        if !matches!(self.vals[v.0 as usize].back, Back::Const(_)) {
            self.stats.fills += 1;
        }
    }

    // ------------------------------------------------------------ definitions ----

    pub fn def_const(&mut self, v: V, c: u64) {
        self.val(v).back = Back::Const(c);
    }

    /// `v = x{g}`.
    pub fn read_reg(&mut self, v: V, g: u8) {
        if self.pinned[g as usize].is_some() {
            match self.pin_val[g as usize] {
                Some(w) => {
                    // P still holds `w` even if `w` lost its own pin (it was moved out of
                    // another pinned register it also lived in) and died.
                    let w = self.r(w);
                    if self.reg_of(w).is_none() {
                        self.val(w).pin = Some(g);
                    }
                    self.alias(v, w)
                }
                None => {
                    self.val(v).pin = Some(g);
                    self.pin_val[g as usize] = Some(v);
                }
            }
        } else if let Some(w) = self.dirty[g as usize] {
            self.alias(v, w);
        } else if let Some(w) = self.home_owner[g as usize] {
            // home(g) holds `w` (nothing stored there since), but `w` itself may have died and
            // released its register or slot: home(g) becomes its backing again.
            let w = self.r(w);
            let x = self.vals[w.0 as usize];
            if self.reg_of(w).is_none() && x.back == Back::None {
                self.val(w).back = Back::Home(g);
            }
            self.alias(v, w);
        } else {
            self.val(v).back = Back::Home(g);
            self.home_owner[g as usize] = Some(v);
        }
    }

    /// `x{g} = v`.
    pub fn write_reg(&mut self, a: &mut Asm, g: u8, v: V) -> Result<()> {
        let v = self.r(v);
        if let Some(p) = self.pinned[g as usize] {
            if self.pin_val[g as usize] == Some(v) {
                return Ok(());
            }
            // The old value in P may still be needed: keep a copy elsewhere.
            if let Some(o) = self.pin_val[g as usize].take()
                && self.vals[o.0 as usize].pin == Some(g)
            {
                // Still the value of another pinned register (after `mv s0, a4`): that
                // register keeps it, no copy needed.
                if let Some(h) = (1..32u8).find(|&h| h != g && self.pin_val[h as usize] == Some(o))
                {
                    self.val(o).pin = Some(h);
                    return self.write_pinned(a, g, p, v);
                }
                self.val(o).pin = None;
                let x = self.vals[o.0 as usize];
                if self.needed(o) && x.reg.is_none() && x.back == Back::None {
                    let r = self.alloc(a, &[p])?;
                    a.mov_rr(Size::B64, r, p);
                    self.stats.moves += 1;
                    self.take(r, o);
                }
            }
            return self.write_pinned(a, g, p, v);
        }
        if !self.lazy {
            return self.store_home(a, g, v);
        }
        if self.dirty[g as usize] == Some(v) {
            return Ok(());
        }
        if let Some(o) = self.dirty[g as usize].take() {
            self.val(o).dirty &= !(1 << g);
            self.release_if_dead(o);
        }
        self.dirty[g as usize] = Some(v);
        self.val(v).dirty |= 1 << g;
        Ok(())
    }

    /// Move `v` into guest `g`'s pinned register `p` (its old value is already preserved).
    fn write_pinned(&mut self, a: &mut Asm, g: u8, p: Reg, v: V) -> Result<()> {
        match self.reg_of(v) {
            Some(r) if r == p => {}
            Some(r) => {
                a.mov_rr(Size::B64, p, r);
                self.stats.moves += 1;
            }
            None => self.fill(a, v, p),
        }
        self.pin_val[g as usize] = Some(v);
        if self.vals[v.0 as usize].pin.is_none() {
            self.val(v).pin = Some(g);
        }
        Ok(())
    }

    fn take(&mut self, r: Reg, v: V) {
        self.owner[r.num() as usize] = Some(v);
        self.val(v).reg = Some(r);
    }

    /// Store guest register `g`'s value `v` to its home (write-back or eager store).
    fn store_home(&mut self, a: &mut Asm, g: u8, v: V) -> Result<()> {
        // The value currently backed by this home loses that backing.
        if let Some(o) = self.home_owner[g as usize]
            && o != v
            && self.vals[o.0 as usize].back == Back::Home(g)
        {
            // `needed`, not just live: `o` may be the pending (dirty) value of another guest
            // register, e.g. after `mv x7, x8` it still has to reach x7's home.
            if self.reg_of(o).is_none() && self.needed(o) {
                let k = self.slot()?;
                a.load(Size::B64, SCRATCH1, home(g));
                a.store(Size::B64, slot_mem(k), SCRATCH1);
                self.val(o).back = Back::Slot(k);
                self.stats.spills += 1;
            } else {
                self.val(o).back = Back::None;
            }
        }
        let x = self.vals[v.0 as usize];
        match (self.reg_of(v), x.back) {
            (Some(r), _) => a.store(Size::B64, home(g), r),
            (None, Back::Const(c)) => store_const(a, home(g), c),
            (None, Back::Home(h)) => {
                a.load(Size::B64, SCRATCH1, home(h));
                a.store(Size::B64, home(g), SCRATCH1);
            }
            (None, Back::Slot(k)) => {
                a.load(Size::B64, SCRATCH1, slot_mem(k));
                a.store(Size::B64, home(g), SCRATCH1);
            }
            (None, Back::None) => panic!("regalloc: storing lost value {v}"),
        }
        self.home_owner[g as usize] = Some(v);
        if x.back == Back::None {
            self.val(v).back = Back::Home(g);
        }
        self.val(v).dirty &= !(1 << g);
        if self.dirty[g as usize] == Some(v) {
            self.dirty[g as usize] = None;
        }
        self.stats.writebacks += 1;
        Ok(())
    }

    /// Store every dirty guest register home (before exits and helper calls).
    pub fn write_back_all(&mut self, a: &mut Asm) -> Result<()> {
        for g in 1..32u8 {
            if let Some(v) = self.dirty[g as usize] {
                self.store_home(a, g, v)?;
            }
        }
        Ok(())
    }

    /// Where every dirty guest register is right now (the state map of a fault site).
    pub fn dirty_state(&self) -> Vec<(u8, DLoc)> {
        (1..32u8)
            .filter_map(|g| {
                let v = self.dirty[g as usize]?;
                let loc = match (self.reg_of(v), self.vals[v.0 as usize].back) {
                    (Some(r), _) => DLoc::Reg(r),
                    (None, Back::Slot(k)) => DLoc::Slot(k),
                    (None, Back::Const(c)) => DLoc::Const(c),
                    (None, Back::Home(h)) => DLoc::Home(h),
                    (None, Back::None) => panic!("regalloc: dirty x{g} has no location"),
                };
                Some((g, loc))
            })
            .collect()
    }

    // ------------------------------------------------------------ registers ----

    /// Get a register holding `v`, filling it if necessary; never one of `avoid`.
    pub fn get(&mut self, a: &mut Asm, v: V, avoid: &[Reg]) -> Result<Reg> {
        let v = self.r(v);
        if let Some(r) = self.reg_of(v) {
            return Ok(r);
        }
        let r = self.alloc(a, avoid)?;
        self.fill(a, v, r);
        self.take(r, v);
        Ok(r)
    }

    /// Copy `v` into `dst` (a scratch or fixed register the caller owns) without allocating a
    /// pool register for it; `v`'s own location is unchanged.
    pub fn copy_to(&mut self, a: &mut Asm, v: V, dst: Reg) {
        let v = self.r(v);
        match self.reg_of(v) {
            Some(r) if r == dst => {}
            Some(r) => a.mov_rr(Size::B64, dst, r),
            None => self.fill(a, v, dst),
        }
    }

    /// A free pool register (evicting the furthest-next-use value if needed).
    pub fn alloc(&mut self, a: &mut Asm, avoid: &[Reg]) -> Result<Reg> {
        if let Some(&r) = self
            .pool
            .iter()
            .find(|r| self.owner[r.num() as usize].is_none() && !avoid.contains(r))
        {
            return Ok(r);
        }
        let victim = *self
            .pool
            .iter()
            .filter(|r| !avoid.contains(r))
            .max_by_key(|r| {
                // The current op's operands have next use == pos, so they are never chosen
                // while any other value occupies the pool (an op has at most two).
                let v = self.owner[r.num() as usize].expect("pool full");
                self.use_from(v, self.pos).unwrap_or(u32::MAX)
            })
            .expect("allocatable register");
        self.evict(a, victim)?;
        Ok(victim)
    }

    /// Define `v` in a fresh pool register (`prefer` if it is free).
    pub fn def(&mut self, a: &mut Asm, v: V, prefer: Option<Reg>, avoid: &[Reg]) -> Result<Reg> {
        let r = match prefer {
            Some(p) if self.owner[p.num() as usize].is_none() && self.pool.contains(&p) => p,
            _ => self.alloc(a, avoid)?,
        };
        self.take(r, v);
        Ok(r)
    }

    /// Free pool register `r`, preserving its value if it is still needed.
    pub fn evict(&mut self, a: &mut Asm, r: Reg) -> Result<()> {
        let Some(v) = self.owner[r.num() as usize] else {
            return Ok(());
        };
        if self.needed(v) {
            let x = self.vals[v.0 as usize];
            // Dirty: write it home (that store would happen at the exit anyway, §10).
            for g in 1..32u8 {
                if x.dirty & (1 << g) != 0 {
                    self.store_home(a, g, v)?;
                }
            }
            let x = self.vals[v.0 as usize];
            if self.live_now(v) && x.back == Back::None && x.pin.is_none() {
                let k = self.slot()?;
                a.store(Size::B64, slot_mem(k), r);
                self.val(v).back = Back::Slot(k);
                self.stats.spills += 1;
            }
        }
        self.owner[r.num() as usize] = None;
        self.val(v).reg = None;
        Ok(())
    }

    /// Make `r` free, moving its value to another free pool register if there is one.
    pub fn vacate(&mut self, a: &mut Asm, r: Reg, avoid: &[Reg]) -> Result<()> {
        let Some(v) = self.owner[r.num() as usize] else {
            return Ok(());
        };
        if self.needed(v)
            && let Some(&f) = self
                .pool
                .iter()
                .find(|f| self.owner[f.num() as usize].is_none() && !avoid.contains(f) && **f != r)
        {
            a.mov_rr(Size::B64, f, r);
            self.stats.moves += 1;
            self.owner[r.num() as usize] = None;
            self.take(f, v);
            return Ok(());
        }
        self.evict(a, r)
    }

    /// Bind `v` to fixed register `r` (which the caller vacated).
    pub fn def_fixed(&mut self, v: V, r: Reg) {
        debug_assert!(self.owner[r.num() as usize].is_none());
        self.take(r, v);
    }

    fn release_if_dead(&mut self, v: V) {
        if self.live_after(v) || self.vals[v.0 as usize].dirty != 0 {
            return;
        }
        let x = self.vals[v.0 as usize];
        if let Some(r) = x.reg {
            self.owner[r.num() as usize] = None;
            self.val(v).reg = None;
        }
        if let Back::Slot(k) = x.back {
            self.free_slots.push(k);
            self.val(v).back = Back::None;
        }
    }

    /// After the op at `pos`: free the registers of values used or defined there that are dead.
    pub fn release(&mut self, vs: &[V]) {
        for &v in vs {
            let v = self.r(v);
            self.release_if_dead(v);
        }
    }

    // ------------------------------------------------------------ helper calls ----

    /// Before a helper call (full sync, D14): write back dirty registers, store the pinned
    /// registers home, and move every value still needed afterwards into a spill slot (the
    /// call clobbers the pool, and the helper may change any guest register home).
    pub fn sync_for_call(&mut self, a: &mut Asm) -> Result<()> {
        self.write_back_all(a)?;
        for g in 1..32u8 {
            if let Some(p) = self.pinned[g as usize] {
                a.store(Size::B64, home(g), p);
            }
        }
        for i in 0..self.vals.len() {
            let v = V(i as u32);
            if self.rep[i] != v || !self.live_after(v) {
                continue;
            }
            let x = self.vals[i];
            if matches!(x.back, Back::Const(_) | Back::Slot(_)) {
                continue;
            }
            let src = match (self.reg_of(v), x.back) {
                (Some(r), _) => Some(r),
                (None, Back::Home(g)) => {
                    a.load(Size::B64, SCRATCH1, home(g));
                    Some(SCRATCH1)
                }
                _ => None,
            };
            if let Some(r) = src {
                let k = self.slot()?;
                a.store(Size::B64, slot_mem(k), r);
                self.val(v).back = Back::Slot(k);
                self.stats.spills += 1;
            }
        }
        for i in 0..self.vals.len() {
            let x = &mut self.vals[i];
            x.reg = None;
            x.pin = None;
            if matches!(x.back, Back::Home(_)) {
                x.back = Back::None;
            }
        }
        self.owner = [None; 16];
        self.pin_val = [None; 32];
        self.home_owner = [None; 32];
        Ok(())
    }

    /// After a helper call returned to the block: reload the pinned registers (the helper may
    /// have changed them).
    pub fn after_call(&mut self, a: &mut Asm) {
        for g in 1..32u8 {
            if let Some(p) = self.pinned[g as usize] {
                a.load(Size::B64, p, home(g));
            }
        }
    }
}
