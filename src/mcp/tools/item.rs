//! Tool `item`: expand a pointer or fetch a named item, byte-exact. See docs/mcp-tools.md.
//!
//! Stub (M0): T2.6 implements `run`.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::Ctx;
use crate::render::Format;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Pointer `layer:path:A-B`.
    pub loc: Option<String>,
    /// With `name`: search this recipe's tree.
    pub recipe: Option<String>,
    /// With `name`: search a single file (pointer-style `layer:path`).
    pub file: Option<String>,
    /// Item name, e.g. `do_install:append`.
    pub name: Option<String>,
    /// Extra lines around the item (default 0).
    #[serde(default)]
    pub context: u32,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> Result<String> {
    Err(Error::not_implemented())
}
