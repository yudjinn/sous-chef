//! Hook handler run after Edit / Write / MultiEdit on a Yocto file.
//!
//! Stub (T7.1): registered for `PostToolUse` on `Edit`, `Write` and `MultiEdit`, returns no
//! output. T7.4 fills in `handle` (`tool_input.file_path` is always absolute). Reindexing the
//! edited file needs a write path the hook does not have yet: `Ctx::env` opens the index
//! read-only (see the module docs of `hook`).

use super::{Ctx, Event, Handler, POST_TOOL_USE, Response};
use crate::error::Result;

/// `PostToolUse` on `Edit`, `Write` and `MultiEdit`.
pub struct EditFollowUp;

impl Handler for EditFollowUp {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Edit", "Write", "MultiEdit"]
    }

    fn handle(&self, _cx: &mut Ctx<'_>, _ev: &Event) -> Result<Option<Response>> {
        Ok(None)
    }
}
