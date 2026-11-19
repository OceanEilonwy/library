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
- **No accounts**: the site is public behind your reverse proxy's basic
  auth; editing needs the admin password, remembered for the browser session.

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

## Maintenance

```sh
library --config /etc/library/library.toml backup /path/to/library-2026-11-18.sqlite3
library --config /etc/library/library.toml reindex   # rebuild search indexes
```

Back up the data directory (`library.sqlite3`, `photos/`, `session.key`);
the backup command writes a consistent copy of the database even while the
server runs. `models/` can be re-downloaded.

## Development

```sh
cargo test                     # unit + HTTP tests (no network needed)
LIBRARY_TEST_MODEL_DIR=data/models/BAAI--bge-small-en-v1.5 cargo test --release real_model
cargo clippy --all-targets
```

Layout: `src/db` (schema, items, locations, loans, photos), `src/search`
(query parser, SQL compiler, hybrid ranking), `src/embed` (candle BERT and
the background indexer), `src/lookup.rs`, `src/dupes.rs`, `src/media.rs`
(photos, barcodes), `src/views` (maud), `src/web` (handlers, auth,
middleware), `static/` (CSS, htmx, small progressive-enhancement JS, fonts).

The design reference (the Broadsheet canvas) is in `design-canvas/`.

Fonts: Fraunces, Instrument Sans and IBM Plex Mono (SIL Open Font
License), self-hosted. htmx 4.0.2 (BSD Zero Clause) is vendored.
