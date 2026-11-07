//! Audio processing unit: two square channels (one with sweep), a wave
//! channel and a noise channel, mixed to stereo and resampled to the
//! frontend's output rate.

use crate::CPU_HZ;
use crate::state::{self, Reader, Snapshot, Writer};

/// Bits that always read back as 1, for 0xFF10-0xFF2F.
const READ_MASK: [u8; 0x20] = [
    0x80, 0x3F, 0x00, 0xFF, 0xBF, // NR10-NR14
    0xFF, 0x3F, 0x00, 0xFF, 0xBF, // (unused), NR21-NR24
    0x7F, 0xFF, 0x9F, 0xFF, 0xBF, // NR30-NR34
    0xFF, 0xFF, 0x00, 0x00, 0xBF, // (unused), NR41-NR44
    0x00, 0x00, 0x70, // NR50-NR52
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // unused
];

const DUTY_PATTERNS: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

const NOISE_DIVISORS: [u32; 8] = [8, 16, 32, 48, 64, 80, 96, 112];

/// Keep at most this many seconds of unread samples.
const MAX_BUFFER_SECONDS: usize = 1;

#[derive(Default, Clone, Copy)]
struct Length {
    counter: u16,
    enabled: bool,
}

impl Length {
    /// Returns true when the counter just expired.
    fn clock(&mut self) -> bool {
        if self.enabled && self.counter > 0 {
            self.counter -= 1;
            return self.counter == 0;
        }
        false
    }

    /// NRx4 write: length enable plus the extra-clock and reload quirks.
    /// `next_step_skips_length` is true when the next frame-sequencer step
    /// does not clock length. Returns false if the channel must turn off.
    fn write_control(
        &mut self,
        enable: bool,
        trigger: bool,
        next_step_skips_length: bool,
        max: u16,
    ) -> bool {
        let mut keep_on = true;
        if next_step_skips_length && !self.enabled && enable && self.counter > 0 {
            self.counter -= 1;
            if self.counter == 0 && !trigger {
                keep_on = false;
            }
        }
        self.enabled = enable;
        if trigger && self.counter == 0 {
            self.counter = if enable && next_step_skips_length {
                max - 1
            } else {
                max
            };
        }
        keep_on
    }
}

#[derive(Default, Clone, Copy)]
struct Envelope {
    initial: u8,
    up: bool,
    period: u8,
    volume: u8,
    timer: u8,
}

impl Envelope {
    fn write(&mut self, value: u8) {
        self.initial = value >> 4;
        self.up = value & 0x08 != 0;
        self.period = value & 0x07;
    }

    fn trigger(&mut self) {
        self.volume = self.initial;
        self.timer = if self.period == 0 { 8 } else { self.period };
    }

    fn clock(&mut self) {
        if self.period == 0 {
            return;
        }
        self.timer = self.timer.saturating_sub(1);
        if self.timer == 0 {
            self.timer = self.period;
            if self.up && self.volume < 15 {
                self.volume += 1;
            } else if !self.up && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Square {
    on: bool,
    dac: bool,
    length: Length,
    envelope: Envelope,
    duty: u8,
    freq: u16,
    timer: i32,
    position: u8,
}

impl Square {
    fn period(&self) -> i32 {
        (2048 - self.freq as i32) * 4
    }

    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += self.period();
            self.position = (self.position + 1) & 7;
        }
    }

    fn trigger(&mut self) {
        self.on = self.dac;
        self.timer = self.period();
        self.envelope.trigger();
    }

    fn output(&self) -> u8 {
        if !self.on {
            return 0;
        }
        DUTY_PATTERNS[self.duty as usize][self.position as usize] * self.envelope.volume
    }
}

#[derive(Default, Clone, Copy)]
struct Sweep {
    period: u8,
    negate: bool,
    shift: u8,
    timer: u8,
    shadow: u16,
    enabled: bool,
    /// A subtraction happened since the last trigger; clearing negate now
    /// disables the channel.
    negate_used: bool,
}

#[derive(Default, Clone, Copy)]
struct Wave {
    on: bool,
    dac: bool,
    length: Length,
    volume_code: u8,
    freq: u16,
    timer: i32,
    position: u8,
    sample: u8,
}

impl Wave {
    fn period(&self) -> i32 {
        (2048 - self.freq as i32) * 2
    }

    fn output(&self) -> u8 {
        if !self.on || self.volume_code == 0 {
            return 0;
        }
        self.sample >> (self.volume_code - 1)
    }
}

#[derive(Default, Clone, Copy)]
struct Noise {
    on: bool,
    dac: bool,
    length: Length,
    envelope: Envelope,
    shift: u8,
    width7: bool,
    divisor: u8,
    timer: i32,
    lfsr: u16,
}

impl Noise {
    fn period(&self) -> i32 {
        (NOISE_DIVISORS[self.divisor as usize] << self.shift) as i32
    }

    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += self.period();
            // Shift values 14 and 15 stop the LFSR.
            if self.shift < 14 {
                let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
                self.lfsr = (self.lfsr >> 1) | (bit << 14);
                if self.width7 {
                    self.lfsr = (self.lfsr & !0x40) | (bit << 6);
                }
            }
        }
    }

    fn output(&self) -> u8 {
        if !self.on || self.lfsr & 1 != 0 {
            return 0;
        }
        self.envelope.volume
    }
}

pub(crate) struct Apu {
    powered: bool,
    /// Raw register values for 0xFF10-0xFF2F (read back through READ_MASK).
    regs: [u8; 0x20],
    wave_ram: [u8; 16],
    /// Next frame-sequencer step, 0-7.
    frame_step: u8,
    ch1: Square,
    sweep: Sweep,
    ch2: Square,
    ch3: Wave,
    ch4: Noise,

    sample_rate: u32,
    /// Fractional position toward the next output sample.
    sample_phase: u64,
    sum_left: f32,
    sum_right: f32,
    sum_count: u32,
    /// High-pass filter capacitor state, removing DC offset like the real hardware.
    cap_left: f32,
    cap_right: f32,
    cap_charge: f32,
    samples: Vec<f32>,
}

impl Apu {
    pub(crate) fn new() -> Self {
        let mut apu = Self {
            powered: false,
            regs: [0; 0x20],
            wave_ram: [0; 16],
            frame_step: 0,
            ch1: Square::default(),
            sweep: Sweep::default(),
            ch2: Square::default(),
            ch3: Wave::default(),
            ch4: Noise::default(),
            sample_rate: 0,
            sample_phase: 0,
            sum_left: 0.0,
            sum_right: 0.0,
            sum_count: 0,
            cap_left: 0.0,
            cap_right: 0.0,
            cap_charge: 0.0,
            samples: Vec::new(),
        };
        // Post-boot-ROM state: powered, channel 1 left on by the boot chime.
        apu.write(0xFF26, 0x80);
        apu.write(0xFF11, 0x80);
        apu.write(0xFF12, 0xF3);
        apu.write(0xFF25, 0xF3);
        apu.write(0xFF24, 0x77);
        apu
    }

    /// Output sample rate in Hz; 0 stops sample generation.
    pub(crate) fn set_sample_rate(&mut self, rate: u32) {
        self.sample_rate = rate;
        if rate > 0 {
            self.cap_charge = 0.999958_f32.powf(CPU_HZ as f32 / rate as f32);
        }
    }

    /// Move every generated stereo sample (interleaved L, R) into `out`.
    pub(crate) fn drain_samples(&mut self, out: &mut Vec<f32>) {
        out.append(&mut self.samples);
    }

    // ---- timing ---------------------------------------------------------

    /// Advance one M-cycle (4 T-cycles).
    pub(crate) fn tick(&mut self) {
        if self.powered {
            self.ch1.step(4);
            self.ch2.step(4);
            self.step_wave(4);
            self.ch4.step(4);
        }
        self.mix();
    }

    fn step_wave(&mut self, cycles: i32) {
        let ch = &mut self.ch3;
        ch.timer -= cycles;
        while ch.timer <= 0 {
            ch.timer += ch.period();
            ch.position = (ch.position + 1) & 31;
            let byte = self.wave_ram[ch.position as usize / 2];
            ch.sample = if ch.position & 1 == 0 {
                byte >> 4
            } else {
                byte & 0x0F
            };
        }
    }

    /// Clocked at 512 Hz by the divider (falling edge of DIV bit 4).
    pub(crate) fn clock_frame_sequencer(&mut self) {
        if !self.powered {
            return;
        }
        let step = self.frame_step;
        self.frame_step = (step + 1) & 7;
        if step & 1 == 0 {
            if self.ch1.length.clock() {
                self.ch1.on = false;
            }
            if self.ch2.length.clock() {
                self.ch2.on = false;
            }
            if self.ch3.length.clock() {
                self.ch3.on = false;
            }
            if self.ch4.length.clock() {
                self.ch4.on = false;
            }
        }
        if step == 2 || step == 6 {
            self.clock_sweep();
        }
        if step == 7 {
            self.ch1.envelope.clock();
            self.ch2.envelope.clock();
            self.ch4.envelope.clock();
        }
    }

    fn next_step_skips_length(&self) -> bool {
        self.frame_step & 1 == 1
    }

    // ---- sweep ----------------------------------------------------------

    fn sweep_calculate(&mut self) -> u16 {
        let delta = self.sweep.shadow >> self.sweep.shift;
        let value = if self.sweep.negate {
            self.sweep.negate_used = true;
            self.sweep.shadow.wrapping_sub(delta)
        } else {
            self.sweep.shadow + delta
        };
        if value > 2047 {
            self.ch1.on = false;
        }
        value
    }

    fn clock_sweep(&mut self) {
        self.sweep.timer = self.sweep.timer.saturating_sub(1);
        if self.sweep.timer != 0 {
            return;
        }
        self.sweep.timer = if self.sweep.period == 0 {
            8
        } else {
            self.sweep.period
        };
        if self.sweep.enabled && self.sweep.period != 0 {
            let value = self.sweep_calculate();
            if value <= 2047 && self.sweep.shift != 0 {
                self.sweep.shadow = value;
                self.ch1.freq = value;
                self.sweep_calculate();
            }
        }
    }

    fn trigger_sweep(&mut self) {
        self.sweep.shadow = self.ch1.freq;
        self.sweep.timer = if self.sweep.period == 0 {
            8
        } else {
            self.sweep.period
        };
        self.sweep.enabled = self.sweep.period != 0 || self.sweep.shift != 0;
        self.sweep.negate_used = false;
        if self.sweep.shift != 0 {
            self.sweep_calculate();
        }
    }

    // ---- mixing ---------------------------------------------------------

    fn mix(&mut self) {
        if self.sample_rate == 0 {
            return;
        }
        let (mut left, mut right) = (0.0, 0.0);
        if self.powered {
            let channels = [
                (self.ch1.dac, self.ch1.output()),
                (self.ch2.dac, self.ch2.output()),
                (self.ch3.dac, self.ch3.output()),
                (self.ch4.dac, self.ch4.output()),
            ];
            let panning = self.regs[0x15];
            for (i, (dac, digital)) in channels.into_iter().enumerate() {
                if !dac {
                    continue;
                }
                let analog = digital as f32 / 7.5 - 1.0;
                if panning & (0x10 << i) != 0 {
                    left += analog;
                }
                if panning & (0x01 << i) != 0 {
                    right += analog;
                }
            }
            let volume = self.regs[0x14];
            left *= (((volume >> 4) & 7) + 1) as f32 / 8.0 / 4.0;
            right *= ((volume & 7) + 1) as f32 / 8.0 / 4.0;
        }
        self.sum_left += left;
        self.sum_right += right;
        self.sum_count += 1;

        self.sample_phase += self.sample_rate as u64 * 4;
        if self.sample_phase >= CPU_HZ as u64 {
            self.sample_phase -= CPU_HZ as u64;
            let n = self.sum_count as f32;
            let (l, r) = (self.sum_left / n, self.sum_right / n);
            self.sum_left = 0.0;
            self.sum_right = 0.0;
            self.sum_count = 0;

            let out_l = l - self.cap_left;
            self.cap_left = l - out_l * self.cap_charge;
            let out_r = r - self.cap_right;
            self.cap_right = r - out_r * self.cap_charge;

            let max = self.sample_rate as usize * 2 * MAX_BUFFER_SECONDS;
            if self.samples.len() >= max {
                self.samples.drain(..max / 2);
            }
            self.samples.push(out_l);
            self.samples.push(out_r);
        }
    }

    // ---- registers ------------------------------------------------------

    pub(crate) fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF30..=0xFF3F => self.wave_ram[addr as usize - 0xFF30],
            0xFF26 => {
                0x70 | (self.powered as u8) << 7
                    | (self.ch4.on as u8) << 3
                    | (self.ch3.on as u8) << 2
                    | (self.ch2.on as u8) << 1
                    | self.ch1.on as u8
            }
            0xFF10..=0xFF2F => {
                let i = addr as usize - 0xFF10;
                self.regs[i] | READ_MASK[i]
            }
            _ => 0xFF,
        }
    }

    fn power_off(&mut self) {
        // DMG: length counters survive power-off.
        let lengths = [
            self.ch1.length.counter,
            self.ch2.length.counter,
            self.ch3.length.counter,
            self.ch4.length.counter,
        ];
        self.regs = [0; 0x20];
        self.ch1 = Square::default();
        self.sweep = Sweep::default();
        self.ch2 = Square::default();
        self.ch3 = Wave::default();
        self.ch4 = Noise::default();
        self.ch1.length.counter = lengths[0];
        self.ch2.length.counter = lengths[1];
        self.ch3.length.counter = lengths[2];
        self.ch4.length.counter = lengths[3];
        self.powered = false;
    }

    pub(crate) fn write(&mut self, addr: u16, value: u8) {
        if let 0xFF30..=0xFF3F = addr {
            self.wave_ram[addr as usize - 0xFF30] = value;
            return;
        }
        if addr == 0xFF26 {
            let on = value & 0x80 != 0;
            if self.powered && !on {
                self.power_off();
            } else if !self.powered && on {
                self.powered = true;
                self.frame_step = 0;
            }
            return;
        }
        if !(0xFF10..=0xFF25).contains(&addr) {
            return;
        }
        if !self.powered {
            // DMG: only length counters can be written while powered off.
            match addr {
                0xFF11 => self.ch1.length.counter = 64 - (value & 0x3F) as u16,
                0xFF16 => self.ch2.length.counter = 64 - (value & 0x3F) as u16,
                0xFF1B => self.ch3.length.counter = 256 - value as u16,
                0xFF20 => self.ch4.length.counter = 64 - (value & 0x3F) as u16,
                _ => {}
            }
            return;
        }
        self.regs[addr as usize - 0xFF10] = value;
        let skips = self.next_step_skips_length();
        let trigger = value & 0x80 != 0;
        let length_enable = value & 0x40 != 0;

        match addr {
            // Channel 1
            0xFF10 => {
                let negate = value & 0x08 != 0;
                if self.sweep.negate && !negate && self.sweep.negate_used {
                    self.ch1.on = false;
                }
                self.sweep.period = (value >> 4) & 7;
                self.sweep.negate = negate;
                self.sweep.shift = value & 7;
            }
            0xFF11 => {
                self.ch1.duty = value >> 6;
                self.ch1.length.counter = 64 - (value & 0x3F) as u16;
            }
            0xFF12 => {
                self.ch1.envelope.write(value);
                self.ch1.dac = value & 0xF8 != 0;
                self.ch1.on &= self.ch1.dac;
            }
            0xFF13 => self.ch1.freq = (self.ch1.freq & 0x700) | value as u16,
            0xFF14 => {
                self.ch1.freq = (self.ch1.freq & 0xFF) | ((value as u16 & 7) << 8);
                if !self
                    .ch1
                    .length
                    .write_control(length_enable, trigger, skips, 64)
                {
                    self.ch1.on = false;
                }
                if trigger {
                    self.ch1.trigger();
                    self.trigger_sweep();
                }
            }
            // Channel 2
            0xFF16 => {
                self.ch2.duty = value >> 6;
                self.ch2.length.counter = 64 - (value & 0x3F) as u16;
            }
            0xFF17 => {
                self.ch2.envelope.write(value);
                self.ch2.dac = value & 0xF8 != 0;
                self.ch2.on &= self.ch2.dac;
            }
            0xFF18 => self.ch2.freq = (self.ch2.freq & 0x700) | value as u16,
            0xFF19 => {
                self.ch2.freq = (self.ch2.freq & 0xFF) | ((value as u16 & 7) << 8);
                if !self
                    .ch2
                    .length
                    .write_control(length_enable, trigger, skips, 64)
                {
                    self.ch2.on = false;
                }
                if trigger {
                    self.ch2.trigger();
                }
            }
            // Channel 3
            0xFF1A => {
                self.ch3.dac = value & 0x80 != 0;
                self.ch3.on &= self.ch3.dac;
            }
            0xFF1B => self.ch3.length.counter = 256 - value as u16,
            0xFF1C => self.ch3.volume_code = (value >> 5) & 3,
            0xFF1D => self.ch3.freq = (self.ch3.freq & 0x700) | value as u16,
            0xFF1E => {
                self.ch3.freq = (self.ch3.freq & 0xFF) | ((value as u16 & 7) << 8);
                if !self
                    .ch3
                    .length
                    .write_control(length_enable, trigger, skips, 256)
                {
                    self.ch3.on = false;
                }
                if trigger {
                    self.ch3.on = self.ch3.dac;
                    self.ch3.timer = self.ch3.period();
                    self.ch3.position = 0;
                }
            }
            // Channel 4
            0xFF20 => self.ch4.length.counter = 64 - (value & 0x3F) as u16,
            0xFF21 => {
                self.ch4.envelope.write(value);
                self.ch4.dac = value & 0xF8 != 0;
                self.ch4.on &= self.ch4.dac;
            }
            0xFF22 => {
                self.ch4.shift = value >> 4;
                self.ch4.width7 = value & 0x08 != 0;
                self.ch4.divisor = value & 0x07;
            }
            0xFF23 => {
                if !self
                    .ch4
                    .length
                    .write_control(length_enable, trigger, skips, 64)
                {
                    self.ch4.on = false;
                }
                if trigger {
                    self.ch4.on = self.ch4.dac;
                    self.ch4.timer = self.ch4.period();
                    self.ch4.lfsr = 0x7FFF;
                    self.ch4.envelope.trigger();
                }
            }
            _ => {} // NR50/NR51 are read directly from `regs` when mixing.
        }
    }
}

// ---- save states --------------------------------------------------------
// Frequencies are masked to 11 bits and table indices to their table size on
// load, so a corrupt state can never produce a zero timer period (which
// would hang) or an out-of-bounds lookup.

impl Length {
    fn save(&self, w: &mut Writer) {
        w.u16(self.counter);
        w.bool(self.enabled);
    }
    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.counter = r.u16()?.min(256);
        self.enabled = r.bool()?;
        Ok(())
    }
}

impl Envelope {
    fn save(&self, w: &mut Writer) {
        w.u8(self.initial);
        w.bool(self.up);
        w.u8(self.period);
        w.u8(self.volume);
        w.u8(self.timer);
    }
    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.initial = r.u8()? & 0x0F;
        self.up = r.bool()?;
        self.period = r.u8()? & 0x07;
        self.volume = r.u8()? & 0x0F;
        self.timer = r.u8()?;
        Ok(())
    }
}

impl Square {
    fn save(&self, w: &mut Writer) {
        w.bool(self.on);
        w.bool(self.dac);
        self.length.save(w);
        self.envelope.save(w);
        w.u8(self.duty);
        w.u16(self.freq);
        w.i32(self.timer);
        w.u8(self.position);
    }
    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.on = r.bool()?;
        self.dac = r.bool()?;
        self.length.load(r)?;
        self.envelope.load(r)?;
        self.duty = r.u8()? & 0x03;
        self.freq = r.u16()? & 0x7FF;
        self.timer = r.i32()?.clamp(0, self.period());
        self.position = r.u8()? & 0x07;
        Ok(())
    }
}

impl Sweep {
    fn save(&self, w: &mut Writer) {
        w.u8(self.period);
        w.bool(self.negate);
        w.u8(self.shift);
        w.u8(self.timer);
        w.u16(self.shadow);
        w.bool(self.enabled);
        w.bool(self.negate_used);
    }
    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.period = r.u8()? & 0x07;
        self.negate = r.bool()?;
        self.shift = r.u8()? & 0x07;
        self.timer = r.u8()?;
        self.shadow = r.u16()? & 0x7FF;
        self.enabled = r.bool()?;
        self.negate_used = r.bool()?;
        Ok(())
    }
}

impl Wave {
    fn save(&self, w: &mut Writer) {
        w.bool(self.on);
        w.bool(self.dac);
        self.length.save(w);
        w.u8(self.volume_code);
        w.u16(self.freq);
        w.i32(self.timer);
        w.u8(self.position);
        w.u8(self.sample);
    }
    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.on = r.bool()?;
        self.dac = r.bool()?;
        self.length.load(r)?;
        self.volume_code = r.u8()? & 0x03;
        self.freq = r.u16()? & 0x7FF;
        self.timer = r.i32()?.clamp(0, self.period());
        self.position = r.u8()? & 0x1F;
        self.sample = r.u8()? & 0x0F;
        Ok(())
    }
}

impl Noise {
    fn save(&self, w: &mut Writer) {
        w.bool(self.on);
        w.bool(self.dac);
        self.length.save(w);
        self.envelope.save(w);
        w.u8(self.shift);
        w.bool(self.width7);
        w.u8(self.divisor);
        w.i32(self.timer);
        w.u16(self.lfsr);
    }
    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.on = r.bool()?;
        self.dac = r.bool()?;
        self.length.load(r)?;
        self.envelope.load(r)?;
        self.shift = r.u8()? & 0x0F;
        self.width7 = r.bool()?;
        self.divisor = r.u8()? & 0x07;
        self.timer = r.i32()?.clamp(0, self.period());
        self.lfsr = r.u16()? & 0x7FFF;
        Ok(())
    }
}

/// The output sample rate and unread samples belong to the frontend and are
/// left as they are.
impl Snapshot for Apu {
    fn save(&self, w: &mut Writer) {
        w.bool(self.powered);
        w.bytes(&self.regs);
        w.bytes(&self.wave_ram);
        w.u8(self.frame_step);
        self.ch1.save(w);
        self.sweep.save(w);
        self.ch2.save(w);
        self.ch3.save(w);
        self.ch4.save(w);
        w.u64(self.sample_phase);
        w.f32(self.sum_left);
        w.f32(self.sum_right);
        w.u32(self.sum_count);
        w.f32(self.cap_left);
        w.f32(self.cap_right);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.powered = r.bool()?;
        r.bytes(&mut self.regs)?;
        r.bytes(&mut self.wave_ram)?;
        self.frame_step = r.u8()? & 0x07;
        self.ch1.load(r)?;
        self.sweep.load(r)?;
        self.ch2.load(r)?;
        self.ch3.load(r)?;
        self.ch4.load(r)?;
        self.sample_phase = r.u64()? % CPU_HZ as u64;
        self.sum_left = r.f32()?;
        self.sum_right = r.f32()?;
        self.sum_count = r.u32()?;
        self.cap_left = r.f32()?;
        self.cap_right = r.f32()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_samples_at_requested_rate() {
        let mut apu = Apu::new();
        apu.set_sample_rate(48_000);
        for _ in 0..CPU_HZ / 4 {
            apu.tick();
        }
        let mut out = Vec::new();
        apu.drain_samples(&mut out);
        assert_eq!(out.len(), 48_000 * 2);
    }

    #[test]
    fn power_off_clears_registers_but_keeps_wave_ram() {
        let mut apu = Apu::new();
        apu.write(0xFF30, 0x12);
        apu.write(0xFF24, 0x77);
        apu.write(0xFF26, 0x00);
        assert_eq!(apu.read(0xFF24), 0x00);
        assert_eq!(apu.read(0xFF26), 0x70);
        apu.write(0xFF24, 0x55);
        assert_eq!(apu.read(0xFF24), 0x00, "writes ignored while off");
        assert_eq!(apu.read(0xFF30), 0x12);
    }

    #[test]
    fn length_expiry_silences_channel() {
        let mut apu = Apu::new();
        apu.write(0xFF17, 0xF0); // DAC on
        apu.write(0xFF16, 0x3E); // length 2
        apu.write(0xFF19, 0xC0); // trigger, length enabled
        assert_eq!(apu.read(0xFF26) & 0x02, 0x02);
        for _ in 0..4 {
            apu.clock_frame_sequencer();
        }
        assert_eq!(apu.read(0xFF26) & 0x02, 0);
    }

    #[test]
    fn silence_has_no_dc_offset() {
        let mut apu = Apu::new();
        apu.set_sample_rate(48_000);
        apu.write(0xFF26, 0x00);
        for _ in 0..CPU_HZ / 4 {
            apu.tick();
        }
        let mut out = Vec::new();
        apu.drain_samples(&mut out);
        assert!(out.iter().all(|s| s.abs() < 1e-3));
    }
}
