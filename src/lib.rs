//! Bridge-V: a dynamic binary translator that JIT-compiles RISC-V RV64GC guest code into native
//! x86-64 host code. The architecture and every design decision are specified in `CLAUDE.md`;
//! the build plan is `docs/ROADMAP.md`.

pub mod backend;
pub mod cpu;
pub mod elf;
pub mod interp;
pub mod ir;
pub mod isa;
pub mod jit;
pub mod mem;
pub mod regalloc;
pub mod stats;
pub mod system;
pub mod user;
