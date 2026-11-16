//! Module: mirante_raw_sony::sr2
//! Purpose: Finds and decrypts Sony's SR2SubIFD, which holds levels and white balance.
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (sony_decrypt). IFD0's
//! DNGPrivateData points to the SR2Private IFD, which gives the offset, length and
//! key of the encrypted SR2SubIFD; its value offsets are absolute file positions.

use mirante_codec::Endian;
use mirante_codec::tiff::{Ifd, Tiff};
use mirante_core::DecodeError;

const DNG_PRIVATE_DATA: u16 = 0xc634;
const SR2_SUB_IFD_OFFSET: u16 = 0x7200;
const SR2_SUB_IFD_LENGTH: u16 = 0x7201;
const SR2_SUB_IFD_KEY: u16 = 0x7221;
const PAD_WORDS: usize = 128;

/// The decrypted SR2SubIFD, readable with absolute file offsets.
pub struct Sr2 {
    /// The file up to the end of the SR2SubIFD, with that part decrypted.
    data: Vec<u8>,
    offset: usize,
    endian: Endian,
}

impl Sr2 {
    /// Returns None when the file has no SR2 data; errors when it is damaged.
    pub fn find(tiff: &Tiff<'_>) -> Result<Option<Self>, DecodeError> {
        let Some(main) = tiff.ifds.first() else {
            return Ok(None);
        };
        let Some(private) = main.get(DNG_PRIVATE_DATA) else {
            return Ok(None);
        };
        let pointer: [u8; 4] = tiff
            .raw_bytes(private)?
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| DecodeError::corrupt("Sony SR2Private pointer is too short"))?;
        let private_offset = match tiff.endian() {
            Endian::Little => u32::from_le_bytes(pointer),
            Endian::Big => u32::from_be_bytes(pointer),
        };
        let (private, _) = tiff.read_ifd(private_offset as usize)?;
        let field = |tag| tiff.get_u32(&private, tag);
        let (Some(offset), Some(length), Some(key)) =
            (field(SR2_SUB_IFD_OFFSET)?, field(SR2_SUB_IFD_LENGTH)?, key(tiff, &private)?)
        else {
            return Ok(None);
        };
        let (offset, length) = (offset as usize, length as usize);
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= tiff.data().len())
            .ok_or_else(|| DecodeError::corrupt("Sony SR2SubIFD lies outside the file"))?;
        let mut data = tiff.data()[..end].to_vec();
        decrypt(&mut data[offset..], key);
        Ok(Some(Self { data, offset, endian: tiff.endian() }))
    }

    /// The decrypted directory, with a reader whose offsets are file positions.
    pub fn ifd(&self) -> Result<(Tiff<'_>, Ifd), DecodeError> {
        let tiff = Tiff::headerless(&self.data, self.endian);
        let (ifd, _) = tiff.read_ifd(self.offset)?;
        Ok((tiff, ifd))
    }
}

/// The key is four undefined bytes read as one integer in the file's byte order.
fn key(tiff: &Tiff<'_>, private: &Ifd) -> Result<Option<u32>, DecodeError> {
    let Some(entry) = private.get(SR2_SUB_IFD_KEY) else {
        return Ok(None);
    };
    let bytes: [u8; 4] = match tiff.raw_bytes(entry)?.get(..4) {
        Some(bytes) => bytes.try_into().expect("four bytes"),
        None => return Ok(None),
    };
    Ok(Some(match tiff.endian() {
        Endian::Little => u32::from_le_bytes(bytes),
        Endian::Big => u32::from_be_bytes(bytes),
    }))
}

/// XORs `data` with Sony's key stream; applying it twice restores the input.
/// Words are big-endian and a trailing partial word is left alone.
pub fn decrypt(data: &mut [u8], mut key: u32) {
    let mut pad = [0_u32; PAD_WORDS];
    for word in &mut pad[..4] {
        key = key.wrapping_mul(48_828_125).wrapping_add(1);
        *word = key;
    }
    pad[3] = pad[3] << 1 | (pad[0] ^ pad[2]) >> 31;
    for p in 4..PAD_WORDS - 1 {
        pad[p] = (pad[p - 4] ^ pad[p - 2]) << 1 | (pad[p - 3] ^ pad[p - 1]) >> 31;
    }
    for (index, chunk) in data.chunks_exact_mut(4).enumerate() {
        let p = PAD_WORDS + index;
        let next = pad[p % PAD_WORDS] ^ pad[(p + 64) % PAD_WORDS];
        pad[(p - 1) % PAD_WORDS] = next;
        let word = u32::from_be_bytes((&*chunk).try_into().expect("four bytes")) ^ next;
        chunk.copy_from_slice(&word.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_stream_matches_dcraw() {
        // dcraw's first words for this key; the same stream decrypts real ARW files.
        let mut data = [0_u8; 8];
        decrypt(&mut data, 0x4433_2211);
        let words = [
            u32::from_be_bytes(data[..4].try_into().unwrap()),
            u32::from_be_bytes(data[4..].try_into().unwrap()),
        ];
        assert_eq!(words, [0x54c2_c4d6, 0x49c4_78bc]);
    }

    #[test]
    fn decryption_is_its_own_inverse() {
        let plain: Vec<u8> = (0..=255).collect();
        let mut data = plain.clone();
        decrypt(&mut data, 7);
        assert_ne!(data, plain);
        decrypt(&mut data, 7);
        assert_eq!(data, plain);
    }

    #[test]
    fn reads_an_encrypted_sub_ifd_through_sr2_private() {
        // IFD0: DNGPrivateData -> SR2Private at 26 -> SR2SubIFD at 64 (12 + 2 bytes).
        let mut file = b"II*\0".to_vec();
        file.extend(8_u32.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend([0x34, 0xc6, 1, 0, 4, 0, 0, 0, 26, 0, 0, 0]);
        file.extend(0_u32.to_le_bytes());
        file.extend(3_u16.to_le_bytes());
        file.extend([0x00, 0x72, 4, 0, 1, 0, 0, 0, 64, 0, 0, 0]);
        file.extend([0x01, 0x72, 4, 0, 1, 0, 0, 0, 20, 0, 0, 0]);
        file.extend([0x21, 0x72, 7, 0, 4, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
        file.extend(0_u32.to_le_bytes());
        file.resize(64, 0);
        let mut sub_ifd = 1_u16.to_le_bytes().to_vec();
        sub_ifd.extend([0x11, 0x73, 3, 0, 1, 0, 0, 0, 0x2a, 0, 0, 0]);
        sub_ifd.extend([0; 6]);
        decrypt(&mut sub_ifd, 0x4433_2211);
        file.extend(sub_ifd);
        let tiff = Tiff::parse(&file).unwrap();
        let sr2 = Sr2::find(&tiff).unwrap().unwrap();
        let (sub, ifd) = sr2.ifd().unwrap();
        assert_eq!(sub.get_u32(&ifd, 0x7311).unwrap(), Some(42));
    }
}
