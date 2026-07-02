//! PDF carver: a PDF ends at its **last** `%%EOF` (incremental updates
//! append additional xref sections, each with its own `%%EOF`). The scan
//! window is cut short at the next `%PDF-` header so a following PDF in
//! the image is never swallowed.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Pdf;

static SPEC: CarverSpec = CarverSpec {
    name: "pdf",
    extension: "pdf",
    magics: &[b"%PDF-"],
    magic_offset: 0,
    max_len: 256 << 20,
};

impl Carver for Pdf {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let limit = (start + SPEC.max_len).min(r.total_len());
        let eof_finder = memchr::memmem::Finder::new(b"%%EOF");
        let hdr_finder = memchr::memmem::Finder::new(b"%PDF-");
        let mut buf = vec![0u8; 256 * 1024];
        let mut last_eof_end: Option<u64> = None;
        let mut pos = start;
        const OVERLAP: u64 = 4;
        'outer: while pos < limit {
            let want = ((limit - pos) as usize).min(buf.len());
            let got = read_full(r, pos, &mut buf[..want])?;
            if got == 0 {
                break;
            }
            let hay = &buf[..got];
            // Window ends where the next PDF begins.
            let window_end = match hdr_finder.find(&hay[if pos == start { 5 } else { 0 }..]) {
                Some(i) => i + if pos == start { 5 } else { 0 },
                None => got,
            };
            let mut search_from = 0;
            while let Some(i) = eof_finder.find(&hay[search_from..window_end]) {
                let at = search_from + i;
                last_eof_end = Some(pos + at as u64 + 5);
                search_from = at + 5;
            }
            if window_end < got {
                break 'outer;
            }
            pos += (got as u64).saturating_sub(OVERLAP).max(1);
        }
        let Some(mut end) = last_eof_end else {
            return Ok(None);
        };
        // Include the customary trailing newline (\n or \r\n).
        let mut tail = [0u8; 2];
        let n = read_full(r, end, &mut tail)?;
        if n >= 1 && tail[0] == b'\r' {
            end += 1;
            if n >= 2 && tail[1] == b'\n' {
                end += 1;
            }
        } else if n >= 1 && tail[0] == b'\n' {
            end += 1;
        }
        Ok(Some(Measured {
            len: end - start,
            extension: SPEC.extension,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_pdf;

    fn measure(data: &[u8]) -> Option<Measured> {
        let mut r: &[u8] = data;
        Pdf.measure(&mut r, 0).unwrap()
    }

    #[test]
    fn ends_at_last_eof() {
        let pdf = minimal_pdf(10_000);
        let mut padded = pdf.clone();
        padded.extend_from_slice(&[b'x'; 2048]);
        let m = measure(&padded).unwrap();
        assert_eq!(m.len, pdf.len() as u64);
    }

    #[test]
    fn does_not_swallow_next_pdf() {
        let a = minimal_pdf(1000);
        let b = minimal_pdf(500);
        let mut disk = a.clone();
        disk.extend_from_slice(&b);
        let m = measure(&disk).unwrap();
        assert_eq!(m.len, a.len() as u64);
    }

    #[test]
    fn no_eof_is_rejected() {
        let data = b"%PDF-1.4\nsome content without terminator".to_vec();
        assert!(measure(&data).is_none());
    }
}
