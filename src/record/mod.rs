use anyhow::anyhow;
use chrono::{DateTime, Utc};
use futures::future::{BoxFuture, FutureExt};
use futures::stream::{StreamExt, TryStreamExt, iter};
use m3u8_rs::*;
use reqwest::Client;
use std::io;
use std::path::{Path, PathBuf};
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

use crate::shared::{ByteRange, MediaSelect, Recording, VariantSelectOptions};
pub use rewrite::*;

mod rewrite;

const MAX_CONCURRENT_DOWNLOADS: usize = 4;

#[derive(Debug, Copy, Clone)]
pub struct RecordOptions {
    pub variant_select: VariantSelectOptions,
    pub audio: MediaSelect,
    pub video: MediaSelect,
    pub subtitle: MediaSelect,
    pub start: Option<f32>,
    pub end: Option<f32>,
}

#[derive(thiserror::Error, Debug)]
pub enum RecordError {
    #[error("configuration error: {0}")]
    Config(&'static str),
    #[error("parse error: {0}")]
    Parse(#[source] anyhow::Error),
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
    fs::create_dir_all(dest).await?;
    let recording_path = dest.join("recording.json");
    let recording = RecordingFile::new(&recording_path).await?;
    let recording = Arc::new(Mutex::new(recording));
    // Download initial playlist
    let client = Client::new();
    let raw_playlist = token
        .run_until_cancelled(download_playlist(&client, url))
        .await
        .ok_or(RecordError::Cancelled)??;
    let initial_playlist = parse_playlist_res(raw_playlist.as_bytes())
        .map_err(|e| RecordError::Parse(anyhow!("Error while parsing playlist: {e}")))?;
    match initial_playlist {
        Playlist::MasterPlaylist(master_playlist) => {
            // Master playlist
            record_master_playlist(
                &client,
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
                &client,
                url,
                "",
                Some(media_playlist),
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
    client: &Client,
    url: &Url,
    dest: &Path,
    recording: Arc<Mutex<RecordingFile>>,
    options: RecordOptions,
    master_playlist: MasterPlaylist,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let mut new_master_playlist = master_playlist.clone();
    // Select variant streams
    new_master_playlist.variants = options
        .variant_select
        .filter_variants(&master_playlist.variants)
        .to_vec();
    if new_master_playlist.variants.is_empty() {
        return Err(RecordError::Config("No variant streams selected."));
    }
    // Select renditions
    let alternatives = &mut new_master_playlist.alternatives;
    alternatives.retain(|media| {
        // Must apply to at least one selected variant stream
        new_master_playlist
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

    new_master_playlist.alternatives = audio_renditions;
    new_master_playlist.alternatives.extend(video_renditions);
    new_master_playlist.alternatives.extend(subtitle_renditions);
    new_master_playlist.alternatives.extend(cc_renditions);
    new_master_playlist.alternatives.extend(other_renditions);

    // Start recording selected variant streams and renditions
    let mut join_set = JoinSet::new();
    for (i, variant) in new_master_playlist.variants.iter_mut().enumerate() {
        let variant_url = url
            .join(&variant.uri)
            .map_err(|_| RecordError::Parse(anyhow!("Bad URL: {}", &variant.uri)))?;
        let variant_dir = format!("variant{i}/");
        let client = client.clone();
        let dest = PathBuf::from(dest);
        let recording = recording.clone();
        variant.other_attributes.get_or_insert_default().insert(
            ORIGINAL_URI.to_string(),
            QuotedOrUnquoted::Quoted(variant_url.as_str().to_string()),
        );
        variant.uri = format!("{variant_dir}index.m3u8");
        let token = token.clone();
        join_set.spawn(async move {
            record_media_playlist(
                &client,
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
    for (i, media) in new_master_playlist.alternatives.iter_mut().enumerate() {
        let Some(media_uri) = &media.uri else {
            continue;
        };
        let media_url = url
            .join(media_uri)
            .map_err(|e| RecordError::Parse(anyhow!("Error while parsing playlist: {e}")))?;
        let media_dir = format!("media-{}-{}/", media.group_id, i);
        let client = client.clone();
        let dest = PathBuf::from(dest);
        let recording = recording.clone();
        media.other_attributes.get_or_insert_default().insert(
            ORIGINAL_URI.to_string(),
            QuotedOrUnquoted::Quoted(media_url.as_str().to_string()),
        );
        media.uri = Some(format!("{media_dir}index.m3u8"));
        let token = token.clone();
        join_set.spawn(async move {
            record_media_playlist(
                &client, &media_url, &media_dir, None, &dest, recording, options, token,
            )
            .await
        });
    }
    // Write updated master playlist
    let master_name = "index.m3u8";
    write_master_playlist(&dest.join(master_name), &new_master_playlist).await?;
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

async fn record_media_playlist(
    client: &Client,
    url: &Url,
    dir: &str,
    mut initial_playlist: Option<MediaPlaylist>,
    dest: &Path,
    recording: Arc<Mutex<RecordingFile>>,
    mut options: RecordOptions,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let name_in_recording = format!("{dir}index.m3u8");
    let dest = dest.join(dir);
    fs::create_dir_all(&dest).await?;
    let mut previous_playlist = None;
    let mut last_segment_ext = None;
    let mut lowest_media_sequence = 0;
    let mut highest_media_sequence = None;
    loop {
        // Download and rewrite playlist
        let mut media_playlist = if let Some(playlist) = initial_playlist.take() {
            playlist
        } else {
            let raw_playlist = token
                .run_until_cancelled(download_playlist(client, url))
                .await
                .ok_or(RecordError::Cancelled)??;
            parse_media_playlist_res(raw_playlist.as_bytes()).map_err(|e| {
                RecordError::Parse(anyhow!("Error while parsing media playlist: {e}"))
            })?
        };
        let now = Instant::now();
        let playlist_time = Utc::now();
        let file_name = if previous_playlist.is_none() && media_playlist.end_list {
            // Playlist is a VOD. No need for a timestamp, since we won't ever refresh it.
            "index.m3u8".to_string()
        } else {
            // Playlist is live, or was live and has now ended
            format!("index-{}.m3u8", playlist_time.format("%Y%m%dT%H%M%S"))
        };
        let file_path = format!("{dir}{file_name}");
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
        rewrite_media_playlist(url, &mut media_playlist, &mut last_segment_ext)?;
        write_media_playlist(&dest.join(&file_name), &media_playlist).await?;
        // Update recording
        recording
            .lock()
            .await
            .add_and_save(playlist_time, &name_in_recording, file_path)
            .await?;
        // Download segments
        download_segments(
            client,
            &media_playlist.segments,
            &dest,
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

async fn download_playlist(client: &Client, url: &Url) -> Result<String, RecordError> {
    client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| RecordError::Io(io::Error::other(e)))?
        .text()
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

async fn download_segments(
    client: &Client,
    media_segments: &[MediaSegment],
    dir: &Path,
    max_concurrent_downloads: usize,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let segment_tasks = media_segments
        .iter()
        .flat_map(|segment| make_segment_download_tasks(client, dir, segment, token.clone()));
    iter(segment_tasks)
        .boxed() // https://github.com/rust-lang/rust/issues/104382
        .buffered(max_concurrent_downloads)
        .try_collect::<()>() // drop individual results
        .await
}

fn make_segment_download_tasks<'a>(
    client: &'a Client,
    dir: &'a Path,
    segment: &'a MediaSegment,
    token: CancellationToken,
) -> Vec<BoxFuture<'a, Result<(), RecordError>>> {
    let mut tasks = Vec::with_capacity(3);
    tasks.push(download_segment(client, segment, dir, token.clone()).boxed());
    if let Some(key) = segment.key.as_ref() {
        tasks.push(download_key(client, key, segment, dir, token.clone()).boxed());
    }
    if let Some(map) = segment.map.as_ref() {
        tasks.push(download_map(client, map, segment, dir, token).boxed());
    }
    tasks
}

async fn download_segment(
    client: &Client,
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
        client,
        segment_url,
        segment_byte_range,
        segment_file,
        dir,
        token,
    )
    .await
}

async fn download_key(
    client: &Client,
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
        client,
        key_uri.as_str(),
        None,
        key_file.as_str(),
        dir,
        token,
    )
    .await
}

async fn download_map(
    client: &Client,
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
        client,
        map_uri.as_str(),
        map_byte_range,
        map_file,
        dir,
        token,
    )
    .await
}

async fn download_file(
    client: &Client,
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
    let range_header = byte_range.map(|byte_range| {
        let start = byte_range.offset;
        let end = start + byte_range.length - 1; // end byte for a range request is inclusive!
        format!("bytes={start}-{end}")
    });
    println!(
        "Download: {url} {}",
        range_header.as_ref().unwrap_or(&String::new())
    );
    let mut request = client.get(url);
    if let Some(range_header) = range_header {
        request = request.header(reqwest::header::RANGE, range_header);
    }
    let response = token
        .run_until_cancelled(request.send())
        .await
        .ok_or(RecordError::Cancelled)?
        .map_err(|e| RecordError::Io(io::Error::other(e)))?;
    let response_stream = response.bytes_stream().map_err(io::Error::other);
    let mut response_stream = StreamReader::new(response_stream);
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
        let client = Client::new();
        let token = CancellationToken::new();

        fn require_send<T: Send>(_t: T) {}
        require_send(download_playlist(&client, &url));
        require_send(write_master_playlist(path, &MasterPlaylist::default()));
        require_send(write_media_playlist(path, &MediaPlaylist::default()));
        require_send(download_segments(&client, &[], path, 0, token.clone()));
        require_send(download_segment(
            &client,
            &MediaSegment::empty(),
            path,
            token.clone(),
        ));
        require_send(download_key(
            &client,
            &Key::default(),
            &MediaSegment::empty(),
            path,
            token.clone(),
        ));
        require_send(download_map(
            &client,
            &Map::default(),
            &MediaSegment::empty(),
            path,
            token.clone(),
        ));
        require_send(download_file(&client, "", None, "", path, token.clone()));
    }
}
