//! Write-side abstraction. Bad sectors are never written, so the image
//! stays sparse: holes mark exactly the data that was never rescued
//! (same behavior as ddrescue).

use std::fs::File;
use std::io;
use std::path::Path;

pub trait ImageSink {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()>;
    /// Pre-size the image (sparse where supported).
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

pub struct FileSink {
    file: File,
}

impl FileSink {
    /// Open or create the image without truncating (resume keeps data).
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(FileSink { file })
    }
}

impl ImageSink for FileSink {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.write_all_at(data, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut done = 0;
            while done < data.len() {
                let n = self.file.seek_write(&data[done..], offset + done as u64)?;
                done += n;
            }
            Ok(())
        }
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        if self.file.metadata()?.len() < len {
            self.file.set_len(len)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}

/// In-memory sink for unit tests.
#[derive(Default)]
pub struct MemSink {
    pub data: Vec<u8>,
}

impl ImageSink for MemSink {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let end = offset as usize + data.len();
        if self.data.len() < end {
            self.data.resize(end, 0);
        }
        self.data[offset as usize..end].copy_from_slice(data);
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        if self.data.len() < len as usize {
            self.data.resize(len as usize, 0);
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
