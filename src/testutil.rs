//! Builders for tiny-but-structurally-valid media files, plus a
//! deterministic filler. Used by the test suite and the `mkdemo` example;
//! not part of the recovery logic.

/// Bitwise CRC-32 (IEEE), enough for generating valid PNG chunks.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Deterministic xorshift filler so synthetic disks are reproducible.
pub struct Xorshift(pub u64);

impl Xorshift {
    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            let bytes = self.0.to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&bytes[..n]);
        }
    }
}

fn png_chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = typ.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(typ);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Minimal structurally-valid PNG: signature, IHDR, one IDAT, IEND.
pub fn minimal_png(idat_len: usize) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&16u32.to_be_bytes()); // width
    ihdr.extend_from_slice(&16u32.to_be_bytes()); // height
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // depth, color, comp, filter, interlace
    png_chunk(&mut out, b"IHDR", &ihdr);
    let mut idat = vec![0u8; idat_len];
    Xorshift(0x1DA7).fill(&mut idat);
    for b in idat.iter_mut() {
        // Keep filler from ever containing chunk-breaking 0xFF runs; not
        // required for PNG, but keeps fixtures boring.
        *b &= 0x7F;
    }
    png_chunk(&mut out, b"IDAT", &idat);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn jpeg_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.push(0xFF);
    out.push(marker);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Minimal structurally-valid JPEG: SOI, APP0, DQT, SOF0, DHT, SOS,
/// entropy data (0xFF-free), EOI.
pub fn minimal_jpeg(entropy_len: usize) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8]; // SOI
    jpeg_segment(
        &mut out,
        0xE0,
        b"JFIF\0\x01\x02\x00\x00\x01\x00\x01\x00\x00",
    );
    let mut dqt = vec![0u8; 65];
    dqt[0] = 0; // table id
    for (i, v) in dqt[1..].iter_mut().enumerate() {
        *v = (i % 63 + 1) as u8;
    }
    jpeg_segment(&mut out, 0xDB, &dqt);
    // SOF0: precision 8, 16x16, 1 component
    jpeg_segment(&mut out, 0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0]);
    // DHT: minimal table (all zero counts, no symbols)
    let mut dht = vec![0u8; 17];
    dht[0] = 0x00;
    jpeg_segment(&mut out, 0xC4, &dht);
    // SOS: 1 component
    jpeg_segment(&mut out, 0xDA, &[1, 1, 0x00, 0, 63, 0]);
    let mut entropy = vec![0u8; entropy_len];
    Xorshift(0x0DDB).fill(&mut entropy);
    for b in entropy.iter_mut() {
        if *b == 0xFF {
            *b = 0x7E; // encoders escape 0xFF in entropy data; keep it out
        }
    }
    out.extend_from_slice(&entropy);
    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    out
}

fn bmff_box(out: &mut Vec<u8>, typ: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(payload);
}

fn bmff_box_large(out: &mut Vec<u8>, typ: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(&((payload.len() + 16) as u64).to_be_bytes());
    out.extend_from_slice(payload);
}

fn ftyp(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
    let mut p = major.to_vec();
    p.extend_from_slice(&0u32.to_be_bytes());
    for c in compat {
        p.extend_from_slice(*c);
    }
    p
}

/// Minimal MP4: ftyp + free + mdat(payload) + moov. `moov_first` swaps
/// the mdat/moov order (faststart layout); `large_mdat` uses a 64-bit
/// box size.
pub fn minimal_mp4(mdat_len: usize, moov_first: bool, large_mdat: bool) -> Vec<u8> {
    let mut out = Vec::new();
    bmff_box(&mut out, b"ftyp", &ftyp(b"isom", &[b"isom", b"mp42"]));
    bmff_box(&mut out, b"free", &[0u8; 8]);
    let mut mdat = vec![0u8; mdat_len];
    Xorshift(0x3D47).fill(&mut mdat);
    let mut moov = vec![0u8; 64];
    Xorshift(0x300F).fill(&mut moov);
    let emit_mdat = |out: &mut Vec<u8>| {
        if large_mdat {
            bmff_box_large(out, b"mdat", &mdat);
        } else {
            bmff_box(out, b"mdat", &mdat);
        }
    };
    if moov_first {
        bmff_box(&mut out, b"moov", &moov);
        emit_mdat(&mut out);
    } else {
        emit_mdat(&mut out);
        bmff_box(&mut out, b"moov", &moov);
    }
    out
}

/// Minimal HEIC: ftyp(heic) + meta + mdat.
pub fn minimal_heic(mdat_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    bmff_box(&mut out, b"ftyp", &ftyp(b"heic", &[b"mif1", b"heic"]));
    bmff_box(&mut out, b"meta", &[0u8; 32]);
    let mut mdat = vec![0u8; mdat_len];
    Xorshift(0x4E1C).fill(&mut mdat);
    bmff_box(&mut out, b"mdat", &mdat);
    out
}

/// Minimal QuickTime MOV: ftyp(qt) + wide + mdat + moov.
pub fn minimal_mov(mdat_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    bmff_box(&mut out, b"ftyp", &ftyp(b"qt  ", &[b"qt  "]));
    bmff_box(&mut out, b"wide", &[]);
    let mut mdat = vec![0u8; mdat_len];
    Xorshift(0x0A07).fill(&mut mdat);
    bmff_box(&mut out, b"mdat", &mdat);
    let mut moov = vec![0u8; 48];
    Xorshift(0xA00F).fill(&mut moov);
    bmff_box(&mut out, b"moov", &moov);
    out
}

/// Minimal PDF with two %%EOF (incremental update), ending at the last.
pub fn minimal_pdf(body_len: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let mut body = vec![0u8; body_len];
    Xorshift(0x9D4F).fill(&mut body);
    for b in body.iter_mut() {
        *b = b'a' + (*b % 26); // printable stream filler
    }
    out.extend_from_slice(b"2 0 obj\n<< /Length ");
    out.extend_from_slice(body_len.to_string().as_bytes());
    out.extend_from_slice(b" >>\nstream\n");
    out.extend_from_slice(&body);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out.extend_from_slice(b"trailer\n<< /Size 3 >>\nstartxref\n0\n%%EOF\n");
    out.extend_from_slice(b"3 0 obj\n<< >>\nendobj\ntrailer\n<< /Size 4 >>\nstartxref\n0\n%%EOF\n");
    out
}

/// A synthetic disk: deterministic filler with files planted at fixed,
/// sector-aligned offsets. Returns (disk, plants).
pub fn build_disk(size: usize, plants: &[(u64, &[u8])]) -> Vec<u8> {
    let mut disk = vec![0u8; size];
    Xorshift(0xD15C).fill(&mut disk);
    for (off, data) in plants {
        let off = *off as usize;
        assert!(off + data.len() <= size, "plant out of range");
        disk[off..off + data.len()].copy_from_slice(data);
    }
    disk
}
