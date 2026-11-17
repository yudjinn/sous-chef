//! Tool `recipe`: resolve a recipe and show its shape. See docs/mcp-tools.md.
//!
//! Stub (M0): T2.6 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// Recipe file, versions, bbappends in application order, classes, key values.
    #[default]
    Summary,
    /// The include/inherit tree in parse order.
    Tree,
    /// Every task and function across the tree.
    Outline,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe: `pn`, `pn_pv` or a pointer.
    pub name: String,
    /// Default: the preferred version.
    pub version: Option<String>,
    /// `summary` (default), `tree` or `outline`.
    #[serde(default)]
    pub show: Show,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
