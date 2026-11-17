//! The SQLite index. Contract: `docs/architecture.md` ("Index contract").
//!
//! Stub (M0): T2.1 implements `open` (schema in `schema.rs`), T2.2 `refresh` (in `scan.rs`).

pub mod query;
pub mod scan;
pub mod schema;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::types::Loc;
use crate::workspace::Workspace;

/// Handle on the index database. Not `Sync`: shared through a mutex in `mcp::Ctx`.
#[derive(Debug)]
pub struct Index {
    // Stub (M0): used once T2.1 lands.
    #[allow(dead_code)]
    conn: rusqlite::Connection,
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
    pub fn open(_ws: &Workspace) -> Result<Index> {
        Err(Error::not_implemented())
    }

    /// Brings the index up to date with the files on disk. Call at the start of every tool.
    pub fn refresh(&mut self, _ws: &Workspace) -> Result<RefreshReport> {
        Err(Error::not_implemented())
    }
}
