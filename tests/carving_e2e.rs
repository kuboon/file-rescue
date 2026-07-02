//! End-to-end carving tests over a synthetic disk with planted media
//! files, including the imaging → carving pipeline with a rescue map.

use file_rescue::carve::{carve_scan, write_reports, CarveOptions};
use file_rescue::map::{RescueMap, SectorStatus};
use file_rescue::testutil::*;
use std::path::Path;

struct Plant {
    offset: u64,
    data: Vec<u8>,
    name: &'static str,
}

fn plants() -> Vec<Plant> {
    vec![
        Plant {
            offset: 0x1000,
            data: minimal_jpeg(30_000),
            name: "jpeg",
        },
        Plant {
            offset: 0x40000,
            data: minimal_png(20_000),
            name: "png",
        },
        // Straddles the 4 MiB chunk boundary on purpose.
        Plant {
            offset: 4 * 1024 * 1024 - 0x2000,
            data: minimal_mp4(1_000_000, false, false),
            name: "mp4",
        },
        Plant {
            offset: 0x600000,
            data: minimal_mov(50_000),
            name: "mp4", // same carver, .mov extension
        },
        Plant {
            offset: 0x700000,
            data: minimal_heic(40_000),
            name: "mp4",
        },
        Plant {
            offset: 0x800000,
            data: minimal_pdf(10_000),
            name: "pdf",
        },
        Plant {
            offset: 0x900000,
            data: minimal_bmp(64, 64),
            name: "bmp",
        },
        Plant {
            offset: 0x920000,
            data: minimal_gif(5_000),
            name: "gif",
        },
        Plant {
            offset: 0x940000,
            data: minimal_avi(20_000, 1),
            name: "riff",
        },
        Plant {
            offset: 0x980000,
            data: minimal_mkv(30_000, true),
            name: "mkv",
        },
        Plant {
            offset: 0x9C0000,
            data: minimal_asf(20_000),
            name: "asf",
        },
        Plant {
            offset: 0xA00000,
            data: minimal_tiff(20_000, true, false),
            name: "tiff",
        },
        Plant {
            offset: 0xA40000,
            data: minimal_mpeg_ps(50),
            name: "mpg",
        },
        Plant {
            offset: 0xA60000,
            data: minimal_m2ts(300),
            name: "m2ts",
        },
    ]
}

fn build(plants: &[Plant], size: usize) -> Vec<u8> {
    let spec: Vec<(u64, &[u8])> = plants.iter().map(|p| (p.offset, &p.data[..])).collect();
    build_disk(size, &spec)
}

fn scan(
    disk: &[u8],
    outdir: &Path,
    map: Option<&RescueMap>,
) -> Vec<file_rescue::carve::CarvedFile> {
    let mut r: &[u8] = disk;
    carve_scan(
        &mut r,
        outdir,
        map,
        &CarveOptions::default(),
        &mut |_, _| {},
    )
    .unwrap()
}

#[test]
fn extracts_all_planted_files_byte_exact() {
    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let dir = tempfile::tempdir().unwrap();
    let found = scan(&disk, dir.path(), None);
    assert_eq!(found.len(), plants.len(), "found: {found:?}");
    for p in &plants {
        let f = found
            .iter()
            .find(|f| f.offset == p.offset)
            .unwrap_or_else(|| panic!("no file found at 0x{:X}", p.offset));
        assert_eq!(f.len, p.data.len() as u64, "wrong length for {}", p.name);
        assert_eq!(f.format, p.name);
        assert!(!f.damaged);
        let extracted = std::fs::read(&f.path).unwrap();
        assert_eq!(extracted, p.data, "content mismatch for {}", p.name);
    }
}

#[test]
fn video_is_extracted_whole_not_fragmented() {
    // The regression this tool exists for: a large video must come out
    // as ONE file with the exact declared length.
    let mp4 = minimal_mp4(8_000_000, false, false);
    let mp4_len = mp4.len();
    let disk = build_disk(16 * 1024 * 1024, &[(0x100000, &mp4[..])]);
    let dir = tempfile::tempdir().unwrap();
    let found = scan(&disk, dir.path(), None);
    let videos: Vec<_> = found.iter().filter(|f| f.format == "mp4").collect();
    assert_eq!(videos.len(), 1, "video must not be split: {found:?}");
    assert_eq!(videos[0].len, mp4_len as u64);
    let extracted = std::fs::read(&videos[0].path).unwrap();
    assert_eq!(extracted, mp4);
}

#[test]
fn extensions_follow_brand() {
    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let dir = tempfile::tempdir().unwrap();
    let found = scan(&disk, dir.path(), None);
    let ext_of = |off: u64| {
        found
            .iter()
            .find(|f| f.offset == off)
            .unwrap()
            .path
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_string()
    };
    assert_eq!(ext_of(0x600000), "mov");
    assert_eq!(ext_of(0x700000), "heic");
    assert_eq!(ext_of(0x1000), "jpg");
    assert_eq!(ext_of(0x940000), "avi");
    assert_eq!(ext_of(0x980000), "webm");
    assert_eq!(ext_of(0x9C0000), "wmv");
    assert_eq!(ext_of(0xA00000), "tif");
    assert_eq!(ext_of(0xA60000), "m2ts");
}

#[test]
fn map_flags_files_overlapping_bad_regions_as_damaged() {
    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let mut map = RescueMap::new_untried(disk.len() as u64);
    map.mark(0, disk.len() as u64, SectorStatus::Rescued);
    // A bad area inside the big MP4's mdat.
    let mp4_off = 4 * 1024 * 1024 - 0x2000;
    map.mark(mp4_off + 0x8000, 0x400, SectorStatus::Bad);
    let dir = tempfile::tempdir().unwrap();
    let found = scan(&disk, dir.path(), Some(&map));
    let mp4 = found.iter().find(|f| f.offset == mp4_off).unwrap();
    assert!(mp4.damaged, "file over a bad region must be flagged");
    let jpeg = found.iter().find(|f| f.offset == 0x1000).unwrap();
    assert!(!jpeg.damaged);
}

#[test]
fn map_skips_signatures_in_unrescued_regions() {
    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let mut map = RescueMap::new_untried(disk.len() as u64);
    map.mark(0, disk.len() as u64, SectorStatus::Rescued);
    // Pretend the JPEG's header sector was never rescued.
    map.mark(0x1000, 512, SectorStatus::NonTried);
    let dir = tempfile::tempdir().unwrap();
    let found = scan(&disk, dir.path(), Some(&map));
    assert!(
        !found.iter().any(|f| f.offset == 0x1000),
        "unrescued header must not be carved"
    );
}

#[test]
fn format_filter_restricts_carvers() {
    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let dir = tempfile::tempdir().unwrap();
    let mut r: &[u8] = &disk[..];
    let opts = CarveOptions {
        formats: Some(vec!["pdf".into()]),
        ..Default::default()
    };
    let found = carve_scan(&mut r, dir.path(), None, &opts, &mut |_, _| {}).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].format, "pdf");
}

#[test]
fn reports_are_written() {
    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let dir = tempfile::tempdir().unwrap();
    let found = scan(&disk, dir.path(), None);
    write_reports(&found, dir.path()).unwrap();
    let txt = std::fs::read_to_string(dir.path().join("report.txt")).unwrap();
    assert!(txt.contains("mp4"));
    let json = std::fs::read_to_string(dir.path().join("report.json")).unwrap();
    assert!(json.contains("\"format\": \"pdf\""));
}

#[test]
fn imaging_then_carving_pipeline() {
    use file_rescue::image::sink::MemSink;
    use file_rescue::image::source::{BadRegion, FaultySource, MemSource};
    use file_rescue::image::{ImagingEngine, ImagingOptions};
    use std::sync::atomic::AtomicBool;

    let plants = plants();
    let disk = build(&plants, 12 * 1024 * 1024);
    let mp4_off = 4 * 1024 * 1024 - 0x2000;
    // Bad sectors inside the big MP4 and inside filler.
    let src = FaultySource::new(
        MemSource::new(disk.clone(), 512),
        vec![
            BadRegion::forever(mp4_off + 0x10000..mp4_off + 0x10400),
            BadRegion::forever(0x200000..0x201000),
        ],
    );
    let mut map = RescueMap::new_untried(disk.len() as u64);
    let mut engine = ImagingEngine::new(
        src,
        MemSink::default(),
        ImagingOptions {
            cluster_size: 65536,
            ..Default::default()
        },
    );
    let stop = AtomicBool::new(false);
    let summary = engine.run(&mut map, &stop, &mut |_| {}).unwrap();
    assert_eq!(summary.bad_bytes, 0x400 + 0x1000);

    let dir = tempfile::tempdir().unwrap();
    let image = std::mem::take(&mut engine.sink.data);
    let mut r: &[u8] = &image;
    let found = carve_scan(
        &mut r,
        dir.path(),
        Some(&map),
        &CarveOptions::default(),
        &mut |_, _| {},
    )
    .unwrap();
    // The MP4 still comes out whole (declared length), flagged damaged.
    let mp4 = found.iter().find(|f| f.offset == mp4_off).unwrap();
    assert_eq!(mp4.len, plants[2].data.len() as u64);
    assert!(mp4.damaged);
    // Files untouched by bad sectors are intact and not flagged.
    let jpeg = found.iter().find(|f| f.offset == 0x1000).unwrap();
    assert!(!jpeg.damaged);
    let extracted = std::fs::read(&jpeg.path).unwrap();
    assert_eq!(extracted, plants[0].data);
}
