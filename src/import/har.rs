pub use har::v1_3::*;
use serde::{Deserialize, Serialize};

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
