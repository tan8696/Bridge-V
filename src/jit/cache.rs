//! Translation cache (CLAUDE.md §13.1, P2.5, P3.1): translation-block metadata, the
//! `pc → TB` map, lookup by host address, and the chaining metadata (exit slots, incoming
//! links) that `chain.rs` patches.
//!
//! TBs are numbered in allocation order within a generation. Code is bump-allocated, so TB
//! host addresses increase with the id and a host RIP is mapped to its TB by binary search.
//! A full flush drops every TB and starts a new generation (§12). TbFlags (privilege, MMU
//! index) arrive in Phase 7: translations do not depend on them yet (D28).

use rustc_hash::FxHashMap;

use crate::cpu::trap::Exception;
use crate::isa::Decoded;

/// Host offset → guest instruction, recorded at the start of each guest instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcEntry {
    /// Offset of the instruction's first host byte from the TB start.
    pub host_off: u32,
    /// Index of the guest instruction in `TranslationBlock::insns`.
    pub idx: u32,
    pub guest_pc: u64,
}

/// A chainable direct exit (§13.3): a `jmp`/`jcc rel32` whose 4-byte-aligned rel32 field
/// initially targets the TB's own exit stub and can be patched to a successor TB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitSlot {
    /// RX address of the rel32 field.
    pub patch_at: u64,
    /// RX address of this slot's exit stub (the unlinked target).
    pub stub: u64,
    /// Guest pc this exit continues at.
    pub target_pc: u64,
    /// TB the slot currently jumps to.
    pub linked: Option<u32>,
}

/// One translated guest basic block.
pub struct TranslationBlock {
    pub guest_pc: u64,
    /// The decoded guest instructions (for fault resolution, lockstep and dumps).
    pub insns: Vec<Decoded>,
    /// Raised when execution reaches the end of `insns` (the next fetch faulted).
    pub fetch_fault: Option<Exception>,
    pub guest_bytes: u32,
    /// RX address and length of the host code.
    pub host: u64,
    pub host_len: u32,
    pub pcmap: Vec<PcEntry>,
    /// Direct exits: slot 0 (fall-through / jump), slot 1 (branch taken).
    pub exits: [Option<ExitSlot>; 2],
    /// `(tb, slot)` pairs whose exit slot is linked to this TB (for unlinking).
    pub incoming: Vec<(u32, u8)>,
    /// False once invalidated: no longer reachable through the map or new links.
    pub valid: bool,
}

impl TranslationBlock {
    /// The guest instruction whose host code contains `rip`.
    pub fn entry_for(&self, rip: u64) -> Option<&PcEntry> {
        let off = rip.checked_sub(self.host)? as u32;
        if off >= self.host_len {
            return None;
        }
        let i = self.pcmap.partition_point(|e| e.host_off <= off);
        i.checked_sub(1).map(|i| &self.pcmap[i])
    }
}

/// All TBs of the current generation.
#[derive(Default)]
pub struct TbCache {
    tbs: Vec<TranslationBlock>,
    map: FxHashMap<u64, u32>,
    /// Number of full flushes so far.
    pub generation: u64,
}

impl TbCache {
    pub fn lookup(&self, pc: u64) -> Option<u32> {
        self.map.get(&pc).copied()
    }

    /// Id the next inserted TB will get.
    pub fn next_id(&self) -> u32 {
        self.tbs.len() as u32
    }

    /// Insert a TB (its host address must be above every existing one).
    pub fn insert(&mut self, tb: TranslationBlock) -> u32 {
        debug_assert!(self.tbs.last().is_none_or(|l| l.host < tb.host));
        let id = self.tbs.len() as u32;
        self.map.insert(tb.guest_pc, id);
        self.tbs.push(tb);
        id
    }

    pub fn get(&self, id: u32) -> &TranslationBlock {
        &self.tbs[id as usize]
    }

    pub fn get_mut(&mut self, id: u32) -> &mut TranslationBlock {
        &mut self.tbs[id as usize]
    }

    /// Remove TB `id` from the pc map and mark it invalid. Its code stays in the buffer until
    /// the next full flush; the caller unlinks incoming chains first (`chain::unlink_incoming`).
    pub fn invalidate(&mut self, id: u32) {
        let tb = &mut self.tbs[id as usize];
        tb.valid = false;
        if self.map.get(&tb.guest_pc) == Some(&id) {
            self.map.remove(&tb.guest_pc);
        }
    }

    pub fn len(&self) -> usize {
        self.tbs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tbs.is_empty()
    }

    /// The TB whose host code contains `rip`.
    pub fn find_host(&self, rip: u64) -> Option<&TranslationBlock> {
        let i = self.tbs.partition_point(|t| t.host <= rip);
        let tb = &self.tbs[i.checked_sub(1)?];
        (rip < tb.host + tb.host_len as u64).then_some(tb)
    }

    /// Drop every TB (the caller also resets the code buffer).
    pub fn flush(&mut self) {
        self.tbs.clear();
        self.map.clear();
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(pc: u64, host: u64, len: u32) -> TranslationBlock {
        TranslationBlock {
            guest_pc: pc,
            insns: Vec::new(),
            fetch_fault: None,
            guest_bytes: 0,
            host,
            host_len: len,
            pcmap: vec![
                PcEntry {
                    host_off: 0,
                    idx: 0,
                    guest_pc: pc,
                },
                PcEntry {
                    host_off: 10,
                    idx: 1,
                    guest_pc: pc + 4,
                },
            ],
            exits: [None, None],
            incoming: Vec::new(),
            valid: true,
        }
    }

    #[test]
    fn lookup_find_host_flush() {
        let mut c = TbCache::default();
        assert_eq!(c.insert(tb(0x100, 0x1000, 32)), 0);
        assert_eq!(c.insert(tb(0x200, 0x1020, 16)), 1);
        assert_eq!(c.lookup(0x200), Some(1));
        assert_eq!(c.find_host(0x1000).unwrap().guest_pc, 0x100);
        assert_eq!(c.find_host(0x102f).unwrap().guest_pc, 0x200);
        assert!(c.find_host(0x1030).is_none() && c.find_host(0xfff).is_none());
        let t = c.find_host(0x1015).unwrap();
        assert_eq!(t.entry_for(0x1009).unwrap().guest_pc, 0x100);
        assert_eq!(t.entry_for(0x100a).unwrap().guest_pc, 0x104);
        c.invalidate(1);
        assert!(c.lookup(0x200).is_none() && !c.get(1).valid);
        assert_eq!(c.find_host(0x1020).unwrap().guest_pc, 0x200);
        c.flush();
        assert!(c.is_empty() && c.lookup(0x100).is_none());
        assert_eq!(c.generation, 1);
    }
}
