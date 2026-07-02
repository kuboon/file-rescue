//! Matroska / WebM carver (also the usual home of VP9/AV1 video).
//! EBML gives every element a declared size: the file is the EBML
//! header element plus the Segment element, both measured exactly via
//! EBML variable-length integers.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Matroska;

static SPEC: CarverSpec = CarverSpec {
    name: "mkv",
    extension: "mkv",
    magics: &[b"\x1A\x45\xDF\xA3"],
    magic_offset: 0,
    max_len: 64 << 30,
};

/// Parse an EBML vint at `pos`: returns (value, encoded_len). For sizes,
/// an all-ones value means "unknown size" and is returned as None.
fn vint(r: &mut dyn ReadAt, pos: u64) -> io::Result<Option<(Option<u64>, u64)>> {
    let mut first = [0u8; 1];
    if read_full(r, pos, &mut first)? < 1 {
        return Ok(None);
    }
    let b = first[0];
    if b == 0 {
        return Ok(None);
    }
    let len = b.leading_zeros() as usize + 1; // 1..=8
    let mut rest = [0u8; 8];
    if len > 1 && read_full(r, pos + 1, &mut rest[..len - 1])? < len - 1 {
        return Ok(None);
    }
    let mask: u8 = if len >= 8 { 0 } else { 0xFF >> len };
    let mut value = (b & mask) as u64;
    for &x in &rest[..len - 1] {
        value = (value << 8) | x as u64;
    }
    // All value bits set = reserved "unknown size".
    let all_ones = (1u64 << (7 * len)) - 1;
    let v = if value == all_ones { None } else { Some(value) };
    Ok(Some((v, len as u64)))
}

impl Carver for Matroska {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        // EBML header element: ID (4 bytes, the magic) + size + payload.
        let Some((Some(hdr_size), hdr_len)) = vint(r, start + 4)? else {
            return Ok(None);
        };
        if hdr_size > 4096 {
            return Ok(None);
        }
        let mut hdr_payload = vec![0u8; hdr_size as usize];
        let got = read_full(r, start + 4 + hdr_len, &mut hdr_payload)?;
        let extension = if memchr::memmem::find(&hdr_payload[..got], b"webm").is_some() {
            "webm"
        } else if memchr::memmem::find(&hdr_payload[..got], b"matroska").is_some() {
            "mkv"
        } else {
            return Ok(None); // EBML but not a Matroska family doctype
        };
        // Segment element: ID 0x18538067 + size.
        let seg_at = start + 4 + hdr_len + hdr_size;
        let mut seg_id = [0u8; 4];
        if read_full(r, seg_at, &mut seg_id)? < 4 || seg_id != [0x18, 0x53, 0x80, 0x67] {
            return Ok(None);
        }
        let Some((seg_size, seg_len)) = vint(r, seg_at + 4)? else {
            return Ok(None);
        };
        let Some(seg_size) = seg_size else {
            // Unknown-size segment (live streaming): end is not declared,
            // don't guess.
            return Ok(None);
        };
        let len = (seg_at + 4 + seg_len + seg_size - start).min(SPEC.max_len);
        Ok(Some(Measured { len, extension }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_mkv;

    #[test]
    fn measures_exact_length_and_doctype() {
        for (webm, ext) in [(false, "mkv"), (true, "webm")] {
            let f = minimal_mkv(30_000, webm);
            let mut padded = f.clone();
            padded.extend_from_slice(&[5u8; 300]);
            let mut r: &[u8] = &padded;
            let m = Matroska.measure(&mut r, 0).unwrap().unwrap();
            assert_eq!(m.len, f.len() as u64);
            assert_eq!(m.extension, ext);
        }
    }

    #[test]
    fn rejects_non_matroska_ebml() {
        let mut data = b"\x1A\x45\xDF\xA3".to_vec();
        data.push(0x84); // size 4
        data.extend_from_slice(b"junk");
        data.extend_from_slice(&[0u8; 32]);
        let mut r: &[u8] = &data;
        assert!(Matroska.measure(&mut r, 0).unwrap().is_none());
    }
}
