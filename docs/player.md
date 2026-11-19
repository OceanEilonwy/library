# The Player — digital copies, accounts and the streaming PWA

This document is the contract between the server (Rust, `src/`) and the
Player PWA (Solid 2, `player/`). Both sides follow it; if it is wrong, fix it
here in the same change.

## Goals (from the brief)

1. Simple accounts: username, email, password. Every account is approved
   by a librarian before it can be used.
2. Digital media (PDFs, music, video…) needs an account to access.
3. An admin sets an account's access level: `child` or `adult`.
4. An admin marks an item `restricted`: only adults can access its media.
5. Anyone (with an account) can make playlists; all playlists are public.
   Entries the viewer cannot access are not shown and not playable; the rest
   work. A playlist left with no visible entries is not shown.
6. Streamed items can be downloaded to the device for offline playback.
7. A screen manages downloads / cached items and clears storage.
8. Downloading is easy: one tap for a track, an album/item, or a playlist.
9. Platform playback controls (Media Session API: lock screen, headset keys).
10. Music: previous / next / play-pause / scrubbable progress / back 10 s
    (and forward 10 s), in the spirit of YouTube Music.
11. A queue: "Play next" and "Add to queue", reorder, remove.
12. Favourites (per account, stored on the server).
13. Listen history stored on the device (IndexedDB), searchable.
14. The htmx site has a "Player" button that opens the PWA.
15. Installable (manifest, icons, service worker).
16. Seamless updates (never interrupt playback; no "reload?" prompt).
17. Proper tests: Rust integration tests; Vitest unit/component tests;
    a Playwright end-to-end smoke test.
18. Targets evergreen browsers on desktop, laptop and mobile.
19. A pleasant admin interface for uploading copies of CDs and DVDs.
20. Film playback has subtitle options.
21. No server transcoding: files are served as uploaded.

## Decisions

- **Admin** stays what it is today: whoever has the admin password on the
  htmx site (the "Librarian"). Admin features (accounts, restriction,
  uploads) live on the Librarian's desk, not in the PWA.
- **Registration is open** from the PWA, but **every account must be
  approved by the Librarian** before it can be used. Registering makes an
  account with `status = 'pending'` and no session; the Librarian approves
  it on `/admin/accounts` (choosing `child` or `adult` at the same time) or
  rejects it (which deletes it). Signing in with the right password to a
  pending account answers 403 (a wrong password is still 401, so pending
  status is never revealed to someone guessing). The `SignedIn` extractor
  also refuses sessions of accounts that are not approved (401), in case
  one ever exists.
- New accounts ask for `child` access (safe default), which is also the
  choice pre-selected when approving; the Librarian can promote them to
  `adult` later. Configurable with
  `[accounts] default_access = "child" | "adult"` in `library.toml`, and
  `[accounts] open_registration = true | false` (default true). The CLI has
  `library create-account <username> <email> [--adult]` (prompts for the
  password; the account is approved straight away, since it is the
  Librarian acting) and `library approve-account <username> [--adult]`.
- **Sessions**: on login the server creates a random 32-byte
  token, stores its SHA-256 in `account_sessions`, and sets cookie
  `library_account=<token>`; `HttpOnly; SameSite=Lax; Path=/; Max-Age=31536000`
  (plus `Secure` when the request came over HTTPS / `X-Forwarded-Proto: https`).
  Logout deletes the row. Deleting an account deletes its sessions.
- **Restriction** is on the item (`items.restricted`), so every file of a
  restricted item is restricted. A `child` account never receives a
  restricted item, media file, subtitle or playlist entry in any response,
  and gets **404** (not 403, to avoid revealing it) when requesting one
  directly. Unauthenticated requests to `/api/*` (other than the account
  endpoints below) get **401**.
- **Favourites** are per media file.
- **History** is device-only (IndexedDB in the PWA); the server stores none.
- **Playlists** hold media files (tracks, films, PDFs — any kind). Playlist
  visibility rule for a viewer:
  - entries the viewer cannot access are removed from the response;
  - a playlist with zero visible entries is omitted from lists and returns
    404 from `GET /api/playlists/{id}` — **except** for its owner, who always
    sees their own playlists (so a freshly created empty playlist is
    editable), still without the entries they cannot access;
  - when an owner rewrites entries (`PUT …/entries`) entries hidden from them
    are kept, in their original relative order, after the visible ones.
- **No transcoding.** Uploads are stored byte-for-byte. The only conversion
  is SubRip (`.srt`) subtitles to WebVTT text on upload (a text rewrite).

## Data (schema v2, migration in `src/db/mod.rs`; version 3 adds Library Rip's `archive` kind, `media_files.sha256` and `api_keys`, see docs/rip.md)

```sql
ALTER TABLE items ADD COLUMN restricted INTEGER NOT NULL DEFAULT 0 CHECK (restricted IN (0,1));

CREATE TABLE accounts (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    email         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,              -- argon2id PHC string
    access        TEXT NOT NULL DEFAULT 'child' CHECK (access IN ('child','adult')),
    -- A librarian approves every account before it can sign in.
    status        TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved')),
    created_at    INTEGER NOT NULL,
    approved_at   INTEGER                     -- NULL while pending
) STRICT;

CREATE TABLE account_sessions (
    token_hash  BLOB PRIMARY KEY,             -- sha256(token)
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE media_files (
    id            INTEGER PRIMARY KEY,
    item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    -- 'archive': kept for the record (Library Rip's logs, cue sheets…),
    -- never returned by the Player API (docs/rip.md).
    kind          TEXT NOT NULL CHECK (kind IN ('audio','video','document','archive')),
    title         TEXT NOT NULL,
    disc          INTEGER NOT NULL DEFAULT 1,
    track         INTEGER,                    -- NULL for films/documents
    file          TEXT NOT NULL UNIQUE,       -- stored name under <data_dir>/media
    original_name TEXT NOT NULL,
    mime          TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    duration_ms   INTEGER,                    -- measured in the browser at upload
    position      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    sha256        TEXT                        -- hex, worked out while uploading
) STRICT;
CREATE INDEX media_files_item ON media_files (item_id, disc, track, position);

CREATE TABLE subtitles (
    id            INTEGER PRIMARY KEY,
    media_file_id INTEGER NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    language      TEXT NOT NULL,              -- BCP 47, e.g. "en"
    label         TEXT NOT NULL,              -- "English", "English (SDH)"
    file          TEXT NOT NULL UNIQUE,       -- stored .vtt under <data_dir>/media
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE playlists (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE playlist_entries (
    playlist_id   INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position      INTEGER NOT NULL,
    media_file_id INTEGER NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    PRIMARY KEY (playlist_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE favourites (
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    media_file_id INTEGER NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, media_file_id)
) STRICT, WITHOUT ROWID;
```

Schema v2 also holds `api_keys` for Library Rip (see `docs/rip.md`).

Media files live in `<data_dir>/media/` with random names plus the original
extension. Deleting an item, media file or subtitle deletes its files.
(The migration also adds plain indexes on the foreign-key columns
`account_sessions.account_id`, `subtitles.media_file_id`,
`playlists.account_id`, `playlist_entries.media_file_id` and
`favourites.media_file_id`.)

## JSON API (`/api`, all JSON, camelCase keys)

Errors: `{"error": "Plain English message"}` with the HTTP status. Validation
errors (422) add `"fields": {"username": "Already taken"}`.

### Types

```ts
type Access = "child" | "adult";
type Account = { id: number; username: string; email: string; access: Access };
// The answer to registering (202). `message` is plain English for the screen.
type RegistrationPending = { status: "pending"; message: string };

// Files of the server's "archive" kind are never sent to the Player.
type MediaKind = "audio" | "video" | "document";
type Subtitle = { id: number; language: string; label: string; url: string }; // url = /api/subtitles/{id}
type MediaFile = {
  id: number; itemId: number; kind: MediaKind; title: string;
  disc: number; track: number | null; mime: string; sizeBytes: number;
  durationMs: number | null;
  streamUrl: string;            // /api/media/{id}/stream
  subtitles: Subtitle[];
  // denormalised for queue/history/media-session display:
  itemTitle: string; creator: string; year: number | null;
  cover: string | null;         // /photos/{file} of the item's first photo, or null
};
type ItemSummary = {
  id: number; title: string; creator: string; year: number | null;
  type: string;                 // media type name, e.g. "Music"
  typeSlug: string;             // e.g. "music"
  restricted: boolean;          // only ever true for adult viewers
  cover: string | null;
  kinds: MediaKind[];           // distinct kinds of its files
  fileCount: number; totalDurationMs: number | null;
};
type ItemDetail = ItemSummary & { description: string; media: MediaFile[] }; // ordered disc, track, position
type PlaylistSummary = {
  id: number; name: string; owner: string; mine: boolean;
  entryCount: number;           // visible entries only
  totalDurationMs: number | null; covers: string[]; // up to 4 distinct covers
  updatedAt: number;            // unix seconds
};
type Playlist = PlaylistSummary & { entries: MediaFile[] };
```

### Endpoints

| Method & path | Auth | Body → Response |
|---|---|---|
| `POST /api/account/register` | none | `{username,email,password}` → 202 `RegistrationPending`, **no cookie** (the account waits for the Librarian). 403 if registration closed (or too many accounts were made in the last hour). Rules: username 3–32 chars `[A-Za-z0-9_.-]`, email contains `@`, password ≥ 8 chars. |
| `POST /api/account/login` | none | `{login,password}` (username or email) → 200 `Account` + cookie; 401 wrong; 403 `{"error":"Your account is waiting for a librarian to approve it."}` when the password is right but the account isn't approved yet (no cookie); 429 rate-limited |
| `POST /api/account/logout` | any | → 204, clears cookie |
| `GET /api/account/me` | account | → `Account` (401 if none) |
| `GET /api/info` | none | → `{name, version, sourceUrl}`: the library's title, the server version and where its source code is (the AGPL's offer; show it on the about screen) |
| `GET /api/items?q=&kind=&type=&offset=&limit=` | account | → `{items: ItemSummary[], total: number}`; only items with ≥1 media file the viewer may access; `q` is a plain substring match on title/creator; newest first by default (the item whose latest file was uploaded most recently), `sort=title` alphabetical; `limit` defaults to 50, at most 200 |
| `GET /api/items/{id}` | account | → `ItemDetail` (404 if it has no files the viewer may access) |
| `GET /api/media/{id}` | account | → `MediaFile` |
| `POST /api/media/check` | account | `{ids:number[]}` → `{allowed:number[]}` (which still exist and are accessible; used to purge downloads) |
| `GET /api/media/{id}/stream` | account | bytes; supports `Range` (206, `Content-Range`, `Accept-Ranges: bytes`), `ETag`, `Last-Modified`, `If-None-Match`; `Cache-Control: private, max-age=0, must-revalidate`; `Content-Disposition: inline; filename="…"`. Never compressed. |
| `GET /api/subtitles/{id}` | account | `text/vtt; charset=utf-8` |
| `GET /api/playlists?mine=1` | account | → `PlaylistSummary[]` (visibility rule above), most recently updated first |
| `POST /api/playlists` | account | `{name}` → 201 `Playlist` |
| `GET /api/playlists/{id}` | account | → `Playlist` |
| `PATCH /api/playlists/{id}` | owner | `{name}` → `Playlist` |
| `DELETE /api/playlists/{id}` | owner | → 204 |
| `POST /api/playlists/{id}/entries` | owner | `{mediaIds:number[]}` appended → `Playlist` (ids the owner cannot access → 404) |
| `PUT /api/playlists/{id}/entries` | owner | `{mediaIds:number[]}` replaces visible entries (reorder/remove) → `Playlist` |
| `GET /api/favourites` | account | → `MediaFile[]`, newest first |
| `PUT /api/favourites/{mediaId}` | account | → 204 |
| `DELETE /api/favourites/{mediaId}` | account | → 204 |

Non-owners get 403 on owner endpoints for playlists they can see.
Archive files (`kind = 'archive'`, uploaded by Library Rip) are left out of
every response above, don't count towards `fileCount`, `kinds` or
`totalDurationMs`, and answer 404 when asked for directly; `kind=archive`
is a 400. An item with only archive files is not listed.
Any other `/api/…` path answers 404 with the JSON error shape. Login failures
are 401 `{"error": "That username or password isn't right."}`; after 10
wrong passwords for one login name (or 100 overall) in 10 minutes, 429.

JSON `POST/PUT/PATCH/DELETE` must pass the existing same-origin check
(`Origin`/`Sec-Fetch-Site`); the PWA is same-origin so this just works.

## Serving the PWA

- Built by Vite into `player/dist/` and served at **`/player/`** by the Rust
  server (`rust-embed` of `player/dist`; when the folder is missing at build
  time the route shows a page saying "The Player hasn't been built: run
  `npm --prefix player ci && npm --prefix player run build`").
- `/player/` and any unknown `/player/*` path without a file extension serve
  `index.html` (client routing). Hashed assets under `/player/assets/` get
  `Cache-Control: public, max-age=31536000, immutable`; `index.html`,
  `sw.js`, `manifest.webmanifest` and `registerSW.js` get `no-cache`.
- Service worker scope is `/player/`. The CSP for `/player/*` adds
  `worker-src 'self'; manifest-src 'self'; frame-src 'self'` (PDFs are shown
  in an iframe of the stream URL) and `img-src 'self' data: blob:`, and
  allows `style-src 'self' 'unsafe-inline'` (components set inline style
  attributes; scripts stay `script-src 'self'`, so no inline scripts).
  The "hasn't been built" page answers 503. Stream responses carry only
  `Content-Security-Policy: frame-ancestors 'self'` (an `object-src` rule
  would stop Chrome's built-in PDF viewer). The
  frame-ancestors/X-Frame-Options rule is relaxed to `'self'` /
  `SAMEORIGIN` for `/api/media/*/stream` so the PWA can frame PDFs.
- In development, `npm run dev` (Vite, port 5173) proxies `/api`, `/photos`
  and `/static` to `http://127.0.0.1:8080` (or `$LIBRARY_BACKEND`, e.g.
  `http://127.0.0.1:8090` for the dev config).

## htmx site changes

- A **Player** button in the top bar (front page) and compact header (other
  pages) linking to `/player/`.
- Item edit form: a **Restricted (adults only)** checkbox. Restricted items
  show a small "Adults only" badge on the item page when the admin is
  unlocked. (The public catalogue is unchanged otherwise — the restriction
  covers digital media only.)
- Item page, when unlocked: a **Digital copy** section listing media files
  with an "Add or manage files" link to `/admin/items/{id}/media`.
- `/admin/items/{id}/media`: the upload studio. Drag in a folder or files
  ripped from a CD/DVD (or pick them; folder picking via `webkitdirectory`).
  In the browser, before upload, it: detects kind from MIME/extension;
  parses disc/track numbers and a clean title from file names like
  `01 - Title.flac`, `1-03 Title.mp3`, `CD2/04. Title.m4a`; measures
  duration with a hidden media element; lets the librarian edit titles and
  numbers and reorder; attaches `.srt`/`.vtt` files to the matching video
  (by file name stem) with a language guess (`Film.en.srt` → `en`). Each file
  uploads with its own progress bar (XHR, one request per file, multipart
  streamed to disk server-side, no size limit on that route besides
  `[media] max_upload_mb`, default 20000). Existing files can be renamed,
  renumbered, reordered and deleted; subtitles added/removed per video.
- **Fill from MusicBrainz** (`static/js/musicbrainz-fill.js`, server in
  `src/web/fill.rs`, `src/musicbrainz.rs`, `src/trackmatch.rs`): a panel
  above the pending table, shown while music files wait to upload.
  - Search box prefilled with the item's creator and title; the item's
    `identifier` is searched as a barcode too when it is an EAN/UPC (not an
    ISBN). A pasted MusicBrainz release link or MBID fetches that release
    alone. No MusicBrainz release id is stored on items (the add form's
    lookup keeps none), so the barcode is the exact route.
  - Once every file's length is measured it searches by itself. The server
    runs the barcode and text searches, fetches the full track lists of the
    (at most 4) releases whose track counts are closest to the number of
    files, and ranks every candidate by its fit score (below); the rest are
    scored by track count only and rank lower. Each candidate shows title,
    artist, year, country, format ("2×CD"), tracks per disc, total length
    and a badge ("Fits your 12 files", "10 of your 12 files fit · 13 tracks").
  - Picking one shows a preview: each file → disc, track, title (and the
    track's artist where it differs from the release's), with a marker:
    **Good**, **Check** (with the reason) or **No match** (not applied).
    Tracks no file matched are listed. **Apply** fills the pending rows and
    re-sorts them by disc and track; a field the librarian typed in by hand
    is kept unless "Overwrite titles and numbers I've typed myself" is
    ticked. Options: put each track's artist in its title (compilations);
    set the item's year / creator when they are empty.
  - With `[lookup] enabled = false` the panel only explains that lookups
    are off.
  - **Matching** lives in Rust (`src/trackmatch.rs`), not the browser: the
    server already fetches and caches the candidate releases, ranking needs
    several full track lists that would otherwise all travel to the page,
    and the rules get unit tests beside the parser. Each file is scored
    against each track — measured length vs the track's length (weight 6;
    full marks within 2 s, falling to nothing at 15 s), disc/track numbers
    parsed from the name (weight 3; a straight-through number on a
    multi-disc release counts), normalised title similarity (weight 2;
    "Track 01"-style names carry no title) — and the Hungarian method picks
    the best one-to-one assignment. A file more than 10 s (or 2% of a long
    track) from its track is **No match**, never assigned silently; within
    3 s with an agreeing name, or a length no other track shares, is
    **Good**; anything else is **Check**. A release's fit score (0–100)
    starts at 100 and loses 12 per file/track count difference (up to 60),
    20 per unmatched file, 4 per file to check and 3 per second of mean
    length error (up to 40).
  - MusicBrainz etiquette: every request to it from the process (the add
    form's search too) passes one rate limiter (`[lookup]
    musicbrainz_interval_ms`, default 1000, never less than 1000 for
    musicbrainz.org itself), carries the `home-library/<version> ( contact )`
    user agent, and answers are cached in memory for an hour (releases by
    MBID, searches by query; 200 of each).
- Studio endpoints (librarian only, same-origin check applies):
  - `POST /admin/items/{id}/media/upload` — multipart, one file per request:
    text fields `kind`, `title`, `disc`, `track`, `duration_ms` **before**
    the `file` field. Streamed to disk; → 201 `{id, title, kind}`; 413 over
    the limit, 422 `{error, fields}` for a file type it won't take.
  - `POST /admin/items/{id}/media/{mid}/subtitles` — multipart `file`
    (`.srt`/`.vtt`), optional `language`, `label`; with
    `Accept: application/json` → 201 `{id}`, otherwise redirects back.
  - `POST /admin/items/{id}/media/musicbrainz/search` — JSON
    `{query, barcode?, files: FileInfo[]}` → `{candidates, notes}`;
    candidates (best fit first) carry `id, title, artist, year, date,
    country, barcode, format, discTracks, trackCount, totalMs, url` and
    `fit: {score, files, tracks, good, check, none, durationErrorMs,
    compared, summary}` (`null` when no files were sent). 403 when lookups
    are off, 400 for an empty query, 502 when MusicBrainz doesn't answer.
    `FileInfo = {key, name, disc?, track?, title, durationMs?}` (`key` is
    the studio's row id, echoed back).
  - `GET /admin/items/{id}/media/musicbrainz/release/{mbid}` — the release:
    `{id, title, artist, year, date, country, barcode, media: [{position,
    format, title, tracks: [{position, number, title, lengthMs, artist}]}]}`
    (`artist` only when it differs from the release's); 404 for an unknown
    id.
  - `POST /admin/items/{id}/media/musicbrainz/match` — `{release, files}` →
    `{release, matches: [{key, confidence: "good"|"check"|"none", disc,
    track, title, artist, lengthMs, note}], missing: [{disc, track, title,
    lengthMs}], fit}`.
  - `POST /admin/items/{id}/media/musicbrainz/item` — `{creator?, year?}`
    sets them only where the item's are empty → `{creator, year}`.
  - `GET /admin/items/{id}/media/list` — the list of existing files (HTML
    fragment); `POST …/media/{mid}` (title, disc, track),
    `POST …/media/{mid}/move` (`dir=up|down`), `POST …/media/{mid}/delete`,
    `POST /admin/items/{id}/subtitles/{sid}/delete` answer that fragment to
    htmx and redirect otherwise.
- `/admin/accounts`: a **Waiting for approval** section at the top when any
  account is pending (username, email, requested date), each with an access
  select (pre-set from `default_access`) and **Approve**, and **Reject**
  (confirm; deletes the account). Both are htmx posts answering the
  `#account-lists` fragment (and redirect without htmx):
  `POST /admin/accounts/{id}/approve` (`access=child|adult`; 404 if not
  pending) and `POST /admin/accounts/{id}/reject` (404 if not pending).
  Below, approved accounts (username, email, access, created): change access
  with a select (htmx), delete an account (confirm). Linked from the
  Librarian's desk, whose link shows a count badge while accounts are
  waiting.

## PWA behaviour

- Routes: Home (recently added, continue listening from history, your
  favourites, playlists), Library (search, filter by kind), Item, Playlists,
  Playlist, Favourites, History, Downloads, Video player, Document viewer,
  Account (login/register/logout), Settings/about with version.
- Registering never signs in: the app shows an **Account requested** screen
  ("A librarian needs to approve it before you can sign in") with the
  server's message and a "Back to sign in" button. Signing in to an account
  that is still waiting shows a "Not approved yet" notice with the server's
  403 message, distinct from the wrong-password error.
- One persistent `<audio>` element for music; mini-player bar above the
  bottom nav; expands to a full "Now playing" view with artwork, title,
  scrub bar (pointer + keyboard), back 10 s, previous, play/pause, next,
  forward 10 s, shuffle, repeat (off/all/one), and an "Up next" queue tab.
  "Previous" restarts the track if > 3 s in, otherwise goes back.
- Queue model is a pure TypeScript module (`src/player/queue.ts`) with unit
  tests: play list from index, play next, add to end, remove, move, shuffle
  (keeps current first), repeat modes, next/previous semantics.
- Video plays in its own route with native `<video>` + `<track kind=subtitles>`
  per subtitle, a subtitle picker (Off / each language), remembering the
  last chosen language; Media Session also set for video.
- Documents open in an iframe of the stream URL (browser PDF viewer) with a
  download button.
- Media Session: metadata (title, artist = creator, album = item title,
  artwork = cover), handlers play/pause/stop/previoustrack/nexttrack/
  seekbackward/seekforward/seekto, `setPositionState` kept up to date.
- Downloads: the service worker serves `/api/media/{id}/stream` from the
  `media-downloads` cache when present, answering `Range` requests by
  slicing the cached blob (206). Download = fetch full file with progress,
  `cache.put` under the stream URL, metadata (`MediaFile` JSON) saved in
  IndexedDB. Subtitles and covers of downloaded media are cached too.
  `navigator.storage.persist()` is requested on first download. Buttons:
  download track, download item (all files), download playlist. On start-up
  when online, `POST /api/media/check` purges downloads no longer allowed.
  Offline, the app shows downloaded media and plays from cache.
- Downloads screen: total usage (`navigator.storage.estimate()`), list of
  downloaded files grouped by item with sizes, delete one/item, "Clear all
  downloads", and "Clear other cached data" (app/image caches, keeps
  downloads), plus listen history clear.
- History: each play (after 10 s or completion) is recorded on the device
  with a `MediaFile` snapshot and timestamp; History screen groups by day,
  searchable, tap to play, clear.
- Updates: `vite-plugin-pwa` (injectManifest). A new service worker installs
  in the background; the app activates it (`skipWaiting`) and reloads only
  when nothing is playing and the user navigates between routes or the tab is
  hidden; otherwise it activates on next launch. No prompts.
- Install: manifest (`name` from server title is not required; use
  "Library Player"), `display: standalone`, `scope`/`start_url` `/player/`,
  theme colours, maskable PNG icons 192/512 and an Apple touch icon; an
  "Install app" button when `beforeinstallprompt` fires, iOS hint otherwise.
- Responsive: bottom tab bar on phones, side rail on wide screens; keyboard
  shortcuts on desktop (Space play/pause, ←/→ seek 10 s, Shift+N/P next/prev).
- Typography follows the site (Fraunces, Instrument Sans, IBM Plex Mono,
  served from `/static/fonts/fonts.css`).

### Implementation notes (as built)

- The service worker is registered from the app bundle
  (`virtual:pwa-register`, `injectRegister: false`), so the build has no
  `registerSW.js`; the server's `no-cache` rule for it is harmless.
- Caches: the precache (app shell), `media-downloads` (downloaded streams,
  one full response per stream URL), `media-extras` (subtitles and covers of
  downloads), `photos` (runtime, stale-while-revalidate, 400 entries) and
  `fonts` (runtime, cache-first). "Clear other cached data" deletes
  `photos` and `fonts` only, never the precache or downloads.
- The worker calls `clientsClaim()` on activation so a first visit can
  download straight away; a *new* version still waits for the app's
  `SKIP_WAITING` message, sent only when nothing is playing and no download
  is running.
- Offline metadata: besides downloads, the app keeps its last copy of the
  favourites list, the playlists list and each visited playlist in
  IndexedDB (never in the service worker) and shows those when offline.
- Films are recorded in history too (after 10 s of watching or at the end).
- Signing out keeps downloads on the device; the next sign-in runs
  `POST /api/media/check`, which removes anything the new account may not
  open.
- Service workers and the Cache API need a secure context: over plain
  `http://` to another device (not `localhost`), the Player works but
  cannot install or download; it says so when a download is attempted.
- `npm --prefix player run dev:mock` serves the Player with a pretend API
  (`player/scripts/mock-api.ts`) for trying the UI without the Rust server.

## Running locally

See README "The Player". Short version (from the repository root):

```sh
npm --prefix player ci && npm --prefix player run build
scripts/dev-seed.py                               # wipes and seeds ./data-dev
cargo run -- --config library.dev.toml serve      # http://127.0.0.1:8090
open http://127.0.0.1:8090/player/                # adult or kid / password123
                                                  # (newbie / password123 waits for approval)
```

`library.dev.toml` listens on 8090 rather than 8080 so it can run beside a
real library; the admin password is `librarian`.

## End-to-end tests

`npm --prefix player run test:e2e` (Playwright, Chromium, `player/e2e/`).
The global setup seeds a temporary data folder with `scripts/dev-seed.py`,
which uploads its sample files through `POST /admin/items/{id}/media/upload`,
starts the server on `E2E_PORT` (default 18431) and stops it afterwards. The
specs cover registration (the request, the waiting screen, the
librarian approving it on `/admin/accounts`, then signing in), turning a
request down, child/adult visibility of items and
playlists, playback (next, previous, ±10 s), the Up next order, history,
favourites, playlists (create, add, reorder, rename, delete, 403 for
others), downloading and playing offline, clearing downloads, film
subtitles, the PDF viewer, the manifest and service worker, the Player
button, the upload studio (including filling tracks from MusicBrainz), and
an absence of console errors. The global setup also starts a pretend
MusicBrainz (`e2e/mock-musicbrainz.ts`) in the test runner and points every
`[lookup]` source at it, so nothing leaves the machine. Lock-screen
and headset controls (Media Session) cannot be driven from a headless
browser; `src/player/mediaSession.test.ts` covers them.
