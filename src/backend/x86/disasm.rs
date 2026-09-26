//! Host-code disassembly for `--dump-x86` and lockstep divergence reports. With the `disasm`
//! cargo feature it uses `iced-x86`; without it, a hex dump plus the objdump command line.

/// Disassemble `code` located at host address `ip`, one instruction per line.
#[cfg(feature = "disasm")]
pub fn disasm_x86(code: &[u8], ip: u64) -> String {
    use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};
    let mut d = Decoder::with_ip(64, code, ip, DecoderOptions::NONE);
    let mut f = IntelFormatter::new();
    let mut out = String::new();
    let mut text = String::new();
    for i in &mut d {
        text.clear();
        f.format(&i, &mut text);
        let off = (i.ip() - ip) as usize;
        let bytes: String = code[off..off + i.len()]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        out += &format!("  {:#x}: {bytes:<24} {text}\n", i.ip());
    }
    out
}

/// Hex dump of `code` (build with `--features disasm` for real disassembly).
#[cfg(not(feature = "disasm"))]
pub fn disasm_x86(code: &[u8], ip: u64) -> String {
    let mut out = format!(
        "  (hex; disassemble with `objdump -D -b binary -m i386:x86-64 --adjust-vma={ip:#x}`)\n"
    );
    for (n, chunk) in code.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        out += &format!("  {:#x}: {}\n", ip + 16 * n as u64, hex.join(" "));
    }
    out
}
