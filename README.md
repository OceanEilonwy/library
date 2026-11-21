# The Library

A home catalogue for books, music, films, camping kit, tools — anything on
the shelves, in the shed or in the loft. One small Rust program (axum on
hyper, maud templates, htmx 4) with everything in a single SQLite file.

- **Search** with words, wildcards (`sea*`, `gr?y`), phrases, `AND`/`OR`/`NOT`
  and fields (`author:`, `year:1960..1975`, `type:`, `in:shed`, `loan:*`,
  `idea:"…"`), plus **meaning-based search** from a small local model, so
  "books about the sea" finds *Moby-Dick* even though the word isn't in it.
- **Locations** as a tree: `Shed › Camping shelf › Crate 1`.
- **Lending**: anyone can lend an item by giving a name; the date is recorded.
- **Adding items**: look up by title, ISBN or barcode (Open Library, Google
  Books, MusicBrainz, TMDb, a barcode database), scan a barcode with the
  phone camera, add photos, add your own fields per type, and get a gentle
  warning about possible duplicates (never a hard block).
- **No accounts for the catalogue**: the site is public behind your reverse
  proxy's basic auth; editing needs the admin password, remembered for the
  browser session.
- **The Player**: digital copies of your CDs, DVDs and books (music, films,
  PDFs) play in an installable web app at `/player/`, with simple accounts
  (each approved by the librarian), playlists, favourites and offline
  downloads. See [The Player](#the-player).
- **Library Rip**: a small desktop app that copies CDs into the library with
  checked, archival-quality rips, and DVDs as a lossless MKV plus a WebM copy
  for the Player (through your own MakeMKV and ffmpeg). See [Library Rip](#library-rip).

## Running it

You need Rust (1.88 or newer) on Linux x86_64.

```sh
cargo build --release
sudo install -m 755 target/release/library /usr/local/bin/library

# Configure
sudo mkdir -p /etc/library
sudo cp library.example.toml /etc/library/library.toml
library hash-password            # prints admin_password_hash = "…"
sudoedit /etc/library/library.toml   # paste that line, set data_dir

# Optional: fetch the ~130 MB model now rather than on first start
library --config /etc/library/library.toml fetch-model

# Try it
library --config /etc/library/library.toml serve
# → open http://127.0.0.1:8080
```

As a service, with HTTPS and basic auth in front:

```sh
sudo useradd --system --home /var/lib/library --shell /usr/sbin/nologin library
sudo cp deploy/library.service /etc/systemd/system/
sudo systemctl enable --now library
# then adapt deploy/Caddyfile (domain + `caddy hash-password` output)
```

Without a reverse proxy doing basic auth, set `site_password` in the config
and the library asks for it itself (any user name works).

Settings can also come from the environment, which overrides the file:
`LIBRARY_ADMIN_PASSWORD` (plain; hashed at startup), `LIBRARY_ADMIN_PASSWORD_HASH`,
`LIBRARY_SITE_PASSWORD`, `LIBRARY_TITLE`, `LIBRARY_TMDB_TOKEN`,
`LIBRARY_LOOKUP_CONTACT`, `LIBRARY_SEMANTIC` and `LIBRARY_LOOKUPS` (`on`/`off`);
and `library serve --listen 127.0.0.1:9000 --data-dir /path` overrides those two.
The monokulo-nodo package uses these.

If you use nginx instead of Caddy, pass the original host through
(`proxy_set_header Host $host; proxy_set_header X-Forwarded-Proto $scheme;`):
the app refuses form posts whose `Origin` doesn't match the host.

The library is usable straight away; meaning-based search switches on once
the model has loaded (a few seconds; the first start downloads it), and
items are indexed in the background. The Librarian's desk (`/admin`) shows
the status.

## Using it

- **Front page**: the big search line, categories, and "Advanced search"
  (opens in place on wide screens, as its own page on phones).
- **Librarian** (top right): enter the admin password to add and edit. It
  stays unlocked until the browser closes.
- **New entry**: pick the type, look the item up (or scan the barcode, or
  type it in), choose a match to fill the form, set the location, save.
  Duplicates are flagged with what differs ("same title & creator,
  different publisher"); tick "keep both" for another edition.
- **Your own fields**: "+ Add a field" on the form (text, number, date,
  choice list or yes/no), for every item of that type or just one item.
  New types (e.g. "Equipment · Sports") from "+ New type".
- **Librarian's desk** (`/admin`): library title and tagline, types and
  fields, and places (rename, remove empty ones).

### Search language

| You type | Finds |
|---|---|
| `le guin` | both words (AND is implied) |
| `sea*` / `gr?y` | wildcards |
| `"the left hand"` | exact phrase |
| `miyazaki NOT ponyo`, `-film` | exclusion |
| `(book OR film) earthsea` | grouping |
| `title:` `author:` `publisher:` `tag:` `isbn:` | one field |
| `year:1990`, `year:1984..2001`, `year:1980s`, `year:..1950` | dates |
| `type:book`, `type:camping`, `type:equipment` | kind of thing |
| `in:shed`, `in:lounge/bookcase*` | location (and everything inside) |
| `loan:*`, `loan:kasia`, `loan:no` | lending |
| `idea:"lonely lighthouse keepers"` | by meaning |

Plain words also widen to close matches by meaning ("by idea" in the
results), with weaker ones under "See also".

### Hand-drawn art

Drop image files (SVG, PNG, WebP or JPG) into the art directory
(`<data_dir>/art` by default); they appear within 30 seconds:

| File name | Where it shows |
|---|---|
| `hero.*` | above the title on the front page |
| `margin-left.*`, `margin-right.*` | either side of the big search line (wide screens) |
| `empty.*` | when a search finds nothing |

## The Player

The Player is a separate web app (a PWA, built with Solid and Vite) that the
library serves at `/player/`. It plays the digital copies you upload:
albums with a queue, back/forward 10 s and lock-screen controls, films with
subtitles, and PDFs. Tracks, albums and playlists can be downloaded to the
device for offline listening, and it installs like an app on phones and
desktops. A **▶ Player** button on every page of the catalogue opens it.

### Building it

You need Node.js 20 or newer. Build the Player before the server, because
release builds embed it:

```sh
npm --prefix player ci && npm --prefix player run build   # → player/dist
cargo build --release
```

Until it is built, `/player/` shows a page saying how. (Debug builds read
`player/dist` from disk, so a rebuilt Player shows up without rebuilding the
server.) For working on the Player itself, `npm --prefix player run dev`
serves it on port 5173 and passes `/api`, `/photos` and `/static` to the
library on port 8080 (`LIBRARY_BACKEND` changes that).

### Trying it locally

`library.dev.toml` is a development config that keeps everything in
`./data-dev` (gitignored), listens on **127.0.0.1:8090** (so it can run
beside a real library on 8080), has meaning search and online lookups
switched off, and uses the admin password **librarian**.
`scripts/dev-seed.py` fills it with demo data. From the repository root:

```sh
npm --prefix player ci && npm --prefix player run build   # the Player → player/dist
scripts/dev-seed.py          # wipes ./data-dev, then seeds it (needs Python 3.11+;
                             # uses ffmpeg for the sample files when installed)
cargo run -- --config library.dev.toml serve
```

Then open:

- <http://127.0.0.1:8090/player/>: the Player. Sign in as **adult** /
  **password123** (sees everything) or **kid** / **password123** (a child
  account), or ask for a new account (it waits for your approval).
- <http://127.0.0.1:8090/>: the catalogue. **Unlock** with **librarian** for
  the Librarian's desk, the upload studio (an item's **Add or manage files**)
  and **Player accounts**, where **newbie** (password **password123**) is
  waiting for approval so you can try approving it.

The seed makes, through the real upload endpoint: "Sine Songs" (three tones,
FLAC/MP3/Opus), "Late Night Tones" (marked Restricted, so only adults see
it), "Test Pattern" (a short film with English and French subtitles) and
"A Tiny Book" (a PDF), plus playlists that mix in restricted tracks
("Saturday mix"), are only restricted tracks ("After hours", hidden from
kid) or belong to kid ("Kid's picks"). Run it again any time to start
fresh; it refuses to run while something is listening on the config's port,
and refuses to wipe a folder called `data`.

To work on the Player with hot reloading, keep the server running and run
`LIBRARY_BACKEND=http://127.0.0.1:8090 npm --prefix player run dev`, then
open <http://localhost:5173/player/>.

### Tests

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd player
npm run typecheck && npm test        # Vitest unit and component tests
npm run build && npm run test:e2e    # Playwright, against the real server
```

The end-to-end tests need Chromium once (`npx playwright install chromium`),
Python 3 and, for the film, ffmpeg. They seed a throwaway data folder with
`scripts/dev-seed.py`, start the server on port 18431 (`E2E_PORT` changes
it), and stop it and delete the folder afterwards (`E2E_KEEP=1` keeps it, with
the server log). `LIBRARY_BIN` points them at a server binary instead of
`cargo build`.

### Accounts

Digital copies need an account; the catalogue doesn't. People ask for one
in the Player (username, email, password), and **a librarian approves every
account before it can be used**: until then, signing in says the account is
waiting. Requests appear at the top of **Player accounts** on the
Librarian's desk (`/admin/accounts`; the desk's link shows how many are
waiting) under **Waiting for approval**, where you **Approve** each one,
choosing **Child** or **Adult** access as you do, or **Reject** it (which
deletes it).

**Child** accounts never see an item marked **Restricted (adults only)**,
not even in someone else's playlist. The librarian can change an approved
account between child and **adult** on the same page, where accounts can
also be deleted. From the server:

```sh
library --config /etc/library/library.toml create-account alice alice@example.org --adult
library --config /etc/library/library.toml approve-account bob            # keeps the access bob asked with
library --config /etc/library/library.toml approve-account carol --adult
library --config /etc/library/library.toml set-access alice child
```

`create-account` makes an account that is approved straight away; it asks
for the password (or reads `LIBRARY_ACCOUNT_PASSWORD`). In `library.toml`,
`[accounts] default_access = "adult"` makes Adult the access new requests
start with (and the choice pre-selected when approving), and
`open_registration = false` stops people asking for accounts in the Player.

Playlists are visible to every account, but each person only sees the
entries they're allowed to play; a playlist with nothing left to show is
hidden (except from the person who made it). Listening history stays on each
device.

### Adding digital copies

Unlock editing, open an item and choose **Add or manage files** under
**Digital copy** (or go to `/admin/items/{id}/media`). Drag in the folder
you ripped a CD or DVD into, or pick files or a folder. Before anything is
sent, the studio reads disc and track numbers and titles from the file
names (`01 - Title.flac`, `1-03 Title.mp3`, `CD2/04. Title.m4a`), measures
each file's length and pairs subtitle files with their film
(`Film.en.srt` becomes English subtitles for `Film.mp4`). Fix anything that
looks wrong, reorder by dragging, then **Upload all**: each file gets its own
progress bar and can be retried. Files already uploaded can be renamed,
renumbered, reordered and deleted, and subtitles added or removed.

**Fill from MusicBrainz.** While music files are waiting, a panel above the
list searches MusicBrainz for the CD (prefilled with the entry's artist and
title, plus its barcode if the identifier is one; you can also paste a
MusicBrainz release link). The releases found are ranked by how well their
track lengths fit your files, so the pressing you ripped usually comes
first, with a badge such as "Fits your 12 files". Pick one to see which file
would get which disc, track and title, each marked **Good**, **Check** (a
length a few seconds off, or a file name that disagrees) or **No match**
(nothing fits: those files are left alone). **Apply** fills the list; titles
and numbers you typed yourself are kept unless you tick "Overwrite titles
and numbers I've typed myself". It can also fill in the entry's year and
artist when they are empty. It needs `[lookup] enabled = true`; with
lookups off the panel says so and offers nothing. Requests to MusicBrainz
are spaced a second apart, identify the library with `[lookup] contact`,
and are remembered for an hour.

Accepted: music (MP3, M4A, AAC, FLAC, Ogg, Opus, WAV, WebM), films (MP4,
M4V, WebM, MKV, MOV), documents (PDF, EPUB) and subtitles (SRT, converted to
WebVTT, or VTT). Nothing else is converted: files are stored and streamed
exactly as uploaded, so use formats your browsers play (MP3/AAC/FLAC/Opus,
H.264 MP4). Tick **Restricted (adults only)** on an item's edit form to keep
its files from child accounts.

Files live in `<data_dir>/media`; back it up with the rest of the data
directory. The largest single upload is `[media] max_upload_mb` (20000 MB by
default).

### Deploying

- Your reverse proxy must let big uploads through: in Caddy,
  `request_body { max_size 20GB }` (as in `deploy/Caddyfile`); in nginx,
  `client_max_body_size 20g;` and consider `proxy_request_buffering off;`.
- Basic auth in front of the whole site works for the Player too: browsers
  send the same credentials for the app, its service worker and streams.
- Streams support byte ranges, so seeking works through the proxy; they are
  never compressed by the library.

## Library Rip

Library Rip (in `rip/`, its own Cargo package) is a desktop app that turns
a CD into an exact, verified copy (FLAC per track, checked against
AccurateRip and CTDB, with a cue sheet and a rip log) and uploads it to an
entry in the library. A DVD is copied with the MakeMKV you installed into a
lossless MKV (kept as an archive file), and the ffmpeg you installed makes a
WebM (AV1 and Opus) copy the Player plays. Blu-ray is untested. The
contract between it and the server is `docs/rip.md`; how to build it is in `rip/README.md`.

- **Get it**: `/ripper` ("Get Library Rip", linked from the Librarian's desk
  and the upload studio) lists the builds you copy into
  `<data_dir>/downloads/library-rip/` (or `[ripper] downloads_dir`), with each
  file's size and SHA-256, and explains how to install MakeMKV and ffmpeg for films.
- **Connect it**: in Library Rip, type the library's address and press
  Connect. Your browser opens `/connect`; unlock as the librarian and say
  **Yes, connect**. If the browser is on another device, Library Rip shows a
  code like `WXYZ-1234` to type on `/connect/device`. Either way the app
  gets its own API key (only its SHA-256 is stored).
- **Disconnect it**: the desk lists **Connected rippers** (name, when
  connected, when last used) with **Disconnect**; the app can also
  disconnect itself. A disconnected key stops working at once.
- **What it uploads**: each track as music (playable in the Player), and the
  rip log, cue sheet and `rip.json` as **archive files**: kept for the
  record, listed under "Archive files" on the item page for the librarian to
  download, and never shown in the Player. Every upload's SHA-256 is checked
  as it arrives and stored.
- An API key only opens `/api/rip/…`: never the Player's API or the desk.
  Because Library Rip sends its key in the `Authorization` header, your
  reverse proxy's basic auth must leave `/api/rip/*` alone (as
  `deploy/Caddyfile` does); `site_password` already does.

### Trying Library Rip locally

```sh
# Terminal 1: the dev library on http://127.0.0.1:8090 (admin password "librarian")
cargo run -- --config library.dev.toml serve

# Terminal 2: the ripper's window; connect to http://127.0.0.1:8090
cd rip && cargo run
```

No drive? Make a simulated disc and serve stand-in MusicBrainz, AccurateRip
and CTDB answers for it, then rip it with or without the window:

```sh
cd rip
cargo run -- make-fake-disc /tmp/disc --tracks 3 --seconds 20 --error 500:jitter
cargo run -- mock-services /tmp/disc --listen 127.0.0.1:8092   # prints export lines
# in another terminal, after pasting those export lines:
cargo run -- --fake-disc /tmp/disc                              # the window
cargo run -- headless --library http://127.0.0.1:8090 --fake-disc /tmp/disc --device-code
```

With a real drive, `cargo run` in `rip/` lists it on the **Disc** screen
(libcdio-paranoia; system packages in `rip/README.md`). `cargo test` in
`rip/` includes `tests/e2e_headless.rs`, which builds and starts this server
with a temporary data folder and rips a simulated disc into it end to end.

## Maintenance

```sh
library --config /etc/library/library.toml backup /path/to/library-2026-11-18.sqlite3
library --config /etc/library/library.toml reindex   # rebuild search indexes
```

Back up the data directory (`library.sqlite3`, `photos/`, `media/`, `session.key`);
the backup command writes a consistent copy of the database even while the
server runs. `models/` can be re-downloaded.

## Development

```sh
cargo test                     # unit + HTTP tests (no network needed)
LIBRARY_TEST_MODEL_DIR=data/models/BAAI--bge-small-en-v1.5 cargo test --release real_model
cargo clippy --all-targets
```

Layout: `src/db` (schema, items, locations, loans, photos, accounts, media
files, playlists, favourites), `src/search`
(query parser, SQL compiler, hybrid ranking), `src/embed` (candle BERT and
the background indexer), `src/lookup.rs`, `src/musicbrainz.rs` (release
track lists, rate limit, cache), `src/trackmatch.rs` (matching files to
tracks), `src/dupes.rs`, `src/media.rs`
(photos, barcodes), `src/digital.rs` (upload rules, subtitles), `src/views`
(maud), `src/web` (handlers, auth, middleware, the Player's JSON API in
`api.rs`/`accounts.rs`/`stream.rs`, the upload studio in `studio.rs` and
its MusicBrainz panel in `fill.rs`, Library Rip's API in `rip.rs` and its
connect and download pages in `connect.rs`),
`static/` (CSS, htmx, small progressive-enhancement JS, fonts), `player/`
(the Player PWA). The contract between the server and the Player is
`docs/player.md`.

The design reference (the Broadsheet canvas) is in `design-canvas/`.

Fonts: Fraunces, Instrument Sans and IBM Plex Mono (SIL Open Font
License), self-hosted. htmx 4.0.2 (BSD Zero Clause) is vendored.

`cargo deny check licenses` checks that every Rust dependency's licence can
be combined with the AGPL (the allowed list is in `deny.toml`).

## Licence

The Library, the Player and Library Rip are free software under the
[GNU Affero General Public License, version 3 or later](LICENSE)
(AGPL-3.0-or-later). You may use, study, share and change them; if you share
them, changed or not, you share them under the same licence with their
source code.

What the AGPL adds to the ordinary GPL is its network clause (section 13):
if you run a **changed** copy of the library on a server that other people
use over a network, you must offer those people the source code of your
changed version. For self-hosters that means:

- Running it unchanged for your household or friends asks nothing of you:
  every page's footer, the Player's about screen and Library Rip already
  link to the source code (`source_url`, by default
  https://github.com/OceanEilonwy/library).
- If you change the code and let others use it, publish your changes
  somewhere they can get them (a public Git repository is enough) and set
  `source_url` in `library.toml` (or `LIBRARY_SOURCE_URL`) to point there.
- Your catalogue, photos and media are yours: the licence covers the
  program, not what you keep in it.
