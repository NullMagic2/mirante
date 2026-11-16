//! Module: mirante_color::curve
//! Purpose: ICC tone response curves and fast lookup tables for them.
//! Created: 2026-11-16
//! Architecture: Curves are evaluated exactly to fill tables once per transform;
//! per-pixel work only interpolates. Values outside 0..1 are evaluated exactly,
//! so linear wide-gamut data survives the round trip.

/// Entries per lookup table; ample for 16-bit sources once interpolated.
const TABLE_SIZE: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub enum Curve {
    Linear,
    Gamma(f32),
    /// ICC parametric curve: function type 0..=4 and its parameters g, a, b, c, d, e, f.
    Parametric {
        function: u16,
        params: [f32; 7],
    },
    /// Sampled curve with output in 0..1, input spread evenly over 0..1.
    Table(Vec<f32>),
}

impl Curve {
    pub fn srgb() -> Self {
        let params = [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045, 0.0, 0.0];
        Self::Parametric { function: 3, params }
    }

    pub fn evaluate(&self, x: f32) -> f32 {
        // Odd symmetry keeps negative (out-of-gamut) values meaningful.
        if x < 0.0 && !matches!(self, Self::Linear) {
            return -self.evaluate(-x);
        }
        match self {
            Self::Linear => x,
            Self::Gamma(gamma) => x.powf(*gamma),
            Self::Parametric { function, params } => parametric(*function, params, x),
            Self::Table(table) => interpolate(table, x),
        }
    }

    /// A table for the inverse curve, found by bisection on the forward curve.
    pub fn inverse_table(&self) -> Vec<f32> {
        (0..TABLE_SIZE)
            .map(|i| {
                let target = i as f32 / (TABLE_SIZE - 1) as f32;
                let (mut low, mut high) = (0.0_f32, 1.0_f32);
                for _ in 0..24 {
                    let middle = (low + high) / 2.0;
                    if self.evaluate(middle) < target { low = middle } else { high = middle }
                }
                (low + high) / 2.0
            })
            .collect()
    }
}

fn parametric(function: u16, p: &[f32; 7], x: f32) -> f32 {
    let [g, a, b, c, d, e, f] = *p;
    let power = |base: f32| if base > 0.0 { base.powf(g) } else { 0.0 };
    match function {
        0 => power(x),
        1 => {
            if x >= -b / a {
                power(a * x + b)
            } else {
                0.0
            }
        }
        2 => {
            if x >= -b / a {
                power(a * x + b) + c
            } else {
                c
            }
        }
        3 => {
            if x >= d {
                power(a * x + b)
            } else {
                c * x
            }
        }
        _ => {
            if x >= d {
                power(a * x + b) + e
            } else {
                c * x + f
            }
        }
    }
}

fn interpolate(table: &[f32], x: f32) -> f32 {
    let last = table.len() - 1;
    let position = x.clamp(0.0, 1.0) * last as f32;
    let index = (position as usize).min(last.saturating_sub(1));
    let fraction = position - index as f32;
    let next = table[(index + 1).min(last)];
    table[index] + (next - table[index]) * fraction
}

/// A curve flattened to a table for speed, with an exact path outside 0..1.
pub struct Lookup {
    table: Vec<f32>,
    /// The exact curve for input outside 0..1; None clips instead.
    exact: Option<Curve>,
}

impl Lookup {
    pub fn forward(curve: &Curve) -> Self {
        let table =
            (0..TABLE_SIZE).map(|i| curve.evaluate(i as f32 / (TABLE_SIZE - 1) as f32)).collect();
        Self { table, exact: Some(curve.clone()) }
    }

    /// Inverse lookup; out-of-range input is clipped, as for a display or file.
    pub fn inverse(curve: &Curve) -> Self {
        Self { table: curve.inverse_table(), exact: None }
    }

    pub fn apply(&self, x: f32) -> f32 {
        match &self.exact {
            Some(curve) if !(0.0..=1.0).contains(&x) => curve.evaluate(x),
            _ => interpolate(&self.table, x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_matches_known_values() {
        let srgb = Curve::srgb();
        assert!((srgb.evaluate(0.5) - 0.214).abs() < 1e-3);
        assert!((srgb.evaluate(0.02) - 0.02 / 12.92).abs() < 1e-6);
    }

    #[test]
    fn inverse_table_round_trips() {
        let inverse = Lookup::inverse(&Curve::Gamma(2.2));
        assert!((inverse.apply(0.5_f32.powf(2.2)) - 0.5).abs() < 1e-3);
    }
}
