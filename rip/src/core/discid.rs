// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disc identifiers computed from the table of contents.
//!
//! Conventions, checked against published examples (the MusicBrainz "Disc ID
//! Calculation" page and the test vectors of the `cdtoc` crate, which agree
//! with the live AccurateRip and CUETools databases) and against the code
//! of the tools the databases come from:
//!
//! - **MusicBrainz** (<https://musicbrainz.org/doc/Disc_ID_Calculation>,
//!   libdiscid `src/toc.c`, whipper `image/table.py`
//!   `_getMusicBrainzValues`): SHA-1 over upper-case hex text: first track
//!   (2 digits), last track (2), lead-out (8), then 99 track offsets (8
//!   each, `0` for missing tracks); offsets include the 150-sector lead-in.
//!   Base64 with `+/=` replaced by `._-`. On an enhanced CD (audio session
//!   followed by a data session) the trailing data tracks are left out, the
//!   last track is the last audio track, and the lead-out is the audio
//!   session's end: the data track's start less the 11,400-sector session
//!   gap (whipper: `self.tracks[-1].getIndex(1).absolute - 11400`). We use
//!   the end of the last audio track from the TOC, which the drive layer
//!   sets to exactly that (or to the session lead-out the drive reports). A
//!   data track *before* the audio (mixed mode) is kept, as libdiscid does.
//! - **freedb/CDDB** (whipper `getCDDBValues`: "cddb disc id takes into
//!   account data tracks"): every track, data tracks too, and the real
//!   lead-out of the whole disc (the end of the data track); the last byte
//!   is the number of tracks including data tracks.
//! - **AccurateRip** (CUETools `AccurateRipVerify.CalculateAccurateRipId`,
//!   whipper `accuraterip_ids`: "AccurateRip does not take into account data
//!   tracks, but does count the data track to determine the leadout
//!   offset"): id 1 is the sum of the audio track offsets plus the real
//!   lead-out (for an enhanced CD the end of the data track, so the data
//!   track's extent is included); id 2 the sum of each audio track's offset
//!   times its position among the audio tracks (1-based), plus the lead-out
//!   times (audio tracks + 1). Sector offsets without the lead-in; a zero
//!   offset weighs as 1 in id 2. The `dBAR` file name carries the number of
//!   audio tracks. The track checksums themselves cover audio tracks only.
//! - **CTDB** (CUETools `CDImageLayout.ToString`): every track start
//!   without the lead-in, data tracks prefixed with `-`, then the real
//!   lead-out, joined with `:`. The CTDB disc CRC covers the audio only,
//!   up to the end of the audio session (CUETools `AudioLength`, whose last
//!   audio track ends `152 × 75` = 11,400 sectors before the data track).

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{GeneralPurpose, general_purpose::PAD};
use sha1::{Digest, Sha1};

use super::{CoreError, Result};
pub use crate::model::SESSION_GAP_SECTORS;
use crate::model::{
    AccurateRipDiscId, DiscIds, Lba, MSF_OFFSET_SECTORS, SECTORS_PER_SECOND, Toc, TrackEntry,
};

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

/// The end of the audio: the lead-out of an audio CD, or on an enhanced CD
/// the audio session's end, where the last audio track really ends; the
/// sectors from there to the data track cannot be read as audio.
///
/// A TOC from an older report may list an enhanced CD flat, its last audio
/// track running on to the data track in the same session; then, as
/// libdiscid does, the audio ends [`SESSION_GAP_SECTORS`] before the data.
pub fn audio_lead_out(toc: &Toc) -> Lba {
    let tracks = toc.tracks();
    let Some(last_audio) = tracks.iter().rposition(|t| t.is_audio) else {
        return toc.lead_out();
    };
    let (Some(audio), next) = (tracks.get(last_audio), tracks.get(last_audio + 1)) else {
        return toc.lead_out();
    };
    let end = audio.end_lba().unwrap_or(toc.lead_out());
    match next {
        Some(data) if data.session == audio.session && data.start_lba == end => {
            Lba(data.start_lba.0.saturating_sub(SESSION_GAP_SECTORS)).max(audio.start_lba)
        }
        Some(_) | None => end,
    }
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
    use crate::model::{Sectors, SessionNumber, TrackNumber};

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
                    session: SessionNumber::FIRST,
                }
            })
            .collect();
        Toc::new_assuming_enhanced_gap(tracks, Lba(lead_out)).unwrap()
    }

    pub(crate) const SIMPLE: &str = "4+96+2D2B+6256+B327+D84A";
    pub(crate) const ENHANCED: &str =
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

    /// Ladyhawke, "Ladyhawke" (barcode 0602517818866): an enhanced CD of
    /// 12 audio tracks and a data track. TOC and expected ids are whipper's
    /// `LadyhawkeTestCase` (whipper/test/test_image_table.py), whose CDDB id
    /// was checked against freedb and whose AccurateRip URL against EAC's
    /// (captured with Wireshark). Offsets are without the lead-in; the data
    /// track starts at 207106 and the disc's lead-out is 210385.
    fn ladyhawke() -> Toc {
        let starts = [
            0u32, 15537, 31691, 50866, 66466, 81202, 99409, 115920, 133093, 149847, 161560, 177682,
            207106,
        ];
        let lead_out = 210_385u32;
        let tracks = starts
            .iter()
            .enumerate()
            .map(|(i, start)| {
                let end = starts.get(i + 1).copied().unwrap_or(lead_out);
                TrackEntry {
                    number: TrackNumber::new(i as u8 + 1).unwrap(),
                    start_lba: Lba(*start),
                    length_sectors: Sectors(end - start),
                    is_audio: i < 12,
                    pregap: Sectors(0),
                    isrc: None,
                    session: SessionNumber::FIRST,
                }
            })
            .collect();
        Toc::new_assuming_enhanced_gap(tracks, Lba(lead_out)).unwrap()
    }

    #[test]
    fn enhanced_cd_known_answers_from_a_real_disc() {
        let toc = ladyhawke();
        // The audio session ends 11,400 sectors before the data track.
        let last_audio = toc.track(TrackNumber::new(12).unwrap()).unwrap();
        assert_eq!(last_audio.end_lba(), Some(Lba(207_106 - 11_400)));
        assert_eq!(toc.audio_end(), Some(Lba(195_706)));
        let data = toc.track(TrackNumber::new(13).unwrap()).unwrap();
        assert!(!data.is_audio);
        assert_eq!(data.session.get(), 2);
        assert_eq!(toc.layout(), crate::model::DiscLayout::Enhanced);

        assert_eq!(cddb_disc_id(&toc).unwrap(), 0xc60a_f50d);
        assert_eq!(
            musicbrainz_disc_id(&toc).unwrap(),
            "KnpGsLhvH.lPrNc1PBL21lb9Bg4-"
        );
        let ar = accuraterip_disc_id(&toc).unwrap();
        assert_eq!(ar_name(&ar), "012-0013bd5a-00b8d489-c60af50d");
        assert_eq!(
            crate::core::accuraterip::database_url(
                &url::Url::parse("http://www.accuraterip.com/accuraterip/").unwrap(),
                &ar
            )
            .unwrap()
            .path(),
            "/accuraterip/a/5/d/dBAR-012-0013bd5a-00b8d489-c60af50d.bin"
        );
        // CUETools convention (no published value to compare with): every
        // track start, the data track marked with `-`, then the lead-out.
        assert_eq!(
            ctdb_toc_id(&toc).unwrap(),
            "0:15537:31691:50866:66466:81202:99409:115920:133093:149847:161560:177682:-207106:210385"
        );
    }

    #[test]
    fn a_flat_listing_of_an_enhanced_cd_gives_the_same_ids() {
        // An older report lists the last audio track running on to the data
        // track in one session; the ids must not change.
        let toc = ladyhawke();
        let mut tracks = toc.tracks().to_vec();
        tracks[11].length_sectors = Sectors(207_106 - 177_682);
        tracks[12].session = SessionNumber::FIRST;
        let flat = Toc::new(tracks, toc.lead_out()).unwrap();
        assert_eq!(audio_lead_out(&flat), Lba(195_706));
        assert_eq!(disc_ids(&flat).unwrap(), disc_ids(&toc).unwrap());
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
                session: SessionNumber::FIRST,
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
