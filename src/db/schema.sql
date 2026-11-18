-- Version 1 of the catalogue schema. Applied once, inside a transaction.

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- "Book", "Music", "Equipment · Camping"… `section` is the short label used
-- in the category navigation; `lookup` says which online sources fill it in.
CREATE TABLE media_types (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
    slug          TEXT NOT NULL UNIQUE,
    section       TEXT NOT NULL,
    lookup        TEXT NOT NULL DEFAULT 'none'
                  CHECK (lookup IN ('book', 'music', 'film', 'product', 'none')),
    creator_label TEXT NOT NULL DEFAULT 'Creator',
    position      INTEGER NOT NULL DEFAULT 0
) STRICT;

-- A tree of places. `path` is the materialised "Lounge › Bookcase 1 › Shelf 2",
-- kept up to date by the application whenever a node is renamed or moved.
CREATE TABLE locations (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES locations(id) ON DELETE RESTRICT,
    name      TEXT NOT NULL,
    path      TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX locations_sibling_name ON locations (coalesce(parent_id, 0), name COLLATE NOCASE);
CREATE INDEX locations_parent ON locations (parent_id);

CREATE TABLE items (
    id          INTEGER PRIMARY KEY,
    type_id     INTEGER NOT NULL REFERENCES media_types(id) ON DELETE RESTRICT,
    title       TEXT NOT NULL,
    creator     TEXT NOT NULL DEFAULT '',
    year        INTEGER,
    publisher   TEXT NOT NULL DEFAULT '',
    format      TEXT NOT NULL DEFAULT '',
    identifier  TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    tags        TEXT NOT NULL DEFAULT '',
    notes       TEXT NOT NULL DEFAULT '',
    location_id INTEGER REFERENCES locations(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    -- Hash of the text last embedded; NULL means the item needs (re)embedding.
    embed_hash  TEXT
) STRICT;
CREATE INDEX items_type ON items (type_id);
CREATE INDEX items_location ON items (location_id);
CREATE INDEX items_created ON items (created_at);
CREATE INDEX items_identifier ON items (identifier) WHERE identifier <> '';
CREATE INDEX items_needs_embedding ON items (id) WHERE embed_hash IS NULL;

-- User-defined fields. Either for every item of a type, or for one item.
CREATE TABLE field_defs (
    id       INTEGER PRIMARY KEY,
    type_id  INTEGER REFERENCES media_types(id) ON DELETE CASCADE,
    item_id  INTEGER REFERENCES items(id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    kind     TEXT NOT NULL CHECK (kind IN ('text', 'number', 'date', 'choice', 'bool')),
    options  TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    CHECK ((type_id IS NULL) <> (item_id IS NULL))
) STRICT;
CREATE INDEX field_defs_type ON field_defs (type_id);
CREATE INDEX field_defs_item ON field_defs (item_id);

CREATE TABLE item_fields (
    item_id  INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    field_id INTEGER NOT NULL REFERENCES field_defs(id) ON DELETE CASCADE,
    value    TEXT NOT NULL,
    PRIMARY KEY (item_id, field_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE photos (
    id         INTEGER PRIMARY KEY,
    item_id    INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    file       TEXT NOT NULL UNIQUE,
    width      INTEGER NOT NULL,
    height     INTEGER NOT NULL,
    position   INTEGER NOT NULL DEFAULT 0,
    caption    TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX photos_item ON photos (item_id, position);

CREATE TABLE loans (
    id          INTEGER PRIMARY KEY,
    item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    borrower    TEXT NOT NULL,
    loaned_at   INTEGER NOT NULL,
    returned_at INTEGER
) STRICT;
CREATE UNIQUE INDEX loans_one_active ON loans (item_id) WHERE returned_at IS NULL;
CREATE INDEX loans_item ON loans (item_id, loaned_at);

-- Full-text index; rowid is the item id. Maintained by the application in
-- the same transaction as every item write (custom field values go in `extra`).
CREATE VIRTUAL TABLE items_fts USING fts5 (
    title, creator, publisher, description, tags, notes, identifier, extra,
    tokenize = 'unicode61 remove_diacritics 2'
);

INSERT INTO media_types (name, slug, section, lookup, creator_label, position) VALUES
    ('Book', 'book', 'Books', 'book', 'Author', 10),
    ('Music', 'music', 'Music', 'music', 'Artist', 20),
    ('Film', 'film', 'Film', 'film', 'Director', 30),
    ('Equipment · Camping', 'camping', 'Camping', 'product', 'Brand', 40),
    ('Equipment · Tools', 'tools', 'Tools', 'product', 'Brand', 50),
    ('Game', 'game', 'Games', 'product', 'Publisher', 60);
