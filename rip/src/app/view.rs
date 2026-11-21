// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the window shows, as plain strings and rows built from
//! [`AppState`]. Kept apart from Slint so the wording is tested; `gui.rs`
//! copies a [`ViewModel`] into the window's properties without deciding
//! anything.

use super::film::ViewingSupport;
use super::state::{
    AppState, ConnectPhase, Destination, DiscPhase, RipPhase, Screen, Task, TrackStage,
    UploadPhase, UploadStage,
};
use crate::model::{AccurateRipStatus, CoverArt, CtdbStatus, MediaKind, OffsetSource, ReadMode};

/// How a status reads at a glance.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// Nothing to note.
    #[default]
    Neutral,
    /// Good news (verified, uploaded).
    Good,
    /// Worth a look (mismatch, could not check).
    Warn,
    /// A problem (unreadable sectors).
    Bad,
}

impl Tone {
    /// The number the Slint side switches on.
    pub fn index(self) -> i32 {
        match self {
            Tone::Neutral => 0,
            Tone::Good => 1,
            Tone::Warn => 2,
            Tone::Bad => 3,
        }
    }
}

/// A two-line choice (MusicBrainz match, catalogue entry).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChoiceRow {
    /// First line.
    pub title: String,
    /// Second line.
    pub detail: String,
}

/// A title of a film disc: ticked or not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TitleRow {
    /// First line.
    pub title: String,
    /// Second line.
    pub detail: String,
    /// It will be copied.
    pub selected: bool,
}

/// An editable track title on the Disc screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackLine {
    /// "1", "2", …
    pub number: String,
    /// Title.
    pub title: String,
    /// "3:45".
    pub length: String,
}

/// A row with progress (Rip and Upload screens).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressRow {
    /// Short label on the left ("1", "Audio").
    pub label: String,
    /// Title.
    pub title: String,
    /// Length or kind.
    pub detail: String,
    /// 0.0–1.0.
    pub progress: f32,
    /// Plain-English status.
    pub status: String,
    /// How the status reads.
    pub tone: Tone,
}

/// Everything the window shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewModel {
    /// Current screen.
    pub screen: Screen,
    /// The step the progress strip highlights (Settings keeps the last).
    pub step: Screen,
    /// "Connected to The Library as Henry's desktop".
    pub connection: String,
    /// What is happening in the background, if anything.
    pub busy: String,
    /// The problem banner: message.
    pub problem: String,
    /// The problem banner: what to do next.
    pub next_step: String,

    // Connect
    /// The address field.
    pub address: String,
    /// Connect buttons enabled.
    pub can_connect: bool,
    /// Waiting for the librarian.
    pub waiting: bool,
    /// Heading while waiting.
    pub waiting_title: String,
    /// Explanation while waiting.
    pub waiting_detail: String,
    /// The page or code to show while waiting.
    pub waiting_link: String,
    /// The device code, large.
    pub waiting_code: String,

    // Disc
    /// Drive labels.
    pub drives: Vec<String>,
    /// Chosen drive (-1 for none).
    pub drive_index: i32,
    /// A disc is ready.
    pub disc_ready: bool,
    /// "3 tracks · 47:12 · LIBRARY Simulated Drive" or why there is none.
    pub disc_summary: String,
    /// MusicBrainz matches.
    pub candidates: Vec<ChoiceRow>,
    /// Chosen match (-1 for none / typed by hand).
    pub candidate_index: i32,
    /// "Looking the disc up on MusicBrainz…" etc.
    pub lookup_status: String,
    /// Album title field.
    pub album_title: String,
    /// Album artist field.
    pub album_artist: String,
    /// Year field.
    pub year: String,
    /// Track titles.
    pub tracks: Vec<TrackLine>,
    /// Changes when the album details are replaced rather than edited.
    pub release_revision: u64,
    /// The front cover of the chosen release.
    pub cover: Option<CoverArt>,
    /// "Add to existing" is chosen.
    pub destination_existing: bool,
    /// What the destination is, in words.
    pub destination: String,
    /// Catalogue search results.
    pub items: Vec<ChoiceRow>,
    /// The chosen catalogue entry (-1 for none).
    pub item_index: i32,
    /// Rip can be pressed.
    pub can_rip: bool,
    /// The disc is a film disc.
    pub film_mode: bool,
    /// What MakeMKV and ffmpeg make of it.
    pub film_notice: String,
    /// The titles on a film disc.
    pub film_titles: Vec<TitleRow>,
    /// A viewing copy can be made, so there is a choice.
    pub film_can_choose_copy: bool,
    /// The viewing copy will be made.
    pub film_viewing_copy: bool,

    // Rip
    /// Rip heading.
    pub rip_title: String,
    /// Offset, speed, mode.
    pub rip_detail: String,
    /// Per-track rows.
    pub rip_rows: Vec<ProgressRow>,
    /// CTDB result.
    pub ctdb: String,
    /// Reading is running (Cancel shown).
    pub rip_running: bool,
    /// Read problems: choose upload anyway / rip again.
    pub rip_decision: bool,
    /// The read problems.
    pub rip_issues: Vec<String>,
    /// The rip failed or stopped.
    pub rip_failed: bool,

    // Upload
    /// Upload heading.
    pub upload_title: String,
    /// Per-file rows.
    pub upload_rows: Vec<ProgressRow>,
    /// Sending (Cancel shown).
    pub upload_running: bool,
    /// Stopped (Try again shown).
    pub upload_stopped: bool,

    // Done
    /// Done heading.
    pub done_title: String,
    /// The item page.
    pub done_link: String,
    /// Lines under the heading.
    pub done_lines: Vec<String>,
    /// Eject can be pressed.
    pub can_eject: bool,

    // Settings
    /// The offset field.
    pub offset_text: String,
    /// What an empty offset means.
    pub offset_hint: String,
    /// Test-and-copy chosen.
    pub test_and_copy: bool,
    /// The keep folder field.
    pub keep_text: String,
    /// A problem with what was typed.
    pub settings_problem: String,
    /// Disconnect can be pressed.
    pub can_disconnect: bool,
    /// "Library Rip 0.1.0".
    pub version: String,
    /// The source code address.
    pub source_url: String,
}

/// `m:ss` (or `h:mm:ss`).
pub fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Where an offset came from, in words.
pub fn offset_source(source: OffsetSource) -> &'static str {
    match source {
        OffsetSource::AccurateRipDatabase => "from the AccurateRip drive list",
        OffsetSource::User => "set in Settings",
        OffsetSource::Unknown => "drive not in the AccurateRip list, so no correction",
    }
}

/// How a track read, in words.
pub fn read_summary(unrecovered_sectors: usize, test_matched: Option<bool>) -> String {
    let read = match unrecovered_sectors {
        0 => "read without errors".to_owned(),
        1 => "1 sector could not be read reliably".to_owned(),
        n => format!("{n} sectors could not be read reliably"),
    };
    match test_matched {
        Some(true) => format!("{read}, test and copy match."),
        Some(false) => format!("{read}, but the test and copy reads differ."),
        None => format!("{read}."),
    }
}

fn accuraterip_tone(status: &AccurateRipStatus) -> Tone {
    match status {
        AccurateRipStatus::Accurate { .. } => Tone::Good,
        AccurateRipStatus::Mismatch { .. } => Tone::Warn,
        AccurateRipStatus::NotInDatabase | AccurateRipStatus::Unavailable { .. } => Tone::Neutral,
    }
}

fn busy_label(state: &AppState) -> String {
    if state.film_scanning && state.is_busy(Task::Disc) {
        return "Asking MakeMKV what is on the disc… this can take a minute".to_owned();
    }
    // The most important running task speaks.
    let label = [
        (Task::Rip, ""),
        (Task::Upload, ""),
        (Task::Restore, "Checking the saved connection…"),
        (Task::Connect, ""),
        (Task::Disconnect, "Disconnecting…"),
        (Task::Drives, "Looking for drives…"),
        (Task::Disc, "Reading the disc…"),
        (Task::Release, "Loading the album details…"),
        (Task::Releases, "Looking the disc up on MusicBrainz…"),
        (Task::Items, "Searching the catalogue…"),
        (Task::Eject, "Ejecting…"),
        (Task::Folder, "Choosing a folder…"),
    ]
    .into_iter()
    .find(|(task, _)| state.is_busy(*task))
    .map(|(_, label)| label)
    .unwrap_or_default();
    label.to_owned()
}

impl ViewModel {
    /// Builds the view of `state`.
    pub fn new(state: &AppState) -> ViewModel {
        let mut view = ViewModel {
            screen: state.screen,
            step: if state.screen == Screen::Settings {
                state.previous_screen.unwrap_or(Screen::Connect)
            } else {
                state.screen
            },
            connection: match &state.connection {
                Some(who) => format!("Connected to {} as {}", who.library.name, who.key_name),
                None => "Not connected".to_owned(),
            },
            busy: busy_label(state),
            problem: state
                .problem
                .as_ref()
                .map(|p| p.message.clone())
                .unwrap_or_default(),
            next_step: state
                .problem
                .as_ref()
                .map(|p| p.next_step.clone())
                .unwrap_or_default(),
            version: format!("Library Rip {}", crate::VERSION),
            source_url: crate::SOURCE_URL.to_owned(),
            ..ViewModel::default()
        };
        view.connect(state);
        view.disc(state);
        view.rip(state);
        view.upload(state);
        view.done(state);
        view.settings(state);
        view
    }

    fn connect(&mut self, state: &AppState) {
        self.address = state.address.clone();
        let connecting = state.is_busy(Task::Connect) || state.is_busy(Task::Restore);
        self.can_connect = !connecting;
        match &state.connect {
            ConnectPhase::Idle => {}
            ConnectPhase::Checking => {
                self.waiting = true;
                self.waiting_title = "Contacting the library…".to_owned();
            }
            ConnectPhase::WaitingForBrowser { url } => {
                self.waiting = true;
                self.waiting_title = "Waiting for the librarian".to_owned();
                self.waiting_detail = "Your browser has opened the library. Unlock it if asked, \
                    then press Yes, connect. If no browser opened, open this page yourself:"
                    .to_owned();
                self.waiting_link = url.clone();
            }
            ConnectPhase::WaitingForCode {
                user_code,
                verification_uri,
            } => {
                self.waiting = true;
                self.waiting_title = "Type this code on the library".to_owned();
                self.waiting_detail =
                    "On any device, open the page below, unlock the library and type the code. \
                     It works for ten minutes."
                        .to_owned();
                self.waiting_link = verification_uri.clone();
                self.waiting_code = user_code.clone();
            }
        }
    }

    fn disc(&mut self, state: &AppState) {
        self.drives = state.drives.iter().map(|d| d.label.clone()).collect();
        self.drive_index = index(state.drive);
        self.disc_ready =
            state.disc_phase == DiscPhase::Ready && (state.disc.is_some() || state.film.is_some());
        self.film(state);
        self.disc_summary = match (state.disc_phase, &state.disc) {
            (DiscPhase::Ready, None) if state.film.is_some() => film_summary(state),
            (DiscPhase::Ready, Some(disc)) => {
                let audio = disc.toc.audio_tracks().count();
                let tracks = if audio == 1 { "track" } else { "tracks" };
                let data = if disc.toc.data_tracks().next().is_some() {
                    " · enhanced CD (its computer data is left alone)"
                } else {
                    ""
                };
                format!(
                    "{audio} {tracks} · {}{data} · in {}",
                    duration(disc.toc.audio_duration_ms()),
                    disc.drive
                )
            }
            (DiscPhase::Looking, _) if state.film_scanning => {
                "This is not an audio CD. Asking MakeMKV what is on it…".to_owned()
            }
            (DiscPhase::Looking, _) => "Looking for a disc…".to_owned(),
            (DiscPhase::NoDrive, _) => {
                "No disc drive was found. Plug one in and press Look again.".to_owned()
            }
            (DiscPhase::Empty, _) | (DiscPhase::Ready, None) => {
                "No disc found. Put in an audio CD, DVD or Blu-ray disc and press Look again."
                    .to_owned()
            }
        };
        self.candidates = state
            .candidates
            .iter()
            .map(|c| {
                let mut detail = vec![c.medium.clone()];
                detail.extend(c.date.clone());
                detail.extend(c.country.clone());
                detail.extend(c.barcode.clone().map(|b| format!("barcode {b}")));
                ChoiceRow {
                    title: format!("{} — {}", c.artist, c.title),
                    detail: detail.join(" · "),
                }
            })
            .collect();
        self.candidate_index = index(state.candidate);
        self.lookup_status = if state.is_busy(Task::Releases) {
            "Looking the disc up on MusicBrainz…".to_owned()
        } else if state.is_busy(Task::Release) {
            "Loading the album details…".to_owned()
        } else if self.disc_ready && state.candidates.is_empty() {
            "MusicBrainz has no match. Type the details below, or search by name.".to_owned()
        } else if self.disc_ready && state.candidate.is_none() {
            "Typed by hand. Pick a match to fill the details in.".to_owned()
        } else {
            String::new()
        };
        self.album_title = state.release.title.clone();
        self.album_artist = state.release.artist.clone();
        self.year = state.year_text.clone();
        self.release_revision = state.release_revision;
        let lengths: Vec<u64> = state
            .disc
            .as_ref()
            .map(|d| {
                d.toc
                    .audio_tracks()
                    .map(|t| t.length_sectors.duration_ms())
                    .collect()
            })
            .unwrap_or_default();
        self.tracks = state
            .release
            .tracks
            .iter()
            .enumerate()
            .map(|(i, track)| TrackLine {
                number: track
                    .number
                    .map(|n| n.get().to_string())
                    .unwrap_or_else(|| (i + 1).to_string()),
                title: track.title.clone(),
                length: lengths.get(i).map(|ms| duration(*ms)).unwrap_or_default(),
            })
            .collect();
        self.cover = state.release.cover.clone();
        self.items = state
            .items
            .iter()
            .map(|item| {
                let mut detail = Vec::new();
                detail.extend(item.creator.clone());
                detail.extend(item.year.map(|y| y.to_string()));
                detail.push(match item.file_count {
                    0 => "no files yet".to_owned(),
                    1 => "1 file".to_owned(),
                    n => format!("{n} files"),
                });
                ChoiceRow {
                    title: item.title.clone(),
                    detail: detail.join(" · "),
                }
            })
            .collect();
        match &state.destination {
            Destination::NewItem => {
                self.destination_existing = false;
                self.destination =
                    "A new catalogue entry will be made from these details.".to_owned();
                self.item_index = -1;
            }
            Destination::Existing(item) => {
                self.destination_existing = true;
                self.destination = format!("The files will be added to \"{}\".", item.title);
                self.item_index = index(state.items.iter().position(|i| i.id == item.id));
            }
        }
        self.can_rip = self.disc_ready
            && state.connection.is_some()
            && !state.is_busy(Task::Rip)
            && !state.is_busy(Task::Upload);
    }

    fn film(&mut self, state: &AppState) {
        let Some(film) = &state.film else {
            return;
        };
        self.film_mode = true;
        self.film_can_choose_copy = film.can_make_viewing_copy();
        self.film_viewing_copy = film.viewing_copy;
        self.film_notice = match &film.scan.viewing {
            ViewingSupport::Available { encoder } => format!(
                "Copies are made with MakeMKV. The viewing copy is made with your ffmpeg ({encoder}, Opus)."
            ),
            ViewingSupport::Unavailable { reason } => format!(
                "{reason} Without it only the lossless master can be archived: the Player will have nothing to play yet. \
                 Install ffmpeg with AV1 (libsvtav1 or libaom) and Opus, then press Look again."
            ),
        };
        self.film_titles = film
            .scan
            .disc
            .titles
            .iter()
            .map(|title| {
                let selected = film.selected.contains(&title.id);
                let name = if title.name.trim().is_empty() {
                    format!("Title {}", title.id + 1)
                } else {
                    title.name.trim().to_owned()
                };
                TitleRow {
                    title: name,
                    detail: format!(
                        "{} · {}",
                        if selected {
                            "Will be copied"
                        } else {
                            "Not copied"
                        },
                        title.summary()
                    ),
                    selected,
                }
            })
            .collect();
    }

    fn rip(&mut self, state: &AppState) {
        let Some(rip) = &state.rip else {
            return;
        };
        if state.film.is_some() {
            self.film_rip(state, rip);
            return;
        }
        self.rip_running = state.is_busy(Task::Rip);
        self.rip_title = match &rip.phase {
            RipPhase::Running if self.rip_running => {
                if rip.rows.iter().any(|r| r.stage == TrackStage::Encoding) {
                    "Writing FLAC files…".to_owned()
                } else {
                    "Reading the disc carefully…".to_owned()
                }
            }
            RipPhase::Running => "Ripped".to_owned(),
            RipPhase::Stopping => "Stopping…".to_owned(),
            RipPhase::NeedsDecision { .. } => "Ripped, with read errors".to_owned(),
            RipPhase::Failed => "The rip did not finish".to_owned(),
        };
        let mut detail = Vec::new();
        if let Some((offset, source)) = rip.offset {
            detail.push(format!("Offset {offset} ({})", offset_source(source)));
        }
        detail.push(format!("{} mode", state.settings.read_mode));
        if self.rip_running && rip.speed > 0.0 {
            detail.push(format!("{:.1}× speed", rip.speed));
        }
        if rip.retried_sectors > 0 {
            detail.push(format!("{} sectors re-read", rip.retried_sectors));
        }
        self.rip_detail = detail.join(" · ");
        self.rip_rows = rip
            .rows
            .iter()
            .map(|row| {
                let (status, tone) = match (&row.stage, &row.accuraterip) {
                    (TrackStage::Waiting, _) => ("Waiting".to_owned(), Tone::Neutral),
                    (TrackStage::Reading { pass }, _) => (
                        if *pass > 1 {
                            format!("Test read {}%", percent(row.fraction))
                        } else {
                            format!("Reading {}%", percent(row.fraction))
                        },
                        Tone::Neutral,
                    ),
                    (_, _) if row.unrecovered_sectors > 0 || row.test_matched == Some(false) => (
                        read_summary(row.unrecovered_sectors, row.test_matched)
                            .trim_end_matches('.')
                            .to_owned(),
                        Tone::Bad,
                    ),
                    (_, Some(status)) => (status.summary(), accuraterip_tone(status)),
                    (TrackStage::Encoding, None) => ("Writing FLAC".to_owned(), Tone::Neutral),
                    (TrackStage::Read | TrackStage::Done, None) => {
                        ("Read without errors".to_owned(), Tone::Neutral)
                    }
                };
                ProgressRow {
                    label: row.number.get().to_string(),
                    title: row.title.clone(),
                    detail: duration(row.duration_ms),
                    progress: match row.stage {
                        TrackStage::Waiting => 0.0,
                        TrackStage::Reading { .. } => row.fraction,
                        TrackStage::Read | TrackStage::Encoding | TrackStage::Done => 1.0,
                    },
                    status,
                    tone,
                }
            })
            .collect();
        self.ctdb = rip
            .ctdb
            .as_ref()
            .map(|ctdb| format!("CUETools database: {}", ctdb_summary(ctdb)))
            .unwrap_or_default();
        if let RipPhase::NeedsDecision { issues } = &rip.phase {
            self.rip_decision = true;
            self.rip_issues = issues.clone();
        }
        self.rip_failed = rip.phase == RipPhase::Failed;
    }

    fn film_rip(&mut self, state: &AppState, rip: &crate::app::state::RipProgress) {
        self.rip_running = state.is_busy(Task::Rip);
        let viewing = state.film.as_ref().is_some_and(|f| f.viewing_copy);
        self.rip_title = match &rip.phase {
            RipPhase::Running if self.rip_running => {
                if rip.rows.iter().any(|r| r.stage == TrackStage::Encoding) {
                    "Making the viewing copy…".to_owned()
                } else {
                    "Copying the disc with MakeMKV…".to_owned()
                }
            }
            RipPhase::Running => "Copied".to_owned(),
            RipPhase::Stopping => "Stopping…".to_owned(),
            RipPhase::NeedsDecision { .. } | RipPhase::Failed => {
                "The film did not finish copying".to_owned()
            }
        };
        self.rip_detail = if viewing {
            "Lossless MKV master, then a WebM viewing copy. A DVD takes a while: leave this open."
        } else {
            "Lossless MKV master only. A DVD takes a while: leave this open."
        }
        .to_owned();
        self.rip_rows = rip
            .rows
            .iter()
            .map(|row| {
                let (status, progress) = match row.stage {
                    TrackStage::Waiting => ("Waiting".to_owned(), 0.0),
                    TrackStage::Reading { .. } => {
                        (format!("Copying {}%", percent(row.fraction)), row.fraction)
                    }
                    TrackStage::Read => ("Copied".to_owned(), 1.0),
                    TrackStage::Encoding => (
                        format!("Making viewing copy {}%", percent(row.fraction)),
                        row.fraction,
                    ),
                    TrackStage::Done => ("Done".to_owned(), 1.0),
                };
                ProgressRow {
                    label: row.number.get().to_string(),
                    title: row.title.clone(),
                    detail: duration(row.duration_ms),
                    progress,
                    status,
                    tone: Tone::Neutral,
                }
            })
            .collect();
        self.rip_failed = rip.phase == RipPhase::Failed;
    }

    fn upload(&mut self, state: &AppState) {
        let Some(upload) = &state.upload else {
            return;
        };
        self.upload_running = state.is_busy(Task::Upload);
        self.upload_stopped = upload.phase == UploadPhase::Stopped;
        let total = upload.rows.len();
        let done = upload
            .rows
            .iter()
            .filter(|r| matches!(r.stage, UploadStage::Sent | UploadStage::AlreadyThere))
            .count();
        self.upload_title = match upload.phase {
            UploadPhase::Running => format!("Sending to the library… {done} of {total}"),
            UploadPhase::Stopping => "Stopping…".to_owned(),
            UploadPhase::Stopped => {
                format!("Upload stopped: {done} of {total} files are on the library")
            }
        };
        self.upload_rows = upload
            .rows
            .iter()
            .map(|row| {
                let (status, tone) = match row.stage {
                    UploadStage::Waiting => ("Waiting".to_owned(), Tone::Neutral),
                    UploadStage::Sending => {
                        (format!("Sending {}%", percent(row.fraction)), Tone::Neutral)
                    }
                    UploadStage::Sent => ("Sent".to_owned(), Tone::Good),
                    UploadStage::AlreadyThere => ("Already there".to_owned(), Tone::Good),
                };
                ProgressRow {
                    label: match row.kind {
                        MediaKind::Audio => "Audio".to_owned(),
                        MediaKind::Archive => "Archive".to_owned(),
                        MediaKind::Video => "Video".to_owned(),
                        MediaKind::Document => "Document".to_owned(),
                    },
                    title: row.title.clone(),
                    detail: String::new(),
                    progress: row.fraction,
                    status,
                    tone,
                }
            })
            .collect();
    }

    fn done(&mut self, state: &AppState) {
        let Some(done) = &state.done else {
            return;
        };
        self.done_title = if done.read_issues.is_empty() {
            "Ripped and safely in the library".to_owned()
        } else {
            "In the library, with read errors noted".to_owned()
        };
        self.done_link = done.item_url.clone();
        let mut lines = Vec::new();
        lines.push(match (done.uploaded, done.skipped) {
            (sent, 0) => format!("{sent} files sent."),
            (sent, skipped) => format!("{sent} files sent; {skipped} were already there."),
        });
        if let Some(kept) = &done.kept {
            lines.push(format!("A copy is kept in {}.", kept.display()));
        }
        lines.extend(done.read_issues.iter().cloned());
        lines.extend(done.notes.iter().cloned());
        if done.ejected {
            lines.push("The disc has been ejected.".to_owned());
        }
        self.done_lines = lines;
        self.can_eject =
            !done.ejected && state.selected_drive().is_some() && !state.is_busy(Task::Eject);
    }

    fn settings(&mut self, state: &AppState) {
        self.offset_text = state.offset_text.clone();
        self.offset_hint = match state.settings.read_offset {
            Some(_) => "This offset is used for every rip. Clear it to look the drive up instead."
                .to_owned(),
            None => "Empty: looked up in the AccurateRip drive list for each rip.".to_owned(),
        };
        self.test_and_copy = state.settings.read_mode == ReadMode::TestAndCopy;
        self.keep_text = state.keep_text.clone();
        self.settings_problem = state.settings_problem.clone().unwrap_or_default();
        self.can_disconnect =
            state.connection.is_some() && !state.is_busy(Task::Rip) && !state.is_busy(Task::Upload);
    }
}

fn film_summary(state: &AppState) -> String {
    let (Some(film), drive) = (&state.film, state.selected_drive()) else {
        return String::new();
    };
    let titles = film.scan.disc.titles.len();
    let kind = if film.scan.disc.kind.is_empty() {
        "Film disc"
    } else {
        film.scan.disc.kind.as_str()
    };
    let drive = drive
        .map(|d| format!(" · in {}", d.label))
        .unwrap_or_default();
    format!(
        "{kind} · {titles} {}{drive}",
        if titles == 1 { "title" } else { "titles" }
    )
}

fn ctdb_summary(status: &CtdbStatus) -> String {
    status.summary()
}

fn percent(fraction: f32) -> u32 {
    // Rounded down so 100% only shows when finished.
    let clamped = fraction.clamp(0.0, 1.0);
    (clamped * 100.0).floor() as u32
}

fn index(value: Option<usize>) -> i32 {
    value.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Problem;
    use crate::app::state::{Event, RipProgress, TrackRow};
    use crate::library::api::{LibraryRef, WhoAmI};
    use crate::model::{AccurateRipVersion, TrackNumber};

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(225_000), "3:45");
        assert_eq!(duration(3_725_000), "1:02:05");
    }

    #[test]
    fn read_summaries_are_sentences() {
        assert_eq!(read_summary(0, None), "read without errors.");
        assert_eq!(
            read_summary(1, None),
            "1 sector could not be read reliably."
        );
        assert_eq!(
            read_summary(3, Some(false)),
            "3 sectors could not be read reliably, but the test and copy reads differ."
        );
    }

    #[test]
    fn connect_screen_shows_the_code() {
        let mut state = AppState::default();
        state.handle(Event::AddressEdited("library.test".into()));
        state.handle(Event::ConnectPressed(
            crate::app::state::ConnectMethod::DeviceCode,
        ));
        state.handle(Event::DeviceCodeShown {
            user_code: "WXYZ-1234".into(),
            verification_uri: "http://library.test/connect/device".into(),
        });
        let view = ViewModel::new(&state);
        assert!(view.waiting);
        assert!(!view.can_connect);
        assert_eq!(view.waiting_code, "WXYZ-1234");
        assert_eq!(view.waiting_link, "http://library.test/connect/device");
        assert_eq!(view.connection, "Not connected");
    }

    #[test]
    fn rip_rows_show_progress_and_verification() {
        let mut state = AppState {
            connection: Some(WhoAmI {
                key_id: 1,
                key_name: "Henry's desktop".into(),
                library: LibraryRef {
                    name: "The Library".into(),
                    url: "u".into(),
                },
            }),
            screen: Screen::Rip,
            problem: Some(Problem::new("Oops.", "Do this.")),
            ..AppState::default()
        };
        state.busy.insert(Task::Rip);
        let row = |n: u8, stage, accuraterip, unrecovered| TrackRow {
            number: TrackNumber::new(n).unwrap(),
            title: format!("Song {n}"),
            duration_ms: 61_000,
            stage,
            fraction: 0.426,
            unrecovered_sectors: unrecovered,
            test_matched: None,
            accuraterip,
        };
        state.rip = Some(RipProgress {
            rows: vec![
                row(
                    1,
                    TrackStage::Done,
                    Some(AccurateRipStatus::Accurate {
                        version: AccurateRipVersion::V2,
                        confidence: 12,
                        pressing_offset: crate::model::SampleOffset(0),
                    }),
                    0,
                ),
                row(2, TrackStage::Read, None, 2),
                row(3, TrackStage::Reading { pass: 2 }, None, 0),
                row(4, TrackStage::Waiting, None, 0),
            ],
            speed: 7.94,
            offset: Some((
                crate::model::SampleOffset(6),
                OffsetSource::AccurateRipDatabase,
            )),
            ..RipProgress::default()
        });
        let view = ViewModel::new(&state);
        assert_eq!(
            view.connection,
            "Connected to The Library as Henry's desktop"
        );
        assert_eq!(view.problem, "Oops.");
        assert_eq!(view.next_step, "Do this.");
        assert_eq!(view.rip_title, "Reading the disc carefully…");
        assert_eq!(
            view.rip_detail,
            "Offset +6 (from the AccurateRip drive list) · Secure mode · 7.9× speed"
        );
        let statuses: Vec<(&str, Tone)> = view
            .rip_rows
            .iter()
            .map(|r| (r.status.as_str(), r.tone))
            .collect();
        assert_eq!(
            statuses,
            vec![
                ("Accurately ripped (confidence 12)", Tone::Good),
                ("2 sectors could not be read reliably", Tone::Bad),
                ("Test read 42%", Tone::Neutral),
                ("Waiting", Tone::Neutral),
            ]
        );
        assert_eq!(view.rip_rows[0].detail, "1:01");
        assert!(view.rip_running);
    }

    #[test]
    fn settings_explain_an_empty_offset() {
        let view = ViewModel::new(&AppState::default());
        assert!(view.offset_hint.contains("AccurateRip drive list"));
        assert!(!view.test_and_copy);
        assert!(!view.can_disconnect);
        assert_eq!(view.version, format!("Library Rip {}", crate::VERSION));
        assert_eq!(view.source_url, crate::SOURCE_URL);
    }
}
