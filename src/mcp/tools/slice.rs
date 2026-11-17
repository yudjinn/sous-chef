//! Tool `slice`: read part of a stored output handle. See docs/mcp-tools.md.
//!
//! Stub (M0): T5.2 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `h:<id>`.
    pub handle: String,
    /// Line range `A-B`.
    pub lines: Option<String>,
    /// Regular expression; matching lines with `context`.
    pub grep: Option<String>,
    /// Lines of context for `grep`.
    pub context: Option<u32>,
    /// Variable name (env dumps): its history block and final value.
    pub var: Option<String>,
    /// Error blocks (log and build handles).
    pub failures: Option<bool>,
    /// Page number, 1-based.
    pub page: Option<u32>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
