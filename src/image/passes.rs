//! The four pass algorithms. Each operates on (source, map, sink) and
//! returns `Ok(true)` when interrupted by the stop flag.
//!
//! - copy: large sequential reads, skipping ahead on errors (get the
//!   easy data first, like ddrescue's copy phase)
//! - trim: narrow each failed block from both edges, sector by sector
//! - scrape: read every remaining sector of the interior
//! - retry: optional extra attempts on bad sectors, alternating direction

use super::{ImagingEngine, ImagingError, ProgressFn};
use crate::device::AlignedBuf;
use crate::image::sink::ImageSink;
use crate::image::source::SectorSource;
use crate::map::{Extent, RescueMap, SectorStatus};
use std::sync::atomic::{AtomicBool, Ordering};

const BUF_ALIGN: usize = 4096;

fn stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed)
}

pub fn copy_pass<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    map: &mut RescueMap,
    stop: &AtomicBool,
    on_progress: &mut ProgressFn,
) -> Result<bool, ImagingError> {
    let ss = engine.src.sector_size() as u64;
    let cluster = engine.opts.cluster_size.max(ss) / ss * ss;
    let mut buf = AlignedBuf::new(cluster as usize, BUF_ALIGN);
    // Sweep until nothing is non-tried. Skipped areas stay non-tried and
    // are revisited by the next sweep; every sweep is guaranteed to make
    // progress (its first attempt always resolves at least one cluster).
    while map.bytes_with(SectorStatus::NonTried) > 0 {
        if stopped(stop) {
            return Ok(true);
        }
        let interrupted = if engine.opts.reverse {
            copy_sweep_reverse(engine, map, stop, cluster, &mut buf, on_progress)?
        } else {
            copy_sweep_forward(engine, map, stop, cluster, &mut buf, on_progress)?
        };
        if interrupted {
            return Ok(true);
        }
    }
    Ok(false)
}

fn copy_sweep_forward<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    map: &mut RescueMap,
    stop: &AtomicBool,
    cluster: u64,
    buf: &mut AlignedBuf,
    on_progress: &mut ProgressFn,
) -> Result<bool, ImagingError> {
    let mut skip = engine.opts.skip_size.max(cluster);
    let mut pos = 0u64;
    loop {
        if stopped(stop) {
            return Ok(true);
        }
        let Some(ext) = map.next_range(SectorStatus::NonTried, pos) else {
            return Ok(false);
        };
        let start = ext.start.max(pos);
        let len = cluster.min(ext.end() - start);
        let data = &mut buf.as_mut_slice()[..len as usize];
        match engine.src.read_at(start, data) {
            Ok(()) => {
                engine.sink.write_at(start, data)?;
                map.mark(start, len, SectorStatus::Rescued);
                pos = start + len;
                skip = engine.opts.skip_size.max(cluster);
            }
            Err(_) => {
                engine.note_read_error();
                map.mark(start, len, SectorStatus::NonTrimmed);
                pos = start + len + skip;
                skip = (skip * 2).min(engine.opts.max_skip_size);
            }
        }
        map.current_pos = start + len;
        on_progress(map);
    }
}

fn copy_sweep_reverse<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    map: &mut RescueMap,
    stop: &AtomicBool,
    cluster: u64,
    buf: &mut AlignedBuf,
    on_progress: &mut ProgressFn,
) -> Result<bool, ImagingError> {
    let ss = engine.src.sector_size() as u64;
    let mut skip = engine.opts.skip_size.max(cluster);
    let mut pos = map.size;
    loop {
        if stopped(stop) {
            return Ok(true);
        }
        let Some(ext) = prev_range(map, SectorStatus::NonTried, pos) else {
            return Ok(false);
        };
        let end = ext.end().min(pos);
        let mut start = end.saturating_sub(cluster);
        start = align_down(start, ss).max(ext.start);
        let len = end - start;
        let data = &mut buf.as_mut_slice()[..len as usize];
        match engine.src.read_at(start, data) {
            Ok(()) => {
                engine.sink.write_at(start, data)?;
                map.mark(start, len, SectorStatus::Rescued);
                pos = start;
                skip = engine.opts.skip_size.max(cluster);
            }
            Err(_) => {
                engine.note_read_error();
                map.mark(start, len, SectorStatus::NonTrimmed);
                pos = start.saturating_sub(skip);
                skip = (skip * 2).min(engine.opts.max_skip_size);
            }
        }
        map.current_pos = start;
        on_progress(map);
    }
}

fn prev_range(map: &RescueMap, status: SectorStatus, pos: u64) -> Option<Extent> {
    map.extents()
        .iter()
        .rev()
        .copied()
        .find(|e| e.status == status && e.start < pos)
}

fn align_down(v: u64, align: u64) -> u64 {
    v / align * align
}

pub fn trim_pass<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    map: &mut RescueMap,
    stop: &AtomicBool,
    on_progress: &mut ProgressFn,
) -> Result<bool, ImagingError> {
    let ss = engine.src.sector_size() as u64;
    let mut buf = AlignedBuf::new(ss as usize, BUF_ALIGN);
    for ext in map.ranges(SectorStatus::NonTrimmed) {
        let mut lo = ext.start;
        let mut hi = ext.end();
        // Forward from the left edge until the first failing sector.
        while lo < hi {
            if stopped(stop) {
                return Ok(true);
            }
            let len = ss.min(hi - lo);
            let ok = read_sector(engine, lo, len, &mut buf, map)?;
            map.current_pos = lo;
            lo += len;
            on_progress(map);
            if !ok {
                break;
            }
        }
        // Backward from the right edge until the first failing sector.
        while hi > lo {
            if stopped(stop) {
                return Ok(true);
            }
            let s = align_down(hi - 1, ss).max(lo);
            let len = hi - s;
            let ok = read_sector(engine, s, len, &mut buf, map)?;
            map.current_pos = s;
            hi = s;
            on_progress(map);
            if !ok {
                break;
            }
        }
        // Whatever remains between the two failures is left for scraping.
        if hi > lo {
            map.mark(lo, hi - lo, SectorStatus::NonScraped);
            on_progress(map);
        }
    }
    Ok(false)
}

pub fn scrape_pass<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    map: &mut RescueMap,
    stop: &AtomicBool,
    on_progress: &mut ProgressFn,
) -> Result<bool, ImagingError> {
    let ss = engine.src.sector_size() as u64;
    let mut buf = AlignedBuf::new(ss as usize, BUF_ALIGN);
    for ext in map.ranges(SectorStatus::NonScraped) {
        let mut pos = ext.start;
        while pos < ext.end() {
            if stopped(stop) {
                return Ok(true);
            }
            let len = ss.min(ext.end() - pos);
            read_sector(engine, pos, len, &mut buf, map)?;
            map.current_pos = pos;
            pos += len;
            on_progress(map);
        }
    }
    Ok(false)
}

pub fn retry_pass<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    map: &mut RescueMap,
    stop: &AtomicBool,
    reverse: bool,
    on_progress: &mut ProgressFn,
) -> Result<bool, ImagingError> {
    let ss = engine.src.sector_size() as u64;
    let mut buf = AlignedBuf::new(ss as usize, BUF_ALIGN);
    let mut extents = map.ranges(SectorStatus::Bad);
    if reverse {
        extents.reverse();
    }
    for ext in extents {
        let mut offsets: Vec<u64> = Vec::new();
        let mut o = 0u64;
        while o < ext.len {
            offsets.push(ext.start + o);
            o += ss;
        }
        if reverse {
            offsets.reverse();
        }
        for pos in offsets {
            if stopped(stop) {
                return Ok(true);
            }
            let len = ss.min(ext.end() - pos);
            let data = &mut buf.as_mut_slice()[..len as usize];
            if engine.src.read_at(pos, data).is_ok() {
                engine.sink.write_at(pos, data)?;
                map.mark(pos, len, SectorStatus::Rescued);
            } else {
                engine.note_read_error();
            }
            map.current_pos = pos;
            on_progress(map);
        }
    }
    Ok(false)
}

/// Read one sector; on success write it out and mark rescued, on failure
/// mark bad. Returns whether the read succeeded.
fn read_sector<S: SectorSource, K: ImageSink>(
    engine: &mut ImagingEngine<S, K>,
    pos: u64,
    len: u64,
    buf: &mut AlignedBuf,
    map: &mut RescueMap,
) -> Result<bool, ImagingError> {
    let data = &mut buf.as_mut_slice()[..len as usize];
    match engine.src.read_at(pos, data) {
        Ok(()) => {
            engine.sink.write_at(pos, data)?;
            map.mark(pos, len, SectorStatus::Rescued);
            Ok(true)
        }
        Err(_) => {
            engine.note_read_error();
            map.mark(pos, len, SectorStatus::Bad);
            Ok(false)
        }
    }
}
