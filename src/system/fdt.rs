//! Flattened devicetree (DTB) writer and the `virt`-compatible machine description
//! (CLAUDE.md §20.3, P9.2). Format (Devicetree Specification v0.4 §5): a big-endian header, the
//! memory reservation block, the structure block (BEGIN_NODE 1, END_NODE 2, PROP 3, END 9;
//! names and values padded to 4 bytes) and the strings block.

use super::machine::{CLINT_BASE, PLIC_BASE, SYSCON_BASE, UART_BASE};

const FDT_MAGIC: u32 = 0xd00d_feed;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const END: u32 = 9;

/// Builds a DTB node by node.
#[derive(Default)]
pub struct Fdt {
    structs: Vec<u8>,
    strings: Vec<u8>,
    depth: usize,
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

impl Fdt {
    pub fn new() -> Self {
        Self::default()
    }

    fn u32(&mut self, x: u32) {
        self.structs.extend_from_slice(&x.to_be_bytes());
    }

    pub fn begin(&mut self, name: &str) {
        self.u32(BEGIN_NODE);
        self.structs.extend_from_slice(name.as_bytes());
        self.structs.push(0);
        pad4(&mut self.structs);
        self.depth += 1;
    }

    pub fn end(&mut self) {
        assert!(self.depth > 0, "unbalanced end");
        self.u32(END_NODE);
        self.depth -= 1;
    }

    fn name_off(&mut self, name: &str) -> u32 {
        // Reuse an identical name already in the strings block.
        let bytes = name.as_bytes();
        let mut i = 0;
        while i < self.strings.len() {
            let end = i + self.strings[i..].iter().position(|&b| b == 0).unwrap();
            if &self.strings[i..end] == bytes {
                return i as u32;
            }
            i = end + 1;
        }
        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(bytes);
        self.strings.push(0);
        off
    }

    pub fn prop(&mut self, name: &str, value: &[u8]) {
        let off = self.name_off(name);
        self.u32(PROP);
        self.u32(value.len() as u32);
        self.u32(off);
        self.structs.extend_from_slice(value);
        pad4(&mut self.structs);
    }

    pub fn prop_empty(&mut self, name: &str) {
        self.prop(name, &[]);
    }

    pub fn prop_u32(&mut self, name: &str, v: u32) {
        self.prop(name, &v.to_be_bytes());
    }

    pub fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let v: Vec<u8> = cells.iter().flat_map(|c| c.to_be_bytes()).collect();
        self.prop(name, &v);
    }

    pub fn prop_u64(&mut self, name: &str, v: u64) {
        self.prop(name, &v.to_be_bytes());
    }

    pub fn prop_str(&mut self, name: &str, s: &str) {
        self.prop_strs(name, &[s]);
    }

    /// A string list (each NUL-terminated).
    pub fn prop_strs(&mut self, name: &str, ss: &[&str]) {
        let mut v = Vec::new();
        for s in ss {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        self.prop(name, &v);
    }

    /// `reg = <addr size>` with #address-cells = #size-cells = 2.
    pub fn prop_reg(&mut self, base: u64, size: u64) {
        self.prop_cells(
            "reg",
            &[
                (base >> 32) as u32,
                base as u32,
                (size >> 32) as u32,
                size as u32,
            ],
        );
    }

    /// The finished blob, with the given memory reservations.
    pub fn finish(mut self, reserve: &[(u64, u64)]) -> Vec<u8> {
        assert_eq!(self.depth, 0, "unclosed nodes");
        self.u32(END);
        let mut rsv = Vec::new();
        for &(a, s) in reserve.iter().chain([(0, 0)].iter()) {
            rsv.extend_from_slice(&a.to_be_bytes());
            rsv.extend_from_slice(&s.to_be_bytes());
        }
        let off_rsv = 40u32; // header size, 8-byte aligned
        let off_struct = off_rsv + rsv.len() as u32;
        let off_strings = off_struct + self.structs.len() as u32;
        let total = off_strings + self.strings.len() as u32;
        let mut out = Vec::with_capacity(total as usize);
        for x in [
            FDT_MAGIC,
            total,
            off_struct,
            off_strings,
            off_rsv,
            17, // version
            16, // last compatible version
            0,  // boot CPU
            self.strings.len() as u32,
            self.structs.len() as u32,
        ] {
            out.extend_from_slice(&x.to_be_bytes());
        }
        out.extend_from_slice(&rsv);
        out.extend_from_slice(&self.structs);
        out.extend_from_slice(&self.strings);
        out
    }
}

/// What the generated devicetree describes.
pub struct VirtConfig {
    pub ram_base: u64,
    pub ram_size: u64,
    pub bootargs: String,
    pub initrd: Option<(u64, u64)>,
    /// Advertise Sv48 (`mmu-type = "riscv,sv48"`) instead of Sv39.
    pub sv48: bool,
    /// A virtio-blk device at `VIRTIO_BASE` (Phase 10).
    pub virtio_blk: bool,
}

const PH_INTC: u32 = 1;
const PH_PLIC: u32 = 2;
const PH_TEST: u32 = 3;

/// The devicetree of Bridge-V's `virt`-compatible machine (§20.3): one hart (rv64imafdc,
/// Sv39), RAM, CLINT, PLIC, one NS16550A UART on PLIC source 10, the SiFive test finisher with
/// syscon poweroff/reboot nodes.
pub fn virt_dtb(c: &VirtConfig) -> Vec<u8> {
    let mut f = Fdt::new();
    f.begin("");
    f.prop_u32("#address-cells", 2);
    f.prop_u32("#size-cells", 2);
    f.prop_str("compatible", "riscv-virtio");
    f.prop_str("model", "bridgev,virt");

    f.begin("chosen");
    f.prop_str("bootargs", &c.bootargs);
    f.prop_str("stdout-path", "/soc/serial@10000000");
    if let Some((start, end)) = c.initrd {
        f.prop_u64("linux,initrd-start", start);
        f.prop_u64("linux,initrd-end", end);
    }
    f.end();

    f.begin(&format!("memory@{:x}", c.ram_base));
    f.prop_str("device_type", "memory");
    f.prop_reg(c.ram_base, c.ram_size);
    f.end();

    f.begin("cpus");
    f.prop_u32("#address-cells", 1);
    f.prop_u32("#size-cells", 0);
    f.prop_u32("timebase-frequency", 10_000_000);
    f.begin("cpu@0");
    f.prop_str("device_type", "cpu");
    f.prop_u32("reg", 0);
    f.prop_str("status", "okay");
    f.prop_str("compatible", "riscv");
    f.prop_str("riscv,isa", "rv64imafdc_zicntr_zicsr_zifencei");
    f.prop_str("riscv,isa-base", "rv64i");
    f.prop_strs(
        "riscv,isa-extensions",
        &["i", "m", "a", "f", "d", "c", "zicntr", "zicsr", "zifencei"],
    );
    f.prop_str("mmu-type", if c.sv48 { "riscv,sv48" } else { "riscv,sv39" });
    f.begin("interrupt-controller");
    f.prop_u32("#interrupt-cells", 1);
    f.prop_empty("interrupt-controller");
    f.prop_str("compatible", "riscv,cpu-intc");
    f.prop_u32("phandle", PH_INTC);
    f.end();
    f.end();
    f.end();

    f.begin("soc");
    f.prop_u32("#address-cells", 2);
    f.prop_u32("#size-cells", 2);
    f.prop_str("compatible", "simple-bus");
    f.prop_empty("ranges");

    f.begin(&format!("clint@{CLINT_BASE:x}"));
    f.prop_strs("compatible", &["sifive,clint0", "riscv,clint0"]);
    f.prop_reg(CLINT_BASE, 0x10000);
    f.prop_cells("interrupts-extended", &[PH_INTC, 3, PH_INTC, 7]);
    f.end();

    f.begin(&format!("plic@{PLIC_BASE:x}"));
    f.prop_strs("compatible", &["sifive,plic-1.0.0", "riscv,plic0"]);
    f.prop_u32("#address-cells", 0);
    f.prop_u32("#interrupt-cells", 1);
    f.prop_empty("interrupt-controller");
    f.prop_reg(PLIC_BASE, 0x60_0000);
    f.prop_u32("riscv,ndev", super::plic::NDEV as u32);
    f.prop_cells("interrupts-extended", &[PH_INTC, 11, PH_INTC, 9]);
    f.prop_u32("phandle", PH_PLIC);
    f.end();

    f.begin(&format!("serial@{UART_BASE:x}"));
    f.prop_str("compatible", "ns16550a");
    f.prop_reg(UART_BASE, 0x100);
    f.prop_u32("clock-frequency", 3_686_400);
    f.prop_u32("interrupts", 10);
    f.prop_u32("interrupt-parent", PH_PLIC);
    f.end();

    if c.virtio_blk {
        use super::virtio_blk::{VIRTIO_BASE, VIRTIO_IRQ};
        f.begin(&format!("virtio_mmio@{VIRTIO_BASE:x}"));
        f.prop_str("compatible", "virtio,mmio");
        f.prop_reg(VIRTIO_BASE, 0x1000);
        f.prop_u32("interrupts", VIRTIO_IRQ as u32);
        f.prop_u32("interrupt-parent", PH_PLIC);
        f.end();
    }

    f.begin(&format!("test@{SYSCON_BASE:x}"));
    f.prop_strs("compatible", &["sifive,test1", "sifive,test0", "syscon"]);
    f.prop_reg(SYSCON_BASE, 0x1000);
    f.prop_u32("phandle", PH_TEST);
    f.end();

    f.end(); // soc

    for (node, compat, value) in [
        ("poweroff", "syscon-poweroff", 0x5555),
        ("reboot", "syscon-reboot", 0x7777),
    ] {
        f.begin(node);
        f.prop_str("compatible", compat);
        f.prop_u32("regmap", PH_TEST);
        f.prop_u32("offset", 0);
        f.prop_u32("value", value);
        f.end();
    }
    f.end(); // root
    f.finish(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_blocks_are_consistent() {
        let dtb = virt_dtb(&VirtConfig {
            ram_base: 0x8000_0000,
            ram_size: 512 << 20,
            bootargs: "console=ttyS0".into(),
            initrd: Some((0x8800_0000, 0x8810_0000)),
            sv48: false,
            virtio_blk: true,
        });
        let be = |o: usize| u32::from_be_bytes(dtb[o..o + 4].try_into().unwrap());
        assert_eq!(be(0), FDT_MAGIC);
        assert_eq!(be(4) as usize, dtb.len());
        let (st, strs) = (be(8) as usize, be(12) as usize);
        assert_eq!(
            st + be(36) as usize,
            strs,
            "structure block precedes the strings"
        );
        assert_eq!(strs + be(32) as usize, dtb.len());
        assert_eq!(be(st), BEGIN_NODE);
        assert_eq!(be(strs - 4), END);
        let strings = &dtb[strs..];
        for name in [
            "riscv,isa",
            "bootargs",
            "linux,initrd-start",
            "interrupts-extended",
        ] {
            assert!(
                strings.windows(name.len()).any(|w| w == name.as_bytes()),
                "missing {name}"
            );
        }
    }

    /// The blob decompiles with `dtc` (installed by tools/setup.sh; skipped without it)
    /// without warnings, and describes the devices Linux binds.
    #[test]
    fn dtc_accepts_the_blob() {
        let dtb = virt_dtb(&VirtConfig {
            ram_base: 0x8000_0000,
            ram_size: 512 << 20,
            bootargs: "console=ttyS0 earlycon=sbi".into(),
            initrd: Some((0x9f00_0000, 0x9f40_0000)),
            sv48: false,
            virtio_blk: true,
        });
        let path = std::env::temp_dir().join(format!("bridgev-{}.dtb", std::process::id()));
        std::fs::write(&path, &dtb).unwrap();
        let out = std::process::Command::new("dtc")
            .args(["-I", "dtb", "-O", "dts"])
            .arg(&path)
            .output();
        std::fs::remove_file(&path).unwrap();
        let Ok(out) = out else {
            eprintln!("dtc not installed: skipped");
            return;
        };
        let (dts, err) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(out.status.success(), "{err}");
        assert!(err.trim().is_empty(), "dtc warnings: {err}");
        for want in [
            "memory@80000000",
            "reg = <0x00 0x80000000 0x00 0x20000000>;",
            "riscv,isa = \"rv64imafdc_zicntr_zicsr_zifencei\";",
            "mmu-type = \"riscv,sv39\";",
            "clint@2000000",
            "plic@c000000",
            "serial@10000000",
            "compatible = \"ns16550a\";",
            "test@100000",
            "linux,initrd-start = <0x00 0x9f000000>;",
            "stdout-path = \"/soc/serial@10000000\";",
            "compatible = \"virtio,mmio\";",
        ] {
            assert!(dts.contains(want), "missing {want:?} in\n{dts}");
        }
    }
}
