//! Full-system emulation of a QEMU `virt`-compatible RISC-V machine (CLAUDE.md §20). Phase 9.

pub mod clint;
pub mod fdt;
pub mod machine;
pub mod plic;
pub mod sbi;
pub mod syscon;
pub mod uart16550;
