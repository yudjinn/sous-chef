//! Tool `bitbake`: run bitbake operations as background jobs. See docs/mcp-tools.md.
//!
//! Stub (M0): T5.4 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// Operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// `bitbake <target>`.
    Build,
    /// `bitbake -c <task> <target>`.
    Task,
    /// `bitbake -e [<target>]`, stored as a handle.
    Env,
    /// `bitbake -c clean|cleansstate|cleanall <target>`.
    Clean,
    /// `devtool modify|status|finish|reset|build`.
    Devtool,
    /// `bitbake-layers show-layers|show-appends|show-recipes|show-overlayed`.
    Layers,
    /// State of the current or last job.
    Status,
    /// Cancel the running job.
    Cancel,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `build`, `task`, `env`, `clean`, `devtool`, `layers`, `status` or `cancel`.
    pub op: Op,
    /// Recipe or image.
    pub target: Option<String>,
    /// For `op: task`, e.g. `compile`.
    pub task: Option<String>,
    /// Op-specific arguments, validated against an allowlist.
    #[serde(default)]
    pub args: Vec<String>,
    /// Must be true for destructive operations.
    #[serde(default)]
    pub confirm: bool,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
