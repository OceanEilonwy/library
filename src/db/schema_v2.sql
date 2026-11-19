-- Version 2: accounts, digital copies (media files and subtitles), playlists
-- and favourites, plus the "restricted" flag on items. Applied once, inside a
-- transaction, on top of version 1.

ALTER TABLE items ADD COLUMN restricted INTEGER NOT NULL DEFAULT 0 CHECK (restricted IN (0,1));

CREATE TABLE accounts (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    email         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,              -- argon2id PHC string
    access        TEXT NOT NULL DEFAULT 'child' CHECK (access IN ('child','adult')),
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE account_sessions (
    token_hash  BLOB PRIMARY KEY,             -- sha256(token)
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX account_sessions_account ON account_sessions (account_id);

CREATE TABLE media_files (
    id            INTEGER PRIMARY KEY,
    item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('audio','video','document')),
    title         TEXT NOT NULL,
    disc          INTEGER NOT NULL DEFAULT 1,
    track         INTEGER,                    -- NULL for films/documents
    file          TEXT NOT NULL UNIQUE,       -- stored name under <data_dir>/media
    original_name TEXT NOT NULL,
    mime          TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    duration_ms   INTEGER,                    -- measured in the browser at upload
    position      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
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
CREATE INDEX subtitles_media ON subtitles (media_file_id);

CREATE TABLE playlists (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX playlists_account ON playlists (account_id);

CREATE TABLE playlist_entries (
    playlist_id   INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position      INTEGER NOT NULL,
    media_file_id INTEGER NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    PRIMARY KEY (playlist_id, position)
) STRICT, WITHOUT ROWID;
CREATE INDEX playlist_entries_media ON playlist_entries (media_file_id);

CREATE TABLE favourites (
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    media_file_id INTEGER NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, media_file_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX favourites_media ON favourites (media_file_id);
