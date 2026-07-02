//! file-rescue: ddrescue-style imaging and media-focused file carving.
//!
//! The library is split into three domains:
//! - [`map`]: the rescue map (GNU ddrescue mapfile compatible) tracking sector states
//! - [`image`]: the multi-pass imaging engine (copy / trim / scrape / retry)
//! - [`carve`]: signature-based file extraction with structure-aware carvers
//!
//! Platform notes: everything except [`device`]'s block-device support is
//! portable; `rescue image` is only wired up on Linux.

pub mod carve;
pub mod cli;
pub mod device;
pub mod image;
pub mod map;
pub mod testutil;
pub mod ui;
