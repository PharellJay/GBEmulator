//! DIV/TIMA/TMA/TAC timer, modelled on the internal 16-bit divider.

use crate::interrupt;
use crate::state::{self, Reader, Snapshot, Writer};

pub(crate) struct Timer {
    /// Internal 16-bit divider; DIV is the upper byte.
    div: u16,
    tima: u8,
    tma: u8,
    tac: u8,
    /// TIMA overflowed last cycle; reload from TMA on the next one.
    overflow_pending: bool,
    /// TIMA was reloaded from TMA during the current M-cycle.
    reloaded_this_cycle: bool,
}

impl Timer {
    pub(crate) fn new() -> Self {
        Self {
            div: 0xABCC,
            tima: 0,
            tma: 0,
            tac: 0,
            overflow_pending: false,
            reloaded_this_cycle: false,
        }
    }

    /// The divider bit selected by TAC, ANDed with the enable bit.
    fn signal(&self) -> bool {
        let bit = match self.tac & 0x03 {
            0 => 9,
            1 => 3,
            2 => 5,
            _ => 7,
        };
        self.tac & 0x04 != 0 && self.div & (1 << bit) != 0
    }

    fn increment_tima(&mut self) {
        let (value, overflow) = self.tima.overflowing_add(1);
        self.tima = value;
        if overflow {
            self.overflow_pending = true;
        }
    }

    /// Internal 16-bit divider (DIV is its upper byte).
    pub(crate) fn counter(&self) -> u16 {
        self.div
    }

    /// Advance one M-cycle (4 T-cycles).
    pub(crate) fn tick(&mut self, if_reg: &mut u8) {
        self.reloaded_this_cycle = false;
        if self.overflow_pending {
            self.overflow_pending = false;
            self.tima = self.tma;
            *if_reg |= interrupt::TIMER;
            self.reloaded_this_cycle = true;
        }
        let before = self.signal();
        self.div = self.div.wrapping_add(4);
        if before && !self.signal() {
            self.increment_tima();
        }
    }

    pub(crate) fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF04 => (self.div >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            _ => 0xF8 | self.tac,
        }
    }

    pub(crate) fn write(&mut self, addr: u16, value: u8) {
        let before = self.signal();
        match addr {
            0xFF04 => self.div = 0,
            0xFF05 => {
                // A write during the overflow cycle cancels the reload;
                // a write during the reload cycle is ignored.
                self.overflow_pending = false;
                if !self.reloaded_this_cycle {
                    self.tima = value;
                }
            }
            0xFF06 => {
                self.tma = value;
                if self.reloaded_this_cycle {
                    self.tima = value;
                }
            }
            _ => self.tac = value & 0x07,
        }
        // Changing DIV or TAC can produce a falling edge on the timer input.
        if before && !self.signal() {
            self.increment_tima();
        }
    }
}

impl Snapshot for Timer {
    fn save(&self, w: &mut Writer) {
        w.u16(self.div);
        w.u8(self.tima);
        w.u8(self.tma);
        w.u8(self.tac);
        w.bool(self.overflow_pending);
        w.bool(self.reloaded_this_cycle);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.div = r.u16()?;
        self.tima = r.u8()?;
        self.tma = r.u8()?;
        self.tac = r.u8()? & 0x07;
        self.overflow_pending = r.bool()?;
        self.reloaded_this_cycle = r.bool()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tima_counts_every_four_mcycles_at_fastest_rate() {
        let mut t = Timer::new();
        let mut if_reg = 0;
        t.write(0xFF04, 0);
        t.write(0xFF07, 0x05);
        for _ in 0..16 {
            t.tick(&mut if_reg);
        }
        assert_eq!(t.read(0xFF05), 4);
    }

    #[test]
    fn overflow_reloads_one_cycle_late() {
        let mut t = Timer::new();
        let mut if_reg = 0;
        t.write(0xFF04, 0);
        t.write(0xFF06, 0x42);
        t.write(0xFF05, 0xFF);
        t.write(0xFF07, 0x05);
        for _ in 0..4 {
            t.tick(&mut if_reg);
        }
        assert_eq!(
            t.read(0xFF05),
            0x00,
            "TIMA reads 0 for one cycle after overflow"
        );
        assert_eq!(if_reg & interrupt::TIMER, 0);
        t.tick(&mut if_reg);
        assert_eq!(t.read(0xFF05), 0x42);
        assert_ne!(if_reg & interrupt::TIMER, 0);
    }

    #[test]
    fn div_reset_can_tick_tima() {
        let mut t = Timer::new();
        let mut if_reg = 0;
        t.write(0xFF04, 0);
        t.write(0xFF07, 0x05);
        t.tick(&mut if_reg);
        t.tick(&mut if_reg); // div = 8: bit 3 set
        let before = t.read(0xFF05);
        t.write(0xFF04, 0);
        assert_eq!(t.read(0xFF05), before + 1);
    }
}
