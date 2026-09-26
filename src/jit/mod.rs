//! JIT runtime: executable code memory, trampolines, translation cache, block chaining, the
//! dispatcher loop and the lockstep checker (CLAUDE.md §8.4, §12, §13, §21).

pub mod cache;
pub mod chain;
pub mod code_mem;
pub mod dispatch;
pub mod lockstep;
pub mod perfmap;
pub mod trampoline;

pub use dispatch::{Jit, JitOptions};

use crate::interp::{Engine, Interp};

/// `--engine`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EngineKind {
    #[default]
    Interp,
    Jit,
    Lockstep,
}

/// Create the selected execution engine.
pub fn make_engine(kind: EngineKind, opts: &JitOptions) -> std::io::Result<Box<dyn Engine>> {
    Ok(match kind {
        EngineKind::Interp => Box::new(Interp::new()),
        EngineKind::Jit => Box::new(Jit::new(opts.clone())?),
        EngineKind::Lockstep => Box::new(lockstep::Lockstep::new(opts.clone())?),
    })
}
