use base64::prelude::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::io::{Error, Result};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Har {
    pub(crate) log: Log,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Log {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Entry {
    pub started_date_time: DateTime<Utc>,
    pub request: Request,
    pub response: Response,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Request {
    #[serde(default)]
    pub method: String,
    pub url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Response {
    #[serde(default)]
    pub status: i64,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub content: Content,
    #[serde(default, rename = "redirectURL")]
    pub redirect_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Content {
    #[serde(default = "default_size")]
    pub size: i64,
    pub mime_type: Option<String>,
    // Parse as raw value, to avoid keeping a ton of strings in memory.
    text: Option<Box<RawValue>>,
    pub encoding: Option<String>,
}

impl Content {
    pub(crate) fn text(&self) -> Result<String> {
        let Some(raw_text) = self.text.as_ref() else {
            // Response has no content.
            return Ok(String::new());
        };
        // Parse back into a string.
        let text = serde_json::from_str::<String>(raw_text.as_ref().get())
            .map_err(|_| Error::other("invalid content"))?;
        Ok(text)
    }

    pub(crate) fn as_bytes(&self) -> Result<Vec<u8>> {
        let text = self.text()?;
        // Decode as base64 (if needed).
        match self.encoding.as_ref() {
            Some(encoding) if encoding == "base64" => BASE64_STANDARD
                .decode(text.as_bytes())
                .map_err(|_| Error::other("invalid base64")),
            _ => Ok(text.into_bytes()),
        }
    }
}

fn default_size() -> i64 {
    -1
}
