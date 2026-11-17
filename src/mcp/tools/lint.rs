//! Tool `lint`: lint a recipe, file or layer against Yocto conventions. See docs/mcp-tools.md.
//!
//! Stub (M0): T4.5 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::lint::Severity;
use crate::mcp::Ctx;
use crate::render::Format;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// A recipe, a file pointer or a layer name; default: writable layers.
    pub target: Option<String>,
    /// Rule ids or groups to run; default: all enabled rules.
    pub rules: Option<Vec<String>>,
    /// Minimum severity: `info`, `warning` or `error`.
    pub severity: Option<Severity>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
