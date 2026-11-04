use std::fs;
use std::path::{Path, PathBuf};

use insta::assert_snapshot;
use reqwest::header::HeaderMap;
use streamrr::import::{Har, import_har};
use streamrr::record::{RecordError, RecordOptions};
use streamrr::shared::{MediaSelect, VariantSelectOptions};
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
