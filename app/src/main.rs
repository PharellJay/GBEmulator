mod audio;
mod emulator_app;
mod saves;
mod settings;

use std::path::Path;
use std::process::ExitCode;

use gb_core::{GameBoy, SCREEN_HEIGHT, SCREEN_WIDTH};
use settings::Settings;

use emulator_app::{EmulatorApp, window_size};
use saves::{Loaded, SaveFile, StateFile};

fn main() -> ExitCode {
    let Some(rom_path) = std::env::args().nth(1) else {
        eprintln!("usage: app <rom.gb>");
        return ExitCode::FAILURE;
    };
    let rom = match std::fs::read(&rom_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("cannot read {rom_path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut gb = match GameBoy::new(rom) {
        Ok(gb) => gb,
        Err(err) => {
            eprintln!("cannot load {rom_path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    let save = if gb.has_battery() {
        let save = SaveFile::for_rom(Path::new(&rom_path));
        match save.load(&mut gb) {
            Ok(Loaded::Restored) => println!("Loaded save {}", save.path().display()),
            Ok(Loaded::None) => {}
            Ok(Loaded::Rejected { reason, backup }) => eprintln!(
                "warning: ignoring {} ({reason}); a copy was kept at {}",
                save.path().display(),
                backup.display()
            ),
            Err(err) => {
                eprintln!("cannot read {}: {err}", save.path().display());
                return ExitCode::FAILURE;
            }
        }
        Some(save)
    } else {
        None
    };

    let state = StateFile::for_rom(Path::new(&rom_path));

    let title = if gb.title().is_empty() {
        "GBEmulator".to_string()
    } else {
        format!("GBEmulator - {}", gb.title())
    };
    let settings_path = settings::default_path();
    let settings = match &settings_path {
        Some(path) => {
            let (settings, warnings) = Settings::load(path);
            for warning in warnings {
                eprintln!("warning: {warning}");
            }
            settings
        }
        None => {
            eprintln!("warning: no config directory found; settings will not be saved");
            Settings::default()
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size(window_size(settings.scale))
            .with_min_inner_size([SCREEN_WIDTH as f32, SCREEN_HEIGHT as f32]),
        ..Default::default()
    };

    let result = eframe::run_native(
        &title,
        options,
        Box::new(|_cc| {
            Ok(Box::new(EmulatorApp::new(
                gb,
                save,
                state,
                settings,
                settings_path,
            )))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("window error: {err}");
            ExitCode::FAILURE
        }
    }
}
