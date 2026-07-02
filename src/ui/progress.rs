//! indicatif adapter. The engines know nothing about display; this
//! consumes their progress callbacks.

use indicatif::{ProgressBar, ProgressStyle};

pub fn byte_bar(total: u64, quiet: bool) -> Option<ProgressBar> {
    if quiet || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return None;
    }
    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::with_template(
            "{bar:32} {bytes}/{total_bytes} ({binary_bytes_per_sec}, ETA {eta}) {msg}",
        )
        .expect("static template"),
    );
    Some(bar)
}
