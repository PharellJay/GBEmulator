//! Headless Blargg test-ROM runs. The ROMs are not distributed with the repo;
//! place them under `test-roms/` at the workspace root.

use std::path::PathBuf;

use gb_core::GameBoy;

/// About 115 emulated seconds; cpu_instrs needs roughly 55.
const MAX_MCYCLES: u64 = 120_000_000;

fn load(candidates: &[&str]) -> Vec<u8> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-roms");
    for name in candidates {
        if let Ok(bytes) = std::fs::read(root.join(name)) {
            return bytes;
        }
    }
    panic!(
        "test ROM missing: place one of {candidates:?} under {}",
        root.display()
    );
}

/// Run until the serial output reports a result; return the full output.
fn run_serial(rom: Vec<u8>) -> String {
    let mut gb = GameBoy::new(rom).expect("ROM loads");
    let mut deadline = MAX_MCYCLES;
    while gb.cycles() < deadline {
        gb.run_frame();
        let out = String::from_utf8_lossy(gb.serial_output());
        if out.contains("Passed") {
            break;
        }
        if out.contains("Failed") && deadline == MAX_MCYCLES {
            // Let the ROM finish printing failure details.
            deadline = gb.cycles() + 2_000_000;
        }
    }
    String::from_utf8_lossy(gb.serial_output()).into_owned()
}

#[test]
fn cpu_instrs() {
    let out = run_serial(load(&["cpu_instrs.gb", "cpu_instrs/cpu_instrs.gb"]));
    println!("{out}");
    for n in 1..=11 {
        assert!(
            out.contains(&format!("{n:02}:ok")),
            "sub-test {n:02} failed:\n{out}"
        );
    }
    assert!(out.contains("Passed"), "serial output:\n{out}");
}

#[test]
fn instr_timing() {
    let out = run_serial(load(&["instr_timing.gb", "instr_timing/instr_timing.gb"]));
    println!("{out}");
    assert!(out.contains("Passed"), "serial output:\n{out}");
}

#[test]
fn mem_timing() {
    let out = run_serial(load(&["mem_timing.gb", "mem_timing/mem_timing.gb"]));
    println!("{out}");
    assert!(out.contains("Passed"), "serial output:\n{out}");
}

/// Run a ROM that reports through cartridge RAM at 0xA000 (signature
/// DE B0 61 at 0xA001); returns (result code, text output).
fn run_memory_protocol(rom: Vec<u8>) -> (u8, String) {
    let mut gb = GameBoy::new(rom).expect("ROM loads");
    while gb.cycles() < MAX_MCYCLES {
        gb.run_frame();
        let ram = gb.save_data(0);
        if ram.len() > 4 && ram[1..4] == [0xDE, 0xB0, 0x61] && ram[0] != 0x80 {
            break;
        }
    }
    let ram = gb.save_data(0);
    let text: Vec<u8> = ram[4..].iter().copied().take_while(|&b| b != 0).collect();
    (ram[0], String::from_utf8_lossy(&text).into_owned())
}

fn dmg_sound(names: &[&str]) {
    let mut failures = Vec::new();
    for name in names {
        let (code, text) =
            run_memory_protocol(load(&[&format!("dmg_sound/rom_singles/{name}.gb")]));
        println!("{name}: code {code}\n{text}");
        if code != 0 {
            failures.push(format!("{name}: code {code}: {}", text.trim()));
        }
    }
    assert!(failures.is_empty(), "failed:\n{}", failures.join("\n"));
}

#[test]
fn dmg_sound_01_to_03() {
    dmg_sound(&["01-registers", "02-len ctr", "03-trigger"]);
}

/// Beyond the M4 gate; run with `cargo test -- --ignored`.
#[test]
#[ignore = "beyond the M4 gate"]
fn dmg_sound_04_to_12() {
    dmg_sound(&[
        "04-sweep",
        "05-sweep details",
        "06-overflow on trigger",
        "07-len sweep period sync",
        "08-len ctr during power",
        "09-wave read while on",
        "10-wave trigger while on",
        "11-regs after power",
        "12-wave write while on",
    ]);
}
