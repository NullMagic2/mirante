//! Module: mirante_raw_fuji::tests::samples
//! Purpose: Decodes real RAF files named in the environment; ignored by default.
//! Created: 2026-11-16
//! Architecture: MIRANTE_RAF lists files separated by ':'. If MIRANTE_RAF_REFERENCES
//! names a folder, "<file name>.photosites" there (row-major little-endian u16,
//! e.g. LibRaw's raw_image) is compared sample for sample. No samples live in the repo.

use std::time::Instant;

#[test]
#[ignore = "needs MIRANTE_RAF pointing at RAF samples"]
fn decodes_samples() {
    let paths = std::env::var("MIRANTE_RAF").expect("MIRANTE_RAF names RAF files");
    for path in paths.split(':') {
        let bytes = std::fs::read(path).expect("sample is readable");
        let start = Instant::now();
        let raw = mirante_raw_fuji::decode(&bytes).expect("sample decodes");
        let elapsed = start.elapsed();
        let plane = &raw.plane;
        println!(
            "{path}: {}x{} {}-bit, visible {:?}, black {:?}, white {}, wb {:?}, {} {}, orientation {}, {elapsed:?}",
            plane.width,
            plane.height,
            plane.bits_per_sample,
            raw.visible,
            raw.black.at(0, 0),
            raw.white,
            raw.white_balance,
            raw.make,
            raw.model,
            raw.orientation
        );
        let pattern = raw.pattern.as_ref().expect("RAF is a mosaic");
        let rows: Vec<String> =
            (0..6).map(|y| (0..6).map(|x| pattern.color_at(x, y).to_string()).collect()).collect();
        println!("  pattern from the plane origin: {}", rows.join(" "));
        let Ok(folder) = std::env::var("MIRANTE_RAF_REFERENCES") else {
            continue;
        };
        let name = std::path::Path::new(path).file_name().expect("a file").to_string_lossy();
        let reference = std::fs::read(format!("{folder}/{name}.photosites")).expect("reference");
        let expected: Vec<u16> =
            reference.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        assert_eq!(expected.len(), plane.samples.len(), "{path}: photosite count");
        let differing = expected.iter().zip(&plane.samples).filter(|(a, b)| a != b).count();
        let first = expected.iter().zip(&plane.samples).position(|(a, b)| a != b);
        assert_eq!(differing, 0, "{path}: first difference at {first:?}");
        println!("  photosites match the reference exactly");
    }
}
