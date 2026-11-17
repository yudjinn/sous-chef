//! Tool `search`: search statement names, file content (regex) or full text. See docs/mcp-tools.md.
//!
//! Stub (M0): T2.7 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// Search mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Statement names, substring (index).
    #[default]
    Name,
    /// File content, line by line.
    Regex,
    /// Full-text search over names and snippets (index).
    Fts,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Search text or regular expression.
    pub query: String,
    /// `name` (default), `regex` or `fts`.
    #[serde(default)]
    pub mode: Mode,
    /// Statement kinds: `assign`, `function`, `task`, `inherit`, `include`, ...
    pub kind: Option<Vec<String>>,
    /// Only this layer.
    pub layer: Option<String>,
    /// Only this file kind: `bb`, `bbappend`, `inc`, `bbclass`, `conf`.
    pub file_kind: Option<String>,
    /// Only this recipe's tree.
    pub recipe: Option<String>,
    /// Maximum hits (default 30).
    pub limit: Option<usize>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
