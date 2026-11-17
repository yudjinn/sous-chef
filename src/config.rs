//! `sous-chef.toml` and command-line overrides. See `docs/architecture.md` ("Configuration").
//!
//! Stub (M0): the types are final in shape; loading, merging and validation are T1.5.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::workspace::Workspace;

/// Effective configuration: the file (if any) merged with command-line flags.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where `conf/bblayers.conf` lives.
    pub build_dir: Option<PathBuf>,
    /// Extra layer roots, used when there is no build directory.
    pub layers: Vec<PathBuf>,
    /// Layer names scaffold may write to.
    pub writable_layers: Vec<String>,
    /// Sourced before any bitbake command.
    pub init_script: Option<PathBuf>,
    pub bitbake: BitbakeConfig,
    pub lint: LintConfig,
    pub output: OutputConfig,
    /// Directory that discovery starts from (not read from the file; set by the CLI,
    /// normally the process working directory).
    #[serde(skip)]
    pub work_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            build_dir: None,
            layers: Vec::new(),
            writable_layers: Vec::new(),
            init_script: None,
            bitbake: BitbakeConfig::default(),
            lint: LintConfig::default(),
            output: OutputConfig::default(),
            work_dir: PathBuf::from("."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BitbakeConfig {
    pub enabled: bool,
    /// Allows `cleansstate`, `cleanall`, `devtool reset`, `devtool finish`.
    pub allow_destructive: bool,
}

impl Default for BitbakeConfig {
    fn default() -> BitbakeConfig {
        BitbakeConfig {
            enabled: true,
            allow_destructive: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LintConfig {
    /// Rule ids or group names to disable.
    pub disable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub budget_tokens: usize,
}

impl Default for OutputConfig {
    fn default() -> OutputConfig {
        OutputConfig { budget_tokens: 600 }
    }
}

/// Global command-line flags that override the file. Flags win.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    /// `--build-dir`
    pub build_dir: Option<PathBuf>,
    /// `--config`
    pub config: Option<PathBuf>,
    /// `--layer`, repeatable.
    pub layers: Vec<PathBuf>,
    /// The process working directory.
    pub work_dir: PathBuf,
}

impl Config {
    /// Loads `sous-chef.toml` (from `cli.config`, else `cli.work_dir`) and merges the flags.
    // Stub (M0): T1.5 implements it.
    pub fn load(_cli: &CliOverrides) -> Result<Config> {
        Err(Error::not_implemented())
    }

    /// Parses the TOML text of a config file (no flag merge).
    // Stub (M0): T1.5 implements it.
    pub fn from_toml(_text: &str, _origin: &Path) -> Result<Config> {
        Err(Error::not_implemented())
    }

    /// Checks the configuration against the discovered workspace (paths exist, writable
    /// layers name real layers).
    // Stub (M0): T1.5 implements it.
    pub fn validate(&self, _ws: &Workspace) -> Result<()> {
        Err(Error::not_implemented())
    }
}
