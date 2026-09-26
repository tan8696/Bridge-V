//! `--perf-map`: appends `"<hex start> <hex size> tb_<guest pc>"` lines to
//! `/tmp/perf-<pid>.map`, the format `perf report` uses to symbolize JIT code (§23).

use std::fs::{File, OpenOptions};
use std::io::Write;

pub struct PerfMap {
    file: File,
}

impl PerfMap {
    pub fn open() -> std::io::Result<PerfMap> {
        let path = format!("/tmp/perf-{}.map", std::process::id());
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(PerfMap { file })
    }

    /// Record one TB. Errors are ignored: the map is a debugging aid.
    pub fn record(&mut self, host: u64, len: u32, guest_pc: u64) {
        let _ = writeln!(self.file, "{host:x} {len:x} tb_{guest_pc:x}");
    }
}
