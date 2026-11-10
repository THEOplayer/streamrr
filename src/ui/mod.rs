mod log;
mod terminal;

pub use log::LogProgress;
pub use terminal::TerminalProgress;

use chrono::{DateTime, Utc};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use streamrr::record::{
    DownloadProgress, PlaylistInfo, PlaylistUpdate, RecordProgress, ResourceKind,
};
use url::Url;

/// Progress for the CLI: the terminal UI when stderr is a terminal,
/// otherwise the plain line logger.
pub enum CliProgress {
    Terminal(TerminalProgress),
    Log(LogProgress),
}

impl CliProgress {
    /// Create the appropriate progress reporter for this environment.
    pub fn create(verb: &'static str, summary_verb: &'static str, dest: PathBuf) -> Arc<Self> {
        if std::io::stderr().is_terminal() {
            Arc::new(CliProgress::Terminal(TerminalProgress::new(
                verb,
                summary_verb,
                dest.display().to_string(),
            )))
        } else {
            Arc::new(CliProgress::Log(LogProgress::new(verb, summary_verb, dest)))
        }
    }

    /// Print the final frame / summary line.
    pub fn finish(&self) {
        match self {
            CliProgress::Terminal(progress) => progress.finish(),
            CliProgress::Log(progress) => progress.finish(),
        }
    }
}

impl RecordProgress for CliProgress {
    fn started(&self, url: &Url) {
        match self {
            CliProgress::Terminal(p) => p.started(url),
            CliProgress::Log(p) => p.started(url),
        }
    }
    fn skipped(&self, url: &Url) {
        match self {
            CliProgress::Terminal(p) => p.skipped(url),
            CliProgress::Log(p) => p.skipped(url),
        }
    }
    fn playlists(&self, playlists: &[PlaylistInfo]) {
        match self {
            CliProgress::Terminal(p) => p.playlists(playlists),
            CliProgress::Log(p) => p.playlists(playlists),
        }
    }
    fn playlist_loaded(&self, playlist: usize, update: &PlaylistUpdate) {
        match self {
            CliProgress::Terminal(p) => p.playlist_loaded(playlist, update),
            CliProgress::Log(p) => p.playlist_loaded(playlist, update),
        }
    }
    fn playlist_waiting(&self, playlist: usize, next_refresh: DateTime<Utc>) {
        match self {
            CliProgress::Terminal(p) => p.playlist_waiting(playlist, next_refresh),
            CliProgress::Log(p) => p.playlist_waiting(playlist, next_refresh),
        }
    }
    fn playlist_ended(&self, playlist: usize) {
        match self {
            CliProgress::Terminal(p) => p.playlist_ended(playlist),
            CliProgress::Log(p) => p.playlist_ended(playlist),
        }
    }
    fn download_started(
        &self,
        playlist: usize,
        path: &str,
        kind: ResourceKind,
    ) -> Box<dyn DownloadProgress> {
        match self {
            CliProgress::Terminal(p) => p.download_started(playlist, path, kind),
            CliProgress::Log(p) => p.download_started(playlist, path, kind),
        }
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
