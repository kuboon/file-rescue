//! JPEG carver: validate the marker-segment structure up to SOS, then
//! scan the entropy-coded data for the EOI marker. `FF D9` cannot occur
//! inside entropy data (encoders byte-stuff `FF` as `FF 00`), and the
//! thumbnails embedded in EXIF segments are skipped whole via their
//! declared segment lengths, so the first `FF D9` after SOS is the real
//! end of image — including for progressive JPEGs.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Jpeg;

static SPEC: CarverSpec = CarverSpec {
    name: "jpeg",
    extension: "jpg",
    magics: &[b"\xFF\xD8\xFF"],
    magic_offset: 0,
    max_len: 256 << 20, // 256 MiB
};

impl Carver for Jpeg {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let limit = (start + SPEC.max_len).min(r.total_len());
        let mut pos = start + 2; // past SOI
                                 // Walk marker segments until the first SOS.
        loop {
            if pos + 4 > limit {
                return Ok(None);
            }
            let mut hdr = [0u8; 4];
            if read_full(r, pos, &mut hdr)? < 4 {
                return Ok(None);
            }
            if hdr[0] != 0xFF {
                return Ok(None);
            }
            let marker = hdr[1];
            match marker {
                0x00 | 0xFF => return Ok(None), // not a marker: bogus structure
                0xD9 => {
                    return Ok(Some(Measured {
                        // EOI with no scan data: degenerate but well-formed.
                        len: pos + 2 - start,
                        extension: SPEC.extension,
                    }));
                }
                0x01 | 0xD0..=0xD8 => {
                    pos += 2; // standalone marker
                    continue;
                }
                _ => {
                    let seg_len = u16::from_be_bytes([hdr[2], hdr[3]]) as u64;
                    if seg_len < 2 {
                        return Ok(None);
                    }
                    pos += 2 + seg_len;
                    if marker == 0xDA {
                        break; // SOS: entropy-coded data follows
                    }
                }
            }
        }
        // Scan entropy data for EOI.
        let mut buf = vec![0u8; 64 * 1024];
        let finder = memchr::memmem::Finder::new(b"\xFF\xD9");
        while pos < limit {
            let want = ((limit - pos) as usize).min(buf.len());
            let got = read_full(r, pos, &mut buf[..want])?;
            if got < 2 {
                return Ok(None);
            }
            if let Some(i) = finder.find(&buf[..got]) {
                return Ok(Some(Measured {
                    len: pos + i as u64 + 2 - start,
                    extension: SPEC.extension,
                }));
            }
            // Overlap by 1 so a marker split across reads is not missed.
            pos += (got - 1) as u64;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_jpeg;

    fn measure(data: &[u8]) -> Option<Measured> {
        let mut r: &[u8] = data;
        Jpeg.measure(&mut r, 0).unwrap()
    }

    #[test]
    fn measures_exact_length() {
        let jpg = minimal_jpeg(200_000); // larger than one scan buffer
        let mut padded = jpg.clone();
        padded.extend_from_slice(&[0x11; 999]);
        let m = measure(&padded).unwrap();
        assert_eq!(m.len, jpg.len() as u64);
    }

    #[test]
    fn skips_ffd9_inside_marker_segments() {
        // An APP1 segment containing FF D9 (like an EXIF thumbnail EOI)
        // must be skipped by its declared length.
        let mut data = vec![0xFF, 0xD8];
        let payload = [0x00, 0xFF, 0xD9, 0x00];
        data.extend_from_slice(&[0xFF, 0xE1]);
        data.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(&payload);
        // then a real tail: SOS + entropy + EOI
        data.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 63, 0]);
        data.extend_from_slice(&[0x12, 0x34, 0x56]);
        data.extend_from_slice(&[0xFF, 0xD9]);
        let m = measure(&data).unwrap();
        assert_eq!(m.len, data.len() as u64);
    }

    #[test]
    fn rejects_broken_structure() {
        let mut jpg = minimal_jpeg(100);
        jpg[3] = 0x00; // corrupt the APP0 marker area
                       // Depending on where corruption lands this may parse or not, so
                       // build a clearly broken one instead:
        let broken = [0xFF, 0xD8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(measure(&broken).is_none());
    }

    #[test]
    fn unterminated_scan_returns_none() {
        let jpg = minimal_jpeg(4096);
        let cut = &jpg[..jpg.len() - 2]; // drop the EOI
        assert!(measure(cut).is_none());
    }
}
