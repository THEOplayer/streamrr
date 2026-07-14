use crate::import::Har;
use crate::record::Source;
use crate::shared::{ByteRange, Timed};
use chrono::{DateTime, Utc};
use tokio_util::bytes::Bytes;
use url::Url;

#[derive(Clone)]
pub struct HarSource {
    har: Har,
}

impl HarSource {
    pub fn new(har: Har) -> Self {
        Self { har }
    }
}

impl Source for HarSource {
    type Error = std::io::Error;

    async fn advance_to_time(&mut self, _time: DateTime<Utc>) {
        todo!()
    }

    async fn request(
        &self,
        _url: &Url,
        _byte_range: Option<ByteRange>,
    ) -> Result<Timed<Bytes>, Self::Error> {
        todo!()
    }
}
