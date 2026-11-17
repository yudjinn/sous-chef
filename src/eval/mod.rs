//! Static evaluator. Contract: `docs/architecture.md` ("Evaluator contract").
//!
//! [`evaluate`] replays the files in BitBake's order (`replay.rs`) into a [`DataStore`]
//! (`datastore.rs`), whose values are finalised by `overrides.rs` and expanded by
//! `expand.rs`.

pub mod datastore;
pub mod expand;
pub mod overrides;
pub mod providers;
pub mod replay;

pub use datastore::{DataStore, OpStatus, UnsetRecord, VarOp};
pub use replay::{
    AnonPython, ConfigCache, FunctionPiece, RefIssue, RefProblem, ReplayNotes, evaluate_with,
    global_cache,
};

use serde::Serialize;

use crate::error::Result;
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
///
/// `target.recipe` is a recipe name (`hello`; `target.version` picks a version, `%`
/// allowed, else the preferred one by layer priority and version), an absolute path to a
/// `.bb`, or `layer:path` (`meta-core:recipes-core/hello/hello_1.1.bb`). `None` evaluates
/// the configuration only. `machine` / `distro` override the configured values.
///
/// The index must be refreshed by the caller. The configuration pass is cached in a
/// process-wide [`ConfigCache`] (see [`evaluate_with`] for an explicit one). An unknown
/// recipe is `Error::NotFound`; problems in the metadata never fail, they are recorded in
/// [`DataStore::replay`].
pub fn evaluate(index: &Index, ws: &Workspace, target: &Target) -> Result<DataStore> {
    replay::evaluate_with(replay::global_cache(), index, ws, target)
}
