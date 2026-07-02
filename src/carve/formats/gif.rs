//! GIF carver: exact-length walk of the block structure — header,
//! logical screen descriptor (+ global color table), then image
//! descriptors / extensions with sub-block chains, ending at the 0x3B
//! trailer.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Gif;

static SPEC: CarverSpec = CarverSpec {
    name: "gif",
    extension: "gif",
    magics: &[b"GIF87a", b"GIF89a"],
    magic_offset: 0,
    max_len: 256 << 20,
};

/// Skip a chain of data sub-blocks (length byte + data, 0 terminates).
fn skip_subblocks(r: &mut dyn ReadAt, mut pos: u64, limit: u64) -> io::Result<Option<u64>> {
    loop {
        if pos >= limit {
            return Ok(None);
        }
        let mut len = [0u8; 1];
        if read_full(r, pos, &mut len)? < 1 {
            return Ok(None);
        }
        pos += 1;
        if len[0] == 0 {
            return Ok(Some(pos));
        }
        pos += len[0] as u64;
    }
}

impl Carver for Gif {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let limit = (start + SPEC.max_len).min(r.total_len());
        // Logical screen descriptor: 7 bytes after the 6-byte header.
        let mut lsd = [0u8; 7];
        if read_full(r, start + 6, &mut lsd)? < 7 {
            return Ok(None);
        }
        let mut pos = start + 13;
        if lsd[4] & 0x80 != 0 {
            pos += 3 << ((lsd[4] & 0x07) + 1); // global color table
        }
        let mut seen_image = false;
        while pos < limit {
            let mut b = [0u8; 1];
            if read_full(r, pos, &mut b)? < 1 {
                return Ok(None);
            }
            pos += 1;
            match b[0] {
                0x3B => {
                    // Trailer: end of file.
                    if !seen_image {
                        return Ok(None);
                    }
                    return Ok(Some(Measured {
                        len: pos - start,
                        extension: SPEC.extension,
                    }));
                }
                0x2C => {
                    // Image descriptor: 9 bytes, optional local color
                    // table, LZW min code size, sub-blocks.
                    let mut desc = [0u8; 9];
                    if read_full(r, pos, &mut desc)? < 9 {
                        return Ok(None);
                    }
                    pos += 9;
                    if desc[8] & 0x80 != 0 {
                        pos += 3 << ((desc[8] & 0x07) + 1);
                    }
                    pos += 1; // LZW minimum code size
                    match skip_subblocks(r, pos, limit)? {
                        Some(p) => pos = p,
                        None => return Ok(None),
                    }
                    seen_image = true;
                }
                0x21 => {
                    // Extension: label byte then sub-blocks.
                    pos += 1;
                    match skip_subblocks(r, pos, limit)? {
                        Some(p) => pos = p,
                        None => return Ok(None),
                    }
                }
                _ => return Ok(None), // structure broken
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_gif;

    #[test]
    fn measures_exact_length() {
        let gif = minimal_gif(5000);
        let mut padded = gif.clone();
        padded.extend_from_slice(&[0x42; 512]);
        let mut r: &[u8] = &padded;
        let m = Gif.measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.len, gif.len() as u64);
    }

    #[test]
    fn rejects_header_without_blocks() {
        let mut junk = b"GIF89a".to_vec();
        junk.extend_from_slice(&[0xFFu8; 64]);
        let mut r: &[u8] = &junk;
        assert!(Gif.measure(&mut r, 0).unwrap().is_none());
    }
}
