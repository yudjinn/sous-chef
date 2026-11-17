//! `sous-chef.toml` and command-line overrides. See `docs/architecture.md` ("Configuration").

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::lint::Severity;
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
    /// The Yocto release codename the workspace targets (`release = "scarthgap"`). Overrides
    /// detection; see `release::detect`.
    pub release: Option<String>,
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
            release: None,
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
    /// `[lint.severity]`: rule id to `info`, `warning` or `error`, replacing the rule's
    /// default severity.
    pub severity: BTreeMap<String, Severity>,
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

/// The file name looked up in the working directory.
pub const CONFIG_FILE: &str = "sous-chef.toml";

impl Config {
    /// Loads `sous-chef.toml` (from `cli.config`, else `cli.work_dir`) and merges the flags.
    ///
    /// - `--config` names a file that must exist. Without it, `<work_dir>/sous-chef.toml` is
    ///   used if present, else the defaults.
    /// - Paths in the file are relative to the file's directory; paths in flags are relative
    ///   to `cli.work_dir`. The result holds absolute paths.
    /// - Flags win: `--build-dir` replaces `build_dir`, and any `--layer` replaces the file's
    ///   `layers` list.
    pub fn load(cli: &CliOverrides) -> Result<Config> {
        let work_dir = if cli.work_dir.is_absolute() {
            cli.work_dir.clone()
        } else {
            std::env::current_dir()?.join(&cli.work_dir)
        };
        let file = match &cli.config {
            Some(p) => {
                let p = work_dir.join(p);
                if !p.is_file() {
                    return Err(Error::Config {
                        msg: format!("config file {} does not exist; fix --config", p.display()),
                    });
                }
                Some(p)
            }
            None => Some(work_dir.join(CONFIG_FILE)).filter(|p| p.is_file()),
        };
        let mut cfg = match file {
            Some(p) => {
                let text = std::fs::read_to_string(&p).map_err(|e| Error::Config {
                    msg: format!("cannot read {}: {e}", p.display()),
                })?;
                Config::from_toml(&text, &p)?
            }
            None => Config::default(),
        };
        if let Some(b) = &cli.build_dir {
            cfg.build_dir = Some(work_dir.join(b));
        }
        if !cli.layers.is_empty() {
            cfg.layers = cli.layers.iter().map(|l| work_dir.join(l)).collect();
        }
        cfg.work_dir = work_dir;
        Ok(cfg)
    }

    /// Parses the TOML text of a config file (no flag merge). `origin` is the file's path;
    /// relative paths in the file (`build_dir`, `layers`, `init_script`) are taken relative
    /// to its directory (to `origin` itself if it is a directory). `work_dir` is set to that
    /// directory too; `load` replaces it with the real working directory.
    pub fn from_toml(text: &str, origin: &Path) -> Result<Config> {
        let mut cfg: Config = toml::from_str(text).map_err(|e| Error::Config {
            msg: format!(
                "{}:{}: {}",
                origin.display(),
                e.span()
                    .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1)
                    .unwrap_or(1),
                e.message().lines().next().unwrap_or("invalid TOML")
            ),
        })?;
        let base = if origin.is_dir() {
            origin.to_path_buf()
        } else {
            origin.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        if let Some(b) = &cfg.build_dir {
            cfg.build_dir = Some(base.join(b));
        }
        cfg.layers = cfg.layers.iter().map(|l| base.join(l)).collect();
        if let Some(s) = &cfg.init_script {
            cfg.init_script = Some(base.join(s));
        }
        cfg.work_dir = base;
        Ok(cfg)
    }

    /// Checks the configuration against the discovered workspace (paths exist, writable
    /// layers name real layers). Every problem is reported in one line, separated by `; `.
    pub fn validate(&self, ws: &Workspace) -> Result<()> {
        let mut problems = Vec::new();
        if let Some(b) = &self.build_dir
            && !b.join("conf").join("bblayers.conf").is_file()
        {
            problems.push(format!(
                "build_dir {} has no conf/bblayers.conf",
                b.display()
            ));
        }
        for l in &self.layers {
            if !l.join("conf").join("layer.conf").is_file() {
                problems.push(format!("layer {} has no conf/layer.conf", l.display()));
            }
        }
        if let Some(s) = &self.init_script
            && !s.is_file()
        {
            problems.push(format!("init_script {} does not exist", s.display()));
        }
        for w in &self.writable_layers {
            if ws.layer(w).is_none() {
                let known: Vec<&str> = ws.layers.iter().map(|l| l.name.as_str()).collect();
                problems.push(format!(
                    "writable_layers names {w:?}, which is not a layer (layers: {})",
                    known.join(", ")
                ));
            }
        }
        if self.output.budget_tokens == 0 {
            problems.push("output.budget_tokens must be greater than 0".to_string());
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Config {
                msg: problems.join("; "),
            })
        }
    }
}
