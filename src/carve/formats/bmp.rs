//! BMP carver: the file header declares the exact total size at offset 2
//! (little-endian). The 2-byte "BM" magic is weak, so the header is
//! validated strictly before trusting it.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Bmp;

static SPEC: CarverSpec = CarverSpec {
    name: "bmp",
    extension: "bmp",
    magics: &[b"BM"],
    magic_offset: 0,
    max_len: 256 << 20,
};

impl Carver for Bmp {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let mut hdr = [0u8; 18];
        if read_full(r, start, &mut hdr)? < 18 {
            return Ok(None);
        }
        let file_size = u32::from_le_bytes([hdr[2], hdr[3], hdr[4], hdr[5]]) as u64;
        let data_offset = u32::from_le_bytes([hdr[10], hdr[11], hdr[12], hdr[13]]) as u64;
        let dib_size = u32::from_le_bytes([hdr[14], hdr[15], hdr[16], hdr[17]]);
        // Known DIB header sizes: BITMAPCOREHEADER..BITMAPV5HEADER.
        if !matches!(dib_size, 12 | 40 | 52 | 56 | 64 | 108 | 124) {
            return Ok(None);
        }
        if file_size < 14 + dib_size as u64
            || file_size > SPEC.max_len
            || data_offset < 14 + dib_size as u64
            || data_offset >= file_size
        {
            return Ok(None);
        }
        Ok(Some(Measured {
            len: file_size,
            extension: SPEC.extension,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_bmp;

    #[test]
    fn measures_declared_size() {
        let bmp = minimal_bmp(64, 64);
        let mut padded = bmp.clone();
        padded.extend_from_slice(&[9u8; 777]);
        let mut r: &[u8] = &padded;
        let m = Bmp.measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.len, bmp.len() as u64);
    }

    #[test]
    fn rejects_bogus_headers() {
        let junk = b"BM\xFF\xFF\xFF\xFF\x00\x00\x00\x00\x00\x00\x00\x00\x99\x00\x00\x00".to_vec();
        let mut r: &[u8] = &junk;
        assert!(Bmp.measure(&mut r, 0).unwrap().is_none());
    }
}
