//! Tool `deps`: build, runtime and PACKAGECONFIG dependencies, reverse deps, providers. See docs/mcp-tools.md.
//!
//! Stub (M0): T3.7 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

/// Which dependencies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `DEPENDS`.
    #[default]
    Build,
    /// `RDEPENDS` per package.
    Runtime,
    /// `PACKAGECONFIG` options and their effect.
    Packageconfig,
    /// Recipes depending on this one (static).
    Reverse,
    /// Providers of a name such as `virtual/kernel`, and the selected one.
    Providers,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe, or for `kind: providers` a provided name such as `virtual/kernel`.
    pub recipe: String,
    /// `build` (default), `runtime`, `packageconfig`, `reverse` or `providers`.
    #[serde(default)]
    pub kind: Kind,
    /// Depth (default 1).
    pub depth: Option<u32>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
