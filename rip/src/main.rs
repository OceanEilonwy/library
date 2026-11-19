// SPDX-License-Identifier: AGPL-3.0-or-later
//! `library-rip`: the GUI. With the `testsupport` feature (tests only, never
//! release builds) also `--fake-disc`, `headless`, `make-fake-disc` and
//! `mock-services`.

// Release builds on Windows are GUI programs: no console window opens.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(feature = "testsupport")]
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(feature = "testsupport")]
use std::sync::Arc;
#[cfg(feature = "testsupport")]
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use clap::Parser;
#[cfg(feature = "testsupport")]
use clap::Subcommand;
#[cfg(feature = "testsupport")]
use library_rip::app::headless::{self, HeadlessOptions};
#[cfg(feature = "testsupport")]
use library_rip::app::pipeline::Endpoints;
#[cfg(feature = "testsupport")]
use library_rip::app::state::library_address;
#[cfg(feature = "testsupport")]
use library_rip::model::{ReadMode, SampleOffset};

/// Library Rip: archival disc ripper for The Library.
///
/// With no command, opens the window.
#[derive(Debug, Parser)]
#[command(name = "library-rip", version, about, long_about = None)]
struct Cli {
    /// Open the window with a simulated disc instead of the real drives.
    #[cfg(feature = "testsupport")]
    #[arg(long, value_name = "DIR")]
    fake_disc: Option<PathBuf>,

    #[cfg(feature = "testsupport")]
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(feature = "testsupport")]
#[derive(Debug, Subcommand)]
enum Command {
    /// Rip a simulated disc and upload it without a window.
    ///
    /// Exit codes: 0 done; 1 something else went wrong; 2 bad command
    /// line; 3 could not connect to the library; 4 drive or disc problem;
    /// 5 the rip has read errors and was not uploaded; 6 the upload failed;
    /// 130 stopped with Ctrl-C.
    #[cfg(feature = "testsupport")]
    Headless(Box<HeadlessArgs>),
    /// Write a simulated disc of sine tones into a folder.
    #[cfg(feature = "testsupport")]
    MakeFakeDisc {
        /// Folder to create.
        dir: PathBuf,
        /// Number of tracks.
        #[arg(long, default_value_t = 3)]
        tracks: u8,
        /// Seconds per track.
        #[arg(long, default_value_t = 20)]
        seconds: u32,
        /// Simulated drive read offset in samples.
        #[arg(long, default_value_t = 6, allow_hyphen_values = true)]
        offset: i32,
        /// Inject a read problem at a sector, as LBA:KIND where KIND is
        /// unreadable, jitter or flaky_once (repeat for several).
        #[arg(long = "error", value_name = "LBA:KIND", value_parser = parse_injected_error)]
        errors: Vec<library_rip::fakedisc::InjectedError>,
        /// Make an enhanced CD (CD-Extra): the audio, the 11,400-sector gap
        /// between sessions and a data track in session 2.
        #[arg(long)]
        enhanced: bool,
    },
    /// Serve stand-in MusicBrainz, Cover Art Archive, AccurateRip and
    /// CUETools database answers for a simulated disc (no network needed).
    ///
    /// Prints the LIBRARY_RIP_*_URL variables to set for `library-rip`.
    #[cfg(feature = "testsupport")]
    MockServices {
        /// The simulated disc folder.
        dir: PathBuf,
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:0", value_name = "ADDRESS")]
        listen: std::net::SocketAddr,
        /// A pressing the AccurateRip and CTDB stand-ins know, as
        /// OFFSET[:CONFIDENCE] (offset in samples; repeat for several).
        /// Default: only the disc's own pressing, 0:12. Use e.g.
        /// `--pressing 102` to know only a pressing 102 samples later.
        #[arg(long = "pressing", value_name = "OFFSET[:CONFIDENCE]", allow_hyphen_values = true, value_parser = parse_pressing)]
        pressings: Vec<library_rip::mockservices::MockPressing>,
    },
}

#[cfg(feature = "testsupport")]
#[derive(Debug, clap::Args)]
struct HeadlessArgs {
    /// The library's address, e.g. http://localhost:8080
    #[arg(long, value_name = "URL")]
    library: String,
    /// A simulated disc folder (see `make-fake-disc`).
    #[arg(long, value_name = "DIR")]
    fake_disc: PathBuf,
    /// Connect with a code typed on the library instead of a browser page.
    #[arg(long)]
    device_code: bool,
    /// Drive read offset in samples (default: look the drive up).
    #[arg(long, allow_hyphen_values = true, value_name = "SAMPLES")]
    offset: Option<i32>,
    /// Read every track twice and compare.
    #[arg(long)]
    test_and_copy: bool,
    /// Also keep the rip in this folder.
    #[arg(long, value_name = "DIR")]
    keep: Option<PathBuf>,
    /// Upload even if some sectors could not be read reliably.
    #[arg(long)]
    upload_with_errors: bool,
    /// Keep the connection in this file so later runs need no approval.
    #[arg(long, value_name = "FILE")]
    key_file: Option<PathBuf>,
    /// MusicBrainz web service (for testing).
    #[arg(long, hide = true, env = "LIBRARY_RIP_MUSICBRAINZ_URL")]
    musicbrainz_url: Option<url::Url>,
    /// Cover Art Archive (for testing).
    #[arg(long, hide = true, env = "LIBRARY_RIP_COVER_ART_URL")]
    cover_art_url: Option<url::Url>,
    /// AccurateRip database (for testing).
    #[arg(long, hide = true, env = "LIBRARY_RIP_ACCURATERIP_URL")]
    accuraterip_url: Option<url::Url>,
    /// AccurateRip drive offset list (for testing).
    #[arg(long, hide = true, env = "LIBRARY_RIP_DRIVE_OFFSETS_URL")]
    drive_offsets_url: Option<url::Url>,
    /// CUETools database (for testing).
    #[arg(long, hide = true, env = "LIBRARY_RIP_CTDB_URL")]
    ctdb_url: Option<url::Url>,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let result = run(Cli::parse());
    result.unwrap_or_else(|error| {
        eprintln!("Error: {error:#}");
        ExitCode::FAILURE
    })
}

/// Release builds: the window and nothing else.
#[cfg(not(feature = "testsupport"))]
fn run(_cli: Cli) -> anyhow::Result<ExitCode> {
    library_rip::app::gui::run(None)
        .context("Library Rip could not start")
        .map(|()| ExitCode::SUCCESS)
}

/// Test builds: the window (optionally with a simulated disc) or one of
/// the test commands.
#[cfg(feature = "testsupport")]
fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        None => library_rip::app::gui::run(cli.fake_disc)
            .context("Library Rip could not start")
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Headless(args)) => run_headless(*args),
        Some(Command::MakeFakeDisc {
            dir,
            tracks,
            seconds,
            offset,
            errors,
            enhanced,
        }) => make_fake_disc(dir, tracks, seconds, offset, errors, enhanced)
            .map(|()| ExitCode::SUCCESS),
        Some(Command::MockServices {
            dir,
            listen,
            pressings,
        }) => mock_services(dir, listen, pressings).map(|()| ExitCode::SUCCESS),
    }
}

#[cfg(feature = "testsupport")]
fn run_headless(args: HeadlessArgs) -> anyhow::Result<ExitCode> {
    let library = match library_address(&args.library) {
        Ok(url) => url,
        Err(problem) => {
            eprintln!("{} {}", problem.message, problem.next_step);
            return Ok(ExitCode::from(2));
        }
    };
    let official = Endpoints::official().context("the built-in service addresses are invalid")?;
    let endpoints = Endpoints {
        musicbrainz: args.musicbrainz_url.unwrap_or(official.musicbrainz),
        cover_art: args.cover_art_url.unwrap_or(official.cover_art),
        accuraterip: args.accuraterip_url.unwrap_or(official.accuraterip),
        drive_offsets: args.drive_offsets_url.unwrap_or(official.drive_offsets),
        ctdb: args.ctdb_url.unwrap_or(official.ctdb),
    };
    let options = HeadlessOptions {
        library,
        fake_disc: args.fake_disc,
        device_code: args.device_code,
        offset: args.offset.map(SampleOffset),
        read_mode: if args.test_and_copy {
            ReadMode::TestAndCopy
        } else {
            ReadMode::Secure
        },
        keep: args.keep,
        upload_with_errors: args.upload_with_errors,
        key_file: args.key_file,
        endpoints,
        work_root: std::env::temp_dir().join(format!("library-rip-{}", std::process::id())),
    };
    let runtime = tokio::runtime::Runtime::new().context("could not start the runtime")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let report: headless::Reporter = Arc::new(|line: &str| println!("{line}"));
    let outcome = runtime.block_on(async {
        let watcher = {
            let cancel = Arc::clone(&cancel);
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("Stopping…");
                    cancel.store(true, Ordering::Relaxed);
                }
            })
        };
        let outcome = headless::run(&options, report, Arc::clone(&cancel)).await;
        watcher.abort();
        outcome
    });
    // Only removes the temporary folder when empty (a rip with read errors
    // is left there for inspection).
    let _ = std::fs::remove_dir(&options.work_root);
    match outcome {
        Ok(outcome) => {
            println!(
                "Done: {} files uploaded ({} already there) to {}",
                outcome.uploaded, outcome.skipped, outcome.item_url
            );
            for note in &outcome.notes {
                println!("Note: {note}");
            }
            if let Some(kept) = &outcome.kept {
                println!("A copy is kept in {}", kept.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            let problem = error.problem();
            eprintln!("{}\n{}", problem.message, problem.next_step);
            Ok(ExitCode::from(error.exit_code()))
        }
    }
}

#[cfg(feature = "testsupport")]
fn make_fake_disc(
    dir: PathBuf,
    tracks: u8,
    seconds: u32,
    offset: i32,
    errors: Vec<library_rip::fakedisc::InjectedError>,
    enhanced: bool,
) -> anyhow::Result<()> {
    use library_rip::testsupport::{FakeDiscOptions, make_fake_disc};
    let options = FakeDiscOptions {
        tracks,
        seconds,
        read_offset: SampleOffset(offset),
        errors,
        enhanced,
    };
    let disc = make_fake_disc(&dir, &options)
        .with_context(|| format!("could not make a simulated disc in {}", dir.display()))?;
    let data = disc.spec.toc.data_tracks().count();
    println!(
        "Made a simulated disc with {} audio tracks{} in {}",
        disc.spec.toc.audio_tracks().count(),
        if data > 0 {
            " and a data track in a second session (an enhanced CD)"
        } else {
            ""
        },
        dir.display()
    );
    Ok(())
}

/// Serves the mock services until Ctrl-C.
#[cfg(feature = "testsupport")]
fn mock_services(
    dir: PathBuf,
    listen: std::net::SocketAddr,
    pressings: Vec<library_rip::mockservices::MockPressing>,
) -> anyhow::Result<()> {
    let mut options = library_rip::mockservices::MockOptions::default();
    if !pressings.is_empty() {
        options.pressings = pressings;
    }
    use std::io::Write as _;
    let runtime = tokio::runtime::Runtime::new().context("could not start the runtime")?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("could not listen on {listen}"))?;
        let mock = library_rip::mockservices::start_with(&dir, listener, &options)
            .await
            .with_context(|| format!("could not serve the disc in {}", dir.display()))?;
        println!("# Mock services for {} at {}", dir.display(), mock.base);
        println!("# MusicBrainz disc id: {}", mock.disc_id);
        for (name, value) in mock.environment() {
            println!("export {name}={value}");
        }
        std::io::stdout().flush().ok();
        tokio::select! {
            () = mock.wait() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        anyhow::Ok(())
    })
}

/// Parses `--pressing 102` or `--pressing -48:5`.
#[cfg(feature = "testsupport")]
fn parse_pressing(text: &str) -> Result<library_rip::mockservices::MockPressing, String> {
    let (offset, confidence) = match text.split_once(':') {
        Some((offset, confidence)) => (offset, Some(confidence)),
        None => (text, None),
    };
    let offset: i32 = offset
        .trim()
        .parse()
        .map_err(|_| format!("{offset:?} is not a number of samples"))?;
    let confidence = match confidence {
        Some(text) => text
            .trim()
            .parse()
            .map_err(|_| format!("{text:?} is not a confidence (1 to 255)"))?,
        None => library_rip::mockservices::ACCURATERIP_CONFIDENCE,
    };
    Ok(library_rip::mockservices::MockPressing { offset, confidence })
}

/// Parses `--error 700:jitter`.
#[cfg(feature = "testsupport")]
fn parse_injected_error(text: &str) -> Result<library_rip::fakedisc::InjectedError, String> {
    use library_rip::fakedisc::{InjectedError, InjectedErrorKind};
    use library_rip::model::Lba;
    let (lba, kind) = text
        .split_once(':')
        .ok_or_else(|| format!("write it as LBA:KIND, for example 700:jitter (got {text:?})"))?;
    let lba: u32 = lba
        .trim()
        .parse()
        .map_err(|_| format!("{lba:?} is not a sector number"))?;
    let kind = match kind.trim() {
        "unreadable" => InjectedErrorKind::Unreadable,
        "jitter" => InjectedErrorKind::Jitter,
        "flaky_once" | "flaky-once" => InjectedErrorKind::FlakyOnce,
        other => {
            return Err(format!(
                "{other:?} is not a kind of read problem: use unreadable, jitter or flaky_once"
            ));
        }
    };
    Ok(InjectedError {
        lba: Lba(lba),
        kind,
    })
}

#[cfg(all(test, feature = "testsupport"))]
mod tests {
    use super::*;
    use library_rip::fakedisc::InjectedErrorKind;

    #[test]
    fn injected_errors_parse() {
        let error = parse_injected_error("700:jitter").unwrap();
        assert_eq!(error.lba.0, 700);
        assert_eq!(error.kind, InjectedErrorKind::Jitter);
        assert_eq!(
            parse_injected_error("12:flaky_once").unwrap().kind,
            InjectedErrorKind::FlakyOnce
        );
        assert!(parse_injected_error("12").is_err());
        assert!(parse_injected_error("x:jitter").is_err());
        assert!(parse_injected_error("12:scratched").is_err());
    }

    #[test]
    fn pressings_parse() {
        let plain = parse_pressing("102").unwrap();
        assert_eq!((plain.offset, plain.confidence), (102, 12));
        let earlier = parse_pressing("-48:5").unwrap();
        assert_eq!((earlier.offset, earlier.confidence), (-48, 5));
        assert!(parse_pressing("x").is_err());
        assert!(parse_pressing("3:999").is_err());
    }

    #[test]
    fn the_command_line_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
