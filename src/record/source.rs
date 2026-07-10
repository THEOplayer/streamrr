use crate::shared::ByteRange;
use chrono::{DateTime, Utc};
use futures::{Stream, future::ready, stream::once};
use tokio_util::bytes::Bytes;
use url::Url;

pub mod http;

pub use http::HttpSource;

pub trait Source: Clone + Send + Sync {
    type Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send;

    /// Set the simulated time for subsequent requests.
    fn set_request_time(&mut self, _time: DateTime<Utc>) {}

    /// Request the resource at the given URL,
    /// as it was at the time set by `set_request_time`.
    fn request(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = Result<Bytes, Self::Error>> + Send;

    /// Request the resource at the given URL as a string,
    /// as it was at the time set by `set_request_time`.
    fn request_string(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = Result<String, Self::Error>> + Send {
        async move {
            let bytes = self.request(url, byte_range).await?;
            Ok(String::from_utf8_lossy(&bytes).to_string())
        }
    }

    /// Request the resource at the given URL as a stream,
    /// as it was at the time set by `set_request_time`.
    fn request_stream(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = impl Stream<Item = Result<Bytes, Self::Error>> + Send> + Send {
        async move {
            let bytes = self.request(url, byte_range).await;
            once(ready(bytes))
        }
    }
}
