// SPDX-License-Identifier: AGPL-3.0-or-later
//! What `makemkvcon -r info` says about a disc, as a disc and its titles.

use std::collections::BTreeMap;

use serde::Serialize;

use super::robot::{self, Line, attr};

/// One stream (picture, sound or subtitles) of a title.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilmStream {
    /// `video`, `audio` or `subtitles`.
    pub kind: String,
    /// Language name, when known.
    pub language: String,
    /// Short codec name (`DD`, `Mpeg2`).
    pub codec: String,
    /// Video size (`720x480`) or audio channels (`5.1`).
    pub detail: String,
}

/// One title on the disc: a film, an episode, an extra, a menu.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilmTitle {
    /// MakeMKV's title number (what `mkv disc:N <id>` takes).
    pub id: usize,
    /// MakeMKV's name for it.
    pub name: String,
    /// Length.
    pub duration_secs: u64,
    /// Size on the disc.
    pub size_bytes: u64,
    /// Chapters.
    pub chapters: u32,
    /// The disc file it comes from.
    pub source: String,
    /// Picture, sound and subtitles.
    pub streams: Vec<FilmStream>,
}

impl FilmTitle {
    /// "1 h 42 min · 7.3 GB · 24 chapters · 720x480 · English 5.1, French 2.0".
    pub fn summary(&self) -> String {
        let mut parts = vec![length(self.duration_secs), size(self.size_bytes)];
        if self.chapters > 0 {
            parts.push(match self.chapters {
                1 => "1 chapter".to_owned(),
                n => format!("{n} chapters"),
            });
        }
        if let Some(video) = self.streams.iter().find(|s| s.kind == "video")
            && !video.detail.is_empty()
        {
            parts.push(video.detail.clone());
        }
        let audio: Vec<String> = self
            .streams
            .iter()
            .filter(|s| s.kind == "audio")
            .map(|s| {
                [s.language.as_str(), s.detail.as_str()]
                    .iter()
                    .filter(|p| !p.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty())
            .collect();
        if !audio.is_empty() {
            parts.push(audio.join(", "));
        }
        parts.join(" · ")
    }
}

/// "1 h 42 min", "7 min", "45 s".
pub fn length(secs: u64) -> String {
    match (secs / 3600, secs % 3600 / 60) {
        (0, 0) => format!("{secs} s"),
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m:02} min"),
    }
}

/// "7.3 GB", "640 MB".
pub fn size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

/// A disc and its titles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscScan {
    /// The disc's name as MakeMKV reads it.
    pub name: String,
    /// The disc's volume label.
    pub volume: String,
    /// What it is ("DVD disc", "Blu-ray disc").
    pub kind: String,
    /// The titles, in disc order.
    pub titles: Vec<FilmTitle>,
}

impl DiscScan {
    /// Builds a scan from robot lines (any other lines are ignored).
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut scan = DiscScan::default();
        let mut titles: BTreeMap<usize, FilmTitle> = BTreeMap::new();
        let mut streams: BTreeMap<(usize, usize), FilmStream> = BTreeMap::new();
        for line in lines {
            match robot::parse_line(line) {
                Some(Line::DiscInfo { id, value }) => match id {
                    attr::NAME => scan.name = value,
                    attr::VOLUME_NAME => scan.volume = value,
                    attr::TYPE => scan.kind = value,
                    _ => {}
                },
                Some(Line::TitleInfo { title, id, value }) => {
                    let t = titles.entry(title).or_insert_with(|| FilmTitle {
                        id: title,
                        ..FilmTitle::default()
                    });
                    match id {
                        attr::NAME => t.name = value,
                        attr::DURATION => {
                            t.duration_secs = robot::parse_duration(&value).unwrap_or(0)
                        }
                        attr::SIZE_BYTES => t.size_bytes = value.trim().parse().unwrap_or(0),
                        attr::CHAPTERS => t.chapters = value.trim().parse().unwrap_or(0),
                        attr::SOURCE_FILE => t.source = value,
                        _ => {}
                    }
                }
                Some(Line::StreamInfo {
                    title,
                    stream,
                    id,
                    value,
                }) => {
                    let s = streams.entry((title, stream)).or_default();
                    match id {
                        attr::TYPE => s.kind = stream_kind(&value),
                        attr::LANG_NAME => s.language = value,
                        attr::CODEC_SHORT => s.codec = value,
                        attr::VIDEO_SIZE => s.detail = value,
                        attr::CHANNEL_LAYOUT => s.detail = value,
                        attr::CHANNELS if s.detail.is_empty() && s.kind == "audio" => {
                            s.detail = format!("{value} channels");
                        }
                        _ => {}
                    }
                    titles.entry(title).or_insert_with(|| FilmTitle {
                        id: title,
                        ..FilmTitle::default()
                    });
                }
                _ => {}
            }
        }
        for ((title, _), stream) in streams {
            if let Some(t) = titles.get_mut(&title) {
                t.streams.push(stream);
            }
        }
        scan.titles = titles.into_values().collect();
        scan
    }

    /// The title most likely to be the film: the longest.
    pub fn main_title(&self) -> Option<&FilmTitle> {
        self.titles
            .iter()
            .max_by_key(|t| (t.duration_secs, t.size_bytes))
    }
}

fn stream_kind(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "video" => "video",
        "audio" => "audio",
        "subtitles" | "subtitle" => "subtitles",
        other => return other.to_owned(),
    }
    .to_owned()
}

/// A disc name made readable: `THE_BIG_FILM` becomes "The Big Film".
/// Names that already have lower case or spaces are left alone.
pub fn clean_name(name: &str) -> String {
    let name = name.trim();
    let has_lower = name.chars().any(char::is_lowercase);
    let spaced = name.replace(['_', '.'], " ");
    let spaced = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if has_lower {
        return spaced;
    }
    spaced
        .split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../tests/data/makemkv-info-dvd.txt");

    #[test]
    fn a_dvd_scan_gives_titles_with_streams() {
        let scan = DiscScan::from_lines(SAMPLE.lines());
        assert_eq!(scan.name, "THE_LIBRARY_CARD");
        assert_eq!(scan.kind, "DVD disc");
        assert_eq!(scan.titles.len(), 3);
        let main = scan.main_title().unwrap();
        assert_eq!(main.id, 0);
        assert_eq!(main.duration_secs, 6150);
        assert_eq!(main.size_bytes, 7_821_123_584);
        assert_eq!(main.chapters, 24);
        assert_eq!(main.streams.len(), 4);
        assert_eq!(main.streams[0].kind, "video");
        assert_eq!(main.streams[0].detail, "720x480");
        assert_eq!(main.streams[1].language, "English");
        assert_eq!(main.streams[1].detail, "5.1");
        assert_eq!(main.streams[3].kind, "subtitles");
        assert_eq!(
            main.summary(),
            "1 h 42 min · 7.8 GB · 24 chapters · 720x480 · English 5.1, French 2.0"
        );
    }

    #[test]
    fn titles_that_are_only_streams_still_appear() {
        let scan = DiscScan::from_lines(["SINFO:2,0,1,6201,\"Video\""]);
        assert_eq!(scan.titles.len(), 1);
        assert_eq!(scan.titles[0].id, 2);
    }

    #[test]
    fn names_are_cleaned_for_people() {
        assert_eq!(clean_name("THE_LIBRARY_CARD"), "The Library Card");
        assert_eq!(clean_name("Already Nice_Name"), "Already Nice Name");
        assert_eq!(clean_name("  "), "");
        assert_eq!(length(45), "45 s");
        assert_eq!(length(7 * 60), "7 min");
        assert_eq!(size(640_000_000), "640 MB");
    }
}
