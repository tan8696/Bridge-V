//! Softmmu equivalence fuzzer (P7.6): the JIT's inline TLB fast path and its cold slow path
//! must behave exactly like the interpreter's MMU, for random loads and stores over an Sv39
//! layout mixing read-write, read-only, clean (D = 0), user, execute-only, unmapped and MMIO
//! pages, with misaligned and page-crossing accesses, under random SUM/MXR.
//!
//! Each block runs from identical states in two worlds (interpreter, JIT), twice: first with
//! a cold TLB (fills, walks, A/D updates), then again with the TLB the first run left (fast
//! path hits). Compared: registers, pc, exit/exception, every data page, the page tables
//! (A/D bits) and the device's access log. `PROPTEST_CASES` sets the number of blocks.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use bridgev::cpu::csr::mstatus;
use bridgev::cpu::state::CpuState;
use bridgev::cpu::trap::prv;
use bridgev::interp::{BlockExit, Engine, build_block_soft, exec_block};
use bridgev::jit::{Jit, JitOptions, RegAlloc};
use bridgev::mem::direct::DirectMem;
use bridgev::mem::mmu::{self, SATP_SV39, pte};
use bridgev::mem::phys::Mmio;
use bridgev::mem::{GuestVirt, prot};
use proptest::prelude::*;

const RAM: u64 = 0x8000_0000;
const ROOT: u64 = RAM + 0x1000;
const L1: u64 = RAM + 0x2000;
const L0: u64 = RAM + 0x3000;
const CODE_VA: u64 = 0x1_0000;
const CODE_PA: u64 = RAM + 0x1_0000;
const DATA_VA: u64 = 0x10_0000;
const DATA_PA: u64 = RAM + 0x2_0000;
const PAGES: u64 = 8;
const DEV_PA: u64 = 0x1000_0000;

/// What a data page is.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Rw,
    RwClean,
    Ro,
    User,
    ExecOnly,
    Unmapped,
    Mmio,
}

fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        4 => Just(Kind::Rw),
        2 => Just(Kind::RwClean),
        2 => Just(Kind::Ro),
        1 => Just(Kind::User),
        1 => Just(Kind::ExecOnly),
        1 => Just(Kind::Unmapped),
        1 => Just(Kind::Mmio),
    ]
}

type Log = Arc<Mutex<Vec<(bool, u64, u64, u64)>>>;

/// A device that logs every access; reads return a value derived from the access and the
/// number of accesses so far (side effects must happen exactly once, in order).
struct Dev(Log);
impl Mmio for Dev {
    fn name(&self) -> &str {
        "fuzz"
    }
    fn read(&mut self, off: u64, size: u64) -> u64 {
        let mut l = self.0.lock().unwrap();
        let v = (0x1111_2222_3333_4444u64 ^ off.wrapping_mul(0x9e37_79b9) ^ l.len() as u64)
            & (u64::MAX >> (64 - 8 * size));
        l.push((false, off, size, v));
        v
    }
    fn write(&mut self, off: u64, size: u64, val: u64) {
        self.0.lock().unwrap().push((true, off, size, val));
    }
}

fn i(imm: i32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    ((imm as u32) & 0xFFF) << 20 | rs1 << 15 | f3 << 12 | rd << 7 | opc
}
fn s(imm: i32, rs2: u32, rs1: u32, f3: u32) -> u32 {
    let imm = imm as u32;
    ((imm >> 5) & 0x7F) << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | (imm & 0x1F) << 7 | 0x23
}

/// Loads and stores off base registers x5..x8 (each 2 KiB into a different pair of data
/// pages), plus ALU ops that change the values stored and the bases.
fn insn() -> impl Strategy<Value = u32> {
    let base = 5u32..9;
    let val = 9u32..16;
    let off = prop_oneof![
        3 => -2048i32..2048,
        1 => prop::sample::select(vec![-2048, -2047, -2045, -2041, 2040, 2044, 2046, 2047]),
    ];
    prop_oneof![
        5 => (0u32..7, off.clone(), base.clone(), 9u32..16).prop_map(|(f3, o, b, d)| i(o, b, f3, d, 0x03)),
        5 => (0u32..4, off, base.clone(), val.clone()).prop_map(|(f3, o, b, v)| s(o, v, b, f3)),
        2 => (-64i32..64, val.clone(), val).prop_map(|(imm, a, d)| i(imm, a, 0, d, 0x13)),
        1 => (-16i32..16, base).prop_map(|(imm, b)| i(imm * 8, b, 0, b, 0x13)),
    ]
}

#[derive(Clone, Debug)]
struct Case {
    kinds: Vec<Kind>,
    code: Vec<u32>,
    regs: [u64; 32],
    sum: bool,
    mxr: bool,
    fill: u64,
}

fn case() -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(kind(), PAGES as usize),
        prop::collection::vec(insn(), 1..24),
        prop::collection::vec(any::<u64>(), 7),
        any::<bool>(),
        any::<bool>(),
        any::<u64>(),
    )
        .prop_map(|(kinds, mut code, vals, sum, mxr, fill)| {
            code.push(0x0000_0073); // ecall ends the block
            let mut regs = [0u64; 32];
            for (k, r) in (5..9).enumerate() {
                regs[r] = DATA_VA + 0x2000 * k as u64 + 0x800;
            }
            for (k, v) in vals.into_iter().enumerate() {
                regs[9 + k] = v;
            }
            Case {
                kinds,
                code,
                regs,
                sum,
                mxr,
                fill,
            }
        })
}

struct World {
    cpu: Box<CpuState>,
    mem: DirectMem,
    log: Log,
}

fn put(mem: &mut DirectMem, a: u64, v: u64) {
    mem.store(a, 8, v).unwrap();
}

fn leaf(pa: u64, flags: u64) -> u64 {
    (pa >> 12) << 10 | flags | pte::V
}

fn world(c: &Case) -> World {
    let mut mem = DirectMem::new().unwrap();
    mem.map(GuestVirt(RAM), 1 << 20, prot::RWX).unwrap();
    let log: Log = Arc::default();
    mem.add_device(DEV_PA, PAGES * 4096, Box::new(Dev(log.clone())));
    // One L1/L0 chain covers VA 0..2 MiB: code at 0x10000, data at 0x100000.
    put(&mut mem, ROOT, (L1 >> 12) << 10 | pte::V);
    put(&mut mem, L1, (L0 >> 12) << 10 | pte::V);
    let (r, w, x, u, ad) = (pte::R, pte::W, pte::X, pte::U, pte::A | pte::D);
    put(
        &mut mem,
        L0 + (CODE_VA >> 12) * 8,
        leaf(CODE_PA, r | x | ad),
    );
    for (k, &kind) in c.kinds.iter().enumerate() {
        let k = k as u64;
        let pa = DATA_PA + k * 4096;
        let p = match kind {
            Kind::Rw => leaf(pa, r | w | ad),
            Kind::RwClean => leaf(pa, r | w),
            Kind::Ro => leaf(pa, r | pte::A),
            Kind::User => leaf(pa, r | w | u | ad),
            Kind::ExecOnly => leaf(pa, x | ad),
            Kind::Unmapped => 0,
            Kind::Mmio => leaf(DEV_PA + k * 4096, r | w | ad),
        };
        put(&mut mem, L0 + ((DATA_VA >> 12) + k) * 8, p);
        for q in 0..512 {
            put(&mut mem, pa + 8 * q, c.fill.rotate_left(q as u32) ^ q);
        }
    }
    for (k, &insn) in c.code.iter().enumerate() {
        mem.store(CODE_PA + 4 * k as u64, 4, insn as u64).unwrap();
    }
    let mut cpu = CpuState::new_machine(CODE_VA);
    cpu.softmmu = 1;
    cpu.prv = prv::S;
    cpu.csr.satp = SATP_SV39 << 60 | ROOT >> 12;
    cpu.csr.mstatus |= if c.sum { mstatus::SUM } else { 0 } | if c.mxr { mstatus::MXR } else { 0 };
    cpu.x = c.regs;
    World { cpu, mem, log }
}

/// Architecturally visible outcome of one run.
#[derive(Debug, PartialEq)]
struct Outcome {
    exit: BlockExit,
    x: [u64; 32],
    pc: u64,
    icount: u64,
    data: Vec<u64>,
    tables: Vec<u64>,
    dev: Vec<(bool, u64, u64, u64)>,
}

fn outcome(w: &mut World, exit: BlockExit) -> Outcome {
    let words =
        |mem: &DirectMem, base: u64, n: u64| (0..n).map(|q| mem.peek(base + 8 * q, 8)).collect();
    Outcome {
        exit,
        x: w.cpu.x,
        pc: w.cpu.pc,
        icount: w.cpu.icount,
        data: words(&w.mem, DATA_PA, 512 * PAGES),
        tables: words(&w.mem, ROOT, 3 * 512),
        dev: std::mem::take(&mut *w.log.lock().unwrap()),
    }
}

/// The fields where `got` (JIT) differs from `want` (interpreter).
fn diff(got: &Outcome, want: &Outcome) -> Vec<String> {
    let mut d = Vec::new();
    if got.exit != want.exit {
        d.push(format!("exit: jit {:?}, interp {:?}", got.exit, want.exit));
    }
    for r in 0..32 {
        if got.x[r] != want.x[r] {
            d.push(format!(
                "x{r}: jit {:#x}, interp {:#x}",
                got.x[r], want.x[r]
            ));
        }
    }
    if (got.pc, got.icount) != (want.pc, want.icount) {
        d.push(format!(
            "pc/icount: jit {:#x}/{}, interp {:#x}/{}",
            got.pc, got.icount, want.pc, want.icount
        ));
    }
    for (name, g, w, base) in [
        ("data", &got.data, &want.data, DATA_PA),
        ("pt", &got.tables, &want.tables, ROOT),
    ] {
        for (q, (a, b)) in g.iter().zip(w).enumerate() {
            if a != b {
                d.push(format!(
                    "{name}[{:#x}]: jit {a:#x}, interp {b:#x}",
                    base + 8 * q as u64
                ));
            }
        }
    }
    if got.dev != want.dev {
        d.push(format!("dev: jit {:x?}, interp {:x?}", got.dev, want.dev));
    }
    d
}

fn run_interp(w: &mut World) -> Outcome {
    let b = build_block_soft(CODE_VA, &mut w.cpu, &mut w.mem, 128);
    let e = exec_block(&mut w.cpu, &mut w.mem, &b.insns, b.fetch_fault, false);
    outcome(w, e)
}

fn run_jit(w: &mut World, jit: &mut Jit) -> Outcome {
    let ppage = mmu::fetch_page(&mut w.cpu, &mut w.mem, CODE_VA).expect("code is mapped");
    let id = jit.tb_for_soft(&mut w.cpu, &mut w.mem, ppage);
    let n = jit.tb(id).insns.len() as i64;
    let e = jit.exec(&mut w.cpu, &mut w.mem, id, n);
    outcome(w, e)
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000)
}

thread_local! {
    static JITS: RefCell<Vec<(&'static str, Jit)>> = RefCell::new(
        [("linear", RegAlloc::Linear), ("pinned", RegAlloc::Pinned), ("none", RegAlloc::None)]
            .into_iter()
            .map(|(n, regalloc)| {
                let o = JitOptions { code_cache: 1 << 20, regalloc, ..JitOptions::default() };
                (n, Jit::new(o).expect("JIT"))
            })
            .collect(),
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn softmmu_jit_matches_interpreter(c in case()) {
        JITS.with(|jits| -> Result<(), TestCaseError> {
            for (name, jit) in jits.borrow_mut().iter_mut() {
                Engine::flush(jit);
                let (mut a, mut b) = (world(&c), world(&c));
                for round in 0..2 {
                    for w in [&mut a, &mut b] {
                        w.cpu.pc = CODE_VA;
                        w.cpu.x = c.regs;
                    }
                    let want = run_interp(&mut a);
                    let got = run_jit(&mut b, jit);
                    let d = diff(&got, &want);
                    prop_assert!(d.is_empty(), "config `{}`, round {} ({} TLB): {}\ncode: {:08x?}",
                        name, round, if round == 0 { "cold" } else { "warm" }, d.join("; "), c.code);
                }
            }
            Ok(())
        })?;
    }
}
