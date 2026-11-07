//! Save-state round trips: a restored machine must continue exactly like the
//! original. Uses Blargg's cpu_instrs (MBC1, exercises CPU/timer/serial)
//! and dmg-acid2 (PPU) from `test-roms/`.

use std::path::PathBuf;

use gb_core::{GameBoy, StateError};

fn rom(path: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../test-roms")
        .join(path);
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing {}: {e}", path.display()))
}

fn boot(rom: &[u8]) -> GameBoy {
    let mut gb = GameBoy::new(rom.to_vec()).unwrap();
    gb.set_audio_sample_rate(48_000);
    gb
}

fn run(gb: &mut GameBoy, frames: u32) -> Vec<f32> {
    let mut audio = Vec::new();
    for _ in 0..frames {
        gb.run_frame();
        gb.drain_audio(&mut audio);
    }
    audio
}

/// Run `before` frames, snapshot, then check that the original and a fresh
/// machine restored from the snapshot produce identical output for `after`.
fn assert_deterministic_resume(rom: &[u8], before: u32, after: u32) {
    let mut original = boot(rom);
    run(&mut original, before);
    let state = original.save_state();
    let serial_mark = original.serial_output().len();

    let mut restored = boot(rom);
    restored.load_state(&state).unwrap();
    assert_eq!(restored.cycles(), original.cycles());

    let audio_a = run(&mut original, after);
    let audio_b = run(&mut restored, after);
    assert_eq!(restored.cycles(), original.cycles());
    assert_eq!(restored.framebuffer(), original.framebuffer());
    assert_eq!(
        restored.serial_output(),
        &original.serial_output()[serial_mark..]
    );
    assert!(audio_a == audio_b, "audio diverged after restore");
    assert_eq!(restored.save_state(), original.save_state());
}

#[test]
fn cpu_instrs_resumes_identically() {
    assert_deterministic_resume(&rom("cpu_instrs/cpu_instrs.gb"), 600, 900);
}

#[test]
fn acid2_resumes_identically() {
    assert_deterministic_resume(&rom("dmg-acid2/dmg-acid2.gb"), 30, 60);
}

#[test]
fn rejects_state_from_another_game() {
    let mut a = boot(&rom("dmg-acid2/dmg-acid2.gb"));
    let b = boot(&rom("cpu_instrs/cpu_instrs.gb"));
    assert_eq!(a.load_state(&b.save_state()), Err(StateError::WrongGame));
}

#[test]
fn rejects_garbage_and_truncation_without_changing_anything() {
    let rom = rom("cpu_instrs/cpu_instrs.gb");
    let mut gb = boot(&rom);
    run(&mut gb, 120);
    let state = gb.save_state();
    run(&mut gb, 10);
    let before = gb.save_state();

    assert_eq!(gb.load_state(b"hello"), Err(StateError::NotAState));
    assert_eq!(
        gb.load_state(&state[..state.len() / 2]),
        Err(StateError::Truncated)
    );
    let mut trailing = state.clone();
    trailing.push(0);
    assert!(matches!(
        gb.load_state(&trailing),
        Err(StateError::Invalid(_))
    ));
    let mut bad_version = state.clone();
    bad_version[4] = 99;
    assert_eq!(
        gb.load_state(&bad_version),
        Err(StateError::UnsupportedVersion(99))
    );

    assert_eq!(
        gb.save_state(),
        before,
        "failed loads must not change state"
    );
}
