//! User settings, persisted as TOML in the OS config directory.
//!
//! Loading never fails: a missing file gives defaults, an unreadable or
//! corrupt one gives defaults plus a warning (and a backup of the bad file),
//! and individual bad values fall back to their defaults.

use std::io;
use std::path::{Path, PathBuf};

use eframe::egui::Key;
use gb_core::Button;
use serde::{Deserialize, Serialize};

pub const MIN_SCALE: u8 = 1;
pub const MAX_SCALE: u8 = 8;
/// Opens and closes the settings menu.
pub const MENU_KEY: Key = Key::Escape;
pub const SAVE_STATE_KEY: Key = Key::F5;
pub const LOAD_STATE_KEY: Key = Key::F9;
/// Keys with fixed functions; they cannot be bound to a Game Boy button.
pub const RESERVED_KEYS: [Key; 3] = [MENU_KEY, SAVE_STATE_KEY, LOAD_STATE_KEY];

/// Default key per button, in `Button::ALL` order.
const DEFAULT_KEYS: [Key; 8] = [
    Key::ArrowRight,
    Key::ArrowLeft,
    Key::ArrowUp,
    Key::ArrowDown,
    Key::Z,
    Key::X,
    Key::Backspace,
    Key::Enter,
];

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Master volume, 0-100.
    pub volume: u8,
    pub muted: bool,
    /// Integer window scale, MIN_SCALE..=MAX_SCALE.
    pub scale: u8,
    pub keys: KeyBindings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 100,
            muted: false,
            scale: 3,
            keys: KeyBindings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyBindings([Key; 8]);

impl Default for KeyBindings {
    fn default() -> Self {
        Self(DEFAULT_KEYS)
    }
}

impl KeyBindings {
    pub fn key(&self, button: Button) -> Key {
        self.0[button as usize]
    }

    /// Bind `key` to `button`. If another button already used `key`, it
    /// takes over `button`'s previous key, so no binding is lost.
    pub fn set(&mut self, button: Button, key: Key) {
        let previous = self.key(button);
        if let Some(other) = self.0.iter().position(|&k| k == key) {
            self.0[other] = previous;
        }
        self.0[button as usize] = key;
    }
}

pub fn button_label(button: Button) -> &'static str {
    match button {
        Button::Right => "Right",
        Button::Left => "Left",
        Button::Up => "Up",
        Button::Down => "Down",
        Button::A => "A",
        Button::B => "B",
        Button::Select => "Select",
        Button::Start => "Start",
    }
}

/// On-disk shape. Every field is optional so partial files still load;
/// numbers are wide so out-of-range values can be clamped, not rejected.
#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    volume: Option<i64>,
    muted: Option<bool>,
    scale: Option<i64>,
    #[serde(default)]
    keys: KeysFile,
}

#[derive(Default, Serialize, Deserialize)]
struct KeysFile {
    up: Option<String>,
    down: Option<String>,
    left: Option<String>,
    right: Option<String>,
    a: Option<String>,
    b: Option<String>,
    start: Option<String>,
    select: Option<String>,
}

impl KeysFile {
    fn slot(&mut self, button: Button) -> &mut Option<String> {
        match button {
            Button::Right => &mut self.right,
            Button::Left => &mut self.left,
            Button::Up => &mut self.up,
            Button::Down => &mut self.down,
            Button::A => &mut self.a,
            Button::B => &mut self.b,
            Button::Select => &mut self.select,
            Button::Start => &mut self.start,
        }
    }
}

impl Settings {
    /// Parse TOML text. Returns the settings plus a warning for every value
    /// that had to be replaced; Err only if the text is not valid TOML.
    pub fn from_toml(text: &str) -> Result<(Self, Vec<String>), toml::de::Error> {
        let mut file: SettingsFile = toml::from_str(text)?;
        let defaults = Settings::default();
        let mut warnings = Vec::new();

        let volume = match file.volume {
            None => defaults.volume,
            Some(v) if (0..=100).contains(&v) => v as u8,
            Some(v) => {
                let clamped = v.clamp(0, 100) as u8;
                warnings.push(format!("volume {v} out of range 0-100; using {clamped}"));
                clamped
            }
        };
        let scale = match file.scale {
            None => defaults.scale,
            Some(s) if (MIN_SCALE as i64..=MAX_SCALE as i64).contains(&s) => s as u8,
            Some(s) => {
                let clamped = s.clamp(MIN_SCALE as i64, MAX_SCALE as i64) as u8;
                warnings.push(format!(
                    "scale {s} out of range {MIN_SCALE}-{MAX_SCALE}; using {clamped}"
                ));
                clamped
            }
        };
        let mut keys = KeyBindings::default();
        for button in Button::ALL {
            let Some(name) = file.keys.slot(button).take() else {
                continue;
            };
            match Key::from_name(&name) {
                Some(key) if !RESERVED_KEYS.contains(&key) => keys.0[button as usize] = key,
                Some(_) => warnings.push(format!(
                    "{} cannot use {name} (reserved); using default",
                    button_label(button)
                )),
                None => warnings.push(format!(
                    "unknown key \"{name}\" for {}; using default",
                    button_label(button)
                )),
            }
        }

        let settings = Settings {
            volume,
            muted: file.muted.unwrap_or(defaults.muted),
            scale,
            keys,
        };
        Ok((settings, warnings))
    }

    pub fn to_toml(&self) -> String {
        let mut keys = KeysFile::default();
        for button in Button::ALL {
            *keys.slot(button) = Some(self.keys.key(button).name().to_string());
        }
        let file = SettingsFile {
            volume: Some(self.volume as i64),
            muted: Some(self.muted),
            scale: Some(self.scale as i64),
            keys,
        };
        toml::to_string_pretty(&file).expect("settings serialize")
    }

    /// Load from `path`, never failing. Returns warnings to show the user.
    pub fn load(path: &Path) -> (Self, Vec<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Self::default(), Vec::new()),
            Err(e) => {
                return (
                    Self::default(),
                    vec![format!(
                        "cannot read {}: {e}; using defaults",
                        path.display()
                    )],
                );
            }
        };
        match Self::from_toml(&text) {
            Ok(result) => result,
            Err(err) => {
                let backup = with_suffix(path, ".bak");
                let kept = match std::fs::copy(path, &backup) {
                    Ok(_) => format!("a copy was kept at {}", backup.display()),
                    Err(e) => format!("backing it up failed: {e}"),
                };
                let reason = err.message().trim().to_string();
                (
                    Self::default(),
                    vec![format!(
                        "{} is not valid ({reason}); using defaults, {kept}",
                        path.display()
                    )],
                )
            }
        }
    }

    /// Write to `path` atomically, creating parent directories.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = with_suffix(path, ".tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(&tmp, path)
    }
}

/// The settings file location, e.g. `~/.config/gbemulator/settings.toml`
/// on Linux. None if the OS reports no home directory.
pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "GBEmulator")
        .map(|dirs| dirs.config_dir().join("settings.toml"))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gbemu-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested/settings.toml");
        let mut settings = Settings {
            volume: 35,
            muted: true,
            scale: 5,
            keys: KeyBindings::default(),
        };
        settings.keys.set(Button::A, Key::K);
        settings.keys.set(Button::Start, Key::Space);
        settings.save(&path).unwrap();

        let (loaded, warnings) = Settings::load(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded, settings);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_gives_defaults_silently() {
        let (settings, warnings) = Settings::load(&temp_dir("missing").join("settings.toml"));
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn corrupt_file_gives_defaults_and_is_backed_up() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, "volume = [this is not toml").unwrap();

        let (settings, warnings) = Settings::load(&path);
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.toml.bak")).unwrap(),
            "volume = [this is not toml"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let (settings, warnings) = Settings::from_toml("volume = 40\n[keys]\nb = \"Q\"\n").unwrap();
        assert!(warnings.is_empty());
        assert_eq!(settings.volume, 40);
        assert_eq!(settings.scale, Settings::default().scale);
        assert_eq!(settings.keys.key(Button::B), Key::Q);
        assert_eq!(settings.keys.key(Button::A), Key::Z);
    }

    #[test]
    fn bad_values_fall_back_with_warnings() {
        let text =
            "volume = 250\nscale = 0\n[keys]\na = \"NotAKey\"\nstart = \"Escape\"\nb = \"F5\"\n";
        let (settings, warnings) = Settings::from_toml(text).unwrap();
        assert_eq!(settings.volume, 100);
        assert_eq!(settings.scale, MIN_SCALE);
        assert_eq!(settings.keys.key(Button::A), Key::Z);
        assert_eq!(settings.keys.key(Button::Start), Key::Enter);
        assert_eq!(settings.keys.key(Button::B), Key::X);
        assert_eq!(warnings.len(), 5, "{warnings:?}");
    }

    #[test]
    fn rebinding_to_a_used_key_swaps() {
        let mut keys = KeyBindings::default();
        keys.set(Button::A, Key::X); // X was B's key
        assert_eq!(keys.key(Button::A), Key::X);
        assert_eq!(keys.key(Button::B), Key::Z);
    }
}
