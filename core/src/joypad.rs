//! Joypad register (P1/JOYP, 0xFF00).

use crate::interrupt;
use crate::state::{self, Reader, Snapshot, Writer};

/// A Game Boy button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Right,
    Left,
    Up,
    Down,
    A,
    B,
    Select,
    Start,
}

impl Button {
    /// Every button, in register bit order (directions, then actions).
    pub const ALL: [Button; 8] = [
        Button::Right,
        Button::Left,
        Button::Up,
        Button::Down,
        Button::A,
        Button::B,
        Button::Select,
        Button::Start,
    ];

    fn mask(self) -> u8 {
        1 << (self as u8)
    }
}

pub(crate) struct Joypad {
    /// Bits 4-5 as last written: 0 selects that button group.
    select: u8,
    /// Pressed buttons: bits 0-3 directions, bits 4-7 actions (1 = pressed).
    pressed: u8,
}

impl Joypad {
    pub(crate) fn new() -> Self {
        Self {
            select: 0x30,
            pressed: 0,
        }
    }

    /// Active-low input lines for the currently selected groups.
    fn lines(&self) -> u8 {
        let mut low = 0;
        if self.select & 0x10 == 0 {
            low |= self.pressed & 0x0F;
        }
        if self.select & 0x20 == 0 {
            low |= self.pressed >> 4;
        }
        !low & 0x0F
    }

    pub(crate) fn read(&self) -> u8 {
        0xC0 | self.select | self.lines()
    }

    pub(crate) fn write(&mut self, value: u8) {
        self.select = value & 0x30;
    }

    pub(crate) fn set(&mut self, button: Button, pressed: bool, if_reg: &mut u8) {
        let before = self.lines();
        if pressed {
            self.pressed |= button.mask();
        } else {
            self.pressed &= !button.mask();
        }
        // Interrupt on any selected line going from high to low.
        if before & !self.lines() != 0 {
            *if_reg |= interrupt::JOYPAD;
        }
    }
}

/// Pressed buttons are live input, so only the select lines are saved.
impl Snapshot for Joypad {
    fn save(&self, w: &mut Writer) {
        w.u8(self.select);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.select = r.u8()? & 0x30;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_group_reads_active_low() {
        let mut j = Joypad::new();
        let mut if_reg = 0;
        j.set(Button::Start, true, &mut if_reg);
        j.set(Button::Left, true, &mut if_reg);
        j.write(0x10); // select actions
        assert_eq!(j.read() & 0x0F, 0b0111);
        j.write(0x20); // select directions
        assert_eq!(j.read() & 0x0F, 0b1101);
        j.write(0x30); // nothing selected
        assert_eq!(j.read() & 0x0F, 0x0F);
    }

    #[test]
    fn press_on_selected_group_requests_interrupt() {
        let mut j = Joypad::new();
        let mut if_reg = 0;
        j.write(0x20);
        j.set(Button::A, true, &mut if_reg);
        assert_eq!(if_reg, 0, "actions not selected");
        j.set(Button::Down, true, &mut if_reg);
        assert_eq!(if_reg, interrupt::JOYPAD);
    }
}
