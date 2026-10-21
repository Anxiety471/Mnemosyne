-- Every file and folder is a node with a stable id. Its name and path come from its parent
-- chain, so rename, move and trash never rewrite other rows.
CREATE TABLE IF NOT EXISTS nodes (
    id            TEXT PRIMARY KEY,
    parent_id     TEXT REFERENCES nodes(id),
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
    owner         TEXT NOT NULL,
    head_revision INTEGER,
    trashed       INTEGER NOT NULL DEFAULT 0,
    starred       INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    modified_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(parent_id);
-- Two live nodes cannot share a name in one folder. Trashed nodes do not count.
CREATE UNIQUE INDEX IF NOT EXISTS nodes_live_name ON nodes(parent_id, name) WHERE trashed = 0;
INSERT OR IGNORE INTO nodes (id, parent_id, name, kind, owner, trashed, starred, created_at, modified_at)
    VALUES ('root', NULL, '', 'folder', 'system', 0, 0, '1970-01-01T00:00:00.000Z', '1970-01-01T00:00:00.000Z');

-- Immutable revisions of a file's content. Rows are only ever inserted.
CREATE TABLE IF NOT EXISTS revisions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id    TEXT NOT NULL REFERENCES nodes(id),
    revision   INTEGER NOT NULL,
    hash       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    mime       TEXT NOT NULL,
    author     TEXT NOT NULL,
    comment    TEXT NOT NULL,
    properties TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE (node_id, revision)
);

-- Role grants on a node. Grants apply to everything below the node. Roles: reader < writer < owner.
CREATE TABLE IF NOT EXISTS permissions (
    node_id   TEXT NOT NULL REFERENCES nodes(id),
    principal TEXT NOT NULL,
    role      TEXT NOT NULL CHECK (role IN ('reader', 'writer', 'owner')),
    PRIMARY KEY (node_id, principal)
);

-- Bearer-capability download links. Persist only the hash of the secret.
-- Deleting a node permanently cascades to its links.
CREATE TABLE IF NOT EXISTS download_links (
    id          TEXT PRIMARY KEY,
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    token_hash  TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS download_links_node ON download_links(node_id);

-- Resumable uploads; parts are stored under transfers/<id>.part.
CREATE TABLE IF NOT EXISTS transfers (
    id               TEXT PRIMARY KEY,
    path             TEXT NOT NULL,
    size             INTEGER NOT NULL,
    received         INTEGER NOT NULL DEFAULT 0,
    actor            TEXT NOT NULL,
    sha256           TEXT,
    if_head_revision INTEGER,
    expires_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS transfers_expiry ON transfers(expires_at);

-- Optional fixed revision for a public link; absence means follow the latest head.
CREATE TABLE IF NOT EXISTS download_link_revisions (
    link_id TEXT PRIMARY KEY REFERENCES download_links(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision > 0)
);

CREATE TABLE IF NOT EXISTS download_link_limits (
    link_id TEXT PRIMARY KEY REFERENCES download_links(id) ON DELETE CASCADE,
    max_downloads INTEGER NOT NULL CHECK (max_downloads > 0),
    downloads INTEGER NOT NULL DEFAULT 0 CHECK (downloads >= 0)
);

CREATE TABLE IF NOT EXISTS transfer_history (
    id TEXT PRIMARY KEY, actor TEXT NOT NULL, path TEXT NOT NULL,
    size INTEGER NOT NULL, received INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL, direction TEXT NOT NULL DEFAULT 'upload',
    error TEXT, updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS transfer_history_actor ON transfer_history(actor, updated_at);

CREATE TRIGGER IF NOT EXISTS transfers_track_insert AFTER INSERT ON transfers BEGIN
    INSERT OR IGNORE INTO transfer_history (id, actor, path, size, received, status, updated_at)
        VALUES (NEW.id, NEW.actor, NEW.path, NEW.size, NEW.received, 'active', strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
INSERT OR IGNORE INTO transfer_history (id, actor, path, size, received, status, updated_at)
    SELECT id, actor, path, size, received, 'active', strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM transfers;

-- Roots may only be registered through the local operator CLI.
CREATE TABLE IF NOT EXISTS host_mounts (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, root TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS activity (
    id INTEGER PRIMARY KEY AUTOINCREMENT, actor TEXT NOT NULL,
    method TEXT NOT NULL, route TEXT NOT NULL, status INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS upload_links (
    id TEXT PRIMARY KEY, node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE, expires_at TEXT NOT NULL,
    max_files INTEGER NOT NULL, max_bytes INTEGER NOT NULL,
    files_uploaded INTEGER NOT NULL DEFAULT 0, bytes_uploaded INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS deposit_chunks (
    id TEXT PRIMARY KEY, link_id TEXT NOT NULL REFERENCES upload_links(id) ON DELETE CASCADE,
    name TEXT NOT NULL, size INTEGER NOT NULL, sha256 TEXT NOT NULL,
    filename TEXT NOT NULL, received INTEGER NOT NULL DEFAULT 0,
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_accounts (
    id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL,
    quota_bytes INTEGER NOT NULL CHECK(quota_bytes>0), disabled INTEGER NOT NULL DEFAULT 0
);
