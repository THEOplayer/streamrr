use std::convert::Infallible;
use std::fmt::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use m3u8_rs::Playlist;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use warp::http::{Response, StatusCode, Uri, header::CONTENT_TYPE};
use warp::path::{FullPath, Peek, Tail};
use warp::reject::{Reject, custom};
use warp::{Filter, Rejection, Reply, reject, reply};

use crate::record::strip_media_playlist;
use crate::shared::Recording;

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
struct PlaylistQueryParams {
    start: Option<i64>,
}

#[derive(thiserror::Error, Debug)]
pub enum ReplayError {
    #[error("Missing recording file")]
    MissingRecording,
    #[error("Invalid recording: {0}")]
    InvalidRecording(#[from] serde_json::Error),
    #[error("Missing start time in recording")]
    MissingStartTime,
    #[error("Cancelled")]
    Cancelled,
}

pub async fn replay(
    recording_path: &Path,
    port: u16,
    token: CancellationToken,
) -> Result<(), ReplayError> {
    let recording_path = recording_path.to_owned();
    let raw_recording = fs::read_to_string(recording_path.join("recording.json"))
        .await
        .map_err(|_| ReplayError::MissingRecording)?;
    let recording = serde_json::from_str::<Recording>(&raw_recording)?;
    let recording_start = *recording
        .earliest_time()
        .ok_or(ReplayError::MissingStartTime)?;

    let segments = warp::fs::dir(recording_path.clone());

    let recording = Arc::new(recording);
    let recording_path = Arc::new(recording_path);

    let with_start = warp::path::tail()
        .and(warp::query::<PlaylistQueryParams>())
        .and_then(move |tail: Tail, params: PlaylistQueryParams| {
            let file_name = tail.as_str().to_string();
            let recording = recording.clone();
            let recording_path = recording_path.clone();
            async move {
                let start = params.start.ok_or_else(reject)?;
                let absolute_path = playlist_path_at_time(
                    &file_name,
                    recording,
                    recording_start,
                    &recording_path,
                    start,
                )
                .ok_or_else(|| custom(ServerError::PlaylistNotFound))?;
                let reply = m3u8_reply(&absolute_path, start)
                    .await
                    .map_err(|_| custom(ServerError::PlaylistFileError))?
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
    let playlist = path_extension(".m3u8").and(with_start.or(without_start));

    let cors = warp::cors().allow_any_origin().build();

    let service = playlist.or(segments).with(cors).recover(handle_rejection);

    let address = SocketAddr::from(([127, 0, 0, 1], port));
    println!("Replay server listening on http://{address}/");
    token
        .run_until_cancelled(warp::serve(service).run(address))
        .await
        .ok_or(ReplayError::Cancelled)?;

    Ok(())
}

fn playlist_path_at_time(
    playlist_name: &str,
    recording: Arc<Recording>,
    recording_start: DateTime<Utc>,
    recording_path: &Path,
    client_start: i64,
) -> Option<PathBuf> {
    // At T = client_start + X, serve the playlist at recording_start + X
    let client_start = Utc.timestamp_millis_opt(client_start).single()?;
    let offset = Utc::now() - client_start;
    let recording_time = recording_start + offset;
    let (_, relative_path) = recording
        .find_latest_before(playlist_name, recording_time)
        .or_else(|| recording.earliest_time_for(playlist_name))?;
    Some(recording_path.join(relative_path))
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

async fn m3u8_reply(path: &Path, start: i64) -> tokio::io::Result<impl Reply + use<>> {
    let mut file = fs::File::open(&path).await?;
    // Parse the playlist
    let mut raw_playlist = Vec::new();
    file.read_to_end(&mut raw_playlist).await?;
    let mut playlist = m3u8_rs::parse_playlist_res(&raw_playlist)
        .map_err(|_| io::Error::other("Error while parsing playlist"))?;
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
enum ServerError {
    #[error("No playlist found")]
    PlaylistNotFound,
    #[error("Failed to load playlist")]
    PlaylistFileError,
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
