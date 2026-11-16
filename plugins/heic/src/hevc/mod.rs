//! Module: mirante_format_heic::hevc
//! Purpose: The HEVC (H.265) intra decoder used for `hvc1` items.
//! Created: 2026-11-16
//! Architecture: The container side hands over the `hvcC` record and the item's
//! length-prefixed NAL units; this module walks the NAL units, decodes the first
//! picture's I slices (ctu.rs), runs deblock.rs and sao.rs, and returns the planes
//! cropped to the conformance window. Main, Main 10, Main Still Picture and the
//! simple RExt tools are supported; colour conversion happens in the plugin.

mod bitstream;
mod cabac;
mod ctu;
mod deblock;
mod intra;
mod params;
mod picture;
mod residual;
mod sao;
mod scaling;
mod scan;
mod slice;
mod transform;

use crate::planes::{ChromaFormat, Plane, Planes};
use bitstream::{Nal, nal};
use ctu::{Carry, SliceDecoder};
use mirante_codec::ByteReader;
use mirante_codec::Endian;
use mirante_core::DecodeError;
use params::{Pps, Sps};
use picture::{NOT_DECODED, Picture};
use slice::SliceHeader;

/// Only version 1 of HEVCDecoderConfigurationRecord exists.
const CONFIGURATION_VERSION: u8 = 1;
/// Byte offset of chroma_format_idc, after the profile, tier and level fields.
const CHROMA_FORMAT_OFFSET: usize = 16;

/// A parameter set (VPS, SPS, PPS or SEI) carried in the `hvcC` box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NalUnit {
    pub nal_type: u8,
    pub data: Vec<u8>,
}

/// The parts of `hvcC` a decoder needs before reading the item's NAL units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HvcConfig {
    pub profile_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    /// Bytes in each NAL unit length prefix of the item data (1, 2 or 4).
    pub nal_length_size: u8,
    pub parameter_sets: Vec<NalUnit>,
}

impl HvcConfig {
    pub fn parse(payload: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = ByteReader::new(payload, Endian::Big);
        if reader.u8()? != CONFIGURATION_VERSION {
            return Err(DecodeError::unsupported("hvcC configuration version"));
        }
        let profile_idc = reader.u8()? & 0x1F;
        let mut header = reader.at(CHROMA_FORMAT_OFFSET)?;
        let chroma_format_idc = header.u8()? & 0b11;
        let bit_depth_luma = (header.u8()? & 0b111) + 8;
        let bit_depth_chroma = (header.u8()? & 0b111) + 8;
        header.skip(2)?;
        let nal_length_size = (header.u8()? & 0b11) + 1;
        if nal_length_size == 3 {
            return Err(DecodeError::corrupt("hvcC NAL length size of 3 bytes"));
        }
        let mut parameter_sets = Vec::new();

        for _ in 0..header.u8()? {
            let nal_type = header.u8()? & 0x3F;
            for _ in 0..header.u16()? {
                let length = usize::from(header.u16()?);
                parameter_sets.push(NalUnit { nal_type, data: header.bytes(length)?.to_vec() });
            }
        }
        Ok(Self {
            profile_idc,
            chroma_format_idc,
            bit_depth_luma,
            bit_depth_chroma,
            nal_length_size,
            parameter_sets,
        })
    }
}

/// Decodes one intra-coded HEVC picture (a still image or a grid tile).
pub fn decode_intra(config: &HvcConfig, data: &[u8]) -> Result<Planes, DecodeError> {
    let mut sets = ParameterSets::default();
    for unit in &config.parameter_sets {
        let nal = Nal::parse(&unit.data)?;
        if nal.layer_id == 0 {
            sets.add(&nal)?;
        }
    }
    let mut state: Option<PictureState> = None;
    for bytes in bitstream::split_length_prefixed(data, config.nal_length_size)? {
        let nal = Nal::parse(bytes)?;
        if nal.layer_id != 0 {
            continue;
        }
        match nal.nal_type {
            // Parameter sets may not change while a picture is being decoded.
            nal::SPS | nal::PPS if state.is_none() => sets.add(&nal)?,
            // The VPS describes layers and timing, nothing an intra decoder needs.
            nal::VPS => {}
            nal_type if is_coded_slice(nal_type) => {
                let previous = state.as_ref().and_then(|s| s.slices.last());
                let header = SliceHeader::parse(&nal.rbsp, nal_type, |id| sets.get(id), previous)?;
                if header.first_in_picture && state.is_some() {
                    break;
                }
                let state = match &mut state {
                    Some(state) => state,
                    None if header.first_in_picture => {
                        let (pps, sps) = sets.get(header.pps_id)?;
                        state.insert(PictureState::new(sps.clone(), pps.clone())?)
                    }
                    None => {
                        return Err(DecodeError::corrupt("HEVC picture without its first slice"));
                    }
                };
                state.decode_slice(header, &nal.rbsp)?;
            }
            _ => {}
        }
    }
    state.ok_or_else(|| DecodeError::corrupt("HEVC item has no coded picture"))?.finish()
}

/// Slice segment NAL types: trailing, leading and IRAP pictures (Table 7-1).
fn is_coded_slice(nal_type: u8) -> bool {
    nal_type <= 9 || (16..=21).contains(&nal_type)
}

#[derive(Default)]
struct ParameterSets {
    sps: Vec<(u32, Sps)>,
    pps: Vec<(u32, Pps)>,
}

impl ParameterSets {
    fn add(&mut self, nal: &Nal) -> Result<(), DecodeError> {
        match nal.nal_type {
            nal::SPS => {
                let sps = Sps::parse(&nal.rbsp)?;
                self.sps.retain(|(id, _)| *id != sps.id);
                self.sps.push((sps.id, sps));
            }
            nal::PPS => {
                let pps = Pps::parse(&nal.rbsp)?;
                self.pps.retain(|(id, _)| *id != pps.id);
                self.pps.push((pps.id, pps));
            }
            _ => {}
        }
        Ok(())
    }

    /// The PPS with this id and the SPS it refers to.
    fn get(&self, pps_id: u32) -> Result<(&Pps, &Sps), DecodeError> {
        let missing = || DecodeError::corrupt("HEVC slice refers to a missing parameter set");
        let (_, pps) = self.pps.iter().find(|(id, _)| *id == pps_id).ok_or_else(missing)?;
        let (_, sps) = self.sps.iter().find(|(id, _)| *id == pps.sps_id).ok_or_else(missing)?;
        Ok((pps, sps))
    }
}

/// The picture under construction and the slice headers that built it.
struct PictureState {
    sps: Sps,
    pps: Pps,
    picture: Picture,
    slices: Vec<SliceHeader>,
    carry: Carry,
}

impl PictureState {
    fn new(sps: Sps, pps: Pps) -> Result<Self, DecodeError> {
        pps.validate(&sps)?;
        let picture = Picture::new(&sps, &pps)?;
        Ok(Self { sps, pps, picture, slices: Vec::new(), carry: Carry::default() })
    }

    fn decode_slice(&mut self, header: SliceHeader, rbsp: &[u8]) -> Result<(), DecodeError> {
        if header.pps_id != self.pps.id {
            return Err(DecodeError::corrupt("HEVC slices of one picture use different PPSs"));
        }
        let index = self.slices.len() as u32;
        self.slices.push(header);
        let header = &self.slices[index as usize];
        let (sps, pps) = (&self.sps, &self.pps);
        SliceDecoder::new(sps, pps, header, index, rbsp, &mut self.picture, &mut self.carry)
            .decode()
    }

    /// Runs the loop filters and crops to the conformance window.
    fn finish(mut self) -> Result<Planes, DecodeError> {
        if self.picture.ctb_slice_address.contains(&NOT_DECODED) {
            return Err(DecodeError::corrupt("HEVC picture is missing slices"));
        }
        let chroma_format = self.sps.chroma_format_idc;
        deblock::apply(&mut self.picture, &self.slices, &self.pps, chroma_format);
        if self.sps.sao {
            sao::apply(&mut self.picture, &self.slices, &self.pps);
        }
        let [left, right, top, bottom] = self.sps.conformance;
        let (sx, sy) = self.picture.chroma_shift;
        let mut planes = self.picture.planes.into_iter().enumerate().map(|(index, plane)| {
            let (sx, sy) = if index == 0 { (0, 0) } else { (sx, sy) };
            let (x0, y0) = ((left >> sx) as usize, (top >> sy) as usize);
            let width = plane.width - ((left + right) >> sx) as usize;
            let height = plane.height - ((top + bottom) >> sy) as usize;
            let samples = (y0..y0 + height)
                .flat_map(|y| {
                    plane.samples[y * plane.width + x0..y * plane.width + x0 + width]
                        .iter()
                        .copied()
                })
                .collect();
            Plane::new(width as u32, height as u32, plane.bit_depth as u8, samples)
        });
        let luma =
            planes.next().ok_or_else(|| DecodeError::corrupt("HEVC picture has no planes"))??;
        let chroma = match (planes.next(), planes.next()) {
            (Some(cb), Some(cr)) => Some([cb?, cr?]),
            _ => None,
        };
        Planes::new(ChromaFormat::from_idc(chroma_format as u8)?, luma, chroma)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turns an Annex-B stream into the hvcC parameter sets and length-prefixed data.
    fn from_annex_b(stream: &[u8]) -> (HvcConfig, Vec<u8>) {
        let mut starts = Vec::new();
        for i in 0..stream.len().saturating_sub(3) {
            if stream[i..i + 3] == [0, 0, 1] {
                starts.push(i + 3);
            }
        }
        let mut config = HvcConfig {
            profile_idc: 1,
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            nal_length_size: 4,
            parameter_sets: Vec::new(),
        };
        let mut data = Vec::new();
        for (index, &start) in starts.iter().enumerate() {
            let mut end = starts.get(index + 1).map_or(stream.len(), |&next| next - 3);
            while end > start && stream[end - 1] == 0 {
                end -= 1;
            }
            let unit = &stream[start..end];
            let nal_type = (unit[0] >> 1) & 0x3F;
            if (nal::VPS..=nal::PPS).contains(&nal_type) {
                config.parameter_sets.push(NalUnit { nal_type, data: unit.to_vec() });
            } else {
                data.extend((unit.len() as u32).to_be_bytes());
                data.extend(unit);
            }
        }
        (config, data)
    }

    /// A 16x16 4:2:0 picture made of one PCM coding unit, cropped to 12x14.
    fn pcm_picture() -> (HvcConfig, Vec<u8>, Vec<u8>) {
        use bitstream::writer::{escape, pack, se, ue};
        let profile = [(0x01, 8), (0x6000_0000, 32), (0x9000_0000, 32), (0, 16), (93, 8)];
        let mut sps = vec![(0, 4), (0, 3), (1, 1)];
        sps.extend(profile);
        sps.extend([ue(0), ue(1), ue(16), ue(16), (1, 1), ue(0), ue(2), ue(0), ue(1)]);
        sps.extend([ue(0), ue(0), ue(0), (1, 1), ue(0), ue(0), ue(0), ue(1), ue(0)]);
        sps.extend([ue(0), ue(2), ue(0), ue(0), (0, 3), (1, 1), (7, 4), (7, 4), ue(0)]);
        sps.extend([ue(1), (1, 1), ue(0), (0, 5)]);
        let pps = [ue(0), ue(0), (0, 7), ue(0), ue(0), se(0), (0, 3), se(0), se(0), (0, 10)];
        let pps: Vec<_> = pps.into_iter().chain([ue(0), (0, 2)]).collect();
        let header = pack(&[(1, 1), (0, 1), ue(0), ue(2), se(0)]);

        // CABAC: part_mode 2Nx2N then pcm_flag; after the samples, end_of_slice_segment_flag.
        let samples: Vec<u8> = (0..16 * 16 + 2 * 8 * 8).map(|i| 16 + (i * 7 % 200) as u8).collect();
        let mut slice = vec![0x26, 0x01];
        let rbsp = [header, vec![0x86, 0x80], samples.clone(), vec![0xFE, 0x80]].concat();
        slice.extend(escape(&rbsp));
        let unit = |nal_type: u8, rbsp: &[u8]| {
            let data = [vec![nal_type << 1, 1], escape(rbsp)].concat();
            NalUnit { nal_type, data }
        };
        let config = HvcConfig {
            profile_idc: 1,
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            nal_length_size: 2,
            parameter_sets: vec![unit(nal::SPS, &pack(&sps)), unit(nal::PPS, &pack(&pps))],
        };
        let data = [(slice.len() as u16).to_be_bytes().to_vec(), slice].concat();
        (config, data, samples)
    }

    #[test]
    fn decodes_a_pcm_picture_and_crops_it() {
        let (config, data, samples) = pcm_picture();
        let planes = decode_intra(&config, &data).unwrap();
        assert_eq!((planes.width(), planes.height()), (12, 14));
        assert_eq!(planes.luma.get(11, 13), u16::from(samples[13 * 16 + 11]));
        let [cb, cr] = planes.chroma.as_ref().unwrap();
        assert_eq!((cb.width, cb.height), (6, 7));
        assert_eq!(cb.get(5, 6), u16::from(samples[256 + 6 * 8 + 5]));
        assert_eq!(cr.get(0, 0), u16::from(samples[256 + 64]));
    }

    #[test]
    fn rejects_truncated_slices() {
        let (config, data, _) = pcm_picture();
        let slice = &data[2..];
        for length in 2..slice.len() {
            let truncated = [(length as u16).to_be_bytes().as_slice(), &slice[..length]].concat();
            assert!(decode_intra(&config, &truncated).is_err(), "length {length}");
        }
    }

    /// Compares every `<name>.hevc` in $HEVC_REFERENCE_DIR with the raw planes in
    /// `<name>.yuv` written by a reference decoder (ffmpeg -f rawvideo).
    #[test]
    #[ignore]
    fn matches_reference_decoder() {
        let Ok(directory) = std::env::var("HEVC_REFERENCE_DIR") else {
            return;
        };
        let mut paths: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "hevc"))
            .collect();
        paths.sort();
        let mut failures = Vec::new();
        for path in paths {
            let (config, data) = from_annex_b(&std::fs::read(&path).unwrap());
            let Ok(reference) = std::fs::read(path.with_extension("yuv")) else {
                continue;
            };
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let planes = match decode_intra(&config, &data) {
                Ok(planes) => planes,
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    continue;
                }
            };
            let mut decoded = Vec::new();
            for plane in std::iter::once(&planes.luma).chain(planes.chroma.iter().flatten()) {
                for &sample in &plane.samples {
                    match plane.bit_depth {
                        8 => decoded.push(sample as u8),
                        _ => decoded.extend(sample.to_le_bytes()),
                    }
                }
            }
            match decoded.iter().zip(&reference).position(|(a, b)| a != b) {
                None if decoded.len() == reference.len() => println!("{name}: bit-exact"),
                None => {
                    failures.push(format!("{name}: size {} vs {}", decoded.len(), reference.len()))
                }
                Some(at) => failures.push(format!("{name}: first difference at byte {at}")),
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn parses_configuration_record() {
        let mut record = vec![1, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0x5D, 0xF0, 0];
        record.extend([0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F, 1, 0x21, 0, 1, 0, 3, 0x42, 0x01, 0x01]);
        let config = HvcConfig::parse(&record).unwrap();
        assert_eq!(config.profile_idc, 1);
        assert_eq!(config.chroma_format_idc, 1);
        assert_eq!((config.bit_depth_luma, config.bit_depth_chroma), (8, 8));
        assert_eq!(config.nal_length_size, 4);
        assert_eq!(config.parameter_sets, [NalUnit { nal_type: 33, data: vec![0x42, 1, 1] }]);
    }
}
