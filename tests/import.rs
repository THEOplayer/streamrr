use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use insta::assert_snapshot;
use reqwest::header::HeaderMap;
use streamrr::import::{Har, import_har};
use streamrr::record::{RecordError, RecordOptions};
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
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(RecordError::NotFound(_))));
    assert!(!dest.exists());
}
