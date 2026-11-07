# GBEmulator

An original Game Boy (DMG) emulator written in Rust. Load a `.gb` ROM and play it with video, sound, remappable controls, battery saves and save states.

## Requirements

- Rust 1.85 or newer (the project uses the 2024 edition; developed on 1.93)
- On Linux, the ALSA development headers for audio:
  - Debian/Ubuntu: `sudo apt install libasound2-dev`
  - Fedora: `sudo dnf install alsa-lib-devel`

It has only been tested on Linux (X11/Wayland).

## Running

Commands must be run from the project folder, where `Cargo.toml` is:

```sh
cd path/to/GBEmulator
cargo run --release -p app -- path/to/game.gb
```

Keep `--release`. A debug build may be too slow to run at full speed. The first build takes a few minutes.

After building, you can also start the binary directly from anywhere:

```sh
path/to/GBEmulator/target/release/app path/to/game.gb
```

ROMs are not included. Use your own cartridge dumps or homebrew.

## Controls

| Key | Action |
|---|---|
| Arrow keys | D-pad |
| Z / X | A / B |
| Enter / Backspace | Start / Select |
| Esc | Open/close the settings menu (pauses the game) |
| F5 | Save state |
| F9 | Load state |

The game buttons can be rebound in the menu. Esc, F5 and F9 are reserved.

## Settings menu

Press Esc to change:

- **Sound:** volume (0–100%) and mute
- **Display:** window scale (1×–8×)
- **Controls:** click a button, then press the new key. Picking a key another button already uses swaps the two. There is also "Reset controls to defaults".
- **Save state:** save and load buttons

Settings are saved when the menu closes and on exit, to `~/.config/gbemulator/settings.toml` on Linux. If the file is missing or broken, the emulator starts with defaults. A broken file is copied to `settings.toml.bak` first.

## Saves

| File | What | When |
|---|---|---|
| `game.sav` | In-game save (battery-backed cartridge RAM, plus the clock for MBC3 games) | Loaded at start; written every 5 s after the game saves, and on exit |
| `game.state` | Save state, one slot per game | F5 to write, F9 to load |

Both files sit next to the ROM. `.sav` files use the common format (RAM followed by the BGB/VBA-M clock block), so saves from those emulators should work. A `.sav` that doesn't fit the cartridge is not loaded and is kept as `game.sav.<timestamp>.bak`.

If the program is killed instead of closed, up to 5 seconds of in-game save changes can be lost.

## Compatibility

Supported cartridges: ROM only, MBC1, MBC3 (with real-time clock) and MBC5. This covers most of the Game Boy library.

Not supported:

- Game Boy Color-only games
- MBC2, MBC1 multicarts, and rarer mappers. These show an "unsupported cartridge type" error.

Known inaccuracies:

- Each screen line is drawn in one go, so effects that change scroll or palette in the middle of a line don't show.
- Video memory is not locked while the screen is being drawn.
- Reading or writing wave RAM while channel 3 plays is not emulated (Blargg `dmg_sound` 09, 10 and 12 fail).

## Project layout

```
core/   gb_core: the emulator itself (CPU, PPU, APU, timer, cartridges, save states).
        No windowing, audio or file I/O, and no dependencies.
app/    Desktop frontend: window (eframe/egui), audio (cpal), settings, save files.
```

## Tests

```sh
cargo test --workspace
```

Unit tests run as-is. The accuracy tests need test ROMs placed in `test-roms/` (git-ignored). They fail with a "test ROM missing" message if a ROM isn't there.

| Folder | Source | Tests |
|---|---|---|
| `test-roms/cpu_instrs/`, `instr_timing/`, `mem_timing/`, `dmg_sound/` | [retrio/gb-test-roms](https://github.com/retrio/gb-test-roms) (Blargg) | CPU, timing, sound |
| `test-roms/dmg-acid2/` | [mattcurrie/dmg-acid2](https://github.com/mattcurrie/dmg-acid2): `dmg-acid2.gb` from the v1.0 release and `img/reference-dmg.png` | Pixel-exact rendering |
| `test-roms/mooneye/mbc1/`, `mbc5/` | [Mooneye test suite](https://gekkio.fi/files/mooneye-test-suite/), `emulator-only/` folder | Memory bank controllers |

Current results:

- All of Blargg `cpu_instrs`, `instr_timing` and `mem_timing` pass.
- `dmg-acid2` matches its reference image exactly.
- All Mooneye MBC1 and MBC5 tests pass, except the out-of-scope multicart.
- Blargg `dmg_sound` 01–08 and 11 pass.

The sound tests beyond 01–03 are marked ignored. Run them with:

```sh
cargo test --workspace -- --ignored
```

Lint and formatting checks:

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## License

MIT, see [LICENSE](LICENSE).
