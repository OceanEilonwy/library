//! Matching ripped files to a release's tracks, and ranking releases by how
//! well they fit the files.
//!
//! This lives on the server (not in the studio's script) because the server
//! fetches and caches the candidate releases anyway: ranking needs several
//! full track lists, which would otherwise all have to travel to the
//! browser, and the rules get Rust unit tests next to the parser.
//!
//! Each file is compared with each track on three signals, weighted:
//! the measured length against the track's length (the main one: a rip is
//! within a second or two of the listed length), the disc and track
//! numbers read from the file name, and how alike the titles are. The
//! assignment that scores best overall (one file per track) is found with
//! the Hungarian method, so a shuffled folder still lines up. A file whose
//! length is far from its track's is never assigned silently: it is marked
//! "no match" and the studio leaves it alone.

use serde::{Deserialize, Serialize};

use crate::musicbrainz::Release;
use crate::util;

/// What the studio knows about a file waiting to upload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    /// The studio's own id for the row, echoed back.
    pub key: String,
    /// The original file name (with its folder, if any).
    #[serde(default)]
    pub name: String,
    /// Disc number read from the name or folder ("CD2/…", "2-03 …").
    #[serde(default)]
    pub disc: Option<u32>,
    /// Track number read from the name.
    #[serde(default)]
    pub track: Option<u32>,
    /// Title read from the name.
    #[serde(default)]
    pub title: String,
    /// Length measured in the browser.
    #[serde(default)]
    pub duration_ms: Option<i64>,
}

/// How sure a match is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Length and name agree: safe to apply.
    Good,
    /// Probably right, but something disagrees: worth a look.
    Check,
    /// Not assigned: nothing fits, or the length is far off.
    None,
}

/// Where one file goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMatch {
    pub key: String,
    pub confidence: Confidence,
    /// The track it would go to (shown even for `None`, as a hint).
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub length_ms: Option<i64>,
    /// Plain English: why it needs checking, or why it wasn't matched.
    pub note: String,
}

/// A track no file was matched to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingTrack {
    pub disc: u32,
    pub track: u32,
    pub title: String,
    pub length_ms: Option<i64>,
}

/// How well a release fits the files, 0–100, with a one-line summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fit {
    pub score: u32,
    pub files: usize,
    pub tracks: usize,
    pub good: usize,
    pub check: usize,
    pub none: usize,
    /// Summed length difference of the matched files.
    pub duration_error_ms: Option<i64>,
    /// False when only track counts were compared (no track list fetched).
    pub compared: bool,
    /// "Fits your 12 files", "10 of 12 files fit"…
    pub summary: String,
}

/// The best assignment of files to a release's tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    /// One per file, in the order the files were given.
    pub matches: Vec<FileMatch>,
    pub missing: Vec<MissingTrack>,
    pub fit: Fit,
}

/// Within this, a length counts as the same.
const GOOD_MS: i64 = 3_000;

/// Beyond this (or 2% of a long track), a length is "way off".
fn off_limit(length_ms: i64) -> i64 {
    (length_ms / 50).max(10_000)
}

const W_DURATION: f64 = 6.0;
const W_NUMBER: f64 = 3.0;
const W_TITLE: f64 = 2.0;

/// A track flattened out of its medium.
struct Slot<'a> {
    disc: u32,
    track: u32,
    /// 1-based position across the whole release.
    overall: u32,
    discs: usize,
    title: &'a str,
    title_key: String,
    artist: Option<&'a str>,
    length_ms: Option<i64>,
}

fn slots(release: &Release) -> Vec<Slot<'_>> {
    let discs = release.media.len();
    let mut overall = 0u32;
    let mut out = Vec::with_capacity(release.track_count());
    for m in &release.media {
        for t in &m.tracks {
            overall = overall.saturating_add(1);
            out.push(Slot {
                disc: m.position,
                track: t.position,
                overall,
                discs,
                title: &t.title,
                title_key: util::normalize(&t.title),
                artist: t.artist.as_deref(),
                length_ms: t.length_ms,
            });
        }
    }
    out
}

/// A file's title as a comparison key, or `None` when it says nothing
/// ("Track 01", "Audio Track 3", a bare number).
fn file_title_key(title: &str) -> Option<String> {
    let key = util::normalize(title);
    let words: Vec<&str> = key.split(' ').filter(|w| !w.is_empty()).collect();
    let filler = |w: &&str| {
        w.chars().all(|c| c.is_ascii_digit())
            || matches!(
                *w,
                "track" | "audio" | "audiotrack" | "piste" | "titel" | "untitled" | "cd" | "disc"
            )
    };
    if words.iter().all(filler) {
        None
    } else {
        Some(key)
    }
}

/// The title key for a file: its parsed title, else the words of its file
/// name without the numbers.
fn title_key_of(file: &FileInfo) -> Option<String> {
    if !file.title.trim().is_empty() {
        return file_title_key(&file.title);
    }
    let words: Vec<String> = util::normalize(crate::digital::stem(&file.name))
        .split(' ')
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .collect();
    let joined = words.join(" ");
    if joined.chars().count() < 3 {
        None
    } else {
        file_title_key(&joined)
    }
}

/// How alike two normalised titles are, 0–1: the better of edit-distance
/// similarity and shared words, with one containing the other counting
/// as close.
fn title_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let edit = util::similarity(a, b);
    let wa: std::collections::BTreeSet<&str> = a.split(' ').collect();
    let wb: std::collections::BTreeSet<&str> = b.split(' ').collect();
    let shared = wa.intersection(&wb).count() as f64;
    let words = shared / wa.union(&wb).count().max(1) as f64;
    let contains = if a.contains(b) || b.contains(a) {
        0.85
    } else {
        0.0
    };
    edit.max(words).max(contains)
}

fn duration_score(diff_ms: i64) -> f64 {
    let d = diff_ms as f64;
    if diff_ms <= 2_000 {
        1.0
    } else if diff_ms <= 5_000 {
        1.0 - 0.4 * (d - 2_000.0) / 3_000.0
    } else if diff_ms <= 15_000 {
        0.6 - 0.6 * (d - 5_000.0) / 10_000.0
    } else {
        0.0
    }
}

/// How well the numbers in the file name agree with a slot, 0–1, or
/// `None` when the name had no track number.
fn number_score(file: &FileInfo, slot: &Slot) -> Option<f64> {
    let track = file.track?;
    let disc_ok = file.disc.is_none_or(|d| d == slot.disc);
    Some(if track == slot.track && disc_ok {
        1.0
    } else if track == slot.overall && (file.disc.is_none() || slot.discs == 1) {
        0.9
    } else if track == slot.track {
        0.3
    } else {
        0.0
    })
}

/// The signals for one file against one slot.
struct Signals {
    diff_ms: Option<i64>,
    number: Option<f64>,
    title: Option<f64>,
    score: f64,
}

fn signals(file: &FileInfo, file_key: Option<&str>, slot: &Slot) -> Signals {
    let diff_ms = match (file.duration_ms.filter(|d| *d > 0), slot.length_ms) {
        (Some(a), Some(b)) => Some((a - b).abs()),
        _ => None,
    };
    let number = number_score(file, slot);
    let title = file_key.map(|k| title_similarity(k, &slot.title_key));
    let mut total = 0.0;
    let mut weight = 0.0;
    for (w, v) in [
        (W_DURATION, diff_ms.map(duration_score)),
        (W_NUMBER, number),
        (W_TITLE, title),
    ] {
        if let Some(v) = v {
            total += w * v;
            weight += w;
        }
    }
    Signals {
        diff_ms,
        number,
        title,
        score: if weight > 0.0 { total / weight } else { 0.0 },
    }
}

/// "3:05".
fn clock(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "4 seconds", "1:12".
fn gap(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    if s < 60 {
        format!("{s} second{}", if s == 1 { "" } else { "s" })
    } else {
        clock(ms)
    }
}

/// Finds the assignment of rows to columns with the least total cost
/// (Hungarian method, O(n²m)). Rows ≤ columns is handled by transposing.
fn hungarian(cost: &[Vec<i64>]) -> Vec<Option<usize>> {
    let rows = cost.len();
    let cols = cost.first().map_or(0, Vec::len);
    if rows == 0 || cols == 0 {
        return vec![None; rows];
    }
    if rows > cols {
        let transposed: Vec<Vec<i64>> = (0..cols)
            .map(|j| (0..rows).map(|i| cost[i][j]).collect())
            .collect();
        let by_col = hungarian(&transposed);
        let mut out = vec![None; rows];
        for (j, i) in by_col.into_iter().enumerate() {
            if let Some(i) = i {
                out[i] = Some(j);
            }
        }
        return out;
    }
    let (n, m) = (rows, cols);
    const INF: i64 = i64::MAX / 4;
    let mut u = vec![0i64; n + 1];
    let mut v = vec![0i64; m + 1];
    let mut p = vec![0usize; m + 1];
    let mut way = vec![0usize; m + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0usize;
        let mut minv = vec![INF; m + 1];
        let mut used = vec![false; m + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = INF;
            let mut j1 = 0usize;
            for j in 1..=m {
                if !used[j] {
                    let cur = cost[i0 - 1][j - 1] - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=m {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = vec![None; n];
    for j in 1..=m {
        if p[j] != 0 {
            out[p[j] - 1] = Some(j - 1);
        }
    }
    out
}

/// Matches `files` to the tracks of `release`.
pub fn assign(files: &[FileInfo], release: &Release) -> Assignment {
    let slots = slots(release);
    let keys: Vec<Option<String>> = files.iter().map(title_key_of).collect();
    let table: Vec<Vec<Signals>> = files
        .iter()
        .zip(&keys)
        .map(|(f, k)| slots.iter().map(|s| signals(f, k.as_deref(), s)).collect())
        .collect();
    let cost: Vec<Vec<i64>> = table
        .iter()
        .map(|row| {
            row.iter()
                .map(|s| 1_000_000 - (s.score * 1_000_000.0).round() as i64)
                .collect()
        })
        .collect();
    let picked = hungarian(&cost);

    let mut taken = vec![false; slots.len()];
    let mut matches = Vec::with_capacity(files.len());
    let mut error_ms: Option<i64> = None;
    for (i, file) in files.iter().enumerate() {
        let Some(j) = picked.get(i).copied().flatten() else {
            matches.push(FileMatch {
                key: file.key.clone(),
                confidence: Confidence::None,
                disc: None,
                track: None,
                title: None,
                artist: None,
                length_ms: None,
                note: "This release has no track left for this file.".into(),
            });
            continue;
        };
        let slot = &slots[j];
        let sig = &table[i][j];
        // Another track as long as this file makes the length alone ambiguous.
        let lookalike = file.duration_ms.is_some_and(|d| {
            slots
                .iter()
                .enumerate()
                .any(|(k, s)| k != j && s.length_ms.is_some_and(|l| (l - d).abs() <= GOOD_MS))
        });
        let number_agrees = sig.number.is_some_and(|n| n >= 0.9);
        let number_disagrees = sig.number.is_some_and(|n| n < 0.5);
        let title_agrees = sig.title.is_some_and(|t| t >= 0.5);
        let named_track = file
            .track
            .map(|t| match file.disc {
                Some(d) if slot.discs > 1 => format!("disc {d}, track {t}"),
                _ => format!("track {t}"),
            })
            .unwrap_or_default();
        let (confidence, note) = match sig.diff_ms {
            Some(d) if d > off_limit(slot.length_ms.unwrap_or(0)) => (
                Confidence::None,
                format!(
                    "Its length ({}) is {} away from the closest track left ({}).",
                    clock(file.duration_ms.unwrap_or(0)),
                    gap(d),
                    clock(slot.length_ms.unwrap_or(0))
                ),
            ),
            Some(d) => {
                error_ms = Some(error_ms.unwrap_or(0) + d);
                if d > GOOD_MS {
                    (
                        Confidence::Check,
                        format!("Its length is {} off the track's.", gap(d)),
                    )
                } else if number_disagrees && !title_agrees {
                    (
                        Confidence::Check,
                        format!("The length fits, but the file name says {named_track}."),
                    )
                } else if number_agrees || title_agrees || !lookalike {
                    (Confidence::Good, String::new())
                } else {
                    (
                        Confidence::Check,
                        "Another track has the same length; check the title.".into(),
                    )
                }
            }
            None => {
                let reason = if file.duration_ms.is_some_and(|d| d > 0) {
                    "MusicBrainz doesn't list this track's length"
                } else {
                    "The file's length couldn't be measured"
                };
                if number_agrees && title_agrees {
                    (Confidence::Good, String::new())
                } else if number_agrees || title_agrees {
                    (
                        Confidence::Check,
                        format!("{reason}; matched by its name only."),
                    )
                } else {
                    (
                        Confidence::None,
                        format!("{reason}, and its name doesn't match a track."),
                    )
                }
            }
        };
        if confidence != Confidence::None {
            taken[j] = true;
        }
        matches.push(FileMatch {
            key: file.key.clone(),
            confidence,
            disc: Some(slot.disc),
            track: Some(slot.track),
            title: Some(slot.title.to_owned()),
            artist: slot.artist.map(str::to_owned),
            length_ms: slot.length_ms,
            note,
        });
    }
    let missing = slots
        .iter()
        .zip(&taken)
        .filter(|(_, t)| !**t)
        .map(|(s, _)| MissingTrack {
            disc: s.disc,
            track: s.track,
            title: s.title.to_owned(),
            length_ms: s.length_ms,
        })
        .collect();
    let fit = fit_of(&matches, slots.len(), error_ms);
    Assignment {
        matches,
        missing,
        fit,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn fit_of(matches: &[FileMatch], tracks: usize, error_ms: Option<i64>) -> Fit {
    let count = |c: Confidence| matches.iter().filter(|m| m.confidence == c).count();
    let (good, check, none) = (
        count(Confidence::Good),
        count(Confidence::Check),
        count(Confidence::None),
    );
    let files = matches.len();
    let timed = matches
        .iter()
        .filter(|m| m.confidence != Confidence::None && m.length_ms.is_some())
        .count();
    let mean_error_s = match (error_ms, timed) {
        (Some(e), n) if n > 0 => e as f64 / n as f64 / 1000.0,
        _ => 0.0,
    };
    let penalty = (12 * files.abs_diff(tracks)).min(60) as f64
        + 20.0 * none as f64
        + 4.0 * check as f64
        + (mean_error_s * 3.0).min(40.0);
    let score = (100.0 - penalty).clamp(0.0, 100.0).round() as u32;
    let fitting = good + check;
    let summary = if files == 0 {
        plural(tracks, "track", "tracks")
    } else if fitting == files && files == tracks {
        if check == 0 {
            format!("Fits your {}", plural(files, "file", "files"))
        } else {
            format!(
                "Fits your {} (check {check})",
                plural(files, "file", "files")
            )
        }
    } else {
        let mut s = format!("{fitting} of your {} fit", plural(files, "file", "files"));
        if tracks != files {
            s.push_str(&format!(" · {}", plural(tracks, "track", "tracks")));
        }
        s
    };
    Fit {
        score,
        files,
        tracks,
        good,
        check,
        none,
        duration_error_ms: error_ms,
        compared: true,
        summary,
    }
}

/// A rough fit from the track count alone, for releases whose track list
/// hasn't been fetched. Always ranks below a release that was compared
/// and fits.
pub fn count_fit(files: usize, tracks: usize) -> Fit {
    let score = 50u32.saturating_sub(u32::try_from(12 * files.abs_diff(tracks)).unwrap_or(50));
    let summary = if files == 0 {
        plural(tracks, "track", "tracks")
    } else if files == tracks {
        format!(
            "{} — same count as your files",
            plural(tracks, "track", "tracks")
        )
    } else {
        format!(
            "{} for your {}",
            plural(tracks, "track", "tracks"),
            plural(files, "file", "files")
        )
    };
    Fit {
        score,
        files,
        tracks,
        good: 0,
        check: 0,
        none: 0,
        duration_error_ms: None,
        compared: false,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::musicbrainz::{Medium, Track};

    fn release(id: &str, discs: &[&[(&str, i64)]]) -> Release {
        Release {
            id: id.into(),
            title: "Album".into(),
            artist: "Band".into(),
            year: None,
            date: String::new(),
            country: String::new(),
            barcode: String::new(),
            media: discs
                .iter()
                .enumerate()
                .map(|(d, tracks)| Medium {
                    position: d as u32 + 1,
                    format: "CD".into(),
                    title: String::new(),
                    tracks: tracks
                        .iter()
                        .enumerate()
                        .map(|(t, (title, ms))| Track {
                            position: t as u32 + 1,
                            number: (t + 1).to_string(),
                            title: (*title).into(),
                            length_ms: Some(*ms),
                            artist: None,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn file(key: &str, disc: Option<u32>, track: Option<u32>, title: &str, ms: i64) -> FileInfo {
        FileInfo {
            key: key.into(),
            name: format!("{key}.flac"),
            disc,
            track,
            title: title.into(),
            duration_ms: Some(ms),
        }
    }

    fn placed(a: &Assignment) -> Vec<(String, Option<u32>, Option<u32>, Confidence)> {
        a.matches
            .iter()
            .map(|m| (m.key.clone(), m.disc, m.track, m.confidence))
            .collect()
    }

    const ALBUM: &[(&str, i64)] = &[
        ("Morning Glory", 245_000),
        ("Paper Boats", 181_000),
        ("Lanterns", 302_000),
        ("The Long Road Home", 412_000),
    ];

    #[test]
    fn exact_rip_fits() {
        let r = release("a", &[ALBUM]);
        let files: Vec<FileInfo> = ALBUM
            .iter()
            .enumerate()
            .map(|(i, (t, ms))| file(&format!("f{i}"), None, Some(i as u32 + 1), t, ms + 400))
            .collect();
        let a = assign(&files, &r);
        assert!(a.matches.iter().all(|m| m.confidence == Confidence::Good));
        assert_eq!(a.matches[3].title.as_deref(), Some("The Long Road Home"));
        assert!(a.missing.is_empty());
        assert_eq!(a.fit.summary, "Fits your 4 files");
        assert!(a.fit.score >= 95, "{}", a.fit.score);
    }

    #[test]
    fn reordered_files_with_bare_names_line_up_by_length() {
        let r = release("a", &[ALBUM]);
        // Shuffled, named "Track 0N" by a ripper that numbered them wrongly.
        let files = vec![
            file("x", None, None, "Track 01", 412_600),
            file("y", None, None, "Track 02", 245_300),
            file("z", None, None, "Track 03", 302_100),
            file("w", None, None, "Track 04", 180_200),
        ];
        let a = assign(&files, &r);
        assert_eq!(
            placed(&a),
            vec![
                ("x".into(), Some(1), Some(4), Confidence::Good),
                ("y".into(), Some(1), Some(1), Confidence::Good),
                ("z".into(), Some(1), Some(3), Confidence::Good),
                ("w".into(), Some(1), Some(2), Confidence::Good),
            ]
        );
    }

    #[test]
    fn numbers_and_titles_win_when_lengths_are_unknown() {
        let r = release("a", &[ALBUM]);
        let mut files = vec![
            file("b", None, Some(2), "Paper Boats", 0),
            file("a", None, Some(1), "Morning Glory", 0),
            file("c", None, None, "lanterns", 0),
        ];
        files[2].duration_ms = None;
        let a = assign(&files, &r);
        assert_eq!(a.matches[0].track, Some(2));
        assert_eq!(a.matches[0].confidence, Confidence::Good);
        assert_eq!(a.matches[2].track, Some(3));
        assert_eq!(a.matches[2].confidence, Confidence::Check);
        assert!(a.matches[2].note.contains("couldn't be measured"));
    }

    #[test]
    fn a_missing_track_is_reported() {
        let r = release("a", &[ALBUM]);
        let files = vec![
            file("1", None, Some(1), "Morning Glory", 245_000),
            file("2", None, Some(2), "Paper Boats", 181_500),
            file("4", None, Some(4), "The Long Road Home", 411_000),
        ];
        let a = assign(&files, &r);
        assert_eq!(a.matches[2].track, Some(4));
        assert!(a.matches.iter().all(|m| m.confidence == Confidence::Good));
        assert_eq!(a.missing.len(), 1);
        assert_eq!(a.missing[0].title, "Lanterns");
        assert_eq!(a.fit.summary, "3 of your 3 files fit · 4 tracks");
        assert!(a.fit.score < 95 && a.fit.score > 70, "{}", a.fit.score);
    }

    #[test]
    fn a_file_far_off_any_track_is_not_assigned() {
        let r = release("a", &[ALBUM]);
        let files = vec![
            file("1", None, Some(1), "Morning Glory", 245_000),
            file("2", None, Some(2), "Paper Boats", 181_000),
            file("3", None, Some(3), "Lanterns", 302_000),
            file("bonus", None, Some(5), "Hidden Track", 95_000),
        ];
        let a = assign(&files, &r);
        let bonus = &a.matches[3];
        assert_eq!(bonus.confidence, Confidence::None);
        assert!(bonus.note.contains("away from"), "{}", bonus.note);
        assert_eq!(a.missing.len(), 1, "its track stays free");
        // More files than tracks: the extra one gets no track at all.
        let mut more = files.clone();
        more.push(file("4", None, Some(4), "The Long Road Home", 412_000));
        more.push(file("6", None, Some(6), "Outro", 412_000));
        let a = assign(&more, &r);
        let unplaced: Vec<&FileMatch> = a
            .matches
            .iter()
            .filter(|m| m.confidence == Confidence::None)
            .collect();
        assert_eq!(unplaced.len(), 2);
        assert!(unplaced.iter().any(|m| m.track.is_none()));
    }

    #[test]
    fn two_discs_use_the_folder_disc_number() {
        // Both discs have a 3:00 track 2, so only the disc tells them apart.
        let r = release(
            "a",
            &[
                &[("One", 200_000), ("Two", 180_000)],
                &[("Uno", 240_000), ("Dos", 180_500)],
            ],
        );
        let files = vec![
            file("d2t2", Some(2), Some(2), "Dos", 180_400),
            file("d1t1", Some(1), Some(1), "One", 200_100),
            file("d2t1", Some(2), Some(1), "Uno", 240_000),
            file("d1t2", Some(1), Some(2), "Two", 180_100),
        ];
        let a = assign(&files, &r);
        assert_eq!(
            placed(&a),
            vec![
                ("d2t2".into(), Some(2), Some(2), Confidence::Good),
                ("d1t1".into(), Some(1), Some(1), Confidence::Good),
                ("d2t1".into(), Some(2), Some(1), Confidence::Good),
                ("d1t2".into(), Some(1), Some(2), Confidence::Good),
            ]
        );
        // Numbered straight through (1–4) with no disc: still right.
        let through = vec![
            file("a", None, Some(3), "", 240_000),
            file("b", None, Some(4), "", 180_400),
            file("c", None, Some(1), "", 200_000),
            file("d", None, Some(2), "", 180_000),
        ];
        let a = assign(&through, &r);
        assert_eq!(a.matches[0].disc, Some(2));
        assert_eq!(a.matches[0].track, Some(1));
        assert_eq!(a.matches[1].disc, Some(2));
        assert_eq!(a.matches[1].track, Some(2));
        assert_eq!(a.matches[3].disc, Some(1));
    }

    #[test]
    fn duration_ranking_picks_the_right_pressing() {
        // Same tracks, same count; the remaster's lengths differ.
        let original = release("original", &[ALBUM]);
        let remaster = release(
            "remaster",
            &[&[
                ("Morning Glory", 262_000),
                ("Paper Boats", 199_000),
                ("Lanterns", 318_000),
                ("The Long Road Home", 431_000),
            ]],
        );
        let files: Vec<FileInfo> = ALBUM
            .iter()
            .enumerate()
            .map(|(i, (t, ms))| file(&i.to_string(), None, Some(i as u32 + 1), t, ms + 700))
            .collect();
        let good = assign(&files, &original).fit;
        let bad = assign(&files, &remaster).fit;
        assert!(good.score > bad.score + 30, "{good:?} vs {bad:?}");
        assert!(bad.none > 0, "lengths 15–20 s off are not assigned");
        assert!(count_fit(4, 4).score < good.score);
        assert!(count_fit(4, 4).score > count_fit(4, 6).score);
    }

    #[test]
    fn same_length_tracks_are_flagged_without_other_clues() {
        let r = release("a", &[&[("Alpha", 200_000), ("Beta", 200_500)]]);
        let files = vec![
            file("p", None, None, "", 200_100),
            file("q", None, None, "", 200_400),
        ];
        let a = assign(&files, &r);
        assert!(a.matches.iter().all(|m| m.confidence == Confidence::Check));
        assert!(a.matches[0].note.contains("same length"));
    }

    #[test]
    fn a_contradicting_track_number_asks_for_a_check() {
        let r = release("a", &[ALBUM]);
        let files = vec![file("1", None, Some(3), "Something Else", 181_000)];
        let a = assign(&files, &r);
        assert_eq!(a.matches[0].track, Some(2));
        assert_eq!(a.matches[0].confidence, Confidence::Check);
        assert!(a.matches[0].note.contains("track 3"));
    }

    #[test]
    fn hungarian_finds_the_cheapest_assignment() {
        let cost = vec![vec![4, 1, 3], vec![2, 0, 5], vec![3, 2, 2]];
        assert_eq!(hungarian(&cost), vec![Some(1), Some(0), Some(2)]);
        let wide = vec![vec![9, 1, 9, 9]];
        assert_eq!(hungarian(&wide), vec![Some(1)]);
        let tall = vec![vec![5], vec![1], vec![3]];
        assert_eq!(hungarian(&tall), vec![None, Some(0), None]);
        assert_eq!(hungarian(&[]), Vec::<Option<usize>>::new());
    }

    #[test]
    fn title_keys_ignore_filler() {
        assert_eq!(file_title_key("Track 07"), None);
        assert_eq!(file_title_key("07"), None);
        assert_eq!(file_title_key("Audio Track 3"), None);
        assert_eq!(
            file_title_key("Paper Boats (Live)").as_deref(),
            Some("paper boats live")
        );
        assert!(title_similarity("paper boats live", "paper boats") > 0.8);
        assert!(title_similarity("lanterns", "morning glory") < 0.3);
        assert_eq!(gap(4_200), "4 seconds");
        let named = FileInfo {
            name: "CD1/03 - Paper Boats.flac".into(),
            ..Default::default()
        };
        assert_eq!(title_key_of(&named).as_deref(), Some("paper boats"));
        let bare = FileInfo {
            name: "03.flac".into(),
            ..Default::default()
        };
        assert_eq!(title_key_of(&bare), None);
        assert_eq!(gap(72_000), "1:12");
    }
}
