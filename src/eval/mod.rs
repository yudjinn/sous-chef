//! Static evaluator. Contract: `docs/architecture.md` ("Evaluator contract").
//!
//! Stub (M0): T3.4 implements `evaluate` (replay in `replay.rs`).

pub mod datastore;
pub mod expand;
pub mod overrides;
pub mod providers;
pub mod replay;

pub use datastore::{DataStore, OpStatus, VarOp};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::index::Index;
use crate::types::{FileKind, Loc};
use crate::workspace::Workspace;

/// What an evaluation is for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize)]
pub struct Target {
    /// `None` = global configuration only.
    pub recipe: Option<String>,
    pub version: Option<String>,
    /// `None` = as set in `local.conf`.
    pub machine: Option<String>,
    pub distro: Option<String>,
}

/// A finalised value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Value {
    pub text: String,
    /// The value contains `${@...}` that was left unevaluated.
    pub unevaluated_python: bool,
    /// Variable references that could not be resolved.
    pub unresolved: Vec<String>,
}

/// One file visited during replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileVisit {
    /// The whole file.
    pub loc: Loc,
    pub kind: FileKind,
    /// The statements that pulled this file in, outermost first.
    pub via: Vec<Loc>,
}

/// Replays configuration (and the recipe, if any) and returns the datastore.
pub fn evaluate(_index: &Index, _ws: &Workspace, _target: &Target) -> Result<DataStore> {
    Err(Error::not_implemented())
}
