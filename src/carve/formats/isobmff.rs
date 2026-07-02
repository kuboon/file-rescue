//! ISO Base Media File Format (MP4 / MOV / HEIC / 3GP...) carver.
//!
//! Instead of guessing where a video ends by content sniffing (which is
//! what makes other carvers split videos into fragments), this walks the
//! top-level box chain — `ftyp`, `moov`, `mdat`, ... — following each
//! box's declared size, including 64-bit `largesize` boxes. The sum of
//! the declared sizes IS the exact file length, so the file is carved
//! whole in one piece.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct IsoBmff;

static SPEC: CarverSpec = CarverSpec {
    name: "mp4",
    extension: "mp4",
    magics: &[b"ftyp"],
    magic_offset: 4,
    max_len: 64 << 30, // 64 GiB: don't cap real camera footage
};

/// Box types that legitimately appear at the top level of a media file.
const TOP_LEVEL: [&[u8; 4]; 17] = [
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta", b"moof", b"mfra",
    b"sidx", b"styp", b"pnot", b"udta", b"prft", b"emsg", b"mfro",
];

impl Carver for IsoBmff {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let input_len = r.total_len();
        let mut pos = start;
        let mut brand = [0u8; 4];
        let mut seen_payload = false; // mdat / moov / meta
        let mut truncated = false;
        while pos < input_len && pos - start < SPEC.max_len {
            let mut hdr = [0u8; 8];
            if read_full(r, pos, &mut hdr)? < 8 {
                break;
            }
            let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let typ: [u8; 4] = [hdr[4], hdr[5], hdr[6], hdr[7]];
            if pos == start {
                if &typ != b"ftyp" {
                    return Ok(None);
                }
                // A plausible ftyp is small; reject garbage that happens
                // to contain the string "ftyp".
                if !(16..=256).contains(&size32) || size32 % 4 != 0 {
                    return Ok(None);
                }
                read_full(r, pos + 8, &mut brand)?;
            } else if &typ == b"ftyp" {
                break; // next concatenated file begins here
            } else if !TOP_LEVEL.contains(&&typ) {
                break; // no longer walking valid top-level boxes
            }
            let box_len = match size32 {
                0 => {
                    // "box extends to end of file": length unknowable when
                    // carving, stop here with what we have.
                    break;
                }
                1 => {
                    let mut large = [0u8; 8];
                    if read_full(r, pos + 8, &mut large)? < 8 {
                        break;
                    }
                    let l = u64::from_be_bytes(large);
                    if l < 16 {
                        break;
                    }
                    l
                }
                s if s < 8 => break,
                s => s as u64,
            };
            if matches!(&typ, b"mdat" | b"moov" | b"meta" | b"moof") {
                seen_payload = true;
            }
            let next = pos.saturating_add(box_len);
            if next > input_len {
                // Declared length runs past the end of the image
                // (truncated source): keep the declared extent, clamped
                // by the caller.
                pos = next;
                truncated = true;
                break;
            }
            pos = next;
        }
        let len = pos - start;
        if !seen_payload || len <= 24 {
            return Ok(None);
        }
        let _ = truncated;
        let extension = match &brand {
            b"qt  " => "mov",
            b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc" => "heic",
            b"avif" => "avif",
            _ => "mp4",
        };
        Ok(Some(Measured { len, extension }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{minimal_heic, minimal_mov, minimal_mp4};

    fn measure(data: &[u8]) -> Option<Measured> {
        let mut r: &[u8] = data;
        IsoBmff.measure(&mut r, 0).unwrap()
    }

    #[test]
    fn measures_exact_mp4_length() {
        for (moov_first, large) in [(false, false), (true, false), (false, true)] {
            let mp4 = minimal_mp4(100_000, moov_first, large);
            let mut padded = mp4.clone();
            padded.extend_from_slice(&[0xAB; 4096]); // trailing garbage
            let m = measure(&padded).expect("should match");
            assert_eq!(
                m.len,
                mp4.len() as u64,
                "moov_first={moov_first} large={large}"
            );
            assert_eq!(m.extension, "mp4");
        }
    }

    #[test]
    fn detects_mov_and_heic_brands() {
        let mov = minimal_mov(5000);
        let m = measure(&mov).unwrap();
        assert_eq!(m.extension, "mov");
        assert_eq!(m.len, mov.len() as u64);

        let heic = minimal_heic(3000);
        let m = measure(&heic).unwrap();
        assert_eq!(m.extension, "heic");
        assert_eq!(m.len, heic.len() as u64);
    }

    #[test]
    fn stops_at_next_concatenated_file() {
        let a = minimal_mp4(10_000, false, false);
        let b = minimal_mp4(5_000, false, false);
        let mut disk = a.clone();
        disk.extend_from_slice(&b);
        let m = measure(&disk).unwrap();
        assert_eq!(m.len, a.len() as u64);
    }

    #[test]
    fn truncated_input_keeps_declared_extent() {
        let mp4 = minimal_mp4(100_000, false, false);
        let cut = &mp4[..mp4.len() / 2];
        let m = measure(cut).expect("truncated file still carved");
        // mdat declared past EOF: extent runs to (at least) end of input.
        assert!(m.len >= cut.len() as u64);
    }

    #[test]
    fn rejects_bare_ftyp_string_in_garbage() {
        let mut junk = vec![0u8; 64];
        junk[4..8].copy_from_slice(b"ftyp"); // size32 = 0 → invalid ftyp
        assert!(measure(&junk).is_none());
    }
}
