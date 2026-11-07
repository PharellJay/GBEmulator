//! Save-state serialization: a small little-endian binary format.
//!
//! Layout: magic "GBST", format version (u16), ROM hash (u64), then each
//! component's fields in a fixed order.

use std::fmt;

pub(crate) const MAGIC: &[u8; 4] = b"GBST";
pub(crate) const VERSION: u16 = 1;

/// Why a save state could not be loaded. The emulator is left unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// The data does not start with the save-state signature.
    NotAState,
    /// Written by an incompatible version of the format.
    UnsupportedVersion(u16),
    /// Saved from a different ROM.
    WrongGame,
    /// The data ends early.
    Truncated,
    /// A field holds a value this emulator cannot have produced.
    Invalid(&'static str),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAState => write!(f, "not a save state"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported save state version {v}"),
            Self::WrongGame => write!(f, "save state belongs to a different game"),
            Self::Truncated => write!(f, "save state is truncated"),
            Self::Invalid(what) => write!(f, "save state is corrupt ({what})"),
        }
    }
}

impl std::error::Error for StateError {}

pub(crate) type Result<T> = std::result::Result<T, StateError>;

/// Components that can write and restore their state.
pub(crate) trait Snapshot {
    fn save(&self, w: &mut Writer);
    fn load(&mut self, r: &mut Reader) -> Result<()>;
}

#[derive(Default)]
pub(crate) struct Writer {
    pub(crate) buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub(crate) fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    pub(crate) fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    /// Length-prefixed byte block.
    pub(crate) fn block(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.bytes(v);
    }
}

pub(crate) struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.data.len() < N {
            return Err(StateError::Truncated);
        }
        let (head, rest) = self.data.split_at(N);
        self.data = rest;
        Ok(head.try_into().unwrap())
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    pub(crate) fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StateError::Invalid("boolean")),
        }
    }
    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    pub(crate) fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    pub(crate) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    pub(crate) fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub(crate) fn bytes(&mut self, out: &mut [u8]) -> Result<()> {
        if self.data.len() < out.len() {
            return Err(StateError::Truncated);
        }
        let (head, rest) = self.data.split_at(out.len());
        out.copy_from_slice(head);
        self.data = rest;
        Ok(())
    }
    /// Length-prefixed block that must match `out`'s length exactly.
    pub(crate) fn block(&mut self, out: &mut [u8], what: &'static str) -> Result<()> {
        if self.u32()? as usize != out.len() {
            return Err(StateError::Invalid(what));
        }
        self.bytes(out)
    }
    pub(crate) fn finish(&self) -> Result<()> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(StateError::Invalid("trailing data"))
        }
    }
}

/// FNV-1a, used to tie a save state to the ROM it came from.
pub(crate) fn rom_hash(rom: &[u8]) -> u64 {
    rom.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3)
    })
}
