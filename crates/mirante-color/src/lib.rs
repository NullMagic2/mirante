//! Module: mirante_color
//! Purpose: Moves images between file, working, display and export colour spaces.
//! Created: 2026-11-16
//! Architecture: All editing happens in linear Rec. 2020 so wide-gamut sources
//! (RAW, Display P3, Adobe RGB) keep their colours until the very end. Every
//! conversion is curve → 3x3 matrix through D50 XYZ → inverse curve.

mod curve;
mod icc;
pub mod matrix;

pub use curve::Curve;
pub use icc::Profile;

use curve::Lookup;
use matrix::{D65, inverse, multiply, rgb_to_pcs};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba, parallel};

const SRGB_PRIMARIES: [(f64, f64); 3] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];
const REC2020_PRIMARIES: [(f64, f64); 3] = [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)];
const DISPLAY_P3_PRIMARIES: [(f64, f64); 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
const ADOBE_RGB_PRIMARIES: [(f64, f64); 3] = [(0.64, 0.33), (0.21, 0.71), (0.15, 0.06)];
/// Adobe RGB (1998)'s gamma, 2 51/256.
const ADOBE_RGB_GAMMA: f32 = 563.0 / 256.0;
/// BT.1886's reference display gamma, used for the Rec. 2020 preset.
const BT1886_GAMMA: f32 = 2.4;

/// Standard display spaces to choose when the monitor's own profile is unknown or unwanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Srgb,
    DisplayP3,
    AdobeRgb,
    Rec2020,
}

impl Preset {
    pub const ALL: [Self; 4] = [Self::Srgb, Self::DisplayP3, Self::AdobeRgb, Self::Rec2020];

    /// Stable identifier for settings files.
    pub fn id(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::DisplayP3 => "display-p3",
            Self::AdobeRgb => "adobe-rgb",
            Self::Rec2020 => "rec2020",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == id)
    }

    /// The standard's own name, the same in every language.
    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::DisplayP3 => "Display P3",
            Self::AdobeRgb => "Adobe RGB (1998)",
            Self::Rec2020 => "Rec. 2020",
        }
    }

    pub fn profile(self) -> Profile {
        let rgb = |primaries, curve: Curve| Profile {
            to_pcs: rgb_to_pcs(primaries, D65),
            curves: [curve.clone(), curve.clone(), curve],
        };
        match self {
            Self::Srgb => Profile::srgb(),
            Self::DisplayP3 => rgb(DISPLAY_P3_PRIMARIES, Curve::srgb()),
            Self::AdobeRgb => rgb(ADOBE_RGB_PRIMARIES, Curve::Gamma(ADOBE_RGB_GAMMA)),
            Self::Rec2020 => rgb(REC2020_PRIMARIES, Curve::Gamma(BT1886_GAMMA)),
        }
    }
}

impl Profile {
    pub fn srgb() -> Self {
        let curve = Curve::srgb();
        Self {
            to_pcs: rgb_to_pcs(SRGB_PRIMARIES, D65),
            curves: [curve.clone(), curve.clone(), curve],
        }
    }

    pub fn linear_srgb() -> Self {
        Self {
            to_pcs: rgb_to_pcs(SRGB_PRIMARIES, D65),
            curves: [Curve::Linear, Curve::Linear, Curve::Linear],
        }
    }

    /// The editing space.
    pub fn linear_rec2020() -> Self {
        Self {
            to_pcs: rgb_to_pcs(REC2020_PRIMARIES, D65),
            curves: [Curve::Linear, Curve::Linear, Curve::Linear],
        }
    }

    pub fn from_icc(bytes: &[u8]) -> Result<Self, DecodeError> {
        icc::parse(bytes)
    }

    /// Unreadable embedded profiles fall back to sRGB, as browsers do.
    pub fn for_space(space: &ColorSpace) -> Self {
        match space {
            ColorSpace::Srgb => Self::srgb(),
            ColorSpace::LinearSrgb => Self::linear_srgb(),
            ColorSpace::LinearRec2020 => Self::linear_rec2020(),
            ColorSpace::Icc(bytes) => Self::from_icc(bytes).unwrap_or_else(|error| {
                eprintln!("mirante: using sRGB instead of the embedded profile: {error}");
                Self::srgb()
            }),
        }
    }
}

/// One profile-to-profile conversion, prepared for fast per-pixel use.
struct Transform {
    decode: [Option<Lookup>; 3],
    matrix: [[f32; 3]; 3],
    encode: [Option<Lookup>; 3],
}

impl Transform {
    fn new(source: &Profile, target: &Profile) -> Self {
        let target_inverse = inverse(&target.to_pcs).unwrap_or_else(|| {
            inverse(&Profile::srgb().to_pcs).expect("the sRGB matrix is invertible")
        });
        let matrix = multiply(&target_inverse, &source.to_pcs).map(|row| row.map(|v| v as f32));
        let decode =
            source.curves.each_ref().map(|c| (*c != Curve::Linear).then(|| Lookup::forward(c)));
        let encode =
            target.curves.each_ref().map(|c| (*c != Curve::Linear).then(|| Lookup::inverse(c)));
        Self { decode, matrix, encode }
    }

    fn apply(&self, [r, g, b, a]: Rgba) -> Rgba {
        let curve = |lookup: &Option<Lookup>, v: f32| lookup.as_ref().map_or(v, |l| l.apply(v));
        let linear =
            [curve(&self.decode[0], r), curve(&self.decode[1], g), curve(&self.decode[2], b)];
        let mixed =
            self.matrix.map(|row| row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]);
        let [r, g, b] = [0, 1, 2].map(|i| curve(&self.encode[i], mixed[i]));
        [r, g, b, a]
    }

    fn apply_all(&self, pixels: &mut [Rgba]) {
        parallel::for_each_chunk(pixels, |part| part.iter_mut().for_each(|p| *p = self.apply(*p)));
    }
}

/// Converts a freshly decoded image to the linear Rec. 2020 working space.
pub fn to_working(mut image: Image) -> Image {
    if *image.color_space() == ColorSpace::LinearRec2020 {
        return image;
    }
    let transform =
        Transform::new(&Profile::for_space(image.color_space()), &Profile::linear_rec2020());
    transform.apply_all(image.pixels_mut());
    image.with_color_space(ColorSpace::LinearRec2020)
}

/// Converts working pixels to sRGB in 0..1, ready for an encoder.
pub fn to_export_srgb(mut image: Image) -> Image {
    let transform = Transform::new(&Profile::for_space(image.color_space()), &Profile::srgb());
    transform.apply_all(image.pixels_mut());
    for pixel in image.pixels_mut() {
        *pixel = pixel.map(|v| v.clamp(0.0, 1.0));
    }
    image.with_color_space(ColorSpace::Srgb)
}

/// Working space to monitor, producing Cairo's premultiplied native-endian ARGB32.
pub struct DisplayTransform {
    transform: Transform,
}

impl DisplayTransform {
    /// `monitor` is the display's profile; sRGB when the monitor's is unknown.
    pub fn new(monitor: &Profile) -> Self {
        Self { transform: Transform::new(&Profile::linear_rec2020(), monitor) }
    }

    pub fn to_cairo_argb32(&self, image: &Image) -> Vec<u8> {
        let mut pixels = image.pixels().to_vec();
        self.transform.apply_all(&mut pixels);
        pixels.iter().flat_map(|&pixel| premultiplied_argb32(pixel)).collect()
    }
}

/// Cairo stores ARGB32 as a native-endian u32 with colour premultiplied by alpha.
fn premultiplied_argb32([r, g, b, a]: Rgba) -> [u8; 4] {
    let alpha = a.clamp(0.0, 1.0);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
    let packed = u32::from_be_bytes([(alpha * 255.0).round() as u8, byte(r), byte(g), byte(b)]);
    packed.to_ne_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    fn pixel(space: ColorSpace, value: Rgba) -> Image {
        Image::new(1, 1, vec![value], space).unwrap()
    }

    #[test]
    fn srgb_white_stays_white_in_working_space() {
        let working = to_working(pixel(ColorSpace::Srgb, [1.0; 4]));
        assert!(working.pixels()[0].iter().all(|&v| close(v, 1.0)));
    }

    #[test]
    fn srgb_mid_grey_is_linearised() {
        let working = to_working(pixel(ColorSpace::Srgb, [0.5, 0.5, 0.5, 1.0]));
        assert!(working.pixels()[0][..3].iter().all(|&v| close(v, 0.214)));
    }

    #[test]
    fn srgb_round_trips_through_working_space() {
        let original = [0.9, 0.3, 0.1, 1.0];
        let back = to_export_srgb(to_working(pixel(ColorSpace::Srgb, original)));
        assert!(back.pixels()[0].iter().zip(original).all(|(&a, b)| close(a, b)));
    }

    #[test]
    fn pure_srgb_red_is_inside_rec2020() {
        let working = to_working(pixel(ColorSpace::Srgb, [1.0, 0.0, 0.0, 1.0]));
        assert!(working.pixels()[0][..3].iter().all(|&v| v > 0.0));
    }

    #[test]
    fn embedded_profile_equal_to_srgb_primaries_matches() {
        let columns = [0, 1, 2].map(|c| Profile::srgb().to_pcs.map(|row| row[c]));
        let icc = icc::tests::build_rgb_profile(columns);
        let mut profile = Profile::from_icc(&icc).unwrap();
        assert!(matches!(profile.curves[0], Curve::Gamma(_)));
        profile.curves = Profile::srgb().curves;
        assert!(
            profile
                .to_pcs
                .iter()
                .flatten()
                .zip(Profile::srgb().to_pcs.iter().flatten())
                .all(|(a, b)| (a - b).abs() < 1e-4)
        );
    }

    #[test]
    fn presets_round_trip_their_ids_and_keep_white() {
        for preset in Preset::ALL {
            assert_eq!(Preset::from_id(preset.id()), Some(preset));
            let display = DisplayTransform::new(&preset.profile());
            let white = pixel(ColorSpace::LinearRec2020, [1.0; 4]);
            let argb = display.to_cairo_argb32(&white);
            assert!(argb.iter().all(|&v| v >= 254), "{}", preset.name());
        }
    }

    #[test]
    fn display_output_is_premultiplied() {
        let image = pixel(ColorSpace::LinearRec2020, [1.0, 1.0, 1.0, 0.5]);
        let bytes = DisplayTransform::new(&Profile::srgb()).to_cairo_argb32(&image);
        let argb = u32::from_ne_bytes(bytes[..4].try_into().unwrap()).to_be_bytes();
        assert_eq!(argb[0], 128);
        assert!(argb[1..].iter().all(|&v| v.abs_diff(128) <= 1), "{argb:?}");
    }
}
