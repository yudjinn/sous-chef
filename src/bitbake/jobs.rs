//! Background bitbake jobs.
//!
//! Stub (M0): T5.4 implements the job table.

/// Table of background jobs, held in `mcp::Ctx`. Methods take `&self`.
#[derive(Debug, Default)]
pub struct JobTable {}

impl JobTable {
    pub fn new() -> JobTable {
        JobTable {}
    }
}
