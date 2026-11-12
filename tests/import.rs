use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use insta::assert_snapshot;
use reqwest::header::HeaderMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use streamrr::import::{Har, import_har};
use streamrr::record::{
    DownloadProgress, NoProgress, PlaylistInfo, PlaylistKind, PlaylistUpdate, RecordError,
    RecordOptions, RecordProgress, ResourceKind,
};
use streamrr::shared::{MediaSelect, VariantSelect, VariantSelectOptions};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use url::Url;

fn options() -> RecordOptions {
    RecordOptions {
        variant_select: VariantSelectOptions::default(),
        audio: MediaSelect::Default,
        video: MediaSelect::Default,
        subtitle: MediaSelect::Default,
        start: None,
        end: None,
        headers: HeaderMap::new(),
        keep_names: false,
    }
}

fn load_har(name: &str) -> Har {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/har")
        .join(name);
    let file = fs::File::open(path).unwrap();
    serde_json::from_reader(std::io::BufReader::new(file)).unwrap()
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// A deterministic dump of all files in a directory: sorted relative
/// paths followed by their (lossy UTF-8) contents.
fn dump_dir(dir: &Path) -> String {
    let mut files = Vec::new();
    collect_files(dir, &mut files);
    files.sort();
    let mut output = String::new();
    for file in files {
        let relative = file.strip_prefix(dir).unwrap();
        // Normalize path separators in the listing so snapshots match
        // on Windows and Unix.
        output.push_str(&relative.to_string_lossy().replace('\\', "/"));
        output.push('\n');
        let bytes = fs::read(&file).unwrap();
        output.push_str(&String::from_utf8_lossy(&bytes));
        if !bytes.ends_with(b"\n") {
            output.push('\n');
        }
        output.push('\n');
    }
    output
}

async fn import(har: &str, url: Option<Url>) -> (TempDir, Result<(), RecordError>) {
    let dir = TempDir::new().unwrap();
    let result = import_har(
        load_har(har),
        url,
        dir.path(),
        options(),
        Arc::new(NoProgress),
        CancellationToken::new(),
    )
    .await;
    (dir, result)
}

#[tokio::test]
async fn import_vod() {
    let (dir, result) = import("vod.har", None).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_live() {
    let (dir, result) = import("live.har", None).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_byte_ranges() {
    let (dir, result) = import("byteranges.har", None).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_byte_ranges_from_full_response() {
    let (dir, result) = import("byteranges_full.har", None).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_first_playlist() {
    let (dir, result) = import("two_streams.har", None).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_url() {
    let url = Url::parse("https://b.example.com/master.m3u8").unwrap();
    let (dir, result) = import("two_streams.har", Some(url)).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_url_redirect() {
    let url = Url::parse("https://d.example.com/master.m3u8").unwrap();
    let (dir, result) = import("two_streams.har", Some(url)).await;
    result.unwrap();
    assert_snapshot!(dump_dir(dir.path()));
}

#[tokio::test]
async fn import_url_not_found() {
    let url = Url::parse("https://c.example.com/master.m3u8").unwrap();
    let (_dir, result) = import("two_streams.har", Some(url.clone())).await;
    assert!(matches!(result, Err(RecordError::NotFound(u)) if u == url));
}

fn all_options() -> RecordOptions {
    RecordOptions {
        variant_select: VariantSelectOptions::Named(VariantSelect::All),
        audio: MediaSelect::All,
        video: MediaSelect::All,
        subtitle: MediaSelect::All,
        start: None,
        end: None,
        headers: HeaderMap::new(),
        keep_names: false,
    }
}

/// Recording into `MemoryStorage` produces the same playlists and
/// resources as recording into `FsStorage`, without downloading
/// resources (they are decoded on demand from the source).
#[tokio::test]
async fn import_to_memory_matches_fs() {
    use streamrr::record::{FsStorage, MemoryStorage, Source};
    for fixture in [
        "vod.har",
        "live.har",
        "byteranges.har",
        "byteranges_full.har",
        "two_streams.har",
    ] {
        let dir = TempDir::new().unwrap();
        let fs_storage = FsStorage::new(dir.path());
        streamrr::import::import_har_into(
            load_har(fixture),
            None,
            fs_storage,
            all_options(),
            Arc::new(NoProgress),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let memory = MemoryStorage::default();
        let source = streamrr::import::import_har_into(
            load_har(fixture),
            None,
            memory.clone(),
            all_options(),
            Arc::new(NoProgress),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let memory = memory.take();

        // Same playlists and resources, byte-for-byte.
        let mut files = Vec::new();
        collect_files(dir.path(), &mut files);
        files.sort();
        let mut playlist_paths = HashSet::new();
        let mut resource_paths = HashSet::new();
        let mut recording_json = None;
        for file in files {
            let relative = file
                .strip_prefix(dir.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&file).unwrap();
            if relative == "recording.json" {
                recording_json = Some(bytes);
            } else if relative.ends_with(".m3u8") {
                assert_eq!(
                    memory.playlists[&relative].as_ref(),
                    bytes.as_slice(),
                    "{fixture}: {relative}"
                );
                playlist_paths.insert(relative);
            } else {
                let resource = &memory.resources[&relative];
                let fetched = source
                    .request(&resource.url, resource.byte_range)
                    .await
                    .unwrap();
                assert_eq!(
                    fetched.value.as_ref(),
                    bytes.as_slice(),
                    "{fixture}: {relative}"
                );
                resource_paths.insert(relative);
            }
        }
        // Same entries (no extras on either side).
        assert_eq!(
            playlist_paths,
            memory.playlists.keys().cloned().collect::<HashSet<_>>(),
            "{fixture}: playlists"
        );
        assert_eq!(
            resource_paths,
            memory.resources.keys().cloned().collect::<HashSet<_>>(),
            "{fixture}: resources"
        );
        assert_eq!(
            serde_json::to_string_pretty(&memory.recording)
                .unwrap()
                .as_bytes(),
            recording_json.unwrap().as_slice(),
            "{fixture}: recording.json"
        );
    }
}

/// A failed import must not touch an existing recording directory.
#[tokio::test]
async fn failed_import_keeps_existing_recording() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("recording.json"), r#"{"existing":true}"#).unwrap();
    let url = Url::parse("https://c.example.com/master.m3u8").unwrap();
    let result = import_har(
        load_har("two_streams.har"),
        Some(url),
        dir.path(),
        options(),
        Arc::new(NoProgress),
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(RecordError::NotFound(_))));
    assert_eq!(
        fs::read_to_string(dir.path().join("recording.json")).unwrap(),
        r#"{"existing":true}"#
    );
}

/// A failed import must not create the destination directory.
#[tokio::test]
async fn failed_import_creates_nothing() {
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("recording");
    let url = Url::parse("https://c.example.com/master.m3u8").unwrap();
    let result = import_har(
        load_har("two_streams.har"),
        Some(url),
        &dest,
        options(),
        Arc::new(NoProgress),
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(RecordError::NotFound(_))));
    assert!(!dest.exists());
}

/// Records all progress events for assertions.
#[derive(Default)]
struct MockProgress {
    started: std::sync::Mutex<Vec<String>>,
    skipped: std::sync::Mutex<Vec<String>>,
    playlists: std::sync::Mutex<Vec<(String, String)>>,
    loaded: std::sync::Mutex<Vec<(usize, usize, usize)>>,
    ended: std::sync::Mutex<Vec<usize>>,
    downloads: std::sync::Mutex<Vec<Arc<DownloadRecord>>>,
}

struct DownloadRecord {
    path: String,
    kind: ResourceKind,
    size: std::sync::atomic::AtomicU64,
    got: std::sync::atomic::AtomicU64,
    finished: std::sync::atomic::AtomicBool,
}

impl RecordProgress for MockProgress {
    fn started(&self, url: &Url) {
        self.started.lock().unwrap().push(url.to_string());
    }
    fn skipped(&self, url: &Url) {
        self.skipped.lock().unwrap().push(url.to_string());
    }
    fn playlists(&self, playlists: &[PlaylistInfo]) {
        let mut out = self.playlists.lock().unwrap();
        for info in playlists {
            let kind = match &info.kind {
                PlaylistKind::Variant {
                    bandwidth,
                    resolution,
                } => format!("variant {bandwidth} {resolution:?}"),
                PlaylistKind::Rendition { name, .. } => {
                    format!("rendition {name}")
                }
                PlaylistKind::Media => "media".to_string(),
            };
            out.push((info.path.clone(), kind));
        }
    }
    fn playlist_loaded(&self, playlist: usize, update: &PlaylistUpdate) {
        self.loaded
            .lock()
            .unwrap()
            .push((playlist, update.new_segments, update.new_gaps));
    }
    fn playlist_ended(&self, playlist: usize) {
        self.ended.lock().unwrap().push(playlist);
    }
    fn download_started(
        &self,
        _playlist: usize,
        path: &str,
        kind: ResourceKind,
    ) -> Box<dyn DownloadProgress> {
        let record = Arc::new(DownloadRecord {
            path: path.to_string(),
            kind,
            size: 0.into(),
            got: 0.into(),
            finished: false.into(),
        });
        self.downloads.lock().unwrap().push(record.clone());
        Box::new(MockDownload { record })
    }
}

struct MockDownload {
    record: Arc<DownloadRecord>,
}

impl DownloadProgress for MockDownload {
    fn set_size(&self, size: u64) {
        self.record.size.store(size, Ordering::Relaxed);
    }
    fn inc(&self, bytes: u64) {
        self.record.got.fetch_add(bytes, Ordering::Relaxed);
    }
    fn finish(&self) {
        self.record.finished.store(true, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn import_reports_progress() {
    let progress = Arc::new(MockProgress::default());
    let dir = TempDir::new().unwrap();
    import_har(
        load_har("vod.har"),
        None,
        dir.path(),
        all_options(),
        progress.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    // The master playlist's URL is reported.
    assert_eq!(
        *progress.started.lock().unwrap(),
        vec!["https://example.com/master.m3u8".to_string()]
    );
    // The unavailable variant is reported as skipped.
    assert_eq!(
        *progress.skipped.lock().unwrap(),
        vec!["https://example.com/low/index.m3u8".to_string()]
    );
    // The available variant is the only playlist, with its bandwidth.
    assert_eq!(
        *progress.playlists.lock().unwrap(),
        vec![(
            "variant1".to_string(),
            "variant 200000 Some((1280, 720))".to_string()
        )]
    );
    // All 3 segments are reported once, no gaps.
    let loaded = progress.loaded.lock().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, 0);
    assert_eq!(loaded[0].1, 3);
    assert_eq!(loaded[0].2, 0);
    drop(loaded);
    // The playlist ended (VOD has ENDLIST).
    assert_eq!(*progress.ended.lock().unwrap(), vec![0]);
    // Every download finished, and all reported bytes were written.
    let total_bytes: u64 = progress
        .downloads
        .lock()
        .unwrap()
        .iter()
        .map(|d| {
            assert!(d.finished.load(Ordering::Relaxed));
            d.got.load(Ordering::Relaxed)
        })
        .sum();
    let written_bytes: u64 = [
        "variant1/segment-0.ts",
        "variant1/segment-1.ts",
        "variant1/segment-2.ts",
    ]
    .iter()
    .map(|f| fs::metadata(dir.path().join(f)).unwrap().len())
    .sum();
    assert_eq!(total_bytes, written_bytes);
}

#[tokio::test]
async fn import_live_reports_progress() {
    let progress = Arc::new(MockProgress::default());
    let dir = TempDir::new().unwrap();
    import_har(
        load_har("live.har"),
        None,
        dir.path(),
        all_options(),
        progress.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    // A media-only import reports one media playlist.
    assert_eq!(
        *progress.playlists.lock().unwrap(),
        vec![("".to_string(), "media".to_string())]
    );
    // 3 playlist snapshots were loaded for playlist 0.
    let loaded = progress.loaded.lock().unwrap();
    assert_eq!(loaded.len(), 3);
    assert!(loaded.iter().all(|(id, _, _)| *id == 0));
    // New segments and gaps add up to the distinct segments seen.
    assert_eq!(loaded.iter().map(|l| l.1).sum::<usize>(), 2);
    assert_eq!(loaded.iter().map(|l| l.2).sum::<usize>(), 2);
    let new_segments: usize = loaded.iter().map(|l| l.1).sum();
    drop(loaded);
    assert_eq!(*progress.ended.lock().unwrap(), vec![0]);
    // Each resource path is downloaded at most once, and every
    // segment download finished.
    let downloads = progress.downloads.lock().unwrap();
    let paths: HashSet<_> = downloads.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths.len(), downloads.len());
    let finished_segments = downloads
        .iter()
        .filter(|d| matches!(d.kind, ResourceKind::Segment))
        .inspect(|d| {
            assert!(d.finished.load(Ordering::Relaxed));
        })
        .count();
    assert_eq!(finished_segments, new_segments);
    assert!(downloads.iter().all(|d| d.finished.load(Ordering::Relaxed)));
}
