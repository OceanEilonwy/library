// SPDX-License-Identifier: AGPL-3.0-or-later
//! The screen state machine, independent of Slint so it can be tested.
//!
//! The GUI forwards user actions, and the controller forwards the results
//! of background work, as [`Event`]s to [`AppState::handle`], which updates
//! the state and returns [`Command`]s for the controller to run (connect,
//! read the disc, rip, upload, …). Nothing here blocks or touches the
//! network, so every transition is a plain function call in the tests.

use std::collections::BTreeSet;
use std::path::PathBuf;

use url::Url;

use super::Problem;
use super::film::{FilmEvent, FilmJob, FilmOutput, FilmScan, ViewingSupport};
use super::pipeline::{PipelineEvent, RipOutput, UploadOutput};
use super::settings::Settings;
use crate::drive::DriveDescriptor;
use crate::library::api::{ItemSummary, WhoAmI};
use crate::model::{
    AccurateRipStatus, CtdbStatus, DiscInfo, ItemTarget, MediaKind, NewItem, OffsetSource,
    ReadMode, ReleaseMetadata, SampleOffset, Toc, TrackMetadata, TrackNumber, UploadPlan,
};
use crate::musicbrainz::ReleaseCandidate;

/// The screens of the ripper (docs/rip.md, "Ripper UI").
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// Type the library address and connect.
    #[default]
    Connect,
    /// Pick a drive, see the disc, confirm metadata.
    Disc,
    /// Reading and verifying.
    Rip,
    /// Uploading files.
    Upload,
    /// Finished: link to the item, eject.
    Done,
    /// Offset, read mode, local copy, disconnect, About.
    Settings,
}

/// How to connect.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConnectMethod {
    /// Open the library in this computer's browser (loopback + PKCE).
    Browser,
    /// Show a code to type on the library from any device.
    DeviceCode,
}

/// Where the Connect screen is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConnectPhase {
    /// Waiting for the user.
    #[default]
    Idle,
    /// Asking the library who it is.
    Checking,
    /// The browser was sent to this page; waiting for the librarian.
    WaitingForBrowser {
        /// The `/connect` page, in case the browser did not open.
        url: String,
    },
    /// Waiting for the librarian to type the code.
    WaitingForCode {
        /// The code, e.g. `WXYZ-1234`.
        user_code: String,
        /// Where to type it.
        verification_uri: String,
    },
}

/// Background work, so the state knows what is running and what Cancel
/// means.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Task {
    /// Restoring the saved connection at start-up.
    Restore,
    /// The connect handshake.
    Connect,
    /// Revoking the key.
    Disconnect,
    /// Listing drives.
    Drives,
    /// Reading the disc's table of contents.
    Disc,
    /// Looking up or searching MusicBrainz.
    Releases,
    /// Loading one release's details and cover.
    Release,
    /// Searching the library's catalogue.
    Items,
    /// Reading, verifying and encoding.
    Rip,
    /// Uploading.
    Upload,
    /// Ejecting.
    Eject,
    /// Saving settings.
    Settings,
    /// The folder picker.
    Folder,
    /// Opening a web page.
    OpenLink,
}

/// What the Disc screen knows about the drive.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum DiscPhase {
    /// Looking for drives or reading the disc.
    #[default]
    Looking,
    /// No drive was found.
    NoDrive,
    /// The drive is empty or the disc could not be read.
    Empty,
    /// A disc is in and its contents are known.
    Ready,
}

/// Where the rip goes on the library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Destination {
    /// A new catalogue entry made from the album details.
    #[default]
    NewItem,
    /// An entry already in the catalogue.
    Existing(ItemSummary),
}

/// Where one track is on the Rip screen.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TrackStage {
    /// Not started.
    #[default]
    Waiting,
    /// Being read (pass 2 is the test pass).
    Reading {
        /// Pass number.
        pass: u8,
    },
    /// Read; waiting for the others and verification.
    Read,
    /// Being written as FLAC.
    Encoding,
    /// Finished.
    Done,
}

/// One row of the Rip screen.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    /// Track number.
    pub number: TrackNumber,
    /// Title.
    pub title: String,
    /// Length.
    pub duration_ms: u64,
    /// Stage.
    pub stage: TrackStage,
    /// Fraction of the current pass, 0.0–1.0.
    pub fraction: f32,
    /// Sectors that never verified.
    pub unrecovered_sectors: usize,
    /// Test-and-copy: whether both reads matched.
    pub test_matched: Option<bool>,
    /// AccurateRip result once known.
    pub accuraterip: Option<AccurateRipStatus>,
}

/// Where the rip as a whole is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RipPhase {
    /// Reading, verifying, encoding.
    #[default]
    Running,
    /// Cancel was pressed; waiting for the reader to stop.
    Stopping,
    /// Finished, but some sectors could not be read reliably: the user
    /// decides whether to upload anyway or rip again.
    NeedsDecision {
        /// One plain sentence per problem.
        issues: Vec<String>,
    },
    /// The rip failed (the problem says why).
    Failed,
}

/// The Rip screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RipProgress {
    /// One row per audio track.
    pub rows: Vec<TrackRow>,
    /// Phase.
    pub phase: RipPhase,
    /// The offset applied, once known.
    pub offset: Option<(SampleOffset, OffsetSource)>,
    /// Latest read speed (× playing speed).
    pub speed: f32,
    /// Sectors re-read on the current track.
    pub retried_sectors: u32,
    /// CTDB result once known.
    pub ctdb: Option<CtdbStatus>,
}

/// Where one file is on the Upload screen.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum UploadStage {
    /// Not sent yet.
    #[default]
    Waiting,
    /// Being sent.
    Sending,
    /// On the library.
    Sent,
    /// Already on the library (same checksum), not sent again.
    AlreadyThere,
}

/// One row of the Upload screen.
#[derive(Clone, Debug, PartialEq)]
pub struct UploadRow {
    /// File title.
    pub title: String,
    /// Audio or archive.
    pub kind: MediaKind,
    /// Where it is.
    pub stage: UploadStage,
    /// Fraction sent, 0.0–1.0.
    pub fraction: f32,
}

/// Where the upload as a whole is.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum UploadPhase {
    /// Sending.
    #[default]
    Running,
    /// Cancel was pressed.
    Stopping,
    /// Stopped by a problem or by the user; can be retried.
    Stopped,
}

/// The Upload screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UploadProgress {
    /// One row per file.
    pub rows: Vec<UploadRow>,
    /// Phase.
    pub phase: UploadPhase,
    /// The item page, once the item exists.
    pub item_url: Option<String>,
}

/// The Done screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// The item page on the library.
    pub item_url: String,
    /// Files sent.
    pub uploaded: usize,
    /// Files that were already there.
    pub skipped: usize,
    /// The local copy, when kept.
    pub kept: Option<PathBuf>,
    /// Read problems (when the user chose to upload anyway).
    pub read_issues: Vec<String>,
    /// Verification notes and other remarks.
    pub notes: Vec<String>,
    /// The disc has been ejected.
    pub ejected: bool,
}

/// A film disc on the Disc screen: what MakeMKV found and what the user
/// has ticked.
#[derive(Clone, Debug, PartialEq)]
pub struct FilmSelection {
    /// What MakeMKV found.
    pub scan: FilmScan,
    /// MakeMKV title ids to copy.
    pub selected: BTreeSet<usize>,
    /// Make the WebM viewing copy (false: archive the master only).
    pub viewing_copy: bool,
    /// The titles of the running rip, in row order (MakeMKV ids).
    pub ripping: Vec<usize>,
}

impl FilmSelection {
    /// Can ffmpeg make viewing copies here?
    pub fn can_make_viewing_copy(&self) -> bool {
        matches!(self.scan.viewing, ViewingSupport::Available { .. })
    }
}

/// Something that happened: a user action or the result of background work.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    // ----- start-up -----
    /// The app started with these settings and saved library address.
    Started {
        /// Settings from disk.
        settings: Settings,
        /// The address of the saved connection, if any.
        address: Option<String>,
    },

    // ----- Connect screen -----
    /// The library address field changed.
    AddressEdited(String),
    /// Connect was pressed.
    ConnectPressed(ConnectMethod),
    /// The browser was sent to this page.
    BrowserOpened {
        /// The `/connect` page.
        url: String,
    },
    /// The library gave a code for the librarian to type.
    DeviceCodeShown {
        /// The code.
        user_code: String,
        /// Where to type it.
        verification_uri: String,
    },
    /// Connection established (or restored at start-up).
    Connected(WhoAmI),

    // ----- Disc screen -----
    /// Drives found.
    DrivesListed(Vec<DriveDescriptor>),
    /// The user picked a drive.
    DriveChosen(usize),
    /// "Look again": re-read the drive.
    RefreshPressed,
    /// A disc was read.
    DiscDetected(Box<DiscInfo>),
    /// The disc has no music: MakeMKV is being asked about it (film path).
    FilmScanning,
    /// MakeMKV read a film disc.
    FilmDetected(Box<FilmScan>),
    /// A title was ticked or unticked (index into the scan's titles).
    FilmTitleToggled(usize),
    /// "Archive the master only" was ticked or unticked.
    FilmViewingCopyToggled,
    /// Progress of a film rip.
    FilmProgress(FilmEvent),
    /// A film rip finished.
    FilmRipped(Box<FilmOutput>),
    /// MusicBrainz candidates arrived.
    CandidatesFound(Vec<ReleaseCandidate>),
    /// The user picked a candidate.
    CandidateChosen(usize),
    /// "None of these": type the details by hand.
    ManualEntryChosen,
    /// Search MusicBrainz by text.
    ReleaseSearchPressed(String),
    /// A release's details (and cover) arrived.
    ReleaseLoaded(Box<ReleaseMetadata>),
    /// Album title edited.
    AlbumTitleEdited(String),
    /// Album artist edited.
    AlbumArtistEdited(String),
    /// Year edited.
    YearEdited(String),
    /// A track title edited (`index` counts audio tracks from 0).
    TrackTitleEdited {
        /// Row index.
        index: usize,
        /// New title.
        title: String,
    },
    /// "New catalogue entry".
    NewItemChosen,
    /// "Add to existing…": search the catalogue.
    ItemSearchPressed(String),
    /// Catalogue search results.
    ItemsFound(Vec<ItemSummary>),
    /// The user picked an existing entry.
    ItemChosen(usize),
    /// The user confirmed the metadata and pressed Rip.
    RipPressed,

    // ----- Rip and Upload screens -----
    /// Progress from the pipeline.
    Progress(PipelineEvent),
    /// Reading, verifying and encoding finished.
    RipFinished(Box<RipOutput>),
    /// After read problems: upload anyway.
    UploadAnywayPressed,
    /// After read problems or a failure: rip again.
    RipAgainPressed,
    /// Back to the Disc screen (after a failure or stop).
    BackToDiscPressed,
    /// Try the upload again (resumes).
    RetryUploadPressed,
    /// Upload finished.
    UploadFinished(UploadOutput),

    // ----- Done screen -----
    /// Eject the disc.
    EjectPressed,
    /// The disc was ejected.
    Ejected,
    /// Rip another disc.
    AnotherDiscPressed,
    /// Open a web page (the item, the source code, the connect page).
    OpenLink(String),

    // ----- anywhere -----
    /// Cancel whatever is running.
    CancelPressed,
    /// Open Settings.
    SettingsPressed,
    /// Leave Settings.
    BackPressed,
    /// The read offset field changed (empty = look it up).
    OffsetEdited(String),
    /// Read mode picked.
    ReadModeChosen(ReadMode),
    /// The keep-a-copy folder field changed (empty = do not keep).
    KeepFolderEdited(String),
    /// "Choose…" next to the folder.
    ChooseFolderPressed,
    /// The folder picker closed (`None` = cancelled).
    FolderPicked(Option<PathBuf>),
    /// Settings saved.
    SettingsSaved,
    /// Disconnect from the library.
    DisconnectPressed,
    /// The problem banner was dismissed.
    DismissProblem,
    /// Background work finished with nothing more to say.
    TaskDone(Task),
    /// Background work failed.
    Failed {
        /// Which work.
        task: Task,
        /// What to tell the user.
        problem: Problem,
    },
    /// Background work was cancelled.
    Cancelled(Task),
}

/// Everything a rip needs, for [`Command::StartRip`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RipJob {
    /// The drive's device.
    pub device: String,
    /// The confirmed metadata.
    pub release: ReleaseMetadata,
    /// Where the files go.
    pub target: ItemTarget,
    /// Offset, read mode, local copy.
    pub settings: Settings,
}

/// Work for the controller to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Restore the saved connection.
    Restore,
    /// Run the connect handshake.
    Connect {
        /// The library.
        address: Url,
        /// How.
        method: ConnectMethod,
    },
    /// Stop a running task.
    Cancel(Task),
    /// Revoke the key and forget it.
    Disconnect,
    /// List drives.
    ListDrives,
    /// Read the disc in a drive.
    ReadDisc {
        /// The drive.
        device: String,
    },
    /// Look the disc up on MusicBrainz.
    FindReleases {
        /// The disc.
        disc: Box<DiscInfo>,
    },
    /// Search MusicBrainz by text.
    SearchReleases {
        /// The search.
        query: String,
    },
    /// Load a release's details and cover.
    LoadRelease {
        /// Which.
        candidate: Box<ReleaseCandidate>,
        /// The disc, so the tracks can be matched.
        toc: Box<Toc>,
    },
    /// Search the library's catalogue.
    SearchItems {
        /// The search.
        query: String,
    },
    /// Read, verify and encode.
    StartRip(Box<RipJob>),
    /// Copy film titles with MakeMKV and make the viewing copies.
    StartFilmRip(Box<FilmJob>),
    /// Upload (resumes when files are already there).
    StartUpload {
        /// What to send.
        plan: Box<UploadPlan>,
        /// The rip's work folder (removed after a successful upload).
        folder: PathBuf,
    },
    /// Eject.
    Eject {
        /// The drive.
        device: String,
    },
    /// Open a web page.
    OpenUrl(String),
    /// Save settings.
    SaveSettings(Settings),
    /// Show the folder picker.
    PickFolder,
}

/// Everything the GUI shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppState {
    /// Current screen.
    pub screen: Screen,
    /// Screen to return to from Settings.
    pub previous_screen: Option<Screen>,
    /// Work running in the background.
    pub busy: BTreeSet<Task>,
    /// The current problem, plain English, with what to do next.
    pub problem: Option<Problem>,

    /// Settings in force.
    pub settings: Settings,
    /// The offset field as typed.
    pub offset_text: String,
    /// The folder field as typed.
    pub keep_text: String,
    /// A problem with what was typed in Settings.
    pub settings_problem: Option<String>,

    /// The address field.
    pub address: String,
    /// The Connect screen's phase.
    pub connect: ConnectPhase,
    /// The connection, if any.
    pub connection: Option<WhoAmI>,

    /// Drives found.
    pub drives: Vec<DriveDescriptor>,
    /// The chosen drive (index into `drives`).
    pub drive: Option<usize>,
    /// What the drive holds.
    pub disc_phase: DiscPhase,
    /// The disc in the drive.
    pub disc: Option<DiscInfo>,
    /// MusicBrainz candidates.
    pub candidates: Vec<ReleaseCandidate>,
    /// The chosen candidate.
    pub candidate: Option<usize>,
    /// Metadata being edited / confirmed.
    pub release: ReleaseMetadata,
    /// The year field as typed.
    pub year_text: String,
    /// Bumped whenever `release` is replaced (not when the user edits it),
    /// so the window knows to refill its fields.
    pub release_revision: u64,
    /// Where the files will go.
    pub destination: Destination,
    /// Catalogue search results.
    pub items: Vec<ItemSummary>,

    /// The Rip screen.
    pub rip: Option<RipProgress>,
    /// The latest finished rip, ready to upload.
    pub output: Option<RipOutput>,
    /// A film disc is in the drive (instead of an audio CD).
    pub film: Option<FilmSelection>,
    /// MakeMKV is reading the disc.
    pub film_scanning: bool,
    /// The latest finished film rip, ready to upload.
    pub film_output: Option<FilmOutput>,
    /// The Upload screen.
    pub upload: Option<UploadProgress>,
    /// The Done screen.
    pub done: Option<Finished>,
}

/// The library's item type for audio CDs.
pub const MUSIC_TYPE: &str = "music";

impl AppState {
    /// True while `task` runs.
    pub fn is_busy(&self, task: Task) -> bool {
        self.busy.contains(&task)
    }

    /// The chosen drive.
    pub fn selected_drive(&self) -> Option<&DriveDescriptor> {
        self.drive.and_then(|index| self.drives.get(index))
    }

    /// Applies an event and returns what to do next.
    pub fn handle(&mut self, event: Event) -> Vec<Command> {
        match event {
            Event::Started { settings, address } => self.started(settings, address),
            Event::AddressEdited(text) => {
                self.address = text;
                Vec::new()
            }
            Event::ConnectPressed(method) => self.connect_pressed(method),
            Event::BrowserOpened { url } => {
                if self.is_busy(Task::Connect) {
                    self.connect = ConnectPhase::WaitingForBrowser { url };
                }
                Vec::new()
            }
            Event::DeviceCodeShown {
                user_code,
                verification_uri,
            } => {
                if self.is_busy(Task::Connect) {
                    self.connect = ConnectPhase::WaitingForCode {
                        user_code,
                        verification_uri,
                    };
                }
                Vec::new()
            }
            Event::Connected(who) => self.connected(who),
            Event::DrivesListed(drives) => self.drives_listed(drives),
            Event::DriveChosen(index) => self.drive_chosen(index),
            Event::RefreshPressed => self.refresh(),
            Event::DiscDetected(disc) => self.disc_detected(*disc),
            Event::FilmScanning => {
                self.film_scanning = true;
                Vec::new()
            }
            Event::FilmDetected(scan) => self.film_detected(*scan),
            Event::FilmTitleToggled(index) => {
                if let Some(film) = &mut self.film
                    && let Some(title) = film.scan.disc.titles.get(index)
                    && !film.selected.remove(&title.id)
                {
                    film.selected.insert(title.id);
                }
                Vec::new()
            }
            Event::FilmViewingCopyToggled => {
                if let Some(film) = &mut self.film
                    && film.can_make_viewing_copy()
                {
                    film.viewing_copy = !film.viewing_copy;
                }
                Vec::new()
            }
            Event::FilmProgress(progress) => {
                self.film_progress(progress);
                Vec::new()
            }
            Event::FilmRipped(output) => self.film_ripped(*output),
            Event::CandidatesFound(candidates) => self.candidates_found(candidates),
            Event::CandidateChosen(index) => self.candidate_chosen(index),
            Event::ManualEntryChosen => {
                self.candidate = None;
                if let Some(disc) = &self.disc {
                    self.set_release(release_from_disc(disc));
                }
                Vec::new()
            }
            Event::ReleaseSearchPressed(query) => {
                let query = query.trim().to_owned();
                if query.is_empty() || !self.require_connection() {
                    return Vec::new();
                }
                self.busy.insert(Task::Releases);
                vec![Command::SearchReleases { query }]
            }
            Event::ReleaseLoaded(release) => {
                self.busy.remove(&Task::Release);
                self.set_release(*release);
                Vec::new()
            }
            Event::AlbumTitleEdited(title) => {
                self.release.title = title;
                Vec::new()
            }
            Event::AlbumArtistEdited(artist) => {
                self.release.artist = artist;
                Vec::new()
            }
            Event::YearEdited(text) => {
                self.release.year = parse_year(&text);
                self.year_text = text;
                Vec::new()
            }
            Event::TrackTitleEdited { index, title } => {
                if let Some(track) = self.release.tracks.get_mut(index) {
                    track.title = title;
                }
                Vec::new()
            }
            Event::NewItemChosen => {
                self.destination = Destination::NewItem;
                Vec::new()
            }
            Event::ItemSearchPressed(query) => {
                if !self.require_connection() {
                    return Vec::new();
                }
                self.busy.insert(Task::Items);
                vec![Command::SearchItems {
                    query: query.trim().to_owned(),
                }]
            }
            Event::ItemsFound(items) => {
                self.busy.remove(&Task::Items);
                self.items = items;
                Vec::new()
            }
            Event::ItemChosen(index) => {
                if let Some(item) = self.items.get(index) {
                    self.destination = Destination::Existing(item.clone());
                }
                Vec::new()
            }
            Event::RipPressed | Event::RipAgainPressed => self.start_rip(),
            Event::Progress(progress) => {
                self.progress(progress);
                Vec::new()
            }
            Event::RipFinished(output) => self.rip_finished(*output),
            Event::UploadAnywayPressed => {
                if matches!(
                    self.rip.as_ref().map(|rip| &rip.phase),
                    Some(RipPhase::NeedsDecision { .. })
                ) {
                    self.start_upload()
                } else {
                    Vec::new()
                }
            }
            Event::BackToDiscPressed => {
                if !self.is_busy(Task::Rip) && !self.is_busy(Task::Upload) {
                    self.rip = None;
                    self.upload = None;
                    self.screen = Screen::Disc;
                }
                Vec::new()
            }
            Event::RetryUploadPressed => {
                if self.upload.as_ref().map(|upload| upload.phase) == Some(UploadPhase::Stopped) {
                    self.start_upload()
                } else {
                    Vec::new()
                }
            }
            Event::UploadFinished(output) => self.upload_finished(output),
            Event::EjectPressed => match self.selected_drive() {
                Some(drive) if !self.is_busy(Task::Rip) => {
                    let device = drive.device.clone();
                    self.busy.insert(Task::Eject);
                    vec![Command::Eject { device }]
                }
                _ => Vec::new(),
            },
            Event::Ejected => {
                self.busy.remove(&Task::Eject);
                if let Some(done) = &mut self.done {
                    done.ejected = true;
                }
                self.clear_disc();
                self.disc_phase = DiscPhase::Empty;
                Vec::new()
            }
            Event::AnotherDiscPressed => self.another_disc(),
            Event::OpenLink(url) => {
                self.busy.insert(Task::OpenLink);
                vec![Command::OpenUrl(url)]
            }
            Event::CancelPressed => self.cancel(),
            Event::SettingsPressed => {
                if self.screen != Screen::Settings {
                    self.previous_screen = Some(self.screen);
                    self.screen = Screen::Settings;
                }
                Vec::new()
            }
            Event::BackPressed => {
                if self.screen == Screen::Settings {
                    self.screen = self.previous_screen.take().unwrap_or(self.home_screen());
                }
                Vec::new()
            }
            Event::OffsetEdited(text) => self.offset_edited(text),
            Event::ReadModeChosen(mode) => {
                self.settings.read_mode = mode;
                self.save_settings()
            }
            Event::KeepFolderEdited(text) => {
                let trimmed = text.trim();
                self.settings.keep_local_copy =
                    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
                self.keep_text = text;
                self.save_settings()
            }
            Event::ChooseFolderPressed => {
                if self.is_busy(Task::Folder) {
                    return Vec::new();
                }
                self.busy.insert(Task::Folder);
                vec![Command::PickFolder]
            }
            Event::FolderPicked(folder) => {
                self.busy.remove(&Task::Folder);
                match folder {
                    Some(folder) => {
                        self.keep_text = folder.display().to_string();
                        self.settings.keep_local_copy = Some(folder);
                        self.save_settings()
                    }
                    None => Vec::new(),
                }
            }
            Event::SettingsSaved => {
                self.busy.remove(&Task::Settings);
                Vec::new()
            }
            Event::DisconnectPressed => self.disconnect_pressed(),
            Event::DismissProblem => {
                self.problem = None;
                Vec::new()
            }
            Event::TaskDone(task) => {
                self.busy.remove(&task);
                if task == Task::Releases || task == Task::Release {
                    self.busy.remove(&Task::Releases);
                    self.busy.remove(&Task::Release);
                }
                Vec::new()
            }
            Event::Failed { task, problem } => self.failed(task, problem),
            Event::Cancelled(task) => self.cancelled(task),
        }
    }

    // ----- start-up and connecting -----

    fn started(&mut self, settings: Settings, address: Option<String>) -> Vec<Command> {
        self.offset_text = settings
            .read_offset
            .map(|offset| offset.0.to_string())
            .unwrap_or_default();
        self.keep_text = settings
            .keep_local_copy
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.settings = settings;
        match address {
            Some(address) => {
                self.address = address;
                self.busy.insert(Task::Restore);
                self.connect = ConnectPhase::Checking;
                vec![Command::Restore]
            }
            None => Vec::new(),
        }
    }

    fn connect_pressed(&mut self, method: ConnectMethod) -> Vec<Command> {
        if self.is_busy(Task::Connect) || self.is_busy(Task::Restore) {
            return Vec::new();
        }
        match library_address(&self.address) {
            Ok(address) => {
                self.problem = None;
                self.address = address.to_string();
                self.connect = ConnectPhase::Checking;
                self.busy.insert(Task::Connect);
                vec![Command::Connect { address, method }]
            }
            Err(problem) => {
                self.problem = Some(problem);
                Vec::new()
            }
        }
    }

    fn connected(&mut self, who: WhoAmI) -> Vec<Command> {
        self.busy.remove(&Task::Connect);
        self.busy.remove(&Task::Restore);
        self.connect = ConnectPhase::Idle;
        self.connection = Some(who);
        self.problem = None;
        if self.screen == Screen::Settings {
            self.previous_screen = Some(Screen::Disc);
        } else {
            self.screen = Screen::Disc;
        }
        self.disc_phase = DiscPhase::Looking;
        self.busy.insert(Task::Drives);
        vec![Command::ListDrives]
    }

    fn disconnect_pressed(&mut self) -> Vec<Command> {
        if self.is_busy(Task::Rip) || self.is_busy(Task::Upload) {
            self.problem = Some(Problem::new(
                "Library Rip is still working.",
                "Wait for the rip and upload to finish, or stop them first.",
            ));
            return Vec::new();
        }
        if self.connection.is_none() {
            return Vec::new();
        }
        self.forget_connection();
        self.busy.insert(Task::Disconnect);
        vec![Command::Disconnect]
    }

    fn forget_connection(&mut self) {
        // Anything still running reports back harmlessly later.
        self.busy.clear();
        self.connection = None;
        self.connect = ConnectPhase::Idle;
        self.clear_disc();
        self.drives.clear();
        self.drive = None;
        self.rip = None;
        self.output = None;
        self.upload = None;
        self.done = None;
        if self.screen == Screen::Settings {
            self.previous_screen = Some(Screen::Connect);
        } else {
            self.screen = Screen::Connect;
        }
    }

    fn require_connection(&mut self) -> bool {
        if self.connection.is_some() {
            return true;
        }
        self.problem = Some(Problem::reconnect(
            "This computer is not connected to a library.",
            "Connect to your library first.",
        ));
        false
    }

    fn home_screen(&self) -> Screen {
        if self.connection.is_some() {
            Screen::Disc
        } else {
            Screen::Connect
        }
    }

    // ----- the disc -----

    fn drives_listed(&mut self, drives: Vec<DriveDescriptor>) -> Vec<Command> {
        self.busy.remove(&Task::Drives);
        // Keep the same drive chosen when it is still there.
        let previous = self.selected_drive().map(|drive| drive.device.clone());
        self.drives = drives;
        self.drive = previous
            .and_then(|device| self.drives.iter().position(|d| d.device == device))
            .or(if self.drives.is_empty() {
                None
            } else {
                Some(0)
            });
        match self.selected_drive() {
            Some(drive) => {
                let device = drive.device.clone();
                self.read_disc(device)
            }
            None => {
                self.disc_phase = DiscPhase::NoDrive;
                Vec::new()
            }
        }
    }

    fn drive_chosen(&mut self, index: usize) -> Vec<Command> {
        if self.is_busy(Task::Rip) || index >= self.drives.len() || self.drive == Some(index) {
            return Vec::new();
        }
        self.drive = Some(index);
        match self.selected_drive() {
            Some(drive) => {
                let device = drive.device.clone();
                self.read_disc(device)
            }
            None => Vec::new(),
        }
    }

    fn refresh(&mut self) -> Vec<Command> {
        if self.is_busy(Task::Rip) || self.connection.is_none() {
            return Vec::new();
        }
        self.problem = None;
        match self.selected_drive() {
            Some(drive) => {
                let device = drive.device.clone();
                self.read_disc(device)
            }
            None => {
                self.disc_phase = DiscPhase::Looking;
                self.busy.insert(Task::Drives);
                vec![Command::ListDrives]
            }
        }
    }

    fn read_disc(&mut self, device: String) -> Vec<Command> {
        self.clear_disc();
        self.disc_phase = DiscPhase::Looking;
        self.busy.insert(Task::Disc);
        vec![Command::ReadDisc { device }]
    }

    fn clear_disc(&mut self) {
        self.disc = None;
        self.film = None;
        self.film_scanning = false;
        self.candidates.clear();
        self.candidate = None;
        self.release = ReleaseMetadata::default();
        self.release_revision = self.release_revision.wrapping_add(1);
        self.year_text.clear();
        self.destination = Destination::NewItem;
        self.items.clear();
    }

    fn disc_detected(&mut self, disc: DiscInfo) -> Vec<Command> {
        self.busy.remove(&Task::Disc);
        self.set_release(release_from_disc(&disc));
        self.disc_phase = DiscPhase::Ready;
        self.disc = Some(disc.clone());
        self.busy.insert(Task::Releases);
        vec![Command::FindReleases {
            disc: Box::new(disc),
        }]
    }

    fn film_detected(&mut self, scan: FilmScan) -> Vec<Command> {
        self.busy.remove(&Task::Disc);
        self.film_scanning = false;
        let name = if scan.disc.name.trim().is_empty() {
            &scan.disc.volume
        } else {
            &scan.disc.name
        };
        let release = ReleaseMetadata {
            title: crate::video::scan::clean_name(name),
            ..ReleaseMetadata::default()
        };
        self.disc = None;
        self.set_release(release);
        self.disc_phase = DiscPhase::Ready;
        let main = scan.disc.main_title().map(|title| title.id);
        self.film = Some(FilmSelection {
            selected: main.into_iter().collect(),
            viewing_copy: matches!(scan.viewing, ViewingSupport::Available { .. }),
            ripping: Vec::new(),
            scan,
        });
        Vec::new()
    }

    fn start_film_rip(&mut self) -> Vec<Command> {
        let Some(film) = &self.film else {
            return Vec::new();
        };
        let Some(drive) = self.selected_drive() else {
            self.problem = Some(Problem::new(
                "There is no disc to rip.",
                "Put in a disc and press Look again.",
            ));
            return Vec::new();
        };
        let name = self.release.title.trim().to_owned();
        if name.is_empty() {
            self.problem = Some(Problem::new(
                "The film needs a title before it is ripped.",
                "Type it in the Title box.",
            ));
            return Vec::new();
        }
        let titles: Vec<_> = film
            .scan
            .disc
            .titles
            .iter()
            .filter(|title| film.selected.contains(&title.id))
            .take(99)
            .cloned()
            .collect();
        if titles.is_empty() {
            self.problem = Some(Problem::new(
                "No title is ticked.",
                "Tick the film (usually the longest title) in the list, then press Rip.",
            ));
            return Vec::new();
        }
        let rows = titles
            .iter()
            .enumerate()
            .filter_map(|(index, title)| {
                Some(TrackRow {
                    number: TrackNumber::new(u8::try_from(index + 1).ok()?).ok()?,
                    title: if title.name.trim().is_empty() {
                        name.clone()
                    } else {
                        title.name.clone()
                    },
                    duration_ms: title.duration_secs * 1000,
                    stage: TrackStage::Waiting,
                    fraction: 0.0,
                    unrecovered_sectors: 0,
                    test_matched: None,
                    accuraterip: None,
                })
            })
            .collect();
        let job = FilmJob {
            device: drive.device.clone(),
            disc_index: film.scan.disc_index,
            disc: film.scan.disc.clone(),
            name,
            year: self.release.year,
            titles: titles.clone(),
            viewing_copy: film.viewing_copy,
            settings: self.settings.clone(),
        };
        if let Some(film) = &mut self.film {
            film.ripping = titles.iter().map(|title| title.id).collect();
        }
        self.problem = None;
        self.output = None;
        self.film_output = None;
        self.upload = None;
        self.done = None;
        self.rip = Some(RipProgress {
            rows,
            ..RipProgress::default()
        });
        self.screen = Screen::Rip;
        self.busy.insert(Task::Rip);
        vec![Command::StartFilmRip(Box::new(job))]
    }

    fn film_progress(&mut self, event: FilmEvent) {
        let (Some(film), Some(rip)) = (&self.film, &mut self.rip) else {
            return;
        };
        let (title, stage, fraction) = match event {
            FilmEvent::Copying { title, fraction } => {
                (title, TrackStage::Reading { pass: 1 }, fraction)
            }
            FilmEvent::Viewing { title, fraction } => (title, TrackStage::Encoding, fraction),
            FilmEvent::Finished { title } => (title, TrackStage::Done, 1.0),
        };
        if let Some(row) = film
            .ripping
            .iter()
            .position(|id| *id == title)
            .and_then(|index| rip.rows.get_mut(index))
        {
            row.stage = stage;
            row.fraction = fraction;
        }
    }

    fn film_ripped(&mut self, output: FilmOutput) -> Vec<Command> {
        self.busy.remove(&Task::Rip);
        if let Some(rip) = &mut self.rip {
            for row in &mut rip.rows {
                row.stage = TrackStage::Done;
                row.fraction = 1.0;
            }
            rip.phase = RipPhase::Running;
        }
        self.film_output = Some(output);
        self.start_upload()
    }

    fn candidates_found(&mut self, candidates: Vec<ReleaseCandidate>) -> Vec<Command> {
        self.busy.remove(&Task::Releases);
        self.candidates = candidates;
        self.candidate = None;
        if self.candidates.is_empty() {
            return Vec::new();
        }
        self.candidate_chosen(0)
    }

    fn candidate_chosen(&mut self, index: usize) -> Vec<Command> {
        let (Some(candidate), Some(disc)) = (self.candidates.get(index), &self.disc) else {
            return Vec::new();
        };
        let command = Command::LoadRelease {
            candidate: Box::new(candidate.clone()),
            toc: Box::new(disc.toc.clone()),
        };
        self.candidate = Some(index);
        self.busy.insert(Task::Release);
        vec![command]
    }

    fn set_release(&mut self, release: ReleaseMetadata) {
        let mut release = release;
        if let Some(disc) = &self.disc {
            fit_tracks(&mut release, &disc.toc);
        }
        self.year_text = release.year.map(|y| y.to_string()).unwrap_or_default();
        self.release = release;
        self.release_revision = self.release_revision.wrapping_add(1);
    }

    // ----- ripping -----

    fn start_rip(&mut self) -> Vec<Command> {
        if self.is_busy(Task::Rip) || self.is_busy(Task::Upload) {
            return Vec::new();
        }
        if !self.require_connection() {
            return Vec::new();
        }
        if self.film.is_some() {
            return self.start_film_rip();
        }
        let (Some(disc), Some(drive)) = (&self.disc, self.selected_drive()) else {
            self.problem = Some(Problem::new(
                "There is no disc to rip.",
                "Put in an audio CD and press Look again.",
            ));
            return Vec::new();
        };
        if self.release.title.trim().is_empty() {
            self.problem = Some(Problem::new(
                "The album needs a title before it is ripped.",
                "Type it in the Album box.",
            ));
            return Vec::new();
        }
        let device = drive.device.clone();
        let mut release = self.release.clone();
        fit_tracks(&mut release, &disc.toc);
        let rows = disc
            .toc
            .audio_tracks()
            .enumerate()
            .map(|(index, entry)| TrackRow {
                number: entry.number,
                title: release
                    .tracks
                    .get(index)
                    .map(|track| track.title.clone())
                    .unwrap_or_default(),
                duration_ms: entry.length_sectors.duration_ms(),
                stage: TrackStage::Waiting,
                fraction: 0.0,
                unrecovered_sectors: 0,
                test_matched: None,
                accuraterip: None,
            })
            .collect();
        let target = match &self.destination {
            Destination::NewItem => ItemTarget::New(new_item(&release)),
            Destination::Existing(item) => ItemTarget::Existing { id: item.id },
        };
        self.problem = None;
        self.output = None;
        self.upload = None;
        self.done = None;
        self.rip = Some(RipProgress {
            rows,
            ..RipProgress::default()
        });
        self.screen = Screen::Rip;
        self.busy.insert(Task::Rip);
        vec![Command::StartRip(Box::new(RipJob {
            device,
            release,
            target,
            settings: self.settings.clone(),
        }))]
    }

    fn progress(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::Offset { offset, source } => {
                if let Some(rip) = &mut self.rip {
                    rip.offset = Some((offset, source));
                }
            }
            PipelineEvent::Reading {
                track,
                pass,
                fraction,
                speed,
                retried_sectors,
            } => {
                if let Some(rip) = &mut self.rip {
                    rip.speed = speed;
                    rip.retried_sectors = retried_sectors;
                    if let Some(row) = rip.rows.iter_mut().find(|row| row.number == track) {
                        row.stage = TrackStage::Reading { pass };
                        row.fraction = fraction;
                    }
                }
            }
            PipelineEvent::TrackRead {
                track,
                unrecovered_sectors,
                test_matched,
            } => {
                if let Some(row) = self.row_mut(track) {
                    row.stage = TrackStage::Read;
                    row.fraction = 1.0;
                    row.unrecovered_sectors = unrecovered_sectors;
                    row.test_matched = test_matched;
                }
            }
            PipelineEvent::Verified { tracks, ctdb } => {
                if let Some(rip) = &mut self.rip {
                    rip.ctdb = Some(ctdb);
                    for (track, status) in tracks {
                        if let Some(row) = rip.rows.iter_mut().find(|row| row.number == track) {
                            row.accuraterip = Some(status);
                        }
                    }
                }
            }
            PipelineEvent::Encoding { track } => {
                if let Some(rip) = &mut self.rip {
                    for row in &mut rip.rows {
                        if row.number == track {
                            row.stage = TrackStage::Encoding;
                        } else if row.stage == TrackStage::Encoding {
                            row.stage = TrackStage::Done;
                        }
                    }
                }
            }
            PipelineEvent::ItemReady { id, url } => {
                // A retry must not create a second item.
                if let Some(output) = &mut self.output {
                    output.plan.item = ItemTarget::Existing { id };
                }
                if let Some(output) = &mut self.film_output {
                    output.plan.item = ItemTarget::Existing { id };
                }
                if let Some(upload) = &mut self.upload {
                    upload.item_url = Some(url);
                }
            }
            PipelineEvent::Uploading { index, fraction } => {
                if let Some(row) = self.upload_row_mut(index) {
                    row.stage = UploadStage::Sending;
                    row.fraction = fraction;
                }
            }
            PipelineEvent::Uploaded { index, skipped } => {
                if let Some(row) = self.upload_row_mut(index) {
                    row.stage = if skipped {
                        UploadStage::AlreadyThere
                    } else {
                        UploadStage::Sent
                    };
                    row.fraction = 1.0;
                }
            }
        }
    }

    fn row_mut(&mut self, track: TrackNumber) -> Option<&mut TrackRow> {
        self.rip
            .as_mut()
            .and_then(|rip| rip.rows.iter_mut().find(|row| row.number == track))
    }

    fn upload_row_mut(&mut self, index: usize) -> Option<&mut UploadRow> {
        self.upload
            .as_mut()
            .and_then(|upload| upload.rows.get_mut(index))
    }

    fn rip_finished(&mut self, output: RipOutput) -> Vec<Command> {
        self.busy.remove(&Task::Rip);
        let issues = output.read_issues();
        if let Some(rip) = &mut self.rip {
            for row in &mut rip.rows {
                row.stage = TrackStage::Done;
                row.fraction = 1.0;
            }
            rip.phase = if issues.is_empty() {
                RipPhase::Running
            } else {
                RipPhase::NeedsDecision {
                    issues: issues.clone(),
                }
            };
        }
        self.output = Some(output);
        if issues.is_empty() {
            self.start_upload()
        } else {
            Vec::new()
        }
    }

    fn start_upload(&mut self) -> Vec<Command> {
        if self.is_busy(Task::Upload) || !self.require_connection() {
            return Vec::new();
        }
        let (plan, folder) = match (&self.output, &self.film_output) {
            (Some(output), _) => (output.plan.clone(), output.folder.clone()),
            (None, Some(output)) => (output.plan.clone(), output.folder.clone()),
            (None, None) => return Vec::new(),
        };
        let item_url = self.upload.as_ref().and_then(|u| u.item_url.clone());
        let previous_rows = self.upload.take().map(|u| u.rows).unwrap_or_default();
        let rows = plan
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                // Rows already sent stay ticked on a retry.
                let stage = previous_rows
                    .get(index)
                    .map(|row| row.stage)
                    .filter(|stage| matches!(stage, UploadStage::Sent | UploadStage::AlreadyThere))
                    .unwrap_or_default();
                UploadRow {
                    title: file.title.clone(),
                    kind: file.kind,
                    fraction: if stage == UploadStage::Waiting {
                        0.0
                    } else {
                        1.0
                    },
                    stage,
                }
            })
            .collect();
        self.problem = None;
        self.upload = Some(UploadProgress {
            rows,
            phase: UploadPhase::Running,
            item_url,
        });
        self.screen = Screen::Upload;
        self.busy.insert(Task::Upload);
        vec![Command::StartUpload {
            plan: Box::new(plan),
            folder,
        }]
    }

    fn upload_finished(&mut self, result: UploadOutput) -> Vec<Command> {
        self.busy.remove(&Task::Upload);
        let (read_issues, mut notes, kept) = match (&self.output, &self.film_output) {
            (Some(output), _) => (
                output.read_issues(),
                output.verification_notes(),
                output.kept.clone(),
            ),
            (None, Some(film)) => (Vec::new(), film.notes.clone(), film.kept.clone()),
            (None, None) => (Vec::new(), Vec::new(), None),
        };
        if let Some(problem) = &result.cover_problem {
            notes.push(format!(
                "The cover could not be added ({problem}). You can add it on the item page."
            ));
        }
        self.done = Some(Finished {
            item_url: result.item_url,
            uploaded: result.uploaded,
            skipped: result.skipped,
            kept,
            read_issues,
            notes,
            ejected: false,
        });
        self.upload = None;
        self.rip = None;
        self.output = None;
        self.film_output = None;
        self.problem = None;
        self.screen = Screen::Done;
        Vec::new()
    }

    fn another_disc(&mut self) -> Vec<Command> {
        if self.is_busy(Task::Rip) || self.is_busy(Task::Upload) {
            return Vec::new();
        }
        self.done = None;
        self.rip = None;
        self.upload = None;
        self.output = None;
        self.film_output = None;
        self.screen = Screen::Disc;
        self.refresh()
    }

    // ----- cancelling and failures -----

    fn cancel(&mut self) -> Vec<Command> {
        if self.is_busy(Task::Rip) {
            if let Some(rip) = &mut self.rip {
                rip.phase = RipPhase::Stopping;
            }
            return vec![Command::Cancel(Task::Rip)];
        }
        if self.is_busy(Task::Upload) {
            if let Some(upload) = &mut self.upload {
                upload.phase = UploadPhase::Stopping;
            }
            return vec![Command::Cancel(Task::Upload)];
        }
        for task in [Task::Connect, Task::Restore] {
            if self.is_busy(task) {
                self.busy.remove(&task);
                self.connect = ConnectPhase::Idle;
                return vec![Command::Cancel(task)];
            }
        }
        Vec::new()
    }

    fn cancelled(&mut self, task: Task) -> Vec<Command> {
        self.busy.remove(&task);
        match task {
            Task::Rip => {
                self.rip = None;
                self.screen = Screen::Disc;
                self.problem = Some(Problem::new(
                    "The rip was stopped. Nothing was uploaded.",
                    "Press Rip to start again.",
                ));
            }
            Task::Upload => {
                if let Some(upload) = &mut self.upload {
                    upload.phase = UploadPhase::Stopped;
                }
                self.problem = Some(Problem::new(
                    "The upload was stopped.",
                    "Press Try again to send the rest. Files already sent are not sent twice.",
                ));
            }
            Task::Connect | Task::Restore => self.connect = ConnectPhase::Idle,
            Task::Disconnect
            | Task::Drives
            | Task::Disc
            | Task::Releases
            | Task::Release
            | Task::Items
            | Task::Eject
            | Task::Settings
            | Task::Folder
            | Task::OpenLink => {}
        }
        Vec::new()
    }

    fn failed(&mut self, task: Task, problem: Problem) -> Vec<Command> {
        self.busy.remove(&task);
        if problem.reconnect && task != Task::Disconnect {
            // The library no longer knows us: back to Connect.
            self.busy.remove(&Task::Rip);
            self.busy.remove(&Task::Upload);
            self.forget_connection();
            self.problem = Some(problem);
            return Vec::new();
        }
        match task {
            Task::Restore | Task::Connect => self.connect = ConnectPhase::Idle,
            Task::Drives => self.disc_phase = DiscPhase::NoDrive,
            Task::Disc => {
                self.film_scanning = false;
                self.disc_phase = DiscPhase::Empty;
            }
            Task::Rip => {
                if let Some(rip) = &mut self.rip {
                    rip.phase = RipPhase::Failed;
                }
            }
            Task::Upload => {
                if let Some(upload) = &mut self.upload {
                    upload.phase = UploadPhase::Stopped;
                }
            }
            Task::Settings => {
                self.settings_problem = Some(problem.message);
                return Vec::new();
            }
            Task::Disconnect
            | Task::Releases
            | Task::Release
            | Task::Items
            | Task::Eject
            | Task::Folder
            | Task::OpenLink => {}
        }
        self.problem = Some(problem);
        Vec::new()
    }

    // ----- settings -----

    fn offset_edited(&mut self, text: String) -> Vec<Command> {
        let parsed = parse_offset(&text);
        self.offset_text = text;
        match parsed {
            Ok(offset) => {
                self.settings.read_offset = offset;
                self.save_settings()
            }
            Err(message) => {
                self.settings_problem = Some(message);
                Vec::new()
            }
        }
    }

    fn save_settings(&mut self) -> Vec<Command> {
        self.settings_problem = None;
        self.busy.insert(Task::Settings);
        vec![Command::SaveSettings(self.settings.clone())]
    }
}

/// The largest read offset accepted (drives in the AccurateRip list stay
/// well inside ±3000 samples).
pub const MAX_OFFSET: i32 = 5000;

/// Parses the offset field: empty means "look it up".
pub fn parse_offset(text: &str) -> Result<Option<SampleOffset>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.trim_start_matches('+').parse::<i32>() {
        Ok(value) if (-MAX_OFFSET..=MAX_OFFSET).contains(&value) => Ok(Some(SampleOffset(value))),
        _ => Err(format!(
            "Type a whole number of samples between -{MAX_OFFSET} and {MAX_OFFSET}, like 6 or -472, \
             or leave it empty to look it up."
        )),
    }
}

fn parse_year(text: &str) -> Option<u16> {
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|year| (1000..=9999).contains(year))
}

/// Turns what the user typed into a library base address: adds `http://`
/// when no scheme is given and a trailing slash, drops any query.
pub fn library_address(text: &str) -> Result<Url, Problem> {
    let text = text.trim();
    let next_step =
        "Type the address you use to open the library, for example http://library.local:8080.";
    if text.is_empty() {
        return Err(Problem::new("Type the library's address first.", next_step));
    }
    let with_scheme = if text.contains("://") {
        text.to_owned()
    } else {
        format!("http://{text}")
    };
    let invalid = || Problem::new(format!("\"{text}\" is not a web address."), next_step);
    let mut url = Url::parse(&with_scheme).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(invalid());
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// Starting metadata from the disc itself (CD-Text when present).
pub fn release_from_disc(disc: &DiscInfo) -> ReleaseMetadata {
    let text = disc.cd_text.as_ref();
    let album = text.map(|t| &t.album);
    let mut release = ReleaseMetadata {
        title: album.and_then(|a| a.title.clone()).unwrap_or_default(),
        artist: album.and_then(|a| a.performer.clone()).unwrap_or_default(),
        disc_number: 1,
        disc_total: 1,
        ..ReleaseMetadata::default()
    };
    release.tracks = disc
        .toc
        .audio_tracks()
        .map(|entry| {
            let fields = text
                .and_then(|t| t.tracks.iter().find(|track| track.number == entry.number))
                .map(|track| &track.fields);
            TrackMetadata {
                number: Some(entry.number),
                title: fields
                    .and_then(|f| f.title.clone())
                    .unwrap_or_else(|| format!("Track {}", entry.number.get())),
                artist: fields.and_then(|f| f.performer.clone()).unwrap_or_default(),
                isrc: entry.isrc.clone(),
                ..TrackMetadata::default()
            }
        })
        .collect();
    release
}

/// Makes `release.tracks` exactly one entry per audio track of `toc`, in
/// order, with numbers set (matching by number first, then by position).
pub fn fit_tracks(release: &mut ReleaseMetadata, toc: &Toc) {
    let original = std::mem::take(&mut release.tracks);
    release.tracks = toc
        .audio_tracks()
        .enumerate()
        .map(|(index, entry)| {
            let found = original
                .iter()
                .find(|track| track.number == Some(entry.number))
                .or_else(|| original.get(index).filter(|track| track.number.is_none()));
            let mut track = found.cloned().unwrap_or_default();
            track.number = Some(entry.number);
            if track.title.trim().is_empty() {
                track.title = format!("Track {}", entry.number.get());
            }
            if track.isrc.is_none() {
                track.isrc = entry.isrc.clone();
            }
            track
        })
        .collect();
    if release.disc_number == 0 {
        release.disc_number = 1;
    }
    if release.disc_total < release.disc_number {
        release.disc_total = release.disc_number;
    }
}

/// The `POST /api/rip/items` body for a new catalogue entry.
pub fn new_item(release: &ReleaseMetadata) -> NewItem {
    let mut tags = Vec::new();
    if let Some(label) = release.label.as_ref().filter(|l| !l.trim().is_empty()) {
        tags.push(label.trim().to_owned());
    }
    NewItem {
        type_slug: MUSIC_TYPE.to_owned(),
        title: release.title.trim().to_owned(),
        creator: release.artist.trim().to_owned(),
        year: release.year,
        identifier: release.barcode.clone().filter(|b| !b.trim().is_empty()),
        description: (release.disc_total > 1)
            .then(|| format!("Disc {} of {}", release.disc_number, release.disc_total)),
        tags,
        musicbrainz_release_id: release.release_mbid.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::api::LibraryRef;
    use crate::model::{
        CdText, CdTextFields, CdTextTrack, Crc32, DriveIdentity, Lba, Mbid, Peak, Sectors,
        Sha256Digest, TrackEntry, UploadFile,
    };

    fn number(n: u8) -> TrackNumber {
        TrackNumber::new(n).unwrap()
    }

    fn disc(tracks: u8) -> DiscInfo {
        let entries: Vec<TrackEntry> = (1..=tracks)
            .map(|n| TrackEntry {
                number: number(n),
                start_lba: Lba(u32::from(n - 1) * 1500),
                length_sectors: Sectors(1500),
                is_audio: true,
                pregap: Sectors(0),
                isrc: None,
                session: crate::model::SessionNumber::FIRST,
            })
            .collect();
        DiscInfo {
            drive: DriveIdentity {
                vendor: "LIBRARY".into(),
                model: "Simulated".into(),
                revision: "1".into(),
            },
            toc: Toc::new(entries, Lba(u32::from(tracks) * 1500)).unwrap(),
            mcn: None,
            cd_text: None,
        }
    }

    fn who() -> WhoAmI {
        WhoAmI {
            key_id: 7,
            key_name: "Henry's desktop".into(),
            library: LibraryRef {
                name: "The Library".into(),
                url: "http://library.local/".into(),
            },
        }
    }

    fn drive() -> DriveDescriptor {
        DriveDescriptor {
            device: "/dev/sr0".into(),
            label: "Drive".into(),
        }
    }

    fn connected_with_disc() -> AppState {
        let mut state = AppState::default();
        state.handle(Event::Connected(who()));
        state.handle(Event::DrivesListed(vec![drive()]));
        state.handle(Event::DiscDetected(Box::new(disc(2))));
        state
    }

    fn output(unrecovered: Vec<Lba>) -> RipOutput {
        let report_track = |n: u8, unrecovered: Vec<Lba>| crate::model::TrackReport {
            number: number(n),
            title: format!("Track {n}"),
            start_lba: Lba(0),
            length_sectors: Sectors(1500),
            pregap: Sectors(0),
            isrc: None,
            peak: Peak(100),
            copy_crc: Crc32(1),
            test_crc: None,
            accuraterip_crcs: crate::model::AccurateRipCrcs {
                v1: crate::model::AccurateRipCrc(1),
                v2: crate::model::AccurateRipCrc(2),
            },
            accuraterip: AccurateRipStatus::NotInDatabase,
            unrecovered_sectors: unrecovered,
            retried_sectors: 0,
            file_name: format!("0{n}.flac"),
            file_sha256: None,
        };
        let disc = disc(2);
        let file = |title: &str, kind| UploadFile {
            kind,
            title: title.into(),
            disc: 1,
            track: None,
            duration_ms: None,
            sha256: Sha256Digest([1; 32]),
            path: PathBuf::from(format!("/tmp/{title}")),
        };
        RipOutput {
            folder: PathBuf::from("/tmp/rip"),
            kept: None,
            report: crate::model::RipReport {
                app_version: "test".into(),
                started_at: jiff::Timestamp::UNIX_EPOCH,
                finished_at: jiff::Timestamp::UNIX_EPOCH,
                drive: disc.drive.clone(),
                read_offset: SampleOffset(6),
                offset_source: OffsetSource::User,
                read_mode: ReadMode::Secure,
                toc: disc.toc.clone(),
                disc_ids: crate::model::DiscIds {
                    musicbrainz: "id".into(),
                    cddb: 1,
                    accuraterip: crate::model::AccurateRipDiscId {
                        track_count: 2,
                        id1: 1,
                        id2: 2,
                        cddb: 1,
                    },
                    ctdb_toc: "toc".into(),
                },
                mcn: None,
                release: ReleaseMetadata::default(),
                tracks: vec![report_track(1, Vec::new()), report_track(2, unrecovered)],
                ctdb: CtdbStatus::NotInDatabase,
                verdict: crate::model::DiscVerdict::default(),
            },
            plan: UploadPlan {
                item: ItemTarget::New(NewItem::default()),
                cover: None,
                files: vec![
                    file("Track 1", MediaKind::Audio),
                    file("Rip log", MediaKind::Archive),
                ],
            },
        }
    }

    #[test]
    fn addresses_are_normalised() {
        assert_eq!(
            library_address(" library.local:8080 ").unwrap().as_str(),
            "http://library.local:8080/"
        );
        assert_eq!(
            library_address("https://example.org/library?x=1")
                .unwrap()
                .as_str(),
            "https://example.org/library/"
        );
        assert!(library_address("").is_err());
        assert!(library_address("ftp://example.org").is_err());
        assert!(library_address("http://").is_err());
    }

    #[test]
    fn offsets_parse_or_explain() {
        assert_eq!(parse_offset(""), Ok(None));
        assert_eq!(parse_offset("+6"), Ok(Some(SampleOffset(6))));
        assert_eq!(parse_offset("-472"), Ok(Some(SampleOffset(-472))));
        assert!(parse_offset("six").unwrap_err().contains("whole number"));
        assert!(parse_offset("99999").is_err());
    }

    #[test]
    fn start_up_restores_a_saved_connection() {
        let mut state = AppState::default();
        let commands = state.handle(Event::Started {
            settings: Settings {
                read_offset: Some(SampleOffset(6)),
                ..Settings::default()
            },
            address: Some("http://library.local/".into()),
        });
        assert_eq!(commands, vec![Command::Restore]);
        assert_eq!(state.offset_text, "6");
        assert_eq!(state.connect, ConnectPhase::Checking);
        let commands = state.handle(Event::Connected(who()));
        assert_eq!(commands, vec![Command::ListDrives]);
        assert_eq!(state.screen, Screen::Disc);
        assert!(!state.is_busy(Task::Restore));
    }

    #[test]
    fn connect_validates_the_address_and_shows_the_code() {
        let mut state = AppState::default();
        assert!(
            state
                .handle(Event::ConnectPressed(ConnectMethod::Browser))
                .is_empty()
        );
        assert!(state.problem.is_some());

        state.handle(Event::AddressEdited("library.local".into()));
        let commands = state.handle(Event::ConnectPressed(ConnectMethod::DeviceCode));
        assert_eq!(
            commands,
            vec![Command::Connect {
                address: Url::parse("http://library.local/").unwrap(),
                method: ConnectMethod::DeviceCode,
            }]
        );
        assert!(state.problem.is_none());
        // A second press while connecting does nothing.
        assert!(
            state
                .handle(Event::ConnectPressed(ConnectMethod::Browser))
                .is_empty()
        );

        state.handle(Event::DeviceCodeShown {
            user_code: "WXYZ-1234".into(),
            verification_uri: "http://library.local/connect/device".into(),
        });
        assert!(
            matches!(state.connect, ConnectPhase::WaitingForCode { ref user_code, .. } if user_code == "WXYZ-1234")
        );

        // Cancel goes back to idle.
        assert_eq!(
            state.handle(Event::CancelPressed),
            vec![Command::Cancel(Task::Connect)]
        );
        assert_eq!(state.connect, ConnectPhase::Idle);
        // A late code after cancelling is ignored.
        state.handle(Event::BrowserOpened { url: "x".into() });
        assert_eq!(state.connect, ConnectPhase::Idle);
    }

    #[test]
    fn connect_failure_is_shown_with_a_next_step() {
        let mut state = AppState::default();
        state.handle(Event::AddressEdited("library.local".into()));
        state.handle(Event::ConnectPressed(ConnectMethod::Browser));
        state.handle(Event::Failed {
            task: Task::Connect,
            problem: Problem::new("The librarian did not allow the connection.", "Ask again."),
        });
        assert_eq!(state.connect, ConnectPhase::Idle);
        assert_eq!(state.screen, Screen::Connect);
        assert_eq!(state.problem.as_ref().unwrap().next_step, "Ask again.");
        state.handle(Event::DismissProblem);
        assert!(state.problem.is_none());
    }

    #[test]
    fn disc_flow_reads_the_disc_and_loads_the_first_match() {
        let mut state = AppState::default();
        state.handle(Event::Connected(who()));
        let commands = state.handle(Event::DrivesListed(vec![drive()]));
        assert_eq!(
            commands,
            vec![Command::ReadDisc {
                device: "/dev/sr0".into()
            }]
        );
        let mut info = disc(2);
        info.cd_text = Some(CdText {
            album: CdTextFields {
                title: Some("Disc Title".into()),
                performer: Some("Disc Artist".into()),
                ..CdTextFields::default()
            },
            tracks: vec![CdTextTrack {
                number: number(2),
                fields: CdTextFields {
                    title: Some("Second".into()),
                    ..CdTextFields::default()
                },
            }],
        });
        let commands = state.handle(Event::DiscDetected(Box::new(info.clone())));
        assert!(matches!(
            commands.as_slice(),
            [Command::FindReleases { .. }]
        ));
        assert_eq!(state.disc_phase, DiscPhase::Ready);
        assert_eq!(state.release.title, "Disc Title");
        assert_eq!(state.release.tracks[0].title, "Track 1");
        assert_eq!(state.release.tracks[1].title, "Second");

        let candidate = ReleaseCandidate {
            release_mbid: Mbid::new("f5093c06-23e3-404f-aeaa-40f72885ee3a").unwrap(),
            title: "Kind of Blue".into(),
            artist: "Miles Davis".into(),
            date: Some("1959".into()),
            country: None,
            barcode: None,
            medium: "CD".into(),
            medium_position: 1,
            track_count: 2,
            has_front_cover: true,
        };
        let commands = state.handle(Event::CandidatesFound(vec![candidate.clone()]));
        assert_eq!(
            commands,
            vec![Command::LoadRelease {
                candidate: Box::new(candidate),
                toc: Box::new(info.toc.clone()),
            }]
        );
        assert_eq!(state.candidate, Some(0));

        // MusicBrainz knows only one track: the other is kept as "Track 2".
        state.handle(Event::ReleaseLoaded(Box::new(ReleaseMetadata {
            title: "Kind of Blue".into(),
            artist: "Miles Davis".into(),
            year: Some(1959),
            tracks: vec![TrackMetadata {
                number: Some(number(1)),
                title: "So What".into(),
                ..TrackMetadata::default()
            }],
            ..ReleaseMetadata::default()
        })));
        assert_eq!(state.release.tracks.len(), 2);
        assert_eq!(state.release.tracks[0].title, "So What");
        assert_eq!(state.release.tracks[1].title, "Track 2");
        assert_eq!(state.year_text, "1959");
        assert_eq!(state.release.disc_number, 1);

        // "None of these" goes back to the disc's own text.
        state.handle(Event::ManualEntryChosen);
        assert_eq!(state.release.title, "Disc Title");
        assert_eq!(state.candidate, None);
    }

    #[test]
    fn no_drive_and_empty_drive_are_told_apart() {
        let mut state = AppState::default();
        state.handle(Event::Connected(who()));
        state.handle(Event::DrivesListed(Vec::new()));
        assert_eq!(state.disc_phase, DiscPhase::NoDrive);
        let commands = state.handle(Event::RefreshPressed);
        assert_eq!(commands, vec![Command::ListDrives]);
        state.handle(Event::DrivesListed(vec![drive()]));
        state.handle(Event::Failed {
            task: Task::Disc,
            problem: Problem::new("There is no disc in the drive.", "Put one in."),
        });
        assert_eq!(state.disc_phase, DiscPhase::Empty);
    }

    #[test]
    fn editing_metadata_and_choosing_an_existing_item() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Blue".into()));
        state.handle(Event::AlbumArtistEdited("Joni Mitchell".into()));
        state.handle(Event::YearEdited("1971".into()));
        state.handle(Event::TrackTitleEdited {
            index: 1,
            title: "Carey".into(),
        });
        state.handle(Event::TrackTitleEdited {
            index: 9,
            title: "ignored".into(),
        });
        assert_eq!(state.release.year, Some(1971));
        state.handle(Event::YearEdited("19".into()));
        assert_eq!(state.release.year, None);
        assert_eq!(state.year_text, "19");
        assert_eq!(state.release.tracks[1].title, "Carey");

        let commands = state.handle(Event::ItemSearchPressed(" blue ".into()));
        assert_eq!(
            commands,
            vec![Command::SearchItems {
                query: "blue".into()
            }]
        );
        let item = ItemSummary {
            id: 42,
            title: "Blue".into(),
            creator: Some("Joni Mitchell".into()),
            year: Some(1971),
            identifier: None,
            file_count: 0,
            url: "http://library.local/items/42".into(),
        };
        state.handle(Event::ItemsFound(vec![item.clone()]));
        state.handle(Event::ItemChosen(0));
        assert_eq!(state.destination, Destination::Existing(item));

        let commands = state.handle(Event::RipPressed);
        let [Command::StartRip(job)] = commands.as_slice() else {
            panic!("expected a rip, got {commands:?}");
        };
        assert_eq!(job.target, ItemTarget::Existing { id: 42 });
        assert_eq!(job.release.tracks[1].title, "Carey");
        assert_eq!(state.screen, Screen::Rip);
    }

    #[test]
    fn rip_needs_a_title() {
        let mut state = connected_with_disc();
        assert!(state.handle(Event::RipPressed).is_empty());
        assert!(state.problem.as_ref().unwrap().message.contains("title"));
        assert_eq!(state.screen, Screen::Disc);
    }

    #[test]
    fn a_clean_rip_uploads_and_finishes() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Album".into()));
        let commands = state.handle(Event::RipPressed);
        let [Command::StartRip(job)] = commands.as_slice() else {
            panic!("expected a rip");
        };
        let ItemTarget::New(item) = &job.target else {
            panic!("expected a new item");
        };
        assert_eq!(item.type_slug, "music");
        assert_eq!(item.title, "Album");

        state.handle(Event::Progress(PipelineEvent::Reading {
            track: number(1),
            pass: 1,
            fraction: 0.5,
            speed: 8.0,
            retried_sectors: 0,
        }));
        let rip = state.rip.as_ref().unwrap();
        assert_eq!(rip.rows[0].stage, TrackStage::Reading { pass: 1 });
        assert_eq!(rip.rows[0].fraction, 0.5);
        state.handle(Event::Progress(PipelineEvent::TrackRead {
            track: number(1),
            unrecovered_sectors: 0,
            test_matched: None,
        }));
        state.handle(Event::Progress(PipelineEvent::Verified {
            tracks: vec![(
                number(1),
                AccurateRipStatus::Accurate {
                    version: crate::model::AccurateRipVersion::V2,
                    confidence: 12,
                    pressing_offset: crate::model::SampleOffset(0),
                },
            )],
            ctdb: CtdbStatus::NotInDatabase,
        }));
        state.handle(Event::Progress(PipelineEvent::Encoding {
            track: number(1),
        }));
        state.handle(Event::Progress(PipelineEvent::Encoding {
            track: number(2),
        }));
        let rip = state.rip.as_ref().unwrap();
        assert_eq!(rip.rows[0].stage, TrackStage::Done);
        assert_eq!(rip.rows[1].stage, TrackStage::Encoding);
        assert!(rip.rows[0].accuraterip.is_some());

        let commands = state.handle(Event::RipFinished(Box::new(output(Vec::new()))));
        assert!(matches!(commands.as_slice(), [Command::StartUpload { .. }]));
        assert_eq!(state.screen, Screen::Upload);
        assert_eq!(state.upload.as_ref().unwrap().rows.len(), 2);

        state.handle(Event::Progress(PipelineEvent::ItemReady {
            id: 9,
            url: "http://library.local/items/9".into(),
        }));
        assert_eq!(
            state.output.as_ref().unwrap().plan.item,
            ItemTarget::Existing { id: 9 }
        );
        state.handle(Event::Progress(PipelineEvent::Uploading {
            index: 0,
            fraction: 0.25,
        }));
        assert_eq!(
            state.upload.as_ref().unwrap().rows[0].stage,
            UploadStage::Sending
        );
        state.handle(Event::Progress(PipelineEvent::Uploaded {
            index: 1,
            skipped: true,
        }));
        assert_eq!(
            state.upload.as_ref().unwrap().rows[1].stage,
            UploadStage::AlreadyThere
        );

        state.handle(Event::UploadFinished(UploadOutput {
            item_id: 9,
            item_url: "http://library.local/items/9".into(),
            uploaded: 1,
            skipped: 1,
            cover_problem: Some("no".into()),
        }));
        assert_eq!(state.screen, Screen::Done);
        let done = state.done.as_ref().unwrap();
        assert_eq!(done.item_url, "http://library.local/items/9");
        assert!(done.notes[0].contains("cover"));

        assert_eq!(
            state.handle(Event::EjectPressed),
            vec![Command::Eject {
                device: "/dev/sr0".into()
            }]
        );
        state.handle(Event::Ejected);
        assert!(state.done.as_ref().unwrap().ejected);
        assert_eq!(
            state.handle(Event::OpenLink("http://library.local/items/9".into())),
            vec![Command::OpenUrl("http://library.local/items/9".into())]
        );
        let commands = state.handle(Event::AnotherDiscPressed);
        assert_eq!(
            commands,
            vec![Command::ReadDisc {
                device: "/dev/sr0".into()
            }]
        );
        assert_eq!(state.screen, Screen::Disc);
    }

    #[test]
    fn a_rip_with_read_errors_waits_for_the_user() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Album".into()));
        state.handle(Event::RipPressed);
        let commands = state.handle(Event::RipFinished(Box::new(output(vec![Lba(700)]))));
        assert!(commands.is_empty());
        let Some(RipPhase::NeedsDecision { issues }) = state.rip.as_ref().map(|r| &r.phase) else {
            panic!("expected a decision");
        };
        assert_eq!(
            issues,
            &vec!["Track 2: 1 sector could not be read reliably.".to_owned()]
        );
        assert_eq!(state.screen, Screen::Rip);
        let commands = state.handle(Event::UploadAnywayPressed);
        assert!(matches!(commands.as_slice(), [Command::StartUpload { .. }]));
        state.handle(Event::UploadFinished(UploadOutput {
            item_id: 1,
            item_url: "u".into(),
            uploaded: 2,
            skipped: 0,
            cover_problem: None,
        }));
        assert_eq!(state.done.as_ref().unwrap().read_issues.len(), 1);
    }

    #[test]
    fn rip_again_after_read_errors() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Album".into()));
        state.handle(Event::RipPressed);
        state.handle(Event::RipFinished(Box::new(output(vec![Lba(700)]))));
        let commands = state.handle(Event::RipAgainPressed);
        assert!(matches!(commands.as_slice(), [Command::StartRip(_)]));
        assert_eq!(state.rip.as_ref().unwrap().phase, RipPhase::Running);
    }

    #[test]
    fn cancelling_a_rip_returns_to_the_disc() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Album".into()));
        state.handle(Event::RipPressed);
        assert_eq!(
            state.handle(Event::CancelPressed),
            vec![Command::Cancel(Task::Rip)]
        );
        assert_eq!(state.rip.as_ref().unwrap().phase, RipPhase::Stopping);
        // Disconnecting while ripping is refused.
        assert!(state.handle(Event::DisconnectPressed).is_empty());
        assert!(state.connection.is_some());
        state.handle(Event::Cancelled(Task::Rip));
        assert_eq!(state.screen, Screen::Disc);
        assert!(state.problem.as_ref().unwrap().message.contains("stopped"));
        assert!(!state.is_busy(Task::Rip));
    }

    #[test]
    fn a_failed_upload_can_be_retried_without_a_second_item() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Album".into()));
        state.handle(Event::RipPressed);
        state.handle(Event::RipFinished(Box::new(output(Vec::new()))));
        state.handle(Event::Progress(PipelineEvent::ItemReady {
            id: 5,
            url: "http://library.local/items/5".into(),
        }));
        state.handle(Event::Progress(PipelineEvent::Uploaded {
            index: 0,
            skipped: false,
        }));
        state.handle(Event::Failed {
            task: Task::Upload,
            problem: Problem::new("Could not reach the library.", "Try again."),
        });
        assert_eq!(state.upload.as_ref().unwrap().phase, UploadPhase::Stopped);
        let commands = state.handle(Event::RetryUploadPressed);
        let [Command::StartUpload { plan, .. }] = commands.as_slice() else {
            panic!("expected an upload");
        };
        assert_eq!(plan.item, ItemTarget::Existing { id: 5 });
        let upload = state.upload.as_ref().unwrap();
        assert_eq!(upload.rows[0].stage, UploadStage::Sent);
        assert_eq!(upload.rows[1].stage, UploadStage::Waiting);
        assert_eq!(
            upload.item_url.as_deref(),
            Some("http://library.local/items/5")
        );
    }

    #[test]
    fn a_revoked_key_goes_back_to_connect() {
        let mut state = connected_with_disc();
        state.handle(Event::AlbumTitleEdited("Album".into()));
        state.handle(Event::RipPressed);
        state.handle(Event::RipFinished(Box::new(output(Vec::new()))));
        state.handle(Event::Failed {
            task: Task::Upload,
            problem: Problem::reconnect("No longer connected.", "Connect again."),
        });
        assert_eq!(state.screen, Screen::Connect);
        assert!(state.connection.is_none());
        assert!(state.busy.is_empty());
        assert!(state.problem.as_ref().unwrap().reconnect);
    }

    #[test]
    fn settings_edit_validate_and_return() {
        let mut state = connected_with_disc();
        state.handle(Event::SettingsPressed);
        assert_eq!(state.screen, Screen::Settings);
        assert!(state.handle(Event::OffsetEdited("abc".into())).is_empty());
        assert!(state.settings_problem.is_some());
        let commands = state.handle(Event::OffsetEdited("667".into()));
        assert_eq!(state.settings.read_offset, Some(SampleOffset(667)));
        assert!(state.settings_problem.is_none());
        assert_eq!(
            commands,
            vec![Command::SaveSettings(state.settings.clone())]
        );
        state.handle(Event::ReadModeChosen(ReadMode::TestAndCopy));
        assert_eq!(state.settings.read_mode, ReadMode::TestAndCopy);
        state.handle(Event::KeepFolderEdited("/music/rips".into()));
        assert_eq!(
            state.settings.keep_local_copy,
            Some(PathBuf::from("/music/rips"))
        );
        state.handle(Event::KeepFolderEdited("  ".into()));
        assert_eq!(state.settings.keep_local_copy, None);
        assert_eq!(
            state.handle(Event::ChooseFolderPressed),
            vec![Command::PickFolder]
        );
        state.handle(Event::FolderPicked(Some(PathBuf::from("/archive"))));
        assert_eq!(state.keep_text, "/archive");
        state.handle(Event::Failed {
            task: Task::Settings,
            problem: Problem::new("Could not save.", "Check the folder."),
        });
        assert_eq!(state.settings_problem.as_deref(), Some("Could not save."));
        assert!(state.problem.is_none());
        state.handle(Event::BackPressed);
        assert_eq!(state.screen, Screen::Disc);
    }

    #[test]
    fn disconnect_forgets_everything() {
        let mut state = connected_with_disc();
        state.handle(Event::SettingsPressed);
        assert_eq!(
            state.handle(Event::DisconnectPressed),
            vec![Command::Disconnect]
        );
        assert!(state.connection.is_none());
        assert!(state.disc.is_none());
        state.handle(Event::BackPressed);
        assert_eq!(state.screen, Screen::Connect);
    }

    #[test]
    fn new_items_carry_the_release() {
        let release = ReleaseMetadata {
            title: " Abbey Road ".into(),
            artist: "The Beatles".into(),
            year: Some(1969),
            barcode: Some("077774644624".into()),
            label: Some("Apple".into()),
            disc_number: 1,
            disc_total: 2,
            ..ReleaseMetadata::default()
        };
        let item = new_item(&release);
        assert_eq!(item.title, "Abbey Road");
        assert_eq!(item.creator, "The Beatles");
        assert_eq!(item.identifier.as_deref(), Some("077774644624"));
        assert_eq!(item.description.as_deref(), Some("Disc 1 of 2"));
        assert_eq!(item.tags, vec!["Apple".to_owned()]);
    }
}
