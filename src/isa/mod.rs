//! Guest ISA front end: the decoded instruction model, the 32-bit and compressed (RVC) decoders,
//! and the disassembler. See CLAUDE.md §7.

pub mod decode;
pub mod disasm;
pub mod inst;
pub mod rvc;

pub use decode::{decode, decode_parts, insn_len};
pub use disasm::disasm;
pub use inst::{Decoded, Inst};
