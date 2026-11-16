//! Module: mirante_color::matrix
//! Purpose: 3x3 matrix maths for RGB/XYZ conversions and white-point adaptation.
//! Created: 2026-11-16
//! Architecture: Computed in f64 once per transform; per-pixel work uses f32.
//! Public so the RAW pipeline shares these helpers instead of copying them.

pub type Mat3 = [[f64; 3]; 3];
pub type Vec3 = [f64; 3];

/// CIE chromaticities (x, y) of the illuminants we need.
pub const D50: (f64, f64) = (0.3457, 0.3585);
pub const D65: (f64, f64) = (0.3127, 0.3290);

const BRADFORD: Mat3 =
    [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];

pub fn multiply(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, value) in out_row.iter_mut().enumerate() {
            *value = (0..3).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

pub fn diagonal(v: &Vec3) -> Mat3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

pub fn apply(m: &Mat3, v: Vec3) -> Vec3 {
    m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

pub fn inverse(m: &Mat3) -> Option<Mat3> {
    let cofactor =
        |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let adjugate = [
        [cofactor(1, 2, 1, 2), -cofactor(0, 2, 1, 2), cofactor(0, 1, 1, 2)],
        [-cofactor(1, 2, 0, 2), cofactor(0, 2, 0, 2), -cofactor(0, 1, 0, 2)],
        [cofactor(1, 2, 0, 1), -cofactor(0, 2, 0, 1), cofactor(0, 1, 0, 1)],
    ];
    let determinant: f64 = (0..3).map(|k| m[0][k] * adjugate[k][0]).sum();

    if determinant.abs() < 1e-12 {
        return None;
    }
    Some(adjugate.map(|row| row.map(|v| v / determinant)))
}

pub fn xy_to_xyz((x, y): (f64, f64)) -> Vec3 {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Bradford chromatic adaptation between two white points given as XYZ.
pub fn adaptation(from: Vec3, to: Vec3) -> Mat3 {
    let source = apply(&BRADFORD, from);
    let target = apply(&BRADFORD, to);
    let scale = [
        [target[0] / source[0], 0.0, 0.0],
        [0.0, target[1] / source[1], 0.0],
        [0.0, 0.0, target[2] / source[2]],
    ];
    let inverse_bradford = inverse(&BRADFORD).expect("the Bradford matrix is invertible");
    multiply(&inverse_bradford, &multiply(&scale, &BRADFORD))
}

/// Linear RGB to XYZ relative to D50, the ICC connection space.
pub fn rgb_to_pcs(primaries: [(f64, f64); 3], white: (f64, f64)) -> Mat3 {
    let columns = primaries.map(xy_to_xyz);
    let unscaled: Mat3 = [0, 1, 2].map(|row| columns.map(|column| column[row]));
    let white_xyz = xy_to_xyz(white);
    let scale = apply(&inverse(&unscaled).expect("primaries are independent"), white_xyz);
    let native = unscaled.map(|row| [row[0] * scale[0], row[1] * scale[1], row[2] * scale[2]]);
    multiply(&adaptation(white_xyz, xy_to_xyz(D50)), &native)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    #[test]
    fn inverse_round_trips() {
        let m = [[2.0, 1.0, 0.0], [0.0, 1.0, 3.0], [1.0, 0.0, 1.0]];
        let product = multiply(&m, &inverse(&m).unwrap());
        for (row, identity_row) in product.iter().zip(IDENTITY) {
            assert!(row.iter().zip(identity_row).all(|(a, b)| (a - b).abs() < 1e-12));
        }
    }

    #[test]
    fn white_maps_to_d50() {
        let srgb = rgb_to_pcs([(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)], D65);
        let white = apply(&srgb, [1.0; 3]);
        let d50 = xy_to_xyz(D50);
        assert!(white.iter().zip(d50).all(|(a, b)| (a - b).abs() < 1e-3));
    }
}
