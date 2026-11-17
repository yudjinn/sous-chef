//! Hook handler for Grep and Read results.
//!
//! Stub (T7.1): registered for `PostToolUse` on `Grep` and `Read`, returns no output.

use super::{Ctx, Event, Handler, POST_TOOL_USE, Response};
use crate::error::Result;

/// `PostToolUse` on `Grep` and `Read`.
pub struct GrepRead;

impl Handler for GrepRead {
    fn name(&self) -> &'static str {
        "grep_read"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Grep", "Read"]
    }

    fn handle(&self, _cx: &mut Ctx<'_>, _ev: &Event) -> Result<Option<Response>> {
        Ok(None)
    }
}
