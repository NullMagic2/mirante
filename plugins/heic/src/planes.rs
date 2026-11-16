//! Module: mirante_format_heic::planes
//! Purpose: Decoded YCbCr sample planes, the output contract of video-coded items.
//! Created: 2026-11-16
//! Architecture: Produced by hevc::decode_intra (and a future AV1 decoder);
//! ycbcr.rs turns them into RGB. Constructors validate every plane length.

use mirante_core::DecodeError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaFormat {
    Monochrome,
    Yuv420,
    Yuv422,
    Yuv444,
}

impl ChromaFormat {
    /// Maps HEVC/AV1 chroma_format_idc (0..=3).
    pub fn from_idc(idc: u8) -> Result<Self, DecodeError> {
        Ok(match idc {
            0 => Self::Monochrome,
            1 => Self::Yuv420,
            2 => Self::Yuv422,
            3 => Self::Yuv444,
            _ => return Err(DecodeError::corrupt(format!("chroma format {idc}"))),
        })
    }

    /// Horizontal and vertical chroma subsampling as shifts.
    pub fn shifts(self) -> (u32, u32) {
        match self {
            Self::Yuv420 => (1, 1),
            Self::Yuv422 => (1, 0),
            Self::Monochrome | Self::Yuv444 => (0, 0),
        }
    }
}

/// One component's samples, row by row, at most 16 bits each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub samples: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planes {
    pub format: ChromaFormat,
    pub luma: Plane,
    /// Empty for monochrome pictures.
    pub chroma: Option<[Plane; 2]>,
}

impl Plane {
    pub fn new(
        width: u32,
        height: u32,
        bit_depth: u8,
        samples: Vec<u16>,
    ) -> Result<Self, DecodeError> {
        let expected = DecodeError::check_size(width, height)?;
        if samples.len() != expected || !(1..=16).contains(&bit_depth) {
            return Err(DecodeError::corrupt("decoded plane has the wrong size"));
        }
        Ok(Self { width, height, bit_depth, samples })
    }

    pub fn get(&self, x: u32, y: u32) -> u16 {
        self.samples[y as usize * self.width as usize + x as usize]
    }

    pub fn max_value(&self) -> f32 {
        ((1_u32 << self.bit_depth) - 1) as f32
    }
}

impl Planes {
    /// Checks that the chroma planes match the luma size for the format.
    pub fn new(
        format: ChromaFormat,
        luma: Plane,
        chroma: Option<[Plane; 2]>,
    ) -> Result<Self, DecodeError> {
        let (sx, sy) = format.shifts();
        let expected = (luma.width.div_ceil(1 << sx), luma.height.div_ceil(1 << sy));
        let consistent = match (&chroma, format) {
            (None, ChromaFormat::Monochrome) => true,
            (Some(planes), format) if format != ChromaFormat::Monochrome => {
                planes.iter().all(|p| (p.width, p.height) == expected)
            }
            _ => false,
        };
        if !consistent {
            return Err(DecodeError::corrupt("chroma planes do not match the luma plane"));
        }
        Ok(Self { format, luma, chroma })
    }

    pub fn width(&self) -> u32 {
        self.luma.width
    }

    pub fn height(&self) -> u32 {
        self.luma.height
    }
}
