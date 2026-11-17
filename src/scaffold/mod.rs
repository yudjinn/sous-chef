//! Scaffold engine: plans, dry runs, confined writes. Generators live in their own files.
//!
//! Stub (M0): T6.1 implements rendering and applying plans and the dispatch on `kind`.

pub mod append;
pub mod class;
pub mod inc_split;
pub mod layer;
pub mod placement;
pub mod recipe;

use std::path::PathBuf;

use serde::Serialize;

/// What a generator returns. Nothing is written until the engine applies it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
    /// Things the user must supply or know (missing checksums, confinement notes...).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedFile {
    /// Absolute path of the file to create or modify.
    pub path: PathBuf,
    /// Full new content of the file.
    pub content: String,
    pub action: FileAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileAction {
    /// Never overwrites an existing file.
    Create,
    /// Applied only if the file's current sha256 (hex) equals `before_sha`.
    Modify { before_sha: String },
}
