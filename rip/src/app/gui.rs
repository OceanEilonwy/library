// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Slint GUI (ui/main.slint). Holds no logic: callbacks become
//! [`Event`]s for the [`Controller`], and after every event the
//! [`ViewModel`] of the new state is copied into the window's properties.
//!
//! The controller runs on a Tokio runtime in its own thread; the window
//! runs on the main thread. Renders are coalesced: while one is waiting
//! for the event loop, newer states replace it instead of queueing.

#![allow(missing_docs)] // Slint-generated items carry no doc comments.

slint::include_modules!();

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::controller::{Controller, EventSender};
use super::live::LiveBackend;
use super::state::{self, Event};
use super::view::{self, ViewModel};
use super::{AppError, Result};
use crate::model::{CoverArt, ReadMode};

fn gui_error(error: impl std::fmt::Display) -> AppError {
    AppError::Gui(error.to_string())
}

/// Opens the main window and runs until it is closed. `fake_disc` offers a
/// simulated disc instead of the real drives.
pub fn run(fake_disc: Option<PathBuf>) -> Result<()> {
    let backend = LiveBackend::desktop(fake_disc)?;
    let window = MainWindow::new().map_err(gui_error)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("library-rip")
        .build()
        .map_err(|error| AppError::Task(error.to_string()))?;

    let controller = Controller::new(backend);
    connect_callbacks(&window, &controller.sender());

    let pending: Arc<Mutex<Option<ViewModel>>> = Arc::new(Mutex::new(None));
    let weak = window.as_weak();
    std::thread::Builder::new()
        .name("library-rip-controller".to_owned())
        .spawn(move || {
            runtime.block_on(controller.run(move |state| {
                let view = ViewModel::new(state);
                let schedule = {
                    let mut slot = pending.lock().unwrap_or_else(PoisonError::into_inner);
                    let schedule = slot.is_none();
                    *slot = Some(view);
                    schedule
                };
                if schedule {
                    let pending = Arc::clone(&pending);
                    let weak = weak.clone();
                    let posted = slint::invoke_from_event_loop(move || {
                        let view = pending
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .take();
                        if let (Some(view), Some(window)) = (view, weak.upgrade()) {
                            apply(&window, view);
                        }
                    });
                    if let Err(error) = posted {
                        tracing::debug!(%error, "the window has closed");
                    }
                }
            }));
        })
        .map_err(|error| AppError::Task(error.to_string()))?;

    window.run().map_err(gui_error)
}

fn page(screen: state::Screen) -> Page {
    match screen {
        state::Screen::Connect => Page::Connect,
        state::Screen::Disc => Page::Disc,
        state::Screen::Rip => Page::Rip,
        state::Screen::Upload => Page::Upload,
        state::Screen::Done => Page::Done,
        state::Screen::Settings => Page::Settings,
    }
}

/// Forwards every window callback to the controller.
fn connect_callbacks(window: &MainWindow, sender: &EventSender) {
    macro_rules! forward {
        ($on:ident, || $event:expr) => {{
            let sender = sender.clone();
            window.$on(move || sender.send($event));
        }};
        ($on:ident, |$value:ident| $event:expr) => {{
            let sender = sender.clone();
            window.$on(move |$value| sender.send($event));
        }};
    }
    let index = |value: i32| usize::try_from(value).unwrap_or(usize::MAX);

    forward!(on_address_edited, |text| Event::AddressEdited(text.into()));
    forward!(on_connect_browser, || Event::ConnectPressed(
        state::ConnectMethod::Browser
    ));
    forward!(on_connect_code, || Event::ConnectPressed(
        state::ConnectMethod::DeviceCode
    ));
    forward!(on_cancel, || Event::CancelPressed);
    forward!(on_open_link, |url| Event::OpenLink(url.into()));
    forward!(on_drive_chosen, |value| Event::DriveChosen(index(value)));
    forward!(on_refresh, || Event::RefreshPressed);
    forward!(on_candidate_chosen, |value| Event::CandidateChosen(index(
        value
    )));
    forward!(on_manual_entry, || Event::ManualEntryChosen);
    forward!(on_search_releases, |query| Event::ReleaseSearchPressed(
        query.into()
    ));
    forward!(on_album_title_edited, |text| Event::AlbumTitleEdited(
        text.into()
    ));
    forward!(on_album_artist_edited, |text| Event::AlbumArtistEdited(
        text.into()
    ));
    forward!(on_year_edited, |text| Event::YearEdited(text.into()));
    {
        let sender = sender.clone();
        window.on_track_title_edited(move |row, title| {
            sender.send(Event::TrackTitleEdited {
                index: index(row),
                title: title.into(),
            });
        });
    }
    forward!(on_new_item_chosen, || Event::NewItemChosen);
    forward!(on_search_items, |query| Event::ItemSearchPressed(
        query.into()
    ));
    forward!(on_item_chosen, |value| Event::ItemChosen(index(value)));
    forward!(on_rip, || Event::RipPressed);
    forward!(on_film_title_toggled, |value| Event::FilmTitleToggled(
        index(value)
    ));
    forward!(on_film_viewing_copy_toggled, || {
        Event::FilmViewingCopyToggled
    });
    forward!(on_upload_anyway, || Event::UploadAnywayPressed);
    forward!(on_rip_again, || Event::RipAgainPressed);
    forward!(on_back_to_disc, || Event::BackToDiscPressed);
    forward!(on_retry_upload, || Event::RetryUploadPressed);
    forward!(on_eject, || Event::EjectPressed);
    forward!(on_another_disc, || Event::AnotherDiscPressed);
    forward!(on_open_settings, || Event::SettingsPressed);
    forward!(on_close_settings, || Event::BackPressed);
    forward!(on_offset_edited, |text| Event::OffsetEdited(text.into()));
    forward!(on_read_mode_chosen, |test| Event::ReadModeChosen(if test {
        ReadMode::TestAndCopy
    } else {
        ReadMode::Secure
    }));
    forward!(on_keep_edited, |text| Event::KeepFolderEdited(text.into()));
    forward!(on_choose_folder, || Event::ChooseFolderPressed);
    forward!(on_disconnect, || Event::DisconnectPressed);
    forward!(on_dismiss_problem, || Event::DismissProblem);
}

thread_local! {
    static RELEASE_REVISION: Cell<Option<u64>> = const { Cell::new(None) };
    static COVER: RefCell<Option<CoverArt>> = const { RefCell::new(None) };
}

fn text(value: &str) -> SharedString {
    SharedString::from(value)
}

fn strings(values: &[String]) -> Vec<SharedString> {
    values.iter().map(|value| text(value)).collect()
}

/// Updates a list in place when its length is unchanged (so focus and
/// scroll positions survive progress updates), else replaces its rows.
fn sync<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    rows: Vec<T>,
    replace: impl FnOnce(ModelRc<T>),
) {
    if let Some(model) = current.as_any().downcast_ref::<VecModel<T>>() {
        if model.row_count() == rows.len() {
            for (index, row) in rows.into_iter().enumerate() {
                if model.row_data(index).as_ref() != Some(&row) {
                    model.set_row_data(index, row);
                }
            }
        } else {
            model.set_vec(rows);
        }
        return;
    }
    replace(ModelRc::new(VecModel::from(rows)));
}

fn choices(rows: &[view::ChoiceRow]) -> Vec<UiChoice> {
    rows.iter()
        .map(|row| UiChoice {
            title: text(&row.title),
            detail: text(&row.detail),
        })
        .collect()
}

fn progress_rows(rows: &[view::ProgressRow]) -> Vec<UiProgress> {
    rows.iter()
        .map(|row| UiProgress {
            label: text(&row.label),
            title: text(&row.title),
            detail: text(&row.detail),
            progress: row.progress,
            status: text(&row.status),
            tone: row.tone.index(),
        })
        .collect()
}

/// Sets an in-out text property only when it differs, so typing is not
/// disturbed by the echo of its own edit.
macro_rules! set_text_if_changed {
    ($window:ident, $get:ident, $set:ident, $value:expr) => {
        if $window.$get().as_str() != $value {
            $window.$set(text($value));
        }
    };
}

fn apply(window: &MainWindow, view: ViewModel) {
    window.set_page(page(view.screen));
    window.set_step(page(view.step));
    window.set_connection(text(&view.connection));
    window.set_busy(text(&view.busy));
    window.set_problem(text(&view.problem));
    window.set_next_step(text(&view.next_step));

    // Connect
    set_text_if_changed!(window, get_address, set_address, &view.address);
    window.set_can_connect(view.can_connect);
    window.set_waiting(view.waiting);
    window.set_waiting_title(text(&view.waiting_title));
    window.set_waiting_detail(text(&view.waiting_detail));
    window.set_waiting_link(text(&view.waiting_link));
    window.set_waiting_code(text(&view.waiting_code));

    // Disc
    sync(window.get_drives(), strings(&view.drives), |m| {
        window.set_drives(m);
    });
    if window.get_drive_index() != view.drive_index {
        window.set_drive_index(view.drive_index);
    }
    window.set_disc_ready(view.disc_ready);
    window.set_disc_summary(text(&view.disc_summary));
    sync(window.get_candidates(), choices(&view.candidates), |m| {
        window.set_candidates(m);
    });
    window.set_candidate_index(view.candidate_index);
    window.set_lookup_status(text(&view.lookup_status));
    set_text_if_changed!(window, get_album_title, set_album_title, &view.album_title);
    set_text_if_changed!(
        window,
        get_album_artist,
        set_album_artist,
        &view.album_artist
    );
    set_text_if_changed!(window, get_year, set_year, &view.year);
    let tracks: Vec<UiTrack> = view
        .tracks
        .iter()
        .map(|track| UiTrack {
            number: text(&track.number),
            title: text(&track.title),
            length: text(&track.length),
        })
        .collect();
    let revision_changed = RELEASE_REVISION.with(|cell| {
        let changed = cell.get() != Some(view.release_revision);
        cell.set(Some(view.release_revision));
        changed
    });
    if revision_changed {
        // New album details: rebuild the rows so every field shows them.
        window.set_tracks(ModelRc::new(VecModel::from(tracks)));
    } else {
        sync(window.get_tracks(), tracks, |m| window.set_tracks(m));
    }
    apply_cover(window, view.cover);
    window.set_destination_existing(view.destination_existing);
    window.set_destination(text(&view.destination));
    sync(window.get_items(), choices(&view.items), |m| {
        window.set_items(m);
    });
    window.set_item_index(view.item_index);
    window.set_can_rip(view.can_rip);
    window.set_film_mode(view.film_mode);
    window.set_film_notice(text(&view.film_notice));
    let titles: Vec<UiTitle> = view
        .film_titles
        .iter()
        .map(|row| UiTitle {
            title: text(&row.title),
            detail: text(&row.detail),
            selected: row.selected,
        })
        .collect();
    sync(window.get_film_titles(), titles, |m| {
        window.set_film_titles(m)
    });
    window.set_film_can_choose_copy(view.film_can_choose_copy);
    window.set_film_viewing_copy(view.film_viewing_copy);

    // Rip
    window.set_rip_title(text(&view.rip_title));
    window.set_rip_detail(text(&view.rip_detail));
    sync(window.get_rip_rows(), progress_rows(&view.rip_rows), |m| {
        window.set_rip_rows(m);
    });
    window.set_ctdb(text(&view.ctdb));
    window.set_rip_running(view.rip_running);
    window.set_rip_decision(view.rip_decision);
    sync(window.get_rip_issues(), strings(&view.rip_issues), |m| {
        window.set_rip_issues(m);
    });
    window.set_rip_failed(view.rip_failed);

    // Upload
    window.set_upload_title(text(&view.upload_title));
    sync(
        window.get_upload_rows(),
        progress_rows(&view.upload_rows),
        |m| {
            window.set_upload_rows(m);
        },
    );
    window.set_upload_running(view.upload_running);
    window.set_upload_stopped(view.upload_stopped);

    // Done
    window.set_done_title(text(&view.done_title));
    window.set_done_link(text(&view.done_link));
    sync(window.get_done_lines(), strings(&view.done_lines), |m| {
        window.set_done_lines(m);
    });
    window.set_can_eject(view.can_eject);

    // Settings
    set_text_if_changed!(window, get_offset_text, set_offset_text, &view.offset_text);
    window.set_offset_hint(text(&view.offset_hint));
    if window.get_test_and_copy() != view.test_and_copy {
        window.set_test_and_copy(view.test_and_copy);
    }
    set_text_if_changed!(window, get_keep_text, set_keep_text, &view.keep_text);
    window.set_settings_problem(text(&view.settings_problem));
    window.set_can_disconnect(view.can_disconnect);
    window.set_version(text(&view.version));
    window.set_source_url(text(&view.source_url));
}

/// Shows the front cover. Slint loads images from files, so the bytes are
/// written to a temporary file first (only when the cover changes).
fn apply_cover(window: &MainWindow, cover: Option<CoverArt>) {
    let unchanged = COVER.with(|current| *current.borrow() == cover);
    if unchanged {
        return;
    }
    COVER.with(|current| current.replace(cover.clone()));
    let Some(cover) = cover else {
        window.set_has_cover(false);
        return;
    };
    let extension = match cover.mime.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let path = std::env::temp_dir().join(format!(
        "library-rip-cover-{}.{extension}",
        std::process::id()
    ));
    let loaded = std::fs::write(&path, &cover.bytes[..])
        .map_err(|error| error.to_string())
        .and_then(|()| {
            slint::Image::load_from_path(&path).map_err(|_| "unreadable image".to_owned())
        });
    match loaded {
        Ok(image) => {
            window.set_cover(image);
            window.set_has_cover(true);
        }
        Err(error) => {
            tracing::warn!(%error, "could not show the cover");
            window.set_has_cover(false);
        }
    }
}
