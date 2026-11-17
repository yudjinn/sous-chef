//! Hook handler that digests large bitbake output from Bash.
//!
//! Stub (T7.1): registered for `PostToolUse` on `Bash`, returns no output. T7.3 fills in
//! `handle` (the failure detector is `bitbake::logs::detect`; `tool_response` is the Bash
//! output object `{stdout, stderr, interrupted, isImage}`).

use super::{Ctx, Event, Handler, POST_TOOL_USE, Response};
use crate::error::Result;

/// `PostToolUse` on `Bash`.
pub struct BashDigest;

impl Handler for BashDigest {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Bash"]
    }

    fn handle(&self, _cx: &mut Ctx<'_>, _ev: &Event) -> Result<Option<Response>> {
        Ok(None)
    }
}
