//! Signature scan over an image (or any `ReadAt`), extraction, and
//! report generation. With a rescue map, unrescued regions are skipped
//! and files overlapping bad areas get flagged as damaged.

pub mod carver;
pub mod formats;

use crate::map::RescueMap;
use carver::{Carver, ReadAt};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const CHUNK: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct CarveOptions {
    /// Only test signatures at offsets aligned to this (512 matches
    /// filesystem allocation; 1 scans every byte).
    pub align: u64,
    /// Restrict to these format names (spec().name); None = all.
    pub formats: Option<Vec<String>>,
}

impl Default for CarveOptions {
    fn default() -> Self {
        CarveOptions {
            align: 512,
            formats: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CarvedFile {
    pub offset: u64,
    pub len: u64,
    pub format: &'static str,
    pub path: PathBuf,
    /// Overlaps regions the imaging pass could not rescue.
    pub damaged: bool,
    /// True when the file's declared length ran past the end of the input.
    pub truncated: bool,
}

/// Scan and extract. `progress(scanned_bytes, files_found)` is called
/// once per chunk.
pub fn carve_scan(
    reader: &mut dyn ReadAt,
    outdir: &Path,
    map: Option<&RescueMap>,
    opts: &CarveOptions,
    progress: &mut dyn FnMut(u64, usize),
) -> io::Result<Vec<CarvedFile>> {
    let carvers: Vec<Box<dyn Carver>> = carver::builtin_carvers()
        .into_iter()
        .filter(|c| match &opts.formats {
            Some(names) => names.iter().any(|n| n == c.spec().name),
            None => true,
        })
        .collect();
    if carvers.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no carvers selected (check --formats)",
        ));
    }
    fs::create_dir_all(outdir)?;
    let align = opts.align.max(1);
    let overlap = carvers
        .iter()
        .map(|c| {
            c.spec().magic_offset as usize
                + c.spec().magics.iter().map(|m| m.len()).max().unwrap_or(0)
        })
        .max()
        .unwrap_or(16);
    let total = reader.total_len();
    let mut found: Vec<CarvedFile> = Vec::new();
    let mut buf = vec![0u8; (CHUNK as usize) + overlap];
    // Next offset allowed to start a new file (skips past extracted ones).
    let mut cursor = 0u64;
    let mut base = 0u64;
    while base < total {
        let want = ((total - base).min(CHUNK + overlap as u64)) as usize;
        let got = carver::read_full(reader, base, &mut buf[..want])?;
        let chunk_end = (base + CHUNK).min(total);
        let mut off = cursor.max(base).next_multiple_of(align);
        'offsets: while off < chunk_end {
            let rel = (off - base) as usize;
            let skip_here =
                map.is_some_and(|m| m.status_at(off) != Some(crate::map::SectorStatus::Rescued));
            if !skip_here {
                // Several carvers can match the same offset (weak magics
                // like 0x47); try each candidate until one measures.
                for hit in match_magic(&carvers, &buf[..got], rel) {
                    if let Some(m) = carvers[hit].measure(reader, off)? {
                        let file = extract(reader, outdir, &carvers, hit, off, m, map)?;
                        cursor = file.offset + file.len;
                        found.push(file);
                        off = cursor.next_multiple_of(align).max(off + align);
                        continue 'offsets;
                    }
                }
            }
            off += align;
        }
        base += CHUNK;
        progress(base.min(total), found.len());
    }
    Ok(found)
}

fn match_magic(carvers: &[Box<dyn Carver>], buf: &[u8], rel: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    for (i, c) in carvers.iter().enumerate() {
        let spec = c.spec();
        let at = rel + spec.magic_offset as usize;
        for magic in spec.magics {
            if buf.len() >= at + magic.len() && &buf[at..at + magic.len()] == *magic {
                hits.push(i);
                break;
            }
        }
    }
    hits
}

fn extract(
    reader: &mut dyn ReadAt,
    outdir: &Path,
    carvers: &[Box<dyn Carver>],
    hit: usize,
    start: u64,
    measured: carver::Measured,
    map: Option<&RescueMap>,
) -> io::Result<CarvedFile> {
    let total = reader.total_len();
    let truncated = start + measured.len > total;
    let len = measured.len.min(total - start);
    let name = format!("f{start:010X}.{}", measured.extension);
    let path = outdir.join(&name);
    let mut out = fs::File::create(&path)?;
    let mut copied = 0u64;
    let mut buf = vec![0u8; 1024 * 1024];
    while copied < len {
        let want = ((len - copied) as usize).min(buf.len());
        let got = carver::read_full(reader, start + copied, &mut buf[..want])?;
        if got == 0 {
            break;
        }
        out.write_all(&buf[..got])?;
        copied += got as u64;
    }
    Ok(CarvedFile {
        offset: start,
        len,
        format: carvers[hit].spec().name,
        path,
        damaged: map.is_some_and(|m| m.overlaps_non_rescued(start, len)),
        truncated,
    })
}

/// Human-readable and JSON reports next to the extracted files.
pub fn write_reports(files: &[CarvedFile], outdir: &Path) -> io::Result<()> {
    let mut txt = String::new();
    txt.push_str("offset      size        format  flags     file\n");
    for f in files {
        let mut flags = String::new();
        if f.damaged {
            flags.push_str("damaged ");
        }
        if f.truncated {
            flags.push_str("truncated");
        }
        txt.push_str(&format!(
            "0x{:09X} {:>11} {:<7} {:<9} {}\n",
            f.offset,
            f.len,
            f.format,
            flags.trim_end(),
            f.path.file_name().unwrap_or_default().to_string_lossy(),
        ));
    }
    fs::write(outdir.join("report.txt"), txt)?;

    let mut json = String::from("[\n");
    for (i, f) in files.iter().enumerate() {
        json.push_str(&format!(
            "  {{\"offset\": {}, \"size\": {}, \"format\": \"{}\", \"damaged\": {}, \"truncated\": {}, \"file\": \"{}\"}}{}\n",
            f.offset,
            f.len,
            f.format,
            f.damaged,
            f.truncated,
            f.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\""),
            if i + 1 == files.len() { "" } else { "," },
        ));
    }
    json.push_str("]\n");
    fs::write(outdir.join("report.json"), json)?;
    Ok(())
}

/// `ReadAt` over a file (the carve input).
pub struct FileReader {
    file: fs::File,
    len: u64,
}

impl FileReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(FileReader { file, len })
    }
}

impl ReadAt for FileReader {
    fn total_len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_at(buf, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            self.file.seek_read(buf, offset)
        }
    }
}
