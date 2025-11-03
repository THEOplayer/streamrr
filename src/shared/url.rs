use std::path::Path;
use url::Url;

pub(crate) fn url_file_extension(url: &Url) -> Option<&str> {
    let file_name = url.path_segments()?.next_back()?;
    Path::new(file_name).extension()?.to_str()
}
