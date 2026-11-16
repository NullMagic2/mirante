//! Module: mirante_color::tests::system_profiles
//! Purpose: Parses the ICC profiles colord installs, when they are present.
//! Created: 2026-11-16
//! Architecture: Skips silently on systems without colord's profile set.

use mirante_color::Profile;
use std::{fs, path::Path};

const COLORD_PROFILES: &str = "/usr/share/color/icc/colord";

fn load(name: &str) -> Option<Vec<u8>> {
    fs::read(Path::new(COLORD_PROFILES).join(name)).ok()
}

#[test]
fn colord_srgb_matches_the_built_in_srgb() {
    let Some(bytes) = load("sRGB.icc") else { return };
    let parsed = Profile::from_icc(&bytes).unwrap();
    let built_in = Profile::srgb();
    let pairs = parsed.to_pcs.iter().flatten().zip(built_in.to_pcs.iter().flatten());
    assert!(pairs.clone().all(|(a, b)| (a - b).abs() < 2e-3), "{:?}", parsed.to_pcs);
    let mid = parsed.curves[1].evaluate(0.5);
    assert!((mid - built_in.curves[1].evaluate(0.5)).abs() < 2e-3);
}

#[test]
fn wide_gamut_rgb_profiles_parse() {
    for name in ["AdobeRGB1998.icc", "ProPhotoRGB.icc", "Rec709.icc", "WideGamutRGB.icc"] {
        let Some(bytes) = load(name) else { continue };
        assert!(Profile::from_icc(&bytes).is_ok(), "{name}");
    }
}

#[test]
fn cmyk_profiles_are_reported_unsupported() {
    let Some(bytes) = load("FOGRA39L_coated.icc") else { return };
    assert!(Profile::from_icc(&bytes).is_err());
}
