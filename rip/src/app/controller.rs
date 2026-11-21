// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs the [`Command`]s that [`AppState::handle`] returns against a
//! [`Backend`] and feeds the results back as [`Event`]s.
//!
//! The GUI and the tests use the same controller: the GUI with
//! [`super::live::LiveBackend`], the tests with a mock, so the whole app
//! (minus drawing) is tested without a window, a drive or a network.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use url::Url;

use super::film::{FilmJob, FilmOutput, FilmScan};
use super::pipeline::{RipOutput, UploadOutput};
use super::settings::Settings;
use super::state::{AppState, Command, ConnectMethod, Event, RipJob, Task};
use super::{AppError, Result};
use crate::drive::{DriveDescriptor, DriveError};
use crate::library::api::{ItemSummary, WhoAmI};
use crate::model::{DiscInfo, ReleaseMetadata, Toc, UploadPlan};
use crate::musicbrainz::ReleaseCandidate;

/// Sends events to the controller from anywhere (GUI callbacks, background
/// work). Cheap to clone; sending after the controller is gone does nothing.
#[derive(Clone, Debug)]
pub struct EventSender(mpsc::UnboundedSender<Event>);

impl EventSender {
    /// A new channel.
    pub fn channel() -> (EventSender, mpsc::UnboundedReceiver<Event>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (EventSender(sender), receiver)
    }

    /// Sends `event`.
    pub fn send(&self, event: Event) {
        // A closed channel means the app is shutting down.
        let _ = self.0.send(event);
    }
}

/// Everything the app does outside its own state: the library, drives,
/// MusicBrainz, files, the browser. [`super::live::LiveBackend`] is the
/// real one.
pub trait Backend: Send + Sync + 'static {
    /// Settings saved on this computer.
    fn settings(&self) -> Settings;

    /// The address of the saved connection, if any.
    fn saved_address(&self) -> Option<String>;

    /// Checks the saved connection with the library.
    fn restore(&self) -> impl Future<Output = Result<WhoAmI>> + Send;

    /// Runs the connect handshake, reporting [`Event::BrowserOpened`] or
    /// [`Event::DeviceCodeShown`] through `events` while it waits.
    fn connect(
        &self,
        address: Url,
        method: ConnectMethod,
        events: EventSender,
    ) -> impl Future<Output = Result<WhoAmI>> + Send;

    /// Revokes the key and forgets it (forgets it even if the library
    /// cannot be reached).
    fn disconnect(&self) -> impl Future<Output = Result<()>> + Send;

    /// Drives attached.
    fn list_drives(&self) -> impl Future<Output = Result<Vec<DriveDescriptor>>> + Send;

    /// Reads the disc in `device`.
    fn read_disc(&self, device: String) -> impl Future<Output = Result<DiscInfo>> + Send;

    /// Whether film discs can be ripped (MakeMKV is looked for when the disc
    /// turns out to have no music). Backends that cannot leave this false.
    fn film_supported(&self) -> bool {
        false
    }

    /// Asks MakeMKV about the film disc in `device`.
    fn scan_film(&self, device: String) -> impl Future<Output = Result<FilmScan>> + Send {
        let _ = device;
        async { Err(AppError::Task("film discs are not supported here".to_owned())) }
    }

    /// Copies film titles and makes viewing copies, reporting
    /// [`Event::FilmProgress`].
    fn rip_film(
        &self,
        job: FilmJob,
        events: EventSender,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<FilmOutput>> + Send {
        let _ = (job, events, cancel);
        async { Err(AppError::Task("film discs are not supported here".to_owned())) }
    }

    /// MusicBrainz releases matching the disc.
    fn find_releases(
        &self,
        disc: DiscInfo,
    ) -> impl Future<Output = Result<Vec<ReleaseCandidate>>> + Send;

    /// MusicBrainz releases matching a text search.
    fn search_releases(
        &self,
        query: String,
    ) -> impl Future<Output = Result<Vec<ReleaseCandidate>>> + Send;

    /// A release's details and front cover.
    fn load_release(
        &self,
        candidate: ReleaseCandidate,
        toc: Toc,
    ) -> impl Future<Output = Result<ReleaseMetadata>> + Send;

    /// Music items in the library's catalogue matching `query`.
    fn search_items(&self, query: String) -> impl Future<Output = Result<Vec<ItemSummary>>> + Send;

    /// Reads, verifies and encodes, reporting [`Event::Progress`].
    fn rip(
        &self,
        job: RipJob,
        events: EventSender,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<RipOutput>> + Send;

    /// Uploads, reporting [`Event::Progress`]; removes `folder` afterwards.
    fn upload(
        &self,
        plan: UploadPlan,
        folder: PathBuf,
        events: EventSender,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<UploadOutput>> + Send;

    /// Ejects the disc in `device`.
    fn eject(&self, device: String) -> impl Future<Output = Result<()>> + Send;

    /// Saves settings.
    fn save_settings(&self, settings: Settings) -> impl Future<Output = Result<()>> + Send;

    /// Shows a folder picker.
    fn pick_folder(&self) -> impl Future<Output = Option<PathBuf>> + Send;

    /// Opens a web page in the browser.
    fn open_url(&self, url: String) -> impl Future<Output = Result<()>> + Send;
}

struct Running {
    abort: AbortHandle,
    cancel: Option<Arc<AtomicBool>>,
}

/// Owns the state, receives events and runs commands.
pub struct Controller<B: Backend> {
    backend: Arc<B>,
    state: AppState,
    sender: EventSender,
    receiver: mpsc::UnboundedReceiver<Event>,
    running: BTreeMap<Task, Running>,
}

impl<B: Backend> Controller<B> {
    /// A controller for `backend`; queues [`Event::Started`]. Must be used
    /// inside a Tokio runtime.
    pub fn new(backend: B) -> Self {
        let (sender, receiver) = EventSender::channel();
        sender.send(Event::Started {
            settings: backend.settings(),
            address: backend.saved_address(),
        });
        Controller {
            backend: Arc::new(backend),
            state: AppState::default(),
            sender,
            receiver,
            running: BTreeMap::new(),
        }
    }

    /// A sender for user actions.
    pub fn sender(&self) -> EventSender {
        self.sender.clone()
    }

    /// The current state.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Waits for the next event and applies it.
    pub async fn step(&mut self) {
        // The controller holds a sender, so the channel never closes.
        if let Some(event) = self.receiver.recv().await {
            self.dispatch(event);
        }
    }

    /// Applies an event now and starts the commands it returns.
    pub fn dispatch(&mut self, event: Event) {
        tracing::debug!(?event, "event");
        for command in self.state.handle(event) {
            self.execute(command);
        }
    }

    /// Processes events until `done` holds for the state.
    pub async fn run_until(&mut self, mut done: impl FnMut(&AppState) -> bool) {
        while !done(&self.state) {
            self.step().await;
        }
    }

    /// Processes events forever, calling `render` after each one.
    pub async fn run(mut self, mut render: impl FnMut(&AppState)) {
        render(&self.state);
        loop {
            self.step().await;
            render(&self.state);
        }
    }

    fn execute(&mut self, command: Command) {
        tracing::debug!(?command, "command");
        let backend = Arc::clone(&self.backend);
        let events = self.sender.clone();
        match command {
            Command::Restore => self.spawn(Task::Restore, None, async move {
                backend.restore().await.map(Event::Connected)
            }),
            Command::Connect { address, method } => {
                self.spawn(Task::Connect, None, async move {
                    backend
                        .connect(address, method, events)
                        .await
                        .map(Event::Connected)
                });
            }
            Command::Cancel(task) => self.cancel(task),
            Command::Disconnect => self.spawn(Task::Disconnect, None, async move {
                backend
                    .disconnect()
                    .await
                    .map(|()| Event::TaskDone(Task::Disconnect))
            }),
            Command::ListDrives => self.spawn(Task::Drives, None, async move {
                backend.list_drives().await.map(Event::DrivesListed)
            }),
            Command::ReadDisc { device } => self.spawn(Task::Disc, None, async move {
                match backend.read_disc(device.clone()).await {
                    Ok(disc) => Ok(Event::DiscDetected(Box::new(disc))),
                    // No music on it: a film disc, if this backend does those.
                    Err(AppError::Drive(DriveError::NoAudio)) if backend.film_supported() => {
                        events.send(Event::FilmScanning);
                        backend
                            .scan_film(device)
                            .await
                            .map(|scan| Event::FilmDetected(Box::new(scan)))
                    }
                    Err(error) => Err(error),
                }
            }),
            Command::FindReleases { disc } => self.spawn(Task::Releases, None, async move {
                backend
                    .find_releases(*disc)
                    .await
                    .map(Event::CandidatesFound)
            }),
            Command::SearchReleases { query } => self.spawn(Task::Releases, None, async move {
                backend
                    .search_releases(query)
                    .await
                    .map(Event::CandidatesFound)
            }),
            Command::LoadRelease { candidate, toc } => {
                self.spawn(Task::Release, None, async move {
                    backend
                        .load_release(*candidate, *toc)
                        .await
                        .map(|release| Event::ReleaseLoaded(Box::new(release)))
                });
            }
            Command::SearchItems { query } => self.spawn(Task::Items, None, async move {
                backend.search_items(query).await.map(Event::ItemsFound)
            }),
            Command::StartRip(job) => {
                let cancel = Arc::new(AtomicBool::new(false));
                let flag = Arc::clone(&cancel);
                self.spawn(Task::Rip, Some(cancel), async move {
                    backend
                        .rip(*job, events, flag)
                        .await
                        .map(|output| Event::RipFinished(Box::new(output)))
                });
            }
            Command::StartFilmRip(job) => {
                let cancel = Arc::new(AtomicBool::new(false));
                let flag = Arc::clone(&cancel);
                self.spawn(Task::Rip, Some(cancel), async move {
                    backend
                        .rip_film(*job, events, flag)
                        .await
                        .map(|output| Event::FilmRipped(Box::new(output)))
                });
            }
            Command::StartUpload { plan, folder } => {
                let cancel = Arc::new(AtomicBool::new(false));
                let flag = Arc::clone(&cancel);
                self.spawn(Task::Upload, Some(cancel), async move {
                    backend
                        .upload(*plan, folder, events, flag)
                        .await
                        .map(Event::UploadFinished)
                });
            }
            Command::Eject { device } => self.spawn(Task::Eject, None, async move {
                backend.eject(device).await.map(|()| Event::Ejected)
            }),
            Command::OpenUrl(url) => self.spawn(Task::OpenLink, None, async move {
                backend
                    .open_url(url)
                    .await
                    .map(|()| Event::TaskDone(Task::OpenLink))
            }),
            Command::SaveSettings(settings) => self.spawn(Task::Settings, None, async move {
                backend
                    .save_settings(settings)
                    .await
                    .map(|()| Event::SettingsSaved)
            }),
            Command::PickFolder => self.spawn(Task::Folder, None, async move {
                Ok(Event::FolderPicked(backend.pick_folder().await))
            }),
        }
    }

    /// Runs `work` in the background and sends its event (or failure).
    fn spawn(
        &mut self,
        task: Task,
        cancel: Option<Arc<AtomicBool>>,
        work: impl Future<Output = Result<Event>> + Send + 'static,
    ) {
        let events = self.sender.clone();
        let handle = tokio::spawn(async move {
            let event = match work.await {
                Ok(event) => event,
                Err(error) => failure(task, &error),
            };
            events.send(event);
        });
        let abort = handle.abort_handle();
        if let Some(previous) = self.running.insert(task, Running { abort, cancel })
            && previous.cancel.is_none()
            && is_exclusive(task)
        {
            previous.abort.abort();
        }
    }

    fn cancel(&mut self, task: Task) {
        let Some(running) = self.running.remove(&task) else {
            return;
        };
        match running.cancel {
            // Rip and upload stop cleanly and report Cancelled themselves.
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                self.running.insert(
                    task,
                    Running {
                        abort: running.abort,
                        cancel: Some(flag),
                    },
                );
            }
            // Anything else is simply dropped.
            None => running.abort.abort(),
        }
    }
}

/// Tasks where a newer request replaces an older one still running (a
/// second disc read, a second search).
fn is_exclusive(task: Task) -> bool {
    matches!(
        task,
        Task::Connect | Task::Restore | Task::Disc | Task::Releases | Task::Release | Task::Items
    )
}

fn failure(task: Task, error: &AppError) -> Event {
    if error.is_cancelled() {
        Event::Cancelled(task)
    } else {
        tracing::warn!(?task, %error, "task failed");
        Event::Failed {
            task,
            problem: error.problem(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pipeline::PipelineEvent;
    use crate::app::state::{ConnectPhase, DiscPhase, Screen, UploadPhase};
    use crate::library::LibraryError;
    use crate::library::api::LibraryRef;
    use crate::model::{
        DriveIdentity, ItemTarget, Lba, MediaKind, OffsetSource, ReadMode, SampleOffset, Sectors,
        Sha256Digest, TrackEntry, TrackNumber, UploadFile,
    };
    use std::sync::Mutex;
    use std::time::Duration;

    /// A backend that answers from canned values and records what it was
    /// asked to do.
    #[derive(Default)]
    struct Mock {
        saved: Option<String>,
        restore_fails: bool,
        rip_waits_for_cancel: bool,
        /// The disc is a film disc, and the backend does those.
        film: bool,
        upload_fails_once: Mutex<bool>,
        calls: Mutex<Vec<String>>,
        saved_settings: Mutex<Vec<Settings>>,
    }

    impl Mock {
        fn record(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn who() -> WhoAmI {
        WhoAmI {
            key_id: 1,
            key_name: "Test desktop".into(),
            library: LibraryRef {
                name: "The Library".into(),
                url: "http://library.test/".into(),
            },
        }
    }

    fn disc() -> DiscInfo {
        DiscInfo {
            drive: DriveIdentity::default(),
            toc: Toc::new(
                vec![TrackEntry {
                    number: TrackNumber::new(1).unwrap(),
                    start_lba: Lba(0),
                    length_sectors: Sectors(750),
                    is_audio: true,
                    pregap: Sectors(0),
                    isrc: None,
                    session: crate::model::SessionNumber::FIRST,
                }],
                Lba(750),
            )
            .unwrap(),
            mcn: None,
            cd_text: None,
        }
    }

    fn rip_output(job: &RipJob) -> RipOutput {
        let disc = disc();
        RipOutput {
            folder: PathBuf::from("/tmp/mock-rip"),
            kept: None,
            report: crate::model::RipReport {
                app_version: "test".into(),
                started_at: jiff::Timestamp::UNIX_EPOCH,
                finished_at: jiff::Timestamp::UNIX_EPOCH,
                drive: disc.drive.clone(),
                read_offset: SampleOffset(0),
                offset_source: OffsetSource::Unknown,
                read_mode: ReadMode::Secure,
                toc: disc.toc.clone(),
                disc_ids: crate::model::DiscIds {
                    musicbrainz: "x".into(),
                    cddb: 0,
                    accuraterip: crate::model::AccurateRipDiscId {
                        track_count: 1,
                        id1: 0,
                        id2: 0,
                        cddb: 0,
                    },
                    ctdb_toc: "x".into(),
                },
                mcn: None,
                release: job.release.clone(),
                tracks: Vec::new(),
                ctdb: crate::model::CtdbStatus::NotInDatabase,
                verdict: crate::model::DiscVerdict::default(),
            },
            plan: UploadPlan {
                item: job.target.clone(),
                cover: None,
                files: vec![UploadFile {
                    kind: MediaKind::Audio,
                    title: "Track 1".into(),
                    disc: 1,
                    track: TrackNumber::new(1).ok(),
                    duration_ms: Some(10_000),
                    sha256: Sha256Digest([2; 32]),
                    path: PathBuf::from("/tmp/mock-rip/01.flac"),
                }],
            },
        }
    }

    impl Backend for Mock {
        fn settings(&self) -> Settings {
            Settings::default()
        }
        fn saved_address(&self) -> Option<String> {
            self.saved.clone()
        }
        async fn restore(&self) -> Result<WhoAmI> {
            self.record("restore");
            if self.restore_fails {
                Err(LibraryError::Unauthorised.into())
            } else {
                Ok(who())
            }
        }
        async fn connect(
            &self,
            address: Url,
            method: ConnectMethod,
            events: EventSender,
        ) -> Result<WhoAmI> {
            self.record(format!("connect {address} {method:?}"));
            events.send(Event::DeviceCodeShown {
                user_code: "ABCD-1234".into(),
                verification_uri: format!("{address}connect/device"),
            });
            Ok(who())
        }
        async fn disconnect(&self) -> Result<()> {
            self.record("disconnect");
            Ok(())
        }
        async fn list_drives(&self) -> Result<Vec<DriveDescriptor>> {
            Ok(vec![DriveDescriptor {
                device: "sim".into(),
                label: "Simulated".into(),
            }])
        }
        async fn read_disc(&self, device: String) -> Result<DiscInfo> {
            self.record(format!("read {device}"));
            if self.film {
                return Err(AppError::Drive(DriveError::NoAudio));
            }
            Ok(disc())
        }
        fn film_supported(&self) -> bool {
            self.film
        }
        async fn scan_film(&self, device: String) -> Result<FilmScan> {
            self.record(format!("scan {device}"));
            Ok(FilmScan {
                disc_index: 0,
                drive_name: "Mock DVD".into(),
                disc: crate::video::scan::DiscScan::from_lines(
                    include_str!("../../tests/data/makemkv-info-dvd.txt").lines(),
                ),
                viewing: crate::app::film::ViewingSupport::Available {
                    encoder: "libsvtav1".into(),
                },
            })
        }
        async fn rip_film(
            &self,
            job: FilmJob,
            events: EventSender,
            _cancel: Arc<AtomicBool>,
        ) -> Result<FilmOutput> {
            self.record(format!("film {} titles {}", job.name, job.titles.len()));
            events.send(Event::FilmProgress(crate::app::film::FilmEvent::Finished {
                title: job.titles[0].id,
            }));
            Ok(FilmOutput {
                folder: PathBuf::from("/tmp/film"),
                kept: None,
                plan: UploadPlan {
                    item: ItemTarget::New(crate::model::NewItem {
                        type_slug: "film".into(),
                        title: job.name.clone(),
                        ..crate::model::NewItem::default()
                    }),
                    cover: None,
                    files: vec![UploadFile {
                        kind: MediaKind::Video,
                        title: job.name,
                        disc: 1,
                        track: None,
                        duration_ms: Some(1000),
                        sha256: Sha256Digest([3; 32]),
                        path: PathBuf::from("/tmp/film/a.webm"),
                    }],
                },
                notes: Vec::new(),
            })
        }
        async fn find_releases(&self, _disc: DiscInfo) -> Result<Vec<ReleaseCandidate>> {
            Err(crate::musicbrainz::MusicBrainzError::Busy.into())
        }
        async fn search_releases(&self, _query: String) -> Result<Vec<ReleaseCandidate>> {
            Ok(Vec::new())
        }
        async fn load_release(
            &self,
            _candidate: ReleaseCandidate,
            _toc: Toc,
        ) -> Result<ReleaseMetadata> {
            Ok(ReleaseMetadata::default())
        }
        async fn search_items(&self, _query: String) -> Result<Vec<ItemSummary>> {
            Ok(Vec::new())
        }
        async fn rip(
            &self,
            job: RipJob,
            events: EventSender,
            cancel: Arc<AtomicBool>,
        ) -> Result<RipOutput> {
            self.record(format!("rip {}", job.release.title));
            events.send(Event::Progress(PipelineEvent::Encoding {
                track: TrackNumber::new(1).unwrap(),
            }));
            if self.rip_waits_for_cancel {
                while !cancel.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                return Err(AppError::Cancelled);
            }
            Ok(rip_output(&job))
        }
        async fn upload(
            &self,
            plan: UploadPlan,
            _folder: PathBuf,
            events: EventSender,
            _cancel: Arc<AtomicBool>,
        ) -> Result<UploadOutput> {
            self.record(format!("upload {:?}", plan.item));
            events.send(Event::Progress(PipelineEvent::ItemReady {
                id: 3,
                url: "http://library.test/items/3".into(),
            }));
            let fail = std::mem::take(&mut *self.upload_fails_once.lock().unwrap());
            if fail {
                return Err(LibraryError::Status {
                    status: 500,
                    message: "oops".into(),
                }
                .into());
            }
            Ok(UploadOutput {
                item_id: 3,
                item_url: "http://library.test/items/3".into(),
                uploaded: plan.files.len(),
                skipped: 0,
                cover_problem: None,
            })
        }
        async fn eject(&self, device: String) -> Result<()> {
            self.record(format!("eject {device}"));
            Ok(())
        }
        async fn save_settings(&self, settings: Settings) -> Result<()> {
            self.saved_settings.lock().unwrap().push(settings);
            Ok(())
        }
        async fn pick_folder(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/picked"))
        }
        async fn open_url(&self, url: String) -> Result<()> {
            self.record(format!("open {url}"));
            Ok(())
        }
    }

    async fn settle<B: Backend>(
        controller: &mut Controller<B>,
        done: impl FnMut(&AppState) -> bool,
    ) {
        tokio::time::timeout(Duration::from_secs(5), controller.run_until(done))
            .await
            .expect("the controller did not reach the expected state");
    }

    #[tokio::test]
    async fn connects_rips_uploads_and_ejects() {
        let mut controller = Controller::new(Mock::default());
        let sender = controller.sender();
        sender.send(Event::AddressEdited("library.test".into()));
        sender.send(Event::ConnectPressed(ConnectMethod::DeviceCode));
        // The disc is read and the MusicBrainz failure is only a note.
        settle(&mut controller, |s| {
            s.disc_phase == DiscPhase::Ready && !s.is_busy(Task::Releases)
        })
        .await;
        assert_eq!(controller.state().screen, Screen::Disc);
        assert!(
            controller
                .state()
                .problem
                .as_ref()
                .unwrap()
                .message
                .contains("MusicBrainz")
        );

        sender.send(Event::AlbumTitleEdited("Test Album".into()));
        sender.send(Event::RipPressed);
        settle(&mut controller, |s| s.screen == Screen::Done).await;
        assert_eq!(
            controller.state().done.as_ref().unwrap().item_url,
            "http://library.test/items/3"
        );

        sender.send(Event::EjectPressed);
        settle(&mut controller, |s| {
            s.done.as_ref().is_some_and(|d| d.ejected)
        })
        .await;
        controller.dispatch(Event::OpenLink("http://library.test/items/3".into()));
        settle(&mut controller, |s| !s.is_busy(Task::OpenLink)).await;

        let calls = controller.backend().calls();
        assert_eq!(calls[0], "connect http://library.test/ DeviceCode");
        assert!(calls.contains(&"read sim".to_owned()));
        assert!(calls.contains(&"rip Test Album".to_owned()));
        assert!(calls.iter().any(|c| c.starts_with("upload New(")));
        assert!(calls.contains(&"eject sim".to_owned()));
        assert!(calls.contains(&"open http://library.test/items/3".to_owned()));
    }

    #[tokio::test]
    async fn a_disc_with_no_music_is_asked_about_as_a_film_and_ripped() {
        let mut controller = Controller::new(Mock {
            saved: Some("http://library.test/".into()),
            film: true,
            ..Mock::default()
        });
        let sender = controller.sender();
        settle(&mut controller, |s| s.film.is_some()).await;
        // No MusicBrainz, no "no music" banner: the film's name is ready.
        assert_eq!(controller.state().release.title, "The Library Card");
        assert!(controller.state().problem.is_none());
        assert_eq!(controller.state().disc_phase, DiscPhase::Ready);

        sender.send(Event::RipPressed);
        settle(&mut controller, |s| s.screen == Screen::Done).await;
        let calls = controller.backend().calls();
        assert!(calls.contains(&"scan sim".to_owned()), "{calls:?}");
        assert!(calls.contains(&"film The Library Card titles 1".to_owned()));
        assert!(calls.iter().any(|c| c.starts_with("upload New(")));
        assert_eq!(
            controller.state().done.as_ref().unwrap().item_url,
            "http://library.test/items/3"
        );
    }

    #[tokio::test]
    async fn a_backend_without_films_still_says_there_is_no_music() {
        struct NoFilms;
        impl Backend for NoFilms {
            fn settings(&self) -> Settings {
                Settings::default()
            }
            fn saved_address(&self) -> Option<String> {
                Some("http://library.test/".into())
            }
            async fn restore(&self) -> Result<WhoAmI> {
                Ok(who())
            }
            async fn connect(&self, _: Url, _: ConnectMethod, _: EventSender) -> Result<WhoAmI> {
                Ok(who())
            }
            async fn disconnect(&self) -> Result<()> {
                Ok(())
            }
            async fn list_drives(&self) -> Result<Vec<DriveDescriptor>> {
                Ok(vec![DriveDescriptor {
                    device: "sim".into(),
                    label: "Sim".into(),
                }])
            }
            async fn read_disc(&self, _: String) -> Result<DiscInfo> {
                Err(AppError::Drive(DriveError::NoAudio))
            }
            async fn find_releases(&self, _: DiscInfo) -> Result<Vec<ReleaseCandidate>> {
                Ok(Vec::new())
            }
            async fn search_releases(&self, _: String) -> Result<Vec<ReleaseCandidate>> {
                Ok(Vec::new())
            }
            async fn load_release(&self, _: ReleaseCandidate, _: Toc) -> Result<ReleaseMetadata> {
                Ok(ReleaseMetadata::default())
            }
            async fn search_items(&self, _: String) -> Result<Vec<ItemSummary>> {
                Ok(Vec::new())
            }
            async fn rip(&self, _: RipJob, _: EventSender, _: Arc<AtomicBool>) -> Result<RipOutput> {
                Err(AppError::Cancelled)
            }
            async fn upload(
                &self,
                _: UploadPlan,
                _: PathBuf,
                _: EventSender,
                _: Arc<AtomicBool>,
            ) -> Result<UploadOutput> {
                Err(AppError::Cancelled)
            }
            async fn eject(&self, _: String) -> Result<()> {
                Ok(())
            }
            async fn save_settings(&self, _: Settings) -> Result<()> {
                Ok(())
            }
            async fn pick_folder(&self) -> Option<PathBuf> {
                None
            }
            async fn open_url(&self, _: String) -> Result<()> {
                Ok(())
            }
        }
        let mut controller = Controller::new(NoFilms);
        settle(&mut controller, |s| s.disc_phase == DiscPhase::Empty).await;
        assert!(controller.state().film.is_none());
        assert!(controller.state().problem.as_ref().unwrap().message.contains("no music"));
    }

    #[tokio::test]
    async fn restores_and_falls_back_to_connect_when_revoked() {
        let mut controller = Controller::new(Mock {
            saved: Some("http://library.test/".into()),
            restore_fails: true,
            ..Mock::default()
        });
        settle(&mut controller, |s| {
            !s.is_busy(Task::Restore) && s.address.contains("library.test")
        })
        .await;
        let state = controller.state();
        assert_eq!(state.screen, Screen::Connect);
        assert_eq!(state.connect, ConnectPhase::Idle);
        assert!(state.problem.as_ref().unwrap().reconnect);
    }

    #[tokio::test]
    async fn cancelling_a_rip_stops_it() {
        let mut controller = Controller::new(Mock {
            saved: Some("http://library.test/".into()),
            rip_waits_for_cancel: true,
            ..Mock::default()
        });
        let sender = controller.sender();
        settle(&mut controller, |s| s.disc_phase == DiscPhase::Ready).await;
        sender.send(Event::AlbumTitleEdited("Album".into()));
        sender.send(Event::RipPressed);
        settle(&mut controller, |s| s.is_busy(Task::Rip)).await;
        sender.send(Event::CancelPressed);
        settle(&mut controller, |s| !s.is_busy(Task::Rip)).await;
        assert_eq!(controller.state().screen, Screen::Disc);
    }

    #[tokio::test]
    async fn a_failed_upload_retries_on_the_same_item() {
        let mut controller = Controller::new(Mock {
            saved: Some("http://library.test/".into()),
            upload_fails_once: Mutex::new(true),
            ..Mock::default()
        });
        let sender = controller.sender();
        settle(&mut controller, |s| s.disc_phase == DiscPhase::Ready).await;
        sender.send(Event::AlbumTitleEdited("Album".into()));
        sender.send(Event::RipPressed);
        settle(&mut controller, |s| {
            s.upload
                .as_ref()
                .is_some_and(|u| u.phase == UploadPhase::Stopped)
        })
        .await;
        assert!(
            controller
                .state()
                .problem
                .as_ref()
                .unwrap()
                .message
                .contains("500")
        );
        sender.send(Event::RetryUploadPressed);
        settle(&mut controller, |s| s.screen == Screen::Done).await;
        let calls = controller.backend().calls();
        let uploads: Vec<&String> = calls.iter().filter(|c| c.starts_with("upload")).collect();
        assert_eq!(uploads.len(), 2);
        assert!(uploads[0].starts_with("upload New("));
        assert_eq!(
            uploads[1],
            &format!("upload {:?}", ItemTarget::Existing { id: 3 })
        );
    }

    #[tokio::test]
    async fn settings_are_saved_and_the_folder_picked() {
        let mut controller = Controller::new(Mock::default());
        let sender = controller.sender();
        sender.send(Event::SettingsPressed);
        sender.send(Event::ChooseFolderPressed);
        settle(&mut controller, |s| s.settings.keep_local_copy.is_some()).await;
        settle(&mut controller, |s| !s.is_busy(Task::Settings)).await;
        let saved = controller.backend().saved_settings.lock().unwrap().clone();
        assert_eq!(
            saved.last().unwrap().keep_local_copy,
            Some(PathBuf::from("/picked"))
        );
    }
}
