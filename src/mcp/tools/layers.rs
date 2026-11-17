//! Tool `layers`: layers, shadowed recipes and classes, bbappends. See docs/mcp-tools.md.
//!
//! Stub (M0): T2.7 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// Name, priority, collections, compat, depends, writable, file counts.
    #[default]
    List,
    /// Recipes or classes provided by more than one layer, and which wins.
    Shadowed,
    /// bbappends and what they match, flagging those that match nothing.
    Appends,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `list` (default), `shadowed` or `appends`.
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
