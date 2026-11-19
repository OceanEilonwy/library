// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rip pipeline: drive → core → encode → library.
//!
//! [`rip`]:
//! 1. reads the disc info and computes the disc ids;
//! 2. finds the read offset (the user's, else the AccurateRip drive list);
//! 3. reads every audio track securely ([`crate::drive::reader`]) while the
//!    AccurateRip and CTDB entries are fetched in the background;
//! 4. verifies with AccurateRip and CTDB;
//! 5. encodes the FLACs and writes the cue sheet, log and `rip.json` into
//!    the work folder (and a copy into the "keep" folder when set);
//! 6. returns an [`UploadPlan`].
//!
//! [`upload`] then sends the plan to the library, skipping files whose
//! SHA-256 is already on the item, so an interrupted upload resumes.

use std::collections::HashSet;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use url::Url;

use super::settings::Settings;
use super::{AppError, Result};
use crate::core::accuraterip::{self, DatabaseEntry, TrackCrcState, TrackPosition};
use crate::core::ctdb::{self, CtdbEntry};
use crate::core::{CoreError, cue, discid, log, offset, report_json};
use crate::drive::reader::{ReadOptions, ReadProgress, read_track};
use crate::drive::{DiscDrive, DriveError};
use crate::encode::{FlacMetadata, encode_flac, sha256_file, tags};
use crate::library::LibraryClient;
use crate::library::client::UploadProgress;
use crate::model::{
    AccurateRipCrcs, AccurateRipStatus, CtdbStatus, DiscIds, DiscInfo, DriveIdentity, ItemTarget,
    MediaKind, OffsetSource, Pcm, ReleaseMetadata, RipReport, SampleOffset, Sectors, Sha256Digest,
    Toc, TrackEntry, TrackMetadata, TrackNumber, TrackReadResult, TrackReport, UploadFile,
    UploadPlan,
};
use crate::musicbrainz::{self, MusicBrainzClient};

/// Where the outside services live. Tests point these at local mock servers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// MusicBrainz web service (`…/ws/2/`).
    pub musicbrainz: Url,
    /// Cover Art Archive.
    pub cover_art: Url,
    /// AccurateRip database base.
    pub accuraterip: Url,
    /// AccurateRip drive offset list.
    pub drive_offsets: Url,
    /// CUETools database base.
    pub ctdb: Url,
}

impl Endpoints {
    /// The public services.
    pub fn official() -> Result<Self, url::ParseError> {
        Ok(Endpoints {
            musicbrainz: Url::parse(musicbrainz::DEFAULT_BASE)?,
            cover_art: Url::parse(musicbrainz::DEFAULT_COVER_BASE)?,
            accuraterip: Url::parse(accuraterip::DEFAULT_BASE)?,
            drive_offsets: Url::parse(offset::DEFAULT_DRIVE_DATABASE)?,
            ctdb: Url::parse(ctdb::DEFAULT_BASE)?,
        })
    }

    /// The public services, each replaced by its `LIBRARY_RIP_*_URL`
    /// environment variable when set (for testing against
    /// `library-rip mock-services`; the headless runner takes the same
    /// variables as options).
    pub fn from_environment() -> Result<Self, url::ParseError> {
        Self::with_overrides(|name| std::env::var(name).ok())
    }

    /// [`Endpoints::official`] with overrides looked up by variable name.
    pub fn with_overrides(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, url::ParseError> {
        let official = Self::official()?;
        let pick = |name: &str, default: Url| -> Result<Url, url::ParseError> {
            match lookup(name).filter(|value| !value.trim().is_empty()) {
                Some(value) => Url::parse(value.trim()),
                None => Ok(default),
            }
        };
        Ok(Endpoints {
            musicbrainz: pick("LIBRARY_RIP_MUSICBRAINZ_URL", official.musicbrainz)?,
            cover_art: pick("LIBRARY_RIP_COVER_ART_URL", official.cover_art)?,
            accuraterip: pick("LIBRARY_RIP_ACCURATERIP_URL", official.accuraterip)?,
            drive_offsets: pick("LIBRARY_RIP_DRIVE_OFFSETS_URL", official.drive_offsets)?,
            ctdb: pick("LIBRARY_RIP_CTDB_URL", official.ctdb)?,
        })
    }
}

/// External services used while ripping, injectable for tests.
#[derive(Clone, Debug)]
pub struct Services {
    /// HTTP client for AccurateRip and CTDB.
    pub http: reqwest::Client,
    /// MusicBrainz and the Cover Art Archive.
    pub musicbrainz: MusicBrainzClient,
    /// AccurateRip base (see `core::accuraterip::DEFAULT_BASE`).
    pub accuraterip_base: Url,
    /// AccurateRip drive offset database URL.
    pub drive_offsets_url: Url,
    /// CTDB base.
    pub ctdb_base: Url,
    /// Whole-request limit for each AccurateRip, CTDB and drive-list
    /// request ([`crate::core::SERVICE_TIMEOUT`]; tests shorten it).
    pub request_timeout: Duration,
}

impl Services {
    /// Services at `endpoints`, sharing one HTTP client.
    pub fn new(http: reqwest::Client, endpoints: &Endpoints) -> Self {
        Services {
            musicbrainz: MusicBrainzClient::new(
                http.clone(),
                endpoints.musicbrainz.clone(),
                endpoints.cover_art.clone(),
            ),
            http,
            accuraterip_base: endpoints.accuraterip.clone(),
            drive_offsets_url: endpoints.drive_offsets.clone(),
            ctdb_base: endpoints.ctdb.clone(),
            request_timeout: crate::core::SERVICE_TIMEOUT,
        }
    }
}

/// Progress reported to the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum PipelineEvent {
    /// The read offset that will be applied.
    Offset {
        /// Samples to add.
        offset: SampleOffset,
        /// Where it came from.
        source: OffsetSource,
    },
    /// Reading a track.
    Reading {
        /// Which track.
        track: TrackNumber,
        /// Pass number (2 = test pass in test-and-copy).
        pass: u8,
        /// Fraction of the pass done, 0.0–1.0.
        fraction: f32,
        /// Read speed as a multiple of playing speed ("8.0×").
        speed: f32,
        /// Sectors that needed re-reading so far.
        retried_sectors: u32,
    },
    /// A track finished reading.
    TrackRead {
        /// Which track.
        track: TrackNumber,
        /// Sectors that never verified.
        unrecovered_sectors: usize,
        /// Test-and-copy only: whether the two reads matched.
        test_matched: Option<bool>,
    },
    /// AccurateRip and CTDB results.
    Verified {
        /// Per audio track, in order.
        tracks: Vec<(TrackNumber, AccurateRipStatus)>,
        /// Disc-wide.
        ctdb: CtdbStatus,
    },
    /// Encoding a track.
    Encoding {
        /// Which track.
        track: TrackNumber,
    },
    /// The item exists on the library (created, or the one picked).
    ItemReady {
        /// Item id.
        id: i64,
        /// Item page.
        url: String,
    },
    /// Uploading a file of the plan.
    Uploading {
        /// Index into [`UploadPlan::files`].
        index: usize,
        /// Fraction done, 0.0–1.0.
        fraction: f32,
    },
    /// A file of the plan is on the library.
    Uploaded {
        /// Index into [`UploadPlan::files`].
        index: usize,
        /// It was already there (same SHA-256), so it was not sent again.
        skipped: bool,
    },
}

/// The result of a rip (before upload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RipOutput {
    /// Folder holding the FLACs, cue, log and `rip.json`.
    pub folder: PathBuf,
    /// The copy in the "keep" folder, when one is set.
    pub kept: Option<PathBuf>,
    /// The report.
    pub report: RipReport,
    /// What to upload.
    pub plan: UploadPlan,
}

impl RipOutput {
    /// Read problems that make this rip "with errors" (never hidden).
    pub fn read_issues(&self) -> Vec<String> {
        read_issues(&self.report)
    }

    /// Verification results worth pointing out (not read errors).
    pub fn verification_notes(&self) -> Vec<String> {
        verification_notes(&self.report)
    }
}

/// The result of an upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadOutput {
    /// Item id.
    pub item_id: i64,
    /// Item page.
    pub item_url: String,
    /// Files uploaded (skipped duplicates not counted).
    pub uploaded: usize,
    /// Files already on the item.
    pub skipped: usize,
    /// Set when the cover could not be added (the files are fine).
    pub cover_problem: Option<String>,
}

/// Everything a rip needs besides the drive.
#[derive(Debug)]
pub struct RipRequest<'a> {
    /// Metadata confirmed by the user.
    pub release: &'a ReleaseMetadata,
    /// Where the files will go.
    pub target: ItemTarget,
    /// Offset, read mode, local copy.
    pub settings: &'a Settings,
    /// External services.
    pub services: &'a Services,
    /// Folder to write the rip into.
    pub work_dir: PathBuf,
    /// Set to stop the rip.
    pub cancel: Arc<AtomicBool>,
}

/// Reads, verifies and encodes the disc in `drive` into `request.work_dir`.
/// Blocking parts run on `spawn_blocking`.
pub async fn rip(
    drive: Box<dyn DiscDrive>,
    request: RipRequest<'_>,
    events: &(dyn Fn(PipelineEvent) + Send + Sync),
) -> Result<RipOutput> {
    let RipRequest {
        release,
        target,
        settings,
        services,
        work_dir,
        cancel,
    } = request;
    let started_at = jiff::Timestamp::now();
    std::fs::create_dir_all(&work_dir).map_err(|e| AppError::io(&work_dir, e))?;

    // 1. Disc info and ids.
    let (drive, disc) = disc_info(drive).await?;
    let audio: Vec<TrackEntry> = disc.toc.audio_tracks().cloned().collect();
    if audio.is_empty() {
        return Err(DriveError::NoAudio.into());
    }
    let disc_ids = discid::disc_ids(&disc.toc)?;

    // 2. Offset.
    let (read_offset, offset_source) =
        cancellable(resolve_offset(services, settings, &disc.drive), &cancel).await?;
    events(PipelineEvent::Offset {
        offset: read_offset,
        source: offset_source,
    });
    check(&cancel)?;

    // 3. Read, while the databases are asked.
    let mut verification = tokio::spawn(fetch_verification(
        services.clone(),
        disc_ids.clone(),
        disc.toc.clone(),
    ));
    let pcm_dir = work_dir.join(".pcm");
    std::fs::create_dir_all(&pcm_dir).map_err(|e| AppError::io(&pcm_dir, e))?;
    let (sender, receiver) = mpsc::unbounded_channel();
    let job = ReadJob {
        tracks: audio.clone(),
        lead_out: Sectors(disc.toc.lead_out().0),
        options: ReadOptions {
            mode: settings.read_mode,
            offset: read_offset,
            cancel: Arc::clone(&cancel),
        },
        pcm_dir: pcm_dir.clone(),
    };
    let reading = tokio::task::spawn_blocking(move || read_all(drive, &job, &sender));
    forward(receiver, events).await;
    let reads = match reading.await {
        Ok(Ok(reads)) => reads,
        Ok(Err(error)) => {
            verification.abort();
            remove_quietly(&pcm_dir);
            return Err(error);
        }
        Err(error) => {
            verification.abort();
            remove_quietly(&pcm_dir);
            return Err(AppError::Task(error.to_string()));
        }
    };
    let fetched = match cancellable(&mut verification, &cancel).await {
        Ok(fetched) => fetched,
        Err(error) => {
            verification.abort();
            remove_quietly(&pcm_dir);
            return Err(error);
        }
    };
    let (accuraterip_entries, ctdb_entries) =
        fetched.map_err(|error| AppError::Task(error.to_string()))?;
    check(&cancel)?;

    // 4. Verify.
    let crcs = {
        let reads = reads.clone();
        blocking(move || accuraterip_crcs(&reads)).await??
    };
    let accuraterip_statuses = match &accuraterip_entries {
        Ok(entries) => accuraterip::verify(&crcs, entries),
        Err(reason) => crcs
            .iter()
            .map(|(number, _)| {
                (
                    *number,
                    AccurateRipStatus::Unavailable {
                        reason: reason.clone(),
                    },
                )
            })
            .collect(),
    };
    let ctdb_status = {
        let reads = reads.clone();
        let toc = disc.toc.clone();
        blocking(move || ctdb_status(&reads, &toc, ctdb_entries)).await??
    };
    events(PipelineEvent::Verified {
        tracks: accuraterip_statuses.clone(),
        ctdb: ctdb_status.clone(),
    });

    // 5. Encode.
    let mut track_reports = Vec::with_capacity(reads.len());
    let mut audio_files = Vec::with_capacity(reads.len());
    for (index, (read, entry)) in reads.iter().zip(&audio).enumerate() {
        check(&cancel)?;
        events(PipelineEvent::Encoding { track: read.track });
        let metadata = track_metadata(release, entry.number, index);
        let file_name = flac_file_name(entry.number, &metadata.title);
        let output = work_dir.join(&file_name);
        let flac = FlacMetadata {
            comments: tags::vorbis_comments(release, &metadata),
            cover: release.cover.clone(),
        };
        let pcm = read.pcm.clone();
        let encoded = {
            let output = output.clone();
            blocking(move || encode_flac(&pcm, &output, &flac)).await??
        };
        let (crcs_for_track, status) = verification_for(read.track, &crcs, &accuraterip_statuses);
        track_reports.push(TrackReport {
            number: entry.number,
            title: metadata.title.clone(),
            start_lba: entry.start_lba,
            length_sectors: entry.length_sectors,
            pregap: entry.pregap,
            isrc: entry.isrc.clone().or_else(|| metadata.isrc.clone()),
            peak: read.peak,
            copy_crc: read.copy_crc,
            test_crc: read.test_crc,
            accuraterip_crcs: crcs_for_track,
            accuraterip: status,
            unrecovered_sectors: read.unrecovered_sectors.clone(),
            retried_sectors: read.retried_sectors,
            file_name: file_name.clone(),
            file_sha256: Some(encoded.sha256),
        });
        audio_files.push(UploadFile {
            kind: MediaKind::Audio,
            title: metadata.title,
            disc: release.disc_number.max(1),
            track: Some(entry.number),
            duration_ms: Some(read.samples.duration_ms()),
            sha256: encoded.sha256,
            path: encoded.path,
        });
    }
    remove_quietly(&pcm_dir);

    // Cue sheet, log, rip.json.
    let report = RipReport {
        app_version: crate::VERSION.to_owned(),
        started_at,
        finished_at: jiff::Timestamp::now(),
        drive: disc.drive.clone(),
        read_offset,
        offset_source,
        read_mode: settings.read_mode,
        toc: disc.toc.clone(),
        disc_ids,
        mcn: disc.mcn.clone(),
        release: release.clone(),
        tracks: track_reports,
        ctdb: ctdb_status,
    };
    let base_name = album_file_stem(release);
    let file_names: Vec<String> = report.tracks.iter().map(|t| t.file_name.clone()).collect();
    let cue_text = cue::render(&cue::CueInput {
        disc: &disc,
        release,
        file_names: &file_names,
    })?;
    let log_text = log::render(&report)?;
    let json_text = report_json::render(&report)?;
    let mut archive_files = Vec::with_capacity(3);
    for (name, text, title) in [
        (format!("{base_name}.log"), log_text, "Rip log"),
        (format!("{base_name}.cue"), cue_text, "Cue sheet"),
        (
            report_json::FILE_NAME.to_owned(),
            json_text,
            "Rip report (rip.json)",
        ),
    ] {
        let path = work_dir.join(name);
        std::fs::write(&path, text).map_err(|e| AppError::io(&path, e))?;
        archive_files.push(UploadFile {
            kind: MediaKind::Archive,
            title: title.to_owned(),
            disc: release.disc_number.max(1),
            track: None,
            duration_ms: None,
            sha256: sha256_file(&path)?,
            path,
        });
    }

    let mut files = audio_files;
    files.extend(archive_files);
    let kept = match &settings.keep_local_copy {
        Some(keep) => Some(keep_copy(keep, release, &files)?),
        None => None,
    };
    Ok(RipOutput {
        folder: work_dir,
        kept,
        report,
        plan: UploadPlan {
            item: target,
            cover: release.cover.clone(),
            files,
        },
    })
}

/// Uploads a plan to the library (resumable: skips files already there).
///
/// Sends [`PipelineEvent::ItemReady`] as soon as the item exists, so a
/// caller retrying after a failure can switch the plan to
/// [`ItemTarget::Existing`] and not create a second item.
pub async fn upload(
    plan: &UploadPlan,
    library: &LibraryClient,
    cancel: &AtomicBool,
    events: &(dyn Fn(PipelineEvent) + Send + Sync),
) -> Result<UploadOutput> {
    let (item_id, item_url, is_new) = match &plan.item {
        ItemTarget::New(item) => {
            let created = cancellable(library.create_item(item), cancel).await??;
            (created.id, created.url, true)
        }
        ItemTarget::Existing { id } => (*id, item_page_url(library.base(), *id), false),
    };
    events(PipelineEvent::ItemReady {
        id: item_id,
        url: item_url.clone(),
    });

    let existing: HashSet<Sha256Digest> = if is_new {
        HashSet::new()
    } else {
        cancellable(library.existing_media(item_id), cancel)
            .await??
            .into_iter()
            .collect()
    };

    // The cover goes on new items only, and its failure does not stop the
    // archive: the files matter more than the picture.
    let mut cover_problem = None;
    if is_new
        && let Some(cover) = &plan.cover
        && let Err(error) = cancellable(library.upload_photo(item_id, cover), cancel).await?
    {
        tracing::warn!(%error, "could not upload the cover");
        cover_problem = Some(AppError::from(error).problem().message);
    }

    let mut uploaded = 0;
    let mut skipped = 0;
    for (index, file) in plan.files.iter().enumerate() {
        check(cancel)?;
        if existing.contains(&file.sha256) {
            skipped += 1;
            events(PipelineEvent::Uploaded {
                index,
                skipped: true,
            });
            continue;
        }
        let progress = |progress: UploadProgress| {
            events(PipelineEvent::Uploading {
                index,
                fraction: fraction(progress.sent, progress.total),
            });
        };
        cancellable(library.upload_media(item_id, file, &progress), cancel).await??;
        uploaded += 1;
        events(PipelineEvent::Uploaded {
            index,
            skipped: false,
        });
    }
    Ok(UploadOutput {
        item_id,
        item_url,
        uploaded,
        skipped,
        cover_problem,
    })
}

/// The item page on the library (`/items/{id}`).
pub fn item_page_url(base: &Url, id: i64) -> String {
    base.join(&format!("items/{id}"))
        .map_or_else(|_| format!("{base}items/{id}"), String::from)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

struct ReadJob {
    tracks: Vec<TrackEntry>,
    lead_out: Sectors,
    options: ReadOptions,
    pcm_dir: PathBuf,
}

/// Reads every track (blocking), reporting progress through `sender`.
fn read_all(
    mut drive: Box<dyn DiscDrive>,
    job: &ReadJob,
    sender: &mpsc::UnboundedSender<PipelineEvent>,
) -> Result<Vec<TrackReadResult>> {
    let mut results = Vec::with_capacity(job.tracks.len());
    for track in &job.tracks {
        let path = job
            .pcm_dir
            .join(format!("track{:02}.pcm", track.number.get()));
        let mut pass_started = (0u8, Instant::now());
        let mut on_progress = |progress: ReadProgress| {
            if pass_started.0 != progress.pass {
                pass_started = (progress.pass, Instant::now());
            }
            let seconds = pass_started.1.elapsed().as_secs_f32();
            let audio_seconds =
                progress.sectors_done.0 as f32 / crate::model::SECTORS_PER_SECOND as f32;
            let speed = if seconds > 0.0 {
                audio_seconds / seconds
            } else {
                0.0
            };
            // A closed channel only means nobody is watching.
            let _ = sender.send(PipelineEvent::Reading {
                track: track.number,
                pass: progress.pass,
                fraction: fraction(
                    u64::from(progress.sectors_done.0),
                    u64::from(progress.sectors_total.0),
                ),
                speed,
                retried_sectors: progress.retried_sectors,
            });
        };
        let result = read_track(
            &mut *drive,
            track,
            job.lead_out,
            &job.options,
            &path,
            &mut on_progress,
        )
        .map_err(|error| match error {
            DriveError::Cancelled => AppError::Cancelled,
            other => AppError::Drive(other),
        })?;
        let _ = sender.send(PipelineEvent::TrackRead {
            track: track.number,
            unrecovered_sectors: result.unrecovered_sectors.len(),
            test_matched: result.test_crc.map(|test| test == result.copy_crc),
        });
        results.push(result);
    }
    Ok(results)
}

async fn forward(
    mut receiver: mpsc::UnboundedReceiver<PipelineEvent>,
    events: &(dyn Fn(PipelineEvent) + Send + Sync),
) {
    while let Some(event) = receiver.recv().await {
        events(event);
    }
}

/// The user's offset if set, else the AccurateRip drive list's, else zero.
async fn resolve_offset(
    services: &Services,
    settings: &Settings,
    drive: &DriveIdentity,
) -> (SampleOffset, OffsetSource) {
    if let Some(offset) = settings.read_offset {
        return (offset, OffsetSource::User);
    }
    match offset::fetch_drive_database(
        &services.http,
        &services.drive_offsets_url,
        services.request_timeout,
    )
    .await
    {
        Ok(database) => match offset::find_offset(&database, drive) {
            Some(offset) => (offset, OffsetSource::AccurateRipDatabase),
            None => (SampleOffset(0), OffsetSource::Unknown),
        },
        Err(error) => {
            tracing::warn!(%error, "could not fetch the drive offset list");
            (SampleOffset(0), OffsetSource::Unknown)
        }
    }
}

type Fetched<T> = std::result::Result<Vec<T>, String>;

async fn fetch_verification(
    services: Services,
    ids: DiscIds,
    toc: Toc,
) -> (Fetched<DatabaseEntry>, Fetched<CtdbEntry>) {
    let (accuraterip, ctdb) = tokio::join!(
        accuraterip::fetch_database(
            &services.http,
            &services.accuraterip_base,
            &ids.accuraterip,
            services.request_timeout,
        ),
        ctdb::lookup(
            &services.http,
            &services.ctdb_base,
            &toc,
            services.request_timeout
        ),
    );
    (
        accuraterip.map_err(|e| unavailable_reason(&e)),
        ctdb.map_err(|e| unavailable_reason(&e)),
    )
}

/// Why a database could not be checked, in a few plain words.
fn unavailable_reason(error: &CoreError) -> String {
    match error {
        CoreError::Network { source, .. } if source.is_timeout() => "it took too long".to_owned(),
        CoreError::Network { source, .. } if source.is_connect() => {
            "no internet connection".to_owned()
        }
        CoreError::Network { source, .. } => match source.status() {
            Some(status) => format!("the database answered {status}"),
            None => "the database could not be reached".to_owned(),
        },
        CoreError::Invalid(_) | CoreError::Json(_) => {
            "the database sent something unexpected".to_owned()
        }
    }
}

// ---------------------------------------------------------------------------
// Verifying
// ---------------------------------------------------------------------------

fn position(index: usize, count: usize) -> TrackPosition {
    match (index == 0, index + 1 == count) {
        (true, true) => TrackPosition::Only,
        (true, false) => TrackPosition::First,
        (false, true) => TrackPosition::Last,
        (false, false) => TrackPosition::Middle,
    }
}

fn accuraterip_crcs(reads: &[TrackReadResult]) -> Result<Vec<(TrackNumber, AccurateRipCrcs)>> {
    let count = reads.len();
    reads
        .iter()
        .enumerate()
        .map(|(index, read)| {
            let position = position(index, count);
            let crcs = match &read.pcm {
                Pcm::Memory(bytes) => accuraterip::track_crcs(bytes, position)?,
                Pcm::File(path) => {
                    let mut state = TrackCrcState::new(position, read.samples.0);
                    stream_file(path, |chunk| state.update(chunk).map_err(AppError::from))?;
                    state.finish()?
                }
            };
            Ok((read.track, crcs))
        })
        .collect()
}

/// Feeds a file to `consume` in chunks that are whole stereo samples.
fn stream_file(path: &Path, mut consume: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    const CHUNK: usize = 1 << 20; // a multiple of 4 bytes
    let mut file = std::fs::File::open(path).map_err(|e| AppError::io(path, e))?;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let mut filled = 0;
        while filled < CHUNK {
            let slice = buffer.get_mut(filled..).unwrap_or_default();
            match file.read(slice) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(AppError::io(path, error)),
            }
        }
        if filled == 0 {
            return Ok(());
        }
        consume(buffer.get(..filled).unwrap_or_default())?;
        if filled < CHUNK {
            return Ok(());
        }
    }
}

/// The CTDB whole-disc CRC, streamed track by track from the files (no
/// whole-disc buffer), worked out only when the database has entries.
fn ctdb_status(
    reads: &[TrackReadResult],
    toc: &Toc,
    entries: Fetched<CtdbEntry>,
) -> Result<CtdbStatus> {
    let entries = match entries {
        Err(reason) => return Ok(CtdbStatus::Unavailable { reason }),
        Ok(entries) if entries.is_empty() => return Ok(CtdbStatus::NotInDatabase),
        Ok(entries) => entries,
    };
    let audio_tracks = toc.audio_tracks().count();
    if reads.len() != audio_tracks {
        return Err(CoreError::Invalid(format!(
            "the disc has {audio_tracks} audio tracks but {} were read",
            reads.len()
        ))
        .into());
    }
    let total = reads
        .iter()
        .try_fold(0u64, |total, read| total.checked_add(read.samples.0))
        .ok_or_else(|| CoreError::Invalid("the disc is too long".into()))?;
    let mut state = ctdb::DiscCrcState::new(total);
    for read in reads {
        match &read.pcm {
            Pcm::Memory(bytes) => state.update(bytes)?,
            Pcm::File(path) => {
                stream_file(path, |chunk| state.update(chunk).map_err(AppError::from))?;
            }
        }
    }
    Ok(ctdb::verify(state.finish()?, &entries))
}

fn verification_for(
    track: TrackNumber,
    crcs: &[(TrackNumber, AccurateRipCrcs)],
    statuses: &[(TrackNumber, AccurateRipStatus)],
) -> (AccurateRipCrcs, AccurateRipStatus) {
    let crcs = crcs
        .iter()
        .find(|(number, _)| *number == track)
        .map(|(_, crcs)| *crcs)
        .unwrap_or(AccurateRipCrcs {
            v1: crate::model::AccurateRipCrc(0),
            v2: crate::model::AccurateRipCrc(0),
        });
    let status = statuses
        .iter()
        .find(|(number, _)| *number == track)
        .map(|(_, status)| status.clone())
        .unwrap_or(AccurateRipStatus::Unavailable {
            reason: "no result for this track".to_owned(),
        });
    (crcs, status)
}

/// Read problems in a report, one plain sentence per track.
pub fn read_issues(report: &RipReport) -> Vec<String> {
    let mut issues = Vec::new();
    for track in &report.tracks {
        let count = track.unrecovered_sectors.len();
        if count > 0 {
            let sectors = if count == 1 { "sector" } else { "sectors" };
            issues.push(format!(
                "Track {}: {count} {sectors} could not be read reliably.",
                track.number.get()
            ));
        }
        if track.test_crc.is_some_and(|test| test != track.copy_crc) {
            issues.push(format!(
                "Track {}: the test read and the copy read were different.",
                track.number.get()
            ));
        }
    }
    issues
}

/// Verification mismatches worth mentioning (a different pressing is the
/// usual cause, so these are notes, not errors).
pub fn verification_notes(report: &RipReport) -> Vec<String> {
    let mut notes: Vec<String> = report
        .tracks
        .iter()
        .filter(|track| matches!(track.accuraterip, AccurateRipStatus::Mismatch { .. }))
        .map(|track| {
            format!(
                "Track {}: AccurateRip has other checksums for this track (perhaps a different pressing).",
                track.number.get()
            )
        })
        .collect();
    if matches!(report.ctdb, CtdbStatus::Mismatch { .. }) {
        notes.push("The CUETools database has other checksums for this disc.".to_owned());
    }
    notes
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// The metadata of track `number` (the `index`th audio track), falling back
/// to "Track N" by the album artist.
pub fn track_metadata(
    release: &ReleaseMetadata,
    number: TrackNumber,
    index: usize,
) -> TrackMetadata {
    let found = release
        .tracks
        .iter()
        .find(|track| track.number == Some(number))
        .or_else(|| {
            release
                .tracks
                .get(index)
                .filter(|track| track.number.is_none())
        });
    let mut metadata = found.cloned().unwrap_or_default();
    metadata.number = Some(number);
    if metadata.title.trim().is_empty() {
        metadata.title = format!("Track {}", number.get());
    }
    if metadata.artist.trim().is_empty() {
        metadata.artist = release.artist.clone();
    }
    metadata
}

/// Replaces characters that are not allowed in file names on Windows,
/// macOS or Linux, and trims the result to a sensible length.
pub fn safe_file_name(text: &str) -> String {
    let replaced: String = text
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(100)
        .collect();
    let trimmed = replaced.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        "Untitled".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `01 - Title.flac`.
pub fn flac_file_name(number: TrackNumber, title: &str) -> String {
    format!("{:02} - {}.flac", number.get(), safe_file_name(title))
}

/// `Artist - Album`, for the cue sheet and log.
pub fn album_file_stem(release: &ReleaseMetadata) -> String {
    let artist = release.artist.trim();
    let title = release.title.trim();
    match (artist.is_empty(), title.is_empty()) {
        (false, false) => safe_file_name(&format!("{artist} - {title}")),
        (true, false) => safe_file_name(title),
        (false, true) => safe_file_name(artist),
        (true, true) => "Rip".to_owned(),
    }
}

/// Copies the rip into `<keep>/<Artist>/<Album>[ (disc N)]`, choosing a
/// fresh folder name instead of overwriting an earlier copy.
pub fn keep_copy(keep: &Path, release: &ReleaseMetadata, files: &[UploadFile]) -> Result<PathBuf> {
    let artist = safe_file_name(if release.artist.trim().is_empty() {
        "Unknown artist"
    } else {
        &release.artist
    });
    let mut album = safe_file_name(if release.title.trim().is_empty() {
        "Unknown album"
    } else {
        &release.title
    });
    if release.disc_total > 1 {
        album = format!("{album} (disc {})", release.disc_number);
    }
    let parent = keep.join(artist);
    let mut folder = parent.join(&album);
    let mut attempt = 2;
    while folder.exists() {
        folder = parent.join(format!("{album} ({attempt})"));
        attempt += 1;
    }
    std::fs::create_dir_all(&folder).map_err(|e| AppError::io(&folder, e))?;
    for file in files {
        let Some(name) = file.path.file_name() else {
            continue;
        };
        let destination = folder.join(name);
        std::fs::copy(&file.path, &destination).map_err(|e| AppError::io(&destination, e))?;
    }
    Ok(folder)
}

fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "could not remove temporary files");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}

/// Runs `future`, giving up with [`AppError::Cancelled`] once `cancel` is set.
async fn cancellable<F: Future>(future: F, cancel: &AtomicBool) -> Result<F::Output> {
    tokio::select! {
        output = future => Ok(output),
        () = wait_for(cancel) => Err(AppError::Cancelled),
    }
}

async fn wait_for(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AppError::Task(error.to_string()))
}

/// A disc's info read on a blocking thread.
pub async fn disc_info(drive: Box<dyn DiscDrive>) -> Result<(Box<dyn DiscDrive>, DiscInfo)> {
    let (drive, info) = blocking(move || {
        let mut drive = drive;
        let info = drive.disc_info();
        (drive, info)
    })
    .await?;
    Ok((drive, info?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_addresses_can_be_overridden() {
        let endpoints = Endpoints::with_overrides(|name| match name {
            "LIBRARY_RIP_CTDB_URL" => Some("http://127.0.0.1:9/ctdb/".to_owned()),
            "LIBRARY_RIP_MUSICBRAINZ_URL" => Some("  ".to_owned()),
            _ => None,
        })
        .unwrap();
        let official = Endpoints::official().unwrap();
        assert_eq!(endpoints.ctdb.as_str(), "http://127.0.0.1:9/ctdb/");
        assert_eq!(endpoints.musicbrainz, official.musicbrainz);
        assert_eq!(endpoints.accuraterip, official.accuraterip);
        assert!(Endpoints::with_overrides(|_| Some("not an address".to_owned())).is_err());
    }
    use crate::model::{AccurateRipCrc, Crc32, Lba, Peak};

    fn number(n: u8) -> TrackNumber {
        TrackNumber::new(n).unwrap()
    }

    #[test]
    fn file_names_are_safe_everywhere() {
        assert_eq!(safe_file_name("AC/DC: Live?"), "AC_DC_ Live_");
        assert_eq!(safe_file_name("  ...  "), "Untitled");
        assert_eq!(safe_file_name("Fin."), "Fin");
        assert_eq!(
            flac_file_name(number(3), "Blue in Green"),
            "03 - Blue in Green.flac"
        );
        assert_eq!(safe_file_name(&"x".repeat(300)).len(), 100);
        let release = ReleaseMetadata {
            title: "Kind of Blue".into(),
            artist: "Miles Davis".into(),
            ..ReleaseMetadata::default()
        };
        assert_eq!(album_file_stem(&release), "Miles Davis - Kind of Blue");
        assert_eq!(album_file_stem(&ReleaseMetadata::default()), "Rip");
    }

    #[test]
    fn positions_follow_the_accuraterip_rules() {
        assert_eq!(position(0, 1), TrackPosition::Only);
        assert_eq!(position(0, 3), TrackPosition::First);
        assert_eq!(position(1, 3), TrackPosition::Middle);
        assert_eq!(position(2, 3), TrackPosition::Last);
    }

    #[test]
    fn track_metadata_falls_back_to_track_n() {
        let release = ReleaseMetadata {
            artist: "Nina Simone".into(),
            tracks: vec![TrackMetadata {
                number: Some(number(2)),
                title: "Feeling Good".into(),
                ..TrackMetadata::default()
            }],
            ..ReleaseMetadata::default()
        };
        let second = track_metadata(&release, number(2), 1);
        assert_eq!(second.title, "Feeling Good");
        assert_eq!(second.artist, "Nina Simone");
        let first = track_metadata(&release, number(1), 0);
        assert_eq!(first.title, "Track 1");
        assert_eq!(first.number, Some(number(1)));
    }

    #[test]
    fn streams_files_in_whole_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pcm");
        let data: Vec<u8> = (0..(3 << 20) + 8).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let mut seen = Vec::new();
        stream_file(&path, |chunk| {
            assert_eq!(chunk.len() % 4, 0);
            seen.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, data);
    }

    fn report_track(
        n: u8,
        unrecovered: Vec<Lba>,
        test: Option<u32>,
        status: AccurateRipStatus,
    ) -> TrackReport {
        TrackReport {
            number: number(n),
            title: format!("Track {n}"),
            start_lba: Lba(0),
            length_sectors: Sectors(100),
            pregap: Sectors(0),
            isrc: None,
            peak: Peak(1000),
            copy_crc: Crc32(1),
            test_crc: test.map(Crc32),
            accuraterip_crcs: AccurateRipCrcs {
                v1: AccurateRipCrc(0),
                v2: AccurateRipCrc(0),
            },
            accuraterip: status,
            unrecovered_sectors: unrecovered,
            retried_sectors: 0,
            file_name: String::new(),
            file_sha256: None,
        }
    }

    #[test]
    fn issues_and_notes_are_plain_sentences() {
        let tracks = vec![
            report_track(1, vec![], Some(1), AccurateRipStatus::NotInDatabase),
            report_track(
                2,
                vec![Lba(5), Lba(6)],
                None,
                AccurateRipStatus::NotInDatabase,
            ),
            report_track(
                3,
                vec![],
                Some(2),
                AccurateRipStatus::Mismatch { best_confidence: 4 },
            ),
        ];
        let issues: Vec<String> = {
            let mut issues = Vec::new();
            for track in &tracks {
                let report = minimal_report(vec![track.clone()]);
                issues.extend(read_issues(&report));
            }
            issues
        };
        assert_eq!(
            issues,
            vec![
                "Track 2: 2 sectors could not be read reliably.".to_owned(),
                "Track 3: the test read and the copy read were different.".to_owned(),
            ]
        );
        let notes = verification_notes(&minimal_report(tracks));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("Track 3: AccurateRip"));
    }

    fn minimal_report(tracks: Vec<TrackReport>) -> RipReport {
        let toc = Toc::new(
            vec![TrackEntry {
                number: number(1),
                start_lba: Lba(0),
                length_sectors: Sectors(100),
                is_audio: true,
                pregap: Sectors(0),
                isrc: None,
            }],
            Lba(100),
        )
        .unwrap();
        RipReport {
            app_version: "test".into(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            finished_at: jiff::Timestamp::UNIX_EPOCH,
            drive: DriveIdentity::default(),
            read_offset: SampleOffset(0),
            offset_source: OffsetSource::Unknown,
            read_mode: crate::model::ReadMode::Secure,
            toc,
            disc_ids: DiscIds {
                musicbrainz: String::new(),
                cddb: 0,
                accuraterip: crate::model::AccurateRipDiscId {
                    track_count: 1,
                    id1: 0,
                    id2: 0,
                    cddb: 0,
                },
                ctdb_toc: String::new(),
            },
            mcn: None,
            release: ReleaseMetadata::default(),
            tracks,
            ctdb: CtdbStatus::NotInDatabase,
        }
    }

    #[test]
    fn keeps_a_copy_without_overwriting() {
        let source = tempfile::tempdir().unwrap();
        let keep = tempfile::tempdir().unwrap();
        let path = source.path().join("01 - A.flac");
        std::fs::write(&path, b"flac").unwrap();
        let files = vec![UploadFile {
            kind: MediaKind::Audio,
            title: "A".into(),
            disc: 1,
            track: Some(number(1)),
            duration_ms: Some(1),
            sha256: Sha256Digest([0; 32]),
            path,
        }];
        let release = ReleaseMetadata {
            title: "Album".into(),
            artist: "Artist".into(),
            disc_number: 1,
            disc_total: 1,
            ..ReleaseMetadata::default()
        };
        let first = keep_copy(keep.path(), &release, &files).unwrap();
        let second = keep_copy(keep.path(), &release, &files).unwrap();
        assert_eq!(first, keep.path().join("Artist/Album"));
        assert_eq!(second, keep.path().join("Artist/Album (2)"));
        assert_eq!(std::fs::read(second.join("01 - A.flac")).unwrap(), b"flac");
    }

    #[test]
    fn item_pages_live_under_items() {
        let base = Url::parse("http://library.local:8080/").unwrap();
        assert_eq!(item_page_url(&base, 7), "http://library.local:8080/items/7");
    }

    #[test]
    fn fractions_are_clamped() {
        assert_eq!(fraction(5, 10), 0.5);
        assert_eq!(fraction(20, 10), 1.0);
        assert_eq!(fraction(0, 0), 1.0);
    }

    // -- Services that accept a connection and then never answer ----------

    /// A server that accepts connections and never sends a byte back.
    async fn silent_server() -> Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        Url::parse(&format!("http://{address}/")).unwrap()
    }

    async fn silent_services(request_timeout: Duration) -> Services {
        let base = silent_server().await;
        let endpoints = Endpoints {
            musicbrainz: base.join("ws/2/").unwrap(),
            cover_art: base.clone(),
            accuraterip: base.join("accuraterip/").unwrap(),
            drive_offsets: base.join("DriveOffsets.bin").unwrap(),
            ctdb: base.clone(),
        };
        let mut services = Services::new(crate::library::http_client().unwrap(), &endpoints);
        services.request_timeout = request_timeout;
        services
    }

    fn one_track_drive(dir: &Path) -> Box<dyn DiscDrive> {
        crate::testsupport::make_fake_disc(
            dir,
            &crate::testsupport::FakeDiscOptions {
                tracks: 1,
                seconds: 1,
                ..Default::default()
            },
        )
        .unwrap();
        Box::new(crate::drive::fake::FakeDrive::open(dir).unwrap())
    }

    fn release() -> ReleaseMetadata {
        ReleaseMetadata {
            title: "Album".into(),
            artist: "Artist".into(),
            disc_number: 1,
            disc_total: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn silent_databases_time_out_instead_of_hanging() {
        let disc = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let services = silent_services(Duration::from_millis(300)).await;
        let release = release();
        let settings = Settings::default();
        let started = Instant::now();
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            rip(
                one_track_drive(disc.path()),
                RipRequest {
                    release: &release,
                    target: ItemTarget::Existing { id: 1 },
                    settings: &settings,
                    services: &services,
                    work_dir: work.path().join("rip"),
                    cancel: Arc::new(AtomicBool::new(false)),
                },
                &|_| {},
            ),
        )
        .await
        .expect("the rip hung on a silent database")
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(output.report.offset_source, OffsetSource::Unknown);
        assert_eq!(
            output.report.ctdb,
            CtdbStatus::Unavailable {
                reason: "it took too long".into()
            }
        );
        for track in &output.report.tracks {
            assert_eq!(
                track.accuraterip,
                AccurateRipStatus::Unavailable {
                    reason: "it took too long".into()
                }
            );
        }
    }

    #[tokio::test]
    async fn cancel_stops_a_rip_waiting_for_the_drive_list() {
        let disc = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        // Long enough that only Cancel can end the wait.
        let services = silent_services(Duration::from_secs(3600)).await;
        let release = release();
        let settings = Settings::default(); // offset from the drive list
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            stopper.store(true, Ordering::Relaxed);
        });
        let offsets = std::sync::Mutex::new(0);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            rip(
                one_track_drive(disc.path()),
                RipRequest {
                    release: &release,
                    target: ItemTarget::Existing { id: 1 },
                    settings: &settings,
                    services: &services,
                    work_dir: work.path().join("rip"),
                    cancel,
                },
                &|event| {
                    if matches!(event, PipelineEvent::Offset { .. }) {
                        *offsets.lock().unwrap() += 1;
                    }
                },
            ),
        )
        .await
        .expect("Cancel did not stop the rip");
        assert!(matches!(result, Err(AppError::Cancelled)), "{result:?}");
        assert_eq!(
            *offsets.lock().unwrap(),
            0,
            "stopped before the offset was known"
        );
    }

    #[tokio::test]
    async fn cancel_stops_a_rip_waiting_for_verification() {
        let disc = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let services = silent_services(Duration::from_secs(3600)).await;
        let release = release();
        let settings = Settings {
            read_offset: Some(SampleOffset(6)), // skip the drive list
            ..Settings::default()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = Arc::clone(&cancel);
        let rip_dir = work.path().join("rip");
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            rip(
                one_track_drive(disc.path()),
                RipRequest {
                    release: &release,
                    target: ItemTarget::Existing { id: 1 },
                    settings: &settings,
                    services: &services,
                    work_dir: rip_dir.clone(),
                    cancel,
                },
                // Cancel once every track is read: the rip is then waiting
                // for AccurateRip and CTDB, which never answer.
                &move |event| {
                    if matches!(event, PipelineEvent::TrackRead { .. }) {
                        stopper.store(true, Ordering::Relaxed);
                    }
                },
            ),
        )
        .await
        .expect("Cancel did not stop the rip");
        assert!(matches!(result, Err(AppError::Cancelled)), "{result:?}");
        assert!(!rip_dir.join(".pcm").exists(), "read data is cleaned up");
    }

    #[tokio::test]
    async fn the_shared_client_names_library_rip_with_a_contact() {
        use axum::{Router, http::HeaderMap, routing::get};
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let record = Arc::clone(&seen);
        let app = Router::new().route(
            "/DriveOffsets.bin",
            get(move |headers: HeaderMap| async move {
                *record.lock().unwrap() = headers
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                Vec::<u8>::new()
            }),
        );
        let base = crate::core::testserver::serve(app).await;
        let services = Services {
            drive_offsets_url: base.join("DriveOffsets.bin").unwrap(),
            ..silent_services(Duration::from_secs(5)).await
        };
        let _ = resolve_offset(&services, &Settings::default(), &DriveIdentity::default()).await;
        assert_eq!(*seen.lock().unwrap(), crate::musicbrainz::user_agent());
        assert!(seen.lock().unwrap().contains(crate::SOURCE_URL));
    }
}
