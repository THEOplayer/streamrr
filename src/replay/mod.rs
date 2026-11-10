use anyhow::anyhow;
use std::convert::Infallible;
use std::fmt::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use m3u8_rs::Playlist;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use warp::http::{Response, StatusCode, Uri, header::CONTENT_TYPE};
use warp::path::{FullPath, Peek, Tail};
use warp::reject::{Reject, custom};
use warp::{Filter, Rejection, Reply, reject, reply};

use crate::import::{Har, import_har_into};
use crate::record::{
    HarSource, MemoryRecording, MemoryStorage, RecordError, RecordOptions, Source,
    strip_media_playlist,
};
use crate::shared::{MediaSelect, Recording, StripBom, VariantSelect, VariantSelectOptions};
use reqwest::header::HeaderMap;

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
struct PlaylistQueryParams {
    start: Option<i64>,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ReplayError {
    #[error("Missing recording file")]
    MissingRecording,
    #[error("Invalid recording: {0}")]
    InvalidRecording(#[from] serde_json::Error),
    #[error("Missing start time in recording")]
    MissingStartTime,
    #[error("Failed to bind to address: {0}")]
    Bind(io::Error),
    #[error("Missing playlist file: {0}")]
    MissingPlaylist(String),
    #[error("Invalid playlist: {0}")]
    InvalidPlaylist(#[from] anyhow::Error),
    #[error("import error: {0}")]
    Import(RecordError),
    #[error("Invalid HAR file: {0}")]
    InvalidHar(anyhow::Error),
    #[error("Cancelled")]
    Cancelled,
}

/// Where replayed files are read from.
enum Store {
    /// A recording directory on disk.
    Directory { dir: PathBuf, recording: Recording },
    /// A recording held in memory, with resources decoded
    /// from the source only when requested.
    Memory {
        recording: MemoryRecording,
        source: HarSource,
    },
}

impl Store {
    fn recording(&self) -> &Recording {
        match self {
            Store::Directory { recording, .. } => recording,
            Store::Memory { recording, .. } => &recording.recording,
        }
    }

    /// Read a playlist at the given recording-relative path.
    async fn read_playlist(&self, path: &str) -> Result<Vec<u8>, ReplayError> {
        match self {
            Store::Directory { dir, .. } => fs::read(dir.join(path))
                .await
                .map_err(|_| ReplayError::MissingPlaylist(path.to_string())),
            Store::Memory { recording, .. } => recording
                .playlists
                .get(path)
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| ReplayError::MissingPlaylist(path.to_string())),
        }
    }
}

pub async fn replay(
    recording_path: &Path,
    address: IpAddr,
    port: u16,
    token: CancellationToken,
) -> Result<(), ReplayError> {
    let recording_path = recording_path.to_owned();
    let raw_recording = fs::read_to_string(recording_path.join("recording.json"))
        .await
        .map_err(|_| ReplayError::MissingRecording)?;
    let recording = serde_json::from_str::<Recording>(&raw_recording)?;
    let store = Arc::new(Store::Directory {
        dir: recording_path,
        recording,
    });
    serve(store, address, port, token).await
}

/// Replay a HAR file directly from memory, without importing it to disk.
/// All captured variants and renditions are served, and segments are
/// decoded from the HAR only when requested.
pub async fn replay_har(
    har: Har,
    address: IpAddr,
    port: u16,
    token: CancellationToken,
) -> Result<(), ReplayError> {
    let storage = MemoryStorage::default();
    let options = RecordOptions {
        variant_select: VariantSelectOptions::Named(VariantSelect::All),
        audio: MediaSelect::All,
        video: MediaSelect::All,
        subtitle: MediaSelect::All,
        start: None,
        end: None,
        headers: HeaderMap::new(),
        keep_names: false,
    };
    let source = import_har_into(har, None, storage.clone(), options, token.clone())
        .await
        .map_err(|e| match e {
            RecordError::Cancelled => ReplayError::Cancelled,
            e => ReplayError::Import(e),
        })?;
    let recording = storage.take();
    let store = Arc::new(Store::Memory { recording, source });
    serve(store, address, port, token).await
}

async fn serve(
    store: Arc<Store>,
    address: IpAddr,
    port: u16,
    token: CancellationToken,
) -> Result<(), ReplayError> {
    let recording_start = *store
        .recording()
        .earliest_time()
        .ok_or(ReplayError::MissingStartTime)?;
    let service = routes(store, recording_start);

    let address = SocketAddr::from((address, port));
    let listener = TcpListener::bind(address)
        .await
        .map_err(ReplayError::Bind)?;
    let server = warp::serve(service).incoming(listener);
    println!("Replay server listening on http://{address}/");

    token
        .run_until_cancelled(server.run())
        .await
        .ok_or(ReplayError::Cancelled)?;

    Ok(())
}

fn routes(
    store: Arc<Store>,
    recording_start: DateTime<Utc>,
) -> warp::filters::BoxedFilter<(reply::Response,)> {
    let playlist_store = store.clone();
    let with_start = warp::path::tail()
        .and(warp::query::<PlaylistQueryParams>())
        .and_then(move |tail: Tail, params: PlaylistQueryParams| {
            let file_name = tail.as_str().to_string();
            let store = playlist_store.clone();
            async move {
                let start = params.start.ok_or_else(reject)?;
                let relative_path =
                    playlist_path_at_time(&file_name, store.recording(), recording_start, start)
                        .ok_or_else(|| custom(ServerError::PlaylistNotFound(file_name)))?;
                let raw_playlist = store
                    .read_playlist(&relative_path)
                    .await
                    .map_err(|e| custom(ServerError::PlaylistFileError(e)))?;
                let reply = m3u8_reply(raw_playlist, start)
                    .map_err(|e| custom(ServerError::PlaylistFileError(e)))?
                    .into_response();
                Ok::<reply::Response, Rejection>(reply)
            }
        });
    let without_start =
        warp::path::full()
            .and(no_playlist_params())
            .map(move |full_path: FullPath| {
                let now = Utc::now();
                let client_start = now.timestamp_millis();
                let redirect_uri = format!("{}?start={}", full_path.as_str(), client_start);
                warp::redirect::temporary(Uri::from_str(&redirect_uri).unwrap())
            });
    let playlist = path_extension(".m3u8")
        .and(with_start.or(without_start))
        .map(Reply::into_response);

    let files: warp::filters::BoxedFilter<(reply::Response,)> = match &*store {
        Store::Directory { dir, .. } => {
            warp::fs::dir(dir.clone()).map(Reply::into_response).boxed()
        }
        Store::Memory { .. } => {
            let store = store.clone();
            warp::get()
                .and(warp::path::tail())
                .and_then(move |tail: Tail| {
                    let store = store.clone();
                    async move {
                        let path = tail.as_str();
                        let Store::Memory { recording, source } = &*store else {
                            return Err(reject());
                        };
                        let Some(resource) = recording.resources.get(path) else {
                            return Err(reject());
                        };
                        let bytes = source
                            .request(&resource.url, resource.byte_range)
                            .await
                            .map_err(|e| custom(ServerError::RequestFailed(e.into())))?;
                        let content_type = mime_guess::from_path(path)
                            .first_or_octet_stream()
                            .to_string();
                        // Pass the decoded `Bytes` through without copying.
                        Response::builder()
                            .header(CONTENT_TYPE, content_type)
                            .body(bytes.value)
                            .map_err(|e| custom(ServerError::RequestFailed(e.into())))
                            .map(Reply::into_response)
                    }
                })
                .boxed()
        }
    };

    let cors = warp::cors().allow_any_origin().build();
    playlist
        .or(files)
        .with(cors)
        .recover(handle_rejection)
        .map(Reply::into_response)
        .boxed()
}

fn playlist_path_at_time(
    playlist_name: &str,
    recording: &Recording,
    recording_start: DateTime<Utc>,
    client_start: i64,
) -> Option<String> {
    // At T = client_start + X, serve the playlist at recording_start + X
    let client_start = Utc.timestamp_millis_opt(client_start).single()?;
    let offset = Utc::now() - client_start;
    let recording_time = recording_start + offset;
    let (_, relative_path) = recording
        .find_latest_before(playlist_name, recording_time)
        .or_else(|| recording.earliest_time_for(playlist_name))?;
    Some(relative_path.to_string())
}

fn path_extension(ext: &'static str) -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::path::peek()
        .and_then(move |peek: Peek| async move {
            if peek.as_str().ends_with(ext) {
                Ok(())
            } else {
                Err(reject())
            }
        })
        .untuple_one()
}

fn no_playlist_params() -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::query::<PlaylistQueryParams>()
        .and_then(|params: PlaylistQueryParams| async move {
            if params.start.is_none() {
                Ok(())
            } else {
                Err(reject())
            }
        })
        .untuple_one()
}

fn m3u8_reply(mut raw_playlist: Vec<u8>, start: i64) -> Result<impl Reply + use<>, ReplayError> {
    // Parse the playlist
    (&mut raw_playlist).strip_bom();
    let mut playlist = m3u8_rs::parse_playlist_res(&raw_playlist).map_err(|e| {
        ReplayError::InvalidPlaylist(anyhow!(
            e.map_input(|i| String::from_utf8_lossy(i).to_string())
        ))
    })?;
    // Rewrite the playlist
    match &mut playlist {
        Playlist::MasterPlaylist(playlist) => {
            // Rewrite the variant and media URLs
            for variant in playlist.variants.iter_mut() {
                write!(&mut variant.uri, "?start={start}").unwrap();
            }
            for media in playlist.alternatives.iter_mut() {
                if let Some(uri) = media.uri.as_mut() {
                    write!(uri, "?start={start}").unwrap();
                }
            }
        }
        Playlist::MediaPlaylist(playlist) => {
            // Strip tags with original playlist information
            strip_media_playlist(playlist);
        }
    }
    raw_playlist.clear();
    playlist.write_to(&mut raw_playlist).unwrap();
    // Create a response
    let response = Response::builder()
        .header(CONTENT_TYPE, "application/x-mpegurl")
        .body(raw_playlist)
        .unwrap();
    Ok(response)
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
enum ServerError {
    #[error("No playlist found for {0}")]
    PlaylistNotFound(String),
    #[error("Failed to load playlist: {0}")]
    PlaylistFileError(ReplayError),
    #[error("Failed to load resource: {0}")]
    RequestFailed(Box<dyn std::error::Error + Send + Sync>),
}

impl Reject for ServerError {}

async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let code: StatusCode;
    let message: String;

    if err.is_not_found() {
        code = StatusCode::NOT_FOUND;
        message = "Not found".to_owned();
    } else if let Some(e) = err.find::<ServerError>() {
        message = e.to_string();
        code = StatusCode::INTERNAL_SERVER_ERROR;
    } else {
        eprintln!("Unhandled rejection: {:?}", err);
        code = StatusCode::INTERNAL_SERVER_ERROR;
        message = "Internal server error".to_owned();
    }

    let html = warp::reply::html(message);
    Ok(warp::reply::with_status(html, code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::import_har_into;
    use crate::record::{FsStorage, MemoryStorage};

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

    fn load_har_fixture(name: &str) -> Har {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/har")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    async fn memory_routes(har: Har) -> warp::filters::BoxedFilter<(reply::Response,)> {
        let storage = MemoryStorage::default();
        let source = import_har_into(
            har,
            None,
            storage.clone(),
            all_options(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let recording = storage.take();
        let recording_start = *recording.recording.earliest_time().unwrap();
        routes(
            Arc::new(Store::Memory { recording, source }),
            recording_start,
        )
    }

    #[tokio::test]
    async fn replay_memory_store() {
        let routes = memory_routes(load_har_fixture("vod.har")).await;
        // A playlist request without `start` redirects to a start time.
        let response = warp::test::request()
            .path("/index.m3u8")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert!(
            response.headers()["location"]
                .to_str()
                .unwrap()
                .contains("?start=")
        );
        // The master playlist is served with `?start=` appended to URIs.
        let response = warp::test::request()
            .path("/index.m3u8?start=1")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(response.body().to_vec()).unwrap();
        assert!(body.contains("?start=1"));
        // The variant playlist is served with original-info tags stripped.
        let response = warp::test::request()
            .path("/variant1/index.m3u8?start=1")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(response.body().to_vec()).unwrap();
        assert!(body.contains("#EXT-X-TARGETDURATION:6"));
        assert!(!body.contains("ORIGINAL"));
        // A segment is decoded from the HAR on demand.
        let response = warp::test::request()
            .path("/variant1/segment-0.ts")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "video/vnd.dlna.mpeg-tts"
        );
        assert_eq!(response.body().as_ref(), b"seg-1");
        // An unknown path is not found.
        let response = warp::test::request()
            .path("/variant1/nope.ts")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn replay_memory_store_byte_ranges() {
        let routes = memory_routes(load_har_fixture("byteranges.har")).await;
        // The init segment is sliced from a captured byte-range response.
        let response = warp::test::request()
            .path("/init-a93d0244c63029398205960df07d3faa1e3f2ee0.mp4")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "video/mp4");
        assert_eq!(response.body().as_ref(), b"0123456789");
        // Segment data sliced from the captured range.
        let response = warp::test::request()
            .path("/segment-0.mp4")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body().as_ref(), b"ABCDEFGHIJ");
        // A #EXT-X-GAP segment was never captured: not found.
        let response = warp::test::request()
            .path("/segment-1.mp4")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Serving from a directory behaves the same as serving from memory.
    #[tokio::test]
    async fn replay_directory_store() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path());
        import_har_into(
            load_har_fixture("vod.har"),
            None,
            storage,
            all_options(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let raw_recording = fs::read_to_string(dir.path().join("recording.json"))
            .await
            .unwrap();
        let recording: Recording = serde_json::from_str(&raw_recording).unwrap();
        let recording_start = *recording.earliest_time().unwrap();
        let routes = routes(
            Arc::new(Store::Directory {
                dir: dir.path().to_path_buf(),
                recording,
            }),
            recording_start,
        );
        let response = warp::test::request()
            .path("/index.m3u8")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let response = warp::test::request()
            .path("/variant1/index.m3u8?start=1")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(response.body().to_vec()).unwrap();
        assert!(!body.contains("ORIGINAL"));
        let response = warp::test::request()
            .path("/variant1/segment-0.ts")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body().as_ref(), b"seg-1");
        let response = warp::test::request()
            .path("/variant1/nope.ts")
            .reply(&routes)
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
