use crate::import::Har;
use crate::record::Source;
use crate::shared::{ByteRange, Timed};
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use tokio_util::bytes::Bytes;
use url::Url;

#[derive(Clone)]
pub struct HarSource {
    har: Har,
    time: DateTime<Utc>,
}

impl HarSource {
    pub fn new(har: Har, time: DateTime<Utc>) -> Self {
        Self { har, time }
    }
}

impl Source for HarSource {
    type Error = anyhow::Error;

    async fn advance_to_time(&mut self, time: DateTime<Utc>) {
        self.time = time;
    }

    async fn request(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
    ) -> Result<Timed<Bytes>, Self::Error> {
        let entries = &self.har.log.entries;
        let entry = entries
            .iter()
            .filter(|entry| entry.started_date_time >= self.time)
            .filter(|entry| entry.request.url == url.as_str())
            .min_by_key(|entry| entry.started_date_time)
            .ok_or_else(|| anyhow!("Not found: {url}"))?;
        let mut content = entry
            .response
            .content
            .as_bytes()
            .map_err(|_| anyhow!("Invalid data for: {url}"))?;
        if let Some(byte_range) = byte_range {
            content.drain(0..(byte_range.offset as usize));
            content.truncate(byte_range.length as usize);
        }
        Ok(Timed {
            value: Bytes::from(content),
            time: entry.started_date_time,
        })
    }
}
