use chrono::{DateTime, Utc};
use std::sync::Arc;
use url::Url;

/// Reports the progress of a recording or import.
///
/// The library only emits events; how they are shown (a terminal
/// UI, plain log output, or nothing) is up to the caller.
/// All methods have a default no-op implementation.
pub trait RecordProgress: Send + Sync {
    /// The URL that is actually being recorded or imported.
    fn started(&self, url: &Url) {
        let _ = url;
    }

    /// A variant or rendition is not available and is skipped.
    fn skipped(&self, url: &Url) {
        let _ = url;
    }

    /// The set of recorded playlists, in display order.
    /// The index into `playlists` identifies a playlist in later calls.
    fn playlists(&self, playlists: &[PlaylistInfo]) {
        let _ = playlists;
    }

    /// A playlist was loaded and written.
    fn playlist_loaded(&self, playlist: usize, update: &PlaylistUpdate) {
        let _ = (playlist, update);
    }

    /// A live playlist is now waiting for its next refresh.
    fn playlist_waiting(&self, playlist: usize, next_refresh: DateTime<Utc>) {
        let _ = (playlist, next_refresh);
    }

    /// A playlist ended, or the source has no more versions of it.
    fn playlist_ended(&self, playlist: usize) {
        let _ = playlist;
    }

    /// A download of a resource started. `path` is the
    /// recording-relative path the resource is stored under.
    fn download_started(
        &self,
        playlist: usize,
        path: &str,
        kind: ResourceKind,
    ) -> Box<dyn DownloadProgress> {
        let _ = (playlist, path, kind);
        Box::new(NoProgress)
    }
}

/// Reports the progress of a single download.
///
/// Dropping without calling `finish` means the download was aborted.
pub trait DownloadProgress: Send + Sync {
    /// The total size of the download, if known.
    fn set_size(&self, size: u64) {
        let _ = size;
    }

    /// `bytes` more bytes were downloaded.
    fn inc(&self, bytes: u64) {
        let _ = bytes;
    }

    /// The download finished successfully, including the
    /// "already stored" case.
    fn finish(&self) {}
}

/// Progress callbacks that do nothing.
pub struct NoProgress;

impl RecordProgress for NoProgress {}
impl DownloadProgress for NoProgress {}

/// A recorded playlist's display information.
pub struct PlaylistInfo {
    /// The recording-relative directory ("" for a single media playlist).
    pub path: String,
    pub kind: PlaylistKind,
}

/// What kind of playlist is being recorded.
pub enum PlaylistKind {
    /// A variant stream.
    Variant {
        bandwidth: u64,
        resolution: Option<(u64, u64)>,
    },
    /// A rendition (alternate media) of a variant.
    Rendition {
        media_type: MediaType,
        name: String,
        language: Option<String>,
    },
    /// A media playlist without a master playlist.
    Media,
}

/// The type of an alternate rendition.
pub enum MediaType {
    Audio,
    Video,
    Subtitles,
    ClosedCaptions,
    Other,
}

/// An update about a newly loaded playlist.
pub struct PlaylistUpdate {
    /// The number of segments in the playlist.
    pub segments: usize,
    /// Segments that were not seen in a previous load.
    pub new_segments: usize,
    /// Segments newly marked with `#EXT-X-GAP`.
    pub new_gaps: usize,
    /// Whether the playlist is live (no `#EXT-X-ENDLIST`).
    pub live: bool,
}

/// The kind of a downloaded resource.
pub enum ResourceKind {
    Segment,
    InitSegment,
    Key,
}

/// A shared progress reporter.
pub type Progress = Arc<dyn RecordProgress>;
