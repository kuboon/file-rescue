//! GNU ddrescue mapfile (v2) parsing and serialization.
//!
//! Format: comment lines start with `#`. The first non-comment line is
//! `current_pos current_status [current_pass]`, followed by extent lines
//! `pos size status`. All numbers are hexadecimal with `0x` prefix.
//! Compatible files can be resumed with real ddrescue and viewed with
//! ddrescueview.

use super::{Extent, Phase, RescueMap, SectorStatus};
use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum MapParseError {
    #[error("line {line}: {msg}")]
    Syntax { line: usize, msg: String },
    #[error("mapfile has no status line")]
    Empty,
    #[error("extents are not contiguous at line {line}")]
    NotContiguous { line: usize },
}

fn parse_hex(tok: &str, line: usize) -> Result<u64, MapParseError> {
    let hex = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X"));
    let (s, radix) = match hex {
        Some(h) => (h, 16),
        None => (tok, 10),
    };
    u64::from_str_radix(s, radix).map_err(|_| MapParseError::Syntax {
        line,
        msg: format!("invalid number {tok:?}"),
    })
}

/// Parse a mapfile. The map size is derived from the end of the last extent.
pub fn parse(text: &str) -> Result<RescueMap, MapParseError> {
    let mut status_line: Option<(u64, Phase, u32)> = None;
    let mut extents: Vec<Extent> = Vec::new();
    let mut pos = 0u64;
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        if status_line.is_none() {
            if toks.len() < 2 || toks.len() > 3 {
                return Err(MapParseError::Syntax {
                    line: line_no,
                    msg: "status line must be: current_pos current_status [current_pass]".into(),
                });
            }
            let cur = parse_hex(toks[0], line_no)?;
            let phase = single_char(toks[1], line_no).and_then(|c| {
                Phase::from_char(c).ok_or_else(|| MapParseError::Syntax {
                    line: line_no,
                    msg: format!("unknown current_status {:?}", toks[1]),
                })
            })?;
            let pass = if toks.len() == 3 {
                parse_hex(toks[2], line_no)? as u32
            } else {
                1
            };
            status_line = Some((cur, phase, pass));
            continue;
        }
        if toks.len() != 3 {
            return Err(MapParseError::Syntax {
                line: line_no,
                msg: "extent line must be: pos size status".into(),
            });
        }
        let start = parse_hex(toks[0], line_no)?;
        let len = parse_hex(toks[1], line_no)?;
        let status = single_char(toks[2], line_no).and_then(|c| {
            SectorStatus::from_char(c).ok_or_else(|| MapParseError::Syntax {
                line: line_no,
                msg: format!("unknown extent status {:?}", toks[2]),
            })
        })?;
        if start != pos {
            return Err(MapParseError::NotContiguous { line: line_no });
        }
        if len == 0 {
            continue;
        }
        pos = start + len;
        extents.push(Extent { start, len, status });
    }
    let (current_pos, current_phase, pass) = status_line.ok_or(MapParseError::Empty)?;
    let mut map = RescueMap::new_untried(pos);
    // Rebuild through mark() so coalescing invariants hold even for
    // sloppy input (adjacent same-status lines).
    for e in extents {
        map.mark(e.start, e.len, e.status);
    }
    map.current_pos = current_pos;
    map.current_phase = current_phase;
    map.pass = pass;
    Ok(map)
}

fn single_char(tok: &str, line: usize) -> Result<char, MapParseError> {
    let mut chars = tok.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(MapParseError::Syntax {
            line,
            msg: format!("expected single status character, got {tok:?}"),
        }),
    }
}

pub fn serialize(map: &RescueMap, command_line: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Mapfile. Created by file-rescue version {}\n",
        env!("CARGO_PKG_VERSION")
    ));
    if !command_line.is_empty() {
        out.push_str(&format!("# Command line: {command_line}\n"));
    }
    out.push_str("# current_pos  current_status  current_pass\n");
    out.push_str(&format!(
        "0x{:08X}     {}               {}\n",
        map.current_pos,
        map.current_phase.as_char(),
        map.pass
    ));
    out.push_str("#      pos        size  status\n");
    for e in map.extents() {
        out.push_str(&format!(
            "0x{:08X}  0x{:08X}  {}\n",
            e.start,
            e.len,
            e.status.as_char()
        ));
    }
    out
}

/// Write the mapfile so a crash mid-save can never corrupt it:
/// write to a temp file in the same directory, fsync, rename over.
pub fn save_atomic(map: &RescueMap, path: &Path, command_line: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(serialize(map, command_line).as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<RescueMap, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    Ok(parse(&text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::SectorStatus::*;

    /// Output of a real GNU ddrescue 1.27 run.
    const DDRESCUE_SAMPLE: &str = "\
# Mapfile. Created by GNU ddrescue version 1.27
# Command line: ddrescue /dev/sdb disk.img disk.map
# Start time:   2024-01-01 12:00:00
# Current time: 2024-01-01 12:34:56
# Copying non-tried blocks... Pass 1 (forwards)
# current_pos  current_status  current_pass
0x00120000     ?               1
#      pos        size  status
0x00000000  0x00110000  +
0x00110000  0x00010000  *
0x00120000  0x00008000  -
0x00128000  0x00008000  /
0x00130000  0x7FED0000  ?
";

    #[test]
    fn parses_real_ddrescue_output() {
        let m = parse(DDRESCUE_SAMPLE).unwrap();
        assert_eq!(m.size, 0x130000 + 0x7FED0000);
        assert_eq!(m.current_pos, 0x120000);
        assert_eq!(m.current_phase, Phase::Copying);
        assert_eq!(m.pass, 1);
        assert_eq!(m.bytes_with(Rescued), 0x110000);
        assert_eq!(m.bytes_with(NonTrimmed), 0x10000);
        assert_eq!(m.bytes_with(Bad), 0x8000);
        assert_eq!(m.bytes_with(NonScraped), 0x8000);
        assert_eq!(m.bytes_with(NonTried), 0x7FED0000);
    }

    #[test]
    fn round_trip_preserves_map() {
        let m = parse(DDRESCUE_SAMPLE).unwrap();
        let text = serialize(&m, "test");
        let m2 = parse(&text).unwrap();
        assert_eq!(m.extents(), m2.extents());
        assert_eq!(m.size, m2.size);
        assert_eq!(m.current_pos, m2.current_pos);
        assert_eq!(m.current_phase, m2.current_phase);
        assert_eq!(m.pass, m2.pass);
    }

    #[test]
    fn parses_v1_two_column_status_line() {
        let text = "0x0 ?\n0x0 0x100 ?\n";
        let m = parse(text).unwrap();
        assert_eq!(m.pass, 1);
        assert_eq!(m.size, 0x100);
    }

    #[test]
    fn rejects_gaps() {
        let text = "0x0 ? 1\n0x0 0x100 +\n0x200 0x100 ?\n";
        assert!(matches!(
            parse(text),
            Err(MapParseError::NotContiguous { .. })
        ));
    }

    #[test]
    fn rejects_unknown_status() {
        let text = "0x0 ? 1\n0x0 0x100 X\n";
        assert!(parse(text).is_err());
    }

    #[test]
    fn save_atomic_writes_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.map");
        let mut m = RescueMap::new_untried(4096);
        m.mark(0, 1024, Rescued);
        save_atomic(&m, &path, "cmd").unwrap();
        let m2 = load(&path).unwrap();
        assert_eq!(m.extents(), m2.extents());
        // Overwrite with different content.
        m.mark(1024, 1024, Bad);
        save_atomic(&m, &path, "cmd").unwrap();
        let m3 = load(&path).unwrap();
        assert_eq!(m.extents(), m3.extents());
    }
}
