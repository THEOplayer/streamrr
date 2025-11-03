pub use har::v1_3::*;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "version")]
pub enum Spec {
    /// Version 1.2 of the HAR specification.
    ///
    /// Refer to the official
    /// [specification](https://w3c.github.io/web-performance/specs/HAR/Overview.html)
    /// for more information.
    #[allow(non_camel_case_types)]
    #[serde(rename = "1.2")]
    V1_2(Log),

    // Version 1.3 of the HAR specification.
    //
    // Refer to the draft
    // [specification](https://github.com/ahmadnassri/har-spec/blob/master/versions/1.3.md)
    // for more information.
    #[allow(non_camel_case_types)]
    #[serde(rename = "1.3")]
    V1_3(Log),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Har {
    pub log: Spec,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
pub struct Log {
    pub creator: Creator,
    pub browser: Option<Creator>,
    pub pages: Option<Vec<Pages>>,
    pub entries: Vec<Entries>,
    pub comment: Option<String>,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
pub struct Entries {
    pub pageref: Option<String>,
    #[serde(rename = "startedDateTime")]
    pub started_date_time: String,
    pub time: f64,
    pub request: Request,
    pub response: Response,
    pub cache: Cache,
    pub timings: Timings,
    #[serde(rename = "serverIPAddress")]
    pub server_ip_address: Option<String>,
    pub connection: Option<String>,
    pub comment: Option<String>,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
pub struct Response {
    pub status: i64,
    #[serde(rename = "statusText")]
    pub status_text: String,
    #[serde(rename = "httpVersion")]
    pub http_version: String,
    pub cookies: Vec<Cookies>,
    pub headers: Vec<Headers>,
    pub content: Content,
    #[serde(rename = "redirectURL")]
    pub redirect_url: Option<String>,
    #[serde(rename = "headersSize", default = "default_isize")]
    pub headers_size: i64,
    #[serde(rename = "bodySize", default = "default_isize")]
    pub body_size: i64,
    pub comment: Option<String>,
    #[serde(rename = "headersCompression")]
    pub headers_compression: Option<i64>,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
pub struct Content {
    #[serde(default = "default_isize")]
    pub size: i64,
    pub compression: Option<i64>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub encoding: Option<String>,
    pub comment: Option<String>,
}

fn default_isize() -> i64 {
    -1
}
