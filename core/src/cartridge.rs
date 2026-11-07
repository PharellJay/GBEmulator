//! Cartridge header parsing, memory bank controllers and battery saves.

use std::fmt;

use crate::rtc::{self, Rtc};
use crate::state::{self, Reader, Snapshot, StateError, Writer};

const ROM_BANK_SIZE: usize = 0x4000;
const RAM_BANK_SIZE: usize = 0x2000;

/// Errors raised while loading a ROM image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CartridgeError {
    /// The file is too small to contain a cartridge header.
    TooSmall(usize),
    /// The cartridge type byte (0x0147) names hardware this emulator does not support.
    UnsupportedType(u8),
    /// The RAM size byte (0x0149) holds an invalid value.
    InvalidRamSize(u8),
}

impl fmt::Display for CartridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall(len) => {
                write!(
                    f,
                    "ROM is {len} bytes; too small to contain a cartridge header"
                )
            }
            Self::UnsupportedType(t) => write!(
                f,
                "unsupported cartridge type 0x{t:02X} (supported: ROM only, MBC1, MBC3, MBC5)"
            ),
            Self::InvalidRamSize(s) => write!(f, "invalid RAM size code 0x{s:02X}"),
        }
    }
}

impl std::error::Error for CartridgeError {}

/// A save file whose size does not fit this cartridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSizeError {
    /// Sizes this cartridge accepts.
    pub expected: Vec<usize>,
    /// Size of the data provided.
    pub actual: usize,
}

impl fmt::Display for SaveSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "save data is {} bytes; this cartridge expects {:?}",
            self.actual, self.expected
        )
    }
}

impl std::error::Error for SaveSizeError {}

enum Mbc {
    None,
    Mbc1 {
        ram_enabled: bool,
        /// Lower 5 bits of the ROM bank number (0 is treated as 1).
        rom_bank: u8,
        /// 2-bit secondary bank register (upper ROM bits or RAM bank).
        bank2: u8,
        /// Banking mode select: false = simple, true = advanced.
        advanced: bool,
    },
    Mbc3 {
        ram_enabled: bool,
        rom_bank: u8,
        /// 0x00-0x03 selects a RAM bank, 0x08-0x0C an RTC register.
        select: u8,
        rtc: Option<Rtc>,
        /// Last value written to the latch register; 0 then 1 latches.
        latch_last: u8,
    },
    Mbc5 {
        ram_enabled: bool,
        rom_bank: u16,
        ram_bank: u8,
        /// Rumble carts use RAM bank bit 3 for the motor.
        rumble: bool,
    },
}

pub(crate) struct Cartridge {
    rom: Vec<u8>,
    ram: Vec<u8>,
    mbc: Mbc,
    title: String,
    battery: bool,
    /// Battery-backed state changed since the last `take_save_dirty`.
    dirty: bool,
    /// Identifies the ROM so save states cannot cross games.
    rom_hash: u64,
}

impl Cartridge {
    pub(crate) fn from_rom(rom: Vec<u8>) -> Result<Self, CartridgeError> {
        if rom.len() < 0x150 {
            return Err(CartridgeError::TooSmall(rom.len()));
        }

        let title = rom[0x134..0x144]
            .iter()
            .take_while(|&&b| b != 0)
            .filter(|b| b.is_ascii_graphic() || **b == b' ')
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_string();

        let cart_type = rom[0x147];
        let ram_size = match rom[0x149] {
            0 => 0,
            1 => 0x800,
            2 => RAM_BANK_SIZE,
            3 => 4 * RAM_BANK_SIZE,
            4 => 16 * RAM_BANK_SIZE,
            5 => 8 * RAM_BANK_SIZE,
            code => return Err(CartridgeError::InvalidRamSize(code)),
        };

        let mbc = match cart_type {
            0x00 | 0x08 | 0x09 => Mbc::None,
            0x01..=0x03 => Mbc::Mbc1 {
                ram_enabled: false,
                rom_bank: 1,
                bank2: 0,
                advanced: false,
            },
            0x0F..=0x13 => Mbc::Mbc3 {
                ram_enabled: false,
                rom_bank: 1,
                select: 0,
                rtc: matches!(cart_type, 0x0F | 0x10).then(Rtc::new),
                latch_last: 0xFF,
            },
            0x19..=0x1E => Mbc::Mbc5 {
                ram_enabled: false,
                rom_bank: 1,
                ram_bank: 0,
                rumble: cart_type >= 0x1C,
            },
            t => return Err(CartridgeError::UnsupportedType(t)),
        };
        let battery = matches!(cart_type, 0x03 | 0x09 | 0x0F | 0x10 | 0x13 | 0x1B | 0x1E);

        Ok(Self {
            rom_hash: state::rom_hash(&rom),
            rom,
            ram: vec![0; ram_size],
            mbc,
            title,
            battery,
            dirty: false,
        })
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn rom_hash(&self) -> u64 {
        self.rom_hash
    }

    pub(crate) fn has_battery(&self) -> bool {
        self.battery
    }

    fn rtc(&self) -> Option<&Rtc> {
        match &self.mbc {
            Mbc::Mbc3 { rtc, .. } => rtc.as_ref(),
            _ => None,
        }
    }

    /// Advance cartridge hardware (the MBC3 clock) by one M-cycle.
    pub(crate) fn tick(&mut self) {
        if let Mbc::Mbc3 { rtc: Some(rtc), .. } = &mut self.mbc {
            rtc.tick();
        }
    }

    pub(crate) fn take_save_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Battery-backed state: cartridge RAM, then an RTC block if present.
    pub(crate) fn save_data(&self, unix_now: u64) -> Vec<u8> {
        let mut out = self.ram.clone();
        if let Some(rtc) = self.rtc() {
            out.extend_from_slice(&rtc.to_save(unix_now));
        }
        out
    }

    pub(crate) fn load_save_data(
        &mut self,
        data: &[u8],
        unix_now: u64,
    ) -> Result<(), SaveSizeError> {
        let ram_len = self.ram.len();
        let has_rtc = self.rtc().is_some();
        let expected = if has_rtc {
            vec![
                ram_len + rtc::SAVE_LEN,
                ram_len + rtc::SAVE_LEN - 4,
                ram_len,
            ]
        } else {
            vec![ram_len]
        };
        if !expected.contains(&data.len()) {
            return Err(SaveSizeError {
                expected,
                actual: data.len(),
            });
        }
        self.ram.copy_from_slice(&data[..ram_len]);
        if let Mbc::Mbc3 { rtc: Some(rtc), .. } = &mut self.mbc
            && data.len() > ram_len
        {
            rtc.load_save(&data[ram_len..], unix_now);
        }
        Ok(())
    }

    fn rom_bank_count(&self) -> usize {
        self.rom.len().div_ceil(ROM_BANK_SIZE).max(2)
    }

    fn rom_at(&self, bank: usize, addr: u16) -> u8 {
        let bank = bank % self.rom_bank_count();
        let idx = bank * ROM_BANK_SIZE + (addr as usize & (ROM_BANK_SIZE - 1));
        self.rom.get(idx).copied().unwrap_or(0xFF)
    }

    pub(crate) fn read_rom(&self, addr: u16) -> u8 {
        let high = addr >= 0x4000;
        let bank = match self.mbc {
            Mbc::None => return self.rom.get(addr as usize).copied().unwrap_or(0xFF),
            Mbc::Mbc1 {
                rom_bank,
                bank2,
                advanced,
                ..
            } => match (high, advanced) {
                (true, _) => ((bank2 as usize) << 5) | rom_bank as usize,
                (false, true) => (bank2 as usize) << 5,
                (false, false) => 0,
            },
            Mbc::Mbc3 { rom_bank, .. } if high => rom_bank as usize,
            Mbc::Mbc5 { rom_bank, .. } if high => rom_bank as usize,
            _ => 0,
        };
        self.rom_at(bank, addr)
    }

    pub(crate) fn write_rom(&mut self, addr: u16, value: u8) {
        match &mut self.mbc {
            Mbc::None => {}
            Mbc::Mbc1 {
                ram_enabled,
                rom_bank,
                bank2,
                advanced,
            } => match addr {
                0x0000..=0x1FFF => *ram_enabled = value & 0x0F == 0x0A,
                0x2000..=0x3FFF => *rom_bank = (value & 0x1F).max(1),
                0x4000..=0x5FFF => *bank2 = value & 0x03,
                _ => *advanced = value & 0x01 != 0,
            },
            Mbc::Mbc3 {
                ram_enabled,
                rom_bank,
                select,
                rtc,
                latch_last,
            } => match addr {
                0x0000..=0x1FFF => *ram_enabled = value & 0x0F == 0x0A,
                0x2000..=0x3FFF => *rom_bank = (value & 0x7F).max(1),
                0x4000..=0x5FFF => *select = value,
                _ => {
                    if *latch_last == 0
                        && value == 1
                        && let Some(rtc) = rtc
                    {
                        rtc.latch();
                    }
                    *latch_last = value;
                }
            },
            Mbc::Mbc5 {
                ram_enabled,
                rom_bank,
                ram_bank,
                rumble,
            } => match addr {
                0x0000..=0x1FFF => *ram_enabled = value == 0x0A,
                0x2000..=0x2FFF => *rom_bank = (*rom_bank & 0x100) | value as u16,
                0x3000..=0x3FFF => *rom_bank = (*rom_bank & 0xFF) | ((value as u16 & 1) << 8),
                0x4000..=0x5FFF => *ram_bank = value & if *rumble { 0x07 } else { 0x0F },
                _ => {}
            },
        }
    }

    /// Offset into `ram` for an 0xA000-0xBFFF access, or None if unmapped.
    fn ram_index(&self, addr: u16) -> Option<usize> {
        let offset = addr as usize - 0xA000;
        let bank = match self.mbc {
            Mbc::None => 0,
            Mbc::Mbc1 {
                ram_enabled: true,
                bank2,
                advanced,
                ..
            } => {
                if advanced {
                    bank2 as usize
                } else {
                    0
                }
            }
            Mbc::Mbc3 {
                ram_enabled: true,
                select: select @ 0x00..=0x03,
                ..
            } => select as usize,
            Mbc::Mbc5 {
                ram_enabled: true,
                ram_bank,
                ..
            } => ram_bank as usize,
            _ => return None,
        };
        if self.ram.is_empty() {
            return None;
        }
        Some((bank * RAM_BANK_SIZE + offset) % self.ram.len())
    }

    /// The selected RTC register, if RAM access currently maps to the clock.
    fn rtc_select(&self) -> Option<u8> {
        match self.mbc {
            Mbc::Mbc3 {
                ram_enabled: true,
                select: select @ 0x08..=0x0C,
                rtc: Some(_),
                ..
            } => Some(select),
            _ => None,
        }
    }

    pub(crate) fn read_ram(&self, addr: u16) -> u8 {
        if let (Some(select), Some(rtc)) = (self.rtc_select(), self.rtc()) {
            return rtc.read(select);
        }
        self.ram_index(addr).map_or(0xFF, |i| self.ram[i])
    }

    pub(crate) fn write_ram(&mut self, addr: u16, value: u8) {
        if let Some(select) = self.rtc_select() {
            if let Mbc::Mbc3 { rtc: Some(rtc), .. } = &mut self.mbc {
                rtc.write(select, value);
                self.dirty |= self.battery;
            }
            return;
        }
        if let Some(i) = self.ram_index(addr)
            && self.ram[i] != value
        {
            self.ram[i] = value;
            self.dirty |= self.battery;
        }
    }
}

/// The ROM itself is not saved; the state's ROM hash guarantees it matches.
impl Snapshot for Cartridge {
    fn save(&self, w: &mut Writer) {
        w.block(&self.ram);
        match &self.mbc {
            Mbc::None => w.u8(0),
            Mbc::Mbc1 {
                ram_enabled,
                rom_bank,
                bank2,
                advanced,
            } => {
                w.u8(1);
                w.bool(*ram_enabled);
                w.u8(*rom_bank);
                w.u8(*bank2);
                w.bool(*advanced);
            }
            Mbc::Mbc3 {
                ram_enabled,
                rom_bank,
                select,
                rtc,
                latch_last,
            } => {
                w.u8(3);
                w.bool(*ram_enabled);
                w.u8(*rom_bank);
                w.u8(*select);
                w.u8(*latch_last);
                if let Some(rtc) = rtc {
                    rtc.save(w);
                }
            }
            Mbc::Mbc5 {
                ram_enabled,
                rom_bank,
                ram_bank,
                ..
            } => {
                w.u8(5);
                w.bool(*ram_enabled);
                w.u16(*rom_bank);
                w.u8(*ram_bank);
            }
        }
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        r.block(&mut self.ram, "cartridge RAM size")?;
        let tag = r.u8()?;
        match &mut self.mbc {
            Mbc::None if tag == 0 => {}
            Mbc::Mbc1 {
                ram_enabled,
                rom_bank,
                bank2,
                advanced,
            } if tag == 1 => {
                *ram_enabled = r.bool()?;
                *rom_bank = (r.u8()? & 0x1F).max(1);
                *bank2 = r.u8()? & 0x03;
                *advanced = r.bool()?;
            }
            Mbc::Mbc3 {
                ram_enabled,
                rom_bank,
                select,
                rtc,
                latch_last,
            } if tag == 3 => {
                *ram_enabled = r.bool()?;
                *rom_bank = (r.u8()? & 0x7F).max(1);
                *select = r.u8()?;
                *latch_last = r.u8()?;
                if let Some(rtc) = rtc {
                    rtc.load(r)?;
                }
            }
            Mbc::Mbc5 {
                ram_enabled,
                rom_bank,
                ram_bank,
                rumble,
            } if tag == 5 => {
                *ram_enabled = r.bool()?;
                *rom_bank = r.u16()? & 0x1FF;
                *ram_bank = r.u8()? & if *rumble { 0x07 } else { 0x0F };
            }
            _ => return Err(StateError::Invalid("memory bank controller")),
        }
        // Battery RAM now differs from the .sav on disk.
        self.dirty |= self.battery;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROM whose first byte in each bank is the low byte of its bank number.
    fn rom(cart_type: u8, banks: usize, ram_code: u8) -> Vec<u8> {
        let mut rom = vec![0u8; banks * ROM_BANK_SIZE];
        rom[0x147] = cart_type;
        rom[0x149] = ram_code;
        for bank in 0..banks {
            rom[bank * ROM_BANK_SIZE] = bank as u8;
            rom[bank * ROM_BANK_SIZE + 1] = (bank >> 8) as u8;
        }
        rom
    }

    fn bank_at_4000(cart: &Cartridge) -> usize {
        cart.read_rom(0x4000) as usize | (cart.read_rom(0x4001) as usize) << 8
    }

    #[test]
    fn rejects_tiny_rom() {
        assert!(matches!(
            Cartridge::from_rom(vec![0; 16]),
            Err(CartridgeError::TooSmall(16))
        ));
    }

    #[test]
    fn rejects_unsupported_type() {
        assert!(matches!(
            Cartridge::from_rom(rom(0xFC, 2, 0)),
            Err(CartridgeError::UnsupportedType(0xFC))
        ));
    }

    #[test]
    fn mbc1_bank_zero_maps_to_one() {
        let mut cart = Cartridge::from_rom(rom(0x01, 8, 0)).unwrap();
        cart.write_rom(0x2000, 0);
        assert_eq!(bank_at_4000(&cart), 1);
        cart.write_rom(0x2000, 5);
        assert_eq!(bank_at_4000(&cart), 5);
    }

    #[test]
    fn mbc1_upper_bits_select_high_banks() {
        let mut cart = Cartridge::from_rom(rom(0x01, 64, 0)).unwrap();
        cart.write_rom(0x2000, 0x02);
        cart.write_rom(0x4000, 0x01);
        assert_eq!(bank_at_4000(&cart), 0x22);
        assert_eq!(cart.read_rom(0x0000), 0);
        cart.write_rom(0x6000, 1);
        assert_eq!(cart.read_rom(0x0000), 0x20);
    }

    #[test]
    fn mbc3_banks_rom_and_ram() {
        let mut cart = Cartridge::from_rom(rom(0x13, 128, 3)).unwrap();
        cart.write_rom(0x2000, 0x7F);
        assert_eq!(bank_at_4000(&cart), 0x7F);
        cart.write_rom(0x2000, 0);
        assert_eq!(bank_at_4000(&cart), 1);

        cart.write_rom(0x0000, 0x0A);
        for bank in 0..4u8 {
            cart.write_rom(0x4000, bank);
            cart.write_ram(0xA000, 0x10 + bank);
        }
        cart.write_rom(0x4000, 2);
        assert_eq!(cart.read_ram(0xA000), 0x12);
    }

    #[test]
    fn mbc3_rtc_latches_on_zero_then_one() {
        let mut cart = Cartridge::from_rom(rom(0x10, 4, 3)).unwrap();
        cart.write_rom(0x0000, 0x0A);
        cart.write_rom(0x4000, 0x08);
        cart.write_ram(0xA000, 5); // seconds = 5
        for _ in 0..crate::CPU_HZ / 4 {
            cart.tick();
        }
        assert_eq!(cart.read_ram(0xA000), 5, "not latched yet");
        cart.write_rom(0x6000, 0);
        cart.write_rom(0x6000, 1);
        assert_eq!(cart.read_ram(0xA000), 6);
    }

    #[test]
    fn mbc5_uses_nine_bit_rom_bank_and_allows_bank_zero() {
        let mut cart = Cartridge::from_rom(rom(0x19, 512, 0)).unwrap();
        cart.write_rom(0x2000, 0x34);
        cart.write_rom(0x3000, 0x01);
        assert_eq!(bank_at_4000(&cart), 0x134);
        cart.write_rom(0x2000, 0);
        cart.write_rom(0x3000, 0);
        assert_eq!(bank_at_4000(&cart), 0);
    }

    #[test]
    fn ram_disabled_reads_open_bus() {
        let mut cart = Cartridge::from_rom(rom(0x1B, 4, 3)).unwrap();
        cart.write_rom(0x0000, 0x0A);
        cart.write_ram(0xA000, 0x55);
        cart.write_rom(0x0000, 0x00);
        assert_eq!(cart.read_ram(0xA000), 0xFF);
    }

    #[test]
    fn only_battery_carts_mark_saves_dirty() {
        let mut plain = Cartridge::from_rom(rom(0x02, 4, 2)).unwrap();
        plain.write_rom(0x0000, 0x0A);
        plain.write_ram(0xA000, 1);
        assert!(!plain.take_save_dirty());

        let mut battery = Cartridge::from_rom(rom(0x03, 4, 2)).unwrap();
        battery.write_rom(0x0000, 0x0A);
        battery.write_ram(0xA000, 1);
        assert!(battery.take_save_dirty());
        assert!(!battery.take_save_dirty(), "flag clears once taken");
    }

    #[test]
    fn save_data_round_trip_with_rtc() {
        let mut cart = Cartridge::from_rom(rom(0x10, 4, 3)).unwrap();
        cart.write_rom(0x0000, 0x0A);
        cart.write_ram(0xA123, 0x42);
        cart.write_rom(0x4000, 0x0A); // hours
        cart.write_ram(0xA000, 7);
        let data = cart.save_data(500);
        assert_eq!(data.len(), 0x8000 + rtc::SAVE_LEN);

        let mut restored = Cartridge::from_rom(rom(0x10, 4, 3)).unwrap();
        restored.load_save_data(&data, 500).unwrap();
        restored.write_rom(0x0000, 0x0A);
        assert_eq!(restored.read_ram(0xA123), 0x42);
        restored.write_rom(0x6000, 0);
        restored.write_rom(0x6000, 1);
        restored.write_rom(0x4000, 0x0A);
        assert_eq!(restored.read_ram(0xA000), 7);
    }

    #[test]
    fn wrong_size_save_is_rejected_untouched() {
        let mut cart = Cartridge::from_rom(rom(0x03, 4, 2)).unwrap();
        let err = cart.load_save_data(&[1, 2, 3], 0).unwrap_err();
        assert_eq!(err.actual, 3);
        assert_eq!(cart.save_data(0), vec![0; RAM_BANK_SIZE]);
    }
}
