//! Tool `log`: digest of a build failure log. See docs/mcp-tools.md.
//!
//! Stub (M0): T5.6 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Default: the most recent failure.
    pub recipe: Option<String>,
    /// Task, e.g. `compile`.
    pub task: Option<String>,
    /// Job id from the `bitbake` tool.
    pub job: Option<String>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
