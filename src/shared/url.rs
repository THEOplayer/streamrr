use std::path::Path;
use url::Url;

pub(crate) fn url_file_name(url: &Url) -> Option<&str> {
    let last_path_segment = url.path_segments()?.next_back()?;
    Path::new(last_path_segment).file_name()?.to_str()
}

pub(crate) fn url_file_extension(url: &Url) -> Option<&str> {
    let last_path_segment = url.path_segments()?.next_back()?;
    Path::new(last_path_segment).extension()?.to_str()
}
