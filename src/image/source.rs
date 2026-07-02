//! Read-side abstraction for the imaging engine, plus a fault-injecting
//! wrapper used by tests and the `--simulate-bad` demo flag.

use crate::device::OpenedSource;
use std::fs::File;
use std::io;
use std::ops::Range;

pub trait SectorSource {
    fn size(&self) -> u64;
    fn sector_size(&self) -> u32;
    /// Positioned read filling `buf` completely. An `Err` means the whole
    /// request failed; the engine narrows it down via trim/scrape.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

impl SectorSource for Box<dyn SectorSource> {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn sector_size(&self) -> u32 {
        (**self).sector_size()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected end of file",
                ));
            }
            done += n;
        }
        Ok(())
    }
}

/// A regular file or (on Linux) a block device.
pub struct FileSource {
    file: File,
    /// O_DIRECT requires sector-aligned reads; unaligned tails fall back
    /// to this second, buffered handle.
    buffered: Option<File>,
    size: u64,
    sector_size: u32,
}

impl FileSource {
    pub fn from_opened(src: OpenedSource) -> Self {
        FileSource {
            file: src.file,
            buffered: src.buffered,
            size: src.size,
            sector_size: src.sector_size,
        }
    }

    pub fn open_plain(path: &std::path::Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(FileSource {
            file,
            buffered: None,
            size,
            sector_size: 512,
        })
    }
}

impl SectorSource for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let ss = self.sector_size as u64;
        let aligned = offset.is_multiple_of(ss) && (buf.len() as u64).is_multiple_of(ss);
        let file = match (&self.buffered, aligned) {
            (Some(buffered), false) => buffered,
            _ => &self.file,
        };
        read_exact_at(file, offset, buf)
    }
}

/// One injected fault. `remaining_failures: None` fails forever;
/// `Some(n)` heals after n failed attempts (exercises the retry pass).
#[derive(Clone, Debug)]
pub struct BadRegion {
    pub range: Range<u64>,
    pub remaining_failures: Option<u32>,
}

impl BadRegion {
    pub fn forever(range: Range<u64>) -> Self {
        BadRegion {
            range,
            remaining_failures: None,
        }
    }

    pub fn heals_after(range: Range<u64>, failures: u32) -> Self {
        BadRegion {
            range,
            remaining_failures: Some(failures),
        }
    }
}

/// Wraps a source and injects EIO for configured ranges.
pub struct FaultySource<S: SectorSource> {
    inner: S,
    bad: Vec<BadRegion>,
}

impl<S: SectorSource> FaultySource<S> {
    pub fn new(inner: S, bad: Vec<BadRegion>) -> Self {
        FaultySource { inner, bad }
    }
}

impl<S: SectorSource> SectorSource for FaultySource<S> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset + buf.len() as u64;
        let mut fail = false;
        for region in &mut self.bad {
            if region.range.start < end && region.range.end > offset {
                match &mut region.remaining_failures {
                    None => fail = true,
                    Some(0) => {}
                    Some(n) => {
                        *n -= 1;
                        fail = true;
                    }
                }
            }
        }
        if fail {
            return Err(io::Error::other("simulated I/O error"));
        }
        self.inner.read_at(offset, buf)
    }
}

/// In-memory source for unit tests.
pub struct MemSource {
    pub data: Vec<u8>,
    pub sector_size: u32,
}

impl MemSource {
    pub fn new(data: Vec<u8>, sector_size: u32) -> Self {
        MemSource { data, sector_size }
    }
}

impl SectorSource for MemSource {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = offset as usize;
        let end = start + buf.len();
        if end > self.data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past end",
            ));
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}
