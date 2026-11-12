use relative_path::RelativePath;
use url::{Position, Url};

pub(crate) fn url_file_name(url: &Url) -> Option<&str> {
    let last_path_segment = url.path_segments()?.next_back()?;
    RelativePath::new(last_path_segment).file_name()
}

pub(crate) fn url_file_extension(url: &Url) -> Option<&str> {
    let last_path_segment = url.path_segments()?.next_back()?;
    RelativePath::new(last_path_segment).extension()
}

/// A URL string without fragment, for comparing HAR request URLs.
pub(crate) fn normalize_url(url: &Url) -> &str {
    &url[..Position::AfterQuery]
}
