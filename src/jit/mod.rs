//! JIT runtime: executable code memory, trampolines, translation cache, block chaining and the
//! dispatcher loop (CLAUDE.md §8.4, §12, §13).

pub mod cache;
pub mod chain;
pub mod code_mem;
pub mod dispatch;
pub mod perfmap;
pub mod trampoline;
