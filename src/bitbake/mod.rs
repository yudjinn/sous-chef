//! Optional BitBake backend: environment runner, background jobs, `bitbake -e`, logs.
//!
//! Stub (M0): T5.3 owns this file.

pub mod env;
pub mod envdump;
pub mod jobs;
pub mod logs;

pub use env::BbEnv;
pub use jobs::JobTable;
