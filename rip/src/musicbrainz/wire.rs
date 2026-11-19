// SPDX-License-Identifier: AGPL-3.0-or-later
//! MusicBrainz JSON (web service v2) and its mapping onto the shared
//! metadata types.

use serde::Deserialize;

use super::{MusicBrainzError, ReleaseCandidate, Result};
use crate::model::{Isrc, Mbid, ReleaseMetadata, TrackMetadata, TrackNumber};

/// `discid/{id}`: exact (`id` present) or fuzzy TOC matches.
#[derive(Debug, Deserialize)]
pub(super) struct DiscAnswer {
    #[serde(default)]
    pub(super) releases: Vec<Release>,
}

/// `release?query=`.
#[derive(Debug, Deserialize)]
pub(super) struct SearchAnswer {
    #[serde(default)]
    pub(super) releases: Vec<Release>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct Release {
    id: String,
    title: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    barcode: Option<String>,
    #[serde(default)]
    artist_credit: Vec<Credit>,
    #[serde(default)]
    cover_art_archive: Option<CoverArtArchive>,
    #[serde(default)]
    label_info: Vec<LabelInfo>,
    #[serde(default)]
    release_group: Option<Group>,
    #[serde(default)]
    media: Vec<Medium>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Credit {
    name: String,
    #[serde(default)]
    joinphrase: String,
    artist: Option<Artist>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Artist {
    id: String,
    #[serde(default)]
    sort_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CoverArtArchive {
    #[serde(default)]
    front: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct LabelInfo {
    #[serde(default)]
    catalog_number: Option<String>,
    #[serde(default)]
    label: Option<Label>,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Group {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Medium {
    #[serde(default)]
    position: Option<u32>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    track_count: Option<u32>,
    #[serde(default)]
    discs: Vec<Disc>,
    #[serde(default)]
    tracks: Vec<Track>,
}

#[derive(Debug, Deserialize)]
struct Disc {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Track {
    id: String,
    #[serde(default)]
    position: Option<u32>,
    title: String,
    #[serde(default)]
    artist_credit: Vec<Credit>,
    #[serde(default)]
    recording: Option<Recording>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Recording {
    id: String,
    #[serde(default)]
    artist_credit: Vec<Credit>,
    #[serde(default)]
    isrcs: Vec<String>,
}

/// "Artist A & Artist B" from the credit's names and join phrases.
fn credit_text(credit: &[Credit]) -> String {
    credit
        .iter()
        .map(|part| format!("{}{}", part.name, part.joinphrase))
        .collect()
}

fn credit_mbids(credit: &[Credit]) -> Vec<Mbid> {
    credit
        .iter()
        .filter_map(|part| part.artist.as_ref())
        .filter_map(|artist| Mbid::new(&artist.id).ok())
        .collect()
}

fn non_empty(text: &Option<String>) -> Option<String> {
    text.as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "[none]")
        .map(str::to_owned)
}

fn small(number: impl TryInto<u8>) -> u8 {
    number.try_into().unwrap_or(u8::MAX)
}

impl Medium {
    /// The medium's 1-based position (MusicBrainz omits it in searches).
    fn position_or(&self, index: usize) -> u32 {
        self.position
            .unwrap_or_else(|| u32::try_from(index).map_or(u32::MAX, |i| i.saturating_add(1)))
    }

    fn track_count(&self) -> u32 {
        self.track_count
            .unwrap_or_else(|| u32::try_from(self.tracks.len()).unwrap_or(u32::MAX))
    }
}

impl Release {
    /// The candidate for this release: the medium carrying `disc_id`, or
    /// else the first with `audio_tracks` tracks, or else the first.
    pub(super) fn candidate(
        &self,
        disc_id: Option<&str>,
        audio_tracks: usize,
    ) -> Option<ReleaseCandidate> {
        let by_disc = disc_id.and_then(|disc_id| {
            self.media
                .iter()
                .position(|medium| medium.discs.iter().any(|disc| disc.id == disc_id))
        });
        let by_count = || {
            self.media.iter().position(|medium| {
                usize::try_from(medium.track_count()).is_ok_and(|count| count == audio_tracks)
            })
        };
        let index = by_disc.or_else(by_count).unwrap_or(0);
        self.candidate_for(index)
    }

    /// One candidate per medium (search results).
    pub(super) fn candidates_per_medium(&self) -> Vec<ReleaseCandidate> {
        if self.media.len() <= 1 {
            return self.candidate_for(0).into_iter().collect();
        }
        (0..self.media.len())
            .filter_map(|index| self.candidate_for(index))
            .collect()
    }

    fn candidate_for(&self, index: usize) -> Option<ReleaseCandidate> {
        let release_mbid = Mbid::new(&self.id).ok()?;
        let medium = self.media.get(index);
        let format = medium
            .and_then(|medium| non_empty(&medium.format))
            .unwrap_or_else(|| "Disc".to_owned());
        let position = medium.map_or(1, |medium| medium.position_or(index));
        let label = if self.media.len() > 1 {
            format!("{format} {position} of {}", self.media.len())
        } else {
            format
        };
        Some(ReleaseCandidate {
            release_mbid,
            title: self.title.clone(),
            artist: credit_text(&self.artist_credit),
            date: non_empty(&self.date),
            country: non_empty(&self.country),
            barcode: non_empty(&self.barcode),
            medium: label,
            medium_position: small(position),
            track_count: small(medium.map_or(0, Medium::track_count)),
            has_front_cover: self
                .cover_art_archive
                .as_ref()
                .is_some_and(|archive| archive.front),
        })
    }

    /// Metadata for the medium at `position` (1-based).
    pub(super) fn metadata(&self, position: u8) -> Result<ReleaseMetadata> {
        let medium = self
            .media
            .iter()
            .enumerate()
            .find(|(index, medium)| medium.position_or(*index) == u32::from(position))
            .map(|(_, medium)| medium)
            .ok_or_else(|| {
                MusicBrainzError::NotFound(format!("disc {position} of this release"))
            })?;
        let date = non_empty(&self.date);
        let year = date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse().ok());
        let label_name = self
            .label_info
            .iter()
            .filter_map(|info| info.label.as_ref())
            .map(|label| label.name.clone())
            .find(|name| !name.is_empty() && name != "[no label]");
        let catalog_number = self
            .label_info
            .iter()
            .find_map(|info| non_empty(&info.catalog_number));
        let artist_sort = match self.artist_credit.as_slice() {
            [only] => only
                .artist
                .as_ref()
                .and_then(|artist| artist.sort_name.clone()),
            _ => None,
        };
        let tracks = medium
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| track.metadata(index))
            .collect();
        Ok(ReleaseMetadata {
            title: self.title.clone(),
            artist: credit_text(&self.artist_credit),
            artist_sort,
            release_mbid: Mbid::new(&self.id).ok(),
            release_group_mbid: self
                .release_group
                .as_ref()
                .and_then(|group| Mbid::new(&group.id).ok()),
            artist_mbids: credit_mbids(&self.artist_credit),
            disc_id: None,
            year,
            date,
            label: label_name,
            catalog_number,
            barcode: non_empty(&self.barcode),
            disc_number: position,
            disc_total: small(self.media.len()),
            tracks,
            cover: None,
        })
    }
}

impl Track {
    fn metadata(&self, index: usize) -> TrackMetadata {
        let position = self
            .position
            .unwrap_or_else(|| u32::try_from(index).map_or(u32::MAX, |i| i.saturating_add(1)));
        let credit = match (&self.artist_credit, &self.recording) {
            (credit, _) if !credit.is_empty() => credit.as_slice(),
            (_, Some(recording)) => recording.artist_credit.as_slice(),
            (credit, None) => credit.as_slice(),
        };
        TrackMetadata {
            number: u8::try_from(position)
                .ok()
                .and_then(|number| TrackNumber::new(number).ok()),
            title: self.title.clone(),
            artist: credit_text(credit),
            recording_mbid: self
                .recording
                .as_ref()
                .and_then(|recording| Mbid::new(&recording.id).ok()),
            track_mbid: Mbid::new(&self.id).ok(),
            artist_mbids: credit_mbids(credit),
            isrc: self
                .recording
                .as_ref()
                .and_then(|recording| recording.isrcs.first())
                .and_then(|isrc| Isrc::new(isrc).ok()),
        }
    }
}
