# Library Rip

An archival disc ripper for **The Library**. Put in an audio CD, confirm
what it is, press **Rip**: Library Rip reads it securely (libcdio-paranoia),
checks it against AccurateRip and the CUETools database, writes one FLAC per
track with tags and cover, a cue sheet, a rip log and `rip.json`, and uploads
everything to your library. Film discs (via a user-installed `makemkvcon`)
come in later stages. The contract with the server is
[`docs/rip.md`](../docs/rip.md).

Licence: AGPL-3.0-or-later, like the rest of the repository. Slint is used
under its GPLv3 option; libcdio and libcdio-paranoia are GPLv3; libFLAC is
BSD-licensed.

## Building

```sh
cd rip
cargo build            # GUI + headless, real drives (feature `cdio`)
cargo test
cargo clippy --all-targets -- -D warnings
cargo run              # opens the window
```

`rip/` is a standalone Cargo package (its own `Cargo.lock`, target in
`rip/target`); the root `library` package excludes it.

Features:

| Feature | Default | What it does |
|---|---|---|
| `cdio` | on | Real optical drives through libcdio + libcdio-paranoia. Off: only simulated discs. |
| `testsupport` | off | Test tools: the simulated-disc generator and stand-in services, and the `--fake-disc`, `headless`, `make-fake-disc` and `mock-services` commands. `cargo test` turns it on by itself (a dev-dependency of the package on itself); release builds never have it. |

## Running it against the dev library

Start the dev library in one terminal (from the repository root; the admin
password is `librarian`, everything lives in `./data-dev`):

```sh
cargo run -- --config library.dev.toml serve      # http://127.0.0.1:8090
```

and Library Rip in another:

```sh
cd rip
cargo run                                          # opens the window
```

Type `http://127.0.0.1:8090`, press **Connect**, and say **Yes, connect** in
the browser tab that opens (unlock with `librarian` first). The window then
shows your drives.

### Testing with a simulated disc (no drive needed)

A simulated disc is a folder with `disc.json` and one raw PCM file per track
(format in `src/fakedisc.rs` and `docs/rip.md`). The commands below are test
tools in the `testsupport` feature, so they need `--features testsupport`
(release builds do not have them). Injected read problems
(`--error LBA:KIND`, KIND `unreadable`, `jitter` or `flaky_once`) exercise
the secure reader.

```sh
cd rip
cargo run --features testsupport -- make-fake-disc /tmp/disc --tracks 3 --seconds 20 \
    --error 500:jitter --error 2000:flaky_once
# Add --enhanced for an enhanced CD (CD-Extra): after the audio, the gap
# between sessions and a data track in session 2, which the simulated drive
# refuses to read as audio.

# Stand-in MusicBrainz, Cover Art Archive, AccurateRip and CTDB that know
# this disc (album "Sine Tones" by "The Test Card", with a cover), so
# nothing leaves the machine. It prints `export LIBRARY_RIP_*_URL=…` lines.
cargo run --features testsupport -- mock-services /tmp/disc --listen 127.0.0.1:8092
# Add --pressing 102 (or several, OFFSET[:CONFIDENCE]) to make AccurateRip
# and CTDB know only another pressing, whose audio sits 102 samples later:
# the rip is then "Accurately ripped (confidence 12, pressing offset +102)".

# In another terminal: paste those export lines, then either open the
# window with the simulated disc as its only drive…
cargo run --features testsupport -- --fake-disc /tmp/disc
# …or rip without a window, connecting with a code typed on the library:
cargo run --features testsupport -- headless --library http://127.0.0.1:8090 --fake-disc /tmp/disc \
    --device-code --test-and-copy --key-file /tmp/rip-key.json --keep /tmp/rips
```

The headless run prints a code: open `http://127.0.0.1:8090/connect/device`
(unlocked as the librarian), type it and press **Yes, connect**. Without
`--device-code` it prints a `/connect?…` address to open instead. Without
the `LIBRARY_RIP_*_URL` variables it asks the real MusicBrainz, AccurateRip
and CTDB, which do not know a simulated disc ("Not in database").

### Testing with a real drive

Install the system libraries below, put an audio CD in the drive and run
`cargo run` (the default `cdio` feature reads real drives through
libcdio-paranoia). Pick the drive on the **Disc** screen; the read offset
comes from the AccurateRip drive list, or set it under **Settings**. A
first rip of a disc AccurateRip knows well is the best check that the
offset is right: every track should say "Accurately ripped" with no
"pressing offset" (a pressing offset on every disc you rip, always the
same, suggests the drive offset is wrong by that much). Enhanced CDs rip
their audio and leave the data session alone; mixed-mode CDs (data first)
are refused. On Linux your
user needs read access to the drive (usually the `optical` or `cdrom`
group).

### End-to-end test

`tests/e2e_headless.rs` proves the whole of stage 1 against the real
server: it builds the server in the repository root (`cargo build --bin
library`, into `../target` or `LIBRARY_SERVER_TARGET_DIR`), starts it on a
free port with a copy of `library.dev.toml` pointing at a fresh temporary
data folder, makes a three-track disc with a jittery and a flaky sector,
starts the mock services in-process, runs the real `library-rip headless
--device-code --test-and-copy` binary, approves the code as the librarian
over HTTP (`/unlock`, then `/connect/device`), and checks through the rip
API and the Player API that the entry has the right title, artist and year,
three FLAC tracks with the right numbers and durations, the log, cue sheet
and `rip.json` as archive files hidden from the Player, every SHA-256, and
the cover. A second ripper then connects through the browser handshake
(`/connect`, loopback listener, PKCE) and disconnects itself. A second test
(`enhanced_cd_and_another_pressing_reach_the_real_library`) rips an
enhanced CD (the data track is never read, counted or uploaded) and then a
disc the mock databases know only from a pressing 102 samples later, which
must come out accurately ripped with that pressing offset. It runs as
part of `cargo test`; the first server build takes a few minutes. Set
`LIBRARY_SERVER_BIN` to reuse a built server, or `LIBRARY_RIP_SKIP_E2E=1`
to skip it; if the server cannot be built the test fails with the build
error (only `LIBRARY_RIP_SKIP_E2E` skips it).

### Headless runner

`library-rip headless --library URL --fake-disc DIR` (a test tool: builds
with the `testsupport` feature only) runs the whole flow without a window: connect, read, identify (first MusicBrainz match, else
CD-Text, else "Unknown album"), rip, verify, encode, upload to a new
catalogue entry, eject. Progress goes to standard output.

| Option | Meaning |
|---|---|
| `--device-code` | Connect with a code typed on the library (printed) instead of the browser page (whose address is printed) |
| `--offset N` | Drive read offset in samples (default: the AccurateRip drive list) |
| `--test-and-copy` | Read every track twice and compare |
| `--keep DIR` | Also keep the rip in `DIR/<Artist>/<Album>/` |
| `--upload-with-errors` | Upload even if some sectors could not be read reliably |
| `--key-file FILE` | Keep the connection in `FILE` so later runs need no approval |

Exit codes: `0` done; `1` something else went wrong; `2` bad command line;
`3` could not connect to the library; `4` drive or disc problem; `5` the rip
has read errors and was not uploaded; `6` the upload failed; `130` stopped
with Ctrl-C. Hidden options (or `LIBRARY_RIP_*_URL` variables) point
MusicBrainz, the Cover Art Archive, AccurateRip and CTDB at test servers:

| Variable | Option | Service |
|---|---|---|
| `LIBRARY_RIP_MUSICBRAINZ_URL` | `--musicbrainz-url` | MusicBrainz web service (`…/ws/2/`) |
| `LIBRARY_RIP_COVER_ART_URL` | `--cover-art-url` | Cover Art Archive |
| `LIBRARY_RIP_ACCURATERIP_URL` | `--accuraterip-url` | AccurateRip database (`…/accuraterip/`) |
| `LIBRARY_RIP_DRIVE_OFFSETS_URL` | `--drive-offsets-url` | AccurateRip `DriveOffsets.bin` |
| `LIBRARY_RIP_CTDB_URL` | `--ctdb-url` | CUETools database |

`library-rip mock-services DIR` serves all five for a simulated disc and
prints the variables. The window reads the same variables.

## Releases

Pushing a tag `rip-vX.Y.Z` (matching `version` in `Cargo.toml`) runs
`.github/workflows/rip-release.yml`: Linux tar.gz and AppImage, a Windows
zip (MSYS2 UCRT64 build with its DLLs), macOS dmg and zip per architecture,
and `SHA256SUMS`, attached to a GitHub release. Copy them into the library's
`<data_dir>/downloads/library-rip/`. Nothing is signed or notarised yet.
Each package bundles libcdio and libcdio-paranoia (GPLv3), libFLAC and, on
Windows, libiconv (LGPL), and carries their source in a `sources` folder
with `SOURCES.txt` (`rip/packaging/collect-sources.sh` fetches it from
Ubuntu, MSYS2 or Homebrew, whichever built the copy that was bundled).

To try the packaging without publishing anything, push a branch named
`ci/rip-release-<anything>` (or run the workflow by hand from `main`): the
same builds run, and the packages plus `SHA256SUMS` are left as the run's
`library-rip-<version>` artifact (`gh run download <run-id>`). Only a
`rip-v*` tag publishes a release.

Pull requests touching `rip/` (and pushes to `main` or `ci/rip-*`
branches) run `.github/workflows/rip-ci.yml`: fmt, clippy and all tests on
Linux, including the end-to-end test against the real server; clippy and
the tests except the end-to-end one on Windows (MSYS2 UCRT64) and macOS.

## System dependencies

Found through `pkg-config` by `build.rs`.

| OS | Packages |
|---|---|
| Arch / CachyOS | `libcdio libcdio-paranoia flac pkgconf` (Slint also needs `fontconfig libxkbcommon`, plus `dbus` for the keyring) |
| Debian / Ubuntu | `libcdio-dev libcdio-paranoia-dev libflac-dev pkg-config libfontconfig-dev libxkbcommon-dev libdbus-1-dev` |
| Fedora | `libcdio-devel libcdio-paranoia-devel flac-devel pkgconf fontconfig-devel libxkbcommon-devel dbus-devel` |
| macOS | `brew install libcdio libcdio-paranoia flac pkg-config` |
| Windows | `vcpkg install libcdio libflac` (MSVC), set `PKG_CONFIG_PATH` to vcpkg's `lib/pkgconfig`; or MSYS2 `mingw-w64-ucrt-x86_64-{libcdio,libcdio-paranoia,flac,pkgconf}` |

Film discs (stage 2+) need MakeMKV's `makemkvcon`, installed by the user.

## Module map and lanes

Phase 0 laid out the skeleton; each lane owns its files and does not edit
the others'. Shared types live in `src/model.rs`; change them only by
agreement. Each module has its own error type.

| Path | Lane | Contents |
|---|---|---|
| `src/model.rs` | Phase 0 (shared) | TOC, positions, ids, read results, metadata, verification, report, upload plan |
| `src/fakedisc.rs` | Phase 0 (shared) | Simulated disc format: `disc.json` + `trackNN.raw` |
| `src/testsupport.rs` | Phase 0 (shared) | Sine-tone disc generator |
| `src/core/` | B | `discid`, `accuraterip`, `ctdb`, `pressing` (other pressings: CRCs over a window of shifts, streamed from the track files), `offset`, `cue`, `log`, `report_json` |
| `src/drive/` | C | `DiscDrive`/`DriveProvider` traits, `fake` (FakeDrive), `reader` (secure read, test-and-copy, offset, CRC, peak), `cdio/` (FFI, feature `cdio`) |
| `src/encode/` | C | libFLAC FFI (`ffi.rs`), `encode_flac`, `tags` (Vorbis comments), SHA-256 |
| `src/library/` | D | `api` (wire types), `client` (rip API), `connect/` (`pkce`, `loopback`, `device`), `keystore` |
| `src/musicbrainz/` | D | Disc-ID lookup, search, release metadata, Cover Art Archive |
| `src/app/` | E | `state` (screens), `pipeline` (drive → core → encode → library), `settings`, `headless`, `gui` |
| `ui/main.slint` | E | The window |
| `src/main.rs` | E | CLI: GUI, `headless`, `make-fake-disc`, `mock-services` |
| `src/mockservices.rs` | Integration | Stand-in MusicBrainz, Cover Art Archive, AccurateRip and CTDB for a simulated disc (feature `testsupport`) |
| `build.rs`, `Cargo.toml` | Phase 0 | Add dependencies only when unavoidable, and say so |

Tests: unit tests beside the code; integration tests in `tests/` (mock
servers with `axum`, never the real network): `headless.rs` (mock
library), `e2e_headless.rs` (the real server), `licences.rs` (`cargo deny
check licenses` with `rip/deny.toml`, skipped without cargo-deny),
`make_fake_disc.rs`.
