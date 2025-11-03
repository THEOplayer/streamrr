use crate::record::RecordError;
use crate::shared::{ByteRange, hex, url_file_extension};
use anyhow::{Result, anyhow};
use m3u8_rs::*;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use url::Url;

pub const ORIGINAL_URI: &str = "X-ORIGINAL-URI";
pub const ORIGINAL_MAP_URI: &str = "X-ORIGINAL-MAP-URI";
pub const ORIGINAL_KEY_URI: &str = "X-ORIGINAL-KEY-URI";
pub const ORIGINAL_BYTE_RANGE: &str = "X-ORIGINAL-BYTE-RANGE";
pub const ORIGINAL_SERVER_CONTROL: &str = "X-ORIGINAL-SERVER-CONTROL";
pub const ORIGINAL_PRELOAD_HINT: &str = "X-ORIGINAL-PRELOAD-HINT";
pub const ORIGINAL_RENDITION_REPORT: &str = "X-ORIGINAL-RENDITION-REPORT";

const DEFAULT_FILE_EXT: &str = "ts";

pub fn rewrite_media_playlist(
    url: &Url,
    media_playlist: &mut MediaPlaylist,
    last_segment_ext: &mut Option<String>,
) -> Result<(), RecordError> {
    // Rewrite segments
    let mut next_byte_range_start = 0u64;
    for (i, segment) in media_playlist.segments.iter_mut().enumerate() {
        let media_sequence_number = media_playlist.media_sequence + (i as u64);
        rewrite_segment(
            segment,
            media_sequence_number,
            url,
            last_segment_ext,
            &mut next_byte_range_start,
        )?;
    }
    remove_unsupported_tags(&mut media_playlist.unknown_tags);
    Ok(())
}

fn rewrite_segment(
    media_segment: &mut MediaSegment,
    media_sequence_number: u64,
    playlist_url: &Url,
    last_segment_ext: &mut Option<String>,
    next_byte_range_start: &mut u64,
) -> Result<(), RecordError> {
    let segment_url = playlist_url
        .join(&media_segment.uri)
        .map_err(|_| RecordError::Parse(anyhow!("Bad URL: {}", &media_segment.uri)))?;
    let file_ext = get_or_update_file_ext(&segment_url, last_segment_ext);
    let file_name = format!("segment-{media_sequence_number}.{file_ext}");
    // Put original URL and byte range in extra tag, and replace segment URL with rewritten path
    media_segment.unknown_tags.push(ExtTag {
        tag: ORIGINAL_URI.to_string(),
        rest: Some(segment_url.into()),
    });
    media_segment.uri = file_name;
    // Put original byte range in extra tag
    if let Some(byte_range) = media_segment.byte_range.take() {
        let byte_range = ByteRange::from_m3u8(&byte_range, *next_byte_range_start);
        media_segment.unknown_tags.push(ExtTag {
            tag: ORIGINAL_BYTE_RANGE.to_string(),
            rest: Some(byte_range.to_string()),
        });
        *next_byte_range_start = byte_range.offset + byte_range.length;
    } else {
        *next_byte_range_start = 0;
    }
    if let Some(key) = media_segment.key.as_mut() {
        rewrite_key(key, playlist_url, &mut media_segment.unknown_tags)?;
    }
    if let Some(map) = media_segment.map.as_mut() {
        rewrite_map(
            map,
            playlist_url,
            next_byte_range_start,
            &mut media_segment.unknown_tags,
        )?;
    }
    remove_unsupported_tags(&mut media_segment.unknown_tags);
    Ok(())
}

fn rewrite_key(
    key: &mut Key,
    playlist_url: &Url,
    unknown_tags: &mut Vec<ExtTag>,
) -> Result<(), RecordError> {
    if key.method != KeyMethod::AES128 {
        return Ok(());
    }
    let Some(key_uri) = key.uri.as_mut() else {
        return Ok(());
    };
    let key_url = playlist_url
        .join(key_uri)
        .map_err(|_| RecordError::Parse(anyhow!("Bad URL: {}", key_uri)))?;
    if !matches!(key_url.scheme(), "http" | "https") {
        return Ok(());
    }
    // Put original URL in extra tag, and rewrite key URL as relative path
    unknown_tags.push(ExtTag {
        tag: ORIGINAL_KEY_URI.to_string(),
        rest: Some(key_url.as_str().to_string()),
    });
    // Use a hash of the key URL as filename.
    // Don't use the media sequence number, since it's likely that this key will appear
    // on a different segment in a future media playlist.
    let key_url_hash = Sha1::digest(key_url.as_str().as_bytes());
    *key_uri = format!("key-{}.bin", hex(key_url_hash));
    Ok(())
}

fn rewrite_map(
    map: &mut Map,
    playlist_url: &Url,
    next_byte_range_start: &mut u64,
    unknown_tags: &mut Vec<ExtTag>,
) -> Result<(), RecordError> {
    let map_url = playlist_url
        .join(&map.uri)
        .map_err(|_| RecordError::Parse(anyhow!("Bad URL: {}", &map.uri)))?;
    // Put original URL in extra tag, and rewrite map URL as relative path
    unknown_tags.push(ExtTag {
        tag: ORIGINAL_MAP_URI.to_string(),
        rest: Some(map_url.as_str().to_string()),
    });
    // Put original byte range in extra attribute
    rewrite_byte_range_in_attribute(
        &mut map.byte_range,
        &mut map.other_attributes,
        next_byte_range_start,
    );
    // Use a hash of the key URL as filename.
    // Don't use the media sequence number, since it's likely that this key will appear
    // on a different segment in a future media playlist.
    let map_url_hash = Sha1::digest(map_url.as_str().as_bytes());
    let file_ext = url_file_extension(&map_url).unwrap_or(DEFAULT_FILE_EXT);
    let file_name = format!("init-{}.{}", hex(map_url_hash), file_ext);
    map.uri = file_name;
    Ok(())
}

fn rewrite_byte_range_in_attribute(
    byte_range: &mut Option<m3u8_rs::ByteRange>,
    other_attributes: &mut HashMap<String, QuotedOrUnquoted>,
    next_byte_range_start: &mut u64,
) {
    if let Some(byte_range) = byte_range.take() {
        let byte_range = ByteRange::from_m3u8(&byte_range, *next_byte_range_start);
        other_attributes.insert(
            ORIGINAL_BYTE_RANGE.to_string(),
            QuotedOrUnquoted::Quoted(byte_range.to_string()),
        );
        *next_byte_range_start = byte_range.offset + byte_range.length;
    } else {
        *next_byte_range_start = 0;
    }
}

pub fn remove_segments_from_start(media_playlist: &mut MediaPlaylist, lowest_media_sequence: u64) {
    if media_playlist.media_sequence >= lowest_media_sequence {
        return;
    }
    let remove_count = lowest_media_sequence - media_playlist.media_sequence;
    // Remove segments, but keep track of the last key and map
    let mut last_key = None;
    let mut last_map = None;
    for mut removed_segment in media_playlist.segments.drain(0..(remove_count as usize)) {
        if let Some(key) = removed_segment.key.take() {
            last_key = Some(key);
        }
        if let Some(map) = removed_segment.map.take() {
            last_map = Some(map);
        }
    }
    media_playlist.media_sequence = lowest_media_sequence;
    // Put the last key and map onto the new first segment
    if let Some(first_segment) = media_playlist.segments.first_mut() {
        first_segment.key = first_segment.key.take().or(last_key);
        first_segment.map = first_segment.map.take().or(last_map);
    }
}

pub fn remove_segments_from_end(media_playlist: &mut MediaPlaylist, highest_media_sequence: u64) {
    let remove_start = (highest_media_sequence - media_playlist.media_sequence + 1) as usize;
    if remove_start < media_playlist.segments.len() {
        media_playlist.segments.drain(remove_start..);
    }
    // Stop refreshing
    media_playlist.end_list = true;
}

fn get_or_update_file_ext(url: &Url, last_segment_ext: &mut Option<String>) -> String {
    match (url_file_extension(url), last_segment_ext.as_ref()) {
        (Some(ext), _) => {
            let ext = ext.to_owned();
            *last_segment_ext = Some(ext.clone());
            ext
        }
        (None, Some(last_ext)) => last_ext.clone(),
        (None, None) => DEFAULT_FILE_EXT.to_owned(),
    }
}

fn remove_unsupported_tags(ext_tags: &mut Vec<ExtTag>) {
    // LL-HLS is not yet supported
    ext_tags.retain(|tag| {
        !matches!(
            tag.tag.as_str(),
            "X-PART" | "X-PART-INF" | "X-PRELOAD-HINT" | "X-RENDITION-REPORT" | "X-SERVER-CONTROL"
        )
    });
}

/// Strip inserted tags with original playlist information from a media playlist
pub fn strip_media_playlist(media_playlist: &mut MediaPlaylist) {
    media_playlist.unknown_tags.retain_mut(|ext_tag| {
        !matches!(
            ext_tag.tag.as_str(),
            ORIGINAL_SERVER_CONTROL | ORIGINAL_PRELOAD_HINT | ORIGINAL_RENDITION_REPORT
        )
    });
    media_playlist.segments.iter_mut().for_each(strip_segment);
}

fn strip_segment(media_segment: &mut MediaSegment) {
    media_segment
        .unknown_tags
        .retain_mut(|ext_tag| !matches!(ext_tag.tag.as_str(), ORIGINAL_URI | ORIGINAL_BYTE_RANGE));
}
