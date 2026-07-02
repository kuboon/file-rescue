//! In-memory rescue map: a sorted, coalesced list of extents covering
//! `[0, size)`, each with a [`SectorStatus`]. This mirrors GNU ddrescue's
//! mapfile model so the two tools can interoperate (see [`format`]).

pub mod format;

use std::fmt;

/// State of a byte range of the source, using ddrescue's five states.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SectorStatus {
    /// `?` — not tried yet.
    NonTried,
    /// `*` — a read failed somewhere in this block; edges not yet located.
    NonTrimmed,
    /// `/` — edges located; interior not yet read sector-by-sector.
    NonScraped,
    /// `-` — a single-sector read failed.
    Bad,
    /// `+` — successfully rescued.
    Rescued,
}

impl SectorStatus {
    pub fn as_char(self) -> char {
        match self {
            SectorStatus::NonTried => '?',
            SectorStatus::NonTrimmed => '*',
            SectorStatus::NonScraped => '/',
            SectorStatus::Bad => '-',
            SectorStatus::Rescued => '+',
        }
    }

    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            '?' => SectorStatus::NonTried,
            '*' => SectorStatus::NonTrimmed,
            '/' => SectorStatus::NonScraped,
            '-' => SectorStatus::Bad,
            '+' => SectorStatus::Rescued,
            _ => return None,
        })
    }

    pub const ALL: [SectorStatus; 5] = [
        SectorStatus::NonTried,
        SectorStatus::NonTrimmed,
        SectorStatus::NonScraped,
        SectorStatus::Bad,
        SectorStatus::Rescued,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SectorStatus::NonTried => "non-tried",
            SectorStatus::NonTrimmed => "non-trimmed",
            SectorStatus::NonScraped => "non-scraped",
            SectorStatus::Bad => "bad-sector",
            SectorStatus::Rescued => "rescued",
        }
    }
}

impl fmt::Display for SectorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Current operation phase, stored as the mapfile's `current_status` column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Copying,
    Trimming,
    Scraping,
    Retrying,
    Finished,
}

impl Phase {
    pub fn as_char(self) -> char {
        match self {
            Phase::Copying => '?',
            Phase::Trimming => '*',
            Phase::Scraping => '/',
            Phase::Retrying => '-',
            Phase::Finished => '+',
        }
    }

    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            '?' => Phase::Copying,
            '*' => Phase::Trimming,
            '/' => Phase::Scraping,
            '-' => Phase::Retrying,
            '+' => Phase::Finished,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Copying => "copying",
            Phase::Trimming => "trimming",
            Phase::Scraping => "scraping",
            Phase::Retrying => "retrying",
            Phase::Finished => "finished",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// A contiguous byte range with a single status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub start: u64,
    pub len: u64,
    pub status: SectorStatus,
}

impl Extent {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }
}

/// Sorted, non-overlapping, coalesced extents covering `[0, size)`.
#[derive(Clone, Debug)]
pub struct RescueMap {
    extents: Vec<Extent>,
    pub size: u64,
    pub current_pos: u64,
    pub current_phase: Phase,
    pub pass: u32,
}

impl RescueMap {
    /// A fresh map: the whole source is non-tried.
    pub fn new_untried(size: u64) -> Self {
        let extents = if size > 0 {
            vec![Extent {
                start: 0,
                len: size,
                status: SectorStatus::NonTried,
            }]
        } else {
            Vec::new()
        };
        RescueMap {
            extents,
            size,
            current_pos: 0,
            current_phase: Phase::Copying,
            pass: 1,
        }
    }

    pub fn extents(&self) -> &[Extent] {
        &self.extents
    }

    /// Set `[start, start+len)` (clamped to the map) to `status`,
    /// splitting and re-coalescing extents as needed.
    pub fn mark(&mut self, start: u64, len: u64, status: SectorStatus) {
        let start = start.min(self.size);
        let end = start.saturating_add(len).min(self.size);
        if start >= end {
            return;
        }
        let mut out: Vec<Extent> = Vec::with_capacity(self.extents.len() + 2);
        let mut inserted = false;
        let insert = Extent {
            start,
            len: end - start,
            status,
        };
        for e in &self.extents {
            if e.end() <= start {
                push_coalesced(&mut out, *e);
                continue;
            }
            if !inserted && e.start >= start {
                push_coalesced(&mut out, insert);
                inserted = true;
            }
            if e.start >= end {
                push_coalesced(&mut out, *e);
                continue;
            }
            // e overlaps [start, end): keep the non-overlapped parts.
            if e.start < start {
                push_coalesced(
                    &mut out,
                    Extent {
                        start: e.start,
                        len: start - e.start,
                        status: e.status,
                    },
                );
                if !inserted {
                    push_coalesced(&mut out, insert);
                    inserted = true;
                }
            }
            if e.end() > end {
                push_coalesced(
                    &mut out,
                    Extent {
                        start: end,
                        len: e.end() - end,
                        status: e.status,
                    },
                );
            }
        }
        if !inserted {
            push_coalesced(&mut out, insert);
        }
        self.extents = out;
    }

    /// All extents with the given status, in address order.
    pub fn ranges(&self, status: SectorStatus) -> Vec<Extent> {
        self.extents
            .iter()
            .copied()
            .filter(|e| e.status == status)
            .collect()
    }

    /// First extent with `status` whose end is after `pos` (i.e. the next
    /// one to work on when sweeping forward from `pos`).
    pub fn next_range(&self, status: SectorStatus, pos: u64) -> Option<Extent> {
        self.extents
            .iter()
            .copied()
            .find(|e| e.status == status && e.end() > pos)
    }

    pub fn bytes_with(&self, status: SectorStatus) -> u64 {
        self.extents
            .iter()
            .filter(|e| e.status == status)
            .map(|e| e.len)
            .sum()
    }

    pub fn count_with(&self, status: SectorStatus) -> u64 {
        self.extents.iter().filter(|e| e.status == status).count() as u64
    }

    /// True when nothing readable remains untried (bad sectors may remain).
    pub fn is_finished(&self) -> bool {
        !self.extents.iter().any(|e| {
            matches!(
                e.status,
                SectorStatus::NonTried | SectorStatus::NonTrimmed | SectorStatus::NonScraped
            )
        })
    }

    /// Status at a byte offset (None past the end).
    pub fn status_at(&self, pos: u64) -> Option<SectorStatus> {
        let i = self.extents.partition_point(|e| e.end() <= pos);
        self.extents
            .get(i)
            .filter(|e| e.start <= pos)
            .map(|e| e.status)
    }

    /// Does `[start, start+len)` overlap anything that is not rescued?
    pub fn overlaps_non_rescued(&self, start: u64, len: u64) -> bool {
        let end = start.saturating_add(len).min(self.size);
        if start >= end {
            return false;
        }
        let i = self.extents.partition_point(|e| e.end() <= start);
        self.extents[i..]
            .iter()
            .take_while(|e| e.start < end)
            .any(|e| e.status != SectorStatus::Rescued)
    }

    #[cfg(test)]
    fn assert_invariants(&self) {
        assert!(!self.extents.is_empty() || self.size == 0);
        let mut pos = 0;
        let mut prev: Option<&Extent> = None;
        for e in &self.extents {
            assert!(e.len > 0, "empty extent");
            assert_eq!(e.start, pos, "gap or overlap");
            if let Some(p) = prev {
                assert_ne!(p.status, e.status, "not coalesced");
            }
            pos = e.end();
            prev = Some(e);
        }
        assert_eq!(pos, self.size, "does not cover size");
    }
}

fn push_coalesced(out: &mut Vec<Extent>, e: Extent) {
    if e.len == 0 {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.status == e.status && last.end() == e.start {
            last.len += e.len;
            return;
        }
    }
    out.push(e);
}

#[cfg(test)]
mod tests {
    use super::*;
    use SectorStatus::*;

    #[test]
    fn new_map_is_untried() {
        let m = RescueMap::new_untried(1000);
        assert_eq!(m.extents().len(), 1);
        assert_eq!(m.bytes_with(NonTried), 1000);
        m.assert_invariants();
    }

    #[test]
    fn mark_splits_and_coalesces() {
        let mut m = RescueMap::new_untried(1000);
        m.mark(100, 200, Rescued);
        m.assert_invariants();
        assert_eq!(m.extents().len(), 3);
        m.mark(300, 100, Rescued); // adjacent: coalesce
        m.assert_invariants();
        assert_eq!(m.bytes_with(Rescued), 300);
        assert_eq!(m.extents().len(), 3);
        m.mark(0, 100, Rescued); // prefix: coalesce left
        m.assert_invariants();
        assert_eq!(m.extents().len(), 2);
        m.mark(0, 1000, Rescued);
        m.assert_invariants();
        assert_eq!(m.extents().len(), 1);
        assert!(m.is_finished());
    }

    #[test]
    fn mark_overwrites_middle() {
        let mut m = RescueMap::new_untried(1000);
        m.mark(0, 1000, Rescued);
        m.mark(400, 100, Bad);
        m.assert_invariants();
        assert_eq!(m.bytes_with(Bad), 100);
        assert_eq!(m.bytes_with(Rescued), 900);
        assert_eq!(m.status_at(399), Some(Rescued));
        assert_eq!(m.status_at(400), Some(Bad));
        assert_eq!(m.status_at(499), Some(Bad));
        assert_eq!(m.status_at(500), Some(Rescued));
        assert_eq!(m.status_at(1000), None);
    }

    #[test]
    fn mark_clamps_to_size() {
        let mut m = RescueMap::new_untried(1000);
        m.mark(900, 500, Bad);
        m.assert_invariants();
        assert_eq!(m.bytes_with(Bad), 100);
        m.mark(2000, 10, Bad); // fully out of range: no-op
        m.assert_invariants();
        assert_eq!(m.bytes_with(Bad), 100);
    }

    #[test]
    fn overlaps_non_rescued_works() {
        let mut m = RescueMap::new_untried(1000);
        m.mark(0, 1000, Rescued);
        m.mark(500, 10, Bad);
        assert!(!m.overlaps_non_rescued(0, 500));
        assert!(m.overlaps_non_rescued(490, 20));
        assert!(m.overlaps_non_rescued(505, 1));
        assert!(!m.overlaps_non_rescued(510, 490));
    }

    #[test]
    fn random_marks_keep_invariants() {
        // Deterministic pseudo-random sequence (xorshift).
        let mut m = RescueMap::new_untried(1 << 20);
        let mut x: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let start = x % (1 << 20);
            let len = (x >> 24) % 4096 + 1;
            let status = SectorStatus::ALL[(x >> 40) as usize % 5];
            m.mark(start, len, status);
            m.assert_invariants();
        }
        let total: u64 = SectorStatus::ALL.iter().map(|&s| m.bytes_with(s)).sum();
        assert_eq!(total, 1 << 20);
    }

    #[test]
    fn next_range_finds_following_extent() {
        let mut m = RescueMap::new_untried(1000);
        m.mark(0, 500, Rescued);
        let e = m.next_range(NonTried, 0).unwrap();
        assert_eq!(e.start, 500);
        assert!(m.next_range(NonTried, 999).is_some());
        assert!(m.next_range(NonTried, 1000).is_none());
    }
}
