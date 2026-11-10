use crate::shared::ByteRange;
use base64::prelude::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::io::{Error, Result};
use std::ops::Range;

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
    /// Whether the response content was dropped by the capturer.
    pub(crate) fn is_missing(&self) -> bool {
        self.text.is_none() && self.size != 0
    }

    pub(crate) fn text(&self) -> Result<Cow<'_, str>> {
        #[derive(Deserialize)]
        struct Text<'a>(#[serde(borrow)] Cow<'a, str>);
        let Some(raw_text) = self.text.as_ref() else {
            // Response has no content.
            return Ok(Cow::Borrowed(""));
        };
        // Parse back into a string, borrowing it unless it contains escapes.
        let Text(text) =
            serde_json::from_str(raw_text.get()).map_err(|_| Error::other("invalid content"))?;
        Ok(text)
    }

    pub(crate) fn as_bytes(&self) -> Result<Vec<u8>> {
        let text = self.text()?;
        // Decode as base64 (if needed).
        match self.encoding.as_ref() {
            Some(encoding) if encoding == "base64" => BASE64_STANDARD
                .decode(text.as_bytes())
                .map_err(|_| Error::other("invalid base64")),
            _ => Ok(text.into_owned().into_bytes()),
        }
    }

    /// The length of the decoded body, computed without decoding it
    /// unless the raw text contains JSON escapes. Returns `None` if
    /// the body is invalid.
    pub(crate) fn decoded_len(&self) -> Option<u64> {
        let Some(raw_text) = self.text.as_ref() else {
            // Response has no content.
            return Some(0);
        };
        // The raw value is a JSON string literal; strip the quotes
        // and work on bytes (the content may not be UTF-8).
        let inner = raw_text
            .get()
            .as_bytes()
            .strip_prefix(b"\"")?
            .strip_suffix(b"\"")?;
        // A JSON escape requires decoding the whole string.
        if inner.contains(&b'\\') {
            return self.as_bytes().ok().map(|bytes| bytes.len() as u64);
        }
        if matches!(self.encoding.as_deref(), Some("base64")) {
            // Canonical base64 has a multiple-of-4 length and at most
            // two `=` padding characters at the end.
            if inner.len() % 4 != 0 {
                return None;
            }
            let padding = inner.iter().rev().take_while(|&&b| b == b'=').count();
            if padding > 2 {
                return None;
            }
            Some((inner.len() / 4 * 3 - padding) as u64)
        } else {
            Some(inner.len() as u64)
        }
    }

    /// The first `len` bytes of the decoded body (fewer if the body is
    /// shorter), without decoding the complete body. Returns `None`
    /// if the body is invalid.
    pub(crate) fn prefix(&self, len: usize) -> Option<Vec<u8>> {
        let Some(raw_text) = self.text.as_ref() else {
            // Response has no content.
            return Some(Vec::new());
        };
        // The raw value is a JSON string literal; strip the opening quote
        // and work on bytes (the content may not be UTF-8).
        let raw = raw_text.get().as_bytes().strip_prefix(b"\"")?;
        let base64 = matches!(self.encoding.as_deref(), Some("base64"));
        // The number of raw bytes needed to decode `len` bytes.
        // `encoded_len` with padding rounds up to whole 4-char groups
        // (e.g. 10 -> 16); without padding it would give 14, which
        // `BASE64_STANDARD` rejects.
        let needed = if base64 {
            base64::encoded_len(len, true)?
        } else {
            len
        };
        let chunk = match raw
            .iter()
            .take(needed)
            .position(|&byte| byte == b'\\' || byte == b'"')
        {
            // A JSON escape requires decoding the whole string.
            Some(i) if raw[i] == b'\\' => {
                let mut bytes = self.as_bytes().ok()?;
                bytes.truncate(len);
                return Some(bytes);
            }
            // The closing quote: the chunk is the complete string.
            Some(i) => &raw[..i],
            None => &raw[..needed.min(raw.len())],
        };
        let mut bytes = if base64 {
            BASE64_STANDARD.decode(chunk).ok()?
        } else {
            chunk.to_vec()
        };
        bytes.truncate(len);
        Some(bytes)
    }
}

fn default_size() -> i64 {
    -1
}

/// The part of a resource that a response body covers.
pub(crate) struct BodyRange {
    /// Covered byte range within the resource (half-open).
    pub range: Range<u64>,
    /// Whether the body is the complete resource.
    pub complete: bool,
}

impl BodyRange {
    /// The slice of the body (relative to the body start) that satisfies the
    /// request, or `None` if not covered. A `None` byte range requires
    /// the complete resource.
    pub(crate) fn covers(&self, byte_range: Option<ByteRange>) -> Option<Range<usize>> {
        match byte_range {
            Some(byte_range) => {
                let request_end = byte_range.offset.checked_add(byte_range.length)?;
                if byte_range.offset < self.range.start || request_end > self.range.end {
                    return None;
                }
                let start = (byte_range.offset - self.range.start) as usize;
                Some(start..start + byte_range.length as usize)
            }
            None => self.complete.then(|| 0..self.len() as usize),
        }
    }

    /// The length of the covered range.
    pub(crate) fn len(&self) -> u64 {
        self.range.end - self.range.start
    }
}

/// The covered range of a usable response, without decoding the body
/// (unless the raw text contains JSON escapes).
pub(crate) fn response_range(entry: &Entry) -> Option<BodyRange> {
    let response = &entry.response;
    if !(200..=299).contains(&response.status) {
        return None;
    }
    // A missing body means that DevTools dropped the response content.
    if response.content.is_missing() {
        return None;
    }
    let len = response.content.decoded_len()?;
    if response.status == 206 {
        let content_range = response
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("content-range"))?;
        let (start, end, total) = parse_content_range(&content_range.value)?;
        // Make `end` exclusive.
        let end = end.checked_add(1)?;
        if end <= start || len != end - start {
            return None;
        }
        let complete = start == 0 && total == Some(end);
        Some(BodyRange {
            range: start..end,
            complete,
        })
    } else {
        Some(BodyRange {
            range: 0..len,
            complete: true,
        })
    }
}

/// Parse a `Content-Range` header value (`bytes <start>-<end>/<total|*>`)
/// into `(start, end, total)`.
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let (unit, range) = value.trim().split_once(char::is_whitespace)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = range.trim().split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    let (start, end) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn byte_range(offset: u64, length: u64) -> Option<ByteRange> {
        Some(ByteRange { length, offset })
    }

    fn parse_content(json: &str) -> Content {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn text_borrows_without_escapes() {
        let content = parse_content(r#"{"size":4,"text":"abcd"}"#);
        assert!(matches!(content.text().unwrap(), Cow::Borrowed("abcd")));
        let content = parse_content(r#"{"text":"ab\/cd"}"#);
        assert!(matches!(content.text().unwrap(), Cow::Owned(ref s) if s == "ab/cd"));
    }

    #[test]
    fn prefix_plain_text() {
        // Longer than `len`.
        let content = parse_content(r##"{"text": "#EXTM3U playlist body"}"##);
        assert_eq!(
            content.prefix(10).unwrap(),
            &content.as_bytes().unwrap()[..10]
        );
        // Shorter than `len`.
        let content = parse_content(r#"{"text": "abc"}"#);
        assert_eq!(content.prefix(10).unwrap(), b"abc");
    }

    #[test]
    fn prefix_bom() {
        // A raw UTF-8 BOM in the JSON text.
        let content = parse_content("{\"text\": \"\u{feff}#EXTM3U body\"}");
        assert_eq!(content.prefix(10).unwrap(), "\u{feff}#EXTM3U".as_bytes());
        assert_eq!(
            content.prefix(10).unwrap(),
            &content.as_bytes().unwrap()[..10]
        );
        // An escaped BOM requires decoding the whole string.
        let content = parse_content(r##"{"text": "\ufeff#EXTM3U body"}"##);
        assert_eq!(content.prefix(10).unwrap(), "\u{feff}#EXTM3U".as_bytes());
        assert_eq!(
            content.prefix(10).unwrap(),
            &content.as_bytes().unwrap()[..10]
        );
    }

    #[test]
    fn prefix_multibyte_utf8() {
        // `len` lands in the middle of a multi-byte character.
        let content = parse_content("{\"text\": \"äääää\"}");
        assert_eq!(content.prefix(5).unwrap().len(), 5);
        assert_eq!(
            content.prefix(5).unwrap(),
            &content.as_bytes().unwrap()[..5]
        );
    }

    #[test]
    fn prefix_base64() {
        // Longer than the needed chunk (no padding in the chunk).
        let content = parse_content(
            r#"{"text": "MDEyMzQ1Njc4OUFCQ0RFRkdISUowMTIzNDU2Nzg5", "encoding": "base64"}"#,
        );
        assert_eq!(
            content.prefix(10).unwrap(),
            &content.as_bytes().unwrap()[..10]
        );
        // Shorter than the chunk, with `=` padding.
        let content = parse_content(r#"{"text": "aGk=", "encoding": "base64"}"#);
        assert_eq!(content.prefix(10).unwrap(), b"hi");
    }

    #[test]
    fn prefix_base64_escaped() {
        // An escaped `\/` requires decoding the whole string.
        let content = parse_content(r#"{"text": "aGk\/aGk=", "encoding": "base64"}"#);
        assert_eq!(
            content.prefix(4).unwrap(),
            &content.as_bytes().unwrap()[..4]
        );
    }

    #[test]
    fn prefix_invalid() {
        let content = parse_content(r#"{"text": "!!!!", "encoding": "base64"}"#);
        assert_eq!(content.prefix(10), None);
    }

    #[test]
    fn prefix_missing() {
        let content = parse_content(r#"{"size": 5}"#);
        assert_eq!(content.prefix(10).unwrap(), Vec::<u8>::new());
    }

    fn assert_decoded_len(content: &Content) {
        assert_eq!(
            content.decoded_len(),
            Some(content.as_bytes().unwrap().len() as u64)
        );
    }

    #[test]
    fn test_decoded_len() {
        // Plain text without escapes.
        assert_decoded_len(&parse_content(r#"{"text": "hello world"}"#));
        // Plain text with escapes (fallback path).
        assert_decoded_len(&parse_content(
            r#"{"text": "line1\nline2 \"quoted\" é \ufeff"}"#,
        ));
        // Plain text with raw multi-byte UTF-8.
        assert_decoded_len(&parse_content("{\"text\": \"äääää\"}"));
        // Base64 with 0, 1 and 2 `=` padding.
        assert_decoded_len(&parse_content(r#"{"text": "Zm9v", "encoding": "base64"}"#));
        assert_decoded_len(&parse_content(r#"{"text": "Zm8=", "encoding": "base64"}"#));
        assert_decoded_len(&parse_content(r#"{"text": "Zg==", "encoding": "base64"}"#));
        // Base64 containing an escaped `\/` (fallback path).
        assert_decoded_len(&parse_content(
            r#"{"text": "aGk\/aGk=", "encoding": "base64"}"#,
        ));
        // No content.
        let content = parse_content(r#"{"size": 5}"#);
        assert_eq!(content.decoded_len(), Some(0));
        // Invalid base64.
        let content = parse_content(r#"{"text": "aGk", "encoding": "base64"}"#);
        assert_eq!(content.decoded_len(), None);
        // More than two `=` padding characters.
        let content = parse_content(r#"{"text": "Y===", "encoding": "base64"}"#);
        assert_eq!(content.decoded_len(), None);
    }

    #[test]
    fn covers_complete_body() {
        let body = BodyRange {
            range: 0..10,
            complete: true,
        };
        // Inside range and exact bounds.
        assert_eq!(body.covers(byte_range(4, 2)), Some(4..6));
        assert_eq!(body.covers(byte_range(0, 10)), Some(0..10));
        assert_eq!(body.covers(byte_range(9, 1)), Some(9..10));
        // Out of range.
        assert_eq!(body.covers(byte_range(10, 1)), None);
        // No byte range requires the complete resource.
        assert_eq!(body.covers(None), Some(0..10));
    }

    #[test]
    fn covers_partial_body() {
        let body = BodyRange {
            range: 10..20,
            complete: false,
        };
        assert_eq!(body.covers(byte_range(10, 10)), Some(0..10));
        assert_eq!(body.covers(byte_range(15, 5)), Some(5..10));
        // Before start and past end.
        assert_eq!(body.covers(byte_range(9, 1)), None);
        assert_eq!(body.covers(byte_range(15, 10)), None);
        // Overflow.
        assert_eq!(body.covers(byte_range(u64::MAX - 5, 10)), None);
        // No byte range requires the complete resource.
        assert_eq!(body.covers(None), None);
    }

    #[test]
    fn covers_empty_body() {
        let body = BodyRange {
            range: 0..0,
            complete: true,
        };
        assert_eq!(body.covers(None), Some(0..0));
        assert_eq!(body.covers(byte_range(0, 1)), None);
    }

    #[test]
    fn test_parse_content_range() {
        assert_eq!(
            parse_content_range("bytes 0-99/200"),
            Some((0, 99, Some(200)))
        );
        assert_eq!(parse_content_range("bytes 10-19/*"), Some((10, 19, None)));
        assert_eq!(parse_content_range("items 0-9/10"), None);
        assert_eq!(parse_content_range("garbage"), None);
        assert_eq!(parse_content_range("bytes 0-9"), None);
        assert_eq!(parse_content_range("bytes x-y/z"), None);
    }
}
