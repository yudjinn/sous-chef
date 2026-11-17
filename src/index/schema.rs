//! DDL and migrations for the index (tables in `docs/architecture.md`).
//!
//! The index is derived data, so there are no migrations: any change to [`DDL`] bumps
//! [`SCHEMA_VERSION`], and [`open_db`] deletes and recreates a database whose `user_version`
//! differs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::error::Result;

/// Stored in `PRAGMA user_version`. Bump on every change to [`DDL`].
pub const SCHEMA_VERSION: i64 = 3;

/// `PRAGMA busy_timeout`.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Tables and indexes from the index contract in `docs/architecture.md`, plus:
///
/// - `REFERENCES` clauses (foreign keys are on): deleting a `layers` row deletes its files,
///   deleting a `files` row deletes its statements, edges, tasks, provides and depends, and
///   sets `edges.target_file` to NULL in edges that pointed at it.
/// - indexes on the foreign-key columns the contract does not index (so cascades are fast),
///   and on `files(pn)` and `provides(name)` for recipe lookup.
/// - triggers that keep the external-content FTS5 table `statements_fts` in sync with
///   `statements`, so writers never touch `statements_fts` themselves.
/// - the `handles` table used by the handle store (`src/handles.rs`, T5.1).
/// - `meta(key, value)`: facts about the whole index. `release` is the codename of the
///   release the edges were resolved for ([`META_RELEASE`]); `Index::refresh` recomputes the
///   edges when it differs from the workspace's (schema version 2).
/// - `files.not_collected` (schema version 3): for a `.bb` / `.bbappend` that BitBake does
///   not collect, why ([`NOT_COLLECTED_BBFILES`]: no layer's `BBFILES` glob matches it);
///   NULL for a collected one and for every other kind. Set with the edges. Such a file
///   stays indexed (`item`, `search` show it) but is never a recipe for a name, a provider
///   or an append. A `BBMASK` reason would be another value of the same column. The value
///   [`NOT_COLLECTED_DATA`] marks a `.conf` / `.inc` / `.bbclass` that is not BitBake
///   metadata (a data file); `meta.edges` ([`META_EDGES`]) records the version of the edge
///   step, so an index built before data files were marked gets its marks without a schema
///   change.
pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS layers(
    id          INTEGER PRIMARY KEY,
    name        TEXT UNIQUE,
    root        TEXT,
    priority    INT,
    collections TEXT,
    compat      TEXT,
    depends     TEXT,
    writable    INT
);

CREATE TABLE IF NOT EXISTS files(
    id       INTEGER PRIMARY KEY,
    layer_id INT REFERENCES layers(id) ON DELETE CASCADE,
    path     TEXT,
    kind     TEXT,
    mtime    INT,
    size     INT,
    sha256   TEXT,
    pn       TEXT,
    pv       TEXT,
    not_collected TEXT,
    UNIQUE(layer_id, path)
);
CREATE INDEX IF NOT EXISTS files_pn ON files(pn);

CREATE TABLE IF NOT EXISTS statements(
    id         INTEGER PRIMARY KEY,
    file_id    INT REFERENCES files(id) ON DELETE CASCADE,
    ord        INT,
    kind       TEXT,
    name       TEXT,
    base       TEXT,
    overrides  TEXT,
    flag       TEXT,
    op         TEXT,
    legacy     INT,
    value      TEXT,
    start_line INT,
    end_line   INT,
    snippet    TEXT
);
CREATE INDEX IF NOT EXISTS statements_base ON statements(base);
CREATE INDEX IF NOT EXISTS statements_file_ord ON statements(file_id, ord);
CREATE INDEX IF NOT EXISTS statements_kind_name ON statements(kind, name);

CREATE TABLE IF NOT EXISTS edges(
    id          INTEGER PRIMARY KEY,
    src_file    INT REFERENCES files(id) ON DELETE CASCADE,
    kind        TEXT,
    target_text TEXT,
    target_file INT NULL REFERENCES files(id) ON DELETE SET NULL,
    line        INT
);
CREATE INDEX IF NOT EXISTS edges_src_file ON edges(src_file);
CREATE INDEX IF NOT EXISTS edges_target_file ON edges(target_file);

CREATE TABLE IF NOT EXISTS tasks(
    file_id INT REFERENCES files(id) ON DELETE CASCADE,
    name    TEXT,
    before  TEXT,
    after   TEXT,
    line    INT
);
CREATE INDEX IF NOT EXISTS tasks_file_id ON tasks(file_id);

CREATE TABLE IF NOT EXISTS provides(
    file_id INT REFERENCES files(id) ON DELETE CASCADE,
    name    TEXT
);
CREATE INDEX IF NOT EXISTS provides_file_id ON provides(file_id);
CREATE INDEX IF NOT EXISTS provides_name ON provides(name);

CREATE TABLE IF NOT EXISTS depends(
    file_id INT REFERENCES files(id) ON DELETE CASCADE,
    kind    TEXT,
    pkg     TEXT NULL,
    target  TEXT,
    line    INT
);
CREATE INDEX IF NOT EXISTS depends_file_id ON depends(file_id);

CREATE VIRTUAL TABLE IF NOT EXISTS statements_fts USING fts5(
    name, snippet, content = 'statements', content_rowid = 'id'
);
CREATE TRIGGER IF NOT EXISTS statements_fts_ai AFTER INSERT ON statements BEGIN
    INSERT INTO statements_fts(rowid, name, snippet) VALUES (new.id, new.name, new.snippet);
END;
CREATE TRIGGER IF NOT EXISTS statements_fts_ad AFTER DELETE ON statements BEGIN
    INSERT INTO statements_fts(statements_fts, rowid, name, snippet)
        VALUES ('delete', old.id, old.name, old.snippet);
END;
CREATE TRIGGER IF NOT EXISTS statements_fts_au AFTER UPDATE ON statements BEGIN
    INSERT INTO statements_fts(statements_fts, rowid, name, snippet)
        VALUES ('delete', old.id, old.name, old.snippet);
    INSERT INTO statements_fts(rowid, name, snippet) VALUES (new.id, new.name, new.snippet);
END;

CREATE TABLE IF NOT EXISTS handles(
    id      TEXT PRIMARY KEY,   -- "h:" + 10 hex
    sha256  TEXT,               -- full hex digest; the blob is <cache dir>/handles/<2 hex>/<sha256>
    kind    TEXT,               -- log | env | build | value | text
    command TEXT,
    bytes   INT,
    created INT                 -- seconds since the Unix epoch
);
CREATE INDEX IF NOT EXISTS handles_created ON handles(created);

CREATE TABLE IF NOT EXISTS meta(
    key   TEXT PRIMARY KEY,
    value TEXT
);
"#;

/// `files.not_collected` of a `.bb` / `.bbappend` that no layer's `BBFILES` glob matches.
pub const NOT_COLLECTED_BBFILES: &str = "bbfiles";

/// `files.not_collected` of a `.bb` / `.bbappend` that only a `BBFILES_DYNAMIC` glob matches,
/// whose condition does not hold (the named collection is absent, or for `!<collection>`
/// present), so BitBake does not add the glob to `BBFILES`.
pub const NOT_COLLECTED_DYNAMIC: &str = "dynamic";

/// `files.not_collected` of a `.bb` / `.bbappend` a `BBFILES` glob collects but a `BBMASK`
/// regex masks (`cooker.py` `collect_bbfiles`).
pub const NOT_COLLECTED_BBMASK: &str = "bbmask";

/// The `files.not_collected` values of a `.bb` / `.bbappend` BitBake does not collect.
pub const NOT_COLLECTED_RECIPE_REASONS: &[&str] = &[
    NOT_COLLECTED_BBFILES,
    NOT_COLLECTED_DYNAMIC,
    NOT_COLLECTED_BBMASK,
];

/// `files.not_collected` of a `.conf`, `.inc` or `.bbclass` that is not BitBake metadata: a
/// data file (`wpa_supplicant.conf` in a recipe's `<pn>/` directory, `files/wic/*.wks.inc`),
/// see `scan.rs` ("Data files").
pub const NOT_COLLECTED_DATA: &str = "data";

/// The `meta` key holding the version of the edge computation ([`EDGES_VERSION`]); an index
/// with another value (or none) has its edges and file marks recomputed on the next refresh.
pub const META_EDGES: &str = "edges";

/// Bumped when what the edge step computes changes (it also sets `files.not_collected`).
/// `data-files-1`: data files are marked (w5-fix-eval2).
/// `dynamic-1`: `BBFILES_DYNAMIC` folded into the collection, `not_collected = 'dynamic'`
/// (w6).
pub const EDGES_VERSION: &str = "dynamic-1";

/// The `meta` key holding the codename of the release the edges were resolved for.
pub const META_RELEASE: &str = "release";

/// Every table [`DDL`] creates (the FTS table included).
pub const TABLES: &[&str] = &[
    "layers",
    "files",
    "statements",
    "edges",
    "tasks",
    "provides",
    "depends",
    "statements_fts",
    "handles",
    "meta",
];

/// Opens a connection with the pragmas every index connection uses: WAL journal,
/// `busy_timeout` 30 s, foreign keys on, and `BEGIN IMMEDIATE` as the default for
/// `Connection::transaction`. Does not check or create the schema; see [`open_db`].
pub fn connect(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let mut conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    // Returns the resulting mode; WAL persists in the file once set.
    let _mode: String = conn.pragma_update_and_check(None, "journal_mode", "wal", |r| r.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.set_transaction_behavior(TransactionBehavior::Immediate);
    Ok(conn)
}

/// Reads `PRAGMA user_version`.
pub fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Opens the index database at `path`, creating its parent directories, and makes sure it
/// has the current schema. A database with a different `user_version`, or a file that is
/// not a readable database, is deleted (with its `-wal` and `-shm` files) and recreated.
/// A fresh file (`user_version` 0) gets the schema.
pub fn open_db(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = match connect(path).and_then(|c| user_version(&c).map(|v| (c, v))) {
        Ok(pair) => Some(pair),
        // Not a database: it is derived, so rebuild it. Other errors (permissions, a busy
        // lock past the timeout) are reported, never answered by deleting the file.
        Err(e) if is_corrupt(&e) => None,
        Err(e) => return Err(e),
    };
    let mut conn = match existing {
        Some((conn, v)) if v == SCHEMA_VERSION => conn,
        // user_version 0: a fresh file, unless something else already put objects in it.
        Some((conn, 0)) if object_count(&conn)? == 0 => conn,
        Some((conn, _)) => {
            drop(conn);
            remove_db(path)?;
            connect(path)?
        }
        None => {
            remove_db(path)?;
            connect(path)?
        }
    };
    create_schema(&mut conn)?;
    Ok(conn)
}

/// Creates the schema in one `BEGIN IMMEDIATE` transaction unless it is already current.
/// Safe when two processes race on a fresh file: the second waits for the first and then
/// sees the current `user_version`.
pub fn create_schema(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if user_version(&tx)? != SCHEMA_VERSION {
        tx.execute_batch(DDL)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

/// Deletes the database file and its WAL side files. Missing files are fine.
pub fn remove_db(path: &Path) -> Result<()> {
    for p in [
        path.to_path_buf(),
        side_file(path, "-wal"),
        side_file(path, "-shm"),
    ] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn object_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?)
}

fn is_corrupt(e: &crate::error::Error) -> bool {
    matches!(
        e,
        crate::error::Error::Sqlite(rusqlite::Error::SqliteFailure(f, _))
            if matches!(
                f.code,
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
            )
    )
}

fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}
