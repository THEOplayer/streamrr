use crate::shared::ByteRange;
use chrono::{DateTime, Utc};
use futures::{Stream, future::ready, stream::once};
use tokio_util::bytes::Bytes;
use url::Url;

pub mod har;
pub mod http;

use crate::shared::Timed;
pub use har::HarSource;
pub use http::HttpSource;

pub trait Source: Clone + Send + Sync {
    type Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send;

    /// Advance the current time for subsequent requests.
    #[allow(unused_variables)]
    fn advance_to_time(&mut self, time: DateTime<Utc>) -> impl Future<Output = ()> + Send {
        ready(())
    }

    /// Request the resource at the given URL
    /// as it was at the time set by `advance_to_time`.
    fn request(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = Result<Timed<Bytes>, Self::Error>> + Send;

    /// Request the resource at the given URL as a string,
    /// as it was at the time set by `advance_to_time`.
    fn request_string(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = Result<Timed<String>, Self::Error>> + Send {
        async move {
            let bytes = self.request(url, byte_range).await?;
            Ok(bytes.map(|bytes| String::from_utf8_lossy(&bytes).to_string()))
        }
    }

    /// Request the resource at the given URL as a stream.
    fn request_stream(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Future<Output = impl Stream<Item = Result<Bytes, Self::Error>> + Send> + Send {
        async move {
            let bytes = self.request(url, byte_range).await.map(|bytes| bytes.value);
            once(ready(bytes))
        }
    }
}
