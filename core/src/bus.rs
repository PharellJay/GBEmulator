//! Memory map. Every CPU memory access ticks the rest of the system by one
//! M-cycle, which keeps all components in lockstep.

use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::joypad::Joypad;
use crate::ppu::Ppu;
use crate::serial::Serial;
use crate::state::{self, Reader, Snapshot, Writer};
use crate::timer::Timer;

/// An in-progress OAM DMA transfer.
struct Dma {
    source: u16,
    /// Next byte to copy; transfers start after a one-cycle setup delay.
    index: i16,
}

pub(crate) struct Bus {
    pub(crate) cart: Cartridge,
    pub(crate) ppu: Ppu,
    pub(crate) serial: Serial,
    pub(crate) joypad: Joypad,
    pub(crate) apu: Apu,
    timer: Timer,
    dma: Option<Dma>,
    /// Last value written to 0xFF46.
    dma_reg: u8,
    wram: [u8; 0x2000],
    hram: [u8; 0x7F],
    ie: u8,
    if_reg: u8,
    /// Total M-cycles elapsed since power-on.
    pub(crate) cycles: u64,
}

impl Bus {
    pub(crate) fn new(cart: Cartridge) -> Self {
        Self {
            cart,
            ppu: Ppu::new(),
            serial: Serial::new(),
            joypad: Joypad::new(),
            apu: Apu::new(),
            timer: Timer::new(),
            dma: None,
            dma_reg: 0xFF,
            wram: [0; 0x2000],
            hram: [0; 0x7F],
            ie: 0,
            if_reg: 0x01,
            cycles: 0,
        }
    }

    /// Advance every component by one M-cycle.
    pub(crate) fn tick(&mut self) {
        self.cycles += 1;
        let div_before = self.timer.counter();
        self.timer.tick(&mut self.if_reg);
        self.clock_apu_from_div(div_before);
        self.apu.tick();
        self.ppu.tick(&mut self.if_reg);
        self.serial.tick(&mut self.if_reg);
        self.cart.tick();
        self.tick_dma();
    }

    /// The APU frame sequencer steps on each falling edge of DIV bit 4
    /// (bit 12 of the internal divider), including ones caused by DIV writes.
    fn clock_apu_from_div(&mut self, div_before: u16) {
        if div_before & 0x1000 != 0 && self.timer.counter() & 0x1000 == 0 {
            self.apu.clock_frame_sequencer();
        }
    }

    fn tick_dma(&mut self) {
        let Some(dma) = &mut self.dma else { return };
        dma.index += 1;
        if dma.index < 0 {
            return;
        }
        let (source, index) = (dma.source, dma.index as u16);
        if index >= 0xA0 {
            self.dma = None;
            return;
        }
        let src = source + index;
        // DMA sources above 0xDFFF read the WRAM echo.
        let src = if src >= 0xE000 { src - 0x2000 } else { src };
        let value = self.read_raw(src);
        self.ppu.write_oam(0xFE00 + index, value);
    }

    fn dma_active(&self) -> bool {
        self.dma.as_ref().is_some_and(|d| d.index >= 0)
    }

    /// Interrupts that are both requested and enabled.
    pub(crate) fn pending_interrupts(&self) -> u8 {
        self.ie & self.if_reg & 0x1F
    }

    pub(crate) fn set_button(&mut self, button: crate::Button, pressed: bool) {
        self.joypad.set(button, pressed, &mut self.if_reg);
    }

    pub(crate) fn acknowledge_interrupt(&mut self, mask: u8) {
        self.if_reg &= !mask;
    }

    pub(crate) fn read(&self, addr: u16) -> u8 {
        if self.dma_active() && (0xFE00..=0xFE9F).contains(&addr) {
            return 0xFF;
        }
        self.read_raw(addr)
    }

    fn read_raw(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cart.read_rom(addr),
            0x8000..=0x9FFF => self.ppu.read_vram(addr),
            0xA000..=0xBFFF => self.cart.read_ram(addr),
            0xC000..=0xDFFF => self.wram[addr as usize - 0xC000],
            0xE000..=0xFDFF => self.wram[addr as usize - 0xE000],
            0xFE00..=0xFE9F => self.ppu.read_oam(addr),
            0xFF00 => self.joypad.read(),
            0xFF01..=0xFF02 => self.serial.read(addr),
            0xFF04..=0xFF07 => self.timer.read(addr),
            0xFF10..=0xFF3F => self.apu.read(addr),
            0xFF0F => 0xE0 | self.if_reg,
            0xFF46 => self.dma_reg,
            0xFF40..=0xFF4B => self.ppu.read_reg(addr),
            0xFF80..=0xFFFE => self.hram[addr as usize - 0xFF80],
            0xFFFF => self.ie,
            _ => 0xFF,
        }
    }

    pub(crate) fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x7FFF => self.cart.write_rom(addr, value),
            0x8000..=0x9FFF => self.ppu.write_vram(addr, value),
            0xA000..=0xBFFF => self.cart.write_ram(addr, value),
            0xC000..=0xDFFF => self.wram[addr as usize - 0xC000] = value,
            0xE000..=0xFDFF => self.wram[addr as usize - 0xE000] = value,
            0xFE00..=0xFE9F => {
                if !self.dma_active() {
                    self.ppu.write_oam(addr, value);
                }
            }
            0xFF00 => self.joypad.write(value),
            0xFF01..=0xFF02 => self.serial.write(addr, value),
            0xFF04..=0xFF07 => {
                let div_before = self.timer.counter();
                self.timer.write(addr, value);
                self.clock_apu_from_div(div_before);
            }
            0xFF10..=0xFF3F => self.apu.write(addr, value),
            0xFF0F => self.if_reg = value & 0x1F,
            0xFF46 => {
                self.dma_reg = value;
                self.dma = Some(Dma {
                    source: (value as u16) << 8,
                    index: -2,
                });
            }
            0xFF40..=0xFF4B => self.ppu.write_reg(addr, value, &mut self.if_reg),
            0xFF80..=0xFFFE => self.hram[addr as usize - 0xFF80] = value,
            0xFFFF => self.ie = value,
            _ => {}
        }
    }
}

impl Snapshot for Bus {
    fn save(&self, w: &mut Writer) {
        self.cart.save(w);
        self.ppu.save(w);
        self.serial.save(w);
        self.joypad.save(w);
        self.apu.save(w);
        self.timer.save(w);
        w.bytes(&self.wram);
        w.bytes(&self.hram);
        w.u8(self.ie);
        w.u8(self.if_reg);
        w.u64(self.cycles);
        match &self.dma {
            Some(dma) => {
                w.bool(true);
                w.u16(dma.source);
                w.i16(dma.index);
            }
            None => w.bool(false),
        }
        w.u8(self.dma_reg);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.cart.load(r)?;
        self.ppu.load(r)?;
        self.serial.load(r)?;
        self.joypad.load(r)?;
        self.apu.load(r)?;
        self.timer.load(r)?;
        r.bytes(&mut self.wram)?;
        r.bytes(&mut self.hram)?;
        self.ie = r.u8()?;
        self.if_reg = r.u8()? & 0x1F;
        self.cycles = r.u64()?;
        self.dma = if r.bool()? {
            Some(Dma {
                source: r.u16()? & 0xFF00,
                index: r.i16()?.clamp(-2, 0xA0),
            })
        } else {
            None
        };
        self.dma_reg = r.u8()?;
        Ok(())
    }
}
