mod har;

use crate::record::{HarSource, RecordError, RecordOptions, Recorder};
use crate::shared::url_file_extension;
use anyhow::anyhow;
pub(crate) use har::Har;
use std::io::Error;
use std::path::Path;
use tokio::fs;
use tokio_util::sync::CancellationToken;
use url::Url;

pub async fn import_har(
    har: Har,
    dest: &Path,
    options: RecordOptions,
    token: CancellationToken,
) -> Result<(), RecordError> {
    fs::create_dir_all(dest).await?;

    // Find the first .m3u8 request
    let first_playlist_request = har
        .log
        .entries
        .iter()
        .find(|entry| {
            let url = Url::parse(&entry.request.url).unwrap();
            matches!(url_file_extension(&url), Some("m3u8"))
        })
        .ok_or_else(|| Error::other("no playlist found"))?;
    let url = first_playlist_request
        .request
        .url
        .parse::<Url>()
        .map_err(|e| RecordError::Parse(anyhow!("Invalid URL: {e}")))?;
    let time = first_playlist_request.started_date_time;

    // Create a source that reads from the HAR
    let source = HarSource::new(har, time);

    let recorder = Recorder::new(source, dest, options, token).await?;
    recorder.run(&url).await
}
