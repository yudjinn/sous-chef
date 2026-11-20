//! `sous-chef.toml` and command-line overrides. See `docs/architecture.md` ("Configuration").

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::handles::Retention;
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
    /// kas project files (`kas = ["kas/kiss.yaml", "kas/dogbonedark.yaml"]`), combined in
    /// this order as kas combines `a.yaml:b.yaml` on its command line. Relative to the
    /// file's directory. Empty: kas detection (see `workspace::kas`).
    pub kas: Vec<PathBuf>,
    /// `[build_host]`: what `os.uname()` gives on the machine that runs BitBake. Unset
    /// fields stay unknown (the values built from them are flagged).
    pub build_host: BuildHost,
    pub bitbake: BitbakeConfig,
    pub lint: LintConfig,
    pub output: OutputConfig,
    pub hooks: HooksConfig,
    pub handles: HandlesConfig,
    /// Directory that discovery starts from (not read from the file; set by the CLI,
    /// normally the process working directory).
    #[serde(skip)]
    pub work_dir: PathBuf,
    /// The directory of the configuration file read, if any (not read from the file).
    #[serde(skip)]
    pub config_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            build_dir: None,
            layers: Vec::new(),
            writable_layers: Vec::new(),
            init_script: None,
            release: None,
            kas: Vec::new(),
            build_host: BuildHost::default(),
            bitbake: BitbakeConfig::default(),
            lint: LintConfig::default(),
            output: OutputConfig::default(),
            hooks: HooksConfig::default(),
            handles: HandlesConfig::default(),
            work_dir: PathBuf::from("."),
            config_dir: None,
        }
    }
}

/// `[build_host]`: the two host values OE-core's `bitbake.conf` reads at parse time, as the
/// host that runs BitBake reports them (`BUILD_ARCH := "${@os.uname()[4]}"`,
/// `BUILD_OS := "${@os.uname()[0].lower()}"`, the same lines on Kirkstone, Scarthgap and
/// Wrynose). sous-chef never reads them from the host it runs on, which may not be the
/// build host: given here, the python subset evaluates `os.uname()[4]` to `arch` and
/// `os.uname()[0].lower()` to `os`; left out, those expressions stay unevaluated and every
/// value built from them is flagged. See docs/bitbake-semantics.md, "The host's
/// `os.uname()`".
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildHost {
    /// `os.uname()[4]`, what `uname -m` prints on the build host (`x86_64`, `aarch64`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// `os.uname()[0].lower()`, what `uname -s` prints on the build host, lowered (`linux`).
    /// Written with capitals (`Linux`), it is lowered on load; `os.uname()[0]` itself (not
    /// lowered) stays unknown, since its case is not given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

impl BuildHost {
    /// Neither value is given.
    pub fn is_empty(&self) -> bool {
        self.arch.is_none() && self.os.is_none()
    }

    /// `arch = "x86_64", os = "linux"`: the values given, for notes and `status`; `None`
    /// when none is.
    pub fn describe(&self) -> Option<String> {
        let parts: Vec<String> = [("arch", &self.arch), ("os", &self.os)]
            .into_iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} = {v:?}")))
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Every value that is not one word of the characters a kernel's `uname` fields use
    /// (ASCII letters, digits, `_`, `.`, `+`, `-`), one message each. `uname -m` output
    /// with a space (`Power Macintosh`) is refused, and so is anything python's expansion
    /// would treat as more text to expand (`$`, `{`).
    pub fn check(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (key, what, v) in [
            ("arch", "uname -m", &self.arch),
            ("os", "uname -s", &self.os),
        ] {
            let Some(v) = v else { continue };
            let ok = !v.is_empty()
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'));
            if !ok {
                problems.push(format!(
                    "build_host.{key} = {v:?} is not one word: give what `{what}` prints on the host that runs BitBake (letters, digits and _ . + - only, no spaces), or leave it out"
                ));
            }
        }
        problems
    }

    /// Lowers `os` (python's `str.lower`, which on ASCII text is ASCII lowering).
    pub fn normalise(&mut self) {
        if let Some(o) = self.os.as_mut() {
            o.make_ascii_lowercase();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BitbakeConfig {
    pub enabled: bool,
    /// Allows `cleansstate`, `cleanall`, `devtool reset`, `devtool finish`.
    pub allow_destructive: bool,
    /// Names of [`crate::bitbake::env::STRIPPED_ENV`] that BitBake commands inherit after
    /// all (`pass_env = ["TMPDIR"]`). Any other name is an error ([`BitbakeConfig::check`]).
    pub pass_env: Vec<String>,
}

impl Default for BitbakeConfig {
    fn default() -> BitbakeConfig {
        BitbakeConfig {
            enabled: true,
            allow_destructive: false,
            pass_env: Vec::new(),
        }
    }
}

impl BitbakeConfig {
    /// Every `pass_env` name that sous-chef does not remove (so listing it changes nothing,
    /// most likely a typo), one message each.
    pub fn check(&self) -> Vec<String> {
        use crate::bitbake::env::STRIPPED_ENV;
        self.pass_env
            .iter()
            .filter(|n| !STRIPPED_ENV.contains(&n.as_str()))
            .map(|n| {
                format!(
                    "bitbake.pass_env names {n:?}, which sous-chef never removes from BitBake's environment: it already passes; list only {}",
                    STRIPPED_ENV.join(", ")
                )
            })
            .collect()
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

/// `[hooks]`: switches for the Claude Code hooks (`sous-chef hook`). A hook switched off
/// exits successfully with no output. Each handler names its switch
/// (`hook::Handler::switch`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HooksConfig {
    /// The Grep / Read annotation, and the same annotation of a `grep -n` / `rg -n` run
    /// through Bash.
    pub grep_read: bool,
    /// The digest of large `bitbake` / `devtool` / `bitbake-layers` output from Bash.
    pub bash: bool,
    /// The reindex and lint after Edit / Write / MultiEdit.
    pub edit: bool,
}

impl Default for HooksConfig {
    fn default() -> HooksConfig {
        HooksConfig {
            grep_read: true,
            bash: true,
            edit: true,
        }
    }
}

impl HooksConfig {
    /// Whether the switch `key` (`grep_read`, `bash` or `edit`) is on. A key that is not a
    /// switch is on: only a key the file can set can turn a hook off.
    pub fn enabled(&self, key: &str) -> bool {
        match key {
            "grep_read" => self.grep_read,
            "bash" => self.bash,
            "edit" => self.edit,
            _ => true,
        }
    }
}

/// `[handles]`: retention limits of the handle store (`handles::Retention`), applied when the
/// server opens the store. Defaults: [`Retention::default`] (500 MB, 14 days).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HandlesConfig {
    /// After the age pass, the oldest handles are deleted until the stored output totals at
    /// most this many megabytes (1 MB = 1,000,000 bytes).
    pub max_total_mb: u64,
    /// Handles last stored longer ago than this many days are deleted.
    pub max_age_days: u64,
}

/// Bounds of `[handles] max_total_mb`: 1 MB to 1,000,000 MB (1 TB).
pub const HANDLES_MAX_TOTAL_MB: std::ops::RangeInclusive<u64> = 1..=1_000_000;
/// Bounds of `[handles] max_age_days`: 1 day to 3650 days (10 years).
pub const HANDLES_MAX_AGE_DAYS: std::ops::RangeInclusive<u64> = 1..=3650;

const SECS_PER_DAY: u64 = 24 * 60 * 60;
const BYTES_PER_MB: u64 = 1000 * 1000;

impl Default for HandlesConfig {
    fn default() -> HandlesConfig {
        let r = Retention::default();
        HandlesConfig {
            max_total_mb: r.max_total_bytes / BYTES_PER_MB,
            max_age_days: r.max_age_secs / SECS_PER_DAY,
        }
    }
}

impl HandlesConfig {
    /// The store's limits. Call [`HandlesConfig::check`] first; values out of range are
    /// clamped to it here.
    pub fn retention(&self) -> Retention {
        let mb = self
            .max_total_mb
            .clamp(*HANDLES_MAX_TOTAL_MB.start(), *HANDLES_MAX_TOTAL_MB.end());
        let days = self
            .max_age_days
            .clamp(*HANDLES_MAX_AGE_DAYS.start(), *HANDLES_MAX_AGE_DAYS.end());
        Retention {
            max_age_secs: days * SECS_PER_DAY,
            max_total_bytes: mb * BYTES_PER_MB,
        }
    }

    /// Every value out of its range, one message each.
    pub fn check(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !HANDLES_MAX_TOTAL_MB.contains(&self.max_total_mb) {
            problems.push(format!(
                "handles.max_total_mb = {} is out of range: give {} to {} (megabytes of stored output kept)",
                self.max_total_mb,
                HANDLES_MAX_TOTAL_MB.start(),
                HANDLES_MAX_TOTAL_MB.end()
            ));
        }
        if !HANDLES_MAX_AGE_DAYS.contains(&self.max_age_days) {
            problems.push(format!(
                "handles.max_age_days = {} is out of range: give {} to {} (days a handle is kept)",
                self.max_age_days,
                HANDLES_MAX_AGE_DAYS.start(),
                HANDLES_MAX_AGE_DAYS.end()
            ));
        }
        problems
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
    /// directory too (`load` replaces it with the real working directory), and so is
    /// `config_dir`.
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
        let mut problems = cfg.handles.check();
        problems.extend(cfg.bitbake.check());
        problems.extend(cfg.build_host.check());
        cfg.build_host.normalise();
        if !problems.is_empty() {
            return Err(Error::Config {
                msg: format!("{}: {}", origin.display(), problems.join("; ")),
            });
        }
        let base = if origin.is_dir() {
            origin.to_path_buf()
        } else {
            origin.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        if let Some(b) = &cfg.build_dir {
            cfg.build_dir = Some(base.join(b));
        }
        cfg.layers = cfg.layers.iter().map(|l| base.join(l)).collect();
        cfg.kas = cfg.kas.iter().map(|k| base.join(k)).collect();
        if let Some(s) = &cfg.init_script {
            cfg.init_script = Some(base.join(s));
        }
        cfg.config_dir = Some(base.clone());
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
        for k in &self.kas {
            if !k.is_file() {
                problems.push(format!("kas file {} does not exist", k.display()));
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
