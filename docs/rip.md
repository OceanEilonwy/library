# Library Rip — archival disc ripper

Contract between the desktop ripper (`rip/`, a Slint app) and the library
server (`src/`). Both sides follow it; fix it here in the same change if it
is wrong. Licence: AGPL-3.0-or-later, like the rest of the repository.

## Goals

- A small desktop app, downloadable from the library's htmx site, that turns
  a physical disc into an archival copy and hands it to the library.
- Archival grade: bit-exact, verified, documented (log + checksums), never
  silently "good enough".
- Simple for a librarian: put in a disc, confirm what it is, press Rip.
- The server never transcodes. Anything the browser plays is made here.

## Stages

1. **Stage 1 (done):** connect handshake + API keys, ripper download page,
   audio CD ripping end to end (secure read, AccurateRip + CTDB verification,
   FLAC + cue + log, MusicBrainz disc-ID match, cover art, upload).
2. **Stage 2 (DVD ripping done; renditions and the picker not):** DVD via
   `makemkvcon`: lossless MKV master, one AV1/Opus WebM viewing copy (up to
   1080p, made by the user's `ffmpeg`), uploaded to a new Film entry; see
   "Film disc rip (stage 2, as built)". Not built yet: a 2160p copy for UHD
   sources, renditions on the server, a quality picker in the Player, adding
   to an existing entry, subtitles in the viewing copy.
3. **Stage 3:** Blu-ray / UHD via `makemkvcon`, HDR10 kept in the 2160p copy
   and tone-mapped to SDR for 1080p, picture-subtitle OCR (Tesseract) with a
   review step, forced subtitles burned in.

Decided: video ripping uses only a user-installed `makemkvcon` and `ffmpeg` (never bundled;
DVD ripping in MakeMKV is free; Blu-ray needs the user's MakeMKV key). There is
no libdvdcss path and no H.264/MP4 fallback. If `makemkvcon` is missing the app
explains how to install it per OS (Flathub `com.makemkv.MakeMKV`, the MakeMKV
forum tarballs, Windows/macOS installers) with a short note that breaking disc
copy protection is restricted by law in many countries.

## Repository layout

- `rip/` — the ripper, a Cargo package `library-rip` (binary `library-rip`).
  Stage 1 keeps it a standalone package (its own `Cargo.toml` and lockfile);
  the root may later become a workspace.
  - `src/lib.rs` + modules for everything testable without a drive or a GUI:
    TOC, MusicBrainz disc ID, AccurateRip and CTDB checksums, offset
    correction, secure-read verification logic, cue sheet, rip log, FLAC
    tagging, library API client, connect flow.
  - `src/drive/` — device access behind a `DiscDrive` trait. The real
    implementation uses libcdio + libcdio-paranoia (GPLv3, compatible with
    AGPLv3). A `FakeDrive` built from a TOC and PCM data (with injectable read
    errors and jitter) drives the tests.
  - `ui/*.slint` — the Slint UI (Slint under its GPLv3 option).
  - `src/model.rs` — the shared types every module agrees on;
    `src/fakedisc.rs` — the simulated disc format (below). Module and lane
    map: `rip/README.md`.
- `.github/workflows/rip-release.yml` — builds Linux (AppImage + tar.gz),
  Windows (zip) and macOS (dmg + zip, one per architecture: the bundled
  Homebrew libraries are per architecture) on a `rip-v*` tag, with
  `SHA256SUMS`; `.github/workflows/rip-ci.yml` checks pull requests.

## Connect handshake (OAuth-style loopback with PKCE, plus device code)

1. App: user types the library URL. `GET {base}/api/rip/info` →
   `{"name": "The Library", "version": "0.2.0", "minRipperVersion": "0.1.0", "sourceUrl": "https://…"}`
   confirms it is a library and checks compatibility.
2. App starts an HTTP listener on `127.0.0.1:<random port>`, makes a random
   `state` and a PKCE `code_verifier` (43–128 chars), and opens the browser at
   `{base}/connect?client=library-rip&name=<computer name>&redirect_uri=http://127.0.0.1:<port>/callback&state=<state>&code_challenge=<S256>&code_challenge_method=S256`.
3. Server `/connect` (htmx site): if not unlocked, shows the librarian unlock
   form (returning to the same URL). Then: "Connect **Henry's desktop**
   (Library Rip) to The Library? It will be able to add items and upload
   files. You can disconnect it at any time from the Librarian's desk." with
   **Yes, connect** and **No**. `redirect_uri` must be `http://127.0.0.1:<port>/…`
   or `http://localhost:<port>/…` (anything else is refused).
4. Yes → `302 redirect_uri?code=<one-time code>&state=<state>`.
   No → `302 redirect_uri?error=access_denied&state=<state>`.
   The loopback page tells the user they can close the tab.
5. App: `POST {base}/api/rip/token` JSON
   `{"grantType":"authorization_code","code":"…","codeVerifier":"…","redirectUri":"…"}`
   → `{"apiKey":"lrk_…","keyId":7,"keyName":"Henry's desktop","library":{"name":"The Library","url":"…"}}`.
   Codes are single use, expire after 5 minutes, and are bound to the
   challenge and redirect URI.
6. Fallback when the browser is on another device: `POST /api/rip/device`
   `{"name":"…"}` → `{"deviceCode":"…","userCode":"WXYZ-1234","verificationUri":"{base}/connect/device","interval":5,"expiresIn":600}`.
   The librarian opens `/connect/device`, types the code, presses Yes. The app
   polls `POST /api/rip/token {"grantType":"device_code","deviceCode":"…"}`
   (`428 {"error":"authorization_pending"}` until approved, `403` if refused,
   `410` when expired; `429` from the rate limiter makes the app add 5 s to
   its interval and keep polling). The app waits `interval` seconds before
   each poll and gives up itself after `expiresIn`.
7. API keys: `lrk_` + 32 random bytes (base64url). Stored as SHA-256 in
   `api_keys (id, name, key_hash UNIQUE, created_at, last_used_at, revoked_at)`.
   Sent as `Authorization: Bearer lrk_…`. Scope: rip endpoints only (below).
   The desk lists keys (name, created, last used) with **Disconnect**
   (revoke). Revoked keys get 401.
8. The app stores the key in the OS keyring (`keyring` crate; service
   `library-rip`, account = the library's address, other details in
   `connection.json` in the config folder), falling back to
   `connection-key.json` with 0600 permissions when there is no keyring, and
   offers **Disconnect** which also calls `DELETE /api/rip/token` (revokes
   its own key) and forgets the key even if the library cannot be reached.

## Rip API (Bearer API key)

| Method & path | Body → Response |
|---|---|
| `GET /api/rip/info` | (no auth) see above |
| `GET /api/rip/whoami` | → `{"keyId","keyName","library":{"name","url"}}` |
| `GET /api/rip/types` | → `[{"id","name","slug","lookup"}]` |
| `GET /api/rip/items?q=&type=music` | → `[{"id","title","creator","year","identifier","fileCount","url"}]` — to attach to an existing catalogue entry |
| `POST /api/rip/items` | `{"typeSlug":"music","title","creator","year","identifier","description","tags","musicbrainzReleaseId"}` → 201 `{"id","url"}` |
| `POST /api/rip/items/{id}/photos` | multipart `file` (front cover) → 201 |
| `POST /api/rip/items/{id}/media` | multipart, fields before `file`: `kind` (`audio`/`video`/`document`/`archive`), `title`, `disc`, `track`, `duration_ms`, `sha256` (server verifies), then `file` → 201 `{"id"}`. `track` and `duration_ms` are left out for files without them (archive files). Same limits and rules as the studio upload. |
| `GET /api/rip/items/{id}/media` | → `[{"id","kind","title","disc","track","sha256"}]` — files already on the item (`disc`, `track`, `sha256` may be `null`), so an interrupted upload resumes by skipping matching `sha256`s |
| `DELETE /api/rip/token` | revokes the calling key → 204 |

Errors use the site's JSON shape `{"error": "Plain English message"}`
(422 may add `"fields"`); the ripper shows `error` to the user, so it
should read well on its own. 401 means the key is revoked or unknown (the
ripper asks to connect again); 413 is "file too big". The ripper sends no
`Origin` header, so the same-origin check lets it through; every request
carries `User-Agent: LibraryRip/<version>`.

New media kind **`archive`**: files kept for the record, never shown or
streamed in the Player (rip log, cue sheet, AccurateRip/CTDB report, later
the MKV master). Listed under "Archive files" on the item page for the
librarian, downloadable. `sha256` is stored for every uploaded file.

## Audio CD rip (stage 1)

- **Reading:** libcdio-paranoia in full paranoia mode (overlap + verify,
  scratch repair) with drive cache defeat; every track is read and verified;
  sectors that never verify are reported with positions, never hidden.
  Additionally a test-and-copy mode reads each track twice and compares CRCs;
  sectors where the two passes differ are read again (up to 5 times) until
  the same data comes back twice, and any that never agree are reported as
  unrecovered. Samples the offset pushes before the start of the disc or
  past the lead-out are filled with silence (no overread).
- **Offset:** drive read offset from the AccurateRip drive database (matched
  by drive vendor/model) or set by the user; applied sample-exact.
- **Verification:** AccurateRip v1 and v2 CRCs (first/last 5 frames rule)
  against `http://www.accuraterip.com/accuraterip/…/dBAR-…bin`; CTDB lookup
  (`db.cuetools.net`) for an independent confidence count. Results per track:
  "Accurately ripped (confidence N)", "Not in database", or "Mismatch".
- **Other pressings:** a track that does not match at our own position is
  searched for another pressing of the album: its v1 and v2 CRCs at every
  shift within ±2,939 samples (5 × 588 − 1, as CUETools; `pressingSearch`
  in `settings.json` changes it, `0` turns it off), compared with every
  entry of the `dBAR` file; the CTDB disc CRC likewise (a rolling CRC-32).
  Neighbouring samples come from the saved per-track PCM files, streamed
  from disk (no whole-disc buffer); v1 is updated in constant time per
  shift, v2 through an exact filter plus a direct check of the few shifts
  it lets through (the maths is in `rip/src/core/pressing.rs`). A match
  with our own pressing always wins; otherwise the most confident. A track
  that matches only another pressing is **accurately ripped**: its audio is
  bit-for-bit what that pressing's rippers got, merely positioned
  differently on the disc, so it is an exact archival copy. It is shown as
  "Accurately ripped (confidence 8, pressing offset +102)" (`+k`: that
  pressing's audio sits `k` samples later on its disc), and the log
  explains it. `rip.json` (format version 2; version 1 files still parse)
  records per track the matching pressing's offset, confidence and v1/v2
  (`accuraterip.pressingOffset`, `confidence`, `version`), the CTDB
  pressing offset, and a disc `verdict`: `readErrors`, `accurate` (with
  `otherPressingOffsets`), `partlyAccurate`, `mismatch`, `notInDatabase`
  or `unchecked`.
- **Enhanced CDs** (CD-Extra / Blue Book: an audio session, then a data
  session): the last audio track ends where the audio session does, not at
  the data track. That end comes from the drive's full TOC (MMC READ TOC
  format 2: sessions and each session's lead-out) when it gives one, else
  from libcdio's last-session start, else (as libdiscid and whipper assume)
  11,400 sectors before the data track (lead-out 6,750 + lead-in 4,500 +
  pre-gap 150). The reader never asks for the gap or the data track, and
  progress counts audio only. Disc IDs follow each service: MusicBrainz
  leaves the data track out and uses the audio session's end as lead-out;
  freedb counts every track and the real lead-out; AccurateRip sums the
  audio tracks' offsets plus the real lead-out (the end of the data track),
  with the audio track count in the file name, and checksums audio tracks
  only; the CTDB TOC lists the data track as `-start` and ends with the real
  lead-out, its CRC covering the audio only. Known answers from a real disc
  (Ladyhawke, via whipper's tests) are in `rip/src/core/discid.rs`. The cue
  sheet has a `FILE` per audio track and notes each data track as `REM DATA
  TRACK nn SESSION s START … LENGTH …`; the log lists "Data track nn (not
  ripped)". **Mixed-mode CDs** (a data track before the audio) are refused
  with a plain message.
  Each AccurateRip, CTDB and drive-list request gives up after 30 seconds
  (reported as "it took too long"; the rip goes on), every connection
  fails after 2 minutes of silence, and **Cancel** works while waiting for
  any of them.
- **Output:** one FLAC per track (libFLAC, verify mode, compression level 8),
  tags from MusicBrainz (Vorbis comments incl. `MUSICBRAINZ_*`, ISRC, disc and
  track numbers), front cover embedded; a cue sheet (pre-gaps, HTOA, ISRC,
  CD-Text) and a human-readable rip log (drive, offset, mode, per-track
  CRCs/test CRCs, AccurateRip/CTDB results, peak levels, errors), plus a
  machine-readable `rip.json`. Optionally kept in a local folder too.
- **Identification:** MusicBrainz disc ID from the TOC → `/ws/2/discid/{id}`
  (exact releases); falls back to text search and to the server's existing
  item when attaching. User confirms; nothing is uploaded unconfirmed.
- **Upload:** create or pick the catalogue item, upload the cover, upload each
  FLAC as `audio` (it is both the archival master and browser-playable), then
  log, cue and `rip.json` as `archive`. Resumable per file (skips files
  whose `sha256` already exists on the item).

## Film disc rip (stage 2, as built)

- **Tools:** `makemkvcon` is found through `LIBRARY_RIP_MAKEMKVCON`, the
  `PATH` (`makemkvcon`, `makemkvcon64`), the Flathub app
  (`flatpak run --device=all --filesystem=<work folder>
  --command=makemkvcon com.makemkv.MakeMKV`), then `/usr/bin`,
  `/usr/local/bin`, `/opt/makemkv/bin`, the macOS app bundle and `Program
  Files\MakeMKV`. `ffmpeg` through `LIBRARY_RIP_FFMPEG` or the `PATH`, and
  must list `libopus` and `libsvtav1` (preferred) or `libaom-av1` among its
  encoders. Missing MakeMKV is an error with install help; missing or unfit
  ffmpeg only removes the viewing copy ("archive the master only").
- **Which disc:** the audio-CD path runs first. A disc with no audio tracks
  (`DriveError::NoAudio`) goes to MakeMKV: `makemkvcon -r --cache=1 info
  disc:9999` lists its drives (`DRV` lines); the one whose device matches
  the app's drive, else the only drive holding a disc, is `disc:N`; then
  `makemkvcon -r info disc:N` gives the disc and titles (`CINFO`, `TINFO`,
  `SINFO` lines; MakeMKV's own default minimum title length applies). Linux
  MakeMKV needs the `sg` kernel module to see drives at all.
- **Copy:** `makemkvcon -r --progress=-same mkv disc:N <title> <folder>` per
  ticked title (default: the longest), progress from `PRGV`, cancel kills
  the process group (Windows: `taskkill /T`). The MKV is renamed
  `<Film>.mkv` (`<Film> (title N).mkv` when several).
- **Viewing copy:** `ffmpeg … -map 0:v:0 -map 0:a? -sn -vf
  yadif=deint=interlaced,scale=trunc(iw*sar/2)*2:…,setsar=1,scale=…≤1920x1080
  -c:v libsvtav1 -crf 30 -preset 6 (libaom-av1: -crf 32 -b:v 0 -cpu-used 6)
  -c:a libopus -b:a 128k -ac 2` to `<Film>.webm`: only frames flagged
  interlaced are de-interlaced, non-square pixels become square, nothing is
  scaled up, soft subtitles are dropped (the master keeps them), progress
  from `-progress pipe:1`. No H.264/MP4 fallback.
- **Upload:** a new item (`typeSlug` from `GET /api/rip/types`: `film`, else
  the first type whose lookup is `film`; `title` and `year` as confirmed),
  then the WebM(s) as `video` (`title`, `duration_ms` from the disc, `sha256`),
  then the masters (`Master (lossless MKV): <name>`), the log
  (`<Film>.rip.log`) and `<Film>.rip.json` as `archive`. The same resumable
  upload as music: files whose `sha256` is on the item are skipped.
  `rip.json` for a film (`kind: "film"`, `formatVersion: 1`) holds the disc,
  each title with its streams, and each file's SHA-256.
- **Screens:** the Disc screen shows the titles (tick to copy), title, year
  and "Also make a viewing copy"; the Rip screen has a row per ticked title
  ("Copying 42%", then "Making viewing copy 17%"); Upload and Done are the
  music ones. Cancel stops MakeMKV or ffmpeg and returns to the Disc screen.
- **Not done:** Blu-ray/UHD specifics (untested; MakeMKV's errors are passed
  on), adding to an existing entry, cover art, MakeMKV's own progress detail
  beyond a percentage, 2160p copies, server renditions, subtitle tracks in
  the viewing copy.
- **Tests:** `tests/data/fake-makemkvcon` stands in for `makemkvcon`;
  `tests/e2e_film.rs` runs `library-rip headless-film` against the real
  server and plays the viewing copy back through the Player API. The sample
  MakeMKV output in `tests/data` was written from MakeMKV's documented
  robot-mode format, not captured from a real disc.

## Ripper UI (stage 1)

Screens: **Connect** (URL → browser handshake → "Connected to The Library as
Henry's desktop"), **Disc** (drive picker, disc detected, TOC summary,
MusicBrainz matches with the chosen match's front cover (per-match
thumbnails are a later refinement), editable album/track metadata,
"New catalogue entry" or "Add to existing…"), **Rip** (per-track progress,
read speed, secure-read status, AccurateRip/CTDB result per track; a rip
with unreadable sectors stops for **Rip again** or **Upload anyway**, never
uploading silently), **Upload** (per-file progress; **Try again** resumes on
the same item), **Done** (link to the item on the library, eject). **Settings**: drive offset, read mode (secure / test-and-copy),
keep local copy (folder), disconnect, About (AGPL notice + source link).
Plain English, woodworking-free, library tone. Works with keyboard only.

## Server additions (stage 1)

- Schema version 3 (`src/db/schema_v3.sql`, applied on start on top of
  version 2): `api_keys` table, `archive` kind and `sha256` column on
  `media_files` (the table is rebuilt with foreign keys off so subtitles,
  favourites and playlist entries survive; older files keep a `NULL`
  `sha256`).
- `api_keys` table, connect pages (`/connect`, `/connect/device`), token and
  device endpoints, Bearer extractor, rip API, `archive` kind + `sha256`
  column on `media_files`, desk section "Connected rippers" with revoke.
- `/ripper` page on the htmx site ("Get Library Rip"): detects the visitor's
  OS, lists builds found in `<data_dir>/downloads/library-rip/` (or
  `[ripper] downloads_dir`), each with size and SHA-256, plus the source
  link; shows how to install MakeMKV and ffmpeg for film discs. Linked from the
  Librarian's desk and the upload studio.
- Rate limiting on token/device endpoints; codes and device codes expire.

### As built (server)

- **Token and device errors** carry an OAuth-style code the ripper acts on
  plus plain English: `{"error": "invalid_grant", "message": "…"}`. Codes:
  `invalid_request` (400, a body that makes no sense), `invalid_grant` (400:
  wrong, used, expired or unknown code, a verifier that doesn't match the
  challenge, or a different `redirectUri`; a code is used up by any attempt,
  even a failed one), `authorization_pending` (428), `slow_down` (429,
  polled sooner than `interval`, with a second of leeway), `access_denied`
  (403), `expired_token` (410), `too_many_requests` (429). Every other rip
  API error has the Player API's shape (`{"error": "…"}`, plus `fields` on
  422); a missing or revoked key is 401 with `WWW-Authenticate: Bearer`.
  A successful token answer has `Cache-Control: no-store`.
- **Rate limits** (per client address): 20 new device codes per 10
  minutes (and at most 50 waiting at once on the whole server); after 30
  failed token attempts in 10 minutes, that address's failed attempts
  answer 429 instead of `invalid_grant` until the window passes. A good
  code or an approved device code always works, and polling with a known
  device code (`authorization_pending`, `slow_down`) never counts as a
  failure. The address is the connection's; `X-Forwarded-For` (its last
  entry) is believed only from a connection on this computer, i.e. the
  reverse proxy in front of a library listening on localhost.
- **One-time codes live in memory**: authorization codes (5 minutes) and
  device codes (10 minutes, `interval` 5 s) don't survive a restart; the
  ripper just connects again. User codes are four letters without vowels
  or look-alikes, a dash and four digits; the desk accepts them in any
  case, with or without the dash.
- `/connect` answers 400 with a plain page (never a redirect) for a bad
  `redirect_uri`, a missing or non-S256 challenge, an empty `state` or
  another `client`. Its Content-Security-Policy allows
  `form-action 'self' http://127.0.0.1:* http://localhost:*`, because
  browsers apply form-action to the redirect after **Yes**/**No**.
- `POST /api/rip/items`: `typeSlug` defaults to `music`; `year` may be a
  number or text; `tags` a list or one comma-separated string. Items have no
  MusicBrainz column, so `musicbrainzReleaseId` (a UUID, else 422) is kept
  in the entry's notes as `MusicBrainz release: https://musicbrainz.org/release/{id}`.
- `POST /api/rip/items/{id}/photos` → 201 `{"id"}` (a photo, caption
  "Cover"; 422 if it isn't a readable picture, at most 20 MB).
- `POST /api/rip/items/{id}/media`: `sha256` is **required** (64 hex
  digits, before `file`); the server hashes while writing and refuses a
  mismatch with 422 `fields.sha256`, deleting what it wrote. The `archive`
  kind takes any file type (stored as `text/plain` for .log/.cue/.txt,
  `application/json`, or by extension, else `application/octet-stream`).
  The studio's upload stores a SHA-256 too and checks one if sent.
- `GET /api/rip/items?q=&type=`: every word of `q` must be in the title or
  creator, or `q` equals the identifier (those first); at most 25.
- Archive files: hidden from every Player response (lists, items, media,
  streams, favourites, playlists, `/api/media/check`); on the item page the
  librarian sees "Archive files" with links to
  `GET /admin/items/{id}/media/{mid}/download` (any file of the item, as an
  attachment); the studio lists them with a Download button.
- Builds are served from `GET /ripper/download/{file}` (names of letters,
  digits, `.`, `-`, `_`, `+`; `.sha256`, `.txt`, `.md`, `.asc`, `.sig` and
  partial files are not offered). The visitor's system is guessed from the
  User-Agent; build files are sorted by name: `linux`/`.AppImage` → Linux,
  `.dmg`/`macos`/`darwin` → macOS, `windows`/`win64`/`.exe`/`.msi`/`.zip` →
  Windows. SHA-256s are cached by size and modification time.
- `/api/rip/*` is exempt from `site_password` (Library Rip's `Authorization`
  header can't carry both); `deploy/Caddyfile` leaves it out of basic auth
  for the same reason. Keys and codes protect it instead.
- The desk's "Connected rippers" posts to
  `POST /admin/rippers/{id}/disconnect`. `source_url` (config, default
  `https://github.com/OceanEilonwy/library`) is in every page's footer, in
  `/api/rip/info` and in the Player's `GET /api/info`.

## Simulated discs (tests, no drive needed)

A simulated disc is a folder read by `FakeDrive` (`rip/src/fakedisc.rs`):

- `disc.json` — `{"drive": {"vendor","model","revision"}, "readOffset": 6,
  "toc": {"tracks": [{"number","startLba","lengthSectors","isAudio","pregap","isrc","session"}], "leadOut"},
  "mcn": null, "cdText": null, "errors": [{"lba": 700, "kind": "unreadable" | "jitter" | "flaky_once"}]}`
  (`session` defaults to 1; within a session tracks are contiguous, a new
  session may start after a gap).
- `track01.raw`, `track02.raw`, … — 16-bit little-endian stereo 44.1 kHz
  PCM, exactly `lengthSectors × 2352` bytes each, as read with zero offset,
  for audio tracks only (data tracks have no file).
  `FakeDrive` shifts its output by `-readOffset` samples so that applying
  the `+readOffset` correction recovers the files exactly. Asking it for a
  sector in a data track or in the gap between sessions fails the read, as
  on a real drive, so tests prove the ripper never does.
- Injected errors, per sector: `unreadable` returns different garbage on
  every read (paranoia reports it after 20 tries); `jitter` lands reads a
  few samples early or late in a repeating cycle (paranoia corrects it);
  `flaky_once` returns garbage on the very first read only. With paranoia
  off, every error passes through unnoticed.

`library-rip make-fake-disc DIR --tracks 3 --seconds 20 [--offset 6]
[--error LBA:KIND …] [--enhanced]` writes one of sine tones (KIND
`unreadable`, `jitter` or `flaky_once`; `--enhanced` adds the 11,400-sector
session gap and a 1,500-sector data track in session 2);
`library-rip mock-services DIR [--listen ADDR] [--pressing OFFSET[:CONFIDENCE] …]` serves
stand-in MusicBrainz (album "Sine Tones" by "The Test Card", tracks named
after their tones), Cover Art Archive, AccurateRip (its drive list with the
simulated drive at its true offset, and a `dBAR` with the disc's v1 and v2
CRCs) and CTDB answers for it, worked out from the track files (by default
for the disc's own pressing; `--pressing 102` makes the databases know only
a pressing whose audio sits 102 samples later), and prints
the `LIBRARY_RIP_MUSICBRAINZ_URL`, `LIBRARY_RIP_COVER_ART_URL`,
`LIBRARY_RIP_ACCURATERIP_URL`, `LIBRARY_RIP_DRIVE_OFFSETS_URL` and
`LIBRARY_RIP_CTDB_URL` variables that point both the window and the
headless runner at it; `library-rip headless --library URL --fake-disc DIR
[--device-code] [--offset N] [--test-and-copy] [--keep DIR]
[--upload-with-errors] [--key-file FILE]` rips it and uploads without a
window (exit codes in `rip/README.md`; a rip with unreadable sectors is not
uploaded unless `--upload-with-errors` is given). `library-rip --fake-disc
DIR` opens the window with the simulated disc as its only drive. These
four are test tools behind the ripper's `testsupport` Cargo feature: `cargo
test` turns it on (the package has a dev-dependency on itself with the
feature), release builds never include it.

`rip/tests/e2e_headless.rs` runs stage 1 end to end against the real
server: it builds and starts it with a temporary data folder, rips a
simulated disc (one `jitter` and one `flaky_once` sector, test-and-copy)
with the real `library-rip headless --device-code` binary against
`mock-services`, approves the code over HTTP as the librarian, then checks
the entry, tracks, durations, archive files (hidden from the Player),
SHA-256s and cover through the rip and Player APIs; a second run connects
through `/connect` and the loopback listener and disconnects itself. A
second test rips an enhanced simulated disc (three audio tracks, a data
track in session 2) and checks the data track is neither read nor counted
(disc ids, cue, log, `rip.json`, the library's tracks), then rips a disc
the mock databases know only from a `+102` pressing and checks it is
reported accurately ripped with that pressing offset.
`player/e2e/ripper.spec.ts` drives `/connect` (unlock, **Yes**/**No**, the
redirect to a loopback listener, the token swap) and `/ripper` in Chromium.

Each track's report (`rip.json` `tracks[].retriedSectors`, and a
"Re-read" line in the log) records sectors that read wrong at first and
came right on re-reading; they are not errors, but a struggling disc is
never hidden.
