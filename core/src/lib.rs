//! Frontend-agnostic Game Boy (DMG) emulation core.
//!
//! This crate has no windowing, audio-device or filesystem dependencies.

mod apu;
mod bus;
mod cartridge;
mod cpu;
mod joypad;
mod ppu;
mod rtc;
mod serial;
mod state;
mod timer;

pub use cartridge::{CartridgeError, SaveSizeError};
pub use joypad::Button;
pub use state::StateError;

use state::{Reader, Snapshot, Writer};

use bus::Bus;
use cartridge::Cartridge;
use cpu::Cpu;

/// LCD width in pixels.
pub const SCREEN_WIDTH: usize = 160;
/// LCD height in pixels.
pub const SCREEN_HEIGHT: usize = 144;
/// Master clock frequency in Hz (T-cycles per second).
pub const CPU_HZ: u32 = 4_194_304;
/// T-cycles per video frame.
pub const CYCLES_PER_FRAME: u32 = 70_224;

/// Interrupt request bits shared by IE and IF.
mod interrupt {
    pub const VBLANK: u8 = 0x01;
    pub const STAT: u8 = 0x02;
    pub const TIMER: u8 = 0x04;
    pub const SERIAL: u8 = 0x08;
    pub const JOYPAD: u8 = 0x10;
}

/// A complete DMG system: CPU plus everything on the bus.
pub struct GameBoy {
    cpu: Cpu,
    bus: Bus,
}

impl GameBoy {
    /// Load a ROM image and reset to the post-boot-ROM state.
    pub fn new(rom: Vec<u8>) -> Result<Self, CartridgeError> {
        let cart = Cartridge::from_rom(rom)?;
        Ok(Self {
            cpu: Cpu::new(),
            bus: Bus::new(cart),
        })
    }

    /// Run one CPU step and return the M-cycles it took.
    pub fn step(&mut self) -> u32 {
        let start = self.bus.cycles;
        self.cpu.step(&mut self.bus);
        (self.bus.cycles - start) as u32
    }

    /// Run until the next VBlank, or one frame's worth of cycles if the LCD is off.
    pub fn run_frame(&mut self) {
        let limit = self.bus.cycles + (CYCLES_PER_FRAME / 4) as u64;
        while self.bus.cycles < limit {
            self.cpu.step(&mut self.bus);
            if self.bus.ppu.take_frame_ready() {
                break;
            }
        }
    }

    /// Total M-cycles elapsed since power-on.
    pub fn cycles(&self) -> u64 {
        self.bus.cycles
    }

    /// Cartridge title from the ROM header.
    pub fn title(&self) -> &str {
        self.bus.cart.title()
    }

    /// The last completed frame: one shade per pixel, row-major,
    /// `SCREEN_WIDTH * SCREEN_HEIGHT` bytes, 0 = lightest to 3 = darkest.
    pub fn framebuffer(&self) -> &[u8] {
        self.bus.ppu.framebuffer()
    }

    /// Press or release a button.
    pub fn set_button(&mut self, button: Button, pressed: bool) {
        self.bus.set_button(button, pressed);
    }

    /// Set the audio output rate in Hz. 0 (the default) disables audio
    /// generation. Small adjustments are fine for drift correction.
    pub fn set_audio_sample_rate(&mut self, rate: u32) {
        self.bus.apu.set_sample_rate(rate);
    }

    /// Append all audio generated so far to `out` as interleaved stereo
    /// `f32` samples (L, R, L, R, ...), roughly in -1.0..=1.0.
    pub fn drain_audio(&mut self, out: &mut Vec<f32>) {
        self.bus.apu.drain_samples(out);
    }

    /// Whether the cartridge keeps RAM (and possibly a clock) on battery.
    pub fn has_battery(&self) -> bool {
        self.bus.cart.has_battery()
    }

    /// Returns true if battery-backed state changed since the last call.
    pub fn take_save_dirty(&mut self) -> bool {
        self.bus.cart.take_save_dirty()
    }

    /// Battery-backed state in .sav layout: cartridge RAM, followed by a
    /// 48-byte RTC block (BGB/VBA-M format) for MBC3 timer carts.
    /// `unix_now` (seconds) is stored so the clock can catch up on load.
    pub fn save_data(&self, unix_now: u64) -> Vec<u8> {
        self.bus.cart.save_data(unix_now)
    }

    /// Restore battery-backed state written by [`GameBoy::save_data`]. On a
    /// size mismatch nothing is changed.
    pub fn load_save_data(&mut self, data: &[u8], unix_now: u64) -> Result<(), SaveSizeError> {
        self.bus.cart.load_save_data(data, unix_now)
    }

    /// Snapshot the complete machine state. Only valid for this same ROM.
    pub fn save_state(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.bytes(state::MAGIC);
        w.u16(state::VERSION);
        w.u64(self.bus.cart.rom_hash());
        self.cpu.save(&mut w);
        self.bus.save(&mut w);
        w.buf
    }

    /// Restore a state from [`GameBoy::save_state`]. On any error the
    /// emulator is left exactly as it was.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        let mut r = Reader::new(data);
        let mut magic = [0u8; 4];
        r.bytes(&mut magic).map_err(|_| StateError::NotAState)?;
        if &magic != state::MAGIC {
            return Err(StateError::NotAState);
        }
        let version = r.u16()?;
        if version != state::VERSION {
            return Err(StateError::UnsupportedVersion(version));
        }
        if r.u64()? != self.bus.cart.rom_hash() {
            return Err(StateError::WrongGame);
        }

        let backup = self.save_state();
        let result = self
            .cpu
            .load(&mut r)
            .and_then(|()| self.bus.load(&mut r))
            .and_then(|()| r.finish());
        if result.is_err() {
            // Restoring our own snapshot cannot fail.
            let mut r = Reader::new(&backup[4 + 2 + 8..]);
            self.cpu.load(&mut r).expect("restore CPU");
            self.bus.load(&mut r).expect("restore bus");
        }
        result
    }

    /// Every byte sent out of the serial port so far.
    pub fn serial_output(&self) -> &[u8] {
        &self.bus.serial.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_is_dmg_rate() {
        let fps = CPU_HZ as f64 / CYCLES_PER_FRAME as f64;
        assert!((fps - 59.73).abs() < 0.01);
    }
}
