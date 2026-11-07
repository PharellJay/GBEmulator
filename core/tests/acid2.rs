//! dmg-acid2 rendering test. Place `dmg-acid2.gb` and `reference-dmg.png`
//! (from github.com/mattcurrie/dmg-acid2) under `test-roms/dmg-acid2/`.

use std::path::PathBuf;

use gb_core::{GameBoy, SCREEN_HEIGHT, SCREEN_WIDTH};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-roms/dmg-acid2")
}

/// Reference image as shades 0 (white) to 3 (black).
fn reference() -> Vec<u8> {
    let path = dir().join("reference-dmg.png");
    let file =
        std::fs::File::open(&path).unwrap_or_else(|e| panic!("missing {}: {e}", path.display()));
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (160, 144));
    let channels = info.color_type.samples();
    buf.chunks(channels)
        .take(SCREEN_WIDTH * SCREEN_HEIGHT)
        .map(|px| 3 - px[0] / 0x55)
        .collect()
}

#[test]
fn dmg_acid2_matches_reference() {
    let path = dir().join("dmg-acid2.gb");
    let rom = std::fs::read(&path).unwrap_or_else(|e| panic!("missing {}: {e}", path.display()));
    let mut gb = GameBoy::new(rom).unwrap();
    // The test settles well within a second.
    for _ in 0..120 {
        gb.run_frame();
    }
    let expected = reference();
    assert!(
        (0..4).all(|shade| expected.contains(&shade)),
        "reference image should use all four shades"
    );
    let actual = gb.framebuffer();
    let diffs: Vec<(usize, usize)> = (0..actual.len())
        .filter(|&i| actual[i] != expected[i])
        .map(|i| (i % SCREEN_WIDTH, i / SCREEN_WIDTH))
        .collect();
    if !diffs.is_empty() {
        // Dump the frame as PGM for inspection.
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("acid2-actual.pgm");
        let mut pgm = format!("P5 {SCREEN_WIDTH} {SCREEN_HEIGHT} 255\n").into_bytes();
        pgm.extend(actual.iter().map(|&s| 255 - s * 85));
        std::fs::write(&out, pgm).unwrap();
        panic!(
            "{} pixels differ; first at {:?}; frame written to {}",
            diffs.len(),
            &diffs[..diffs.len().min(10)],
            out.display()
        );
    }
}
