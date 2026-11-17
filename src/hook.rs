//! Claude Code hook entry point: reads the event JSON from stdin, dispatches to a handler,
//! writes the response. Hooks fail open.
//!
//! Stub (M0): T7.1 implements the dispatch, the handler trait and the registry.

pub mod bash;
pub mod edit;
pub mod grep_read;

use crate::error::{Error, Result};

/// Handles one hook event (the JSON text from stdin) and returns the response JSON to print
/// (`None` for no output). The caller treats an `Err` as "change nothing" and exits 0.
pub fn handle(_event_json: &str) -> Result<Option<String>> {
    Err(Error::not_implemented())
}
