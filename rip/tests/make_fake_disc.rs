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
