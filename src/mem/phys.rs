//! System-mode physical memory: RAM plus the MMIO device bus (CLAUDE.md §14.2, P7.3).
//!
//! RAM is a `DirectMem` mapping at its physical address (so a TLB entry's host address is
//! `mem_base + physical`). Devices are kept in `DirectMem::devices`, sorted by base address;
//! a physical address that is neither RAM nor a device raises an access fault.

/// A memory-mapped device. Offsets are relative to the device base; `size` is 1, 2, 4 or 8.
pub trait Mmio: Send {
    fn name(&self) -> &str;
    fn read(&mut self, off: u64, size: u64) -> u64;
    fn write(&mut self, off: u64, size: u64, val: u64);
}

/// A device and the physical range it decodes.
pub struct Device {
    pub base: u64,
    pub size: u64,
    pub dev: Box<dyn Mmio>,
}
