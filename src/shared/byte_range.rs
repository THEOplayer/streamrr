use std::fmt::{Display, Formatter};
use std::str::FromStr;

use anyhow::Context;

#[derive(Debug, Copy, Clone)]
pub struct ByteRange {
    pub length: u64,
    pub offset: u64,
}

impl ByteRange {
    pub fn from_m3u8(range: &m3u8_rs::ByteRange, default_offset: u64) -> Self {
        Self {
            length: range.length,
            offset: range.offset.unwrap_or(default_offset),
        }
    }
}

impl From<ByteRange> for m3u8_rs::ByteRange {
    fn from(value: ByteRange) -> Self {
        Self {
            length: value.length,
            offset: Some(value.offset),
        }
    }
}

impl FromStr for ByteRange {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self, Self::Err> {
        let (length, offset) = s.split_once('@').context("Missing offset in byte range")?;
        Ok(Self {
            length: length.parse().context("Invalid length")?,
            offset: offset.parse().context("Invalid offset")?,
        })
    }
}

impl Display for ByteRange {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.length, self.offset)
    }
}
