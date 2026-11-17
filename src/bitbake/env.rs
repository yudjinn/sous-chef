//! Runs a command inside the initialised build environment.
//!
//! Stub (M0): T5.3 implements detection, argv safety, timeouts and cancellation.

use std::path::PathBuf;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::workspace::Workspace;

/// A usable BitBake environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BbEnv {
    pub build_dir: PathBuf,
    /// `None` when the process environment is already initialised.
    pub init_script: Option<PathBuf>,
}

impl BbEnv {
    /// Finds a build directory and init script, or an already-initialised environment.
    pub fn detect(_cfg: &Config, _ws: &Workspace) -> Option<BbEnv> {
        None
    }

    /// A command running an allowlisted `program` with `args` in the environment. Arguments
    /// are passed as argv, never interpolated into a shell script.
    pub fn command(&self, _program: &str, _args: &[&str]) -> Result<tokio::process::Command> {
        Err(Error::BitbakeUnavailable)
    }
}
