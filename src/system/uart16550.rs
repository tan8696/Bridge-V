//! NS16550A UART (CLAUDE.md §20.1, P9.1), register shift 0: RBR/THR/DLL 0, IER/DLM 1,
//! IIR/FCR 2, LCR 3, MCR 4, LSR 5, MSR 6, SCR 7. Transmission is instantaneous (bytes go to
//! the output sink), so THR is always empty. Received bytes come from an input queue that a
//! host thread (or a test) fills. The interrupt line (PLIC source 10) is high when received
//! data is available with IER.ERBFI, or a THR-empty event is pending with IER.ETBEI; reading
//! IIR while it reports THR empty clears that event, as on a real 16550.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::mem::phys::Mmio;

const IER_RDI: u8 = 1;
const IER_THRI: u8 = 2;
const LCR_DLAB: u8 = 0x80;
const LSR_DR: u8 = 0x01;
const LSR_THRE: u8 = 0x20;
const LSR_TEMT: u8 = 0x40;

/// Where transmitted bytes go.
#[derive(Clone)]
pub enum Sink {
    Stdout,
    Buffer(Arc<Mutex<Vec<u8>>>),
}

#[derive(Default)]
pub struct UartState {
    pub rx: VecDeque<u8>,
    ier: u8,
    lcr: u8,
    mcr: u8,
    fcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    thre_pending: bool,
}

impl UartState {
    /// The interrupt line.
    pub fn irq(&self) -> bool {
        (self.ier & IER_RDI != 0 && !self.rx.is_empty())
            || (self.ier & IER_THRI != 0 && self.thre_pending)
    }
}

pub struct Uart {
    pub state: Arc<Mutex<UartState>>,
    sink: Sink,
}

impl Uart {
    pub fn new(sink: Sink) -> Self {
        Uart {
            state: Arc::new(Mutex::new(UartState::default())),
            sink,
        }
    }
}

impl Mmio for Uart {
    fn name(&self) -> &str {
        "uart16550"
    }

    fn read(&mut self, off: u64, _size: u64) -> u64 {
        let mut s = self.state.lock().unwrap();
        let dlab = s.lcr & LCR_DLAB != 0;
        (match off {
            0 if dlab => s.dll,
            0 => s.rx.pop_front().unwrap_or(0),
            1 if dlab => s.dlm,
            1 => s.ier,
            2 => {
                let fifo = if s.fcr & 1 != 0 { 0xc0 } else { 0 };
                let id = if s.ier & IER_RDI != 0 && !s.rx.is_empty() {
                    0x04
                } else if s.ier & IER_THRI != 0 && s.thre_pending {
                    s.thre_pending = false;
                    0x02
                } else {
                    0x01
                };
                fifo | id
            }
            3 => s.lcr,
            4 => s.mcr,
            5 => LSR_THRE | LSR_TEMT | if s.rx.is_empty() { 0 } else { LSR_DR },
            6 => 0xb0, // DCD, DSR, CTS
            7 => s.scr,
            _ => 0,
        }) as u64
    }

    fn write(&mut self, off: u64, _size: u64, val: u64) {
        let v = val as u8;
        let mut s = self.state.lock().unwrap();
        let dlab = s.lcr & LCR_DLAB != 0;
        match off {
            0 if dlab => s.dll = v,
            0 => {
                match &self.sink {
                    Sink::Stdout => {
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(&[v]);
                        let _ = out.flush();
                    }
                    Sink::Buffer(b) => b.lock().unwrap().push(v),
                }
                s.thre_pending = true;
            }
            1 if dlab => s.dlm = v,
            1 => {
                // Enabling the THR-empty interrupt while THR is empty raises it at once.
                if v & IER_THRI != 0 && s.ier & IER_THRI == 0 {
                    s.thre_pending = true;
                }
                s.ier = v & 0x0f;
            }
            2 => {
                s.fcr = v;
                if v & 2 != 0 {
                    s.rx.clear();
                }
            }
            3 => s.lcr = v,
            4 => s.mcr = v,
            7 => s.scr = v,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_rx_and_interrupts() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let mut u = Uart::new(Sink::Buffer(buf.clone()));
        u.write(0, 1, b'h' as u64);
        u.write(0, 1, b'i' as u64);
        assert_eq!(&*buf.lock().unwrap(), b"hi");
        assert_eq!(u.read(5, 1) as u8 & (LSR_THRE | LSR_DR), LSR_THRE);
        assert!(!u.state.lock().unwrap().irq(), "no interrupts enabled");
        u.state.lock().unwrap().rx.extend(b"ab");
        u.write(1, 1, IER_RDI as u64);
        assert!(u.state.lock().unwrap().irq());
        assert_eq!(u.read(2, 1), 0x04, "RX data available");
        assert_eq!(u.read(5, 1) as u8 & LSR_DR, LSR_DR);
        assert_eq!((u.read(0, 1), u.read(0, 1)), (b'a' as u64, b'b' as u64));
        assert!(!u.state.lock().unwrap().irq());
        u.write(1, 1, (IER_RDI | IER_THRI) as u64); // ETBEI on: THR is empty -> interrupt
        assert!(u.state.lock().unwrap().irq());
        assert_eq!(u.read(2, 1), 0x02);
        assert!(
            !u.state.lock().unwrap().irq(),
            "reading IIR cleared the THRE event"
        );
        u.write(3, 1, LCR_DLAB as u64);
        u.write(0, 1, 0x12);
        assert_eq!(u.read(0, 1), 0x12, "DLAB selects the divisor latch");
    }
}
