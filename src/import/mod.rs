pub(crate) mod har;

use crate::record::{HarSource, RecordError, RecordOptions, Recorder, Source};
use crate::shared::{BOM, StripBom};
use anyhow::anyhow;
use chrono::{DateTime, Utc};
pub use har::Har;
use har::{Entry, response_range};
use std::io::Error;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use url::Url;

const HLS_MIME_TYPES: &[&str] = &[
    "application/vnd.apple.mpegurl",
    "application/x-mpegurl",
    "audio/mpegurl",
    "audio/x-mpegurl",
];

/// Whether the entry is a usable response carrying a complete HLS playlist,
/// detected by MIME type or by content.
fn is_hls_playlist(entry: &Entry) -> bool {
    if !entry.request.method.eq_ignore_ascii_case("GET") {
        return false;
    }
    if !response_range(entry).is_some_and(|body| body.complete) {
        return false;
    }
    let mime_type = entry
        .response
        .content
        .mime_type
        .as_deref()
        .map(|mime_type| mime_type.split(';').next().unwrap_or_default().trim());
    if mime_type.is_some_and(|mime_type| {
        HLS_MIME_TYPES
            .iter()
            .any(|hls_mime_type| mime_type.eq_ignore_ascii_case(hls_mime_type))
    }) {
        return true;
    }
    // Sniff the content only if the MIME type doesn't match.
    const SNIFF_LEN: usize = BOM.len() + b"#EXTM3U".len();
    let Some(bytes) = entry.response.content.prefix(SNIFF_LEN) else {
        return false;
    };
    bytes.as_slice().strip_bom().starts_with(b"#EXTM3U")
}

pub async fn import_har(
    har: Har,
    url: Option<Url>,
    dest: &Path,
    options: RecordOptions,
    token: CancellationToken,
) -> Result<(), RecordError> {
    // Create a source that reads from the HAR
    let (url, source) = match url {
        Some(url) => {
            // Start at the earliest usable entry for the requested URL.
            let source = HarSource::new(har, DateTime::<Utc>::MIN_UTC);
            if !source.is_available(&url, None).await {
                return Err(RecordError::NotFound(url));
            }
            (url, source)
        }
        None => {
            // Find the first HLS playlist in the HAR
            let mut entries: Vec<&Entry> = har.log.entries.iter().collect();
            entries.sort_by_key(|entry| entry.started_date_time);
            let first_playlist_request = entries
                .into_iter()
                .find(|entry| is_hls_playlist(entry))
                .ok_or_else(|| Error::other("no playlist found"))?;
            let url = first_playlist_request
                .request
                .url
                .parse::<Url>()
                .map_err(|e| RecordError::Parse(anyhow!("Invalid URL: {e}")))?;
            let time = first_playlist_request.started_date_time;
            (url, HarSource::new(har, time))
        }
    };
    println!("Importing {url}");

    let recorder = Recorder::new(source, dest, options).await?;
    recorder.run(&url, token).await
}
