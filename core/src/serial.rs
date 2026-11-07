//! Serial port. No link partner: transfers shift in 0xFF. Outgoing bytes are
//! recorded so headless tests can read them.

use crate::interrupt;
use crate::state::{self, Reader, Snapshot, Writer};

/// M-cycles for one 8-bit transfer at the internal 8192 Hz clock.
const TRANSFER_MCYCLES: u32 = 1024;

pub(crate) struct Serial {
    sb: u8,
    sc: u8,
    remaining: u32,
    pub(crate) output: Vec<u8>,
}

impl Serial {
    pub(crate) fn new() -> Self {
        Self {
            sb: 0,
            sc: 0,
            remaining: 0,
            output: Vec::new(),
        }
    }

    pub(crate) fn tick(&mut self, if_reg: &mut u8) {
        if self.remaining > 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.sb = 0xFF;
                self.sc &= 0x7F;
                *if_reg |= interrupt::SERIAL;
            }
        }
    }

    pub(crate) fn read(&self, addr: u16) -> u8 {
        if addr == 0xFF01 {
            self.sb
        } else {
            0x7E | self.sc
        }
    }

    pub(crate) fn write(&mut self, addr: u16, value: u8) {
        if addr == 0xFF01 {
            self.sb = value;
            return;
        }
        self.sc = value & 0x81;
        if value & 0x81 == 0x81 {
            self.output.push(self.sb);
            self.remaining = TRANSFER_MCYCLES;
        }
    }
}

/// The captured `output` is a debugging aid and not part of the state.
impl Snapshot for Serial {
    fn save(&self, w: &mut Writer) {
        w.u8(self.sb);
        w.u8(self.sc);
        w.u32(self.remaining);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.sb = r.u8()?;
        self.sc = r.u8()? & 0x81;
        self.remaining = r.u32()?.min(TRANSFER_MCYCLES);
        Ok(())
    }
}
