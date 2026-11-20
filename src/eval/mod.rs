//! Static evaluator. Contract: `docs/architecture.md` ("Evaluator contract").
//!
//! [`evaluate`] replays the files in BitBake's order (`replay.rs`) into a [`DataStore`]
//! (`datastore.rs`), whose values are finalised by `overrides.rs` and expanded by
//! `expand.rs`.

mod anon;
mod anonmodel;
pub mod classfn;
pub mod datastore;
pub mod expand;
mod license;
pub mod multiconfig;
pub mod overlay;
pub mod overrides;
pub mod providers;
pub mod pyexpr;
pub mod replay;
pub mod skips;
pub mod variant;

pub use anon::NoWrite;
pub use datastore::{DataStore, OpKind, OpStatus, UnsetRecord, VarOp};
pub use overlay::Overlay;
pub use replay::{
    AnonPython, ConfigCache, FunctionPiece, RefIssue, RefProblem, ReplayNotes, UncertainRef,
    compatible_host, compatible_machine, evaluate_overlay_with, evaluate_with, global_cache,
};
pub use variant::ClassExtend;

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
    /// The multiconfig (`BBMULTICONFIG` name) to evaluate in; `None` or empty = the default
    /// configuration. A recipe written `mc:<name>:<recipe>` sets it too (see
    /// [`multiconfig`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mc: Option<String>,
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
/// allowed, else the one provider selection prefers, see `providers`), an absolute path to a
/// `.bb`, or `layer:path` (`meta-core:recipes-core/hello/hello_1.1.bb`). `None` evaluates
/// the configuration only. `machine` / `distro` override the configured values. `mc` (or a
/// recipe written `mc:<name>:<recipe>`) evaluates in that multiconfig: the configuration is
/// parsed again with `BB_CURRENT_MC` set, as BitBake does ([`multiconfig`]); an unknown name
/// is an error listing the known ones.
///
/// The index must be refreshed by the caller. The configuration pass is cached in a
/// process-wide [`ConfigCache`] (see [`evaluate_with`] for an explicit one). An unknown
/// recipe is `Error::NotFound`; problems in the metadata never fail, they are recorded in
/// [`DataStore::replay`].
pub fn evaluate(index: &Index, ws: &Workspace, target: &Target) -> Result<DataStore> {
    replay::evaluate_with(replay::global_cache(), index, ws, target)
}

/// [`evaluate`] with some files replaced by in-memory contents (planned edits, files not
/// created yet): see [`Overlay`] and [`evaluate_overlay_with`]. Writes nothing; the
/// process-wide configuration cache is read, never filled from an overlay evaluation.
pub fn evaluate_with_overlay(
    index: &Index,
    ws: &Workspace,
    target: &Target,
    overlay: &Overlay,
) -> Result<DataStore> {
    replay::evaluate_overlay_with(replay::global_cache(), index, ws, target, overlay)
}

/// `BBMASK` of an evaluated configuration, as `cooker.py` `collect_bbfiles` uses it.
#[derive(Debug, Clone)]
pub enum BbMask {
    /// Unset or empty: nothing is masked.
    None,
    /// A static value: the regular expressions joined with `|`, to be searched for
    /// (`re.search`, anywhere) in each collected file's path.
    Static(regex::Regex),
    /// Set, but sous-chef cannot apply it; the reason, for a note.
    Unusable(String),
}

/// `BBMASK` (BitBake 2.18.0 `lib/bb/cooker.py` `CookerCollectFiles.collect_bbfiles`): the
/// value is split on whitespace; a word starting with `|` loses it; each word is compiled on
/// its own and one that does not compile is ignored (logged as critical); the rest are joined
/// with `|` and `re.search`ed in the path of every file the `BBFILES` globs gave, before the
/// `.bb` / `.bbappend` split, so a masked bbappend is not applied and a masked recipe is no
/// recipe. Here a value with python or undefined variables, or a word Rust's `regex` cannot
/// compile (Python's `re` may: lookaround, backreferences), is [`BbMask::Unusable`]: whether
/// BitBake would ignore that word cannot be told.
pub fn bbmask(ds: &DataStore) -> BbMask {
    let Some(v) = ds.value("BBMASK") else {
        return BbMask::None;
    };
    if v.text.trim().is_empty() {
        return BbMask::None;
    }
    if v.unevaluated_python || !v.unresolved.is_empty() {
        return BbMask::Unusable(if v.unevaluated_python {
            "it holds python".to_string()
        } else {
            format!("it uses undefined variables ({})", v.unresolved.join(", "))
        });
    }
    let mut parts = Vec::new();
    for w in v.text.split_whitespace() {
        let w = w.strip_prefix('|').unwrap_or(w);
        if regex::Regex::new(w).is_err() {
            return BbMask::Unusable(format!(
                "sous-chef cannot compile its regular expression `{w}`"
            ));
        }
        parts.push(w);
    }
    // `"|".join(bbmasks)`, and `if bbmask and ...`: an empty join masks nothing.
    let joined = parts.join("|");
    if joined.is_empty() {
        return BbMask::None;
    }
    match regex::Regex::new(&joined) {
        Ok(r) => BbMask::Static(r),
        Err(_) => BbMask::Unusable("sous-chef cannot compile it".into()),
    }
}
