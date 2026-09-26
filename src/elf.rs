//! ELF64 loader for RISC-V guest executables: header validation, PT_LOAD segments, entry point,
//! symbol lookup (`tohost`/`fromhost`) and `e_flags`. Implemented in Phase 1 (P1.2).
