// SPDX-License-Identifier: AGPL-3.0-or-later
//! `makemkvcon -r` ("robot mode") output.
//!
//! Every line is `KIND:field,field,…` with text fields in double quotes
//! (a backslash escapes the next character). The kinds used here:
//!
//! | Line | Fields |
//! |---|---|
//! | `MSG` | code, flags, parameter count, message, format, parameters… |
//! | `PRGV` | current, total, maximum (progress values) |
//! | `PRGC` | code, id, name (the current job's title) |
//! | `DRV` | index, visible, enabled, flags, drive name, disc name, device |
//! | `TCOUNT` | number of titles |
//! | `CINFO` | attribute, code, value (about the disc) |
//! | `TINFO` | title, attribute, code, value |
//! | `SINFO` | title, stream, attribute, code, value |

/// One parsed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A message for the user.
    Msg {
        /// MakeMKV's message code.
        code: u32,
        /// The message text.
        text: String,
    },
    /// Progress values.
    Progress {
        /// Done in the current job.
        current: u64,
        /// Done in the whole run.
        total: u64,
        /// What "all done" is.
        max: u64,
    },
    /// What the current job is called.
    JobTitle {
        /// The name.
        name: String,
    },
    /// A drive slot (many are empty).
    Drive {
        /// MakeMKV's disc index.
        index: u32,
        /// 256 and over means the slot is empty; 2 means a disc is loaded.
        visible: u32,
        /// The drive's name.
        name: String,
        /// The disc's name.
        disc: String,
        /// The device (`/dev/sr0`, `D:`).
        device: String,
    },
    /// How many titles the disc has.
    TitleCount(usize),
    /// A fact about the disc.
    DiscInfo {
        /// The attribute id.
        id: u32,
        /// The value.
        value: String,
    },
    /// A fact about a title.
    TitleInfo {
        /// The title.
        title: usize,
        /// The attribute id.
        id: u32,
        /// The value.
        value: String,
    },
    /// A fact about one stream of a title.
    StreamInfo {
        /// The title.
        title: usize,
        /// The stream.
        stream: usize,
        /// The attribute id.
        id: u32,
        /// The value.
        value: String,
    },
}

/// Attribute ids (`apdefs.h` in MakeMKV's open-source part).
pub mod attr {
    /// Type (also the stream kind: "Video", "Audio", "Subtitles").
    pub const TYPE: u32 = 1;
    /// Name.
    pub const NAME: u32 = 2;
    /// Language code (`eng`).
    pub const LANG_CODE: u32 = 3;
    /// Language name.
    pub const LANG_NAME: u32 = 4;
    /// Short codec name.
    pub const CODEC_SHORT: u32 = 6;
    /// Chapter count.
    pub const CHAPTERS: u32 = 8;
    /// Duration, `h:mm:ss`.
    pub const DURATION: u32 = 9;
    /// Size in bytes.
    pub const SIZE_BYTES: u32 = 11;
    /// Audio channel count.
    pub const CHANNELS: u32 = 14;
    /// Source file name.
    pub const SOURCE_FILE: u32 = 16;
    /// Video size, `720x480`.
    pub const VIDEO_SIZE: u32 = 19;
    /// Video aspect ratio.
    pub const ASPECT: u32 = 20;
    /// Video frame rate.
    pub const FRAME_RATE: u32 = 21;
    /// Name of the output file.
    pub const OUTPUT_FILE: u32 = 27;
    /// Volume name of the disc.
    pub const VOLUME_NAME: u32 = 32;
    /// Audio channel layout name (`5.1`).
    pub const CHANNEL_LAYOUT: u32 = 40;
}

/// Splits a line's fields: commas outside quotes separate, quotes group,
/// a backslash makes the next character literal.
fn fields(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
        } else if c == '\\' && quoted {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ',' && !quoted {
            out.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    out.push(current);
    out
}

/// Parses one output line; `None` for lines that are not robot output or
/// that this app does not use.
pub fn parse_line(line: &str) -> Option<Line> {
    let (kind, rest) = line.trim_end().split_once(':')?;
    let f = fields(rest);
    let number = |i: usize| f.get(i).and_then(|s| s.trim().parse::<u64>().ok());
    let text = |i: usize| f.get(i).cloned().unwrap_or_default();
    match kind {
        "MSG" => Some(Line::Msg {
            code: u32::try_from(number(0)?).ok()?,
            text: text(3),
        }),
        "PRGV" => Some(Line::Progress {
            current: number(0)?,
            total: number(1)?,
            max: number(2)?,
        }),
        "PRGC" => Some(Line::JobTitle { name: text(2) }),
        "DRV" => Some(Line::Drive {
            index: u32::try_from(number(0)?).ok()?,
            visible: u32::try_from(number(1)?).ok()?,
            name: text(4),
            disc: text(5),
            device: text(6),
        }),
        "TCOUNT" => Some(Line::TitleCount(usize::try_from(number(0)?).ok()?)),
        "CINFO" => Some(Line::DiscInfo {
            id: u32::try_from(number(0)?).ok()?,
            value: text(2),
        }),
        "TINFO" => Some(Line::TitleInfo {
            title: usize::try_from(number(0)?).ok()?,
            id: u32::try_from(number(1)?).ok()?,
            value: text(3),
        }),
        "SINFO" => Some(Line::StreamInfo {
            title: usize::try_from(number(0)?).ok()?,
            stream: usize::try_from(number(1)?).ok()?,
            id: u32::try_from(number(2)?).ok()?,
            value: text(4),
        }),
        _ => None,
    }
}

/// `1:42:30` or `42:30` as seconds.
pub fn parse_duration(text: &str) -> Option<u64> {
    let mut seconds = 0u64;
    for part in text.trim().split(':') {
        seconds = seconds
            .checked_mul(60)?
            .checked_add(part.trim().parse().ok()?)?;
    }
    Some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_fields_keep_commas_and_escaped_quotes() {
        assert_eq!(
            fields(r#"1005,0,1,"MakeMKV v2.0.0, \"x\" started","%1 started""#),
            [
                "1005",
                "0",
                "1",
                "MakeMKV v2.0.0, \"x\" started",
                "%1 started"
            ]
        );
    }

    #[test]
    fn lines_parse() {
        assert_eq!(
            parse_line(r#"MSG:5010,0,0,"Failed to open disc","Failed to open disc""#),
            Some(Line::Msg {
                code: 5010,
                text: "Failed to open disc".into()
            })
        );
        assert_eq!(
            parse_line("PRGV:1024,2048,65536"),
            Some(Line::Progress {
                current: 1024,
                total: 2048,
                max: 65536
            })
        );
        assert_eq!(
            parse_line(r#"DRV:0,2,999,12,"BD-RE LG","MY_FILM","/dev/sr0""#),
            Some(Line::Drive {
                index: 0,
                visible: 2,
                name: "BD-RE LG".into(),
                disc: "MY_FILM".into(),
                device: "/dev/sr0".into()
            })
        );
        assert_eq!(
            parse_line(r#"TINFO:3,9,0,"1:42:30""#),
            Some(Line::TitleInfo {
                title: 3,
                id: 9,
                value: "1:42:30".into()
            })
        );
        assert_eq!(
            parse_line(r#"SINFO:0,1,4,0,"English""#),
            Some(Line::StreamInfo {
                title: 0,
                stream: 1,
                id: 4,
                value: "English".into()
            })
        );
        assert_eq!(parse_line("TCOUNT:7"), Some(Line::TitleCount(7)));
        assert_eq!(parse_line("not robot output"), None);
        assert_eq!(parse_line("PRGV:x,y,z"), None);
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("1:42:30"), Some(6150));
        assert_eq!(parse_duration("0:00:07"), Some(7));
        assert_eq!(parse_duration("42:30"), Some(2550));
        assert_eq!(parse_duration("soon"), None);
    }
}
