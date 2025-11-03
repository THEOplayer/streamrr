use std::path::Path;
use url::Url;

pub(crate) fn url_file_extension(url: &Url) -> Option<String> {
    let url = Url::parse(url.as_str()).ok()?;
    let file_name = url.path_segments()?.next_back()?;
    Some(Path::new(file_name).extension()?.to_str()?.to_owned())
}
