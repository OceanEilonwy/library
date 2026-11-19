// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every dependency's licence can be combined with the AGPL (rip/deny.toml).
//! Skipped, with a note, when cargo-deny is not installed.

use std::process::Command;

#[test]
fn dependency_licences_are_compatible_with_the_agpl() {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let installed = Command::new(&cargo)
        .args(["deny", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !installed {
        eprintln!("licences: SKIPPED, cargo-deny is not installed (cargo install cargo-deny)");
        return;
    }
    let output = Command::new(&cargo)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["deny", "--offline", "check", "licenses"])
        .output()
        .expect("cargo deny runs");
    assert!(
        output.status.success(),
        "cargo deny check licenses failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
