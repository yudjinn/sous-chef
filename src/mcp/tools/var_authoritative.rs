//! `var` with `authoritative: true`: answers from `bitbake -e`. Called by `var::run`.
//!
//! Stub (M0): T5.5 implements it. Not a tool of its own; it is not in the registry.

use crate::error::{Error, Result};
use crate::mcp::Ctx;

use super::var::Args;

/// Renders the history and final value of `args.name` from `bitbake -e`, in the same table
/// format as static mode.
pub async fn run(_ctx: &Ctx, _args: &Args) -> Result<String> {
    Err(Error::BitbakeUnavailable)
}
