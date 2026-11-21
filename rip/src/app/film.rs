// SPDX-License-Identifier: AGPL-3.0-or-later
//! Film discs (stage 2): what the Disc screen knows about one, and the rip
//! that copies titles with MakeMKV, makes viewing copies with ffmpeg and
//! writes the log and `rip.json` that go up with the master.
//!
//! The upload is the same [`UploadPlan`] the audio rip makes, so sending,
//! resuming and the Done screen are shared.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use super::pipeline::safe_file_name;
use super::settings::Settings;
use super::{AppError, Result};
use crate::encode::sha256_file;
use crate::model::{ItemTarget, MediaKind, NewItem, Sha256Digest, UploadFile, UploadPlan};
use crate::video::VideoError;
use crate::video::ffmpeg::Ffmpeg;
use crate::video::makemkv::MakeMkv;
use crate::video::scan::{DiscScan, FilmTitle, length, size};

/// Whether this computer can make the WebM viewing copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewingSupport {
    /// `ffmpeg` can; this is its AV1 encoder.
    Available {
        /// `libsvtav1` or `libaom-av1`.
        encoder: String,
    },
    /// It cannot, and why, in plain English.
    Unavailable {
        /// What is missing.
        reason: String,
    },
}

impl ViewingSupport {
    /// From the result of looking for `ffmpeg`.
    pub fn from_check(found: &Result<Ffmpeg, VideoError>) -> Self {
        match found {
            Ok(ffmpeg) => ViewingSupport::Available {
                encoder: ffmpeg.video_encoder.to_owned(),
            },
            Err(VideoError::FfmpegMissing) => ViewingSupport::Unavailable {
                reason: "ffmpeg is not installed.".to_owned(),
            },
            Err(VideoError::NoEncoders(what)) => ViewingSupport::Unavailable {
                reason: format!("The ffmpeg on this computer is missing something: {what}."),
            },
            Err(other) => ViewingSupport::Unavailable {
                reason: format!("ffmpeg could not be used: {other}."),
            },
        }
    }
}

/// A film disc read by MakeMKV.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilmScan {
    /// MakeMKV's `disc:N`.
    pub disc_index: u32,
    /// The drive as MakeMKV names it.
    pub drive_name: String,
    /// The disc and its titles.
    pub disc: DiscScan,
    /// Whether a viewing copy can be made.
    pub viewing: ViewingSupport,
}

/// Everything a film rip needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilmJob {
    /// The drive's device.
    pub device: String,
    /// MakeMKV's `disc:N`.
    pub disc_index: u32,
    /// What the disc and its titles are.
    pub disc: DiscScan,
    /// The film's name, as confirmed.
    pub name: String,
    /// The year, if typed.
    pub year: Option<u16>,
    /// The titles to copy, in order.
    pub titles: Vec<FilmTitle>,
    /// Make the WebM viewing copy (false: archive the master only).
    pub viewing_copy: bool,
    /// Local copy and the like.
    pub settings: Settings,
}

/// Progress of a film rip. `title` is MakeMKV's title id.
#[derive(Clone, Debug, PartialEq)]
pub enum FilmEvent {
    /// MakeMKV is copying a title.
    Copying {
        /// Which.
        title: usize,
        /// 0.0–1.0.
        fraction: f32,
    },
    /// ffmpeg is making a title's viewing copy.
    Viewing {
        /// Which.
        title: usize,
        /// 0.0–1.0.
        fraction: f32,
    },
    /// A title has everything it will have.
    Finished {
        /// Which.
        title: usize,
    },
}

/// The result of a film rip (before upload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilmOutput {
    /// Folder holding the masters, viewing copies, log and `rip.json`.
    pub folder: PathBuf,
    /// The copy in the "keep" folder, when one is set.
    pub kept: Option<PathBuf>,
    /// What to upload.
    pub plan: UploadPlan,
    /// Remarks for the Done screen.
    pub notes: Vec<String>,
}

/// How a film rip finds its tools and where the item goes.
pub struct FilmTools<'a> {
    /// MakeMKV.
    pub makemkv: &'a MakeMkv,
    /// ffmpeg, when there is one that works.
    pub ffmpeg: Option<&'a Ffmpeg>,
    /// The library's slug for films.
    pub type_slug: &'a str,
    /// MakeMKV's version line, for the log.
    pub makemkv_version: Option<String>,
}

/// One title once it is copied.
struct Copied {
    title: FilmTitle,
    stem: String,
    master: PathBuf,
    master_sha: Sha256Digest,
    viewing: Option<(PathBuf, Sha256Digest)>,
}

/// Copies the titles, makes the viewing copies, writes the log and
/// `rip.json`, and lays out the upload.
pub async fn rip_film(
    job: &FilmJob,
    tools: &FilmTools<'_>,
    work_dir: &Path,
    cancel: &AtomicBool,
    events: &(dyn Fn(FilmEvent) + Send + Sync),
) -> Result<FilmOutput> {
    std::fs::create_dir_all(work_dir).map_err(|e| AppError::io(work_dir, e))?;
    let stem = safe_file_name(&job.name);
    let many = job.titles.len() > 1;
    let make_viewing = job.viewing_copy && tools.ffmpeg.is_some();
    let mut copied: Vec<Copied> = Vec::new();
    let mut notes = Vec::new();

    for (n, title) in job.titles.iter().enumerate() {
        let id = title.id;
        let file_stem = if many {
            format!("{stem} (title {})", n + 1)
        } else {
            stem.clone()
        };
        let mut progress = |p: crate::video::makemkv::CopyProgress| {
            events(FilmEvent::Copying {
                title: id,
                fraction: p.fraction,
            });
        };
        let written = tools
            .makemkv
            .copy_title(job.disc_index, id, work_dir, cancel, &mut progress)
            .await?;
        let master = work_dir.join(format!("{file_stem}.mkv"));
        std::fs::rename(&written, &master).map_err(|e| AppError::io(&master, e))?;
        events(FilmEvent::Copying {
            title: id,
            fraction: 1.0,
        });

        let mut viewing = None;
        if make_viewing && let Some(ffmpeg) = tools.ffmpeg {
            let output = work_dir.join(format!("{file_stem}.webm"));
            let mut progress = |fraction: f32| events(FilmEvent::Viewing { title: id, fraction });
            ffmpeg
                .make_viewing_copy(
                    &master,
                    &output,
                    &display_title(&job.name, title, many),
                    title.duration_secs,
                    cancel,
                    &mut progress,
                )
                .await?;
            viewing = Some(output);
        }
        let master_sha = blocking_sha(&master).await?;
        let viewing = match viewing {
            Some(path) => {
                let sha = blocking_sha(&path).await?;
                Some((path, sha))
            }
            None => None,
        };
        events(FilmEvent::Finished { title: id });
        copied.push(Copied {
            title: title.clone(),
            stem: file_stem,
            master,
            master_sha,
            viewing,
        });
    }

    if !make_viewing {
        notes.push(if job.viewing_copy {
            "No viewing copy was made because ffmpeg could not be used, so the Player has nothing \
             to play yet. The master is safely archived on the item page."
                .to_owned()
        } else {
            "Only the master was archived, so the Player has nothing to play yet.".to_owned()
        });
    }

    let log_path = work_dir.join(format!("{stem}.rip.log"));
    let json_path = work_dir.join(format!("{stem}.rip.json"));
    let log = rip_log(job, tools, &copied, make_viewing);
    std::fs::write(&log_path, log).map_err(|e| AppError::io(&log_path, e))?;
    let json = rip_json(job, tools, &copied, make_viewing)?;
    std::fs::write(&json_path, json).map_err(|e| AppError::io(&json_path, e))?;

    let mut files = Vec::new();
    for c in &copied {
        if let Some((path, sha)) = &c.viewing {
            files.push(UploadFile {
                kind: MediaKind::Video,
                title: display_title(&job.name, &c.title, many),
                disc: 1,
                track: None,
                duration_ms: Some(c.title.duration_secs * 1000),
                sha256: *sha,
                path: path.clone(),
            });
        }
    }
    for c in &copied {
        files.push(UploadFile {
            kind: MediaKind::Archive,
            title: format!("Master (lossless MKV): {}", c.stem),
            disc: 1,
            track: None,
            duration_ms: None,
            sha256: c.master_sha,
            path: c.master.clone(),
        });
    }
    for (path, title) in [
        (&log_path, "Rip log".to_owned()),
        (&json_path, "Rip details (rip.json)".to_owned()),
    ] {
        files.push(UploadFile {
            kind: MediaKind::Archive,
            title,
            disc: 1,
            track: None,
            duration_ms: None,
            sha256: blocking_sha(path).await?,
            path: path.clone(),
        });
    }

    let kept = match &job.settings.keep_local_copy {
        Some(keep) => Some(keep_film_copy(keep, &job.name, &files)?),
        None => None,
    };
    let item = NewItem {
        type_slug: tools.type_slug.to_owned(),
        title: job.name.clone(),
        year: job.year,
        tags: Vec::new(),
        ..NewItem::default()
    };
    Ok(FilmOutput {
        folder: work_dir.to_owned(),
        kept,
        plan: UploadPlan {
            item: ItemTarget::New(item),
            cover: None,
            files,
        },
        notes,
    })
}

fn display_title(name: &str, title: &FilmTitle, many: bool) -> String {
    if many && !title.name.trim().is_empty() {
        format!("{name} — {}", title.name.trim())
    } else {
        name.to_owned()
    }
}

async fn blocking_sha(path: &Path) -> Result<Sha256Digest> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|e| AppError::Task(e.to_string()))?
        .map_err(AppError::from)
}

/// Copies the rip into `<keep>/Films/<Name>`, never over an earlier copy.
pub fn keep_film_copy(keep: &Path, name: &str, files: &[UploadFile]) -> Result<PathBuf> {
    let parent = keep.join("Films");
    let base = safe_file_name(name);
    let mut folder = parent.join(&base);
    let mut attempt = 2;
    while folder.exists() {
        folder = parent.join(format!("{base} ({attempt})"));
        attempt += 1;
    }
    std::fs::create_dir_all(&folder).map_err(|e| AppError::io(&folder, e))?;
    for file in files {
        let Some(file_name) = file.path.file_name() else {
            continue;
        };
        let destination = folder.join(file_name);
        std::fs::copy(&file.path, &destination).map_err(|e| AppError::io(&destination, e))?;
    }
    Ok(folder)
}

fn rip_log(job: &FilmJob, tools: &FilmTools<'_>, copied: &[Copied], viewing: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("Library Rip {} film rip log\n", crate::VERSION));
    out.push_str(&format!(
        "Made {}\n\n",
        jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S %Z")
    ));
    out.push_str(&format!("Film: {}\n", job.name));
    if let Some(year) = job.year {
        out.push_str(&format!("Year: {year}\n"));
    }
    out.push_str(&format!(
        "Disc: {} ({}), volume {}\n",
        job.disc.name, job.disc.kind, job.disc.volume
    ));
    out.push_str(&format!("Copied with: {}", tools.makemkv.describe()));
    if let Some(version) = &tools.makemkv_version {
        out.push_str(&format!(" ({version})"));
    }
    out.push('\n');
    out.push_str("Master: lossless MKV, exactly as MakeMKV read it (nothing re-encoded).\n");
    match (viewing, tools.ffmpeg) {
        (true, Some(ffmpeg)) => out.push_str(&format!(
            "Viewing copy: WebM, {} video (never above 1080p, never scaled up), Opus stereo 128 kbit/s, made with {}\n",
            ffmpeg.video_encoder, ffmpeg.version
        )),
        _ => out.push_str("Viewing copy: none made.\n"),
    }
    for c in copied {
        out.push_str(&format!(
            "\nTitle {} ({}): {} · {} · {} chapters\n",
            c.title.id,
            c.title.name,
            length(c.title.duration_secs),
            size(c.title.size_bytes),
            c.title.chapters
        ));
        for s in &c.title.streams {
            out.push_str(&format!(
                "  {}: {} {} {}\n",
                s.kind, s.language, s.codec, s.detail
            ));
        }
        out.push_str(&format!(
            "  master {}  SHA-256 {}\n",
            file_name(&c.master),
            c.master_sha.to_hex()
        ));
        if let Some((path, sha)) = &c.viewing {
            out.push_str(&format!(
                "  viewing copy {}  SHA-256 {}\n",
                file_name(path),
                sha.to_hex()
            ));
        }
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonFile {
    file: String,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonTitle<'a> {
    #[serde(flatten)]
    title: &'a FilmTitle,
    master: JsonFile,
    viewing_copy: Option<JsonFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonRip<'a> {
    format_version: u32,
    kind: &'static str,
    ripper_version: &'static str,
    name: &'a str,
    year: Option<u16>,
    disc: &'a DiscScan,
    copied_with: String,
    viewing_copy_encoder: Option<&'static str>,
    titles: Vec<JsonTitle<'a>>,
}

fn rip_json(
    job: &FilmJob,
    tools: &FilmTools<'_>,
    copied: &[Copied],
    viewing: bool,
) -> Result<String> {
    let json = JsonRip {
        format_version: 1,
        kind: "film",
        ripper_version: crate::VERSION,
        name: &job.name,
        year: job.year,
        disc: &job.disc,
        copied_with: tools.makemkv.describe(),
        viewing_copy_encoder: tools
            .ffmpeg
            .filter(|_| viewing)
            .map(|ffmpeg| ffmpeg.video_encoder),
        titles: copied
            .iter()
            .map(|c| JsonTitle {
                title: &c.title,
                master: JsonFile {
                    file: file_name(&c.master),
                    sha256: c.master_sha.to_hex(),
                },
                viewing_copy: c.viewing.as_ref().map(|(path, sha)| JsonFile {
                    file: file_name(path),
                    sha256: sha.to_hex(),
                }),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&json).map_err(|e| AppError::Task(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    #[test]
    fn viewing_support_explains_what_is_missing() {
        assert!(matches!(
            ViewingSupport::from_check(&Err(VideoError::FfmpegMissing)),
            ViewingSupport::Unavailable { reason } if reason.contains("not installed")
        ));
        assert!(matches!(
            ViewingSupport::from_check(&Err(VideoError::NoEncoders("no AV1".into()))),
            ViewingSupport::Unavailable { reason } if reason.contains("no AV1")
        ));
    }

    #[test]
    fn a_kept_copy_never_overwrites_an_earlier_one() {
        let keep = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let path = src.path().join("a.rip.log");
        std::fs::write(&path, "x").unwrap();
        let file = UploadFile {
            kind: MediaKind::Archive,
            title: "Rip log".into(),
            disc: 1,
            track: None,
            duration_ms: None,
            sha256: Sha256Digest([0; 32]),
            path,
        };
        let first = keep_film_copy(keep.path(), "A: Film", std::slice::from_ref(&file)).unwrap();
        let second = keep_film_copy(keep.path(), "A: Film", std::slice::from_ref(&file)).unwrap();
        assert_ne!(first, second);
        assert!(first.join("a.rip.log").is_file());
        assert!(second.ends_with("A_ Film (2)"));
    }

    #[cfg(unix)]
    mod with_a_stand_in_makemkv {
        use super::*;
        use crate::video::scan::DiscScan;

        fn fake() -> MakeMkv {
            MakeMkv::at(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/fake-makemkvcon"))
        }

        fn disc() -> DiscScan {
            DiscScan::from_lines(
                include_str!("../../tests/data/makemkv-info-dvd.txt").lines(),
            )
        }

        fn job(titles: &[usize], viewing_copy: bool) -> FilmJob {
            let disc = disc();
            FilmJob {
                device: "/dev/fake-sr0".into(),
                disc_index: 0,
                titles: disc
                    .titles
                    .iter()
                    .filter(|t| titles.contains(&t.id))
                    .cloned()
                    .collect(),
                disc,
                name: "The Library Card".into(),
                year: Some(1999),
                viewing_copy,
                settings: Settings::default(),
            }
        }

        #[tokio::test]
        async fn the_stand_in_is_found_scanned_and_copied() {
            let never = AtomicBool::new(false);
            let mk = fake();
            let drives = mk.drives(&never).await.unwrap();
            assert_eq!(drives.len(), 1);
            let drive = MakeMkv::pick_drive(&drives, "/dev/sr0").unwrap();
            let scan = mk.scan(drive.index, &never).await.unwrap();
            assert_eq!(scan.name, "THE_LIBRARY_CARD");
            assert_eq!(scan.titles.len(), 3);
            assert_eq!(mk.version(&never).await.as_deref(), Some("MakeMKV v2.0.0 linux(x64-release)"));

            let dir = tempfile::tempdir().unwrap();
            let seen = Mutex::new(Vec::new());
            let mut progress = |p: crate::video::makemkv::CopyProgress| seen.lock().unwrap().push(p.fraction);
            let file = mk.copy_title(0, 0, dir.path(), &never, &mut progress).await.unwrap();
            assert!(file.ends_with("THE_LIBRARY_CARD_t00.mkv"));
            assert!(std::fs::metadata(&file).unwrap().len() > 0);
            assert_eq!(seen.lock().unwrap().last(), Some(&1.0));
        }

        #[tokio::test]
        async fn a_master_only_rip_lays_out_the_upload() {
            let work = tempfile::tempdir().unwrap();
            let mk = fake();
            let tools = FilmTools {
                makemkv: &mk,
                ffmpeg: None,
                type_slug: "film",
                makemkv_version: Some("MakeMKV v2.0.0".into()),
            };
            let events = Mutex::new(Vec::new());
            let cancel = AtomicBool::new(false);
            let record = |e| events.lock().unwrap().push(e);
            let out = rip_film(&job(&[0], true), &tools, &work.path().join("rip"), &cancel, &record)
                .await
                .unwrap();
            // No ffmpeg was given: the master only, and the user is told.
            let kinds: Vec<_> = out.plan.files.iter().map(|f| (f.kind, f.title.clone())).collect();
            assert_eq!(
                kinds,
                [
                    (MediaKind::Archive, "Master (lossless MKV): The Library Card".to_owned()),
                    (MediaKind::Archive, "Rip log".to_owned()),
                    (MediaKind::Archive, "Rip details (rip.json)".to_owned()),
                ]
            );
            assert!(out.notes[0].contains("No viewing copy"));
            let ItemTarget::New(item) = &out.plan.item else {
                panic!("a new item");
            };
            assert_eq!((item.type_slug.as_str(), item.title.as_str(), item.year), ("film", "The Library Card", Some(1999)));
            for file in &out.plan.files {
                assert_eq!(sha256_file(&file.path).unwrap(), file.sha256);
            }
            let json = std::fs::read_to_string(out.folder.join("The Library Card.rip.json")).unwrap();
            let json: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(json["kind"], "film");
            assert_eq!(json["titles"][0]["id"], 0);
            assert_eq!(json["titles"][0]["streams"][1]["language"], "English");
            assert_eq!(
                json["titles"][0]["master"]["sha256"],
                out.plan.files[0].sha256.to_hex()
            );
            let log = std::fs::read_to_string(out.folder.join("The Library Card.rip.log")).unwrap();
            assert!(log.contains("Viewing copy: none made."));
            assert!(log.contains("THE_LIBRARY_CARD"));
            assert!(events.lock().unwrap().contains(&FilmEvent::Finished { title: 0 }));
        }

        #[tokio::test]
        async fn a_cancelled_rip_stops() {
            let work = tempfile::tempdir().unwrap();
            let mk = fake();
            let tools = FilmTools {
                makemkv: &mk,
                ffmpeg: None,
                type_slug: "film",
                makemkv_version: None,
            };
            let cancel = AtomicBool::new(true);
            cancel.store(true, Ordering::Relaxed);
            let result = rip_film(&job(&[0], false), &tools, &work.path().join("rip"), &cancel, &|_| {}).await;
            assert!(matches!(result, Err(e) if e.is_cancelled()));
        }

        #[tokio::test]
        async fn the_viewing_copy_is_a_webm_that_ffmpeg_can_read_back() {
            let Ok(ffmpeg) = Ffmpeg::discover().await else {
                eprintln!("skipped: no ffmpeg with AV1 and Opus here");
                return;
            };
            let work = tempfile::tempdir().unwrap();
            let mk = fake();
            let tools = FilmTools {
                makemkv: &mk,
                ffmpeg: Some(&ffmpeg),
                type_slug: "film",
                makemkv_version: None,
            };
            let mut one = job(&[0], true);
            // The stand-in's clip is two seconds, whatever the scan says.
            one.titles[0].duration_secs = 2;
            let events = Mutex::new(Vec::new());
            let record = |e| events.lock().unwrap().push(e);
            let out = rip_film(&one, &tools, &work.path().join("rip"), &AtomicBool::new(false), &record)
                .await
                .unwrap();
            let video = &out.plan.files[0];
            assert_eq!(video.kind, MediaKind::Video);
            assert!(video.path.extension().is_some_and(|e| e == "webm"));
            assert_eq!(video.duration_ms, Some(2000));
            // What ffprobe says about it: AV1 video, Opus sound, square pixels.
            let probe = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "stream=codec_name,sample_aspect_ratio,width,height", "-of", "csv=p=0"])
                .arg(&video.path)
                .output();
            if let Ok(probe) = probe {
                let text = String::from_utf8_lossy(&probe.stdout);
                assert!(text.contains("av1"), "{text}");
                assert!(text.contains("opus"), "{text}");
                // 720x480 with 32:27 pixels is 16:9: 853 or 854 wide, square.
                assert!(text.contains("480") && !text.contains("720,480"), "{text}");
            }
            assert!(events.lock().unwrap().iter().any(|e| matches!(e, FilmEvent::Viewing { .. })));
            assert!(out.notes.is_empty());
        }
    }
}
