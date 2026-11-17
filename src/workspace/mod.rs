//! Workspace and layers. Contract: `docs/architecture.md` ("Workspace contract").
//!
//! Stub (M0): T1.3 implements discovery, `loc_for`, `abs_path` and `confine`.

pub mod layers;
pub mod resolve;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::types::{LineSpan, Loc};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Workspace {
    pub build_dir: Option<PathBuf>,
    /// In `BBLAYERS` order.
    pub layers: Vec<Layer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layer {
    /// Directory name, unique within the workspace.
    pub name: String,
    /// Absolute.
    pub root: PathBuf,
    /// `BBFILE_COLLECTIONS`
    pub collections: Vec<String>,
    /// `BBFILE_PRIORITY`
    pub priority: i32,
    /// `LAYERSERIES_COMPAT`
    pub compat: Vec<String>,
    /// `LAYERDEPENDS`
    pub depends: Vec<String>,
    /// `BBFILES` globs, `${LAYERDIR}` substituted.
    pub bbfiles: Vec<String>,
    /// From `sous-chef.toml` `writable_layers`.
    pub writable: bool,
}

impl Workspace {
    /// Finds the build directory and layers (see "Configuration" in architecture.md).
    pub fn discover(_cfg: &Config) -> Result<Workspace> {
        Err(Error::not_implemented())
    }

    /// The pointer for an absolute path inside a layer (or the build directory, as the
    /// pseudo-layer `build`).
    pub fn loc_for(&self, _abs: &Path, _lines: LineSpan) -> Option<Loc> {
        None
    }

    /// The absolute path a pointer refers to.
    pub fn abs_path(&self, _loc: &Loc) -> Option<PathBuf> {
        None
    }

    /// Confinement check: resolves symlinks and requires the result to be inside a layer
    /// root or the build directory. Every file access goes through this.
    pub fn confine(&self, path: &Path) -> Result<PathBuf> {
        Err(Error::Confined {
            path: path.to_path_buf(),
        })
    }

    /// Looks up a layer by name.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }
}
