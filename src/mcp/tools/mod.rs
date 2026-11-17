//! One file per MCP tool, each with `pub struct Args` and
//! `pub async fn run(ctx: &Ctx, args: Args) -> Result<String>`.
//!
//! Tool tasks fill in their own file only. The registry table is `mcp::TOOLS`.

pub mod bitbake;
pub mod class;
pub mod deps;
pub mod item;
pub mod layers;
pub mod lint;
pub mod log;
pub mod overrides;
pub mod recipe;
pub mod scaffold;
pub mod search;
pub mod slice;
pub mod status;
pub mod var;
pub mod var_authoritative;
