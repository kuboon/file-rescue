//! Multi-pass imaging engine: copy → trim → scrape → retry, driven by
//! the rescue map. The next phase is always derived from the map's
//! contents, so resuming after an interrupt is just "load map, run again".

pub mod passes;
pub mod sink;
pub mod source;

use crate::map::{Phase, RescueMap, SectorStatus};
use sink::ImageSink;
use source::SectorSource;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum ImagingError {
    /// Writing the image failed (e.g. destination full) — always fatal.
    #[error("writing image: {0}")]
    Sink(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct ImagingOptions {
    /// Read size for the copy pass, in bytes (multiple of sector size).
    pub cluster_size: u64,
    /// Sweep the copy pass backwards.
    pub reverse: bool,
    /// Extra passes re-reading bad sectors after scraping.
    pub retry_passes: u32,
    /// Bytes to skip after a copy-phase read error (doubles up to a cap).
    pub skip_size: u64,
    pub max_skip_size: u64,
}

impl Default for ImagingOptions {
    fn default() -> Self {
        ImagingOptions {
            cluster_size: 64 * 1024,
            reverse: false,
            retry_passes: 0,
            skip_size: 64 * 1024,
            max_skip_size: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Interrupted,
}

#[derive(Debug)]
pub struct Summary {
    pub outcome: Outcome,
    pub rescued: u64,
    pub bad_bytes: u64,
    pub bad_areas: u64,
    pub read_errors: u64,
}

/// Called after each unit of work with the current map state, so the
/// caller can update progress display and autosave the map. Read errors
/// on the source are part of normal operation and are reflected in the
/// map, not surfaced as errors.
pub type ProgressFn<'a> = dyn FnMut(&RescueMap) + 'a;

pub struct ImagingEngine<S: SectorSource, K: ImageSink> {
    pub src: S,
    pub sink: K,
    pub opts: ImagingOptions,
    read_errors: u64,
}

impl<S: SectorSource, K: ImageSink> ImagingEngine<S, K> {
    pub fn new(src: S, sink: K, opts: ImagingOptions) -> Self {
        ImagingEngine {
            src,
            sink,
            opts,
            read_errors: 0,
        }
    }

    /// Run all remaining phases. Checks `stop` between reads; when it is
    /// set the engine returns with `Outcome::Interrupted` and a map that
    /// can be saved and resumed later.
    pub fn run(
        &mut self,
        map: &mut RescueMap,
        stop: &AtomicBool,
        on_progress: &mut ProgressFn,
    ) -> Result<Summary, ImagingError> {
        self.sink.set_len(map.size)?;
        let mut retries_done = 0u32;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(self.summary(map, Outcome::Interrupted));
            }
            let phase = self.next_phase(map, retries_done);
            map.current_phase = phase;
            let interrupted = match phase {
                Phase::Copying => passes::copy_pass(self, map, stop, on_progress)?,
                Phase::Trimming => passes::trim_pass(self, map, stop, on_progress)?,
                Phase::Scraping => passes::scrape_pass(self, map, stop, on_progress)?,
                Phase::Retrying => {
                    retries_done += 1;
                    map.pass += 1;
                    let reverse = retries_done.is_multiple_of(2);
                    passes::retry_pass(self, map, stop, reverse, on_progress)?
                }
                Phase::Finished => {
                    self.sink.flush()?;
                    return Ok(self.summary(map, Outcome::Finished));
                }
            };
            self.sink.flush()?;
            if interrupted {
                return Ok(self.summary(map, Outcome::Interrupted));
            }
        }
    }

    fn next_phase(&self, map: &RescueMap, retries_done: u32) -> Phase {
        if map.bytes_with(SectorStatus::NonTried) > 0 {
            Phase::Copying
        } else if map.bytes_with(SectorStatus::NonTrimmed) > 0 {
            Phase::Trimming
        } else if map.bytes_with(SectorStatus::NonScraped) > 0 {
            Phase::Scraping
        } else if map.bytes_with(SectorStatus::Bad) > 0 && retries_done < self.opts.retry_passes {
            Phase::Retrying
        } else {
            Phase::Finished
        }
    }

    fn summary(&self, map: &RescueMap, outcome: Outcome) -> Summary {
        if outcome == Outcome::Finished {
            // Mirror ddrescue: a finished map points at the end.
            // (current_pos already tracks the last operation otherwise.)
        }
        Summary {
            outcome,
            rescued: map.bytes_with(SectorStatus::Rescued),
            bad_bytes: map.bytes_with(SectorStatus::Bad),
            bad_areas: map.count_with(SectorStatus::Bad),
            read_errors: self.read_errors,
        }
    }

    pub(crate) fn note_read_error(&mut self) {
        self.read_errors += 1;
    }
}
