//! RIFF container carver: AVI, WAV and WebP all declare their size in
//! the RIFF header (exact). AVIs over ~2 GiB (OpenDML) continue in
//! additional `RIFF....AVIX` segments, which are appended to the extent.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Riff;

static SPEC: CarverSpec = CarverSpec {
    name: "riff",
    extension: "avi",
    magics: &[b"RIFF"],
    magic_offset: 0,
    max_len: 64 << 30,
};

fn riff_segment(r: &mut dyn ReadAt, pos: u64) -> io::Result<Option<(u64, [u8; 4])>> {
    let mut hdr = [0u8; 12];
    if read_full(r, pos, &mut hdr)? < 12 {
        return Ok(None);
    }
    if &hdr[0..4] != b"RIFF" {
        return Ok(None);
    }
    let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
    if size < 4 {
        return Ok(None);
    }
    let form = [hdr[8], hdr[9], hdr[10], hdr[11]];
    // Chunk data is padded to an even length.
    Ok(Some((8 + size + (size & 1), form)))
}

impl Carver for Riff {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let Some((seg_len, form)) = riff_segment(r, start)? else {
            return Ok(None);
        };
        let extension = match &form {
            b"AVI " => "avi",
            b"WAVE" => "wav",
            b"WEBP" => "webp",
            _ => return Ok(None), // unknown RIFF form: don't carve blindly
        };
        let mut end = start + seg_len;
        if extension == "avi" {
            // OpenDML: follow consecutive AVIX segments.
            while end - start < SPEC.max_len {
                match riff_segment(r, end)? {
                    Some((len, f)) if &f == b"AVIX" => end += len,
                    _ => break,
                }
            }
        }
        Ok(Some(Measured {
            len: (end - start).min(SPEC.max_len),
            extension,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{minimal_avi, minimal_wav, minimal_webp};

    fn measure(data: &[u8]) -> Option<Measured> {
        let mut r: &[u8] = data;
        Riff.measure(&mut r, 0).unwrap()
    }

    #[test]
    fn avi_with_avix_extension_is_one_file() {
        let avi = minimal_avi(10_000, 2); // main + 2 AVIX segments
        let mut padded = avi.clone();
        padded.extend_from_slice(&[7u8; 999]);
        let m = measure(&padded).unwrap();
        assert_eq!(m.len, avi.len() as u64);
        assert_eq!(m.extension, "avi");
    }

    #[test]
    fn wav_and_webp_forms() {
        let wav = minimal_wav(3000);
        let m = measure(&wav).unwrap();
        assert_eq!(m.len, wav.len() as u64);
        assert_eq!(m.extension, "wav");

        let webp = minimal_webp(2000);
        let m = measure(&webp).unwrap();
        assert_eq!(m.len, webp.len() as u64);
        assert_eq!(m.extension, "webp");
    }

    #[test]
    fn unknown_form_is_rejected() {
        let mut data = b"RIFF".to_vec();
        data.extend_from_slice(&100u32.to_le_bytes());
        data.extend_from_slice(b"XXXX");
        data.extend_from_slice(&[0u8; 100]);
        assert!(measure(&data).is_none());
    }
}
