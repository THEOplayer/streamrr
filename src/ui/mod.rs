mod log;
mod terminal;

pub use log::LogProgress;
pub use terminal::TerminalProgress;

use relative_path::RelativePath;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use streamrr::record::RecordProgress;

/// Progress for the CLI: the terminal UI when stderr is a terminal,
/// otherwise the plain line logger.
pub trait CliProgress: RecordProgress {
    /// Print the final frame / summary line.
    fn finish(&self);
}

pub(crate) fn counted(count: usize, singular: &str) -> String {
    format!("{count} {singular}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::counted;

    #[test]
    fn counted_progress_labels() {
        for noun in ["segment", "gap", "new segment"] {
            assert_eq!(counted(0, noun), format!("0 {noun}s"));
            assert_eq!(counted(1, noun), format!("1 {noun}"));
            assert_eq!(counted(2, noun), format!("2 {noun}s"));
        }
    }
}

/// Create the appropriate progress reporter for this environment.
pub fn create(
    verb: &'static str,
    summary_verb: &'static str,
    dest: PathBuf,
) -> Arc<dyn CliProgress> {
    if std::io::stderr().is_terminal() {
        Arc::new(TerminalProgress::new(verb, dest.display().to_string()))
    } else {
        Arc::new(LogProgress::new(verb, summary_verb, dest))
    }
}

/// The display name for a playlist: its recording-relative directory,
/// or "media" for a single media playlist (path "").
pub(crate) fn display_name(path: &RelativePath) -> String {
    if path.as_str().is_empty() {
        "media".to_string()
    } else {
        path.to_string()
    }
}

/// Human-readable byte count (e.g. "1.5 MiB").
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
