use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rescue",
    version,
    about = "Rescue data from failing storage: ddrescue-style imaging + media-focused carving"
)]
pub struct Cli {
    /// Suppress progress output.
    #[arg(long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Copy a failing device/file into an image, tracking progress in a
    /// GNU-ddrescue-compatible mapfile (resumable; Linux only).
    #[cfg(target_os = "linux")]
    Image(ImageArgs),
    /// Extract photos, videos and documents from an image by signature.
    Carve(CarveArgs),
    /// Inspect a mapfile.
    #[command(subcommand)]
    Map(MapCmd),
}

#[derive(Args)]
pub struct ImageArgs {
    /// Source: block device (/dev/sdX) or file.
    pub source: PathBuf,
    /// Destination image file (sparse; holes = never-read data).
    pub image: PathBuf,
    /// Mapfile tracking rescue state (required, enables resume).
    pub mapfile: PathBuf,
    /// Use O_DIRECT to bypass the page cache on the source.
    #[arg(long)]
    pub direct: bool,
    /// Sweep the copy pass backwards.
    #[arg(long)]
    pub reverse: bool,
    /// Copy-pass read size in bytes.
    #[arg(long, default_value_t = 65536)]
    pub cluster_size: u64,
    /// Re-read bad sectors this many extra times after scraping.
    #[arg(long, default_value_t = 0)]
    pub retry_passes: u32,
    /// Override the source's logical sector size.
    #[arg(long)]
    pub sector_size: Option<u32>,
    /// Overwrite an existing image that has no mapfile.
    #[arg(long)]
    pub force: bool,
    /// Inject simulated bad ranges: OFF+LEN[@FAILS][,...] (hex ok),
    /// e.g. 0x140000+0x2000,0x500000+0x400@2
    #[cfg(feature = "fault-injection")]
    #[arg(long, value_name = "SPEC")]
    pub simulate_bad: Option<String>,
}

#[derive(Args)]
pub struct CarveArgs {
    /// Input image (or any file) to scan.
    pub input: PathBuf,
    /// Directory for extracted files and reports.
    pub outdir: PathBuf,
    /// Rescue mapfile: skip unrescued regions and flag damaged files.
    #[arg(long)]
    pub map: Option<PathBuf>,
    /// Comma-separated format names (mp4,jpeg,png,pdf). Default: all.
    #[arg(long)]
    pub formats: Option<String>,
    /// Test signatures only at offsets aligned to N bytes (1 = every byte).
    #[arg(long, default_value_t = 512)]
    pub align: u64,
}

#[derive(Subcommand)]
pub enum MapCmd {
    /// Summarize a mapfile: totals per status and % rescued.
    Show { mapfile: PathBuf },
    /// List extents, optionally filtered by status name or character.
    Regions {
        mapfile: PathBuf,
        /// non-tried | non-trimmed | non-scraped | bad | rescued (or ?*/-+)
        #[arg(long)]
        status: Option<String>,
    },
}

/// Parse "OFF+LEN[@FAILS],..." with decimal or 0x-hex numbers.
#[cfg(any(feature = "fault-injection", test))]
pub fn parse_bad_spec(spec: &str) -> Result<Vec<crate::image::source::BadRegion>, String> {
    use crate::image::source::BadRegion;
    let num = |s: &str| -> Result<u64, String> {
        let s = s.trim();
        if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u64::from_str_radix(h, 16).map_err(|e| format!("{s:?}: {e}"))
        } else {
            s.parse().map_err(|e| format!("{s:?}: {e}"))
        }
    };
    let mut out = Vec::new();
    for part in spec.split(',') {
        let (range, fails) = match part.split_once('@') {
            Some((r, f)) => (r, Some(num(f)? as u32)),
            None => (part, None),
        };
        let (off, len) = range
            .split_once('+')
            .ok_or_else(|| format!("expected OFF+LEN in {part:?}"))?;
        let off = num(off)?;
        let len = num(len)?;
        out.push(match fails {
            Some(n) => BadRegion::heals_after(off..off + len, n),
            None => BadRegion::forever(off..off + len),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn bad_spec_parses() {
        let v = parse_bad_spec("0x1000+0x200,4096+512@2").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].range, 0x1000..0x1200);
        assert_eq!(v[0].remaining_failures, None);
        assert_eq!(v[1].range, 4096..4608);
        assert_eq!(v[1].remaining_failures, Some(2));
        assert!(parse_bad_spec("nonsense").is_err());
    }
}
