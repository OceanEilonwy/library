//! Rules for digital copies: which files the upload studio accepts and what
//! they are, stored file names, and subtitles (SubRip to WebVTT).
//!
//! Nothing is transcoded: media files are stored byte for byte. The one
//! conversion is rewriting `.srt` subtitle text as WebVTT, which browsers
//! understand.

use rand::Rng;

use crate::db::media_files::MediaKind;

/// Lower-case extension of a file name, without the dot.
pub fn extension(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = base.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > 8 {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// The file name without folders or extension.
pub fn stem(name: &str) -> &str {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => base,
    }
}

/// The kinds a file extension can be, and the content type served for each.
fn kinds_for(ext: &str) -> &'static [(MediaKind, &'static str)] {
    use MediaKind::{Audio, Document, Video};
    match ext {
        "mp3" => &[(Audio, "audio/mpeg")],
        "m4a" => &[(Audio, "audio/mp4")],
        "aac" => &[(Audio, "audio/aac")],
        "flac" => &[(Audio, "audio/flac")],
        "ogg" | "oga" | "opus" => &[(Audio, "audio/ogg")],
        "wav" => &[(Audio, "audio/wav")],
        "webm" => &[(Video, "video/webm"), (Audio, "audio/webm")],
        "mp4" | "m4v" => &[(Video, "video/mp4")],
        "mkv" => &[(Video, "video/x-matroska")],
        "mov" => &[(Video, "video/quicktime")],
        "pdf" => &[(Document, "application/pdf")],
        "epub" => &[(Document, "application/epub+zip")],
        _ => &[],
    }
}

/// Extensions the studio accepts, for messages and the file picker.
pub const ACCEPTED: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "wav", "webm", "mp4", "m4v", "mkv", "mov",
    "pdf", "epub",
];

/// The content type stored for an archive file (kept for the record, only
/// ever downloaded by the librarian).
fn archive_mime(ext: &str) -> &'static str {
    match ext {
        "log" | "txt" | "cue" | "md5" | "sfv" | "accurip" | "m3u" | "m3u8" => "text/plain",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        _ => kinds_for(ext)
            .first()
            .map_or("application/octet-stream", |(_, mime)| mime),
    }
}

/// Decides what an uploaded file is from its name, the kind the browser
/// guessed (if any) and the content type it sent. Returns the kind and the
/// content type to store, or a plain-English reason to refuse it. Any file
/// may be kept as an archive file when that kind is asked for.
pub fn classify(
    name: &str,
    wanted: Option<MediaKind>,
    browser_mime: &str,
) -> Result<(MediaKind, &'static str), String> {
    let ext = extension(name).unwrap_or_default();
    if wanted == Some(MediaKind::Archive) {
        return Ok((MediaKind::Archive, archive_mime(&ext)));
    }
    let options = kinds_for(&ext);
    if options.is_empty() {
        return Err(format!(
            "“{}” isn't a file the Player can use. Upload music (MP3, M4A, FLAC, Ogg, Opus, WAV), \
             films (MP4, M4V, WebM, MKV, MOV) or documents (PDF, EPUB).",
            name.rsplit(['/', '\\']).next().unwrap_or(name)
        ));
    }
    let mime = browser_mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let plausible = mime.is_empty()
        || mime == "application/octet-stream"
        || options.iter().any(|(kind, _)| match kind {
            MediaKind::Audio | MediaKind::Video => {
                mime.starts_with("audio/") || mime.starts_with("video/")
            }
            MediaKind::Document => mime.starts_with("application/"),
            MediaKind::Archive => true,
        });
    if !plausible {
        return Err(format!(
            "That file says it is {mime}, which doesn't match its .{ext} name."
        ));
    }
    // A .webm can be either; the browser's own guess settles it.
    let from_mime = if mime.starts_with("audio/") {
        Some(MediaKind::Audio)
    } else if mime.starts_with("video/") {
        Some(MediaKind::Video)
    } else {
        None
    };
    let pick = wanted
        .and_then(|k| options.iter().find(|(o, _)| *o == k))
        .or_else(|| from_mime.and_then(|k| options.iter().find(|(o, _)| *o == k)))
        .or_else(|| options.first());
    match (pick, wanted) {
        (Some(&(kind, _)), Some(w)) if kind != w => Err(format!(
            "A .{ext} file can't be {}.",
            match w {
                MediaKind::Audio => "music",
                MediaKind::Video => "a film",
                MediaKind::Document => "a document",
                MediaKind::Archive => "an archive file",
            }
        )),
        (Some(&(kind, mime)), _) => Ok((kind, mime)),
        (None, _) => Err("That file type isn't supported.".into()),
    }
}

/// A fresh random stored name with the given extension.
pub fn random_name(ext: &str) -> String {
    let mut rng = rand::thread_rng();
    let stem: String = (0..24)
        .map(|_| format!("{:x}", rng.gen_range(0..16u8)))
        .collect();
    let ext: String = ext
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

// ---------- subtitles ----------

/// Reads subtitle bytes as text: UTF-8 (with or without a byte-order mark),
/// or Latin-1 for older files that aren't UTF-8.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// Turns an uploaded subtitle file into WebVTT text.
pub fn to_webvtt(name: &str, bytes: &[u8]) -> Result<String, String> {
    let text = decode_text(bytes);
    match extension(name).as_deref() {
        Some("srt") => {
            let vtt = srt_to_vtt(&text);
            if vtt.trim() == "WEBVTT" {
                Err("That subtitle file has no timings in it.".into())
            } else {
                Ok(vtt)
            }
        }
        Some("vtt") => {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            if text.trim_start_matches('\u{feff}').starts_with("WEBVTT") {
                Ok(text)
            } else {
                Err("That .vtt file doesn't start with WEBVTT, so browsers won't read it.".into())
            }
        }
        _ => Err("Subtitles should be .srt or .vtt files.".into()),
    }
}

/// Converts SubRip (`.srt`) text to WebVTT. Cue numbers are kept as cue
/// identifiers, commas in timings become dots, SubRip position hints and
/// `{\…}` styling tags are dropped.
pub fn srt_to_vtt(srt: &str) -> String {
    let text = srt
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = String::from("WEBVTT\n");
    let mut in_cue = false;
    let mut pending_id: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_cue = false;
            pending_id = None;
            continue;
        }
        if !in_cue {
            if let Some(timing) = convert_timing(trimmed) {
                out.push('\n');
                if let Some(id) = pending_id.take() {
                    out.push_str(id);
                    out.push('\n');
                }
                out.push_str(&timing);
                out.push('\n');
                in_cue = true;
            } else {
                pending_id = Some(trimmed);
            }
            continue;
        }
        out.push_str(&strip_style_tags(line).replace("-->", "→"));
        out.push('\n');
    }
    out
}

fn convert_timing(line: &str) -> Option<String> {
    let (start, end) = line.split_once("-->")?;
    let start = timestamp(start)?;
    let end = timestamp(end.split_whitespace().next()?)?;
    Some(format!("{start} --> {end}"))
}

/// "0:01:02,5" → "00:01:02.500"
fn timestamp(s: &str) -> Option<String> {
    let s = s.trim();
    let (hms, frac) = match s.split_once([',', '.']) {
        Some((a, b)) => (a, b),
        None => (s, "0"),
    };
    if !frac.chars().all(|c| c.is_ascii_digit()) || frac.is_empty() {
        return None;
    }
    let ms: u32 = format!("{frac:0<3}").get(..3)?.parse().ok()?;
    let parts: Vec<u32> = hms
        .split(':')
        .map(|p| {
            if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                None
            } else {
                p.parse().ok()
            }
        })
        .collect::<Option<_>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return None,
    };
    if m > 59 || sec > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}:{sec:02}.{ms:03}"))
}

fn strip_style_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find("{\\") {
        out.push_str(&rest[..open]);
        match rest[open..].find('}') {
            Some(close) => rest = &rest[open + close + 1..],
            None => {
                rest = &rest[open..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Normalises a language tag ("EN", "eng", "pt-br" → "en", "en", "pt-BR");
/// "und" when there's nothing usable.
pub fn clean_language(raw: &str) -> String {
    let raw = raw.trim().replace('_', "-");
    let mut parts = raw.split('-');
    let primary = parts.next().unwrap_or("").to_ascii_lowercase();
    if primary.is_empty() || primary.len() > 8 || !primary.chars().all(|c| c.is_ascii_alphabetic())
    {
        return "und".into();
    }
    let primary = match primary.as_str() {
        "eng" => "en",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "dut" | "nld" => "nl",
        "pol" => "pl",
        "swe" => "sv",
        "dan" => "da",
        "nor" | "nob" => "no",
        "fin" => "fi",
        "jpn" => "ja",
        "chi" | "zho" => "zh",
        "kor" => "ko",
        "rus" => "ru",
        "ara" => "ar",
        "heb" => "he",
        "gre" | "ell" => "el",
        "tur" => "tr",
        "cze" | "ces" => "cs",
        "hun" => "hu",
        "wel" | "cym" => "cy",
        "gle" => "ga",
        other => other,
    }
    .to_owned();
    match parts.next() {
        Some(region)
            if (2..=3).contains(&region.len())
                && region.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            format!("{primary}-{}", region.to_ascii_uppercase())
        }
        _ => primary,
    }
}

/// An English name for a language tag, for the subtitle picker.
pub fn language_label(tag: &str) -> String {
    let primary = tag.split('-').next().unwrap_or(tag);
    let name = match primary {
        "en" => "English",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "pl" => "Polish",
        "sv" => "Swedish",
        "da" => "Danish",
        "no" | "nb" | "nn" => "Norwegian",
        "fi" => "Finnish",
        "ja" => "Japanese",
        "zh" => "Chinese",
        "ko" => "Korean",
        "ru" => "Russian",
        "ar" => "Arabic",
        "he" => "Hebrew",
        "el" => "Greek",
        "tr" => "Turkish",
        "cs" => "Czech",
        "hu" => "Hungarian",
        "cy" => "Welsh",
        "ga" => "Irish",
        "und" => return "Subtitles".into(),
        other => return other.to_owned(),
    };
    name.to_owned()
}

/// Guesses a subtitle's language from its file name: `Film.en.srt` → `en`.
pub fn guess_language(name: &str) -> Option<String> {
    let stem = stem(name);
    let (_, tag) = stem.rsplit_once('.')?;
    let tag = clean_language(tag);
    (tag != "und" && language_label(&tag) != tag).then_some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension_and_browser_type() {
        assert_eq!(
            classify("01 - Song.FLAC", None, "audio/flac"),
            Ok((MediaKind::Audio, "audio/flac"))
        );
        assert_eq!(
            classify("Film.mkv", None, ""),
            Ok((MediaKind::Video, "video/x-matroska"))
        );
        assert_eq!(
            classify("talk.webm", None, "audio/webm"),
            Ok((MediaKind::Audio, "audio/webm"))
        );
        assert_eq!(
            classify("clip.webm", None, "application/octet-stream"),
            Ok((MediaKind::Video, "video/webm"))
        );
        assert_eq!(
            classify("book.pdf", Some(MediaKind::Document), "application/pdf"),
            Ok((MediaKind::Document, "application/pdf"))
        );
        assert_eq!(
            classify("rip.log", Some(MediaKind::Archive), "text/plain"),
            Ok((MediaKind::Archive, "text/plain"))
        );
        assert_eq!(
            classify("rip.json", Some(MediaKind::Archive), ""),
            Ok((MediaKind::Archive, "application/json"))
        );
        assert_eq!(
            classify("master.mkv", Some(MediaKind::Archive), "video/x-matroska"),
            Ok((MediaKind::Archive, "video/x-matroska"))
        );
        assert_eq!(
            classify("odd.bin", Some(MediaKind::Archive), ""),
            Ok((MediaKind::Archive, "application/octet-stream"))
        );
        assert!(
            classify("rip.log", None, "text/plain").is_err(),
            "only kept as an archive when asked"
        );
        assert!(classify("setup.exe", None, "").is_err());
        assert!(classify("noext", None, "").is_err());
        assert!(
            classify("song.mp3", None, "text/html").is_err(),
            "lying type"
        );
        assert!(classify("song.mp3", Some(MediaKind::Video), "").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(extension("CD2/04. Title.M4A").as_deref(), Some("m4a"));
        assert_eq!(extension(".hidden"), None);
        assert_eq!(stem("CD2/04. Title.m4a"), "04. Title");
        let n = random_name("mp3");
        assert!(n.ends_with(".mp3") && crate::media::is_safe_name(&n));
        assert_ne!(random_name("mp3"), n);
    }

    #[test]
    fn srt_becomes_webvtt() {
        let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:04,500 X1:10 X2:20\r\n{\\an8}Hello there.\r\n<i>Hi!</i>\r\n\r\n\r\n2\r\n0:01:02,5 --> 0:01:03,25\r\nIt goes --> there\r\n";
        assert_eq!(
            srt_to_vtt(srt),
            "WEBVTT\n\n1\n00:00:01.000 --> 00:00:04.500\nHello there.\n<i>Hi!</i>\n\n2\n00:01:02.500 --> 00:01:03.250\nIt goes → there\n"
        );
        assert_eq!(timestamp("12:34"), Some("00:12:34.000".into()));
        assert_eq!(timestamp("1:2:3:4"), None);
        assert_eq!(timestamp("00:61:00,000"), None);
    }

    #[test]
    fn subtitle_files() {
        assert!(to_webvtt("a.vtt", b"WEBVTT\r\n\r\n00:01.000 --> 00:02.000\r\nHi").is_ok());
        assert!(to_webvtt("a.vtt", b"not vtt").is_err());
        assert!(to_webvtt("a.srt", b"nothing here").is_err());
        assert!(to_webvtt("a.txt", b"WEBVTT").is_err());
        // Latin-1 text survives.
        let vtt = to_webvtt("a.srt", b"1\n00:00:01,000 --> 00:00:02,000\nCaf\xe9\n").unwrap();
        assert!(vtt.contains("Café"));
    }

    #[test]
    fn languages() {
        assert_eq!(clean_language("ENG"), "en");
        assert_eq!(clean_language("pt_br"), "pt-BR");
        assert_eq!(clean_language(""), "und");
        assert_eq!(clean_language("e1"), "und");
        assert_eq!(language_label("pt-BR"), "Portuguese");
        assert_eq!(language_label("und"), "Subtitles");
        assert_eq!(guess_language("Film.en.srt").as_deref(), Some("en"));
        assert_eq!(guess_language("Film.fre.srt").as_deref(), Some("fr"));
        assert_eq!(guess_language("Film.srt"), None);
        assert_eq!(guess_language("Mr. Smith.srt"), None);
    }
}
