//! Tool `overrides`: the override chain and override-conditional statements. See docs/mcp-tools.md.
//!
//! Stub (M0): T3.6 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// `OVERRIDES` entries, lowest to highest priority, with where each comes from.
    #[default]
    Chain,
    /// Every override-conditional statement in the recipe tree, active or inactive.
    Conditional,
    /// Statements whose override matches nothing known (likely typos).
    Dead,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe; omit for global configuration.
    pub recipe: Option<String>,
    /// Default: `MACHINE` from local.conf.
    pub machine: Option<String>,
    /// Default: `DISTRO` from local.conf.
    pub distro: Option<String>,
    /// `chain` (default), `conditional` or `dead`.
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
