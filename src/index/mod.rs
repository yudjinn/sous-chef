//! The SQLite index. Contract: `docs/architecture.md` ("Index contract").
//!
//! `open` and the schema (`schema.rs`) are T2.1; `refresh` (in `scan.rs`) is T2.2.
//!
//! Cache path: `<cache root>/sous-chef/<key>/index.sqlite`, where `<cache root>` is
//! `$XDG_CACHE_HOME` (if set to an absolute path) or else `$HOME/.cache`, and `<key>` is the
//! first 16 hex digits of the sha256 of the workspace roots (see [`workspace_key`]).
//!
//! Writes: use [`Index::write`], which runs the closure in a `BEGIN IMMEDIATE` transaction
//! so two server processes on one workspace serialise their writes instead of failing
//! halfway through.

pub mod query;
pub mod scan;
pub mod schema;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::types::Loc;
use crate::workspace::Workspace;

/// File name of the database inside the workspace cache directory.
pub const INDEX_FILE: &str = "index.sqlite";

/// Handle on the index database. Not `Sync`: shared through a mutex in `mcp::Ctx`.
#[derive(Debug)]
pub struct Index {
    conn: rusqlite::Connection,
    path: PathBuf,
}

/// What a refresh did. `reindexed` lists the files whose content changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RefreshReport {
    pub scanned: u32,
    pub reindexed: Vec<Loc>,
    pub removed: u32,
    pub millis: u64,
}

impl Index {
    /// Opens (creating if needed) the index for this workspace under
    /// `$XDG_CACHE_HOME/sous-chef/<16 hex>/index.sqlite`.
    pub fn open(ws: &Workspace) -> Result<Index> {
        Index::open_at(&cache_dir(ws)?.join(INDEX_FILE))
    }

    /// Like [`Index::open`], with an explicit cache root in place of `$XDG_CACHE_HOME`
    /// (the database is at `<cache_root>/sous-chef/<16 hex>/index.sqlite`).
    pub fn open_in(cache_root: &Path, ws: &Workspace) -> Result<Index> {
        Index::open_at(&cache_dir_in(cache_root, ws).join(INDEX_FILE))
    }

    /// Opens the database file at `path`: creates parent directories, applies the pragmas,
    /// and deletes and recreates the file if its `user_version` is not
    /// [`schema::SCHEMA_VERSION`].
    pub fn open_at(path: &Path) -> Result<Index> {
        let conn = schema::open_db(path)?;
        Ok(Index {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Brings the index up to date with the files on disk. Call at the start of every tool.
    pub fn refresh(&mut self, _ws: &Workspace) -> Result<RefreshReport> {
        // Stub: T2.2 implements this in scan.rs.
        Err(Error::not_implemented())
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The workspace cache directory holding the database (the handle store lives in
    /// `<cache dir>/handles`).
    pub fn cache_dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// The connection, for reads (`query.rs`). Writes go through [`Index::write`].
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.conn
    }

    /// Runs `f` in a `BEGIN IMMEDIATE` transaction and commits if it returns `Ok`; on `Err`
    /// the transaction is rolled back. The write lock is taken up front (waiting up to the
    /// 30 s busy timeout), so a second process never fails mid-transaction on an upgrade.
    pub fn write<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

/// The workspace key: first 16 hex digits of the sha256 of the workspace roots. The roots are
/// the build directory (if any) and every layer root, sorted and deduplicated, each written
/// as its path bytes followed by `\n`. Paths are used as given (`Layer.root` is absolute).
pub fn workspace_key(ws: &Workspace) -> String {
    let mut roots: Vec<&Path> = ws.layers.iter().map(|l| l.root.as_path()).collect();
    if let Some(b) = &ws.build_dir {
        roots.push(b.as_path());
    }
    roots.sort();
    roots.dedup();
    let mut h = Sha256::new();
    for r in roots {
        h.update(r.as_os_str().as_encoded_bytes());
        h.update(b"\n");
    }
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The cache root from the environment: `$XDG_CACHE_HOME`, else `$HOME/.cache`.
pub fn cache_root() -> Result<PathBuf> {
    cache_root_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}

/// [`cache_root`] with the two variables given explicitly. `XDG_CACHE_HOME` is ignored when
/// empty or relative, as the XDG base directory spec requires.
pub fn cache_root_from(
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(x) = xdg_cache_home.map(PathBuf::from)
        && x.is_absolute()
    {
        return Ok(x);
    }
    match home.map(PathBuf::from) {
        Some(h) if h.is_absolute() => Ok(h.join(".cache")),
        _ => Err(Error::Config {
            msg: "cannot locate a cache directory; set XDG_CACHE_HOME or HOME".into(),
        }),
    }
}

/// `<cache_root>/sous-chef/<workspace key>`.
pub fn cache_dir_in(cache_root: &Path, ws: &Workspace) -> PathBuf {
    cache_root.join("sous-chef").join(workspace_key(ws))
}

/// The workspace cache directory under [`cache_root`]. Not created here.
pub fn cache_dir(ws: &Workspace) -> Result<PathBuf> {
    Ok(cache_dir_in(&cache_root()?, ws))
}
