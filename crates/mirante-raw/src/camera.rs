//! Module: mirante_raw::camera
//! Purpose: Finds a camera's colour matrix and levels in the dcraw-derived table.
//! Created: 2026-11-16
//! Architecture: Normalises maker and model names the way dcraw does, then takes
//! the first camera_table entry whose prefix matches "<make> <model>". Also
//! averages a maker's matrices into a plausible prior for cameras not listed.

use crate::camera_table::{CAMERAS, CameraEntry};
use crate::color::{self, Matrix, Vector};

/// dcraw's matrices and levels are integers; the matrices are scaled by 10000.
const MATRIX_SCALE: f64 = 10_000.0;
/// Company names dcraw reduces a Make tag to, in its order; the first match wins.
const MAKERS: [&str; 22] = [
    "AgfaPhoto",
    "Canon",
    "Casio",
    "Epson",
    "Fujifilm",
    "Mamiya",
    "Minolta",
    "Motorola",
    "Kodak",
    "Konica",
    "Leica",
    "Nikon",
    "Nokia",
    "Olympus",
    "Ricoh",
    "Pentax",
    "Phase One",
    "Samsung",
    "Sigma",
    "Sinar",
    "Sony",
    "YI",
];

/// What the table knows about one camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraProfile {
    pub black: Option<f32>,
    pub white: Option<f32>,
    /// XYZ (D65) to camera RGB, when the table has a three-colour matrix.
    pub xyz_to_camera: Option<Matrix>,
}

fn contains_ignore_case(text: &str, part: &str) -> bool {
    text.to_ascii_lowercase().contains(&part.to_ascii_lowercase())
}

/// The table's maker name for a Make tag, e.g. "NIKON CORPORATION" to "Nikon".
pub fn normalise_make(make: &str, model: &str) -> String {
    let make = make.trim();
    let model = model.trim();
    let starts = |prefix: &str| model.to_ascii_uppercase().starts_with(prefix);
    if contains_ignore_case(make, "OM Digital") {
        return "Olympus".to_owned();
    }
    // Ricoh Imaging makes Pentax cameras as well as its own GR models.
    if (contains_ignore_case(make, "Ricoh") && !starts("GR")) || starts("PENTAX") {
        return "Pentax".to_owned();
    }
    match MAKERS.iter().find(|name| contains_ignore_case(make, name)) {
        Some(name) => (*name).to_owned(),
        None => make.to_owned(),
    }
}

/// The model without the maker's name or dcraw's dropped words.
pub fn normalise_model(make: &str, model: &str) -> String {
    let mut model = model.trim();
    for prefix in [make, "PENTAX", "Digital Camera"] {
        let lower = model.get(..prefix.len()).map(str::to_ascii_lowercase);
        if lower.is_some_and(|start| start == prefix.to_ascii_lowercase())
            && model[prefix.len()..].starts_with(' ')
        {
            model = model[prefix.len()..].trim_start();
        }
    }
    model.strip_prefix("FinePix ").unwrap_or(model).to_owned()
}

pub fn lookup(make: &str, model: &str) -> Option<&'static CameraEntry> {
    let make = normalise_make(make, model);
    let name = format!("{make} {}", normalise_model(&make, model));
    CAMERAS.iter().find(|entry| name.starts_with(entry.prefix))
}

pub fn profile(make: &str, model: &str) -> Option<CameraProfile> {
    let entry = lookup(make, model)?;
    let level = |value: u16| (value > 0).then_some(f32::from(value));
    Some(CameraProfile {
        black: level(entry.black),
        white: level(entry.maximum),
        xyz_to_camera: matrix(entry),
    })
}

/// None for entries without a matrix and for four-colour (CMYG) cameras.
fn matrix(entry: &CameraEntry) -> Option<Matrix> {
    let m = entry.matrix.map(|v| f64::from(v) / MATRIX_SCALE);
    let three_colour = m[9..].iter().all(|&v| v == 0.0) && m[..9].iter().any(|&v| v != 0.0);
    three_colour.then(|| [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]])
}

/// White-balanced camera to linear Rec. 2020 under D65, and the camera's D65 neutral.
pub fn balanced_d65(xyz_to_camera: &Matrix) -> Option<(Matrix, Vector)> {
    let neutral = color::apply(xyz_to_camera, &color::xy_to_xyz(color::D65_XY));
    let camera_to_xyz = color::invert(xyz_to_camera)?;
    let to_rec2020 = color::multiply(&color::XYZ_TO_REC2020, &camera_to_xyz);
    color::is_positive(&neutral)
        .then(|| (color::multiply(&to_rec2020, &color::diagonal(&neutral)), neutral))
}

/// The mean white-balanced matrix of the maker's cameras (of all cameras if none
/// is listed): a plausible starting point for a camera the table lacks.
pub fn prior(make: &str, model: &str) -> Matrix {
    let maker = format!("{} ", normalise_make(make, model));
    let balanced = |entries: &mut dyn Iterator<Item = &CameraEntry>| {
        let matrices: Vec<Matrix> =
            entries.filter_map(matrix).filter_map(|m| balanced_d65(&m)).map(|(m, _)| m).collect();
        (!matrices.is_empty()).then(|| {
            let count = matrices.len() as f64;
            let sum = |r: usize, c: usize| matrices.iter().map(|m| m[r][c]).sum::<f64>() / count;
            [0, 1, 2].map(|r| [0, 1, 2].map(|c| sum(r, c)))
        })
    };
    balanced(&mut CAMERAS.iter().filter(|entry| entry.prefix.starts_with(&maker)))
        .or_else(|| balanced(&mut CAMERAS.iter()))
        .unwrap_or(color::IDENTITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_maker_names() {
        assert_eq!(normalise_make("NIKON CORPORATION", "NIKON D750"), "Nikon");
        assert_eq!(normalise_make("SONY", "ILCE-7M3"), "Sony");
        assert_eq!(normalise_make("OLYMPUS CORPORATION", "E-M10MarkIII"), "Olympus");
        assert_eq!(normalise_make("OM Digital Solutions", "OM-1"), "Olympus");
        assert_eq!(normalise_make("FUJIFILM", "X-T3"), "Fujifilm");
        assert_eq!(normalise_make("RICOH IMAGING COMPANY, LTD.", "PENTAX K-1"), "Pentax");
        assert_eq!(normalise_make("PENTAX Corporation", "PENTAX K-5"), "Pentax");
        assert_eq!(normalise_make("RICOH IMAGING COMPANY, LTD.", "GR II"), "Ricoh");
        assert_eq!(normalise_make("Panasonic", "DC-G9"), "Panasonic");
        assert_eq!(normalise_model("Nikon", "NIKON D750 "), "D750");
        assert_eq!(normalise_model("Pentax", "PENTAX K-1"), "K-1");
        assert_eq!(normalise_model("Canon", "Canon EOS 80D"), "EOS 80D");
        assert_eq!(normalise_model("Fujifilm", "FinePix X100"), "X100");
    }

    #[test]
    fn looks_up_by_prefix_in_table_order() {
        assert_eq!(lookup("NIKON CORPORATION", "NIKON D750").unwrap().prefix, "Nikon D750");
        assert_eq!(lookup("Nikon", "D7500").unwrap().prefix, "Nikon D7500");
        let olympus = lookup("OLYMPUS IMAGING CORP.", "E-M10 Mark III").unwrap();
        assert_eq!(olympus.prefix, "Olympus E-M10");
        let panasonic = profile("Panasonic", "DC-G9").unwrap();
        assert_eq!((panasonic.black, panasonic.white), (Some(15.0), Some(4095.0)));
        assert!(panasonic.xyz_to_camera.is_some());
        assert!(lookup("Canon", "EOS R6").is_none());
        assert!(profile("Canon", "PowerShot A50").unwrap().xyz_to_camera.is_none());
    }

    #[test]
    fn balanced_matrices_map_white_to_white() {
        let xyz_to_camera = profile("Sony", "ILCE-7M3").unwrap().xyz_to_camera.unwrap();
        let (balanced, _) = balanced_d65(&xyz_to_camera).unwrap();
        for m in [balanced, prior("Canon", "EOS R6")] {
            let white = color::apply(&m, &[1.0; 3]);
            assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-3), "{white:?}");
        }
    }
}
