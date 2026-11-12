use crate::ui::{CliProgress, counted, display_name, human_bytes};
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use streamrr::record::{
    DownloadProgress, PlaylistInfo, PlaylistUpdate, RecordProgress, ResourceKind,
};
use url::Url;

/// Plain line-logging progress reporter, used when stderr is not a
/// terminal. Prints one line per event to stdout.
pub struct LogProgress {
    verb: &'static str,
    summary_verb: &'static str,
    dest: PathBuf,
    started_at: Instant,
    names: Mutex<Vec<String>>,
    segments: AtomicUsize,
    bytes: Arc<AtomicU64>,
}

impl LogProgress {
    pub fn new(verb: &'static str, summary_verb: &'static str, dest: PathBuf) -> Self {
        Self {
            verb,
            summary_verb,
            dest,
            started_at: Instant::now(),
            names: Mutex::new(Vec::new()),
            segments: AtomicUsize::new(0),
            bytes: Arc::new(AtomicU64::new(0)),
        }
    }

    fn name(&self, playlist: usize) -> String {
        self.names
            .lock()
            .unwrap()
            .get(playlist)
            .cloned()
            .unwrap_or_else(|| format!("playlist{playlist}"))
    }
}

impl CliProgress for LogProgress {
    /// Print the final summary line.
    fn finish(&self) {
        let elapsed = self.started_at.elapsed().as_secs();
        println!(
            "{} {} ({}) in {:02}:{:02}",
            self.summary_verb,
            counted(self.segments.load(Ordering::Relaxed), "segment"),
            human_bytes(self.bytes.load(Ordering::Relaxed)),
            elapsed / 60,
            elapsed % 60,
        );
    }
}

impl RecordProgress for LogProgress {
    fn started(&self, url: &Url) {
        println!("{} {} → {}", self.verb, url, self.dest.display());
    }

    fn skipped(&self, url: &Url) {
        println!("Skipping {url}: not available");
    }

    fn playlists(&self, playlists: &[PlaylistInfo]) {
        let mut names = self.names.lock().unwrap();
        names.clear();
        names.extend(playlists.iter().map(|info| display_name(&info.path)));
    }

    fn playlist_loaded(&self, playlist: usize, update: &PlaylistUpdate) {
        self.segments
            .fetch_add(update.new_segments, Ordering::Relaxed);
        let mut line = format!(
            "{}: playlist loaded, {}",
            self.name(playlist),
            counted(update.new_segments, "new segment"),
        );
        if update.new_gaps > 0 {
            write!(line, " ({})", counted(update.new_gaps, "gap")).unwrap();
        }
        println!("{line}");
    }

    fn playlist_ended(&self, playlist: usize) {
        println!("{}: ended", self.name(playlist));
    }

    fn download_started(
        &self,
        playlist: usize,
        path: &str,
        _kind: ResourceKind,
    ) -> Box<dyn DownloadProgress> {
        Box::new(LogDownload {
            name: self.name(playlist),
            path: path.to_string(),
            size: AtomicU64::new(u64::MAX),
            got: AtomicU64::new(0),
            bytes: self.bytes.clone(),
        })
    }
}

struct LogDownload {
    name: String,
    path: String,
    size: AtomicU64,
    got: AtomicU64,
    bytes: std::sync::Arc<AtomicU64>,
}

impl DownloadProgress for LogDownload {
    fn set_size(&self, size: u64) {
        self.size.store(size, Ordering::Relaxed);
    }

    fn inc(&self, bytes: u64) {
        self.got.fetch_add(bytes, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    fn finish(&self) {
        let size = self.size.load(Ordering::Relaxed);
        let size = if size == u64::MAX {
            self.got.load(Ordering::Relaxed)
        } else {
            size
        };
        println!(
            "{}: downloaded {} ({})",
            self.name,
            self.path,
            human_bytes(size)
        );
    }
}
