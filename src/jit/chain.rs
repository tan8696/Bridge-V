//! Direct block chaining (CLAUDE.md §13.3, D7, P3.2): patch a TB's exit slot so it jumps
//! straight into its successor's code, and undo such links when the successor goes away.
//!
//! Every chainable exit is a `jmp rel32` (slot 0) or `jcc rel32` (slot 1) whose rel32 field is
//! 4-byte aligned and initially targets the TB's own exit stub. Linking rewrites that field to
//! the successor's entry (computed against RX addresses, written through the RW view as one
//! aligned 32-bit store). The successor records `(tb, slot)` in its `incoming` list so that
//! invalidating it can point every predecessor back at its stub.

use super::cache::TbCache;
use super::code_mem::CodeMem;

/// Link exit `slot` of TB `from` to TB `to`. Returns false (and changes nothing) if the slot
/// is not chainable, already points at `to`, or either TB is invalid.
pub fn link(cache: &mut TbCache, cm: &mut CodeMem, from: u32, slot: u8, to: u32) -> bool {
    let (to_host, to_pc, to_valid) = {
        let t = cache.get(to);
        (t.host, t.guest_pc, t.valid)
    };
    let f = cache.get(from);
    let Some(ex) = f.exits[slot as usize] else {
        return false;
    };
    if !f.valid || !to_valid || ex.linked == Some(to) {
        return false;
    }
    assert_eq!(
        ex.target_pc, to_pc,
        "linking exit to a TB for a different pc"
    );
    if let Some(old) = ex.linked {
        cache.get_mut(old).incoming.retain(|&e| e != (from, slot));
    }
    cm.patch_rel32(ex.patch_at, to_host);
    cache.get_mut(from).exits[slot as usize]
        .as_mut()
        .expect("checked above")
        .linked = Some(to);
    cache.get_mut(to).incoming.push((from, slot));
    true
}

/// Point every exit linked to TB `to` back at its own stub. Returns how many were unlinked.
pub fn unlink_incoming(cache: &mut TbCache, cm: &mut CodeMem, to: u32) -> usize {
    let incoming = std::mem::take(&mut cache.get_mut(to).incoming);
    for &(from, slot) in &incoming {
        let ex = cache.get_mut(from).exits[slot as usize]
            .as_mut()
            .expect("incoming entry for a non-chainable slot");
        debug_assert_eq!(ex.linked, Some(to));
        ex.linked = None;
        let (field, stub) = (ex.patch_at, ex.stub);
        cm.patch_rel32(field, stub);
    }
    incoming.len()
}

#[cfg(test)]
mod tests {
    use crate::backend::x86::emit::Asm;
    use crate::jit::code_mem::{CodeMem, WxMode};

    /// CLAUDE.md §28.5: a `jmp rel32` near +0x1040 retargeted to +0x2000. The E9 must be padded
    /// to +0x1043 so its rel32 field (+0x1044) is 4-byte aligned; rel = 0x2000 - 0x1048.
    #[test]
    fn patch_arithmetic_worked_example() {
        for mode in [WxMode::DualMap, WxMode::Mprotect] {
            let mut cm = CodeMem::new(1 << 16, mode).unwrap();
            let base = cm.next_addr();
            let mut a = Asm::new(base);
            a.nop(0x1040);
            a.align(4, 1);
            assert_eq!(a.pos(), 0x1043);
            let field = base + a.jmp_abs(base) as u64;
            assert_eq!(field, base + 0x1044);
            cm.place(base, &a.finish()).unwrap();
            cm.patch_rel32(field, base + 0x2000);
            assert_eq!(cm.read(base + 0x1043, 5), &[0xE9, 0xB8, 0x0F, 0x00, 0x00]);
        }
    }

    #[test]
    #[should_panic(expected = "unaligned rel32 field")]
    fn unaligned_patch_is_rejected() {
        let mut cm = CodeMem::new(4096, WxMode::DualMap).unwrap();
        let base = cm.next_addr();
        cm.place(base, &[0x90; 16]).unwrap();
        cm.patch_rel32(base + 1, base);
    }
}
