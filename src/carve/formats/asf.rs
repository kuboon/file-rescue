//! ASF (WMV/WMA) carver: an ASF file is a sequence of top-level objects,
//! each a 16-byte GUID followed by a 64-bit little-endian size — so the
//! exact file length is the sum of the declared object sizes.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Asf;

const HEADER_GUID: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const DATA_GUID: [u8; 16] = [
    0x36, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
// Simple Index and Index objects (trail the Data object).
const INDEX_GUIDS: [[u8; 16]; 2] = [
    [
        0x90, 0x08, 0x00, 0x33, 0xB1, 0xE5, 0xCF, 0x11, 0x89, 0xF4, 0x00, 0xA0, 0xC9, 0x03, 0x49,
        0xCB,
    ],
    [
        0xD3, 0x29, 0xE2, 0xD6, 0xDA, 0x35, 0xD1, 0x11, 0x90, 0x34, 0x00, 0xA0, 0xC9, 0x03, 0x49,
        0xBE,
    ],
];

static SPEC: CarverSpec = CarverSpec {
    name: "asf",
    extension: "wmv",
    magics: &[&HEADER_GUID],
    magic_offset: 0,
    max_len: 16 << 30,
};

fn object(r: &mut dyn ReadAt, pos: u64) -> io::Result<Option<([u8; 16], u64)>> {
    let mut hdr = [0u8; 24];
    if read_full(r, pos, &mut hdr)? < 24 {
        return Ok(None);
    }
    let mut guid = [0u8; 16];
    guid.copy_from_slice(&hdr[0..16]);
    let size = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
    if size < 24 {
        return Ok(None);
    }
    Ok(Some((guid, size)))
}

impl Carver for Asf {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let Some((guid, header_size)) = object(r, start)? else {
            return Ok(None);
        };
        if guid != HEADER_GUID || header_size > SPEC.max_len {
            return Ok(None);
        }
        let mut pos = start + header_size;
        let mut seen_data = false;
        while pos - start < SPEC.max_len {
            match object(r, pos)? {
                Some((g, size)) if g == DATA_GUID => {
                    seen_data = true;
                    pos = pos.saturating_add(size);
                }
                Some((g, size)) if INDEX_GUIDS.contains(&g) => {
                    pos = pos.saturating_add(size);
                }
                _ => break,
            }
        }
        if !seen_data {
            return Ok(None);
        }
        Ok(Some(Measured {
            len: (pos - start).min(SPEC.max_len),
            extension: SPEC.extension,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_asf;

    #[test]
    fn measures_header_plus_data() {
        let asf = minimal_asf(20_000);
        let mut padded = asf.clone();
        padded.extend_from_slice(&[3u8; 512]);
        let mut r: &[u8] = &padded;
        let m = Asf.measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.len, asf.len() as u64);
        assert_eq!(m.extension, "wmv");
    }

    #[test]
    fn header_without_data_object_is_rejected() {
        let mut data = HEADER_GUID.to_vec();
        data.extend_from_slice(&30u64.to_le_bytes());
        data.extend_from_slice(&[0u8; 64]);
        let mut r: &[u8] = &data;
        assert!(Asf.measure(&mut r, 0).unwrap().is_none());
    }
}
