-- Version 3: Library Rip (docs/rip.md). API keys for connected rippers, the
-- 'archive' media kind and a SHA-256 for every media file. Applied once, on
-- top of version 2, inside a transaction run with foreign keys switched off
-- (media_files is rebuilt to widen its CHECK, and dropping the old table
-- must not cascade to subtitles, playlist entries and favourites).

CREATE TABLE media_files_v3 (
    id            INTEGER PRIMARY KEY,
    item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    -- 'archive' files are kept for the record (rip logs, cue sheets, masters)
    -- and are never shown or streamed in the Player.
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
    -- Lower-case hex SHA-256 of the stored bytes, worked out while uploading
    -- (NULL for files uploaded before version 3).
    sha256        TEXT CHECK (sha256 IS NULL OR length(sha256) = 64)
) STRICT;

INSERT INTO media_files_v3 (id, item_id, kind, title, disc, track, file, original_name,
                            mime, size_bytes, duration_ms, position, created_at)
SELECT id, item_id, kind, title, disc, track, file, original_name,
       mime, size_bytes, duration_ms, position, created_at
FROM media_files;

DROP TABLE media_files;
ALTER TABLE media_files_v3 RENAME TO media_files;
CREATE INDEX media_files_item ON media_files (item_id, disc, track, position);

-- Library Rip and other trusted desktop apps. Only the SHA-256 of each key
-- is stored; a revoked key stays listed until deleted.
CREATE TABLE api_keys (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,              -- "Henry's desktop"
    key_hash      BLOB NOT NULL UNIQUE,       -- sha256(key)
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER,
    revoked_at    INTEGER
) STRICT;
