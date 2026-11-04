use crate::import::Har;
use crate::import::har::Entry;
use crate::record::Source;
use crate::shared::{ByteRange, Timed, normalize_url};
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::bytes::Bytes;
use url::Url;

struct SharedHar {
    har: Har,
    /// GET entry indices per normalized URL, sorted by `startedDateTime`.
    index: HashMap<String, Vec<usize>>,
}

impl SharedHar {
    fn entries<'a>(&'a self, url: &Url) -> impl Iterator<Item = &'a Entry> {
        self.index
            .get(normalize_url(url))
            .into_iter()
            .flatten()
            .map(|&i| &self.har.log.entries[i])
    }
}

#[derive(Clone)]
pub struct HarSource {
    har: Arc<SharedHar>,
    time: DateTime<Utc>,
}


impl HarSource {
    pub fn new(har: Har, start_time: DateTime<Utc>) -> Self {
        let mut index: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, entry) in har.log.entries.iter().enumerate() {
            // Skip non-GET entries such as CORS preflight requests.
            if !entry.request.method.eq_ignore_ascii_case("GET") {
                continue;
            }
            let Ok(url) = Url::parse(&entry.request.url) else {
                continue;
            };
            index
                .entry(normalize_url(&url).to_string())
                .or_default()
                .push(i);
        }
        for indices in index.values_mut() {
            indices.sort_by_key(|&i| har.log.entries[i].started_date_time);
        }
        Self {
            har: Arc::new(SharedHar { har, index }),
            time: start_time,
        }
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
        let entry = self
            .har
            .entries(url)
            .find(|entry| entry.started_date_time >= self.time)
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
