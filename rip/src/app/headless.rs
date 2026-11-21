// SPDX-License-Identifier: AGPL-3.0-or-later
//! Headless runner: `library-rip headless --library URL --fake-disc DIR`.
//!
//! Runs the whole flow without a window, through the same
//! [`LiveBackend`] the GUI uses:
//!
//! 1. connect (the browser handshake, printing the page to open, or with
//!    `--device-code` the code for the librarian to type), or reuse the
//!    connection in `--key-file`;
//! 2. read the simulated disc;
//! 3. take the first MusicBrainz match (or the disc's CD-Text, or
//!    "Unknown album");
//! 4. rip, verify and encode;
//! 5. upload to a new catalogue entry, unless the rip has read errors
//!    (then only with `--upload-with-errors`);
//! 6. eject.
//!
//! Exit codes (see [`HeadlessError::exit_code`]): 0 done, 1 something else
//! went wrong, 2 bad command line, 3 could not connect, 4 drive or disc
//! problem, 5 the rip has read errors and was not uploaded, 6 the upload
//! failed, 130 stopped with Ctrl-C.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use url::Url;

use super::controller::{Backend, EventSender};
use super::live::{LiveBackend, LiveConfig, MemoryKeyStore};
use super::pipeline::{Endpoints, PipelineEvent, RipOutput};
use super::settings::Settings;
use super::state::{ConnectMethod, Event, RipJob, new_item, release_from_disc};
use super::{AppError, Problem};
use crate::drive::fake::FakeDriveProvider;
use crate::library::LibraryError;
use crate::library::connect::BrowserOpener;
use crate::library::keystore::{FileStore, KeyStore};
use crate::model::{ItemTarget, ReadMode, ReleaseMetadata, SampleOffset};

/// Options from the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessOptions {
    /// Library base address (normalised with a trailing slash).
    pub library: Url,
    /// Simulated disc directory.
    pub fake_disc: PathBuf,
    /// Use the device-code flow instead of the browser.
    pub device_code: bool,
    /// Read offset to apply; `None` looks the drive up.
    pub offset: Option<SampleOffset>,
    /// Secure or test-and-copy.
    pub read_mode: ReadMode,
    /// Also keep the rip in this folder.
    pub keep: Option<PathBuf>,
    /// Upload even when some sectors could not be read reliably.
    pub upload_with_errors: bool,
    /// Keep the connection in this file between runs (otherwise every run
    /// connects again).
    pub key_file: Option<PathBuf>,
    /// Outside services (tests point them at mock servers).
    pub endpoints: Endpoints,
    /// Where the rip is written before upload.
    pub work_root: PathBuf,
}

/// What a headless run produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessOutcome {
    /// The item page on the library.
    pub item_url: String,
    /// Files uploaded.
    pub uploaded: usize,
    /// Files already on the item.
    pub skipped: usize,
    /// The local copy, if kept.
    pub kept: Option<PathBuf>,
    /// Read problems (only when uploaded with `--upload-with-errors`).
    pub read_issues: Vec<String>,
    /// Verification notes.
    pub notes: Vec<String>,
}

/// Why a headless run stopped. Each kind has its own exit code.
#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    /// Connecting failed.
    #[error("could not connect to the library: {0}")]
    Connect(#[source] AppError),
    /// The drive or disc could not be read.
    #[error("could not read the disc: {0}")]
    Disc(#[source] AppError),
    /// Ripping failed.
    #[error("the rip failed: {0}")]
    Rip(#[source] AppError),
    /// The rip has read errors and `--upload-with-errors` was not given.
    #[error("the rip has read errors, so nothing was uploaded: {}", issues.join(" "))]
    ReadErrors {
        /// One sentence per problem.
        issues: Vec<String>,
        /// Where the rip was left.
        folder: PathBuf,
    },
    /// Uploading failed.
    #[error("the upload failed: {0}")]
    Upload(#[source] AppError),
    /// Stopped by Ctrl-C.
    #[error("stopped")]
    Cancelled,
}

impl HeadlessError {
    /// The process exit code.
    pub fn exit_code(&self) -> u8 {
        match self {
            HeadlessError::Connect(_) => 3,
            HeadlessError::Disc(_) => 4,
            HeadlessError::Rip(AppError::Drive(_)) => 4,
            HeadlessError::Rip(_) => 1,
            HeadlessError::ReadErrors { .. } => 5,
            HeadlessError::Upload(_) => 6,
            HeadlessError::Cancelled => 130,
        }
    }

    /// What to tell the operator.
    pub fn problem(&self) -> Problem {
        match self {
            HeadlessError::Connect(error)
            | HeadlessError::Disc(error)
            | HeadlessError::Rip(error)
            | HeadlessError::Upload(error) => error.problem(),
            HeadlessError::ReadErrors { issues, folder } => Problem::new(
                format!("The rip has read errors: {}", issues.join(" ")),
                format!(
                    "Clean the disc and rip again, or run again with --upload-with-errors. \
                     The rip is in {}.",
                    folder.display()
                ),
            ),
            HeadlessError::Cancelled => Problem::new("Stopped.", "Nothing more was uploaded."),
        }
    }

    fn from_stage(stage: fn(AppError) -> HeadlessError, error: AppError) -> HeadlessError {
        if error.is_cancelled() {
            HeadlessError::Cancelled
        } else {
            stage(error)
        }
    }
}

/// Options for [`run_film`].
#[derive(Clone, Debug)]
pub struct HeadlessFilmOptions {
    /// Library base address (normalised with a trailing slash).
    pub library: Url,
    /// Use the device-code flow instead of the browser.
    pub device_code: bool,
    /// The film's name (default: the disc's name, cleaned).
    pub name: Option<String>,
    /// The year.
    pub year: Option<u16>,
    /// MakeMKV title to copy (default: the longest).
    pub title: Option<usize>,
    /// Make the WebM viewing copy (needs ffmpeg with AV1 and Opus).
    pub viewing_copy: bool,
    /// Also keep the rip in this folder.
    pub keep: Option<PathBuf>,
    /// Keep the connection in this file between runs.
    pub key_file: Option<PathBuf>,
    /// Outside services (unused by films, but the backend wants them).
    pub endpoints: Endpoints,
    /// Where the rip is written before upload.
    pub work_root: PathBuf,
}

/// Rips the film disc MakeMKV sees (`LIBRARY_RIP_MAKEMKVCON` points a test
/// at a stand-in) and uploads it: connect, scan, copy, make the viewing
/// copy, upload.
pub async fn run_film(
    options: &HeadlessFilmOptions,
    report: Reporter,
    cancel: Arc<AtomicBool>,
) -> Result<HeadlessOutcome, HeadlessError> {
    let keys: Box<dyn KeyStore> = match &options.key_file {
        Some(path) => Box::new(FileStore::new(path.clone())),
        None => Box::new(MemoryKeyStore::default()),
    };
    let settings = Settings {
        keep_local_copy: options.keep.clone(),
        ..Settings::default()
    };
    let backend = LiveBackend::new(LiveConfig {
        endpoints: options.endpoints.clone(),
        drives: Arc::new(FakeDriveProvider::new(PathBuf::new())),
        keys,
        browser: Arc::new(PrintOnlyBrowser),
        settings_path: None,
        settings: settings.clone(),
        work_root: options.work_root.clone(),
    })
    .map_err(HeadlessError::Connect)?;
    let (events, receiver) = EventSender::channel();
    let printer = tokio::spawn(print_events(receiver, Arc::clone(&report)));
    let result = film_flow(&backend, options, settings, &events, &report, &cancel).await;
    drop(events);
    let _ = tokio::time::timeout(Duration::from_secs(1), printer).await;
    result
}

async fn film_flow(
    backend: &LiveBackend,
    options: &HeadlessFilmOptions,
    settings: Settings,
    events: &EventSender,
    report: &Reporter,
    cancel: &Arc<AtomicBool>,
) -> Result<HeadlessOutcome, HeadlessError> {
    let connect_options = HeadlessOptions {
        library: options.library.clone(),
        fake_disc: PathBuf::new(),
        device_code: options.device_code,
        offset: None,
        read_mode: ReadMode::Secure,
        keep: None,
        upload_with_errors: false,
        key_file: options.key_file.clone(),
        endpoints: options.endpoints.clone(),
        work_root: options.work_root.clone(),
    };
    let who = until_cancelled(cancel, connect(backend, &connect_options, events))
        .await?
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Connect, e))?;
    report(&format!(
        "Connected to {} as {}.",
        who.library.name, who.key_name
    ));

    let device = "film-disc".to_owned();
    let scan = backend
        .scan_film(device.clone())
        .await
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Disc, e))?;
    report(&format!(
        "Disc: {} ({}), {} titles (drive {}).",
        scan.disc.name,
        scan.disc.kind,
        scan.disc.titles.len(),
        scan.drive_name
    ));
    for title in &scan.disc.titles {
        report(&format!("Title {}: {}", title.id, title.summary()));
    }
    let chosen = match options.title {
        Some(id) => scan.disc.titles.iter().find(|t| t.id == id),
        None => scan.disc.main_title(),
    }
    .cloned()
    .ok_or_else(|| HeadlessError::Disc(AppError::Video(crate::video::VideoError::NoTitles)))?;
    let name = options.name.clone().unwrap_or_else(|| {
        crate::video::scan::clean_name(if scan.disc.name.is_empty() {
            &scan.disc.volume
        } else {
            &scan.disc.name
        })
    });
    report(&format!("Film: {name} (title {}).", chosen.id));
    let viewing_copy = options.viewing_copy
        && matches!(scan.viewing, super::film::ViewingSupport::Available { .. });
    if options.viewing_copy && !viewing_copy {
        report("No viewing copy: ffmpeg with AV1 and Opus was not found.");
    }
    let job = super::film::FilmJob {
        device,
        disc_index: scan.disc_index,
        disc: scan.disc.clone(),
        name,
        year: options.year,
        titles: vec![chosen],
        viewing_copy,
        settings,
    };
    let output = backend
        .rip_film(job, events.clone(), Arc::clone(cancel))
        .await
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Rip, e))?;
    report(&format!("Uploading {} files…", output.plan.files.len()));
    let super::film::FilmOutput {
        folder,
        kept,
        plan,
        notes,
    } = output;
    let uploaded = backend
        .upload(plan, folder, events.clone(), Arc::clone(cancel))
        .await
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Upload, e))?;
    Ok(HeadlessOutcome {
        item_url: uploaded.item_url,
        uploaded: uploaded.uploaded,
        skipped: uploaded.skipped,
        kept,
        read_issues: Vec::new(),
        notes,
    })
}

/// A line of progress for the operator.
pub type Reporter = Arc<dyn Fn(&str) + Send + Sync>;

/// Prints the connect page instead of opening a browser (there may be no
/// display).
#[derive(Clone, Copy, Debug, Default)]
pub struct PrintOnlyBrowser;

impl BrowserOpener for PrintOnlyBrowser {
    fn open(&self, _url: &Url) -> std::io::Result<()> {
        // The page was already reported through `Event::BrowserOpened`.
        Ok(())
    }
}

/// Runs the whole flow, printing progress through `report`, stopping when
/// `cancel` is set (Ctrl-C).
pub async fn run(
    options: &HeadlessOptions,
    report: Reporter,
    cancel: Arc<AtomicBool>,
) -> Result<HeadlessOutcome, HeadlessError> {
    run_with_browser(options, Arc::new(PrintOnlyBrowser), report, cancel).await
}

/// [`run`] with a chosen browser (tests use one that approves at once).
pub async fn run_with_browser(
    options: &HeadlessOptions,
    browser: Arc<dyn BrowserOpener>,
    report: Reporter,
    cancel: Arc<AtomicBool>,
) -> Result<HeadlessOutcome, HeadlessError> {
    let keys: Box<dyn KeyStore> = match &options.key_file {
        Some(path) => Box::new(FileStore::new(path.clone())),
        None => Box::new(MemoryKeyStore::default()),
    };
    let settings = Settings {
        read_offset: options.offset,
        read_mode: options.read_mode,
        keep_local_copy: options.keep.clone(),
        pressing_search: None,
    };
    let backend = LiveBackend::new(LiveConfig {
        endpoints: options.endpoints.clone(),
        drives: Arc::new(FakeDriveProvider::new(options.fake_disc.clone())),
        keys,
        browser,
        settings_path: None,
        settings: settings.clone(),
        work_root: options.work_root.clone(),
    })
    .map_err(HeadlessError::Connect)?;

    let (events, receiver) = EventSender::channel();
    let printer = tokio::spawn(print_events(receiver, Arc::clone(&report)));
    let result = flow(&backend, options, settings, &events, &report, &cancel).await;
    drop(events);
    // The printer ends once every sender is gone; never wait long for it.
    let _ = tokio::time::timeout(Duration::from_secs(1), printer).await;
    result
}

async fn flow(
    backend: &LiveBackend,
    options: &HeadlessOptions,
    settings: Settings,
    events: &EventSender,
    report: &Reporter,
    cancel: &Arc<AtomicBool>,
) -> Result<HeadlessOutcome, HeadlessError> {
    // 1. Connect.
    let who = until_cancelled(cancel, connect(backend, options, events))
        .await?
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Connect, e))?;
    report(&format!(
        "Connected to {} as {}.",
        who.library.name, who.key_name
    ));

    // 2. The disc.
    let device = options.fake_disc.display().to_string();
    let disc = backend
        .read_disc(device.clone())
        .await
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Disc, e))?;
    let audio = disc.toc.audio_tracks().count();
    report(&format!(
        "Disc: {audio} audio tracks, {} (drive {}).",
        super::view::duration(disc.toc.audio_duration_ms()),
        disc.drive
    ));
    for data in disc.toc.data_tracks() {
        report(&format!(
            "Data track {} in session {} (not ripped).",
            data.number, data.session
        ));
    }

    // 3. Identify.
    let release = identify(backend, &disc, report).await;
    report(&format!("Album: {} - {}.", release.artist, release.title));

    // 4. Rip.
    let job = RipJob {
        device: device.clone(),
        release: release.clone(),
        target: ItemTarget::New(new_item(&release)),
        settings,
    };
    let output = backend
        .rip(job, events.clone(), Arc::clone(cancel))
        .await
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Rip, e))?;
    let read_issues = output.read_issues();
    for issue in &read_issues {
        report(issue);
    }
    report(&format!(
        "Verdict: {}",
        crate::core::log::summary(&output.report)
    ));
    if !read_issues.is_empty() && !options.upload_with_errors {
        return Err(HeadlessError::ReadErrors {
            issues: read_issues,
            folder: output.kept.clone().unwrap_or_else(|| output.folder.clone()),
        });
    }

    // 5. Upload.
    report(&format!("Uploading {} files…", output.plan.files.len()));
    let RipOutput {
        folder,
        kept,
        plan,
        report: rip_report,
    } = output;
    let notes = super::pipeline::verification_notes(&rip_report);
    let uploaded = backend
        .upload(plan, folder, events.clone(), Arc::clone(cancel))
        .await
        .map_err(|e| HeadlessError::from_stage(HeadlessError::Upload, e))?;
    if let Some(problem) = &uploaded.cover_problem {
        report(&format!("The cover could not be added: {problem}"));
    }

    // 6. Eject (a failure here does not undo anything).
    if let Err(error) = backend.eject(device).await {
        tracing::warn!(%error, "could not eject");
    }
    Ok(HeadlessOutcome {
        item_url: uploaded.item_url,
        uploaded: uploaded.uploaded,
        skipped: uploaded.skipped,
        kept,
        read_issues,
        notes,
    })
}

/// Reuses the saved connection for this library, or connects.
async fn connect(
    backend: &LiveBackend,
    options: &HeadlessOptions,
    events: &EventSender,
) -> Result<crate::library::api::WhoAmI, AppError> {
    if backend.saved_address().as_deref() == Some(options.library.as_str()) {
        match backend.restore().await {
            Ok(who) => return Ok(who),
            Err(AppError::Library(LibraryError::Unauthorised)) => {}
            Err(error) => return Err(error),
        }
    }
    let method = if options.device_code {
        ConnectMethod::DeviceCode
    } else {
        ConnectMethod::Browser
    };
    backend
        .connect(options.library.clone(), method, events.clone())
        .await
}

/// The first MusicBrainz match, else the disc's own text.
async fn identify(
    backend: &LiveBackend,
    disc: &crate::model::DiscInfo,
    report: &Reporter,
) -> ReleaseMetadata {
    let mut release = match backend.find_releases(disc.clone()).await {
        Ok(candidates) => match candidates.first() {
            Some(candidate) => {
                match backend
                    .load_release(candidate.clone(), disc.toc.clone())
                    .await
                {
                    Ok(release) => Some(release),
                    Err(error) => {
                        report(&format!(
                            "MusicBrainz details could not be loaded ({}); using the disc's own text.",
                            error.problem().message
                        ));
                        None
                    }
                }
            }
            None => {
                report("MusicBrainz does not know this disc; using the disc's own text.");
                None
            }
        },
        Err(error) => {
            report(&format!(
                "MusicBrainz could not be asked ({}); using the disc's own text.",
                error.problem().message
            ));
            None
        }
    }
    .unwrap_or_else(|| release_from_disc(disc));
    super::state::fit_tracks(&mut release, &disc.toc);
    if release.title.trim().is_empty() {
        release.title = "Unknown album".to_owned();
    }
    if release.artist.trim().is_empty() {
        release.artist = "Unknown artist".to_owned();
    }
    release
}

async fn until_cancelled<F: std::future::Future>(
    cancel: &AtomicBool,
    future: F,
) -> Result<F::Output, HeadlessError> {
    tokio::select! {
        output = future => Ok(output),
        () = async {
            while !cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } => Err(HeadlessError::Cancelled),
    }
}

/// Turns events into lines for the operator.
async fn print_events(mut receiver: mpsc::UnboundedReceiver<Event>, report: Reporter) {
    while let Some(event) = receiver.recv().await {
        if let Some(line) = describe(&event) {
            report(&line);
        }
    }
}

/// The line printed for an event, if any.
pub fn describe(event: &Event) -> Option<String> {
    match event {
        Event::BrowserOpened { url } => Some(format!(
            "Open this page in a browser to connect Library Rip, then press Yes, connect:\n  {url}"
        )),
        Event::DeviceCodeShown {
            user_code,
            verification_uri,
        } => Some(format!(
            "On the library, open {verification_uri} and type this code: {user_code}"
        )),
        Event::Progress(progress) => describe_progress(progress),
        Event::FilmProgress(super::film::FilmEvent::Finished { title }) => {
            Some(format!("Title {title}: done."))
        }
        Event::FilmProgress(super::film::FilmEvent::Copying { title, fraction })
            if *fraction >= 1.0 =>
        {
            Some(format!("Title {title}: copied."))
        }
        _ => None,
    }
}

fn describe_progress(event: &PipelineEvent) -> Option<String> {
    match event {
        PipelineEvent::Offset { offset, source } => Some(format!(
            "Read offset: {offset} samples ({}).",
            super::view::offset_source(*source)
        )),
        PipelineEvent::TrackRead {
            track,
            unrecovered_sectors,
            test_matched,
        } => Some(format!(
            "Track {}: {}",
            track.get(),
            super::view::read_summary(*unrecovered_sectors, *test_matched)
        )),
        PipelineEvent::Verified { tracks, ctdb } => {
            let mut lines: Vec<String> = tracks
                .iter()
                .map(|(track, status)| {
                    format!("Track {}: AccurateRip: {}", track.get(), status.summary())
                })
                .collect();
            lines.push(format!("CUETools database: {}", ctdb.summary()));
            Some(lines.join("\n"))
        }
        PipelineEvent::Encoding { track } => Some(format!("Track {}: writing FLAC.", track.get())),
        PipelineEvent::ItemReady { url, .. } => Some(format!("Catalogue entry: {url}")),
        PipelineEvent::Uploaded { index, skipped } => Some(if *skipped {
            format!("File {}: already on the library.", index + 1)
        } else {
            format!("File {}: uploaded.", index + 1)
        }),
        PipelineEvent::Reading { .. } | PipelineEvent::Uploading { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OffsetSource, TrackNumber};

    #[test]
    fn exit_codes_are_distinct_per_stage() {
        let library = || AppError::Library(LibraryError::Denied);
        assert_eq!(HeadlessError::Connect(library()).exit_code(), 3);
        assert_eq!(HeadlessError::Disc(library()).exit_code(), 4);
        assert_eq!(
            HeadlessError::Rip(AppError::Drive(crate::drive::DriveError::NoDisc)).exit_code(),
            4
        );
        assert_eq!(HeadlessError::Rip(library()).exit_code(), 1);
        assert_eq!(
            HeadlessError::ReadErrors {
                issues: vec![],
                folder: PathBuf::new()
            }
            .exit_code(),
            5
        );
        assert_eq!(HeadlessError::Upload(library()).exit_code(), 6);
        assert_eq!(HeadlessError::Cancelled.exit_code(), 130);
        assert!(matches!(
            HeadlessError::from_stage(HeadlessError::Rip, AppError::Cancelled),
            HeadlessError::Cancelled
        ));
    }

    #[test]
    fn events_become_plain_lines() {
        let code = describe(&Event::DeviceCodeShown {
            user_code: "WXYZ-1234".into(),
            verification_uri: "http://library.test/connect/device".into(),
        })
        .unwrap();
        assert!(code.contains("WXYZ-1234"));
        assert!(code.contains("/connect/device"));
        let offset = describe(&Event::Progress(PipelineEvent::Offset {
            offset: SampleOffset(6),
            source: OffsetSource::User,
        }))
        .unwrap();
        assert_eq!(offset, "Read offset: +6 samples (set in Settings).");
        let read = describe(&Event::Progress(PipelineEvent::TrackRead {
            track: TrackNumber::new(2).unwrap(),
            unrecovered_sectors: 0,
            test_matched: Some(true),
        }))
        .unwrap();
        assert_eq!(read, "Track 2: read without errors, test and copy match.");
        assert!(describe(&Event::RipPressed).is_none());
    }
}
