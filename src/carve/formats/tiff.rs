//! TIFF carver — also covers TIFF-based camera RAW (CR2, and typically
//! NEF/ARW/DNG). TIFF declares no total length, so the extent is
//! computed by walking the IFD chain and taking the furthest end of any
//! referenced data: out-of-line entry values, strip/tile data
//! (offsets + byte counts), Exif/sub-IFDs. This recovers the full file
//! for the common layouts where image data follows the metadata.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct Tiff;

static SPEC: CarverSpec = CarverSpec {
    name: "tiff",
    extension: "tif",
    magics: &[b"II*\x00", b"MM\x00*"],
    magic_offset: 0,
    max_len: 2 << 30,
};

const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_STRIP_COUNTS: u16 = 279;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_COUNTS: u16 = 325;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_EXIF_IFD: u16 = 0x8769;

fn type_size(t: u16) -> u64 {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16..=18 => 8,
        _ => 0,
    }
}

struct Walker<'a> {
    r: &'a mut dyn ReadAt,
    start: u64,
    le: bool,
    limit: u64,
    max_end: u64,
    ifds_left: u32,
}

impl Walker<'_> {
    fn u16_at(&mut self, off: u64) -> io::Result<Option<u16>> {
        let mut b = [0u8; 2];
        if read_full(self.r, self.start + off, &mut b)? < 2 {
            return Ok(None);
        }
        Ok(Some(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }))
    }

    fn u32_at(&mut self, off: u64) -> io::Result<Option<u32>> {
        let mut b = [0u8; 4];
        if read_full(self.r, self.start + off, &mut b)? < 4 {
            return Ok(None);
        }
        Ok(Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }))
    }

    fn note_end(&mut self, end: u64) {
        if end <= self.limit {
            self.max_end = self.max_end.max(end);
        }
    }

    /// Read a LONG/SHORT array entry's values (offsets or byte counts).
    fn entry_values(
        &mut self,
        typ: u16,
        count: u32,
        value_off: u64,
    ) -> io::Result<Option<Vec<u64>>> {
        let elem = type_size(typ);
        if elem == 0 || count as u64 > 65536 {
            return Ok(None);
        }
        let total = elem * count as u64;
        // Arrays ≤ 4 bytes live inline in the value field.
        let base = if total <= 4 {
            value_off
        } else {
            match self.u32_at(value_off)? {
                Some(o) => o as u64,
                None => return Ok(None),
            }
        };
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as u64 {
            let v = match elem {
                2 => self.u16_at(base + i * 2)?.map(|v| v as u64),
                4 => self.u32_at(base + i * 4)?.map(|v| v as u64),
                _ => return Ok(None),
            };
            match v {
                Some(v) => out.push(v),
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    fn walk_ifd(&mut self, ifd_off: u64) -> io::Result<()> {
        if self.ifds_left == 0 || ifd_off == 0 || ifd_off >= self.limit {
            return Ok(());
        }
        self.ifds_left -= 1;
        let Some(count) = self.u16_at(ifd_off)? else {
            return Ok(());
        };
        if count == 0 || count > 4096 {
            return Ok(());
        }
        let entries = ifd_off + 2;
        let mut strip_offsets: Option<Vec<u64>> = None;
        let mut strip_counts: Option<Vec<u64>> = None;
        for i in 0..count as u64 {
            let e = entries + i * 12;
            let (Some(tag), Some(typ), Some(n)) =
                (self.u16_at(e)?, self.u16_at(e + 2)?, self.u32_at(e + 4)?)
            else {
                return Ok(());
            };
            let total = type_size(typ) * n as u64;
            if total > 4 {
                if let Some(off) = self.u32_at(e + 8)? {
                    self.note_end(off as u64 + total);
                }
            }
            match tag {
                TAG_STRIP_OFFSETS | TAG_TILE_OFFSETS => {
                    strip_offsets = self.entry_values(typ, n, e + 8)?;
                }
                TAG_STRIP_COUNTS | TAG_TILE_COUNTS => {
                    strip_counts = self.entry_values(typ, n, e + 8)?;
                }
                TAG_SUB_IFDS => {
                    if let Some(offs) = self.entry_values(typ, n, e + 8)? {
                        for o in offs {
                            self.walk_ifd(o)?;
                        }
                    }
                }
                TAG_EXIF_IFD => {
                    if let Some(o) = self.u32_at(e + 8)? {
                        self.walk_ifd(o as u64)?;
                    }
                }
                _ => {}
            }
        }
        if let (Some(offs), Some(counts)) = (&strip_offsets, &strip_counts) {
            for (o, c) in offs.iter().zip(counts) {
                self.note_end(o + c);
            }
        }
        // End of this IFD's own table.
        self.note_end(entries + count as u64 * 12 + 4);
        if let Some(next) = self.u32_at(entries + count as u64 * 12)? {
            self.walk_ifd(next as u64)?;
        }
        Ok(())
    }
}

impl Carver for Tiff {
    fn spec(&self) -> &'static CarverSpec {
        &SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let mut hdr = [0u8; 12];
        if read_full(r, start, &mut hdr)? < 12 {
            return Ok(None);
        }
        let le = &hdr[0..2] == b"II";
        // CR2 marks itself at offset 8.
        let is_cr2 = &hdr[8..10] == b"CR";
        let limit = SPEC.max_len.min(r.total_len() - start);
        let mut w = Walker {
            r,
            start,
            le,
            limit,
            max_end: 8,
            ifds_left: 64,
        };
        let ifd0 = match w.u32_at(4)? {
            Some(o) => o as u64,
            None => return Ok(None),
        };
        if ifd0 < 8 {
            return Ok(None);
        }
        w.walk_ifd(ifd0)?;
        let len = w.max_end;
        // A real TIFF references data past its header; a stray magic
        // in garbage will not produce a plausible walk.
        if len <= 8 + 6 {
            return Ok(None);
        }
        Ok(Some(Measured {
            len,
            extension: if is_cr2 { "cr2" } else { "tif" },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::minimal_tiff;

    #[test]
    fn extent_covers_strip_data() {
        for le in [true, false] {
            let tif = minimal_tiff(20_000, le, false);
            let mut padded = tif.clone();
            padded.extend_from_slice(&[1u8; 400]);
            let mut r: &[u8] = &padded;
            let m = Tiff.measure(&mut r, 0).unwrap().unwrap();
            assert_eq!(m.len, tif.len() as u64, "le={le}");
            assert_eq!(m.extension, "tif");
        }
    }

    #[test]
    fn cr2_gets_its_extension() {
        let cr2 = minimal_tiff(10_000, true, true);
        let mut r: &[u8] = &cr2;
        let m = Tiff.measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.extension, "cr2");
    }

    #[test]
    fn bare_magic_in_garbage_is_rejected() {
        let mut junk = b"II*\x00".to_vec();
        junk.extend_from_slice(&[0u8; 60]);
        let mut r: &[u8] = &junk;
        assert!(Tiff.measure(&mut r, 0).unwrap().is_none());
    }
}
