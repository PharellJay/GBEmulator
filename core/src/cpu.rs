//! Sharp SM83 CPU. Each memory access costs one M-cycle; internal delays are
//! explicit `bus.tick()` calls so instruction timing matches hardware.

use crate::bus::Bus;
use crate::state::{self, Reader, Snapshot, Writer};

const FLAG_Z: u8 = 0x80;
const FLAG_N: u8 = 0x40;
const FLAG_H: u8 = 0x20;
const FLAG_C: u8 = 0x10;

pub(crate) struct Cpu {
    a: u8,
    f: u8,
    b: u8,
    c: u8,
    d: u8,
    e: u8,
    h: u8,
    l: u8,
    sp: u16,
    pc: u16,
    ime: bool,
    /// Counts down to IME being set; EI takes effect after the next instruction.
    ime_delay: u8,
    halted: bool,
    /// Next opcode fetch does not increment PC (HALT with IME=0 and a pending interrupt).
    halt_bug: bool,
    /// An illegal opcode hangs the CPU until reset.
    locked: bool,
}

impl Cpu {
    /// Register state after the DMG boot ROM hands over control.
    pub(crate) fn new() -> Self {
        Self {
            a: 0x01,
            f: 0xB0,
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
            sp: 0xFFFE,
            pc: 0x0100,
            ime: false,
            ime_delay: 0,
            halted: false,
            halt_bug: false,
            locked: false,
        }
    }

    /// Execute one instruction, interrupt dispatch, or halted cycle.
    pub(crate) fn step(&mut self, bus: &mut Bus) {
        if self.locked {
            bus.tick();
            return;
        }
        if self.halted {
            if bus.pending_interrupts() == 0 {
                bus.tick();
                return;
            }
            self.halted = false;
        }
        if self.ime && bus.pending_interrupts() != 0 {
            self.service_interrupt(bus);
            return;
        }
        let opcode = self.fetch8(bus);
        self.execute(bus, opcode);
        if self.ime_delay > 0 {
            self.ime_delay -= 1;
            if self.ime_delay == 0 {
                self.ime = true;
            }
        }
    }

    fn service_interrupt(&mut self, bus: &mut Bus) {
        self.ime = false;
        bus.tick();
        bus.tick();
        let [lo, hi] = self.pc.to_le_bytes();
        self.sp = self.sp.wrapping_sub(1);
        self.write(bus, self.sp, hi);
        // The vector is chosen after the high byte is pushed: that push can
        // overwrite IE and cancel the interrupt, in which case PC becomes 0.
        let pending = bus.pending_interrupts();
        self.sp = self.sp.wrapping_sub(1);
        self.write(bus, self.sp, lo);
        if pending == 0 {
            self.pc = 0;
        } else {
            let bit = pending.trailing_zeros() as u16;
            bus.acknowledge_interrupt(1 << bit);
            self.pc = 0x40 + 8 * bit;
        }
        bus.tick();
    }

    // ---- memory helpers -------------------------------------------------

    fn read(&mut self, bus: &mut Bus, addr: u16) -> u8 {
        bus.tick();
        bus.read(addr)
    }

    fn write(&mut self, bus: &mut Bus, addr: u16, value: u8) {
        bus.tick();
        bus.write(addr, value);
    }

    fn fetch8(&mut self, bus: &mut Bus) -> u8 {
        let value = self.read(bus, self.pc);
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.pc = self.pc.wrapping_add(1);
        }
        value
    }

    fn fetch16(&mut self, bus: &mut Bus) -> u16 {
        let lo = self.fetch8(bus);
        let hi = self.fetch8(bus);
        u16::from_le_bytes([lo, hi])
    }

    fn push(&mut self, bus: &mut Bus, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.sp = self.sp.wrapping_sub(1);
        self.write(bus, self.sp, hi);
        self.sp = self.sp.wrapping_sub(1);
        self.write(bus, self.sp, lo);
    }

    fn pop(&mut self, bus: &mut Bus) -> u16 {
        let lo = self.read(bus, self.sp);
        self.sp = self.sp.wrapping_add(1);
        let hi = self.read(bus, self.sp);
        self.sp = self.sp.wrapping_add(1);
        u16::from_le_bytes([lo, hi])
    }

    // ---- register helpers -----------------------------------------------

    fn hl(&self) -> u16 {
        u16::from_be_bytes([self.h, self.l])
    }

    fn set_hl(&mut self, v: u16) {
        [self.h, self.l] = v.to_be_bytes();
    }

    /// 16-bit register by index: 0=BC 1=DE 2=HL 3=SP.
    fn rr(&self, idx: u8) -> u16 {
        match idx {
            0 => u16::from_be_bytes([self.b, self.c]),
            1 => u16::from_be_bytes([self.d, self.e]),
            2 => self.hl(),
            _ => self.sp,
        }
    }

    fn set_rr(&mut self, idx: u8, v: u16) {
        match idx {
            0 => [self.b, self.c] = v.to_be_bytes(),
            1 => [self.d, self.e] = v.to_be_bytes(),
            2 => self.set_hl(v),
            _ => self.sp = v,
        }
    }

    /// 16-bit register for PUSH/POP: 0=BC 1=DE 2=HL 3=AF.
    fn rr_stack(&self, idx: u8) -> u16 {
        if idx == 3 {
            u16::from_be_bytes([self.a, self.f])
        } else {
            self.rr(idx)
        }
    }

    fn set_rr_stack(&mut self, idx: u8, v: u16) {
        if idx == 3 {
            [self.a, self.f] = v.to_be_bytes();
            self.f &= 0xF0;
        } else {
            self.set_rr(idx, v);
        }
    }

    /// 8-bit operand by index: B C D E H L (HL) A.
    fn read_r(&mut self, bus: &mut Bus, idx: u8) -> u8 {
        match idx {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => self.read(bus, self.hl()),
            _ => self.a,
        }
    }

    fn write_r(&mut self, bus: &mut Bus, idx: u8, v: u8) {
        match idx {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            6 => self.write(bus, self.hl(), v),
            _ => self.a = v,
        }
    }

    fn carry(&self) -> bool {
        self.f & FLAG_C != 0
    }

    fn set_flags(&mut self, z: bool, n: bool, h: bool, c: bool) {
        self.f = (if z { FLAG_Z } else { 0 })
            | (if n { FLAG_N } else { 0 })
            | (if h { FLAG_H } else { 0 })
            | (if c { FLAG_C } else { 0 });
    }

    /// Branch condition by index: NZ Z NC C.
    fn condition(&self, idx: u8) -> bool {
        match idx {
            0 => self.f & FLAG_Z == 0,
            1 => self.f & FLAG_Z != 0,
            2 => !self.carry(),
            _ => self.carry(),
        }
    }

    // ---- arithmetic -----------------------------------------------------

    fn alu(&mut self, op: u8, v: u8) {
        let a = self.a;
        let carry_in = self.carry() as u8;
        match op {
            0 | 1 => {
                let c = if op == 1 { carry_in } else { 0 };
                let sum = a as u16 + v as u16 + c as u16;
                let half = (a & 0x0F) + (v & 0x0F) + c > 0x0F;
                self.a = sum as u8;
                self.set_flags(self.a == 0, false, half, sum > 0xFF);
            }
            2 | 3 | 7 => {
                let c = if op == 3 { carry_in } else { 0 };
                let result = a.wrapping_sub(v).wrapping_sub(c);
                let half = (a & 0x0F) < (v & 0x0F) + c;
                let borrow = (a as u16) < v as u16 + c as u16;
                self.set_flags(result == 0, true, half, borrow);
                if op != 7 {
                    self.a = result;
                }
            }
            4 => {
                self.a &= v;
                self.set_flags(self.a == 0, false, true, false);
            }
            5 => {
                self.a ^= v;
                self.set_flags(self.a == 0, false, false, false);
            }
            _ => {
                self.a |= v;
                self.set_flags(self.a == 0, false, false, false);
            }
        }
    }

    /// SP + signed 8-bit offset, with the flags shared by ADD SP,e and LD HL,SP+e.
    fn sp_offset(&mut self, offset: u8) -> u16 {
        let sp = self.sp;
        let half = (sp & 0x0F) + (offset as u16 & 0x0F) > 0x0F;
        let carry = (sp & 0xFF) + offset as u16 > 0xFF;
        self.set_flags(false, false, half, carry);
        sp.wrapping_add(offset as i8 as u16)
    }

    fn daa(&mut self) {
        let mut a = self.a;
        let mut carry = self.carry();
        let half = self.f & FLAG_H != 0;
        if self.f & FLAG_N == 0 {
            if carry || a > 0x99 {
                a = a.wrapping_add(0x60);
                carry = true;
            }
            if half || a & 0x0F > 0x09 {
                a = a.wrapping_add(0x06);
            }
        } else {
            if carry {
                a = a.wrapping_sub(0x60);
            }
            if half {
                a = a.wrapping_sub(0x06);
            }
        }
        self.a = a;
        self.f = (if a == 0 { FLAG_Z } else { 0 })
            | (self.f & FLAG_N)
            | (if carry { FLAG_C } else { 0 });
    }

    /// Shift/rotate group shared by CB 0x00-0x3F; returns (result, carry out).
    fn shift_op(&self, op: u8, v: u8) -> (u8, bool) {
        let cin = self.carry() as u8;
        match op {
            0 => (v.rotate_left(1), v & 0x80 != 0),
            1 => (v.rotate_right(1), v & 0x01 != 0),
            2 => ((v << 1) | cin, v & 0x80 != 0),
            3 => ((v >> 1) | (cin << 7), v & 0x01 != 0),
            4 => (v << 1, v & 0x80 != 0),
            5 => ((v >> 1) | (v & 0x80), v & 0x01 != 0),
            6 => (v.rotate_left(4), false),
            _ => (v >> 1, v & 0x01 != 0),
        }
    }

    // ---- decode / execute -----------------------------------------------

    fn execute(&mut self, bus: &mut Bus, op: u8) {
        match op {
            0x00 => {}
            0x01 | 0x11 | 0x21 | 0x31 => {
                let v = self.fetch16(bus);
                self.set_rr(op >> 4, v);
            }
            0x02 | 0x12 => {
                let addr = self.rr(op >> 4);
                self.write(bus, addr, self.a);
            }
            0x22 | 0x32 => {
                let hl = self.hl();
                self.write(bus, hl, self.a);
                self.set_hl(if op == 0x22 {
                    hl.wrapping_add(1)
                } else {
                    hl.wrapping_sub(1)
                });
            }
            0x0A | 0x1A => {
                let addr = self.rr(op >> 4);
                self.a = self.read(bus, addr);
            }
            0x2A | 0x3A => {
                let hl = self.hl();
                self.a = self.read(bus, hl);
                self.set_hl(if op == 0x2A {
                    hl.wrapping_add(1)
                } else {
                    hl.wrapping_sub(1)
                });
            }
            0x03 | 0x13 | 0x23 | 0x33 => {
                let idx = op >> 4;
                self.set_rr(idx, self.rr(idx).wrapping_add(1));
                bus.tick();
            }
            0x0B | 0x1B | 0x2B | 0x3B => {
                let idx = op >> 4;
                self.set_rr(idx, self.rr(idx).wrapping_sub(1));
                bus.tick();
            }
            0x09 | 0x19 | 0x29 | 0x39 => {
                let hl = self.hl();
                let v = self.rr(op >> 4);
                let half = (hl & 0x0FFF) + (v & 0x0FFF) > 0x0FFF;
                let (sum, carry) = hl.overflowing_add(v);
                self.set_flags(self.f & FLAG_Z != 0, false, half, carry);
                self.set_hl(sum);
                bus.tick();
            }
            0x07 => {
                let (r, c) = self.shift_op(0, self.a);
                self.a = r;
                self.set_flags(false, false, false, c);
            }
            0x0F => {
                let (r, c) = self.shift_op(1, self.a);
                self.a = r;
                self.set_flags(false, false, false, c);
            }
            0x17 => {
                let (r, c) = self.shift_op(2, self.a);
                self.a = r;
                self.set_flags(false, false, false, c);
            }
            0x1F => {
                let (r, c) = self.shift_op(3, self.a);
                self.a = r;
                self.set_flags(false, false, false, c);
            }
            0x08 => {
                let addr = self.fetch16(bus);
                let [lo, hi] = self.sp.to_le_bytes();
                self.write(bus, addr, lo);
                self.write(bus, addr.wrapping_add(1), hi);
            }
            0x10 => {
                // STOP: no speed switch on DMG; skip the padding byte.
                self.fetch8(bus);
            }
            0x18 => {
                let offset = self.fetch8(bus) as i8;
                self.pc = self.pc.wrapping_add(offset as u16);
                bus.tick();
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                let offset = self.fetch8(bus) as i8;
                if self.condition((op >> 3) & 0x03) {
                    self.pc = self.pc.wrapping_add(offset as u16);
                    bus.tick();
                }
            }
            0x27 => self.daa(),
            0x2F => {
                self.a = !self.a;
                self.f |= FLAG_N | FLAG_H;
            }
            0x37 => self.f = (self.f & FLAG_Z) | FLAG_C,
            0x3F => self.f = (self.f & FLAG_Z) | ((self.f ^ FLAG_C) & FLAG_C),
            0x76 => {
                if !self.ime && bus.pending_interrupts() != 0 {
                    self.halt_bug = true;
                } else {
                    self.halted = true;
                }
            }
            op if op & 0xC7 == 0x04 => {
                let idx = (op >> 3) & 0x07;
                let v = self.read_r(bus, idx);
                let r = v.wrapping_add(1);
                let c = self.carry();
                self.set_flags(r == 0, false, v & 0x0F == 0x0F, c);
                self.write_r(bus, idx, r);
            }
            op if op & 0xC7 == 0x05 => {
                let idx = (op >> 3) & 0x07;
                let v = self.read_r(bus, idx);
                let r = v.wrapping_sub(1);
                let c = self.carry();
                self.set_flags(r == 0, true, v & 0x0F == 0, c);
                self.write_r(bus, idx, r);
            }
            op if op & 0xC7 == 0x06 => {
                let v = self.fetch8(bus);
                self.write_r(bus, (op >> 3) & 0x07, v);
            }
            0x40..=0x7F => {
                let v = self.read_r(bus, op & 0x07);
                self.write_r(bus, (op >> 3) & 0x07, v);
            }
            0x80..=0xBF => {
                let v = self.read_r(bus, op & 0x07);
                self.alu((op >> 3) & 0x07, v);
            }
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                bus.tick();
                if self.condition((op >> 3) & 0x03) {
                    self.pc = self.pop(bus);
                    bus.tick();
                }
            }
            0xC9 | 0xD9 => {
                self.pc = self.pop(bus);
                bus.tick();
                if op == 0xD9 {
                    self.ime = true;
                }
            }
            0xC1 | 0xD1 | 0xE1 | 0xF1 => {
                let v = self.pop(bus);
                self.set_rr_stack((op >> 4) & 0x03, v);
            }
            0xC5 | 0xD5 | 0xE5 | 0xF5 => {
                bus.tick();
                let v = self.rr_stack((op >> 4) & 0x03);
                self.push(bus, v);
            }
            0xC2 | 0xCA | 0xD2 | 0xDA | 0xC3 => {
                let addr = self.fetch16(bus);
                if op == 0xC3 || self.condition((op >> 3) & 0x03) {
                    self.pc = addr;
                    bus.tick();
                }
            }
            0xC4 | 0xCC | 0xD4 | 0xDC | 0xCD => {
                let addr = self.fetch16(bus);
                if op == 0xCD || self.condition((op >> 3) & 0x03) {
                    bus.tick();
                    self.push(bus, self.pc);
                    self.pc = addr;
                }
            }
            op if op & 0xC7 == 0xC6 => {
                let v = self.fetch8(bus);
                self.alu((op >> 3) & 0x07, v);
            }
            op if op & 0xC7 == 0xC7 => {
                bus.tick();
                self.push(bus, self.pc);
                self.pc = (op & 0x38) as u16;
            }
            0xCB => {
                let cb = self.fetch8(bus);
                self.execute_cb(bus, cb);
            }
            0xE0 => {
                let n = self.fetch8(bus);
                self.write(bus, 0xFF00 | n as u16, self.a);
            }
            0xF0 => {
                let n = self.fetch8(bus);
                self.a = self.read(bus, 0xFF00 | n as u16);
            }
            0xE2 => self.write(bus, 0xFF00 | self.c as u16, self.a),
            0xF2 => self.a = self.read(bus, 0xFF00 | self.c as u16),
            0xEA => {
                let addr = self.fetch16(bus);
                self.write(bus, addr, self.a);
            }
            0xFA => {
                let addr = self.fetch16(bus);
                self.a = self.read(bus, addr);
            }
            0xE8 => {
                let offset = self.fetch8(bus);
                self.sp = self.sp_offset(offset);
                bus.tick();
                bus.tick();
            }
            0xF8 => {
                let offset = self.fetch8(bus);
                let v = self.sp_offset(offset);
                self.set_hl(v);
                bus.tick();
            }
            0xE9 => self.pc = self.hl(),
            0xF9 => {
                self.sp = self.hl();
                bus.tick();
            }
            0xF3 => {
                self.ime = false;
                self.ime_delay = 0;
            }
            0xFB => {
                if !self.ime && self.ime_delay == 0 {
                    self.ime_delay = 2;
                }
            }
            // D3 DB DD E3 E4 EB EC ED F4 FC FD
            _ => self.locked = true,
        }
    }

    fn execute_cb(&mut self, bus: &mut Bus, cb: u8) {
        let idx = cb & 0x07;
        let bit = (cb >> 3) & 0x07;
        let v = self.read_r(bus, idx);
        match cb >> 6 {
            0 => {
                let (r, c) = self.shift_op(bit, v);
                self.set_flags(r == 0, false, false, c);
                self.write_r(bus, idx, r);
            }
            1 => {
                let c = self.carry();
                self.set_flags(v & (1 << bit) == 0, false, true, c);
            }
            2 => self.write_r(bus, idx, v & !(1 << bit)),
            _ => self.write_r(bus, idx, v | (1 << bit)),
        }
    }
}

impl Snapshot for Cpu {
    fn save(&self, w: &mut Writer) {
        for v in [
            self.a, self.f, self.b, self.c, self.d, self.e, self.h, self.l,
        ] {
            w.u8(v);
        }
        w.u16(self.sp);
        w.u16(self.pc);
        w.bool(self.ime);
        w.u8(self.ime_delay);
        w.bool(self.halted);
        w.bool(self.halt_bug);
        w.bool(self.locked);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        self.a = r.u8()?;
        self.f = r.u8()? & 0xF0;
        self.b = r.u8()?;
        self.c = r.u8()?;
        self.d = r.u8()?;
        self.e = r.u8()?;
        self.h = r.u8()?;
        self.l = r.u8()?;
        self.sp = r.u16()?;
        self.pc = r.u16()?;
        self.ime = r.bool()?;
        self.ime_delay = r.u8()?;
        self.halted = r.bool()?;
        self.halt_bug = r.bool()?;
        self.locked = r.bool()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::GameBoy;

    /// A 32 KiB ROM-only image with `program` at the 0x0100 entry point.
    fn gb(program: &[u8]) -> GameBoy {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        GameBoy::new(rom).unwrap()
    }

    fn run(gb: &mut GameBoy, instructions: usize) {
        for _ in 0..instructions {
            gb.step();
        }
    }

    #[test]
    fn instruction_timings() {
        // (program, M-cycles of the first instruction)
        let cases: &[(&[u8], u32)] = &[
            (&[0x00], 1),             // NOP
            (&[0x01, 0, 0], 3),       // LD BC,d16
            (&[0xCD, 0, 0x02], 6),    // CALL
            (&[0x18, 0x00], 3),       // JR taken
            (&[0x20, 0x00], 2),       // JR NZ not taken (Z set after boot)
            (&[0xC5], 4),             // PUSH BC
            (&[0xC1], 3),             // POP BC
            (&[0x34], 3),             // INC (HL)
            (&[0xCB, 0x06], 4),       // RLC (HL)
            (&[0xCB, 0x46], 3),       // BIT 0,(HL)
            (&[0xE8, 0x01], 4),       // ADD SP,e
            (&[0xF8, 0x01], 3),       // LD HL,SP+e
            (&[0x08, 0x00, 0xC0], 5), // LD (a16),SP
            (&[0xC7], 4),             // RST 00
            (&[0xC0], 2),             // RET NZ not taken
        ];
        for (program, expected) in cases {
            let mut g = gb(program);
            assert_eq!(g.step(), *expected, "opcode {:02X}", program[0]);
        }
    }

    #[test]
    fn daa_corrects_bcd_addition() {
        // LD A,0x45; ADD A,0x38; DAA
        let mut g = gb(&[0x3E, 0x45, 0xC6, 0x38, 0x27]);
        run(&mut g, 3);
        assert_eq!(g.cpu.a, 0x83);
    }

    #[test]
    fn pop_af_masks_low_flag_bits() {
        // LD BC,0x12FF; PUSH BC; POP AF
        let mut g = gb(&[0x01, 0xFF, 0x12, 0xC5, 0xF1]);
        run(&mut g, 3);
        assert_eq!(g.cpu.a, 0x12);
        assert_eq!(g.cpu.f, 0xF0);
    }

    #[test]
    fn ei_takes_effect_after_next_instruction() {
        // LD A,0x01; LDH (FF),A; LDH (0F),A; EI; NOP; NOP
        let mut g = gb(&[0x3E, 0x01, 0xE0, 0xFF, 0xE0, 0x0F, 0xFB, 0x00, 0x00]);
        run(&mut g, 4); // through EI
        g.step(); // NOP after EI still runs
        assert_eq!(g.cpu.pc, 0x0108);
        assert_eq!(g.step(), 5, "interrupt dispatch takes 5 M-cycles");
        assert_eq!(g.cpu.pc, 0x0040);
    }

    #[test]
    fn halt_bug_repeats_next_byte() {
        // LD A,0x01; LDH (FF),A; LDH (0F),A; HALT; INC A
        let mut g = gb(&[0x3E, 0x01, 0xE0, 0xFF, 0xE0, 0x0F, 0x76, 0x3C]);
        run(&mut g, 4);
        run(&mut g, 2); // INC A executes twice
        assert_eq!(g.cpu.a, 0x03);
    }

    #[test]
    fn add_sp_flags_use_low_byte() {
        // LD SP,0x00FF; ADD SP,0x01
        let mut g = gb(&[0x31, 0xFF, 0x00, 0xE8, 0x01]);
        run(&mut g, 2);
        assert_eq!(g.cpu.sp, 0x0100);
        assert_eq!(g.cpu.f, 0x30);
    }
}
