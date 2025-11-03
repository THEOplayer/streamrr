mod har;

use crate::shared::url_file_extension;
use har::Har;
use std::io::Result;
use std::path::Path;
use tokio::fs;
use url::Url;

pub async fn import_har(har: Har, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).await?;

    let spec = har.log;

    // Find the first .m3u8 request
    let first_playlist_request = spec.entries.iter().find(|entry| {
        let url = Url::parse(&entry.request.url).unwrap();
        matches!(url_file_extension(&url), Some("m3u8"))
    });

    Ok(())
}
