//! Tool `class`: a bbclass: its interface, users and outline. See docs/mcp-tools.md.
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
    /// Files (and which wins), inherits, tasks, `EXPORT_FUNCTIONS`, `?=` interface.
    #[default]
    Summary,
    /// Recipes and classes that inherit it, direct and transitive.
    Users,
    /// Functions and tasks.
    Outline,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Class name without `.bbclass`.
    pub name: String,
    /// `summary` (default), `users` or `outline`.
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
