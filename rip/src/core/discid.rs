// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disc identifiers computed from the table of contents.
//!
//! Conventions, checked against published examples (the MusicBrainz "Disc ID
//! Calculation" page and the test vectors of the `cdtoc` crate, which agree
//! with the live AccurateRip and CUETools databases):
//!
//! - **MusicBrainz:** SHA-1 over upper-case hex text: first track (2 digits),
//!   last track (2), lead-out (8), then 99 track offsets (8 each, `0` for
//!   missing tracks); offsets include the 150-sector lead-in. Base64 with
//!   `+/=` replaced by `._-`. On an enhanced CD (audio session followed by a
//!   data session) the trailing data tracks are left out and the lead-out is
//!   the data track's start less the 11 400-sector session gap. A data track
//!   *before* the audio (mixed mode) is kept, as libdiscid does.
//! - **freedb/CDDB:** every track (data tracks too) and the real lead-out.
//! - **AccurateRip:** audio tracks only, weighted by their position among
//!   the audio tracks (1-based); the lead-out is the real lead-out (for an
//!   enhanced CD, the end of the data track), weighted by audio tracks + 1.
//!   Sector offsets without the lead-in; a zero offset weighs as 1 in id 2.
//! - **CTDB:** every track start without the lead-in, data tracks prefixed
//!   with `-`, then the real lead-out, joined with `:`.

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{GeneralPurpose, general_purpose::PAD};
use sha1::{Digest, Sha1};

use super::{CoreError, Result};
use crate::model::{
    AccurateRipDiscId, DiscIds, Lba, MSF_OFFSET_SECTORS, SECTORS_PER_SECOND, Toc, TrackEntry,
};

/// Sectors between the audio session's lead-out and the data track on an
/// enhanced CD (CD-Extra): lead-out, lead-in and pre-gap of the second session.
pub const SESSION_GAP_SECTORS: u32 = 11_400;

/// The MusicBrainz disc ID: SHA-1 over the hex TOC (first/last track, lead-out
/// and 99 track offsets, MSF-offset sectors), base64 with `.`, `_`, `-`.
/// Data tracks after the audio session are excluded per MusicBrainz rules.
pub fn musicbrainz_disc_id(toc: &Toc) -> Result<String> {
    let tracks = musicbrainz_tracks(toc)?;
    let (Some(first), Some(last)) = (tracks.first(), tracks.last()) else {
        return Err(no_audio());
    };
    let mut offsets = [0u32; 100];
    if let Some(slot) = offsets.first_mut() {
        *slot = with_lead_in(audio_lead_out(toc))?;
    }
    for track in tracks {
        if let Some(slot) = offsets.get_mut(usize::from(track.number.get())) {
            *slot = with_lead_in(track.start_lba)?;
        }
    }
    let mut text = String::with_capacity(4 + 8 * offsets.len());
    text.push_str(&format!(
        "{:02X}{:02X}",
        first.number.get(),
        last.number.get()
    ));
    for offset in offsets {
        text.push_str(&format!("{offset:08X}"));
    }
    let digest = Sha1::digest(text.as_bytes());
    Ok(musicbrainz_base64()?.encode(digest).replace('=', "-"))
}

/// The freedb/CDDB disc ID.
pub fn cddb_disc_id(toc: &Toc) -> Result<u32> {
    let tracks = toc.tracks();
    let Some(first) = tracks.first() else {
        return Err(no_audio());
    };
    let mut digit_sum = 0u32;
    for track in tracks {
        let mut seconds = with_lead_in(track.start_lba)? / SECTORS_PER_SECOND;
        while seconds > 0 {
            digit_sum += seconds % 10;
            seconds /= 10;
        }
    }
    let start_seconds = with_lead_in(first.start_lba)? / SECTORS_PER_SECOND;
    let end_seconds = with_lead_in(toc.lead_out())? / SECTORS_PER_SECOND;
    let length_seconds = end_seconds.saturating_sub(start_seconds) & 0xFFFF;
    let count = u32::try_from(tracks.len()).map_err(|_| too_many())? & 0xFF;
    Ok(((digit_sum % 0xFF) << 24) | (length_seconds << 8) | count)
}

/// The AccurateRip disc identity (audio tracks only).
pub fn accuraterip_disc_id(toc: &Toc) -> Result<AccurateRipDiscId> {
    let mut id1 = 0u32;
    let mut id2 = 0u32;
    let mut count = 0u32;
    for track in toc.audio_tracks() {
        count += 1;
        let offset = track.start_lba.0;
        id1 = id1.wrapping_add(offset);
        id2 = id2.wrapping_add(offset.max(1).wrapping_mul(count));
    }
    if count == 0 {
        return Err(no_audio());
    }
    let lead_out = toc.lead_out().0;
    id1 = id1.wrapping_add(lead_out);
    id2 = id2.wrapping_add(lead_out.max(1).wrapping_mul(count + 1));
    Ok(AccurateRipDiscId {
        track_count: u8::try_from(count).map_err(|_| too_many())?,
        id1,
        id2,
        cddb: cddb_disc_id(toc)?,
    })
}

/// The CUETools database TOC string (`toc=` query parameter).
pub fn ctdb_toc_id(toc: &Toc) -> Result<String> {
    if toc.audio_tracks().next().is_none() {
        return Err(no_audio());
    }
    let mut out = String::new();
    for track in toc.tracks() {
        if !track.is_audio {
            out.push('-');
        }
        out.push_str(&track.start_lba.0.to_string());
        out.push(':');
    }
    out.push_str(&toc.lead_out().0.to_string());
    Ok(out)
}

/// All identifiers at once.
pub fn disc_ids(toc: &Toc) -> Result<DiscIds> {
    Ok(DiscIds {
        musicbrainz: musicbrainz_disc_id(toc)?,
        cddb: cddb_disc_id(toc)?,
        accuraterip: accuraterip_disc_id(toc)?,
        ctdb_toc: ctdb_toc_id(toc)?,
    })
}

/// The end of the audio: the lead-out, or on an enhanced CD the start of the
/// first trailing data track less the session gap. This is where the last
/// audio track really ends; the sectors from there to the data track cannot
/// be read as audio.
pub fn audio_lead_out(toc: &Toc) -> Lba {
    match trailing_data_start(toc) {
        Some(start) => Lba(start.0.checked_sub(SESSION_GAP_SECTORS).unwrap_or(start.0)),
        None => toc.lead_out(),
    }
}

/// Start of the data tracks that follow the last audio track (enhanced CD).
fn trailing_data_start(toc: &Toc) -> Option<Lba> {
    let tracks = toc.tracks();
    let last_audio = tracks.iter().rposition(|t| t.is_audio)?;
    tracks.get(last_audio + 1).map(|t| t.start_lba)
}

/// The tracks MusicBrainz hashes: everything up to the last audio track.
fn musicbrainz_tracks(toc: &Toc) -> Result<&[TrackEntry]> {
    let tracks = toc.tracks();
    let last_audio = tracks
        .iter()
        .rposition(|t| t.is_audio)
        .ok_or_else(no_audio)?;
    tracks.get(..=last_audio).ok_or_else(no_audio)
}

fn with_lead_in(lba: Lba) -> Result<u32> {
    lba.0
        .checked_add(MSF_OFFSET_SECTORS)
        .ok_or_else(|| CoreError::Invalid(format!("sector {lba} is beyond the end of any disc")))
}

/// Standard base64 with `.` and `_` for `+` and `/`; the `=` padding is
/// swapped for `-` after encoding.
fn musicbrainz_base64() -> Result<GeneralPurpose> {
    let alphabet =
        Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._")
            .map_err(|e| CoreError::Invalid(format!("base64 alphabet: {e}")))?;
    Ok(GeneralPurpose::new(&alphabet, PAD))
}

fn no_audio() -> CoreError {
    CoreError::Invalid("the disc has no audio tracks".into())
}

fn too_many() -> CoreError {
    CoreError::Invalid("the disc has more than 99 tracks".into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{Sectors, TrackNumber};

    /// Builds a TOC from the `CDTOC` notation used by the `cdtoc` crate's
    /// tests: audio track count (hex), audio track offsets with lead-in
    /// (hex), then either the lead-out or a trailing data track and the
    /// lead-out.
    pub(crate) fn toc_from_cdtoc(text: &str) -> Toc {
        let fields: Vec<u32> = text
            .split('+')
            .map(|f| u32::from_str_radix(f, 16).unwrap())
            .collect();
        let count = fields[0] as usize;
        let audio = &fields[1..=count];
        let rest = &fields[count + 1..];
        let (data, lead_out) = match rest {
            [lead_out] => (None, *lead_out),
            [data, lead_out] => (Some(*data), *lead_out),
            _ => panic!("bad CDTOC"),
        };
        let mut starts: Vec<(u32, bool)> = audio.iter().map(|s| (s - 150, true)).collect();
        if let Some(data) = data {
            starts.push((data - 150, false));
        }
        let lead_out = lead_out - 150;
        let tracks = starts
            .iter()
            .enumerate()
            .map(|(i, (start, is_audio))| {
                let end = starts.get(i + 1).map_or(lead_out, |n| n.0);
                TrackEntry {
                    number: TrackNumber::new(i as u8 + 1).unwrap(),
                    start_lba: Lba(*start),
                    length_sectors: Sectors(end - start),
                    is_audio: *is_audio,
                    pregap: Sectors(0),
                    isrc: None,
                }
            })
            .collect();
        Toc::new(tracks, Lba(lead_out)).unwrap()
    }

    pub(crate) const SIMPLE: &str = "4+96+2D2B+6256+B327+D84A";
    const ENHANCED: &str =
        "D+96+3B5D+78E3+B441+EC83+134F4+17225+1A801+1EA5C+23B5B+27CEF+2B58B+2F974+35D56+514C8";
    const ENHANCED_HTOA: &str = "18+B6+3CE3+7C6F+B2BD+E47F+1121C+15865+175E0+1AED9+1E159+20BF9+235FC+259EF+2826E+29B62+2ED67+311B1+3396B+36ACB+3916B+3BB75+3D60A+40AA6+422FE+48B68+4E4CB";
    const AUDIO_HTOA: &str = "10+B6+5352+62AC+99D6+E218+12AC0+135E7+142E9+178B0+19D22+1B0D0+1E7FA+22882+247DB+27074+2A1BD+2C0FB";
    const DOCS_EXAMPLE: &str = "6+96+3C03+7E3A+B600+F7B6+13A69+174E6";

    fn ar_name(id: &AccurateRipDiscId) -> String {
        format!(
            "{:03}-{:08x}-{:08x}-{:08x}",
            id.track_count, id.id1, id.id2, id.cddb
        )
    }

    #[test]
    fn musicbrainz_documentation_example() {
        // https://musicbrainz.org/doc/Disc_ID_Calculation:
        // "1 6 95462 150 15363 32314 46592 63414 80489".
        assert_eq!(
            musicbrainz_disc_id(&toc_from_cdtoc(DOCS_EXAMPLE)).unwrap(),
            "49HHV7Eb8UKF3aQiNmu1GR8vKTY-"
        );
    }

    #[test]
    fn musicbrainz_known_answers() {
        for (cdtoc, expected) in [
            (SIMPLE, "nljDXdC8B_pDwbdY1vZJvdrAZI4-"),
            (ENHANCED, "ucgpiD84p.2iBxO4j3hdjSjhtnw-"),
            (ENHANCED_HTOA, "eLuEIkHsua.iJpetabxqYM9SIbk-"),
            (AUDIO_HTOA, "PQ02DnwdDaxgWEFSpAzI_IVBL3o-"),
        ] {
            assert_eq!(
                musicbrainz_disc_id(&toc_from_cdtoc(cdtoc)).unwrap(),
                expected,
                "{cdtoc}"
            );
        }
    }

    #[test]
    fn accuraterip_known_answers() {
        for (cdtoc, expected) in [
            (SIMPLE, "004-0002189a-00087f33-1f02e004"),
            (ENHANCED, "013-001802ed-00f8ee31-b611560e"),
        ] {
            let id = accuraterip_disc_id(&toc_from_cdtoc(cdtoc)).unwrap();
            assert_eq!(ar_name(&id), expected, "{cdtoc}");
        }
    }

    #[test]
    fn cddb_by_hand() {
        // Seconds (with lead-in): 2, 204, 430, 621, 845, 1073; digit sums
        // 2+6+7+9+17+11 = 52 = 0x34; length 1272 - 2 = 1270 = 0x4F6; 6 tracks.
        assert_eq!(
            cddb_disc_id(&toc_from_cdtoc(DOCS_EXAMPLE)).unwrap(),
            0x3404_F606
        );
    }

    #[test]
    fn ctdb_toc_strings_match_the_database() {
        assert_eq!(
            ctdb_toc_id(&toc_from_cdtoc(SIMPLE)).unwrap(),
            "0:11413:25024:45713:55220"
        );
        let enhanced = ctdb_toc_id(&toc_from_cdtoc(ENHANCED)).unwrap();
        assert!(enhanced.starts_with("0:15047:"), "{enhanced}");
        assert!(enhanced.ends_with(":-220352:332850"), "{enhanced}");
        let htoa = ctdb_toc_id(&toc_from_cdtoc(AUDIO_HTOA)).unwrap();
        assert!(htoa.starts_with("32:21180:"), "{htoa}");
        assert!(htoa.ends_with(":180325"), "{htoa}");
    }

    #[test]
    fn audio_lead_out_skips_the_session_gap() {
        assert_eq!(audio_lead_out(&toc_from_cdtoc(SIMPLE)), Lba(55220));
        assert_eq!(
            audio_lead_out(&toc_from_cdtoc(ENHANCED)),
            Lba(0x35D56 - 150 - 11_400)
        );
    }

    #[test]
    fn data_only_disc_is_refused() {
        let toc = Toc::new(
            vec![TrackEntry {
                number: TrackNumber::new(1).unwrap(),
                start_lba: Lba(0),
                length_sectors: Sectors(1000),
                is_audio: false,
                pregap: Sectors(0),
                isrc: None,
            }],
            Lba(1000),
        )
        .unwrap();
        assert!(disc_ids(&toc).is_err());
        assert!(musicbrainz_disc_id(&toc).is_err());
    }

    #[test]
    fn disc_ids_bundles_everything() {
        let ids = disc_ids(&toc_from_cdtoc(SIMPLE)).unwrap();
        assert_eq!(ids.cddb, 0x1f02_e004);
        assert_eq!(ids.accuraterip.cddb, ids.cddb);
        assert_eq!(ids.musicbrainz.len(), 28);
    }
}
