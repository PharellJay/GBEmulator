//! MBC3 real-time clock. It advances with emulated time; the frontend
//! supplies wall-clock time only when saving and loading, so the core stays
//! deterministic.

/// M-cycles per emulated second.
use crate::state::{self, Reader, Snapshot, Writer};
const MCYCLES_PER_SECOND: u32 = crate::CPU_HZ / 4;

const SECONDS: usize = 0;
const MINUTES: usize = 1;
const HOURS: usize = 2;
const DAY_LOW: usize = 3;
const DAY_HIGH: usize = 4;

const DH_DAY_BIT8: u8 = 0x01;
const DH_HALT: u8 = 0x40;
const DH_CARRY: u8 = 0x80;

/// Writable bits of S, M, H, DL, DH.
const MASKS: [u8; 5] = [0x3F, 0x3F, 0x1F, 0xFF, 0xC1];

/// Size of the RTC block appended to a .sav (BGB / VBA-M layout).
pub(crate) const SAVE_LEN: usize = 48;

#[derive(Clone)]
pub(crate) struct Rtc {
    live: [u8; 5],
    latched: [u8; 5],
    subsecond: u32,
}

impl Rtc {
    pub(crate) fn new() -> Self {
        Self {
            live: [0; 5],
            latched: [0; 5],
            subsecond: 0,
        }
    }

    fn halted(&self) -> bool {
        self.live[DAY_HIGH] & DH_HALT != 0
    }

    pub(crate) fn tick(&mut self) {
        if self.halted() {
            return;
        }
        self.subsecond += 1;
        if self.subsecond == MCYCLES_PER_SECOND {
            self.subsecond = 0;
            self.advance(1);
        }
    }

    fn days(&self) -> u64 {
        (((self.live[DAY_HIGH] & DH_DAY_BIT8) as u64) << 8) | self.live[DAY_LOW] as u64
    }

    /// Advance the live registers by `seconds`.
    fn advance(&mut self, seconds: u64) {
        if seconds == 0 || self.halted() {
            return;
        }
        // Out-of-range values written by software (e.g. seconds = 61) wrap
        // at the register width on hardware without carrying; step those
        // one second at a time until they are back in range.
        let mut remaining = seconds;
        while remaining > 0
            && (self.live[SECONDS] >= 60 || self.live[MINUTES] >= 60 || self.live[HOURS] >= 24)
        {
            self.step_one_second();
            remaining -= 1;
        }
        if remaining == 0 {
            return;
        }
        let total = self.days() * 86_400
            + self.live[HOURS] as u64 * 3_600
            + self.live[MINUTES] as u64 * 60
            + self.live[SECONDS] as u64
            + remaining;
        let mut days = total / 86_400;
        let mut dh = self.live[DAY_HIGH] & (DH_HALT | DH_CARRY);
        if days >= 512 {
            dh |= DH_CARRY;
            days %= 512;
        }
        self.live[SECONDS] = (total % 60) as u8;
        self.live[MINUTES] = (total / 60 % 60) as u8;
        self.live[HOURS] = (total / 3_600 % 24) as u8;
        self.live[DAY_LOW] = days as u8;
        self.live[DAY_HIGH] = dh | (days >> 8) as u8;
    }

    fn step_one_second(&mut self) {
        let wrap = |v: u8, mask: u8| (v.wrapping_add(1)) & mask;
        self.live[SECONDS] = wrap(self.live[SECONDS], MASKS[SECONDS]);
        if self.live[SECONDS] != 60 {
            return;
        }
        self.live[SECONDS] = 0;
        self.live[MINUTES] = wrap(self.live[MINUTES], MASKS[MINUTES]);
        if self.live[MINUTES] != 60 {
            return;
        }
        self.live[MINUTES] = 0;
        self.live[HOURS] = wrap(self.live[HOURS], MASKS[HOURS]);
        if self.live[HOURS] != 24 {
            return;
        }
        self.live[HOURS] = 0;
        let days = self.days() + 1;
        let mut dh = self.live[DAY_HIGH] & (DH_HALT | DH_CARRY);
        if days >= 512 {
            dh |= DH_CARRY;
        }
        self.live[DAY_LOW] = days as u8;
        self.live[DAY_HIGH] = dh | ((days >> 8) & 1) as u8;
    }

    pub(crate) fn latch(&mut self) {
        self.latched = self.live;
    }

    /// Read a latched register; `select` is 0x08-0x0C.
    pub(crate) fn read(&self, select: u8) -> u8 {
        let i = (select - 0x08) as usize;
        self.latched[i] & MASKS[i]
    }

    pub(crate) fn write(&mut self, select: u8, value: u8) {
        let i = (select - 0x08) as usize;
        if i == SECONDS {
            self.subsecond = 0;
        }
        self.live[i] = value & MASKS[i];
        self.latched[i] = self.live[i];
    }

    pub(crate) fn to_save(&self, unix_now: u64) -> [u8; SAVE_LEN] {
        let mut out = [0u8; SAVE_LEN];
        for (i, &v) in self.live.iter().chain(self.latched.iter()).enumerate() {
            out[i * 4] = v;
        }
        out[40..48].copy_from_slice(&unix_now.to_le_bytes());
        out
    }

    /// Restore from a 44- or 48-byte block and catch up on real time elapsed
    /// since it was written.
    pub(crate) fn load_save(&mut self, data: &[u8], unix_now: u64) {
        for i in 0..5 {
            self.live[i] = data[i * 4] & MASKS[i];
            self.latched[i] = data[20 + i * 4] & MASKS[i];
        }
        let saved_at = if data.len() >= 48 {
            u64::from_le_bytes(data[40..48].try_into().unwrap())
        } else {
            u32::from_le_bytes(data[40..44].try_into().unwrap()) as u64
        };
        self.subsecond = 0;
        self.advance(unix_now.saturating_sub(saved_at));
    }
}

impl Snapshot for Rtc {
    fn save(&self, w: &mut Writer) {
        w.bytes(&self.live);
        w.bytes(&self.latched);
        w.u32(self.subsecond);
    }

    fn load(&mut self, r: &mut Reader) -> state::Result<()> {
        r.bytes(&mut self.live)?;
        r.bytes(&mut self.latched)?;
        for (i, mask) in MASKS.iter().enumerate() {
            self.live[i] &= mask;
            self.latched[i] &= mask;
        }
        self.subsecond = r.u32()? % MCYCLES_PER_SECOND;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(rtc: &mut Rtc, regs: [u8; 5]) {
        for (i, v) in regs.into_iter().enumerate() {
            rtc.write(0x08 + i as u8, v);
        }
    }

    fn latched(rtc: &mut Rtc) -> [u8; 5] {
        rtc.latch();
        std::array::from_fn(|i| rtc.read(0x08 + i as u8))
    }

    #[test]
    fn ticks_one_second_per_emulated_second() {
        let mut rtc = Rtc::new();
        for _ in 0..MCYCLES_PER_SECOND * 2 {
            rtc.tick();
        }
        assert_eq!(latched(&mut rtc), [2, 0, 0, 0, 0]);
    }

    #[test]
    fn rollover_sets_day_carry() {
        let mut rtc = Rtc::new();
        set(&mut rtc, [59, 59, 23, 0xFF, 0x01]); // day 511, 23:59:59
        rtc.advance(1);
        assert_eq!(latched(&mut rtc), [0, 0, 0, 0, DH_CARRY]);
    }

    #[test]
    fn halt_stops_the_clock() {
        let mut rtc = Rtc::new();
        set(&mut rtc, [10, 0, 0, 0, DH_HALT]);
        rtc.advance(100);
        assert_eq!(latched(&mut rtc)[SECONDS], 10);
    }

    #[test]
    fn save_round_trip_catches_up_wall_clock() {
        let mut rtc = Rtc::new();
        set(&mut rtc, [30, 59, 1, 2, 0]);
        let saved = rtc.to_save(1_000);
        let mut restored = Rtc::new();
        restored.load_save(&saved, 1_000 + 3_600 + 30); // 1h 30s later
        assert_eq!(latched(&mut restored), [0, 0, 3, 2, 0]);
    }

    #[test]
    fn out_of_range_seconds_wrap_without_carry() {
        let mut rtc = Rtc::new();
        set(&mut rtc, [62, 0, 0, 0, 0]);
        rtc.advance(2); // 62 -> 63 -> 0, no minute carry
        assert_eq!(latched(&mut rtc)[..2], [0, 0]);
    }
}
