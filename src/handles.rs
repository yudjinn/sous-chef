//! Content-addressed store for large output: `h:<10 hex>` handles, digests, slices.
//!
//! Stub (M0): T5.1 implements storage, retention and the deny-list.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

/// `h:` followed by 10 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Handle(pub String);

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum HandleKind {
    Log,
    Env,
    Build,
    Value,
    Text,
}

/// Metadata kept for each handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandleMeta {
    pub handle: Handle,
    pub kind: HandleKind,
    pub command: String,
    pub bytes: u64,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

/// The handle store. Methods take `&self`; the store is shared through `mcp::Ctx`.
#[derive(Debug)]
pub struct HandleStore {
    // Stub (M0): used once T5.1 lands.
    #[allow(dead_code)]
    dir: PathBuf,
}

impl HandleStore {
    /// Opens the store rooted at `dir` (normally `<cache dir>/handles`).
    // Stub (M0): records the directory only; T5.1 creates it and applies retention.
    pub fn open(dir: &Path) -> Result<HandleStore> {
        Ok(HandleStore {
            dir: dir.to_path_buf(),
        })
    }

    /// Stores output and returns its handle; identical input gives the same handle.
    pub fn put(&self, _kind: HandleKind, _command: &str, _bytes: &[u8]) -> Result<Handle> {
        Err(Error::not_implemented())
    }

    pub fn get(&self, _handle: &Handle) -> Result<Vec<u8>> {
        Err(Error::not_implemented())
    }

    pub fn meta(&self, _handle: &Handle) -> Result<HandleMeta> {
        Err(Error::not_implemented())
    }
}
