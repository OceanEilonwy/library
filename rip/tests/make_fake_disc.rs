// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `make-fake-disc` subcommand writes a disc that `FakeDrive` opens.

use library_rip::drive::DiscDrive;
use library_rip::drive::fake::FakeDrive;

#[test]
fn make_fake_disc_subcommand_writes_an_openable_disc() {
    let dir = tempfile::tempdir().unwrap();
    let disc_dir = dir.path().join("disc");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_library-rip"))
        .args(["make-fake-disc", "--tracks", "2", "--seconds", "1"])
        .arg(&disc_dir)
        .status()
        .unwrap();
    assert!(status.success());
    let mut drive = FakeDrive::open(&disc_dir).unwrap();
    let info = drive.disc_info().unwrap();
    assert_eq!(info.toc.tracks().len(), 2);
    assert_eq!(info.drive.model, "Simulated Drive");
}

#[test]
fn make_fake_disc_can_make_an_enhanced_cd() {
    let dir = tempfile::tempdir().unwrap();
    let disc_dir = dir.path().join("disc");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_library-rip"))
        .args([
            "make-fake-disc",
            "--tracks",
            "2",
            "--seconds",
            "1",
            "--enhanced",
        ])
        .arg(&disc_dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("2 audio tracks and a data track"), "{said}");
    let mut drive = FakeDrive::open(&disc_dir).unwrap();
    let toc = drive.disc_info().unwrap().toc;
    assert_eq!(toc.tracks().len(), 3);
    assert_eq!(toc.audio_tracks().count(), 2);
    assert_eq!(toc.session_count(), 2);
}
