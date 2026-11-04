use crate::import::Har;
use crate::import::har::{Entry, response_range};
use crate::record::Source;
use crate::shared::{ByteRange, Timed, normalize_url};
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use tokio_util::bytes::Bytes;
use url::Url;

const MAX_REDIRECTS: usize = 10;

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

    /// Find the earliest usable response for `url` and `byte_range`,
    /// following redirects. Does not decode the response body.
    ///
    /// If `min_time` is set, only entries with a `startedDateTime`
    /// at/after (`inclusive`) or after it are considered, and redirect
    /// targets must be at/after the redirecting entry's time.
    fn lookup(
        &self,
        url: &Url,
        byte_range: Option<ByteRange>,
        min_time: Option<DateTime<Utc>>,
        inclusive: bool,
        redirects: usize,
    ) -> Option<Found<'_>> {
        if redirects > MAX_REDIRECTS {
            return None;
        }
        for entry in self.entries(url) {
            if let Some(min_time) = min_time {
                let in_time = if inclusive {
                    entry.started_date_time >= min_time
                } else {
                    entry.started_date_time > min_time
                };
                if !in_time {
                    continue;
                }
            }
            let response = &entry.response;
            if (300..=399).contains(&response.status) && !response.redirect_url.is_empty() {
                // Follow the redirect. The target must be captured at/after
                // the redirecting entry if the lookup is time-constrained.
                let Ok(target) = url.join(&response.redirect_url) else {
                    continue;
                };
                let min_time = min_time.map(|_| entry.started_date_time);
                if let Some(found) = self.lookup(&target, byte_range, min_time, true, redirects + 1)
                {
                    return Some(Found {
                        time: entry.started_date_time,
                        ..found
                    });
                }
                continue;
            }
            if let Some((body_len, slice)) = response_range(entry)
                .and_then(|body| body.covers(byte_range).map(|slice| (body.len(), slice)))
            {
                return Some(Found {
                    time: entry.started_date_time,
                    entry,
                    body_len,
                    slice,
                });
            }
        }
        None
    }
}

/// A response found in the HAR index.
struct Found<'a> {
    /// `startedDateTime` of the first entry in the redirect chain.
    time: DateTime<Utc>,
    /// The final entry that has the body.
    entry: &'a Entry,
    /// The expected length of the decoded body.
    body_len: u64,
    /// The slice of the decoded body satisfying the request.
    slice: Range<usize>,
}

impl Found<'_> {
    /// Decode the response body and slice out the requested range.
    fn decode(&self, url: &Url) -> anyhow::Result<Bytes> {
        let bytes = self
            .entry
            .response
            .content
            .as_bytes()
            .map_err(|_| anyhow!("Invalid data for: {url}"))?;
        if bytes.len() as u64 != self.body_len {
            return Err(anyhow!("Invalid data for: {url}"));
        }
        Ok(Bytes::from(bytes).slice(self.slice.clone()))
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
        let found = self
            .har
            .lookup(url, byte_range, Some(self.time), true, 0)
            .ok_or_else(|| anyhow!("Not found: {url}"))?;
        let bytes = found.decode(url)?;
        Ok(Timed {
            value: bytes,
            time: found.time,
        })
    }
}
