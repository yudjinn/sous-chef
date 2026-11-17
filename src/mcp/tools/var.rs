//! Tool `var`: history and final value of a variable. See docs/mcp-tools.md.
//!
//! Stub (M0): T3.5 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Base name, e.g. `SRC_URI`; `FILES:${PN}` allowed.
    pub name: String,
    /// Recipe; omit for global configuration.
    pub recipe: Option<String>,
    /// Default: `MACHINE` from local.conf.
    pub machine: Option<String>,
    /// Default: `DISTRO` from local.conf.
    pub distro: Option<String>,
    /// Run `bitbake -e` instead of the static evaluator (default false).
    #[serde(default)]
    pub authoritative: bool,
    /// Also show `VAR[flag]` operations (default false).
    #[serde(default)]
    pub flags: bool,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
