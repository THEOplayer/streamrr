use super::Source;
use crate::shared::{ByteRange, Timed};
use chrono::Utc;
use futures::{Stream, TryStreamExt, future::ready, stream::once};
use reqwest::{Client, RequestBuilder, Response};
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

        println!(
            "Download: {url} {}",
            range_header.as_ref().unwrap_or(&String::new())
        );

        let mut request = self.client.get(url.clone());
        if let Some(range) = range_header {
            request = request.header(reqwest::header::RANGE, range);
        }
        request
    }
}

impl Source for HttpSource {
    type Error = reqwest::Error;

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

    async fn request_string(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> Result<Timed<String>, Self::Error> {
        let request = self.build_request(url, byte_range);
        let response = request.send().await?;
        let time = Utc::now();
        let text = response.text().await?;
        Ok(Timed { value: text, time })
    }

    async fn request_stream(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> impl Stream<Item = Result<Bytes, Self::Error>> {
        let request = self.build_request(url, byte_range);
        let response = request.send().await;
        once(ready(response))
            .map_ok(Response::bytes_stream)
            .try_flatten()
    }
}
