use anyhow::anyhow;
use chrono::{DateTime, Utc};
use futures::future::{BoxFuture, FutureExt};
use futures::stream::{StreamExt, TryStreamExt, iter};
use m3u8_rs::*;
use reqwest::Client;
use reqwest::header::HeaderMap;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio::time::sleep_until;
use tokio_util::io::StreamReader;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::shared::{ByteRange, MediaSelect, Recording, StripBom, Timed, VariantSelectOptions};
pub use rewrite::*;
pub use source::*;

mod rewrite;
mod source;

const MAX_CONCURRENT_DOWNLOADS: usize = 4;

#[derive(Debug, Clone)]
pub struct RecordOptions {
    pub variant_select: VariantSelectOptions,
    pub audio: MediaSelect,
    pub video: MediaSelect,
    pub subtitle: MediaSelect,
    pub start: Option<f32>,
    pub end: Option<f32>,
    pub headers: HeaderMap,
    pub keep_names: bool,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum RecordError {
    #[error("configuration error: {0}")]
    Config(&'static str),
    #[error("parse error: {0}")]
    Parse(#[source] anyhow::Error),
    #[error("rewrite error: {0}")]
    Rewrite(#[from] RewriteError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("cancelled")]
    Cancelled,
}

pub async fn record(
    url: &Url,
    dest: &Path,
    options: RecordOptions,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let client = Client::builder()
        .cookie_store(true)
        .default_headers(options.headers.clone())
        .build()
        .map_err(|_| RecordError::Config("Error while building HTTP client"))?;
    let source = HttpSource::new(client);
    record_with_source(url, dest, options, source, token).await
}

pub async fn record_with_source(
    url: &Url,
    dest: &Path,
    options: RecordOptions,
    source: impl Source + 'static,
    token: CancellationToken,
) -> Result<(), RecordError> {
    fs::create_dir_all(dest).await?;
    let recording_path = dest.join("recording.json");
    let recording = RecordingFile::new(&recording_path).await?;
    let recording = Arc::new(Mutex::new(recording));
    // Download initial playlist
    let Timed {
        value: initial_playlist,
        time: playlist_time,
    } = token
        .run_until_cancelled(download_playlist(&source, url))
        .await
        .ok_or(RecordError::Cancelled)??
        .and_then(|raw_playlist| {
            let raw_playlist = raw_playlist.strip_bom();
            parse_playlist_res(raw_playlist.as_bytes()).map_err(|e| {
                RecordError::Parse(anyhow!(
                    "Error while parsing playlist: {}",
                    e.map_input(|i| String::from_utf8_lossy(i))
                ))
            })
        })?;
    match initial_playlist {
        Playlist::MasterPlaylist(master_playlist) => {
            // Master playlist
            record_master_playlist(
                source,
                url,
                dest,
                recording,
                options,
                master_playlist,
                token,
            )
            .await?;
        }
        Playlist::MediaPlaylist(media_playlist) => {
            // Media playlist only
            record_media_playlist(
                source,
                url,
                "",
                Some(Timed {
                    value: media_playlist,
                    time: playlist_time,
                }),
                dest,
                recording,
                options,
                token,
            )
            .await?;
        }
    }
    Ok(())
}

async fn record_master_playlist(
    source: impl Source + 'static,
    url: &Url,
    dest: &Path,
    recording: Arc<Mutex<RecordingFile>>,
    options: RecordOptions,
    mut master_playlist: MasterPlaylist,
    token: CancellationToken,
) -> Result<(), RecordError> {
    // Rewrite master playlist
    let rewriter = Rewriter::new(url, dest, options.keep_names);
    rewriter.rewrite_master_playlist(&mut master_playlist)?;

    // Select variant streams
    master_playlist.variants = options
        .variant_select
        .filter_variants(&master_playlist.variants)
        .to_vec();
    if master_playlist.variants.is_empty() {
        return Err(RecordError::Config("No variant streams selected."));
    }
    // Select renditions
    let alternatives = &mut master_playlist.alternatives;
    alternatives.retain(|media| {
        // Must apply to at least one selected variant stream
        master_playlist
            .variants
            .iter()
            .any(|variant| media_applies_to_variant(media, variant))
    });
    let audio_renditions = alternatives
        .iter()
        .filter(|media| media.media_type == AlternativeMediaType::Audio)
        .cloned()
        .collect::<Vec<_>>();
    let video_renditions = alternatives
        .iter()
        .filter(|media| media.media_type == AlternativeMediaType::Video)
        .cloned()
        .collect::<Vec<_>>();
    let subtitle_renditions = alternatives
        .iter()
        .filter(|media| media.media_type == AlternativeMediaType::Subtitles)
        .cloned()
        .collect::<Vec<_>>();
    let cc_renditions = alternatives
        .iter()
        .filter(|media| media.media_type == AlternativeMediaType::ClosedCaptions)
        .cloned()
        .collect::<Vec<_>>();
    let other_renditions = alternatives
        .iter()
        .filter(|media| matches!(media.media_type, AlternativeMediaType::Other(_)))
        .cloned()
        .collect::<Vec<_>>();

    let audio_renditions = options.audio.filter_media(&audio_renditions).to_vec();
    let video_renditions = options.video.filter_media(&video_renditions).to_vec();
    let subtitle_renditions = options.subtitle.filter_media(&subtitle_renditions).to_vec();

    master_playlist.alternatives = audio_renditions;
    master_playlist.alternatives.extend(video_renditions);
    master_playlist.alternatives.extend(subtitle_renditions);
    master_playlist.alternatives.extend(cc_renditions);
    master_playlist.alternatives.extend(other_renditions);

    let master_playlist = master_playlist;

    // Start recording selected variant streams and renditions
    let mut join_set = JoinSet::new();
    for variant in &master_playlist.variants {
        let Some(other_attributes) = &variant.other_attributes else {
            continue;
        };
        let Some(variant_url) = other_attributes.get(ORIGINAL_URI) else {
            continue;
        };
        let variant_url = Url::parse(variant_url.as_str()).unwrap();
        let variant_dir = Path::new(&variant.uri)
            .parent()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let source = source.clone();
        let dest = PathBuf::from(dest);
        let recording = recording.clone();
        let options = options.clone();
        let token = token.clone();
        join_set.spawn(async move {
            record_media_playlist(
                source,
                &variant_url,
                &variant_dir,
                None,
                &dest,
                recording,
                options,
                token,
            )
            .await
        });
    }
    for media in &master_playlist.alternatives {
        let Some(media_uri) = &media.uri else {
            continue;
        };
        let Some(other_attributes) = &media.other_attributes else {
            continue;
        };
        let Some(media_url) = other_attributes.get(ORIGINAL_URI) else {
            continue;
        };
        let media_url = Url::parse(media_url.as_str()).unwrap();
        let media_dir = Path::new(media_uri)
            .parent()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let source = source.clone();
        let dest = PathBuf::from(dest);
        let recording = recording.clone();
        let options = options.clone();
        let token = token.clone();
        join_set.spawn(async move {
            record_media_playlist(
                source, &media_url, &media_dir, None, &dest, recording, options, token,
            )
            .await
        });
    }
    // Write updated master playlist
    let master_name = "index.m3u8";
    write_master_playlist(&dest.join(master_name), &master_playlist).await?;
    recording
        .lock()
        .await
        .add_and_save(Utc::now(), master_name, master_name.to_string())
        .await?;
    // Wait for all tasks to complete
    while let Some(res) = join_set.join_next().await {
        res.map_err(|_| RecordError::Cancelled)??;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn record_media_playlist<S: Source>(
    source: S,
    url: &Url,
    dir: &str,
    mut initial_playlist: Option<Timed<MediaPlaylist>>,
    dest: &Path,
    recording: Arc<Mutex<RecordingFile>>,
    mut options: RecordOptions,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let dest_dir = dest.join(dir);
    fs::create_dir_all(&dest_dir).await?;
    let mut rewriter = Rewriter::new(url, dir.as_ref(), options.keep_names);
    let name_in_recording = rewriter.playlist_path();
    let mut previous_playlist = None;
    let mut lowest_media_sequence = 0;
    let mut highest_media_sequence = None;
    loop {
        // Download and rewrite playlist
        let Timed {
            value: mut media_playlist,
            time: playlist_time,
        } = if let Some(playlist) = initial_playlist.take() {
            playlist
        } else {
            token
                .run_until_cancelled(download_playlist(&source, url))
                .await
                .ok_or(RecordError::Cancelled)??
                .and_then(|raw_playlist| {
                    let raw_playlist = raw_playlist.strip_bom();
                    parse_media_playlist_res(raw_playlist.as_bytes()).map_err(|e| {
                        RecordError::Parse(anyhow!(
                            "Error while parsing media playlist: {}",
                            e.map_input(|i| String::from_utf8_lossy(i))
                        ))
                    })
                })?
        };
        let now = Instant::now();
        let file_name = if previous_playlist.is_none() && media_playlist.end_list {
            // Playlist is a VOD. No need for a timestamp, since we won't ever refresh it.
            rewriter.playlist_path()
        } else {
            // Playlist is live, or was live and has now ended
            rewriter.playlist_path_with_timestamp(&playlist_time)
        };
        // Clip to start and end time (if given)
        if let Some(start) = options.start.take()
            && let Some(start_index) = find_segment_index_by_offset(&media_playlist.segments, start)
        {
            lowest_media_sequence = media_playlist.media_sequence + (start_index as u64)
        }
        if let Some(end) = options.end.take()
            && let Some(end_index) = find_segment_index_by_offset(&media_playlist.segments, end)
        {
            highest_media_sequence = Some(media_playlist.media_sequence + (end_index as u64))
        }
        remove_segments_from_start(&mut media_playlist, lowest_media_sequence);
        if let Some(highest_media_sequence) = highest_media_sequence {
            remove_segments_from_end(&mut media_playlist, highest_media_sequence);
        }
        rewriter.rewrite_media_playlist(&mut media_playlist)?;
        write_media_playlist(&dest.join(&file_name), &media_playlist).await?;
        // Update recording
        recording
            .lock()
            .await
            .add_and_save(playlist_time, &name_in_recording, file_name.to_string())
            .await?;
        // Download segments
        download_segments(
            &source,
            &media_playlist.segments,
            &dest_dir,
            MAX_CONCURRENT_DOWNLOADS,
            token.clone(),
        )
        .await?;
        // Refresh playlist
        if media_playlist.end_list {
            break;
        }
        let next_refresh_time = now + Duration::from_secs(media_playlist.target_duration);
        token
            .run_until_cancelled(sleep_until(next_refresh_time.into()))
            .await
            .ok_or(RecordError::Cancelled)?;
        previous_playlist = Some(media_playlist);
    }
    Ok(())
}

async fn download_playlist<S: Source>(source: &S, url: &Url) -> Result<Timed<String>, RecordError> {
    source
        .request_string(url, None)
        .await
        .map_err(|e| RecordError::Io(io::Error::other(e)))
}

async fn write_master_playlist(
    file_path: &Path,
    playlist: &MasterPlaylist,
) -> Result<(), RecordError> {
    let mut playlist_file = fs::File::create(file_path).await?;
    let mut buffer = vec![];
    playlist.write_to(&mut buffer)?;
    playlist_file.write_all(&buffer).await?;
    Ok(())
}

async fn write_media_playlist(
    file_path: &Path,
    playlist: &MediaPlaylist,
) -> Result<(), RecordError> {
    let mut playlist_file = fs::File::create(file_path).await?;
    let mut buffer = vec![];
    playlist.write_to(&mut buffer)?;
    playlist_file.write_all(&buffer).await?;
    Ok(())
}

fn find_segment_index_by_offset(segments: &[MediaSegment], offset: f32) -> Option<usize> {
    #[inline]
    fn find<'a>(
        iter: impl Iterator<Item = (usize, &'a MediaSegment)>,
        target_time: f32,
    ) -> Option<usize> {
        let mut start_time = 0.0;
        for (index, segment) in iter {
            let end_time = start_time + segment.duration;
            if (start_time..end_time).contains(&target_time) {
                return Some(index);
            }
            start_time = end_time;
        }
        None
    }

    if offset >= 0.0 {
        find(segments.iter().enumerate(), offset)
    } else {
        find(segments.iter().enumerate().rev(), -offset)
    }
}

async fn download_segments<S: Source>(
    source: &S,
    media_segments: &[MediaSegment],
    dir: &Path,
    max_concurrent_downloads: usize,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let segment_tasks = media_segments
        .iter()
        .flat_map(|segment| make_segment_download_tasks(source, dir, segment, token.clone()));
    iter(segment_tasks)
        .boxed() // https://github.com/rust-lang/rust/issues/104382
        .buffered(max_concurrent_downloads)
        .try_collect::<()>() // drop individual results
        .await
}

fn make_segment_download_tasks<'a, S: Source>(
    source: &'a S,
    dir: &'a Path,
    segment: &'a MediaSegment,
    token: CancellationToken,
) -> Vec<BoxFuture<'a, Result<(), RecordError>>> {
    let mut tasks = Vec::with_capacity(3);
    tasks.push(download_segment(source, segment, dir, token.clone()).boxed());
    if let Some(key) = segment.key.as_ref() {
        tasks.push(download_key(source, key, segment, dir, token.clone()).boxed());
    }
    if let Some(map) = segment.map.as_ref() {
        tasks.push(download_map(source, map, segment, dir, token).boxed());
    }
    tasks
}

async fn download_segment<S: Source>(
    source: &S,
    media_segment: &MediaSegment,
    dir: &Path,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let segment_url = media_segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == ORIGINAL_URI)
        .ok_or_else(|| RecordError::Parse(anyhow!("Expected original URL in #EXT-ORIGINAL_URI")))?
        .rest
        .as_ref()
        .unwrap();
    let segment_byte_range = media_segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == ORIGINAL_BYTE_RANGE)
        .map(|ext_tag| ext_tag.rest.as_ref().unwrap().parse())
        .transpose()
        .map_err(|e| {
            RecordError::Parse(anyhow!("Invalid byte range in #X-ORIGINAL-BYTE-RANGE: {e}"))
        })?;
    let segment_file = &media_segment.uri;
    download_file(
        source,
        segment_url,
        segment_byte_range,
        segment_file,
        dir,
        token,
    )
    .await
}

async fn download_key<S: Source>(
    source: &S,
    key: &Key,
    media_segment: &MediaSegment,
    dir: &Path,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let Some(original_key_tag) = &media_segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == ORIGINAL_KEY_URI)
    else {
        return Ok(());
    };
    let key_uri = original_key_tag.rest.as_ref().unwrap();
    let key_file = key.uri.as_ref().unwrap();
    download_file(
        source,
        key_uri.as_str(),
        None,
        key_file.as_str(),
        dir,
        token,
    )
    .await
}

async fn download_map<S: Source>(
    source: &S,
    map: &Map,
    media_segment: &MediaSegment,
    dir: &Path,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let Some(original_map_tag) = &media_segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == ORIGINAL_MAP_URI)
    else {
        return Ok(());
    };
    let map_uri = original_map_tag.rest.as_ref().unwrap();
    let map_byte_range = map
        .other_attributes
        .get(ORIGINAL_BYTE_RANGE)
        .map(|byte_range| byte_range.as_str().parse())
        .transpose()
        .map_err(|e| {
            RecordError::Parse(anyhow!("Invalid byte range in #X-ORIGINAL-BYTE-RANGE: {e}"))
        })?;
    let map_file = &map.uri;
    download_file(
        source,
        map_uri.as_str(),
        map_byte_range,
        map_file,
        dir,
        token,
    )
    .await
}

async fn download_file<S: Source>(
    source: &S,
    url: &str,
    byte_range: Option<ByteRange>,
    file_name: &str,
    dir: &Path,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let absolute_path = dir.join(file_name);
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&absolute_path)
        .await
    {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let url = url
        .parse::<Url>()
        .map_err(|e| RecordError::Parse(anyhow!("Invalid URL: {e}")))?;
    let response_stream = token
        .run_until_cancelled(source.request_stream(&url, byte_range))
        .await
        .ok_or(RecordError::Cancelled)?
        .map_err(io::Error::other);
    let mut response_stream = pin!(StreamReader::new(response_stream));
    tokio::io::copy_buf(&mut response_stream, &mut file).await?;
    Ok(())
}

fn media_applies_to_variant(media: &AlternativeMedia, variant_stream: &VariantStream) -> bool {
    match media.media_type {
        AlternativeMediaType::Audio => variant_stream.audio.as_ref() == Some(&media.group_id),
        AlternativeMediaType::Video => variant_stream.video.as_ref() == Some(&media.group_id),
        AlternativeMediaType::Subtitles => {
            variant_stream.subtitles.as_ref() == Some(&media.group_id)
        }
        AlternativeMediaType::ClosedCaptions => matches!(
            variant_stream.closed_captions,
            Some(ClosedCaptionGroupId::GroupId(ref group_id)) if group_id == &media.group_id
        ),
        AlternativeMediaType::Other(_) => false,
    }
}

struct RecordingFile {
    recording: Recording,
    file: fs::File,
}

impl RecordingFile {
    async fn new(path: &Path) -> io::Result<Self> {
        let file = fs::File::create(path).await?;
        Ok(Self {
            recording: Recording::new(),
            file,
        })
    }

    async fn add_and_save(
        &mut self,
        time: DateTime<Utc>,
        playlist_name: &str,
        playlist_path: String,
    ) -> io::Result<()> {
        self.recording.add(time, playlist_name, playlist_path);
        self.save().await
    }

    async fn save(&mut self) -> io::Result<()> {
        let recording_json = serde_json::to_string_pretty(&self.recording)?;
        let recording_bytes = recording_json.as_bytes();
        self.file.rewind().await?;
        self.file.write_all(recording_bytes).await?;
        self.file.set_len(recording_bytes.len() as u64).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test whether all `async fn`s are `Send`,
    /// so they can be scheduled on Tokio's multi-threaded runtime.
    #[test]
    fn require_async_fn_to_be_send() {
        let url = Url::parse("https://a.com/").unwrap();
        let path = Path::new("");
        let source = HttpSource::new(Client::new());
        let token = CancellationToken::new();

        fn require_send<T: Send>(_t: T) {}
        require_send(download_playlist(&source, &url));
        require_send(write_master_playlist(path, &MasterPlaylist::default()));
        require_send(write_media_playlist(path, &MediaPlaylist::default()));
        require_send(download_segments(&source, &[], path, 0, token.clone()));
        require_send(download_segment(
            &source,
            &MediaSegment::empty(),
            path,
            token.clone(),
        ));
        require_send(download_key(
            &source,
            &Key::default(),
            &MediaSegment::empty(),
            path,
            token.clone(),
        ));
        require_send(download_map(
            &source,
            &Map::default(),
            &MediaSegment::empty(),
            path,
            token.clone(),
        ));
        require_send(download_file(&source, "", None, "", path, token.clone()));
    }
}
