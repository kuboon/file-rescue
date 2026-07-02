//! Opening sources safely. On Linux this understands block devices
//! (size via seek-to-end, logical sector size via sysfs, optional
//! O_DIRECT). On other platforms only regular files are supported.

use std::fs::File;
use std::io::{self, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("cannot open {path}: {source}")]
    Open { path: String, source: io::Error },
    #[error("refusing to write {output} onto the source device {source_dev}")]
    OutputOnSource { output: String, source_dev: String },
    #[error("output {0} is the same file as the source")]
    OutputIsSource(String),
    #[error("{path} exists but its mapfile does not; pass --force to overwrite, or supply the original mapfile to resume")]
    WouldClobber { path: String },
    #[error("block devices are not supported on this platform: {0}")]
    BlockUnsupported(String),
}

/// An opened source plus the geometry the engine needs.
pub struct OpenedSource {
    pub file: File,
    /// Second handle without O_DIRECT for unaligned tail reads (Linux only).
    pub buffered: Option<File>,
    pub size: u64,
    pub sector_size: u32,
    pub is_block: bool,
    pub direct: bool,
}

/// Open a source read-only. Never opens for writing.
pub fn open_source(
    path: &Path,
    direct: bool,
    sector_size_override: Option<u32>,
) -> Result<OpenedSource, DeviceError> {
    let meta = std::fs::metadata(path).map_err(|e| DeviceError::Open {
        path: path.display().to_string(),
        source: e,
    })?;
    let is_block = is_block_device(&meta);
    if is_block && cfg!(not(target_os = "linux")) {
        return Err(DeviceError::BlockUnsupported(path.display().to_string()));
    }
    let file =
        open_readonly(path, direct && is_block_direct_ok()).map_err(|e| DeviceError::Open {
            path: path.display().to_string(),
            source: e,
        })?;
    let buffered = if direct {
        Some(open_readonly(path, false).map_err(|e| DeviceError::Open {
            path: path.display().to_string(),
            source: e,
        })?)
    } else {
        None
    };
    let mut file = file;
    // seek-to-end reports the correct size for both regular files and
    // Linux block devices, with no ioctl needed.
    let size = file
        .seek(SeekFrom::End(0))
        .and_then(|s| file.seek(SeekFrom::Start(0)).map(|_| s))
        .map_err(|e| DeviceError::Open {
            path: path.display().to_string(),
            source: e,
        })?;
    let sector_size = sector_size_override
        .or_else(|| {
            if is_block {
                sysfs_sector_size(path)
            } else {
                None
            }
        })
        .unwrap_or(512);
    Ok(OpenedSource {
        file,
        buffered,
        size,
        sector_size,
        is_block,
        direct,
    })
}

/// Refuse output locations that would write onto the dying source:
/// the source file itself, the source device node, or any filesystem
/// that lives on the source block device.
pub fn check_output_safety(source: &Path, output: &Path) -> Result<(), DeviceError> {
    let src_meta = match std::fs::metadata(source) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    // Compare against the output file if it exists, else its parent dir
    // (that is where the new file's data would land).
    let out_meta = std::fs::metadata(output).or_else(|_| {
        let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
        std::fs::metadata(parent.unwrap_or_else(|| Path::new(".")))
    });
    let out_meta = match out_meta {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if same_file(&src_meta, &out_meta) {
        return Err(DeviceError::OutputIsSource(output.display().to_string()));
    }
    if output_on_source_device(&src_meta, &out_meta) {
        return Err(DeviceError::OutputOnSource {
            output: output.display().to_string(),
            source_dev: source.display().to_string(),
        });
    }
    Ok(())
}

/// Enforce the resume-or-force rule for an existing image file.
pub fn check_clobber(image: &Path, mapfile: &Path, force: bool) -> Result<(), DeviceError> {
    if image.exists() && !mapfile.exists() && !force {
        return Err(DeviceError::WouldClobber {
            path: image.display().to_string(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};

    pub fn is_block_device(meta: &std::fs::Metadata) -> bool {
        meta.file_type().is_block_device()
    }

    pub fn open_readonly(path: &Path, direct: bool) -> io::Result<File> {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        if direct {
            opts.custom_flags(libc::O_DIRECT);
        }
        opts.open(path)
    }

    pub fn is_block_direct_ok() -> bool {
        true
    }

    /// Logical block size from sysfs, e.g. /sys/class/block/sdb/queue/...
    /// (partitions resolve through their parent automatically via
    /// /sys/class/block/<name>; partition dirs have no `queue`, so walk up).
    pub fn sysfs_sector_size(path: &Path) -> Option<u32> {
        let name = path.file_name()?.to_str()?;
        let mut dir = std::path::PathBuf::from("/sys/class/block").join(name);
        for _ in 0..2 {
            let q = dir.join("queue/logical_block_size");
            if let Ok(s) = std::fs::read_to_string(&q) {
                return s.trim().parse().ok();
            }
            // partition: hop to the parent device via the symlink target's parent
            dir = std::fs::canonicalize(&dir).ok()?.parent()?.to_path_buf();
        }
        None
    }

    pub fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }

    /// True when the output lives on a filesystem backed by the source
    /// block device (st_dev of output == st_rdev of source device node).
    pub fn output_on_source_device(src: &std::fs::Metadata, out: &std::fs::Metadata) -> bool {
        if is_block_device(src) {
            if out.dev() == src.rdev() {
                return true;
            }
            // Also catch writing to another node of the same device.
            if is_block_device(out) && out.rdev() == src.rdev() {
                return true;
            }
        }
        false
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub fn is_block_device(_meta: &std::fs::Metadata) -> bool {
        false
    }

    pub fn open_readonly(path: &Path, _direct: bool) -> io::Result<File> {
        File::open(path)
    }

    pub fn is_block_direct_ok() -> bool {
        false
    }

    pub fn sysfs_sector_size(_path: &Path) -> Option<u32> {
        None
    }

    #[cfg(unix)]
    pub fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }

    #[cfg(not(unix))]
    pub fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
        false
    }

    pub fn output_on_source_device(_src: &std::fs::Metadata, _out: &std::fs::Metadata) -> bool {
        false
    }
}

use imp::*;

/// A byte buffer whose data pointer is aligned (needed for O_DIRECT).
/// Implemented with over-allocation so no unsafe code is required.
pub struct AlignedBuf {
    buf: Vec<u8>,
    off: usize,
    len: usize,
}

impl AlignedBuf {
    pub fn new(len: usize, align: usize) -> Self {
        let mut buf = vec![0u8; len + align];
        let off = buf.as_mut_ptr().align_offset(align);
        AlignedBuf { buf, off, len }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buf[self.off..self.off + self.len]
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[self.off..self.off + self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn aligned_buf_is_aligned() {
        let mut b = AlignedBuf::new(8192, 4096);
        assert_eq!(b.as_mut_slice().as_ptr() as usize % 4096, 0);
        assert_eq!(b.as_slice().len(), 8192);
    }

    #[test]
    fn open_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("src.bin");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(&[0u8; 1234])
            .unwrap();
        let s = open_source(&p, false, None).unwrap();
        assert_eq!(s.size, 1234);
        assert_eq!(s.sector_size, 512);
        assert!(!s.is_block);
    }

    #[test]
    fn refuses_output_equal_to_source() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("src.bin");
        std::fs::File::create(&p).unwrap();
        assert!(matches!(
            check_output_safety(&p, &p),
            Err(DeviceError::OutputIsSource(_))
        ));
        // Different file in the same dir is fine (source is a regular file).
        let q = dir.path().join("out.img");
        assert!(check_output_safety(&p, &q).is_ok());
    }

    #[test]
    fn clobber_rule() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("out.img");
        let map = dir.path().join("out.map");
        assert!(check_clobber(&img, &map, false).is_ok()); // nothing exists
        std::fs::File::create(&img).unwrap();
        assert!(check_clobber(&img, &map, false).is_err()); // image w/o map
        assert!(check_clobber(&img, &map, true).is_ok()); // forced
        std::fs::File::create(&map).unwrap();
        assert!(check_clobber(&img, &map, false).is_ok()); // resume
    }
}
