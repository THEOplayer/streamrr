use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use clap::ValueEnum;
use indexmap::IndexMap;
use m3u8_rs::{AlternativeMedia, VariantStream};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Default)]
pub struct Recording {
    // Playlist file path is keyed by playlist name, then by UTC time
    playlists: IndexMap<String, BTreeMap<DateTime<Utc>, String>>,
}

impl Recording {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, time: DateTime<Utc>, playlist_name: &str, playlist_path: String) {
        let playlists = if let Some(playlists) = self.playlists.get_mut(playlist_name) {
            playlists
        } else {
            self.playlists.entry(playlist_name.to_string()).or_default()
        };
        playlists.insert(time, playlist_path);
    }

    pub fn earliest_time(&self) -> Option<&DateTime<Utc>> {
        let (time, _path) = self
            .playlists
            .values()
            .flat_map(|x| x.first_key_value())
            .min()?;
        Some(time)
    }

    pub fn earliest_time_for(&self, playlist_name: &str) -> Option<(&DateTime<Utc>, &str)> {
        let (time, path) = self.playlists.get(playlist_name)?.first_key_value()?;
        Some((time, path))
    }

    pub fn find_latest_before(
        &self,
        playlist_name: &str,
        time: DateTime<Utc>,
    ) -> Option<(&DateTime<Utc>, &str)> {
        let (time, path) = self
            .playlists
            .get(playlist_name)?
            .range(..time)
            .next_back()?;
        Some((time, path))
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum VariantSelect {
    /// Select the first variant stream.
    #[default]
    First,
    /// Select the lowest variant stream.
    Lowest,
    /// Select the highest variant stream.
    Highest,
    /// Select all variant streams.
    All,
}

#[derive(Debug, Copy, Clone)]
pub enum VariantSelectOptions {
    Named(VariantSelect),
    Bandwidth(u64),
}

impl Default for VariantSelectOptions {
    fn default() -> Self {
        VariantSelectOptions::Named(VariantSelect::default())
    }
}

impl VariantSelectOptions {
    pub(crate) fn filter_variants(self, variants: &[VariantStream]) -> &[VariantStream] {
        match self {
            VariantSelectOptions::Named(variant) => match variant {
                VariantSelect::First => Self::first_variant(variants),
                VariantSelect::Lowest => Self::lowest_variant(variants),
                VariantSelect::Highest => Self::highest_variant(variants),
                VariantSelect::All => variants,
            },
            VariantSelectOptions::Bandwidth(max_bandwidth) => {
                Self::variant_with_max_bandwidth(variants, max_bandwidth)
            }
        }
    }

    fn first_variant(variants: &[VariantStream]) -> &[VariantStream] {
        if variants.is_empty() {
            &[]
        } else {
            &variants[0..1]
        }
    }

    fn lowest_variant(variants: &[VariantStream]) -> &[VariantStream] {
        if let Some((idx, _)) = variants
            .iter()
            .enumerate()
            .min_by_key(|(_, variant)| variant.bandwidth)
        {
            &variants[idx..][..1]
        } else {
            &[]
        }
    }

    fn highest_variant(variants: &[VariantStream]) -> &[VariantStream] {
        if let Some((idx, _)) = variants
            .iter()
            .enumerate()
            .max_by_key(|(_, variant)| variant.bandwidth)
        {
            &variants[idx..][..1]
        } else {
            &[]
        }
    }

    fn variant_with_max_bandwidth(
        variants: &[VariantStream],
        max_bandwidth: u64,
    ) -> &[VariantStream] {
        let mut best_variant: Option<(usize, &VariantStream)> = None;
        for (idx, variant) in variants.iter().enumerate() {
            if variant.bandwidth > max_bandwidth {
                continue;
            }
            if let Some((_, best_variant)) = &best_variant
                && best_variant.bandwidth > variant.bandwidth
            {
                continue;
            }
            best_variant = Some((idx, variant));
        }
        if let Some((idx, _)) = best_variant {
            &variants[idx..][..1]
        } else {
            // Fall back to lowest variant
            Self::lowest_variant(variants)
        }
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum MediaSelect {
    /// Select the default rendition.
    #[default]
    Default,
    /// Select the first rendition.
    First,
    /// Select all renditions.
    All,
}

impl MediaSelect {
    pub(crate) fn filter_media(self, medias: &[AlternativeMedia]) -> &[AlternativeMedia] {
        match self {
            MediaSelect::Default => Self::default_media(medias),
            MediaSelect::First => Self::first_media(medias),
            MediaSelect::All => medias,
        }
    }

    fn default_media(medias: &[AlternativeMedia]) -> &[AlternativeMedia] {
        if let Some(index) = medias.iter().position(|media| media.default) {
            &medias[index..][..1]
        } else {
            &[]
        }
    }

    fn first_media(medias: &[AlternativeMedia]) -> &[AlternativeMedia] {
        if medias.is_empty() {
            &[]
        } else {
            &medias[0..1]
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct SerializedRecording {
    playlists: IndexMap<String, Vec<SerializedPlaylist>>,
}

#[derive(Serialize, Deserialize)]
pub struct SerializedPlaylist {
    time: i64,
    path: String,
}

impl Serialize for Recording {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let serialized = SerializedRecording {
            playlists: self
                .playlists
                .iter()
                .map(|(name, playlists)| {
                    let playlists = playlists
                        .iter()
                        .map(|(time, path)| SerializedPlaylist {
                            time: time.timestamp_millis(),
                            path: path.clone(),
                        })
                        .collect::<Vec<SerializedPlaylist>>();
                    (name.clone(), playlists)
                })
                .collect(),
        };
        serialized.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Recording {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let serialized = SerializedRecording::deserialize(deserializer)?;
        Ok(Recording {
            playlists: serialized
                .playlists
                .into_iter()
                .map(|(name, playlists)| {
                    let playlists = playlists
                        .into_iter()
                        .map(|SerializedPlaylist { time, path }| {
                            (Utc.timestamp_millis_opt(time).unwrap(), path)
                        })
                        .collect();
                    (name, playlists)
                })
                .collect(),
        })
    }
}
