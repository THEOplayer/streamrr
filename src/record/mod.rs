use anyhow::anyhow;
use chrono::{DateTime, Utc};
use futures::future::{BoxFuture, FutureExt};
use futures::stream::{StreamExt, TryStreamExt, iter};
use m3u8_rs::*;
use reqwest::Client;
use reqwest::header::HeaderMap;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::shared::{
    ByteRange, MediaSelect, Recording, StripBom, Timed, VariantSelectOptions, join_path,
};
pub use rewrite::*;
pub use source::*;
pub use storage::*;

mod rewrite;
mod source;
mod storage;

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
    #[error("not found: {0}")]
    NotFound(Url),
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
    let storage = FsStorage::new(dest);
    let recorder = Recorder::new(source, storage, options);
    recorder.run(url, token).await
}

#[derive(Clone)]
pub struct Recorder<S: Source, T: Storage> {
    source: S,
    storage: T,
    recording: Arc<Mutex<Recording>>,
    options: RecordOptions,
}

impl<S: Source + 'static, T: Storage> Recorder<S, T> {
    pub fn new(source: S, storage: T, options: RecordOptions) -> Self {
        Self {
            source,
            storage,
            recording: Arc::new(Mutex::new(Recording::new())),
            options,
        }
    }

    pub async fn run(mut self, url: &Url, token: CancellationToken) -> Result<(), RecordError> {
        // Download initial playlist
        let Timed {
            value: initial_playlist,
            time: playlist_time,
        } = token
            .run_until_cancelled(download_playlist(&self.source, url))
            .await
            .ok_or(RecordError::Cancelled)??
            .ok_or_else(|| RecordError::NotFound(url.clone()))?
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
                self.record_master_playlist(url, master_playlist, playlist_time, token)
                    .await?;
            }
            Playlist::MediaPlaylist(media_playlist) => {
                // Media playlist only
                self.record_media_playlist(
                    url,
                    "",
                    Some(Timed {
                        value: media_playlist,
                        time: playlist_time,
                    }),
                    token,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn record_master_playlist(
        &self,
        url: &Url,
        mut master_playlist: MasterPlaylist,
        playlist_time: DateTime<Utc>,
        token: CancellationToken,
    ) -> Result<(), RecordError> {
        // Rewrite master playlist
        let rewriter = Rewriter::new(url, Path::new(""), self.options.keep_names);
        rewriter.rewrite_master_playlist(&mut master_playlist)?;

        // Drop variant streams and renditions that are not available from the source.
        let mut variants = Vec::with_capacity(master_playlist.variants.len());
        for variant in std::mem::take(&mut master_playlist.variants) {
            if self
                .is_original_uri_available(variant.other_attributes.as_ref())
                .await
            {
                variants.push(variant);
            }
        }
        master_playlist.variants = variants;
        let mut alternatives = Vec::with_capacity(master_playlist.alternatives.len());
        for media in std::mem::take(&mut master_playlist.alternatives) {
            if media.uri.is_none()
                || self
                    .is_original_uri_available(media.other_attributes.as_ref())
                    .await
            {
                alternatives.push(media);
            }
        }
        master_playlist.alternatives = alternatives;

        // Select variant streams and renditions
        self.select_variants(&mut master_playlist);
        if master_playlist.variants.is_empty() {
            return Err(RecordError::Config("No variant streams selected."));
        }
        self.select_renditions(&mut master_playlist);
        let master_playlist = master_playlist;

        // Write updated master playlist
        let master_name = "index.m3u8";
        let mut buffer = vec![];
        master_playlist.write_to(&mut buffer)?;
        self.storage.write_playlist(master_name, buffer).await?;
        self.save_recording(playlist_time, master_name, master_name.to_string())
            .await?;

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
            let mut recorder = self.clone();
            let token = token.clone();
            join_set.spawn(async move {
                recorder
                    .record_media_playlist(&variant_url, &variant_dir, None, token)
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
            let mut recorder = self.clone();
            let token = token.clone();
            join_set.spawn(async move {
                recorder
                    .record_media_playlist(&media_url, &media_dir, None, token)
                    .await
            });
        }
        // Wait for all tasks to complete
        while let Some(res) = join_set.join_next().await {
            res.map_err(|_| RecordError::Cancelled)??;
        }
        Ok(())
    }

    /// Add a playlist to the recording and save it.
    async fn save_recording(
        &self,
        time: DateTime<Utc>,
        name: &str,
        path: String,
    ) -> Result<(), RecordError> {
        let mut recording = self.recording.lock().await;
        recording.add(time, name, path);
        self.storage.save_recording(&recording).await?;
        Ok(())
    }

    /// Whether the original URL stored in `other_attributes` is available
    /// from the source. Entries without an original URL are kept.
    async fn is_original_uri_available(
        &self,
        other_attributes: Option<&HashMap<String, QuotedOrUnquoted>>,
    ) -> bool {
        let Some(url) = other_attributes
            .and_then(|attrs| attrs.get(ORIGINAL_URI))
            .and_then(|uri| Url::parse(uri.as_str()).ok())
        else {
            return true;
        };
        let available = self.source.is_available(&url, None).await;
        if !available {
            println!("Skipping {url}: not available");
        }
        available
    }

    fn select_variants(&self, master_playlist: &mut MasterPlaylist) {
        master_playlist.variants = self
            .options
            .variant_select
            .filter_variants(&master_playlist.variants)
            .to_vec();
    }

    fn select_renditions(&self, master_playlist: &mut MasterPlaylist) {
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

        let audio_renditions = self.options.audio.filter_media(&audio_renditions).to_vec();
        let video_renditions = self.options.video.filter_media(&video_renditions).to_vec();
        let subtitle_renditions = self
            .options
            .subtitle
            .filter_media(&subtitle_renditions)
            .to_vec();

        master_playlist.alternatives = audio_renditions;
        master_playlist.alternatives.extend(video_renditions);
        master_playlist.alternatives.extend(subtitle_renditions);
        master_playlist.alternatives.extend(cc_renditions);
        master_playlist.alternatives.extend(other_renditions);
    }

    async fn record_media_playlist(
        &mut self,
        url: &Url,
        dir: &str,
        mut initial_playlist: Option<Timed<MediaPlaylist>>,
        token: CancellationToken,
    ) -> Result<(), RecordError> {
        let mut rewriter = Rewriter::new(url, dir.as_ref(), self.options.keep_names);
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
                let Some(raw_playlist) = token
                    .run_until_cancelled(download_playlist(&self.source, url))
                    .await
                    .ok_or(RecordError::Cancelled)??
                else {
                    // The source has no more versions of this playlist.
                    break;
                };
                raw_playlist.and_then(|raw_playlist| {
                    let raw_playlist = raw_playlist.strip_bom();
                    parse_media_playlist_res(raw_playlist.as_bytes()).map_err(|e| {
                        RecordError::Parse(anyhow!(
                            "Error while parsing media playlist: {}",
                            e.map_input(|i| String::from_utf8_lossy(i))
                        ))
                    })
                })?
            };
            let file_name = if previous_playlist.is_none() && media_playlist.end_list {
                // Playlist is a VOD. No need for a timestamp, since we won't ever refresh it.
                rewriter.playlist_path()
            } else {
                // Playlist is live, or was live and has now ended
                rewriter.playlist_path_with_timestamp(&playlist_time)
            };
            // Clip to start and end time (if given)
            if let Some(start) = self.options.start.take()
                && let Some(start_index) =
                    find_segment_index_by_offset(&media_playlist.segments, start)
            {
                lowest_media_sequence = media_playlist.media_sequence + (start_index as u64)
            }
            if let Some(end) = self.options.end.take()
                && let Some(end_index) = find_segment_index_by_offset(&media_playlist.segments, end)
            {
                highest_media_sequence = Some(media_playlist.media_sequence + (end_index as u64))
            }
            remove_segments_from_start(&mut media_playlist, lowest_media_sequence);
            if let Some(highest_media_sequence) = highest_media_sequence {
                remove_segments_from_end(&mut media_playlist, highest_media_sequence);
            }
            rewriter.rewrite_media_playlist(&mut media_playlist)?;
            mark_gap_segments(&self.source, &mut media_playlist).await;
            let mut buffer = vec![];
            media_playlist.write_to(&mut buffer)?;
            self.storage.write_playlist(&file_name, buffer).await?;
            // Update recording
            self.save_recording(playlist_time, &name_in_recording, file_name.to_string())
                .await?;
            // Download segments
            download_segments(
                &self.source,
                &self.storage,
                &media_playlist.segments,
                dir,
                MAX_CONCURRENT_DOWNLOADS,
                token.clone(),
            )
            .await?;
            // Refresh playlist
            if media_playlist.end_list {
                break;
            }
            token
                .run_until_cancelled(self.source.wait_for_refresh(
                    playlist_time,
                    Duration::from_secs(media_playlist.target_duration),
                ))
                .await
                .ok_or(RecordError::Cancelled)?;
            previous_playlist = Some(media_playlist);
        }
        Ok(())
    }
}

/// Whether the resource at `uri` is available from the source,
/// with results cached per (uri, byte range).
async fn check_availability<S: Source>(
    source: &S,
    cache: &mut HashMap<(String, Option<ByteRange>), bool>,
    uri: &str,
    byte_range: Option<ByteRange>,
) -> bool {
    let key = (uri.to_string(), byte_range);
    if let Some(&available) = cache.get(&key) {
        return available;
    }
    let available = match Url::parse(uri) {
        Ok(url) => source.is_available(&url, byte_range).await,
        Err(_) => false,
    };
    cache.insert(key, available);
    available
}

fn original_tag<'a>(segment: &'a MediaSegment, tag: &str) -> Option<&'a str> {
    segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == tag)
        .and_then(|ext_tag| ext_tag.rest.as_deref())
}

/// Mark segments that cannot be downloaded from the source with `#EXT-X-GAP`.
async fn mark_gap_segments<S: Source>(source: &S, media_playlist: &mut MediaPlaylist) {
    let mut availability = HashMap::new();
    // The key and map of a segment also apply to subsequent segments.
    let mut effective_key: Option<String> = None;
    let mut effective_map: Option<(String, Option<ByteRange>)> = None;
    for segment in &mut media_playlist.segments {
        if segment.key.is_some() {
            // A key tag without an original URL (e.g. METHOD=NONE or SAMPLE-AES)
            // resets the effective key.
            effective_key = original_tag(segment, ORIGINAL_KEY_URI).map(str::to_string);
        }
        if let Some(map) = &segment.map
            && let Some(map_uri) = original_tag(segment, ORIGINAL_MAP_URI)
        {
            let map_byte_range = map
                .other_attributes
                .get(ORIGINAL_BYTE_RANGE)
                .and_then(|byte_range| byte_range.as_str().parse().ok());
            effective_map = Some((map_uri.to_string(), map_byte_range));
        }
        if segment
            .unknown_tags
            .iter()
            .any(|ext_tag| ext_tag.tag == GAP)
        {
            continue;
        }
        let mut available = true;
        if let Some(uri) = original_tag(segment, ORIGINAL_URI) {
            let byte_range = original_tag(segment, ORIGINAL_BYTE_RANGE)
                .and_then(|byte_range| byte_range.parse().ok());
            available &= check_availability(source, &mut availability, uri, byte_range).await;
        }
        if let Some((uri, byte_range)) = &effective_map {
            available &= check_availability(source, &mut availability, uri, *byte_range).await;
        }
        if let Some(uri) = &effective_key {
            available &= check_availability(source, &mut availability, uri, None).await;
        }
        if !available {
            segment.unknown_tags.push(ExtTag {
                tag: GAP.to_string(),
                rest: None,
            });
        }
    }
}

async fn download_playlist<S: Source>(
    source: &S,
    url: &Url,
) -> Result<Option<Timed<String>>, RecordError> {
    source
        .request_playlist(url)
        .await
        .map_err(|e| RecordError::Io(io::Error::other(e)))
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

async fn download_segments<S: Source, T: Storage>(
    source: &S,
    storage: &T,
    media_segments: &[MediaSegment],
    dir: &str,
    max_concurrent_downloads: usize,
    token: CancellationToken,
) -> Result<(), RecordError> {
    let segment_tasks = media_segments.iter().flat_map(|segment| {
        make_segment_download_tasks(source, storage, dir, segment, token.clone())
    });
    iter(segment_tasks)
        .boxed() // https://github.com/rust-lang/rust/issues/104382
        .buffered(max_concurrent_downloads)
        .try_collect::<()>() // drop individual results
        .await
}

fn make_segment_download_tasks<'a, S: Source, T: Storage>(
    source: &'a S,
    storage: &'a T,
    dir: &'a str,
    segment: &'a MediaSegment,
    token: CancellationToken,
) -> Vec<BoxFuture<'a, Result<(), RecordError>>> {
    let mut tasks = Vec::with_capacity(3);
    tasks.push(download_segment(source, storage, segment, dir, token.clone()).boxed());
    if let Some(key) = segment.key.as_ref() {
        tasks.push(download_key(source, storage, key, segment, dir, token.clone()).boxed());
    }
    if let Some(map) = segment.map.as_ref() {
        tasks.push(download_map(source, storage, map, segment, dir, token).boxed());
    }
    tasks
}

async fn download_segment<S: Source, T: Storage>(
    source: &S,
    storage: &T,
    media_segment: &MediaSegment,
    dir: &str,
    token: CancellationToken,
) -> Result<(), RecordError> {
    if media_segment
        .unknown_tags
        .iter()
        .any(|ext_tag| ext_tag.tag == GAP)
    {
        return Ok(());
    }
    let segment_url = media_segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == ORIGINAL_URI)
        .ok_or_else(|| RecordError::Parse(anyhow!("Expected original URL in #EXT-ORIGINAL_URI")))?
        .rest
        .as_ref()
        .unwrap()
        .parse::<Url>()
        .map_err(|e| RecordError::Parse(anyhow!("Invalid URL: {e}")))?;
    let segment_byte_range = media_segment
        .unknown_tags
        .iter()
        .find(|ext_tag| ext_tag.tag == ORIGINAL_BYTE_RANGE)
        .map(|ext_tag| ext_tag.rest.as_ref().unwrap().parse())
        .transpose()
        .map_err(|e| {
            RecordError::Parse(anyhow!("Invalid byte range in #X-ORIGINAL-BYTE-RANGE: {e}"))
        })?;
    let path = join_path(Path::new(dir), &media_segment.uri);
    storage
        .store_resource(source, &segment_url, segment_byte_range, &path, token)
        .await
}

async fn download_key<S: Source, T: Storage>(
    source: &S,
    storage: &T,
    key: &Key,
    media_segment: &MediaSegment,
    dir: &str,
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
    let key_url = key_uri
        .as_str()
        .parse::<Url>()
        .map_err(|e| RecordError::Parse(anyhow!("Invalid URL: {e}")))?;
    if !source.is_available(&key_url, None).await {
        return Ok(());
    }
    let key_file = key.uri.as_ref().unwrap();
    let path = join_path(Path::new(dir), key_file.as_str());
    storage
        .store_resource(source, &key_url, None, &path, token)
        .await
}

async fn download_map<S: Source, T: Storage>(
    source: &S,
    storage: &T,
    map: &Map,
    media_segment: &MediaSegment,
    dir: &str,
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
    let map_url = map_uri
        .as_str()
        .parse::<Url>()
        .map_err(|e| RecordError::Parse(anyhow!("Invalid URL: {e}")))?;
    let map_byte_range = map
        .other_attributes
        .get(ORIGINAL_BYTE_RANGE)
        .map(|byte_range| byte_range.as_str().parse())
        .transpose()
        .map_err(|e| {
            RecordError::Parse(anyhow!("Invalid byte range in #X-ORIGINAL-BYTE-RANGE: {e}"))
        })?;
    if !source.is_available(&map_url, map_byte_range).await {
        return Ok(());
    }
    let path = join_path(Path::new(dir), &map.uri);
    storage
        .store_resource(source, &map_url, map_byte_range, &path, token)
        .await
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Test whether all `async fn`s are `Send`,
    /// so they can be scheduled on Tokio's multi-threaded runtime.
    #[tokio::test]
    async fn require_async_fn_to_be_send() {
        let url = Url::parse("https://a.com/").unwrap();
        let dest = tempfile::tempdir().unwrap();
        let source = HttpSource::new(Client::new());
        let storage = FsStorage::new(dest.path());
        let token = CancellationToken::new();

        fn require_send<T: Send>(_t: T) {}
        require_send(download_playlist(&source, &url));
        require_send(download_segments(
            &source,
            &storage,
            &[],
            "",
            0,
            token.clone(),
        ));
        require_send(download_segment(
            &source,
            &storage,
            &MediaSegment::empty(),
            "",
            token.clone(),
        ));
        require_send(download_key(
            &source,
            &storage,
            &Key::default(),
            &MediaSegment::empty(),
            "",
            token.clone(),
        ));
        require_send(download_map(
            &source,
            &storage,
            &Map::default(),
            &MediaSegment::empty(),
            "",
            token.clone(),
        ));
    }
}
