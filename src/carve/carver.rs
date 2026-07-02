//! The carver abstraction: a cheap magic-byte pre-filter (`CarverSpec`)
//! plus a `measure` that validates the header and computes the file's
//! total length — exactly, via structure walking, where the format
//! allows it.

use std::io;

/// Positioned reader over the carve input (image file or raw source).
pub trait ReadAt {
    fn total_len(&self) -> u64;
    /// May return fewer bytes only at end of input.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

impl ReadAt for &[u8] {
    fn total_len(&self) -> u64 {
        (**self).len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let data: &[u8] = self;
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let start = offset as usize;
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }
}

/// Read exactly `buf.len()` bytes or report how many were available.
pub fn read_full(r: &mut dyn ReadAt, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let n = r.read_at(offset + done as u64, &mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}

pub struct CarverSpec {
    /// Format key used by `--formats` and reports, e.g. "mp4".
    pub name: &'static str,
    /// Default output extension.
    pub extension: &'static str,
    /// Any of these byte strings at `magic_offset` marks a candidate.
    pub magics: &'static [&'static [u8]],
    pub magic_offset: u64,
    /// Hard upper bound for a single carved file.
    pub max_len: u64,
}

/// A validated hit: total file length and the extension to use.
pub struct Measured {
    pub len: u64,
    pub extension: &'static str,
}

pub trait Carver: Sync {
    fn spec(&self) -> &'static CarverSpec;
    /// `start` is the offset where the candidate file begins (magic was
    /// found at `start + magic_offset`). Returns None for false positives.
    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>>;
}

pub fn builtin_carvers() -> Vec<Box<dyn Carver>> {
    // Ordered roughly by magic strength / structure confidence: exact
    // structure walkers first, sync-run heuristics last.
    vec![
        Box::new(super::formats::isobmff::IsoBmff),
        Box::new(super::formats::riff::Riff),
        Box::new(super::formats::matroska::Matroska),
        Box::new(super::formats::asf::Asf),
        Box::new(super::formats::png::Png),
        Box::new(super::formats::gif::Gif),
        Box::new(super::formats::bmp::Bmp),
        Box::new(super::formats::tiff::Tiff),
        Box::new(super::formats::jpeg::Jpeg),
        Box::new(super::formats::mpeg::MpegPs),
        Box::new(super::formats::mpeg::MpegTs::ts188()),
        Box::new(super::formats::mpeg::MpegTs::m2ts192()),
        Box::new(super::formats::pdf::Pdf),
    ]
}
