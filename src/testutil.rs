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
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (off, data) in plants {
        let off = *off as usize;
        assert!(off + data.len() <= size, "plant out of range");
        for (s, e) in &spans {
            assert!(
                off >= *e || off + data.len() <= *s,
                "plants overlap at 0x{off:X}"
            );
        }
        spans.push((off, off + data.len()));
        disk[off..off + data.len()].copy_from_slice(data);
    }
    disk
}

fn filler(seed: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    Xorshift(seed).fill(&mut v);
    v
}

/// Minimal 24-bit BMP with a BITMAPINFOHEADER.
pub fn minimal_bmp(width: u32, height: u32) -> Vec<u8> {
    let row = (width * 3).next_multiple_of(4) as usize;
    let pixels = filler(0x00B4, row * height as usize);
    let file_size = 54 + pixels.len() as u32;
    let mut out = b"BM".to_vec();
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]); // reserved
    out.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
    out.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // compression
    out.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // ppm x/y, colors, important
    out.extend_from_slice(&pixels);
    out
}

/// Minimal GIF89a: global color table, GCE extension, one image.
pub fn minimal_gif(pixel_data_len: usize) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&16u16.to_le_bytes()); // width
    out.extend_from_slice(&16u16.to_le_bytes()); // height
    out.push(0x91); // GCT present, 4 entries
    out.push(0); // background color
    out.push(0); // aspect
    out.extend_from_slice(&[0x20; 12]); // GCT: 4 * RGB
    out.extend_from_slice(&[0x21, 0xF9, 0x04, 0, 0, 0, 0, 0x00]); // GCE
    out.push(0x2C); // image descriptor
    out.extend_from_slice(&[0, 0, 0, 0]); // x, y
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.push(0); // no local color table
    out.push(2); // LZW minimum code size
    let data = filler(0x061F, pixel_data_len);
    for chunk in data.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0); // sub-block terminator
    out.push(0x3B); // trailer
    out
}

fn riff_file(form: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((payload.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(form);
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// Minimal AVI, optionally followed by OpenDML AVIX continuation segments.
pub fn minimal_avi(payload_len: usize, avix_segments: usize) -> Vec<u8> {
    let mut out = riff_file(b"AVI ", &filler(0x0AB1, payload_len & !1));
    for i in 0..avix_segments {
        out.extend_from_slice(&riff_file(b"AVIX", &filler(0x0A10 + i as u64, 1000)));
    }
    out
}

pub fn minimal_wav(payload_len: usize) -> Vec<u8> {
    riff_file(b"WAVE", &filler(0x0A17, payload_len & !1))
}

pub fn minimal_webp(payload_len: usize) -> Vec<u8> {
    riff_file(b"WEBP", &filler(0x0EB, payload_len & !1))
}

/// Minimal TIFF (or CR2 when `cr2`): one IFD with strip offset/count
/// entries pointing at trailing image data.
pub fn minimal_tiff(strip_len: usize, le: bool, cr2: bool) -> Vec<u8> {
    let w16 = |out: &mut Vec<u8>, v: u16| {
        out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() })
    };
    let w32 = |out: &mut Vec<u8>, v: u32| {
        out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() })
    };
    let mut out = Vec::new();
    out.extend_from_slice(if le { b"II*\x00" } else { b"MM\x00*" });
    w32(&mut out, 16); // IFD0 offset
    if cr2 {
        out.extend_from_slice(b"CR\x02\x00");
        out.extend_from_slice(&[0u8; 4]);
    } else {
        out.extend_from_slice(&[0u8; 8]);
    }
    // IFD0 at 16: 4 entries, 12 bytes each, then next-IFD pointer.
    let data_start = 16 + 2 + 4 * 12 + 4;
    w16(&mut out, 4);
    let entry = |out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
        w16(out, tag);
        w16(out, typ);
        w32(out, count);
        // SHORT inline values sit in the high-order-first half per
        // endianness; using LONG everywhere keeps this simple.
        w32(out, value);
    };
    entry(&mut out, 256, 4, 1, 16); // ImageWidth
    entry(&mut out, 257, 4, 1, 16); // ImageLength
    entry(&mut out, 273, 4, 1, data_start as u32); // StripOffsets
    entry(&mut out, 279, 4, 1, strip_len as u32); // StripByteCounts
    w32(&mut out, 0); // next IFD
    out.extend_from_slice(&filler(0x71FF, strip_len));
    out
}

fn ebml_element(out: &mut Vec<u8>, id: &[u8], payload: &[u8]) {
    out.extend_from_slice(id);
    assert!(payload.len() < 127);
    out.push(0x80 | payload.len() as u8);
    out.extend_from_slice(payload);
}

/// Minimal Matroska/WebM: EBML header with a doctype, then a Segment
/// with a declared 8-byte-vint size.
pub fn minimal_mkv(segment_len: usize, webm: bool) -> Vec<u8> {
    let mut header = Vec::new();
    ebml_element(
        &mut header,
        &[0x42, 0x82],
        if webm { b"webm" } else { b"matroska" },
    );
    let mut out = b"\x1A\x45\xDF\xA3".to_vec();
    out.push(0x80 | header.len() as u8);
    out.extend_from_slice(&header);
    out.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment
    out.push(0x01); // 8-byte vint follows
    let mut size = [0u8; 7];
    size.copy_from_slice(&(segment_len as u64).to_be_bytes()[1..]);
    out.extend_from_slice(&size);
    out.extend_from_slice(&filler(0x03A7, segment_len));
    out
}

/// Minimal ASF (WMV): header object + data object.
pub fn minimal_asf(data_len: usize) -> Vec<u8> {
    let header_guid: [u8; 16] = [
        0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE,
        0x6C,
    ];
    let data_guid: [u8; 16] = [
        0x36, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE,
        0x6C,
    ];
    let mut out = header_guid.to_vec();
    out.extend_from_slice(&30u64.to_le_bytes());
    out.extend_from_slice(&[0u8; 6]); // header body filler
    out.extend_from_slice(&data_guid);
    out.extend_from_slice(&((24 + data_len) as u64).to_le_bytes());
    out.extend_from_slice(&filler(0x0A5F, data_len));
    out
}

/// Minimal MPEG-2 program stream: pack header, PES packets, end code.
pub fn minimal_mpeg_ps(pes_packets: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
    out.push(0x44); // MPEG-2 marker bits
    out.extend_from_slice(&[0u8; 8]);
    out.push(0xF8); // no stuffing
    let payload = filler(0x9E5, 100);
    for _ in 0..pes_packets {
        out.extend_from_slice(&[0x00, 0x00, 0x01, 0xE0]); // video PES
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&payload);
    }
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xB9]); // end code
    out
}

fn ts_packets(n: usize, packet: usize, sync_off: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * packet);
    let mut rng = Xorshift(seed);
    for _ in 0..n {
        let mut pkt = vec![0u8; packet];
        rng.fill(&mut pkt);
        pkt[sync_off] = 0x47;
        pkt[sync_off + 1] = 0x1F; // plausible PID bytes
        pkt[sync_off + 2] = 0xFF;
        out.extend_from_slice(&pkt);
    }
    out
}

/// Minimal MPEG transport stream (188-byte packets).
pub fn minimal_ts(packets: usize) -> Vec<u8> {
    ts_packets(packets, 188, 0, 0x7502)
}

/// Minimal AVCHD-style stream (192-byte packets, 4-byte timestamp prefix).
pub fn minimal_m2ts(packets: usize) -> Vec<u8> {
    let mut out = ts_packets(packets, 192, 4, 0x2752);
    // Make sure the first timestamp byte is not itself a stray sync byte
    // so the fixture unambiguously exercises the +4 offset.
    out[0] = 0x00;
    out
}
