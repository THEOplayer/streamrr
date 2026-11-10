use crate::shared::ByteRange;
use chrono::{DateTime, Utc};
use futures::{Stream, future::ready, stream::once};
use std::time::Duration;
use tokio_util::bytes::Bytes;
use url::Url;

pub mod har;
pub mod http;

use crate::shared::Timed;
pub use har::HarSource;
pub use http::HttpSource;

pub trait Source: Clone + Send + Sync {
    type Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send;

    /// Wait until a playlist that was loaded at `playlist_time` should be refreshed.
    fn wait_for_refresh(
        &mut self,
        playlist_time: DateTime<Utc>,
        target_duration: Duration,
    ) -> impl Future<Output = ()> + Send;

    /// Request the playlist at the given URL as it was at the current time.
    /// Returns `None` if the source has no (more) versions of this playlist.
    fn request_playlist(
        &self,
        url: &Url,
    ) -> impl Future<Output = Result<Option<Timed<String>>, Self::Error>> + Send;

    /// Whether the resource at the given URL and byte range is available.
    #[allow(unused_variables)]
    fn is_available(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = bool> + Send {
        ready(true)
    }

    /// Request the resource at the given URL.
    fn request(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = Result<Timed<Bytes>, Self::Error>> + Send;

    /// Request the resource at the given URL as a stream.
    fn request_stream(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = WithSize<impl Stream<Item = Result<Bytes, Self::Error>> + Send>> + Send
    {
        async move {
            let bytes = self.request(url, byte_range).await.map(|bytes| bytes.value);
            let size = bytes.as_ref().ok().map(|bytes| bytes.len() as u64);
            WithSize {
                size,
                contents: once(ready(bytes)),
            }
        }
    }
}

/// A resource's contents along with its total size (if known).
pub struct WithSize<S> {
    pub size: Option<u64>,
    pub contents: S,
}
