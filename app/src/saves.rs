//! Battery save files (`<rom name>.sav`) and save states
//! (`<rom name>.state`), both next to the ROM.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gb_core::GameBoy;

/// How often dirty battery RAM is flushed to disk while playing.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5);

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Outcome of looking for an existing save at startup.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// A save was found and restored.
    Restored,
    /// No save file exists yet.
    None,
    /// The file could not be used; it was copied to `backup` before anything
    /// can overwrite it.
    Rejected { reason: String, backup: PathBuf },
}

/// Keeps a game's battery state in sync with its .sav file.
pub struct SaveFile {
    path: PathBuf,
    dirty: bool,
    last_flush: Instant,
}

impl SaveFile {
    pub fn for_rom(rom_path: &Path) -> Self {
        Self {
            path: rom_path.with_extension("sav"),
            dirty: false,
            last_flush: Instant::now(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self, gb: &mut GameBoy) -> io::Result<Loaded> {
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::None),
            Err(e) => return Err(e),
        };
        match gb.load_save_data(&data, unix_now()) {
            Ok(()) => Ok(Loaded::Restored),
            Err(err) => {
                let mut backup = self.path.clone().into_os_string();
                backup.push(format!(".{}.bak", unix_now()));
                let backup = PathBuf::from(backup);
                std::fs::copy(&self.path, &backup)?;
                Ok(Loaded::Rejected {
                    reason: err.to_string(),
                    backup,
                })
            }
        }
    }

    /// Write the current battery state, replacing the file atomically.
    pub fn write(&mut self, gb: &GameBoy) -> io::Result<()> {
        write_atomic(&self.path, &gb.save_data(unix_now()))?;
        self.dirty = false;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// Call once per update; writes when the game changed its save RAM and
    /// the autosave interval has passed.
    pub fn autosave(&mut self, gb: &mut GameBoy) -> io::Result<()> {
        self.dirty |= gb.take_save_dirty();
        if self.dirty && self.last_flush.elapsed() >= AUTOSAVE_INTERVAL {
            // Retry next interval on failure; `dirty` stays set.
            self.last_flush = Instant::now();
            self.write(gb)?;
        }
        Ok(())
    }
}

/// Write `data` to `path` via a temporary file, so a crash mid-write never
/// leaves a truncated file behind.
fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// The single save-state slot for a game.
pub struct StateFile {
    path: PathBuf,
}

impl StateFile {
    pub fn for_rom(rom_path: &Path) -> Self {
        Self {
            path: rom_path.with_extension("state"),
        }
    }

    pub fn save(&self, gb: &GameBoy) -> Result<(), String> {
        write_atomic(&self.path, &gb.save_state())
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))
    }

    /// Load the state; on any error the game keeps running unchanged.
    pub fn load(&self, gb: &mut GameBoy) -> Result<(), String> {
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err("no save state yet (press F5 to create one)".to_string());
            }
            Err(e) => return Err(format!("cannot read {}: {e}", self.path.display())),
        };
        gb.load_state(&data).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MBC1+RAM+BATTERY ROM whose program writes 0x42 to cartridge RAM
    /// address 0xA123, then loops forever.
    fn saving_game() -> GameBoy {
        let mut rom = vec![0u8; 0x8000];
        rom[0x147] = 0x03;
        rom[0x149] = 0x02;
        let program = [
            0x3E, 0x0A, // LD A,0x0A
            0xEA, 0x00, 0x00, // LD (0x0000),A   ; enable RAM
            0x3E, 0x42, // LD A,0x42
            0xEA, 0x23, 0xA1, // LD (0xA123),A
            0x18, 0xFE, // JR -2
        ];
        rom[0x100..0x100 + program.len()].copy_from_slice(&program);
        GameBoy::new(rom).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gbemu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_game_written_ram_through_sav_file() {
        let dir = temp_dir("roundtrip");
        let rom_path = dir.join("game.gb");

        let mut gb = saving_game();
        gb.run_frame();
        assert!(
            gb.take_save_dirty(),
            "game write should mark the save dirty"
        );
        let mut save = SaveFile::for_rom(&rom_path);
        assert_eq!(save.path(), dir.join("game.sav"));
        save.write(&gb).unwrap();
        assert_eq!(std::fs::read(save.path()).unwrap().len(), 0x2000);

        let mut fresh = saving_game();
        assert_eq!(save.load(&mut fresh).unwrap(), Loaded::Restored);
        assert_eq!(fresh.save_data(0)[0x123], 0x42);
        assert_eq!(fresh.save_data(0), gb.save_data(0));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_state_file_round_trip() {
        let dir = temp_dir("state");
        let state = StateFile::for_rom(&dir.join("game.gb"));
        let mut gb = saving_game();
        assert!(state.load(&mut gb).unwrap_err().contains("no save state"));

        gb.run_frame();
        state.save(&gb).unwrap();
        assert!(dir.join("game.state").exists());
        let mut fresh = saving_game();
        state.load(&mut fresh).unwrap();
        assert_eq!(fresh.save_state(), gb.save_state());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_save_is_not_an_error() {
        let dir = temp_dir("missing");
        let save = SaveFile::for_rom(&dir.join("game.gb"));
        assert_eq!(save.load(&mut saving_game()).unwrap(), Loaded::None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wrong_size_save_is_backed_up_not_lost() {
        let dir = temp_dir("badsize");
        let save = SaveFile::for_rom(&dir.join("game.gb"));
        std::fs::write(save.path(), [1, 2, 3]).unwrap();
        match save.load(&mut saving_game()).unwrap() {
            Loaded::Rejected { backup, .. } => {
                assert_eq!(std::fs::read(backup).unwrap(), [1, 2, 3]);
            }
            other => panic!("expected rejection, got {other:?}"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
