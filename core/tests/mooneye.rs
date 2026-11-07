//! Mooneye test suite MBC tests (emulator-only/mbc1, mbc5). Place them under
//! `test-roms/mooneye/`. A passing test sends the Fibonacci bytes
//! 3 5 8 13 21 34 over serial; a failing one sends 0x42 six times.

use std::path::PathBuf;

use gb_core::GameBoy;

const PASS: [u8; 6] = [3, 5, 8, 13, 21, 34];
const MAX_FRAMES: u32 = 60 * 20;

fn run(dir: &str, name: &str) -> Result<(), String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../test-roms/mooneye")
        .join(dir)
        .join(name);
    let rom = std::fs::read(&path).unwrap_or_else(|e| panic!("missing {}: {e}", path.display()));
    let mut gb = GameBoy::new(rom).map_err(|e| e.to_string())?;
    for _ in 0..MAX_FRAMES {
        gb.run_frame();
        if gb.serial_output().len() >= 6 {
            break;
        }
    }
    match gb.serial_output() {
        out if out.starts_with(&PASS) => Ok(()),
        out => Err(format!("serial output {out:?}")),
    }
}

fn run_all(dir: &str, names: &[&str]) {
    let failures: Vec<String> = names
        .iter()
        .filter_map(|name| run(dir, name).err().map(|e| format!("{dir}/{name}: {e}")))
        .collect();
    assert!(failures.is_empty(), "failed:\n{}", failures.join("\n"));
}

#[test]
fn mbc1() {
    run_all(
        "mbc1",
        &[
            "bits_bank1.gb",
            "bits_bank2.gb",
            "bits_mode.gb",
            "bits_ramg.gb",
            "ram_64kb.gb",
            "ram_256kb.gb",
            "rom_512kb.gb",
            "rom_1Mb.gb",
            "rom_2Mb.gb",
            "rom_4Mb.gb",
            "rom_8Mb.gb",
            "rom_16Mb.gb",
        ],
    );
}

#[test]
fn mbc5() {
    run_all(
        "mbc5",
        &[
            "rom_512kb.gb",
            "rom_1Mb.gb",
            "rom_2Mb.gb",
            "rom_4Mb.gb",
            "rom_8Mb.gb",
            "rom_16Mb.gb",
            "rom_32Mb.gb",
            "rom_64Mb.gb",
        ],
    );
}
