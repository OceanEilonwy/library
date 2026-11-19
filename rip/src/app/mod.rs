// SPDX-License-Identifier: AGPL-3.0-or-later
//! The application (lane E): screen state machine, the rip pipeline that
//! composes drive → core → encode → library, settings, the headless runner
//! and the Slint GUI.
//!
//! - [`state`]: screens, events and commands; pure and testable.
//! - [`view`]: turns the state into the strings and rows the window shows.
//! - [`controller`]: runs commands against a [`controller::Backend`] and
//!   feeds the results back as events.
//! - [`live`]: the real backend (drives, library, MusicBrainz, key store).
//! - [`pipeline`]: reading, verifying, encoding and uploading.
//! - [`headless`]: the whole flow without a window.
//! - [`gui`]: the Slint window.

pub mod controller;
pub mod gui;
pub mod headless;
pub mod live;
pub mod pipeline;
pub mod settings;
pub mod state;
pub mod view;

use crate::drive::DriveError;
use crate::library::LibraryError;

/// Errors surfaced to the user by the app. Each wraps a lane's error.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Drive problem.
    #[error(transparent)]
    Drive(#[from] crate::drive::DriveError),
    /// Checksum, database or report problem.
    #[error(transparent)]
    Core(#[from] crate::core::CoreError),
    /// Encoding problem.
    #[error(transparent)]
    Encode(#[from] crate::encode::EncodeError),
    /// Library problem.
    #[error(transparent)]
    Library(#[from] crate::library::LibraryError),
    /// MusicBrainz problem.
    #[error(transparent)]
    MusicBrainz(#[from] crate::musicbrainz::MusicBrainzError),
    /// A shared value did not satisfy its rules.
    #[error(transparent)]
    Model(#[from] crate::model::ModelError),
    /// Settings could not be read or written.
    #[error("could not use the settings file {path}: {reason}")]
    Settings {
        /// The file involved.
        path: std::path::PathBuf,
        /// Plain-English reason.
        reason: String,
    },
    /// A file or folder of the rip could not be written or read.
    #[error("could not use {path}: {source}")]
    Io {
        /// The file or folder involved.
        path: std::path::PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The web browser could not be opened.
    #[error("could not open {url} in the browser: {source}")]
    Browser {
        /// The page.
        url: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The user cancelled.
    #[error("cancelled")]
    Cancelled,
    /// A background task stopped unexpectedly.
    #[error("a background task stopped unexpectedly: {0}")]
    Task(String),
    /// The GUI failed to start.
    #[error("the window could not be opened: {0}")]
    Gui(String),
}

/// Result type of the app module.
pub type Result<T, E = AppError> = std::result::Result<T, E>;

impl AppError {
    /// An I/O error about `path`.
    pub fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        AppError::Io {
            path: path.into(),
            source,
        }
    }

    /// True when the user (or Ctrl-C) stopped the work.
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            AppError::Cancelled | AppError::Drive(DriveError::Cancelled)
        )
    }

    /// What to tell the user, and what they can do about it.
    pub fn problem(&self) -> Problem {
        match self {
            AppError::Cancelled => cancelled_problem(),
            AppError::Drive(error) => drive_problem(error),
            AppError::Library(error) => library_problem(error),
            AppError::MusicBrainz(error) => Problem::new(
                format!("MusicBrainz could not be asked about this disc ({error})."),
                "You can type the album details yourself, or try again later.",
            ),
            AppError::Core(error) => Problem::new(
                format!("Something went wrong while checking the rip: {error}."),
                "Try ripping the disc again. If it keeps happening, keep the rip log and report it.",
            ),
            AppError::Encode(error) => Problem::new(
                format!("The FLAC files could not be written: {error}."),
                "Check there is free space on this computer, then rip again.",
            ),
            AppError::Model(error) => Problem::new(
                format!("The disc gave a value that does not make sense: {error}."),
                "Clean the disc and try again.",
            ),
            AppError::Settings { path, reason } => Problem::new(
                format!(
                    "Your settings could not be saved in {} ({reason}).",
                    path.display()
                ),
                "Check that the folder exists and you can write to it.",
            ),
            AppError::Io { path, source } => Problem::new(
                format!("Could not use {} ({source}).", path.display()),
                "Check there is free space and that you can write to that folder, then try again.",
            ),
            AppError::Browser { url, .. } => Problem::new(
                "Your web browser could not be opened.",
                format!("Open {url} in a browser yourself."),
            ),
            AppError::Task(reason) => Problem::new(
                format!("Something stopped unexpectedly ({reason})."),
                "Try again. If it keeps happening, restart Library Rip.",
            ),
            AppError::Gui(reason) => Problem::new(
                format!("The window could not be opened ({reason})."),
                "Use `library-rip headless` instead, or check your display settings.",
            ),
        }
    }
}

fn cancelled_problem() -> Problem {
    Problem::new("Stopped.", "Start again whenever you are ready.")
}

fn drive_problem(error: &DriveError) -> Problem {
    match error {
        DriveError::NoDisc => Problem::new(
            "There is no disc in the drive.",
            "Put in an audio CD and press Look again.",
        ),
        DriveError::NoAudio => Problem::new(
            "This disc has no music on it.",
            "Library Rip copies audio CDs for now. Put in an audio CD and press Look again.",
        ),
        DriveError::Open { device, reason } => Problem::new(
            format!("The drive {device} could not be opened ({reason})."),
            "Check that it is plugged in and not being used by another program, then press Look again.",
        ),
        DriveError::Read { lba, reason } => Problem::new(
            format!("The drive could not read the disc at sector {lba} ({reason})."),
            "Clean the disc gently from the centre outwards and rip again.",
        ),
        DriveError::Cancelled => cancelled_problem(),
        DriveError::FakeDisc(error) => Problem::new(
            format!("The simulated disc could not be read: {error}."),
            "Make a new one with `library-rip make-fake-disc`.",
        ),
        DriveError::Io { path, source } => Problem::new(
            format!("Could not write {} ({source}).", path.display()),
            "Check there is free space on this computer, then rip again.",
        ),
    }
}

fn library_problem(error: &LibraryError) -> Problem {
    match error {
        LibraryError::Unauthorised => Problem::reconnect(
            "This computer is no longer connected to the library.",
            "Connect again. A librarian may have disconnected it from the Librarian's desk.",
        ),
        LibraryError::NotConnected => Problem::reconnect(
            "This computer is not connected to a library.",
            "Connect to your library first.",
        ),
        LibraryError::TimedOut => Problem::new(
            "Nobody answered in the browser in time.",
            "Press Connect again, then approve it in the browser within ten minutes.",
        ),
        LibraryError::ConnectFailed(reason) => Problem::new(
            format!("The library could not connect this computer ({reason})."),
            "Press Connect again. If it keeps happening, try Connect with a code.",
        ),
        LibraryError::Browser { url, .. } => Problem::new(
            "Your web browser could not be opened.",
            format!("Open {url} in a browser yourself, or use Connect with a code."),
        ),
        LibraryError::Listener(error) => Problem::new(
            format!("Library Rip could not wait for the browser's answer ({error})."),
            "Use Connect with a code instead.",
        ),
        LibraryError::TooLarge(message) => Problem::new(
            format!("The library will not take a file this big ({message})."),
            "Ask the librarian to raise the upload limit in the library's settings, then press Try again.",
        ),
        LibraryError::Rejected(message) => Problem::new(
            format!("The library did not accept the upload ({message})."),
            "Check the album details and try again.",
        ),
        LibraryError::FileChanged(path) => Problem::new(
            format!("{} changed while it was being uploaded.", path.display()),
            "Rip the disc again.",
        ),
        LibraryError::Unexpected(message) => Problem::new(
            format!("The library sent an answer Library Rip does not understand ({message})."),
            "Check that Library Rip and the library are both up to date.",
        ),
        LibraryError::NotALibrary(address) => Problem::new(
            format!("{address} does not look like a library."),
            "Check the address. It is the one you use to open the library in a browser.",
        ),
        LibraryError::TooOld { required, current } => Problem::new(
            format!("This library needs Library Rip {required} or newer; you have {current}."),
            "Download the latest Library Rip from the library's \"Get Library Rip\" page.",
        ),
        LibraryError::Denied => Problem::new(
            "The librarian did not allow the connection.",
            "Ask a librarian to approve it, then press Connect again.",
        ),
        LibraryError::Expired => Problem::new(
            "The connection request ran out of time.",
            "Press Connect again and approve it within ten minutes.",
        ),
        LibraryError::StateMismatch => Problem::new(
            "The browser sent back an unexpected answer.",
            "Press Connect again. Close any older connect pages first.",
        ),
        LibraryError::Status { status, message } => Problem::new(
            format!("The library answered with an error ({status}: {message})."),
            "Try again. If it keeps happening, check the library's log.",
        ),
        LibraryError::Http(error) => Problem::new(
            format!("Could not reach the library ({error})."),
            "Check the address and your network, and that the library is running, then try again.",
        ),
        LibraryError::Url(error) => Problem::new(
            format!("That is not a web address ({error})."),
            "Type the address you use to open the library, for example http://library.local:8080.",
        ),
        LibraryError::KeyStore(reason) => Problem::new(
            format!("The connection could not be saved on this computer ({reason})."),
            "You can keep working, but you will need to connect again next time.",
        ),
        LibraryError::Io { path, source } => Problem::new(
            format!("Could not use {} ({source}).", path.display()),
            "Check that the file still exists and can be read, then try again.",
        ),
    }
}

/// A problem told to the user: what happened, and what to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// What happened, in plain English.
    pub message: String,
    /// What the user can do about it.
    pub next_step: String,
    /// The connection is gone: the app returns to the Connect screen.
    pub reconnect: bool,
}

impl Problem {
    /// A problem that leaves the connection alone.
    pub fn new(message: impl Into<String>, next_step: impl Into<String>) -> Self {
        Problem {
            message: message.into(),
            next_step: next_step.into(),
            reconnect: false,
        }
    }

    /// A problem that means the library no longer accepts our key.
    pub fn reconnect(message: impl Into<String>, next_step: impl Into<String>) -> Self {
        Problem {
            reconnect: true,
            ..Problem::new(message, next_step)
        }
    }
}

impl From<&AppError> for Problem {
    fn from(error: &AppError) -> Self {
        error.problem()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_say_what_to_do_next() {
        let revoked = AppError::Library(LibraryError::Unauthorised).problem();
        assert!(revoked.reconnect);
        assert!(revoked.next_step.contains("Connect again"));

        let empty = AppError::Drive(DriveError::NoDisc).problem();
        assert!(!empty.reconnect);
        assert_eq!(empty.message, "There is no disc in the drive.");
        assert!(empty.next_step.contains("Look again"));

        assert!(AppError::Drive(DriveError::Cancelled).is_cancelled());
        assert!(AppError::Cancelled.is_cancelled());
        assert!(!AppError::Task("x".into()).is_cancelled());
    }
}
