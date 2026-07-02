//! MPEG program stream (.mpg — DVD recorders, early camcorders) and
//! MPEG transport stream (.ts / AVCHD .m2ts) carvers.
//!
//! PS: walk pack headers / PES packets by start codes and declared
//! lengths until the program end code or loss of sync.
//! TS: validate a run of fixed-size packets (188, or 192 with a 4-byte
//! timestamp prefix as on AVCHD) whose sync byte 0x47 repeats at the
//! packet interval; the file extends while sync holds.

use crate::carve::carver::{read_full, Carver, CarverSpec, Measured, ReadAt};
use std::io;

pub struct MpegPs;

static PS_SPEC: CarverSpec = CarverSpec {
    name: "mpg",
    extension: "mpg",
    magics: &[b"\x00\x00\x01\xBA"],
    magic_offset: 0,
    max_len: 16 << 30,
};

impl Carver for MpegPs {
    fn spec(&self) -> &'static CarverSpec {
        &PS_SPEC
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let limit = (start + PS_SPEC.max_len).min(r.total_len());
        let mut pos = start;
        let mut packets = 0u64;
        while pos + 4 <= limit {
            let mut hdr = [0u8; 6];
            let got = read_full(r, pos, &mut hdr)?;
            if got < 4 || hdr[0] != 0 || hdr[1] != 0 || hdr[2] != 1 {
                break;
            }
            match hdr[3] {
                0xBA => {
                    // Pack header: MPEG-2 starts with bits 01, MPEG-1 with 0010.
                    let mut body = [0u8; 10];
                    if read_full(r, pos + 4, &mut body)? < 10 {
                        break;
                    }
                    if body[0] >> 6 == 0b01 {
                        // MPEG-2: 10 bytes + stuffing length in low 3 bits.
                        pos += 14 + (body[9] & 0x07) as u64;
                    } else if body[0] >> 4 == 0b0010 {
                        pos += 12; // MPEG-1
                    } else {
                        break;
                    }
                }
                0xB9 => {
                    pos += 4; // program end code
                    packets += 1;
                    break;
                }
                // System header and PES packets carry a 16-bit length.
                0xBB..=0xFF => {
                    if got < 6 {
                        break;
                    }
                    let len = u16::from_be_bytes([hdr[4], hdr[5]]) as u64;
                    pos += 6 + len;
                }
                _ => break,
            }
            packets += 1;
        }
        // Require a few packets so garbage after a stray start code is
        // not carved.
        if packets < 4 || pos <= start + 32 {
            return Ok(None);
        }
        Ok(Some(Measured {
            len: (pos - start).min(PS_SPEC.max_len),
            extension: PS_SPEC.extension,
        }))
    }
}

/// Transport stream with `packet` bytes per packet and the sync byte at
/// `sync_off` within each packet.
pub struct MpegTs {
    packet: u64,
    sync_off: u64,
    spec: &'static CarverSpec,
}

static TS_SPEC: CarverSpec = CarverSpec {
    name: "ts",
    extension: "ts",
    magics: &[b"\x47"],
    magic_offset: 0,
    max_len: 64 << 30,
};

static M2TS_SPEC: CarverSpec = CarverSpec {
    name: "m2ts",
    extension: "m2ts",
    magics: &[b"\x47"],
    magic_offset: 4,
    max_len: 64 << 30,
};

impl MpegTs {
    pub fn ts188() -> Self {
        MpegTs {
            packet: 188,
            sync_off: 0,
            spec: &TS_SPEC,
        }
    }

    /// AVCHD / Blu-ray: 4-byte arrival timestamp before each 188-byte packet.
    pub fn m2ts192() -> Self {
        MpegTs {
            packet: 192,
            sync_off: 4,
            spec: &M2TS_SPEC,
        }
    }
}

/// Consecutive valid packets required before trusting a 0x47 hit.
const MIN_PACKETS: u64 = 16;

impl Carver for MpegTs {
    fn spec(&self) -> &'static CarverSpec {
        self.spec
    }

    fn measure(&self, r: &mut dyn ReadAt, start: u64) -> io::Result<Option<Measured>> {
        let limit = (start + self.spec.max_len).min(r.total_len());
        let mut buf = vec![0u8; (self.packet * 256) as usize];
        let mut pos = start;
        let mut packets = 0u64;
        'outer: while pos < limit {
            let want = ((limit - pos) as usize).min(buf.len());
            let got = read_full(r, pos, &mut buf[..want])? as u64;
            let whole = got / self.packet;
            if whole == 0 {
                break;
            }
            for i in 0..whole {
                if buf[(i * self.packet + self.sync_off) as usize] != 0x47 {
                    pos += i * self.packet;
                    break 'outer;
                }
            }
            packets += whole;
            pos += whole * self.packet;
            if got < want as u64 {
                break;
            }
        }
        if packets < MIN_PACKETS {
            return Ok(None);
        }
        Ok(Some(Measured {
            len: pos - start,
            extension: self.spec.extension,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{minimal_m2ts, minimal_mpeg_ps, minimal_ts};

    #[test]
    fn ps_walks_to_end_code() {
        let ps = minimal_mpeg_ps(30);
        let mut padded = ps.clone();
        padded.extend_from_slice(&[0x55; 700]);
        let mut r: &[u8] = &padded;
        let m = MpegPs.measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.len, ps.len() as u64);
    }

    #[test]
    fn ps_rejects_lone_start_code() {
        let mut junk = b"\x00\x00\x01\xBA".to_vec();
        junk.extend_from_slice(&[0xEE; 64]);
        let mut r: &[u8] = &junk;
        assert!(MpegPs.measure(&mut r, 0).unwrap().is_none());
    }

    #[test]
    fn ts_extends_while_sync_holds() {
        let ts = minimal_ts(300);
        let mut padded = ts.clone();
        padded.extend_from_slice(&[0x00; 4096]);
        let mut r: &[u8] = &padded;
        let m = MpegTs::ts188().measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.len, ts.len() as u64);
        assert_eq!(m.extension, "ts");
    }

    #[test]
    fn m2ts_uses_offset_sync() {
        let m2ts = minimal_m2ts(300);
        let mut padded = m2ts.clone();
        padded.extend_from_slice(&[0x00; 4096]);
        let mut r: &[u8] = &padded;
        let m = MpegTs::m2ts192().measure(&mut r, 0).unwrap().unwrap();
        assert_eq!(m.len, m2ts.len() as u64);
        assert_eq!(m.extension, "m2ts");
    }

    #[test]
    fn short_sync_runs_are_rejected() {
        let ts = minimal_ts(4); // fewer than MIN_PACKETS
        let mut r: &[u8] = &ts;
        assert!(MpegTs::ts188().measure(&mut r, 0).unwrap().is_none());
    }
}
