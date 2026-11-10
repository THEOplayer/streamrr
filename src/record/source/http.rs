use super::{Source, WithSize};
use crate::shared::{ByteRange, Timed};
use chrono::{DateTime, Utc};
use futures::{Stream, TryStreamExt, future::ready, stream::once};
use reqwest::{Client, RequestBuilder, Response};
use std::time::Duration;
use tokio::time::sleep;
use tokio_util::bytes::Bytes;
use url::Url;

#[derive(Clone)]
pub struct HttpSource {
    client: Client,
}

impl HttpSource {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    fn build_request(&self, url: &Url, byte_range: Option<ByteRange>) -> RequestBuilder {
        let range_header = byte_range.map(|byte_range| {
            let start = byte_range.offset;
            let end = start + byte_range.length - 1; // end byte for a range request is inclusive!
            format!("bytes={start}-{end}")
        });

        let mut request = self.client.get(url.clone());
        if let Some(range) = range_header {
            request = request.header(reqwest::header::RANGE, range);
        }
        request
    }
}

impl Source for HttpSource {
    type Error = reqwest::Error;

    async fn wait_for_refresh(&mut self, playlist_time: DateTime<Utc>, target_duration: Duration) {
        let next_refresh_time = playlist_time + target_duration;
        let duration = (next_refresh_time - Utc::now())
            .to_std()
            .unwrap_or_default();
        sleep(duration).await
    }

    async fn request_playlist(&self, url: &Url) -> Result<Option<Timed<String>>, Self::Error> {
        let request = self.build_request(url, None);
        let response = request.send().await?;
        let time = Utc::now();
        let text = response.text().await?;
        Ok(Some(Timed { value: text, time }))
    }

    async fn request(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> Result<Timed<Bytes>, Self::Error> {
        let request = self.build_request(url, byte_range);
        let response = request.send().await?;
        let time = Utc::now();
        let bytes = response.bytes().await?;
        Ok(Timed { value: bytes, time })
    }

    async fn request_stream(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> WithSize<impl Stream<Item = Result<Bytes, Self::Error>>> {
        let request = self.build_request(url, byte_range);
        let response = request.send().await;
        let size = response.as_ref().ok().and_then(Response::content_length);
        let contents = once(ready(response))
            .map_ok(Response::bytes_stream)
            .try_flatten();
        WithSize { size, contents }
    }
}
