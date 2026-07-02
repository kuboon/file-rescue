use anyhow::{bail, Context, Result};
use clap::Parser;
use file_rescue::carve;
use file_rescue::cli::{CarveArgs, Cli, Cmd, MapCmd};
use file_rescue::map::{format as mapformat, RescueMap, SectorStatus};
use file_rescue::ui::progress::byte_bar;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rescue: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        #[cfg(target_os = "linux")]
        Cmd::Image(args) => image::run(args, cli.quiet),
        Cmd::Carve(args) => run_carve(args, cli.quiet),
        Cmd::Map(cmd) => run_map(cmd),
    }
}

fn load_map(path: &Path) -> Result<RescueMap> {
    mapformat::load(path)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("loading mapfile {}", path.display()))
}

fn run_carve(args: CarveArgs, quiet: bool) -> Result<ExitCode> {
    let map = args.map.as_deref().map(load_map).transpose()?;
    let mut reader = carve::FileReader::open(&args.input)
        .with_context(|| format!("opening {}", args.input.display()))?;
    let opts = carve::CarveOptions {
        align: args.align,
        formats: args
            .formats
            .map(|f| f.split(',').map(|s| s.trim().to_string()).collect()),
    };
    let bar = byte_bar(carve::carver::ReadAt::total_len(&reader), quiet);
    let mut on_progress = |pos: u64, found: usize| {
        if let Some(b) = &bar {
            b.set_position(pos);
            b.set_message(format!("{found} files"));
        }
    };
    let files = carve::carve_scan(
        &mut reader,
        &args.outdir,
        map.as_ref(),
        &opts,
        &mut on_progress,
    )?;
    if let Some(b) = &bar {
        b.finish_and_clear();
    }
    carve::write_reports(&files, &args.outdir)?;
    let damaged = files.iter().filter(|f| f.damaged).count();
    println!(
        "extracted {} files to {} ({} flagged damaged)",
        files.len(),
        args.outdir.display(),
        damaged
    );
    let mut by_format: std::collections::BTreeMap<&str, usize> = Default::default();
    for f in &files {
        *by_format.entry(f.format).or_default() += 1;
    }
    for (fmt, n) in by_format {
        println!("  {fmt}: {n}");
    }
    println!("report: {}", args.outdir.join("report.txt").display());
    Ok(ExitCode::SUCCESS)
}

fn run_map(cmd: MapCmd) -> Result<ExitCode> {
    match cmd {
        MapCmd::Show { mapfile } => {
            let map = load_map(&mapfile)?;
            println!("size:      {} bytes", map.size);
            println!("phase:     {} (pass {})", map.current_phase, map.pass);
            println!("position:  0x{:X}", map.current_pos);
            for status in SectorStatus::ALL {
                let bytes = map.bytes_with(status);
                let areas = map.count_with(status);
                println!(
                    "{:<12} {:>15} bytes in {:>6} areas",
                    status.label(),
                    bytes,
                    areas
                );
            }
            let rescued = map.bytes_with(SectorStatus::Rescued);
            if map.size > 0 {
                println!(
                    "rescued:   {:.4}%",
                    rescued as f64 / map.size as f64 * 100.0
                );
            }
        }
        MapCmd::Regions { mapfile, status } => {
            let map = load_map(&mapfile)?;
            let filter = status.map(|s| parse_status(&s)).transpose()?;
            for e in map.extents() {
                if filter.is_none_or(|f| f == e.status) {
                    println!(
                        "0x{:010X} 0x{:010X} {} {}",
                        e.start,
                        e.len,
                        e.status.as_char(),
                        e.status.label()
                    );
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_status(s: &str) -> Result<SectorStatus> {
    let by_char = s
        .chars()
        .next()
        .filter(|_| s.len() == 1)
        .and_then(SectorStatus::from_char);
    let found = by_char.or_else(|| {
        SectorStatus::ALL
            .into_iter()
            .find(|st| st.label() == s || st.label().replace('-', "") == s.replace('-', ""))
    });
    match found {
        Some(st) => Ok(st),
        None => bail!("unknown status {s:?} (use ?, *, /, -, + or the names non-tried, non-trimmed, non-scraped, bad-sector, rescued)"),
    }
}

#[cfg(target_os = "linux")]
mod image {
    use super::*;
    use file_rescue::cli::ImageArgs;
    use file_rescue::device;
    use file_rescue::image::sink::FileSink;
    use file_rescue::image::source::{FileSource, SectorSource};
    use file_rescue::image::{ImagingEngine, ImagingOptions, Outcome};
    use file_rescue::map::format::save_atomic;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

    pub fn run(args: ImageArgs, quiet: bool) -> Result<ExitCode> {
        device::check_output_safety(&args.source, &args.image)?;
        device::check_output_safety(&args.source, &args.mapfile)?;
        device::check_clobber(&args.image, &args.mapfile, args.force)?;

        let opened = device::open_source(&args.source, args.direct, args.sector_size)?;
        let size = opened.size;
        let sector_size = opened.sector_size;
        let source = FileSource::from_opened(opened);
        let source = wrap_faults(source, &args)?;

        let mut map = if args.mapfile.exists() {
            let m = load_map(&args.mapfile)?;
            if m.size != size {
                bail!(
                    "mapfile {} describes a {}-byte source but {} is {} bytes",
                    args.mapfile.display(),
                    m.size,
                    args.source.display(),
                    size
                );
            }
            eprintln!(
                "resuming: {} of {} bytes already rescued",
                m.bytes_with(SectorStatus::Rescued),
                m.size
            );
            m
        } else {
            RescueMap::new_untried(size)
        };

        let sink = FileSink::open(&args.image)
            .with_context(|| format!("opening image {}", args.image.display()))?;
        let opts = ImagingOptions {
            cluster_size: args.cluster_size.max(sector_size as u64),
            reverse: args.reverse,
            retry_passes: args.retry_passes,
            ..Default::default()
        };

        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = stop.clone();
            ctrlc::set_handler(move || {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            })
            .context("installing Ctrl-C handler")?;
        }

        let command_line: String = std::env::args().collect::<Vec<_>>().join(" ");
        let bar = byte_bar(size, quiet);
        let mut last_save = Instant::now();
        let mut last_draw = Instant::now() - Duration::from_secs(1);
        let mut engine = ImagingEngine::new(source, sink, opts);
        let result = engine.run(&mut map, &stop, &mut |m| {
            if last_draw.elapsed() >= Duration::from_millis(100) {
                last_draw = Instant::now();
                if let Some(b) = &bar {
                    b.set_position(m.bytes_with(SectorStatus::Rescued));
                    b.set_message(format!(
                        "[{}] bad {} B in {} areas",
                        m.current_phase,
                        m.bytes_with(SectorStatus::Bad),
                        m.count_with(SectorStatus::Bad),
                    ));
                }
            }
            if last_save.elapsed() >= AUTOSAVE_INTERVAL {
                last_save = Instant::now();
                let _ = save_atomic(m, &args.mapfile, &command_line);
            }
        });
        if let Some(b) = &bar {
            b.finish_and_clear();
        }
        // Always persist the map, even if the engine failed.
        save_atomic(&map, &args.mapfile, &command_line)
            .with_context(|| format!("saving mapfile {}", args.mapfile.display()))?;
        let summary = result?;

        println!(
            "{}: rescued {} bytes, {} bad bytes in {} areas ({} read errors)",
            match summary.outcome {
                Outcome::Finished => "finished",
                Outcome::Interrupted => "interrupted (resume with the same command)",
            },
            summary.rescued,
            summary.bad_bytes,
            summary.bad_areas,
            summary.read_errors,
        );
        Ok(match summary.outcome {
            Outcome::Finished => ExitCode::SUCCESS,
            Outcome::Interrupted => ExitCode::FAILURE,
        })
    }

    #[cfg(feature = "fault-injection")]
    fn wrap_faults(source: FileSource, args: &ImageArgs) -> Result<Box<dyn SectorSource>> {
        use file_rescue::cli::parse_bad_spec;
        use file_rescue::image::source::FaultySource;
        Ok(match &args.simulate_bad {
            Some(spec) => {
                let regions = parse_bad_spec(spec).map_err(|e| anyhow::anyhow!(e))?;
                Box::new(FaultySource::new(source, regions))
            }
            None => Box::new(source),
        })
    }

    #[cfg(not(feature = "fault-injection"))]
    fn wrap_faults(source: FileSource, _args: &ImageArgs) -> Result<Box<dyn SectorSource>> {
        Ok(Box::new(source))
    }
}
