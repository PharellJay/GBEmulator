//! Picture processing unit: LY/STAT timing, interrupts and a scanline renderer.
//!
//! Each visible line is rendered in one go when mode 3 begins, so mid-line
//! register changes are not reflected.

use crate::interrupt;
use crate::state::{self, Reader, Snapshot, StateError, Writer};
use crate::{SCREEN_HEIGHT, SCREEN_WIDTH};

const DOTS_PER_LINE: u16 = 456;
const LINES_PER_FRAME: u8 = 154;
const OAM_SCAN_DOTS: u16 = 80;
const TRANSFER_DOTS: u16 = 172;
const MAX_SPRITES_PER_LINE: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    HBlank = 0,
    VBlank = 1,
    OamScan = 2,
    Transfer = 3,
}

pub(crate) struct Ppu {
    vram: [u8; 0x2000],
    oam: [u8; 0xA0],
    lcdc: u8,
    stat: u8,
    scy: u8,
    scx: u8,
    ly: u8,
    lyc: u8,
    bgp: u8,
    obp0: u8,
    obp1: u8,
    wy: u8,
    wx: u8,
    dot: u16,
    mode: Mode,
    stat_line: bool,
    frame_ready: bool,
    /// LY has matched WY at some point this frame.
    wy_triggered: bool,
    /// Internal window line counter; advances only on lines the window draws.
    window_line: u8,
    /// Shade per pixel, 0 (lightest) to 3 (darkest), after palette mapping.
    framebuffer: Vec<u8>,
}

impl Ppu {
    pub(crate) fn new() -> Self {
        Self {
            vram: [0; 0x2000],
            oam: [0; 0xA0],
            lcdc: 0x91,
            stat: 0,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            dot: 0,
            mode: Mode::OamScan,
            stat_line: false,
            frame_ready: false,
            wy_triggered: false,
            window_line: 0,
            framebuffer: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT],
        }
    }

    fn lcd_on(&self) -> bool {
        self.lcdc & 0x80 != 0
    }

    pub(crate) fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    /// Returns true once per frame, when VBlank begins.
    pub(crate) fn take_frame_ready(&mut self) -> bool {
        std::mem::take(&mut self.frame_ready)
    }

    /// Advance one M-cycle (4 dots).
    pub(crate) fn tick(&mut self, if_reg: &mut u8) {
        if !self.lcd_on() {
            return;
        }
        for _ in 0..4 {
            self.dot_step(if_reg);
        }
    }

    fn dot_step(&mut self, if_reg: &mut u8) {
        self.dot += 1;
        if self.dot == DOTS_PER_LINE {
            self.dot = 0;
            self.ly = (self.ly + 1) % LINES_PER_FRAME;
            if self.ly == 144 {
                self.mode = Mode::VBlank;
                *if_reg |= interrupt::VBLANK;
                self.frame_ready = true;
            } else if self.ly == 0 {
                self.mode = Mode::OamScan;
                self.wy_triggered = false;
                self.window_line = 0;
            }
        }
        if self.ly < 144 {
            if self.dot == 0 && self.ly == self.wy {
                self.wy_triggered = true;
            }
            let mode = match self.dot {
                d if d < OAM_SCAN_DOTS => Mode::OamScan,
                d if d < OAM_SCAN_DOTS + TRANSFER_DOTS => Mode::Transfer,
                _ => Mode::HBlank,
            };
            if mode == Mode::Transfer && self.mode != Mode::Transfer {
                self.render_line();
            }
            self.mode = mode;
        }
        self.update_stat_line(if_reg);
    }

    fn update_stat_line(&mut self, if_reg: &mut u8) {
        let line = (self.stat & 0x08 != 0 && self.mode == Mode::HBlank)
            || (self.stat & 0x10 != 0 && self.mode == Mode::VBlank)
            || (self.stat & 0x20 != 0 && self.mode == Mode::OamScan)
            || (self.stat & 0x40 != 0 && self.ly == self.lyc);
        if line && !self.stat_line {
            *if_reg |= interrupt::STAT;
        }
        self.stat_line = line;
    }

    // ---- rendering ------------------------------------------------------

    /// Colour index (0-3) of pixel (`x`, `y`) within a tile.
    fn tile_pixel(&self, tile_addr: usize, x: u8, y: u8) -> u8 {
        let lo = self.vram[tile_addr + y as usize * 2];
        let hi = self.vram[tile_addr + y as usize * 2 + 1];
        let bit = 7 - x;
        (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1)
    }

    /// VRAM offset of a BG/window tile, honouring the LCDC.4 addressing mode.
    fn bg_tile_addr(&self, index: u8) -> usize {
        if self.lcdc & 0x10 != 0 {
            index as usize * 16
        } else {
            (0x1000 + (index as i8 as i32) * 16) as usize
        }
    }

    fn map_pixel(&self, map_base: usize, x: u8, y: u8) -> u8 {
        let index = self.vram[map_base + (y as usize / 8) * 32 + x as usize / 8];
        self.tile_pixel(self.bg_tile_addr(index), x % 8, y % 8)
    }

    fn render_line(&mut self) {
        let ly = self.ly;
        let mut bg_index = [0u8; SCREEN_WIDTH];

        if self.lcdc & 0x01 != 0 {
            let bg_map = if self.lcdc & 0x08 != 0 {
                0x1C00
            } else {
                0x1800
            };
            let win_map = if self.lcdc & 0x40 != 0 {
                0x1C00
            } else {
                0x1800
            };
            let window_on = self.lcdc & 0x20 != 0 && self.wy_triggered && self.wx <= 166;
            let win_start = self.wx as i32 - 7;

            for (x, idx) in bg_index.iter_mut().enumerate() {
                *idx = if window_on && x as i32 >= win_start {
                    let wx = (x as i32 - win_start) as u8;
                    self.map_pixel(win_map, wx, self.window_line)
                } else {
                    let px = (x as u8).wrapping_add(self.scx);
                    let py = ly.wrapping_add(self.scy);
                    self.map_pixel(bg_map, px, py)
                };
            }
            if window_on {
                self.window_line = self.window_line.wrapping_add(1);
            }
        }

        let row = ly as usize * SCREEN_WIDTH;
        for (x, &idx) in bg_index.iter().enumerate() {
            // With LCDC.0 clear the background is blank (white).
            self.framebuffer[row + x] = if self.lcdc & 0x01 != 0 {
                shade(self.bgp, idx)
            } else {
                0
            };
        }

        if self.lcdc & 0x02 != 0 {
            self.render_sprites(&bg_index);
        }
    }

    fn render_sprites(&mut self, bg_index: &[u8; SCREEN_WIDTH]) {
        let ly = self.ly as i32;
        let height = if self.lcdc & 0x04 != 0 { 16 } else { 8 };

        // OAM scan: the first 10 sprites (in OAM order) overlapping this line.
        let mut sprites: Vec<[u8; 4]> = self
            .oam
            .chunks_exact(4)
            .filter(|s| {
                let top = s[0] as i32 - 16;
                ly >= top && ly < top + height
            })
            .take(MAX_SPRITES_PER_LINE)
            .map(|s| [s[0], s[1], s[2], s[3]])
            .collect();
        // DMG priority: lower X wins; ties go to the earlier OAM entry.
        sprites.sort_by_key(|s| s[1]);

        let row = self.ly as usize * SCREEN_WIDTH;
        for x in 0..SCREEN_WIDTH as i32 {
            for &[y, sx, tile, attr] in &sprites {
                let left = sx as i32 - 8;
                if x < left || x >= left + 8 {
                    continue;
                }
                let mut line = (ly - (y as i32 - 16)) as u8;
                if attr & 0x40 != 0 {
                    line = height as u8 - 1 - line;
                }
                let mut col = (x - left) as u8;
                if attr & 0x20 != 0 {
                    col = 7 - col;
                }
                let tile = if height == 16 { tile & 0xFE } else { tile };
                let idx = self.tile_pixel(tile as usize * 16, col, line);
                if idx == 0 {
                    continue; // transparent: lower-priority sprites may show
                }
                if attr & 0x80 == 0 || bg_index[x as usize] == 0 {
                    let palette = if attr & 0x10 != 0 {
                        self.obp1
                    } else {
                        self.obp0
                    };
                    self.framebuffer[row + x as usize] = shade(palette, idx);
                }
                break;
            }
        }
    }

    // ---- memory-mapped access ------------------------------------------

    pub(crate) fn read_vram(&self, addr: u16) -> u8 {
        self.vram[addr as usize & 0x1FFF]
    }

    pub(crate) fn write_vram(&mut self, addr: u16, value: u8) {
        self.vram[addr as usize & 0x1FFF] = value;
    }

    pub(crate) fn read_oam(&self, addr: u16) -> u8 {
        self.oam[addr as usize - 0xFE00]
    }

    pub(crate) fn write_oam(&mut self, addr: u16, value: u8) {
        self.oam[addr as usize - 0xFE00] = value;
    }

    pub(crate) fn read_reg(&self, addr: u16) -> u8 {
        match addr {
            0xFF40 => self.lcdc,
            0xFF41 => {
                let mode = if self.lcd_on() { self.mode as u8 } else { 0 };
                let coincidence = if self.ly == self.lyc { 0x04 } else { 0 };
                0x80 | self.stat | coincidence | mode
            }
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => self.ly,
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            _ => 0xFF,
        }
    }

    pub(crate) fn write_reg(&mut self, addr: u16, value: u8, if_reg: &mut u8) {
        match addr {
            0xFF40 => {
                let was_on = self.lcd_on();
                self.lcdc = value;
                if was_on && !self.lcd_on() {
                    self.ly = 0;
                    self.dot = 0;
                    self.mode = Mode::HBlank;
                    self.framebuffer.fill(0);
                } else if !was_on && self.lcd_on() {
                    self.mode = Mode::OamScan;
                    self.wy_triggered = self.wy == 0;
                    self.window_line = 0;
                }
            }
            0xFF41 => self.stat = value & 0x78,
            0xFF42 => self.scy = value,
            0xFF43 => self.scx = value,
            0xFF45 => self.lyc = value,
            0xFF47 => self.bgp = value,
            0xFF48 => self.obp0 = value,
            0xFF49 => self.obp1 = value,
            0xFF4A => self.wy = value,
            0xFF4B => self.wx = value,
            _ => {}
        }
        if self.lcd_on() {
            self.update_stat_line(if_reg);
        }
    }
}

/// Map a 2-bit colour index through a DMG palette register.
fn shade(palette: u8, index: u8) -> u8 {
    (palette >> (index * 2)) & 0x03
}

impl Snapshot for Ppu {
    fn save(&self, w: &mut Writer) {
        w.bytes(&self.vram);
        w.bytes(&self.oam);
        for v in [
            self.lcdc, self.stat, self.scy, self.scx, self.ly, self.lyc, self.bgp, self.obp0,
            self.obp1, self.wy, self.wx,
        ] {
            w.u8(v);
        }
        w.u16(self.dot);
        w.u8(self.mode as u8);
        w.bool(self.stat_line);
        w.bool(self.frame_ready);
        w.bool(self.wy_triggered);
        w.u8(self.window_line);
        w.bytes(&self.framebuffer);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        r.bytes(&mut self.vram)?;
        r.bytes(&mut self.oam)?;
        self.lcdc = r.u8()?;
        self.stat = r.u8()? & 0x78;
        self.scy = r.u8()?;
        self.scx = r.u8()?;
        self.ly = r.u8()?;
        if self.ly >= LINES_PER_FRAME {
            return Err(StateError::Invalid("LY"));
        }
        self.lyc = r.u8()?;
        self.bgp = r.u8()?;
        self.obp0 = r.u8()?;
        self.obp1 = r.u8()?;
        self.wy = r.u8()?;
        self.wx = r.u8()?;
        self.dot = r.u16()?;
        if self.dot >= DOTS_PER_LINE {
            return Err(StateError::Invalid("PPU dot"));
        }
        self.mode = match r.u8()? {
            0 => Mode::HBlank,
            1 => Mode::VBlank,
            2 => Mode::OamScan,
            3 => Mode::Transfer,
            _ => return Err(StateError::Invalid("PPU mode")),
        };
        self.stat_line = r.bool()?;
        self.frame_ready = r.bool()?;
        self.wy_triggered = r.bool()?;
        self.window_line = r.u8()?;
        r.bytes(&mut self.framebuffer)?;
        for shade in &mut self.framebuffer {
            *shade &= 0x03;
        }
        Ok(())
    }
}
