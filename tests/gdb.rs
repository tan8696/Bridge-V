//! Phase 10: the GDB remote stub (`bridgev run --gdb PORT`), driven by a minimal Remote Serial
//! Protocol client (so the test needs no riscv-capable gdb): registers, memory, a software
//! breakpoint at `main`, a single step, and running to a normal exit.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bridgev::elf::Elf;

struct Rsp(TcpStream);

impl Rsp {
    fn cmd(&mut self, body: &str) -> String {
        let sum = body.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        self.0
            .write_all(format!("${body}#{sum:02x}").as_bytes())
            .unwrap();
        // Reply: '+' ack, then $...#xx.
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        loop {
            self.0.read_exact(&mut b).unwrap();
            if b[0] == b'$' {
                break;
            }
        }
        loop {
            self.0.read_exact(&mut b).unwrap();
            if b[0] == b'#' {
                break;
            }
            out.push(b[0]);
        }
        let mut sum = [0u8; 2];
        self.0.read_exact(&mut sum).unwrap();
        self.0.write_all(b"+").unwrap();
        String::from_utf8(out).unwrap()
    }
}

fn le64(hex: &str) -> u64 {
    let b: Vec<u8> = (0..16)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    u64::from_le_bytes(b.try_into().unwrap())
}

#[test]
fn gdb_stub_breakpoint_step_and_exit() {
    let Some(elf_path) = common::guest_elf("hello-O2") else {
        return;
    };
    let data = std::fs::read(&elf_path).unwrap();
    let elf = Elf::parse(&data).unwrap();
    let main = elf.symbol("main").expect("main symbol");
    let port = 20000 + (std::process::id() % 20000) as u16;
    let child = Command::new(env!("CARGO_BIN_EXE_bridgev"))
        .args(["run", "--gdb", &port.to_string()])
        .arg(&elf_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(e) => {
                assert!(t0.elapsed() < Duration::from_secs(10), "connect: {e}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut g = Rsp(stream);

    assert!(g.cmd("qSupported:swbreak+").contains("PacketSize"));
    assert_eq!(g.cmd("?"), "S05");
    let regs = g.cmd("g");
    assert_eq!(regs.len(), 33 * 16, "{regs}");
    assert_eq!(le64(&regs[32 * 16..]), elf.entry, "pc at the entry point");
    assert_ne!(le64(&regs[2 * 16..3 * 16]), 0, "sp set up");
    assert!(
        g.cmd("qXfer:features:read:target.xml:0,1000")
            .contains("riscv:rv64")
    );

    assert_eq!(g.cmd(&format!("Z0,{main:x},2")), "OK");
    assert_eq!(g.cmd("c"), "S05");
    assert_eq!(le64(&g.cmd("p20")), main, "stopped at main");
    // Memory at main is the program's code.
    let seg = elf
        .loads()
        .find(|s| s.vaddr <= main && main < s.vaddr + s.filesz)
        .unwrap();
    let off = (main - seg.vaddr) as usize;
    let want: String = elf.segment_data(seg)[off..off + 4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(g.cmd(&format!("m{main:x},4")), want);
    // One instruction.
    assert_eq!(g.cmd("s"), "S05");
    assert_ne!(le64(&g.cmd("p20")), main);
    // Write a register and read it back.
    assert_eq!(g.cmd("P7=efbeadde00000000"), "OK");
    assert_eq!(le64(&g.cmd("p7")), 0xdead_beef);
    assert_eq!(g.cmd(&format!("z0,{main:x},2")), "OK");
    assert_eq!(g.cmd("c"), "W00");
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Hello, world!\n");
}
