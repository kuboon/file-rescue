//! PNG carver: exact-length structure walk over the chunk chain
//! (length + type + data + crc) from IHDR to IEND. If the chain breaks
//! after image data was seen, the valid prefix is still carved so a
//! damaged PNG is recovered partially instead of dropped.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Png;

static SPEC: CarverSpec = CarverSpec {
    name: "png",
    extension: "png",
    magics: &[b"\x89PNG\r\n\x1a\n"],
    magic_offset: 0,
    max_len: 256 << 20,
};

impl Carver for Png {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let limit = (start + SPEC.max_len).min(r.total_len());
        let mut pos = start + 8;
        let mut seen_idat = false;
        let mut first = true;
        while pos + 8 <= limit {
            let mut hdr = [0u8; 8];
            if read_full(r, pos, &mut hdr)? < 8 {
                break;
            }
            let len = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
            let typ = &hdr[4..8];
            if len > (1 << 31) - 1 || !typ.iter().all(|b| b.is_ascii_alphabetic()) {
                break;
            }
            if first {
                if typ != b"IHDR" || len != 13 {
                    return Ok(None);
                }
                first = false;
            }
            if typ == b"IDAT" {
                seen_idat = true;
            }
            pos = pos.saturating_add(12 + len);
            if typ == b"IEND" {
                if pos > limit {
                    break;
                }
                return Ok(Some(Measured {
                    len: pos - start,
                    extension: SPEC.extension,
                }));
            }
            if pos > limit {
                pos = limit;
                break;
            }
        }
        // Chain broke before IEND: carve the valid prefix if it holds data.
        if seen_idat && pos > start + 8 {
            return Ok(Some(Measured {
                len: pos.min(limit) - start,
                extension: SPEC.extension,
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_png;

    fn measure(data: &[u8]) -> Option<Measured> {
        let mut r: &[u8] = data;
        Png.measure(&mut r, 0).unwrap()
    }

    #[test]
    fn measures_exact_length() {
        let png = minimal_png(50_000);
        let mut padded = png.clone();
        padded.extend_from_slice(&[0x77; 4096]);
        let m = measure(&padded).unwrap();
        assert_eq!(m.len, png.len() as u64);
    }

    #[test]
    fn carves_partial_when_chain_breaks() {
        let png = minimal_png(50_000);
        let mut broken = png.clone();
        // Corrupt the IEND chunk area with garbage so the chain breaks
        // after IDAT.
        let at = png.len() - 12;
        broken[at..].fill(0x00);
        let m = measure(&broken).expect("partial carve");
        assert!(m.len > 8);
        assert!(m.len <= png.len() as u64);
    }

    #[test]
    fn rejects_signature_without_ihdr() {
        let mut junk = b"\x89PNG\r\n\x1a\n".to_vec();
        junk.extend_from_slice(&[0u8; 64]);
        assert!(measure(&junk).is_none());
    }
}
