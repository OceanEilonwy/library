// SPDX-License-Identifier: AGPL-3.0-or-later
//! Vorbis comments from MusicBrainz metadata.
//!
//! Field names follow the conventions MusicBrainz Picard and most players
//! use: `MUSICBRAINZ_TRACKID` is the *recording* id and
//! `MUSICBRAINZ_RELEASETRACKID` the track-on-this-release id.

use super::VorbisComment;
use crate::model::{ReleaseMetadata, TrackMetadata};

/// Builds the comments for one track: TITLE, ARTIST, ALBUM, ALBUMARTIST,
/// ALBUMARTISTSORT, DATE, TRACKNUMBER, TRACKTOTAL, DISCNUMBER, DISCTOTAL,
/// ISRC, BARCODE, LABEL, CATALOGNUMBER, the `MUSICBRAINZ_*` ids (ALBUMID,
/// RELEASEGROUPID, TRACKID, RELEASETRACKID, ARTISTID, ALBUMARTISTID,
/// DISCID; one comment per artist id), MEDIA=CD and
/// RIPPER=Library Rip <version>. Empty values are left out; a track
/// without its own artist credit gets the album artist.
pub fn vorbis_comments(release: &ReleaseMetadata, track: &TrackMetadata) -> Vec<VorbisComment> {
    let mut comments = Vec::with_capacity(24);
    let mut add = |name: &str, value: &str| {
        let value = value.trim();
        if !value.is_empty() {
            comments.push(VorbisComment {
                name: name.to_owned(),
                value: value.to_owned(),
            });
        }
    };

    add("TITLE", &track.title);
    let artist = if track.artist.trim().is_empty() {
        &release.artist
    } else {
        &track.artist
    };
    add("ARTIST", artist);
    add("ALBUM", &release.title);
    add("ALBUMARTIST", &release.artist);
    add(
        "ALBUMARTISTSORT",
        release.artist_sort.as_deref().unwrap_or_default(),
    );
    let year = release.year.map(|year| year.to_string());
    add(
        "DATE",
        release
            .date
            .as_deref()
            .or(year.as_deref())
            .unwrap_or_default(),
    );
    if let Some(number) = track.number {
        add("TRACKNUMBER", &number.get().to_string());
    }
    if !release.tracks.is_empty() {
        add("TRACKTOTAL", &release.tracks.len().to_string());
    }
    if release.disc_number > 0 {
        add("DISCNUMBER", &release.disc_number.to_string());
    }
    if release.disc_total > 0 {
        add("DISCTOTAL", &release.disc_total.to_string());
    }
    if let Some(isrc) = &track.isrc {
        add("ISRC", isrc.as_str());
    }
    add("BARCODE", release.barcode.as_deref().unwrap_or_default());
    add("LABEL", release.label.as_deref().unwrap_or_default());
    add(
        "CATALOGNUMBER",
        release.catalog_number.as_deref().unwrap_or_default(),
    );
    if let Some(id) = &release.release_mbid {
        add("MUSICBRAINZ_ALBUMID", id.as_str());
    }
    if let Some(id) = &release.release_group_mbid {
        add("MUSICBRAINZ_RELEASEGROUPID", id.as_str());
    }
    if let Some(id) = &track.recording_mbid {
        add("MUSICBRAINZ_TRACKID", id.as_str());
    }
    if let Some(id) = &track.track_mbid {
        add("MUSICBRAINZ_RELEASETRACKID", id.as_str());
    }
    let track_artists = if track.artist_mbids.is_empty() {
        &release.artist_mbids
    } else {
        &track.artist_mbids
    };
    for id in track_artists {
        add("MUSICBRAINZ_ARTISTID", id.as_str());
    }
    for id in &release.artist_mbids {
        add("MUSICBRAINZ_ALBUMARTISTID", id.as_str());
    }
    add(
        "MUSICBRAINZ_DISCID",
        release.disc_id.as_deref().unwrap_or_default(),
    );
    add("MEDIA", "CD");
    add("RIPPER", &format!("Library Rip {}", crate::VERSION));
    comments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Isrc, Mbid, TrackNumber};

    fn mbid(text: &str) -> Mbid {
        Mbid::new(text).unwrap()
    }

    fn sample() -> (ReleaseMetadata, TrackMetadata) {
        let track = TrackMetadata {
            number: Some(TrackNumber::new(2).unwrap()),
            title: "Freddie Freeloader".into(),
            artist: String::new(),
            recording_mbid: Some(mbid("11111111-1111-1111-1111-111111111111")),
            track_mbid: Some(mbid("22222222-2222-2222-2222-222222222222")),
            artist_mbids: Vec::new(),
            isrc: Some(Isrc::new("USSM15900113").unwrap()),
        };
        let release = ReleaseMetadata {
            title: "Kind of Blue".into(),
            artist: "Miles Davis".into(),
            artist_sort: Some("Davis, Miles".into()),
            release_mbid: Some(mbid("33333333-3333-3333-3333-333333333333")),
            release_group_mbid: Some(mbid("44444444-4444-4444-4444-444444444444")),
            artist_mbids: vec![mbid("561d854a-6a28-4aa7-8c99-323e6ce46c2a")],
            disc_id: Some("lwHl8fGzJyLXQR33ug60E8jhf4k-".into()),
            year: Some(1959),
            date: Some("1959-08-17".into()),
            label: Some("Columbia".into()),
            catalog_number: Some("CL 1355".into()),
            barcode: Some("074646493526".into()),
            disc_number: 1,
            disc_total: 1,
            tracks: vec![TrackMetadata::default(); 5],
            cover: None,
        };
        (release, track)
    }

    fn value<'a>(comments: &'a [VorbisComment], name: &str) -> Vec<&'a str> {
        comments
            .iter()
            .filter(|c| c.name == name)
            .map(|c| c.value.as_str())
            .collect()
    }

    #[test]
    fn writes_every_field() {
        let (release, track) = sample();
        let comments = vorbis_comments(&release, &track);
        let expect = [
            ("TITLE", "Freddie Freeloader"),
            ("ARTIST", "Miles Davis"),
            ("ALBUM", "Kind of Blue"),
            ("ALBUMARTIST", "Miles Davis"),
            ("ALBUMARTISTSORT", "Davis, Miles"),
            ("DATE", "1959-08-17"),
            ("TRACKNUMBER", "2"),
            ("TRACKTOTAL", "5"),
            ("DISCNUMBER", "1"),
            ("DISCTOTAL", "1"),
            ("ISRC", "USSM15900113"),
            ("BARCODE", "074646493526"),
            ("LABEL", "Columbia"),
            ("CATALOGNUMBER", "CL 1355"),
            (
                "MUSICBRAINZ_ALBUMID",
                "33333333-3333-3333-3333-333333333333",
            ),
            (
                "MUSICBRAINZ_RELEASEGROUPID",
                "44444444-4444-4444-4444-444444444444",
            ),
            (
                "MUSICBRAINZ_TRACKID",
                "11111111-1111-1111-1111-111111111111",
            ),
            (
                "MUSICBRAINZ_RELEASETRACKID",
                "22222222-2222-2222-2222-222222222222",
            ),
            (
                "MUSICBRAINZ_ARTISTID",
                "561d854a-6a28-4aa7-8c99-323e6ce46c2a",
            ),
            (
                "MUSICBRAINZ_ALBUMARTISTID",
                "561d854a-6a28-4aa7-8c99-323e6ce46c2a",
            ),
            ("MUSICBRAINZ_DISCID", "lwHl8fGzJyLXQR33ug60E8jhf4k-"),
            ("MEDIA", "CD"),
        ];
        for (name, expected) in expect {
            assert_eq!(value(&comments, name), vec![expected], "{name}");
        }
        assert_eq!(
            value(&comments, "RIPPER"),
            vec![format!("Library Rip {}", crate::VERSION).as_str()]
        );
        assert!(comments.iter().all(|c| {
            c.name
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch == '_')
        }));
    }

    #[test]
    fn leaves_out_what_is_unknown() {
        let release = ReleaseMetadata {
            title: "Untitled".into(),
            year: Some(2001),
            ..ReleaseMetadata::default()
        };
        let track = TrackMetadata {
            title: "  ".into(),
            ..TrackMetadata::default()
        };
        let comments = vorbis_comments(&release, &track);
        let names: Vec<&str> = comments.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["ALBUM", "DATE", "MEDIA", "RIPPER"]);
        assert_eq!(value(&comments, "DATE"), vec!["2001"]);
    }

    #[test]
    fn track_artist_overrides_album_artist() {
        let (release, mut track) = sample();
        track.artist = "Miles Davis Sextet".into();
        track.artist_mbids = vec![mbid("55555555-5555-5555-5555-555555555555")];
        let comments = vorbis_comments(&release, &track);
        assert_eq!(value(&comments, "ARTIST"), vec!["Miles Davis Sextet"]);
        assert_eq!(
            value(&comments, "MUSICBRAINZ_ARTISTID"),
            vec!["55555555-5555-5555-5555-555555555555"]
        );
        assert_eq!(value(&comments, "ALBUMARTIST"), vec!["Miles Davis"]);
    }
}
