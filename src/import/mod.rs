mod har;

use crate::shared::url_file_extension;
use har::{Har, Spec};
use std::io::Result;
use std::path::Path;
use tokio::fs;
use url::Url;

pub async fn import_har(har: Har, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).await?;

    let spec = match har.log {
        Spec::V1_2(spec) => spec,
        Spec::V1_3(spec) => spec,
    };

    // Find the first .m3u8 request
    let first_playlist_request = spec.entries.iter().find(|entry| {
        let url = Url::parse(&entry.request.url).unwrap();
        matches!(url_file_extension(&url), Some("m3u8"))
    });

    Ok(())
}
