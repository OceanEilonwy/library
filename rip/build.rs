// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build script: compiles the Slint UI and finds the native audio libraries.
//!
//! - libFLAC is always required (`src/encode/ffi.rs` binds it by hand).
//! - libcdio, libcdio_cdda and libcdio_paranoia are required only with the
//!   `cdio` feature (`src/drive/cdio/ffi.rs`).
//!
//! Library locations come from pkg-config, so on Windows/macOS set
//! `PKG_CONFIG_PATH` to the vcpkg/Homebrew prefix (see README.md).

use std::process::ExitCode;

fn main() -> ExitCode {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ui");

    if let Err(message) = probe("flac", "1.3") {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }
    if std::env::var_os("CARGO_FEATURE_CDIO").is_some() {
        for (name, version) in [
            ("libcdio", "2.0"),
            ("libcdio_cdda", "10.2"),
            ("libcdio_paranoia", "10.2"),
        ] {
            if let Err(message) = probe(name, version) {
                eprintln!("{message}");
                return ExitCode::FAILURE;
            }
        }
    }

    if let Err(error) = slint_build::compile("ui/main.slint") {
        eprintln!("Could not compile the Slint UI: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Asks pkg-config for `name` (at least `min_version`) and emits link flags.
fn probe(name: &str, min_version: &str) -> Result<(), String> {
    pkg_config::Config::new()
        .atleast_version(min_version)
        .probe(name)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "Library Rip needs the system library `{name}` (version {min_version} or newer), \
                 found through pkg-config. Install its development package; see rip/README.md.\n{error}"
            )
        })
}
