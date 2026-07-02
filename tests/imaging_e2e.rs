//! End-to-end tests of the imaging engine against fault-injected
//! in-memory sources: clean copy, bad regions, healing sectors (retry),
//! and interrupt → save map → resume.

use file_rescue::image::sink::MemSink;
use file_rescue::image::source::{BadRegion, FaultySource, MemSource, SectorSource};
use file_rescue::image::{ImagingEngine, ImagingOptions, Outcome};
use file_rescue::map::{format, RescueMap, SectorStatus};
use file_rescue::testutil::Xorshift;
use std::sync::atomic::{AtomicBool, Ordering};

const SS: u32 = 512;

fn make_data(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    Xorshift(0xF00D).fill(&mut data);
    data
}

fn opts() -> ImagingOptions {
    ImagingOptions {
        cluster_size: 4096,
        skip_size: 4096,
        ..Default::default()
    }
}

fn run_engine<S: SectorSource>(
    src: S,
    map: &mut RescueMap,
    opts: ImagingOptions,
) -> (file_rescue::image::Summary, Vec<u8>) {
    let mut engine = ImagingEngine::new(src, MemSink::default(), opts);
    let stop = AtomicBool::new(false);
    let summary = engine.run(map, &stop, &mut |_| {}).unwrap();
    let data = std::mem::take(&mut engine.sink.data);
    (summary, data)
}

#[test]
fn clean_source_copies_identically() {
    let data = make_data(256 * 1024);
    let src = MemSource::new(data.clone(), SS);
    let mut map = RescueMap::new_untried(data.len() as u64);
    let (summary, image) = run_engine(src, &mut map, opts());
    assert_eq!(summary.outcome, Outcome::Finished);
    assert_eq!(summary.rescued, data.len() as u64);
    assert_eq!(summary.bad_bytes, 0);
    assert_eq!(image, data);
    assert!(map.is_finished());
}

#[test]
fn unaligned_source_size_is_fully_copied() {
    let data = make_data(100_003); // not a sector multiple
    let src = MemSource::new(data.clone(), SS);
    let mut map = RescueMap::new_untried(data.len() as u64);
    let (summary, image) = run_engine(src, &mut map, opts());
    assert_eq!(summary.outcome, Outcome::Finished);
    assert_eq!(image, data);
}

#[test]
fn bad_region_is_narrowed_to_exact_sectors() {
    let data = make_data(256 * 1024);
    // 3 bad sectors in the middle of a cluster.
    let bad_start = 0x8000u64 + 512;
    let bad_end = bad_start + 3 * 512;
    let src = FaultySource::new(
        MemSource::new(data.clone(), SS),
        vec![BadRegion::forever(bad_start..bad_end)],
    );
    let mut map = RescueMap::new_untried(data.len() as u64);
    let (summary, image) = run_engine(src, &mut map, opts());
    assert_eq!(summary.outcome, Outcome::Finished);
    // Trim+scrape must locate exactly the injected sectors.
    assert_eq!(summary.bad_bytes, 3 * 512);
    assert_eq!(map.bytes_with(SectorStatus::Bad), 3 * 512);
    let bad = map.ranges(SectorStatus::Bad);
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].start, bad_start);
    assert_eq!(bad[0].end(), bad_end);
    // Everything else must match the source bit-for-bit.
    for (i, (a, b)) in image.iter().zip(data.iter()).enumerate() {
        let in_bad = (i as u64) >= bad_start && (i as u64) < bad_end;
        if !in_bad {
            assert_eq!(a, b, "mismatch at {i}");
        }
    }
    assert!(map.is_finished());
}

#[test]
fn reverse_direction_rescues_the_same() {
    let data = make_data(128 * 1024);
    let src = FaultySource::new(
        MemSource::new(data.clone(), SS),
        vec![BadRegion::forever(0x5000..0x5400)],
    );
    let mut map = RescueMap::new_untried(data.len() as u64);
    let mut o = opts();
    o.reverse = true;
    let (summary, _) = run_engine(src, &mut map, o);
    assert_eq!(summary.outcome, Outcome::Finished);
    assert_eq!(map.bytes_with(SectorStatus::Bad), 0x400);
    assert!(map.is_finished());
}

#[test]
fn healing_sectors_are_rescued_by_retry_pass() {
    let data = make_data(128 * 1024);
    // This region fails plenty of times (copy, trim, scrape attempts eat
    // some), then heals; retry passes must eventually rescue it.
    let src = FaultySource::new(
        MemSource::new(data.clone(), SS),
        vec![BadRegion::heals_after(0x4000..0x4200, 8)],
    );
    let mut map = RescueMap::new_untried(data.len() as u64);
    let mut o = opts();
    o.retry_passes = 10;
    let (summary, image) = run_engine(src, &mut map, o);
    assert_eq!(summary.outcome, Outcome::Finished);
    assert_eq!(summary.bad_bytes, 0, "healed sectors should be rescued");
    assert_eq!(image, data);
}

#[test]
fn without_retry_passes_healing_sectors_stay_bad() {
    let data = make_data(128 * 1024);
    let src = FaultySource::new(
        MemSource::new(data.clone(), SS),
        vec![BadRegion::heals_after(0x4000..0x4200, 1000)],
    );
    let mut map = RescueMap::new_untried(data.len() as u64);
    let (summary, _) = run_engine(src, &mut map, opts());
    assert_eq!(summary.outcome, Outcome::Finished);
    assert_eq!(summary.bad_bytes, 0x200);
}

#[test]
fn interrupt_then_resume_matches_uninterrupted_run() {
    let data = make_data(512 * 1024);
    let bad = vec![
        BadRegion::forever(0x10000..0x10600),
        BadRegion::forever(0x40200..0x40400),
    ];

    // Reference: uninterrupted run.
    let src = FaultySource::new(MemSource::new(data.clone(), SS), bad.clone());
    let mut ref_map = RescueMap::new_untried(data.len() as u64);
    let (ref_summary, ref_image) = run_engine(src, &mut ref_map, opts());
    assert_eq!(ref_summary.outcome, Outcome::Finished);

    // Interrupted run: stop after 20 progress callbacks, save the map to
    // disk, reload it, and finish with a fresh engine (as the CLI would).
    let src = FaultySource::new(MemSource::new(data.clone(), SS), bad.clone());
    let mut map = RescueMap::new_untried(data.len() as u64);
    let mut engine = ImagingEngine::new(src, MemSink::default(), opts());
    let stop = AtomicBool::new(false);
    let mut calls = 0;
    let summary = engine
        .run(&mut map, &stop, &mut |_| {
            calls += 1;
            if calls >= 20 {
                stop.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
    assert_eq!(summary.outcome, Outcome::Interrupted);
    assert!(!map.is_finished(), "test must interrupt mid-run");
    let partial_image = engine.sink.data.clone();

    let dir = tempfile::tempdir().unwrap();
    let map_path = dir.path().join("resume.map");
    format::save_atomic(&map, &map_path, "test").unwrap();
    let mut resumed_map = format::load(&map_path).unwrap();
    assert_eq!(resumed_map.extents(), map.extents());

    // Resume: new engine, new source (heal counters reset like a real
    // re-run), sink pre-filled with the partial image.
    let src = FaultySource::new(MemSource::new(data.clone(), SS), bad);
    let sink = MemSink {
        data: partial_image,
    };
    let mut engine = ImagingEngine::new(src, sink, opts());
    let stop = AtomicBool::new(false);
    let summary = engine.run(&mut resumed_map, &stop, &mut |_| {}).unwrap();
    assert_eq!(summary.outcome, Outcome::Finished);

    assert_eq!(resumed_map.extents(), ref_map.extents());
    assert_eq!(engine.sink.data, ref_image);
}

#[test]
fn sink_holes_stay_zero_for_bad_sectors() {
    let data = make_data(64 * 1024);
    let src = FaultySource::new(
        MemSource::new(data, SS),
        vec![BadRegion::forever(0x2000..0x2200)],
    );
    let mut map = RescueMap::new_untried(64 * 1024);
    let (_, image) = run_engine(src, &mut map, opts());
    assert!(image[0x2000..0x2200].iter().all(|&b| b == 0));
}
