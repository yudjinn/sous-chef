//! Tool `scaffold`: create recipes, bbappends, layers, classes and `.inc` splits, or advise where a change belongs. See docs/mcp-tools.md.
//!
//! Stub (M0): T6.1 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// What to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A new recipe.
    Recipe,
    /// A bbappend (or a change to an existing one).
    Bbappend,
    /// A new layer.
    Layer,
    /// A new bbclass.
    Class,
    /// Move statements shared by several versions into `<pn>.inc`.
    IncSplit,
    /// Write nothing: recommend where a change belongs.
    Where,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `recipe`, `bbappend`, `layer`, `class`, `inc_split` or `where`.
    pub kind: Kind,
    /// Target layer (must be writable).
    pub layer: Option<String>,
    /// Name, as relevant to `kind`.
    pub name: Option<String>,
    /// Version, as relevant to `kind`.
    pub version: Option<String>,
    /// Kind-specific options (source URL, build system, variables, files...).
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
    /// Write the files (default false: dry run returning paths and content).
    #[serde(default)]
    pub write: bool,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
