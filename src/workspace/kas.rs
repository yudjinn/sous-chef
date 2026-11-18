//! kas projects (T9.3). The kas behaviour reproduced here is recorded, with the kas version
//! and the pages it was checked against, in docs/bitbake-semantics.md ("kas").
//!
//! A kas project is a set of kas configuration files (YAML or JSON) that kas combines as
//! `a.yaml:b.yaml`. kas clones the repositories they name into its work directory, writes
//! `<build>/conf/bblayers.conf` and `<build>/conf/local.conf` from them, and runs BitBake.
//!
//! **Detection** ([`detect`]), in order; the first that applies wins and is recorded in
//! [`KasProject::source`], which `status` prints:
//!
//! 1. `kas = [...]` in `sous-chef.toml` ([`KasSource::Config`]).
//! 2. `.config.yaml` in the kas work directory ([`KasSource::MenuConfig`]): the file
//!    `kas menu` writes and kas reads when it is given no file.
//! 3. A build directory kas set up ([`is_kas_build_dir`]: its `CACHEDIR.TAG` ends with
//!    `# kas build directory`, or its `conf/local.conf` ends the way kas writes it,
//!    `MACHINE ??=`, `DISTRO ??=`, `BBMULTICONFIG ?=`), with exactly one kas file (a YAML or JSON file with a
//!    top-level `header`, not a lock file, not included by another candidate) in the build
//!    directory's parent or its `kas/` directory ([`KasSource::Found`]). With several
//!    candidates nothing is guessed: the project has no files, [`KasSource::Ambiguous`] lists
//!    them, and `status` says to set `kas` in `sous-chef.toml`.
//!
//! kas's own work directory is `KAS_WORK_DIR`, else the directory kas is run from (kas 5.5
//! `context.py`), which sous-chef cannot know; [`KasProject::work_dir_source`] records how
//! it was decided. It is `KAS_WORK_DIR`; else the parent of a build directory kas set up
//! (case 1: the configured build directory, `KAS_BUILD_DIR`, or `build/` next to
//! sous-chef.toml or in the working directory; case 3: the build directory found); else
//! (case 1) the directory of sous-chef.toml; else the sous-chef working directory. So with
//! `kas` and a kas build directory configured, the answer does not depend on where
//! sous-chef runs. The build directory is `build_dir` from the configuration, else
//! `KAS_BUILD_DIR`, else `<work dir>/build`. The two variables count only when they are
//! inside, or contain, the sous-chef working directory, the configured build directory or
//! a configured kas file, so a kas setting for another project does not turn an unrelated
//! workspace into a kas project.
//!
//! **Reading** follows kas's include handler: for each file, its lock file
//! (`<stem>.lock.<ext>` next to it) first, then its `header.includes` depth first, then the
//! file itself; the results are merged in that order (mappings recursively, insertion order
//! kept, anything else replaced). A string include is relative to the repository root of the
//! including file (falling back to the file's directory, as kas does); a `{repo, file}`
//! include is followed once that repository is on disk, and listed in
//! [`KasProject::problems`] while it is not. Every file read must resolve inside the project
//! root or the kas work directory (and a repository include inside that repository): an
//! include outside is reported, never read.
//!
//! Nothing here runs kas, git or any other program.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::config::Config;
use crate::error::{Error, Result};

/// The file `kas menu` writes in the work directory; kas reads it when given no file.
pub const MENU_CONFIG: &str = ".config.yaml";

/// kas's defaults (`schema-kas.json`, kas 5.5) for an unset `machine`, `distro`, `target`,
/// `task`.
pub const DEFAULT_MACHINE: &str = "qemux86-64";
pub const DEFAULT_DISTRO: &str = "nodistro";
pub const DEFAULT_TARGET: &str = "core-image-minimal";
pub const DEFAULT_TASK: &str = "build";

/// How deep includes may nest (kas itself has no limit and recurses on a cycle).
const MAX_INCLUDE_DEPTH: usize = 32;

/// Lines kas appends to `local.conf` after the `local_conf_header` entries (kas 5.5
/// `libcmds.py` `WriteBBConfig`), by prefix.
const LOCAL_CONF_TRAILER: &[&str] = &[
    "MACHINE ??= \"",
    "DISTRO ??= \"",
    "BBMULTICONFIG ?= \"",
    "ISAR_ROOTLESS ?= \"",
];

/// Lines kas writes in `bblayers.conf` after the `bblayers_conf_header` entries and the
/// `BBLAYERS ?=` assignment.
const BBLAYERS_TRAILER: &[&str] = &["BBPATH ?= \"${TOPDIR}\"", "BBFILES ??= \"\""];

/// How the kas files were found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KasSource {
    /// `kas = [...]` in `sous-chef.toml`.
    Config,
    /// `.config.yaml` in the kas work directory.
    MenuConfig,
    /// The only kas file next to a build directory kas generated.
    Found,
    /// A build directory kas generated, with several candidate files: none is used.
    Ambiguous { candidates: Vec<PathBuf> },
}

impl KasSource {
    /// One phrase for `status`.
    pub fn describe(&self) -> String {
        match self {
            KasSource::Config => "from kas in sous-chef.toml".into(),
            KasSource::MenuConfig => format!("{MENU_CONFIG} in the kas work directory"),
            KasSource::Found => "the only kas file next to the kas build directory".into(),
            KasSource::Ambiguous { candidates } => format!(
                "the build directory was generated by kas, but {} kas files could be it ({}); set kas = [...] in sous-chef.toml",
                candidates.len(),
                candidates
                    .iter()
                    .map(|c| c.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// A place in a kas file: absolute path and 1-based line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KasLoc {
    pub file: PathBuf,
    pub line: u32,
}

impl KasLoc {
    /// `file:line`, the file relative to `root` when it is inside it.
    pub fn display_in(&self, root: &Path) -> String {
        let f = self.file.strip_prefix(root).unwrap_or(&self.file);
        format!("{}:{}", f.display(), self.line)
    }
}

/// A scalar setting (`machine`, `distro`, `task`, one `target`) and where it was set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KasSetting {
    pub value: String,
    pub loc: KasLoc,
}

/// One entry of `repos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KasRepo {
    /// The key under `repos`.
    pub id: String,
    /// `name`, else the id (the checkout directory name and the sort key of its layers).
    pub name: String,
    pub url: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    /// Deprecated `refspec`.
    pub refspec: Option<String>,
    /// Where the repository is (or will be) on disk.
    pub path: PathBuf,
    /// The project's own repository: no `url` (kas performs no version control operation)
    /// and inside the project root.
    pub own: bool,
    pub loc: Option<KasLoc>,
}

/// One layer kas puts in `BBLAYERS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KasLayer {
    /// The repo id.
    pub repo: String,
    /// The key under `layers` (`""` for the repository itself).
    pub name: String,
    /// Absolute, lexically normalised.
    pub path: PathBuf,
    /// `prio` (higher first in `BBLAYERS`).
    pub prio: i64,
    /// `conf/layer.conf` exists (false before `kas checkout` for a downloaded layer).
    pub present: bool,
    /// In an own repository.
    pub own: bool,
}

/// One `local_conf_header` or `bblayers_conf_header` entry, as merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeaderEntry {
    pub id: String,
    pub text: String,
    /// The line of the first line of `text`.
    pub loc: KasLoc,
}

/// A kas project as sous-chef reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KasProject {
    /// The files on the kas command line, in order (absolute). Empty for
    /// [`KasSource::Ambiguous`].
    pub files: Vec<PathBuf>,
    pub source: KasSource,
    /// The project root: the repository holding the first file (the nearest directory with
    /// `.git` or `.hg`, a submodule's superproject), else that file's directory.
    pub root: PathBuf,
    /// kas's work directory (where it clones repositories and starts).
    pub work_dir: PathBuf,
    /// How [`KasProject::work_dir`] was decided.
    pub work_dir_source: WorkDirSource,
    /// kas's build directory.
    pub build_dir: PathBuf,
    /// Every file merged, in merge order (lock files and includes included).
    pub read: Vec<PathBuf>,
    /// The lock files among [`KasProject::read`].
    pub lock_files: Vec<PathBuf>,
    pub machine: Option<KasSetting>,
    pub distro: Option<KasSetting>,
    pub target: Vec<KasSetting>,
    pub task: Option<KasSetting>,
    pub repos: Vec<KasRepo>,
    /// In the order kas writes them into `BBLAYERS`: `prio` descending, then repo name,
    /// then layer name.
    pub layers: Vec<KasLayer>,
    /// Sorted by id, as kas writes them.
    pub local_conf_header: Vec<HeaderEntry>,
    pub bblayers_conf_header: Vec<HeaderEntry>,
    /// Anything that kept part of the configuration from being read (one line each).
    pub problems: Vec<String>,
    /// Filled by discovery when a build directory exists: kas layers that its
    /// `bblayers.conf` does not list (the kas files changed since the last checkout).
    pub not_in_bblayers: Vec<PathBuf>,
    /// Filled by discovery: workspace layers (names) that no kas file lists.
    pub not_in_kas: Vec<String>,
    /// `KAS_MACHINE`, `KAS_DISTRO`, `KAS_TARGET`, `KAS_TASK` from the environment
    /// sous-chef was started in, which kas lets override the files ([`KasOverrides`]).
    pub env: KasOverrides,
    /// The repositories a `{repo, file}` include names (ids, sorted), checked out or not.
    /// kas fetches and checks these out even under `kas shell --keep-config-unchanged`
    /// (its `repo_setup_loop` is not among the steps `-k` skips; docs/bitbake-semantics.md,
    /// "kas"), so sous-chef's kas mode is not used for such a project.
    pub repo_includes: Vec<String>,
}

/// How the kas work directory was decided ([`detect`]). kas's own rule (kas 5.5
/// `context.py`): `KAS_WORK_DIR`, else the directory kas is run from. sous-chef cannot know
/// the latter, so it takes it from what kas left on disk or from the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WorkDirSource {
    /// `KAS_WORK_DIR`.
    Env,
    /// The parent of a build directory kas set up (kas's default build directory is
    /// `<work dir>/build`).
    BuildDirParent,
    /// The directory holding `.config.yaml`, which kas reads from its work directory.
    MenuConfig,
    /// The directory of sous-chef.toml: a guess (no kas build directory was found).
    ConfigFile,
    /// sous-chef's working directory: a guess.
    WorkingDir,
    /// Passed to [`load`] by its caller.
    Given,
}

impl WorkDirSource {
    /// Whether the work directory is a guess, so the paths of the repositories kas clones
    /// into it may be wrong.
    pub fn is_guess(self) -> bool {
        matches!(self, WorkDirSource::ConfigFile | WorkDirSource::WorkingDir)
    }
}

/// The environment variables kas lets override the kas files' `machine`, `distro`,
/// `target` and `task` (kas 5.5 `config.py`: `get_machine` and `get_distro` return
/// `os.environ.get('KAS_MACHINE' / 'KAS_DISTRO', <file value or default>)`, so a variable
/// that is set wins even when empty; `get_bitbake_task` does the same with `KAS_TASK` after
/// a command-line `-c`; `get_bitbake_targets` takes the whitespace-separated words of
/// `KAS_TARGET` after a command-line `--target`, and only when there is at least one word).
/// `None`: not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KasOverrides {
    pub machine: Option<String>,
    pub distro: Option<String>,
    /// As set; [`KasOverrides::targets`] splits it.
    pub target: Option<String>,
    pub task: Option<String>,
}

impl KasOverrides {
    /// The words of `KAS_TARGET`, empty when unset or blank (then kas ignores it).
    pub fn targets(&self) -> Vec<&str> {
        self.target
            .as_deref()
            .map(|t| t.split_whitespace().collect())
            .unwrap_or_default()
    }
}

/// The process environment kas detection reads. Tests build one directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KasEnv {
    /// `KAS_WORK_DIR`
    pub work_dir: Option<PathBuf>,
    /// `KAS_BUILD_DIR`
    pub build_dir: Option<PathBuf>,
    /// `KAS_MACHINE`, `KAS_DISTRO`, `KAS_TARGET`, `KAS_TASK` (set, even empty, or not).
    pub overrides: KasOverrides,
}

impl KasEnv {
    /// `KAS_WORK_DIR` and `KAS_BUILD_DIR` of this process (empty values count as unset),
    /// and the `KAS_*` overrides (an empty value counts as set, as in kas; a value that is
    /// not UTF-8 is converted lossily).
    pub fn from_process() -> KasEnv {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let text = |k: &str| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned());
        KasEnv {
            work_dir: var("KAS_WORK_DIR"),
            build_dir: var("KAS_BUILD_DIR"),
            overrides: KasOverrides {
                machine: text("KAS_MACHINE"),
                distro: text("KAS_DISTRO"),
                target: text("KAS_TARGET"),
                task: text("KAS_TASK"),
            },
        }
    }
}

/// One kas setting an environment variable overrides ([`KasProject::env_overrides`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvOverride {
    /// `KAS_MACHINE`, `KAS_DISTRO`, `KAS_TARGET` or `KAS_TASK`.
    pub var: &'static str,
    /// The value kas uses (the variable's; targets joined by a space).
    pub value: String,
    /// What the kas files say (joined by a space for targets), `None` when they say
    /// nothing (kas's default would apply).
    pub file_value: Option<String>,
    /// Where the files say it (the first target's place).
    pub loc: Option<KasLoc>,
}

impl KasProject {
    /// The machine kas writes into `local.conf`: `KAS_MACHINE` when set, else `machine`,
    /// else kas's default.
    pub fn machine_or_default(&self) -> &str {
        self.env.machine.as_deref().unwrap_or_else(|| {
            self.machine
                .as_ref()
                .map_or(DEFAULT_MACHINE, |s| s.value.as_str())
        })
    }

    /// The distro kas writes into `local.conf`: `KAS_DISTRO` when set, else `distro`, else
    /// kas's default.
    pub fn distro_or_default(&self) -> &str {
        self.env.distro.as_deref().unwrap_or_else(|| {
            self.distro
                .as_ref()
                .map_or(DEFAULT_DISTRO, |s| s.value.as_str())
        })
    }

    /// The targets `kas build` builds: the words of `KAS_TARGET` when there is one, else the
    /// targets of the files, else kas's default (one).
    pub fn targets_or_default(&self) -> Vec<&str> {
        let env = self.env.targets();
        if !env.is_empty() {
            env
        } else if self.target.is_empty() {
            vec![DEFAULT_TARGET]
        } else {
            self.target.iter().map(|t| t.value.as_str()).collect()
        }
    }

    /// The task `kas build` runs: `KAS_TASK` when set, else `task`, else kas's default.
    pub fn task_or_default(&self) -> &str {
        self.env.task.as_deref().unwrap_or_else(|| {
            self.task
                .as_ref()
                .map_or(DEFAULT_TASK, |s| s.value.as_str())
        })
    }

    /// The settings an environment variable overrides, in the order `KAS_MACHINE`,
    /// `KAS_DISTRO`, `KAS_TARGET`, `KAS_TASK` ([`KasOverrides`]). Empty when none is set.
    pub fn env_overrides(&self) -> Vec<EnvOverride> {
        let one = |var, value: &Option<String>, file: &Option<KasSetting>| {
            value.as_ref().map(|v| EnvOverride {
                var,
                value: v.clone(),
                file_value: file.as_ref().map(|s| s.value.clone()),
                loc: file.as_ref().map(|s| s.loc.clone()),
            })
        };
        let targets = self.env.targets();
        let target = (!targets.is_empty()).then(|| EnvOverride {
            var: "KAS_TARGET",
            value: targets.join(" "),
            file_value: (!self.target.is_empty()).then(|| {
                self.target
                    .iter()
                    .map(|t| t.value.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            loc: self.target.first().map(|t| t.loc.clone()),
        });
        [
            one("KAS_MACHINE", &self.env.machine, &self.machine),
            one("KAS_DISTRO", &self.env.distro, &self.distro),
            target,
            one("KAS_TASK", &self.env.task, &self.task),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Where the value of `machine_or_default` (`machine` true) or `distro_or_default`
    /// comes from, in words: `the environment variable KAS_MACHINE`, `the kas file
    /// <file:line>` or `kas's default (no machine in the kas files)`.
    pub fn setting_source(&self, machine: bool) -> String {
        let (var, env, file, what) = if machine {
            ("KAS_MACHINE", &self.env.machine, &self.machine, "machine")
        } else {
            ("KAS_DISTRO", &self.env.distro, &self.distro, "distro")
        };
        match (env, file) {
            (Some(_), _) => format!("the environment variable {var}"),
            (None, Some(s)) => format!("the kas file {}", s.loc.display_in(&self.root)),
            (None, None) => format!("kas's default (no {what} in the kas files)"),
        }
    }

    /// The files as kas takes them on its command line: `a.yaml:b.yaml`, each relative to
    /// `base` when inside it.
    pub fn files_arg(&self, base: &Path) -> String {
        self.files
            .iter()
            .map(|f| f.strip_prefix(base).unwrap_or(f).display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    }

    /// The own (project) layer at `root`, if any.
    pub fn own_layer(&self, root: &Path) -> Option<&KasLayer> {
        self.layers
            .iter()
            .find(|l| l.own && same_path(&l.path, root))
    }

    /// The kas layer at `root`, if any.
    pub fn layer_at(&self, root: &Path) -> Option<&KasLayer> {
        self.layers.iter().find(|l| same_path(&l.path, root))
    }

    /// Whether this project generates `dir`'s configuration (it is the kas build directory).
    pub fn generates(&self, dir: &Path) -> bool {
        !self.files.is_empty() && same_path(&self.build_dir, dir)
    }

    /// The kas file a new setting for `local.conf` (`bblayers` false) or `bblayers.conf`
    /// should go in: the first file in merge order inside the project root that already has
    /// such a header, else the last file on the command line.
    pub fn file_for_header(&self, bblayers: bool) -> Option<&Path> {
        let headers = if bblayers {
            &self.bblayers_conf_header
        } else {
            &self.local_conf_header
        };
        self.read
            .iter()
            .find(|f| {
                f.starts_with(&self.root)
                    && !self.lock_files.contains(f)
                    && headers.iter().any(|h| &h.loc.file == *f)
            })
            .map(PathBuf::as_path)
            .or_else(|| self.files.last().map(PathBuf::as_path))
    }

    /// The text kas writes before its own lines: `# <id>\n<text>\n` per entry, in id order.
    pub fn header_text(&self, bblayers: bool) -> String {
        let headers = if bblayers {
            &self.bblayers_conf_header
        } else {
            &self.local_conf_header
        };
        let mut out = String::new();
        for h in headers {
            out.push_str(&format!("# {}\n{}\n", h.id, h.text));
        }
        out
    }

    /// The header entry a line of the generated file came from (exact line, ignoring
    /// trailing whitespace), if any.
    pub fn header_line_source(&self, bblayers: bool, line: &str) -> Option<&HeaderEntry> {
        let headers = if bblayers {
            &self.bblayers_conf_header
        } else {
            &self.local_conf_header
        };
        let line = line.trim_end();
        headers
            .iter()
            .find(|h| h.text.lines().any(|l| l.trim_end() == line))
    }
}

/// Whether a line is one kas writes itself after the headers (`MACHINE ??= "..."` and the
/// others in `local.conf`, `BBPATH ?=` / `BBFILES ??=` in `bblayers.conf`).
pub fn is_kas_trailer_line(bblayers: bool, line: &str) -> bool {
    let line = line.trim_end();
    if bblayers {
        BBLAYERS_TRAILER.contains(&line)
    } else {
        LOCAL_CONF_TRAILER
            .iter()
            .any(|p| line.starts_with(p) && line.ends_with('"'))
    }
}

/// Whether `local.conf` text ends the way kas writes it: `MACHINE ??= "..."`,
/// `DISTRO ??= "..."`, `BBMULTICONFIG ?= "..."` on consecutive lines, then at most
/// `ISAR_ROOTLESS ?= "1"`, then nothing but blank lines.
pub fn looks_generated_local_conf(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(|l| l.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut end = lines.len();
    if end > 0 && lines[end - 1].starts_with(LOCAL_CONF_TRAILER[3]) {
        end -= 1;
    }
    if end < 3 {
        return false;
    }
    (0..3).all(|i| {
        let l = lines[end - 3 + i];
        l.starts_with(LOCAL_CONF_TRAILER[i]) && l.ends_with('"')
    })
}

/// Whether `bblayers.conf` text is what kas writes: a `BBLAYERS ?= " \` line, later
/// followed by `BBPATH ?= "${TOPDIR}"` and `BBFILES ??= ""` as the last two lines.
pub fn looks_generated_bblayers(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    lines.len() >= 3
        && lines[lines.len() - 2] == BBLAYERS_TRAILER[0]
        && lines[lines.len() - 1] == BBLAYERS_TRAILER[1]
        && lines.contains(&"BBLAYERS ?= \" \\")
}

/// The last line of the `CACHEDIR.TAG` kas writes into its build directory (observed with
/// kas 5.5 on a yocto-kiss checkout).
pub const CACHEDIR_TAG_MARK: &str = "# kas build directory";

/// Whether `dir` is a build directory kas set up: its `CACHEDIR.TAG` ends with
/// [`CACHEDIR_TAG_MARK`], or its `conf/local.conf` ends the way kas writes it
/// ([`looks_generated_local_conf`]).
pub fn is_kas_build_dir(dir: &Path) -> bool {
    has_kas_cachedir_tag(dir)
        || std::fs::read_to_string(dir.join("conf").join("local.conf"))
            .is_ok_and(|t| looks_generated_local_conf(&t))
}

/// Whether `dir/CACHEDIR.TAG` ends with [`CACHEDIR_TAG_MARK`].
pub fn has_kas_cachedir_tag(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("CACHEDIR.TAG")).is_ok_and(|t| {
        t.lines().map(str::trim_end).rfind(|l| !l.is_empty()) == Some(CACHEDIR_TAG_MARK)
    })
}

// ---------------------------------------------------------------------------------------
// Detection

/// Finds the kas project of `cfg` (see the module documentation). `Ok(None)`: not a kas
/// project. An error only for kas files named in `sous-chef.toml` that cannot be read.
pub fn detect(cfg: &Config, env: &KasEnv) -> Result<Option<KasProject>> {
    let sc_dir = lexical_abs(&cfg.work_dir);
    // The variables count when they are related to what the configuration names, not only
    // to the directory sous-chef happens to run in.
    let anchors: Vec<PathBuf> = std::iter::once(sc_dir.clone())
        .chain(cfg.build_dir.iter().map(|b| lexical_abs(b)))
        .chain(cfg.kas.iter().map(|k| lexical_abs(k)))
        .collect();
    let related = |p: &Path| {
        let p = lexical_abs(p);
        anchors
            .iter()
            .any(|a| a.starts_with(&p) || p.starts_with(a))
    };
    let env_work = env.work_dir.as_deref().filter(|p| related(p));
    let env_build = env.build_dir.as_deref().filter(|p| related(p));

    let work_dir = |fallback: &Path| -> PathBuf { lexical_abs(env_work.unwrap_or(fallback)) };
    let build_dir = |work: &Path| -> PathBuf {
        cfg.build_dir
            .as_deref()
            .or(env_build)
            .map(lexical_abs)
            .unwrap_or_else(|| work.join("build"))
    };

    if !cfg.kas.is_empty() {
        for f in &cfg.kas {
            if !f.is_file() {
                return Err(Error::Config {
                    msg: format!(
                        "kas file {} does not exist; fix kas in sous-chef.toml",
                        f.display()
                    ),
                });
            }
        }
        // kas's work directory is where kas runs unless `KAS_WORK_DIR` says otherwise, and
        // its build directory `<work dir>/build` unless `KAS_BUILD_DIR` says otherwise
        // (kas 5.5 `context.py`). Where the user will run kas is unknown, so it is taken
        // from the configuration, never from sous-chef's working directory alone:
        // `KAS_WORK_DIR`, else the parent of a build directory kas set up (the configured
        // one, `KAS_BUILD_DIR`, or `build/` next to sous-chef.toml or in the working
        // directory), else the directory of sous-chef.toml, else the working directory.
        let config_dir = cfg.config_dir.as_deref().map(lexical_abs);
        let (work, from) = match env_work {
            Some(w) => (lexical_abs(w), WorkDirSource::Env),
            None => {
                let kas_build = [
                    cfg.build_dir.as_deref().map(lexical_abs),
                    env_build.map(lexical_abs),
                    config_dir.as_ref().map(|c| c.join("build")),
                    Some(sc_dir.join("build")),
                ]
                .into_iter()
                .flatten()
                .find(|b| is_kas_build_dir(b));
                match (kas_build.as_deref().and_then(Path::parent), config_dir) {
                    (Some(p), _) => (p.to_path_buf(), WorkDirSource::BuildDirParent),
                    (None, Some(c)) => (c, WorkDirSource::ConfigFile),
                    (None, None) => (sc_dir.clone(), WorkDirSource::WorkingDir),
                }
            }
        };
        let build = build_dir(&work);
        return Ok(Some(with_env(
            with_work_dir_source(load(&cfg.kas, KasSource::Config, &work, &build), from),
            env,
        )));
    }

    let work = work_dir(&sc_dir);
    let menu = work.join(MENU_CONFIG);
    if menu.is_file() {
        let build = build_dir(&work);
        let from = if env_work.is_some() {
            WorkDirSource::Env
        } else {
            WorkDirSource::MenuConfig
        };
        return Ok(Some(with_env(
            with_work_dir_source(load(&[menu], KasSource::MenuConfig, &work, &build), from),
            env,
        )));
    }

    // A build directory kas generated: the configured one, `KAS_BUILD_DIR`, the one
    // discovery would find (walking up), or `<work>/build`.
    let candidates_dirs = [
        cfg.build_dir.as_deref().map(lexical_abs),
        env_build.map(lexical_abs),
        super::layers::find_build_dir(&sc_dir),
        Some(work.join("build")),
    ];
    let Some(build) = candidates_dirs
        .into_iter()
        .flatten()
        .find(|b| is_kas_build_dir(b))
    else {
        return Ok(None);
    };
    let (work, from) = match (env_work, build.parent()) {
        (Some(w), _) => (lexical_abs(w), WorkDirSource::Env),
        (None, Some(p)) => (p.to_path_buf(), WorkDirSource::BuildDirParent),
        (None, None) => (work, WorkDirSource::WorkingDir),
    };
    let found = candidate_files(&work);
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(with_env(
            with_work_dir_source(
                load(std::slice::from_ref(one), KasSource::Found, &work, &build),
                from,
            ),
            env,
        ))),
        many => Ok(Some(KasProject {
            files: Vec::new(),
            source: KasSource::Ambiguous {
                candidates: many.to_vec(),
            },
            root: work.clone(),
            work_dir: work,
            work_dir_source: from,
            build_dir: build,
            read: Vec::new(),
            lock_files: Vec::new(),
            machine: None,
            distro: None,
            target: Vec::new(),
            task: None,
            repos: Vec::new(),
            layers: Vec::new(),
            local_conf_header: Vec::new(),
            bblayers_conf_header: Vec::new(),
            problems: Vec::new(),
            not_in_bblayers: Vec::new(),
            not_in_kas: Vec::new(),
            env: env.overrides.clone(),
            repo_includes: Vec::new(),
        })),
    }
}

/// `project` with its work directory decided by `from`.
fn with_work_dir_source(mut project: KasProject, from: WorkDirSource) -> KasProject {
    project.work_dir_source = from;
    project
}

/// `project` with the `KAS_*` overrides of `env`. They are not tied to a directory (kas
/// reads them wherever it runs), so they count whatever the working directory.
fn with_env(mut project: KasProject, env: &KasEnv) -> KasProject {
    project.env = env.overrides.clone();
    project
}

/// kas files in `dir` and `dir/kas`: `.yml`, `.yaml` or `.json` files whose top level is a
/// mapping with `header`, not lock files, and not included (by a string include) by
/// another candidate. Sorted.
fn candidate_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for d in [dir.to_path_buf(), dir.join("kas")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let ext_ok = [".yml", ".yaml", ".json"].iter().any(|x| name.ends_with(x));
            if !ext_ok || name.contains(".lock.") || name.starts_with('.') || !p.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(doc) = parse_yaml(&text, 0) else {
                continue;
            };
            if doc.get("header").is_some() {
                found.push((p, doc));
            }
        }
    }
    let included: BTreeSet<PathBuf> = found
        .iter()
        .flat_map(|(p, doc)| {
            let base = p.parent().map(Path::to_path_buf).unwrap_or_default();
            string_includes(doc)
                .into_iter()
                .flat_map(move |inc| [normalize(&base.join(&inc)), normalize(&dir.join(&inc))])
                .collect::<Vec<_>>()
        })
        .collect();
    let mut out: Vec<PathBuf> = found
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| !included.contains(&normalize(p)))
        .collect();
    out.sort();
    out
}

fn string_includes(doc: &Node) -> Vec<String> {
    let Some(Value::Seq(items)) = doc
        .get("header")
        .and_then(|h| h.get("includes"))
        .map(|n| &n.value)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|i| i.as_str().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Loading

/// Reads the project made of `files` (absolute) as kas would combine them. Problems never
/// fail the load: they are listed in [`KasProject::problems`].
pub fn load(files: &[PathBuf], source: KasSource, work_dir: &Path, build_dir: &Path) -> KasProject {
    let files: Vec<PathBuf> = files.iter().map(|f| lexical_abs(f)).collect();
    let first_dir = files
        .first()
        .and_then(|f| f.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| work_dir.to_path_buf());
    let root = canonical_or(&repo_root(&first_dir).unwrap_or(first_dir));
    let work_dir = canonical_or(work_dir);
    let mut problems = Vec::new();
    let roots: BTreeSet<Option<PathBuf>> = files
        .iter()
        .map(|f| f.parent().and_then(repo_root).map(|r| canonical_or(&r)))
        .collect();
    if roots.len() > 1 {
        problems.push(
            "the kas files are not all in the same repository, which kas refuses".to_string(),
        );
    }

    let mut loader = Loader {
        root: root.clone(),
        work_dir: work_dir.clone(),
        read: Vec::new(),
        locks: Vec::new(),
        problems: Vec::new(),
        missing_repos: BTreeSet::new(),
        repo_includes: BTreeSet::new(),
        order: Vec::new(),
    };
    // kas iterates: repositories found in one pass make their includes readable in the
    // next. sous-chef never checks out, so repository paths only grow from what is on disk.
    let mut repo_paths: Vec<(String, PathBuf)> = Vec::new();
    let mut merged = Node::map(Vec::new());
    for _ in 0..4 {
        loader.read.clear();
        loader.locks.clear();
        loader.problems.clear();
        loader.missing_repos.clear();
        loader.repo_includes.clear();
        loader.order.clear();
        let mut configs = Vec::new();
        for f in &files {
            loader.file(f, &root, &repo_paths, &mut configs, 0);
        }
        merged = configs
            .iter()
            .fold(Node::map(Vec::new()), |acc, c| merge(&acc, c));
        let repos = build_repos(&merged, &root, &work_dir, &loader.read, &mut Vec::new());
        let next: Vec<(String, PathBuf)> = repos
            .iter()
            .filter(|r| r.path.is_dir())
            .map(|r| (r.id.clone(), r.path.clone()))
            .collect();
        if next == repo_paths {
            break;
        }
        repo_paths = next;
    }
    problems.append(&mut loader.problems);
    for r in &loader.missing_repos {
        problems.push(format!(
            "includes from repo {r} were not read: it is not checked out yet (run kas checkout)"
        ));
    }

    // `read` (read order) is what node positions index; the project lists merge order.
    let read = loader.read;
    let merge_order: Vec<PathBuf> = loader
        .order
        .iter()
        .filter_map(|i| read.get(*i).cloned())
        .collect();
    let repos = build_repos(&merged, &root, &work_dir, &read, &mut problems);
    let mut layers = build_layers(&merged, &repos, &mut problems);
    layers.sort_by(|a, b| {
        let name = |l: &KasLayer| {
            repos
                .iter()
                .find(|r| r.id == l.repo)
                .map(|r| r.name.clone())
                .unwrap_or_default()
        };
        (-a.prio, name(a), &a.name).cmp(&(-b.prio, name(b), &b.name))
    });
    let loc = |n: &Node| KasLoc {
        file: read.get(n.file).cloned().unwrap_or_default(),
        line: n.line,
    };
    let setting = |key: &str| {
        merged.get(key).and_then(|n| {
            n.as_str().map(|v| KasSetting {
                value: v.to_string(),
                loc: loc(n),
            })
        })
    };
    let target = match merged.get("target") {
        Some(n) => match &n.value {
            Value::Seq(items) => items
                .iter()
                .filter_map(|i| {
                    i.as_str().map(|v| KasSetting {
                        value: v.to_string(),
                        loc: loc(i),
                    })
                })
                .collect(),
            _ => n
                .as_str()
                .map(|v| {
                    vec![KasSetting {
                        value: v.to_string(),
                        loc: loc(n),
                    }]
                })
                .unwrap_or_default(),
        },
        None => Vec::new(),
    };
    let headers = |key: &str, problems: &mut Vec<String>| -> Vec<HeaderEntry> {
        let Some(Value::Map(entries)) = merged.get(key).map(|n| &n.value) else {
            return Vec::new();
        };
        let mut out: Vec<HeaderEntry> = Vec::new();
        for (k, v) in entries {
            match v.as_str() {
                Some(text) => out.push(HeaderEntry {
                    id: k.text.clone(),
                    text: text.to_string(),
                    loc: KasLoc {
                        file: read.get(v.file).cloned().unwrap_or_default(),
                        line: v.content_line(),
                    },
                }),
                None => problems.push(format!(
                    "{key} entry {} is not a string; kas would write it as text",
                    k.text
                )),
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    };
    let local_conf_header = headers("local_conf_header", &mut problems);
    let bblayers_conf_header = headers("bblayers_conf_header", &mut problems);
    KasProject {
        machine: setting("machine"),
        distro: setting("distro"),
        task: setting("task"),
        target,
        files,
        source,
        root,
        work_dir,
        work_dir_source: WorkDirSource::Given,
        build_dir: canonical_or(build_dir),
        lock_files: loader.locks,
        read: merge_order,
        repos,
        layers,
        local_conf_header,
        bblayers_conf_header,
        problems,
        not_in_bblayers: Vec::new(),
        not_in_kas: Vec::new(),
        env: KasOverrides::default(),
        repo_includes: loader.repo_includes.into_iter().collect(),
    }
}

struct Loader {
    root: PathBuf,
    work_dir: PathBuf,
    read: Vec<PathBuf>,
    locks: Vec<PathBuf>,
    problems: Vec<String>,
    missing_repos: BTreeSet<String>,
    /// Every repository a `{repo, file}` include names.
    repo_includes: BTreeSet<String>,
    /// Indexes into `read`, in merge order.
    order: Vec<usize>,
}

impl Loader {
    /// Whether `path` (resolved) is inside the project root or the kas work directory.
    fn allowed(&self, resolved: &Path) -> bool {
        resolved.starts_with(&self.root) || resolved.starts_with(&self.work_dir)
    }

    /// Loads `path` with its lock file and includes into `configs`, in kas's merge order.
    /// `repo_path` is the root string includes are relative to.
    fn file(
        &mut self,
        path: &Path,
        repo_path: &Path,
        repos: &[(String, PathBuf)],
        configs: &mut Vec<Node>,
        depth: usize,
    ) {
        if depth > MAX_INCLUDE_DEPTH {
            self.problems.push(format!(
                "includes nest deeper than {MAX_INCLUDE_DEPTH} at {} (a cycle?); not followed",
                path.display()
            ));
            return;
        }
        let resolved = match super::resolve_existing(path) {
            Ok(r) if self.allowed(&r) => r,
            _ => {
                self.problems.push(format!(
                    "kas file {} is outside the project ({}) and the kas work directory; not read",
                    path.display(),
                    self.root.display()
                ));
                return;
            }
        };
        let text = match std::fs::read_to_string(&resolved) {
            Ok(t) => t,
            Err(e) => {
                self.problems
                    .push(format!("cannot read kas file {}: {e}", path.display()));
                return;
            }
        };
        let idx = self.read.len();
        self.read.push(resolved.clone());
        let doc = match parse_yaml(&text, idx) {
            Ok(d) => d,
            Err(e) => {
                self.problems.push(format!(
                    "kas file {} is not valid YAML: {e}",
                    path.display()
                ));
                return;
            }
        };
        if !matches!(doc.value, Value::Map(_)) {
            self.problems.push(format!(
                "kas file {} does not contain a mapping; ignored",
                path.display()
            ));
            return;
        }
        let lock = lock_file_for(&resolved);
        if lock.is_file() && !self.read.contains(&lock) {
            self.locks.push(lock.clone());
            self.file(&lock, repo_path, repos, configs, depth + 1);
        }
        let includes = doc
            .get("header")
            .and_then(|h| h.get("includes"))
            .and_then(|i| match &i.value {
                Value::Seq(items) => Some(items.clone()),
                _ => None,
            })
            .unwrap_or_default();
        for inc in includes {
            match &inc.value {
                Value::Str(s) => {
                    let Some(mut target) = self.inside(repo_path, s, path) else {
                        continue;
                    };
                    if !target.exists()
                        && let Some(dir) = resolved.parent()
                        && let Some(alt) = self.inside(dir, s, path)
                        && alt.exists()
                    {
                        target = alt;
                    }
                    self.file(&target, repo_path, repos, configs, depth + 1);
                }
                Value::Map(_) => {
                    let (Some(repo), Some(file)) = (
                        inc.get("repo").and_then(Node::as_str),
                        inc.get("file").and_then(Node::as_str),
                    ) else {
                        self.problems.push(format!(
                            "an include in {} has no repo or file; ignored",
                            path.display()
                        ));
                        continue;
                    };
                    self.repo_includes.insert(repo.to_string());
                    match repos.iter().find(|(id, _)| id == repo) {
                        Some((_, dir)) => {
                            let dir = canonical_or(dir);
                            if !self.allowed(&dir) {
                                self.problems.push(format!(
                                    "repo {repo} at {} is outside the project and the kas work directory; its include {file} is not read",
                                    dir.display()
                                ));
                                continue;
                            }
                            let Some(target) = self.inside(&dir, file, path) else {
                                continue;
                            };
                            self.file(&target, &dir, repos, configs, depth + 1);
                        }
                        None => {
                            self.missing_repos.insert(repo.to_string());
                        }
                    }
                }
                _ => self.problems.push(format!(
                    "an include in {} is neither a path nor a {{repo, file}} mapping; ignored",
                    path.display()
                )),
            }
        }
        self.order.push(idx);
        configs.push(doc);
    }

    /// `base/include` when it resolves inside `base` (kas's `sanitize_include_path`), else
    /// a problem and `None`.
    fn inside(&mut self, base: &Path, include: &str, from: &Path) -> Option<PathBuf> {
        let base = canonical_or(base);
        let candidate = base.join(include);
        match super::resolve_existing(&candidate) {
            Ok(r) if r.starts_with(&base) => Some(r),
            _ => {
                self.problems.push(format!(
                    "include {include} in {} resolves outside {}; not followed",
                    from.display(),
                    base.display()
                ));
                None
            }
        }
    }
}

/// `<dir>/<stem>.lock.<ext>` for a kas file (kas `get_lock_filename`).
pub fn lock_file_for(file: &Path) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = file
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    file.with_file_name(format!("{stem}.lock{ext}"))
}

/// The value of a repo property: `Some(None)` when it is set to null (which removes a
/// default), `None` when absent.
fn prop<'a>(cfg: Option<&'a Node>, key: &str) -> Option<Option<&'a str>> {
    cfg?.get(key).map(|n| n.as_str())
}

fn build_repos(
    merged: &Node,
    root: &Path,
    work_dir: &Path,
    read: &[PathBuf],
    problems: &mut Vec<String>,
) -> Vec<KasRepo> {
    let Some(Value::Map(entries)) = merged.get("repos").map(|n| &n.value) else {
        return Vec::new();
    };
    let defaults = merged.get("defaults").and_then(|d| d.get("repos"));
    let overrides = merged.get("overrides").and_then(|o| o.get("repos"));
    let mut out = Vec::new();
    for (k, v) in entries {
        let cfg = match &v.value {
            Value::Map(_) => Some(v),
            Value::Null => None,
            _ => {
                problems.push(format!("repos entry {} is not a mapping; ignored", k.text));
                continue;
            }
        };
        let ov = overrides.and_then(|o| o.get(&k.text));
        let s = |o: Option<Option<&str>>| o.flatten().map(str::to_string);
        let name = s(prop(cfg, "name")).unwrap_or_else(|| k.text.clone());
        let refspec = s(prop(cfg, "refspec").or_else(|| prop(defaults, "refspec")));
        let mut url = s(prop(cfg, "url"));
        let mut commit = s(prop(cfg, "commit"));
        let tag = s(prop(cfg, "tag").or_else(|| prop(defaults, "tag")));
        let mut branch = s(prop(cfg, "branch").or_else(|| prop(defaults, "branch")));
        let mut refspec = refspec;
        if refspec.is_none() {
            commit = s(prop(ov, "commit")).or(commit);
            branch = s(prop(ov, "branch")).or(branch);
            url = s(prop(ov, "url")).or(url);
        } else {
            refspec = s(prop(ov, "commit")).or(refspec);
        }
        let path = match (s(prop(cfg, "path")), &url) {
            (None, None) => root.to_path_buf(),
            (None, Some(_)) => work_dir.join(&name),
            (Some(p), _) => {
                let p = PathBuf::from(p);
                if p.is_absolute() {
                    normalize(&p)
                } else {
                    normalize(&work_dir.join(p))
                }
            }
        };
        let own = url.is_none() && path.starts_with(root);
        out.push(KasRepo {
            id: k.text.clone(),
            name,
            url,
            commit,
            branch,
            tag,
            refspec,
            path,
            own,
            loc: Some(KasLoc {
                file: read.get(k.file).cloned().unwrap_or_default(),
                line: k.line,
            }),
        });
    }
    out
}

/// The legacy values that disable a layer (kas `_create_layers_from_dict`), besides
/// `disabled`.
const LEGACY_DISABLED: &[&str] = &["excluded", "n", "no", "0", "false"];

fn build_layers(merged: &Node, repos: &[KasRepo], problems: &mut Vec<String>) -> Vec<KasLayer> {
    let mut out = Vec::new();
    let entries = match merged.get("repos").map(|n| &n.value) {
        Some(Value::Map(e)) => e.clone(),
        _ => Vec::new(),
    };
    for repo in repos {
        let cfg = entries
            .iter()
            .find(|(k, _)| k.text == repo.id)
            .map(|(_, v)| v);
        let layers = cfg.and_then(|c| c.get("layers"));
        // kas 5.5 `Repo.factory`: `repo_config.get('layers', {'': None})`, then one layer per
        // entry (`_create_layers_from_dict`). So a missing key is the repository itself, an
        // empty mapping adds no layer (the schema's description says otherwise; the code
        // decides), and null fails the schema (`layers` must be an object) before that.
        let list: Vec<(String, i64)> = match layers.map(|l| &l.value) {
            None => vec![(String::new(), 0)],
            Some(Value::Null) => {
                problems.push(format!(
                    "layers of repo {} is null, which kas refuses (its schema wants a mapping); leave the key out for the repository itself",
                    repo.id
                ));
                Vec::new()
            }
            Some(Value::Map(m)) => m
                .iter()
                .filter_map(|(k, v)| match &v.value {
                    Value::Null => Some((k.text.clone(), 0)),
                    Value::Str(s)
                        if s == "disabled"
                            || LEGACY_DISABLED.contains(&s.as_str())
                            || (v.plain && yaml11_false_or_zero(s)) =>
                    {
                        None
                    }
                    Value::Map(_) => {
                        let prio = v
                            .get("prio")
                            .and_then(Node::as_str)
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(0);
                        Some((k.text.clone(), prio))
                    }
                    _ => {
                        problems.push(format!(
                            "layer {} of repo {} has a value kas does not accept; ignored",
                            k.text, repo.id
                        ));
                        None
                    }
                })
                .collect(),
            Some(_) => {
                problems.push(format!(
                    "layers of repo {} is not a mapping; ignored",
                    repo.id
                ));
                Vec::new()
            }
        };
        for (name, prio) in list {
            let path = normalize(&repo.path.join(&name));
            out.push(KasLayer {
                repo: repo.id.clone(),
                present: path.join("conf").join("layer.conf").is_file(),
                own: repo.own,
                name,
                path,
                prio,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// YAML

/// A YAML value with the position of its first character.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub value: Value,
    /// Index into the loader's list of files read.
    pub file: usize,
    /// 1-based.
    pub line: u32,
    /// 0-based column.
    pub col: u32,
    /// For a scalar: written as a literal or folded block (`|`, `>`).
    pub block: bool,
    /// For a scalar: plain (not quoted, not a block), so YAML 1.1 may read it as a boolean
    /// or a number ([`yaml11_false_or_zero`]).
    pub plain: bool,
    /// For a mapping or sequence: written in flow style (`{...}`, `[...]`).
    pub flow: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum Value {
    Null,
    /// A string scalar (numbers and booleans are kept as written).
    Str(String),
    Seq(Vec<Node>),
    Map(Vec<(Key, Node)>),
}

#[derive(Debug, Clone)]
pub(crate) struct Key {
    pub text: String,
    pub file: usize,
    pub line: u32,
    pub col: u32,
}

impl Node {
    fn map(entries: Vec<(Key, Node)>) -> Node {
        Node {
            value: Value::Map(entries),
            file: 0,
            line: 1,
            col: 0,
            block: false,
            plain: false,
            flow: false,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|(k, _)| k.text == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The line the text starts on. For a block scalar the parser's mark is already on its
    /// first content line (the line after the `|` indicator), checked in the tests below.
    fn content_line(&self) -> u32 {
        self.line
    }

    /// The last line any part of this node is on (block scalars: their last content line,
    /// approximated by the number of lines of the text).
    pub(crate) fn last_line(&self) -> u32 {
        match &self.value {
            Value::Null => self.line,
            Value::Str(s) => {
                if self.block {
                    let n = u32::try_from(s.lines().count()).unwrap_or(u32::MAX);
                    self.line.saturating_add(n.saturating_sub(1))
                } else {
                    self.line
                }
            }
            Value::Seq(items) => items.iter().map(Node::last_line).max().unwrap_or(self.line),
            Value::Map(entries) => entries
                .iter()
                .map(|(k, v)| match v.value {
                    // An empty value is marked where the next token starts, maybe a later line.
                    Value::Null => k.line,
                    _ => k.line.max(v.last_line()),
                })
                .max()
                .unwrap_or(self.line),
        }
    }

    /// Structural equality, positions ignored.
    pub(crate) fn same(&self, other: &Node) -> bool {
        match (&self.value, &other.value) {
            (Value::Null, Value::Null) => true,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Seq(a), Value::Seq(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            (Value::Map(a), Value::Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka.text == kb.text && va.same(vb))
            }
            _ => false,
        }
    }
}

/// kas's `_internal_dict_merge`: mappings merge recursively (keys keep their first
/// position, new keys go last), anything else is replaced by `upd`.
fn merge(dest: &Node, upd: &Node) -> Node {
    match (&dest.value, &upd.value) {
        (Value::Map(d), Value::Map(u)) => {
            let mut out = d.clone();
            for (k, v) in u {
                match out.iter_mut().find(|(ok, _)| ok.text == k.text) {
                    Some((_, ov)) => {
                        *ov = if matches!(ov.value, Value::Map(_))
                            && matches!(v.value, Value::Map(_))
                        {
                            merge(ov, v)
                        } else {
                            v.clone()
                        };
                    }
                    None => out.push((k.clone(), v.clone())),
                }
            }
            Node {
                value: Value::Map(out),
                ..upd.clone()
            }
        }
        _ => upd.clone(),
    }
}

/// Parses the first document of `text` (kas reads one with `yaml.safe_load`). An empty
/// document is null.
pub(crate) fn parse_yaml(text: &str, file: usize) -> std::result::Result<Node, String> {
    let run = std::panic::catch_unwind(|| {
        let mut b = Builder {
            file,
            text,
            stack: Vec::new(),
            anchors: Vec::new(),
            docs: Vec::new(),
        };
        let mut parser = Parser::new_from_str(text);
        parser
            .load(&mut b, false)
            .map(|()| b.docs.into_iter().next())
            .map_err(|e| e.to_string())
    });
    match run {
        Ok(Ok(Some(doc))) => Ok(doc),
        Ok(Ok(None)) => Ok(Node {
            value: Value::Null,
            file,
            line: 1,
            col: 0,
            block: false,
            plain: false,
            flow: false,
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the YAML parser failed on this file".into()),
    }
}

enum Frame {
    Seq(Node, usize, Vec<Node>),
    Map(Node, usize, Vec<(Key, Node)>, Option<Key>),
}

struct Builder<'a> {
    file: usize,
    text: &'a str,
    stack: Vec<Frame>,
    anchors: Vec<(usize, Node)>,
    docs: Vec<Node>,
}

impl Builder<'_> {
    fn start(&self, mark: Marker) -> Node {
        Node {
            value: Value::Null,
            file: self.file,
            line: u32::try_from(mark.line()).unwrap_or(u32::MAX),
            col: u32::try_from(mark.col()).unwrap_or(u32::MAX),
            block: false,
            plain: false,
            flow: false,
        }
    }

    /// Whether the collection starting at `mark` is in flow style.
    fn flow_at(&self, mark: Marker) -> bool {
        self.text
            .get(mark.index()..)
            .and_then(|s| s.chars().next())
            .is_some_and(|c| c == '{' || c == '[')
    }

    fn push(&mut self, node: Node, anchor: usize) {
        if anchor > 0 {
            self.anchors.push((anchor, node.clone()));
        }
        match self.stack.last_mut() {
            None => self.docs.push(node),
            Some(Frame::Seq(_, _, items)) => items.push(node),
            Some(Frame::Map(_, _, entries, pending)) => match pending.take() {
                None => {
                    let text = match &node.value {
                        Value::Str(s) => s.clone(),
                        Value::Null => "null".to_string(),
                        _ => String::new(),
                    };
                    *pending = Some(Key {
                        text,
                        file: node.file,
                        line: node.line,
                        col: node.col,
                    });
                }
                Some(k) => match entries.iter_mut().find(|(ek, _)| ek.text == k.text) {
                    Some(e) => e.1 = node,
                    None => entries.push((k, node)),
                },
            },
        }
    }
}

impl MarkedEventReceiver for Builder<'_> {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        match ev {
            Event::Scalar(v, style, anchor, _tag) => {
                let mut n = self.start(mark);
                let plain = style == TScalarStyle::Plain;
                n.block = matches!(style, TScalarStyle::Literal | TScalarStyle::Folded);
                n.plain = plain;
                n.value = if plain && matches!(v.as_str(), "" | "~" | "null" | "Null" | "NULL") {
                    Value::Null
                } else {
                    Value::Str(v)
                };
                self.push(n, anchor);
            }
            Event::SequenceStart(anchor, _) => {
                let mut n = self.start(mark);
                n.flow = self.flow_at(mark);
                self.stack.push(Frame::Seq(n, anchor, Vec::new()));
            }
            Event::MappingStart(anchor, _) => {
                let mut n = self.start(mark);
                n.flow = self.flow_at(mark);
                self.stack.push(Frame::Map(n, anchor, Vec::new(), None));
            }
            Event::SequenceEnd | Event::MappingEnd => match self.stack.pop() {
                Some(Frame::Seq(mut n, anchor, items)) => {
                    n.value = Value::Seq(items);
                    self.push(n, anchor);
                }
                Some(Frame::Map(mut n, anchor, entries, _)) => {
                    n.value = Value::Map(flatten_merge_keys(entries));
                    self.push(n, anchor);
                }
                None => {}
            },
            Event::Alias(id) => {
                let node = self
                    .anchors
                    .iter()
                    .rev()
                    .find(|(a, _)| *a == id)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_else(|| self.start(mark));
                self.push(node, 0);
            }
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd => {}
        }
    }
}

/// YAML 1.1 merge keys, as kas reads them (PyYAML `SafeLoader`, `constructor.py`
/// `flatten_mapping`, PyYAML 6.0.3 as installed with kas 5.5): each `<<` entry is removed;
/// its value, a mapping or a sequence of mappings (the later ones first: PyYAML reverses
/// the list), contributes its entries before the mapping's own; then, as a Python dict is
/// built from the pairs, a key keeps its first position and takes its last value, so the
/// mapping's own keys win, and of a sequence the first mapping wins. A `<<` value of any
/// other kind, which PyYAML refuses, is dropped. Two `<<` keys in one mapping are one (the
/// last), as for any repeated key here.
fn flatten_merge_keys(entries: Vec<(Key, Node)>) -> Vec<(Key, Node)> {
    if !entries.iter().any(|(k, _)| k.text == "<<") {
        return entries;
    }
    let mut merged: Vec<(Key, Node)> = Vec::new();
    let mut own: Vec<(Key, Node)> = Vec::new();
    for (k, v) in entries {
        if k.text != "<<" {
            own.push((k, v));
            continue;
        }
        match v.value {
            Value::Map(m) => merged.extend(m),
            Value::Seq(items) => {
                let maps: Vec<Vec<(Key, Node)>> = items
                    .into_iter()
                    .filter_map(|i| match i.value {
                        Value::Map(m) => Some(m),
                        _ => None,
                    })
                    .collect();
                for m in maps.into_iter().rev() {
                    merged.extend(m);
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<(Key, Node)> = Vec::new();
    for (k, v) in merged.into_iter().chain(own) {
        match out.iter_mut().find(|(ek, _)| ek.text == k.text) {
            Some(e) => e.1 = v,
            None => out.push((k, v)),
        }
    }
    out
}

/// Whether a plain scalar is, in YAML 1.1 as PyYAML resolves it (`resolver.py`), the
/// boolean false (`no`, `No`, `NO`, `false`, `False`, `FALSE`, `off`, `Off`, `OFF`) or an
/// integer equal to 0 (`0`, `-0`, `+0`, `00`, `0_0`, `0x0`, `0b0`). kas compares a layer's
/// value with its deprecated "disabled" tokens by Python equality, where `False == 0`, so
/// each of these disables the layer.
pub(crate) fn yaml11_false_or_zero(s: &str) -> bool {
    const FALSE: [&str; 9] = [
        "no", "No", "NO", "false", "False", "FALSE", "off", "Off", "OFF",
    ];
    if FALSE.contains(&s) {
        return true;
    }
    let t = s.strip_prefix(['-', '+']).unwrap_or(s);
    let zeros = |body: &str| {
        !body.is_empty() && body.chars().all(|c| c == '0' || c == '_') && body.contains('0')
    };
    if let Some(body) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0x")) {
        return zeros(body);
    }
    // `0`, and the octal form `0[0-7_]+`.
    t.starts_with('0') && zeros(t)
}

// ---------------------------------------------------------------------------------------
// Editing (scaffold)

/// The result of planning to add a layer to a kas file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerEdit {
    /// `file`'s new content, with the layer line inserted.
    Modify { file: PathBuf, content: String },
    /// No file could be edited safely: the lines to add by hand, and why.
    Manual { lines: String, why: String },
}

/// Plans adding the layer at `layer_root` (absolute) to the project: under `layers` of the
/// first own repository (in merge order, in a file inside the project root) whose
/// `layers` is a block mapping, as one new line indented like the existing entries, right
/// after the last of them. The edit is accepted only when the file still parses and the
/// result equals the old content with the new key added; otherwise, or when the layer is
/// not inside an own repository, the lines to add are returned instead.
pub fn plan_add_layer(project: &KasProject, layer_root: &Path) -> LayerEdit {
    let own = project
        .repos
        .iter()
        .filter(|r| r.own && layer_root.starts_with(&r.path))
        .fold(None::<&KasRepo>, |best, r| match best {
            // The deepest repository; among equals the first, in merge order.
            Some(b) if b.path.components().count() >= r.path.components().count() => Some(b),
            _ => Some(r),
        });
    let Some(repo) = own else {
        return LayerEdit::Manual {
            lines: format!(
                "repos:\n  {}:\n    path: {}\n",
                layer_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                layer_root.display()
            ),
            why: "the new layer is not inside a repository of the project".into(),
        };
    };
    let rel = layer_root
        .strip_prefix(&repo.path)
        .map(|r| r.display().to_string())
        .unwrap_or_default();
    let manual = |why: String| LayerEdit::Manual {
        lines: format!("repos:\n  {}:\n    layers:\n      {rel}:\n", repo.id),
        why,
    };
    if rel.is_empty()
        || rel.contains([
            ':', '#', '\'', '"', '{', '}', '[', ']', ',', '&', '*', '!', '|', '>', '%', '@', '`',
        ])
    {
        return manual("the layer path cannot be written as a plain YAML key".into());
    }
    for file in &project.read {
        if !file.starts_with(&project.root) || project.lock_files.contains(file) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(doc) = parse_yaml(&text, 0) else {
            continue;
        };
        let Some(layers) = doc
            .get("repos")
            .and_then(|r| r.get(&repo.id))
            .and_then(|r| r.get("layers"))
        else {
            continue;
        };
        let rel_display = file
            .strip_prefix(&project.root)
            .unwrap_or(file)
            .display()
            .to_string();
        let Value::Map(entries) = &layers.value else {
            return manual(format!(
                "`layers` of repo {} in {rel_display} is not a mapping with entries",
                repo.id
            ));
        };
        if entries.is_empty() || layers.flow || text.contains('\r') {
            return manual(format!(
                "`layers` of repo {} in {rel_display} is not a block mapping sous-chef can extend safely",
                repo.id
            ));
        }
        if entries.iter().any(|(k, _)| k.text == rel) {
            return manual(format!("{rel_display} already lists layer {rel}"));
        }
        let indent = entries[0].0.col as usize;
        if entries.iter().any(|(k, _)| k.col as usize != indent) {
            return manual(format!(
                "the entries of `layers` in {rel_display} are not aligned"
            ));
        }
        let after = layers.last_line() as usize;
        let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
        if after == 0 || after > lines.len() {
            return manual(format!("cannot place the new line in {rel_display}"));
        }
        let new_line = format!("{}{rel}:\n", " ".repeat(indent));
        let mut content = String::with_capacity(text.len() + new_line.len() + 1);
        let last_has_newline = lines[after - 1].ends_with('\n');
        for (i, l) in lines.iter_mut().enumerate() {
            content.push_str(l);
            if i + 1 == after {
                if !last_has_newline {
                    content.push('\n');
                }
                content.push_str(&new_line);
            }
        }
        if !verify_added(&doc, &content, &repo.id, &rel) {
            return manual(format!(
                "inserting the line into {rel_display} would change more than the layer list"
            ));
        }
        return LayerEdit::Modify {
            file: file.clone(),
            content,
        };
    }
    manual(format!(
        "no kas file of the project lists layers of repo {} as a block mapping",
        repo.id
    ))
}

/// Whether `content` parses to `old` plus the key `rel` under `repos.<id>.layers`.
fn verify_added(old: &Node, content: &str, id: &str, rel: &str) -> bool {
    let Ok(mut new) = parse_yaml(content, 0) else {
        return false;
    };
    let Some(Value::Map(repos)) = map_get_mut(&mut new, "repos").map(|n| &mut n.value) else {
        return false;
    };
    let Some((_, repo)) = repos.iter_mut().find(|(k, _)| k.text == id) else {
        return false;
    };
    let Some(Value::Map(layers)) = map_get_mut(repo, "layers").map(|n| &mut n.value) else {
        return false;
    };
    let Some(pos) = layers.iter().position(|(k, _)| k.text == rel) else {
        return false;
    };
    if pos + 1 != layers.len() || !matches!(layers[pos].1.value, Value::Null) {
        return false;
    }
    layers.remove(pos);
    new.same(old)
}

fn map_get_mut<'a>(n: &'a mut Node, key: &str) -> Option<&'a mut Node> {
    match &mut n.value {
        Value::Map(entries) => entries
            .iter_mut()
            .find(|(k, _)| k.text == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// Paths

/// The repository root of `dir` as kas finds it (`git rev-parse --show-toplevel
/// --show-superproject-working-tree`, else `hg root`), without running either: the nearest
/// ancestor with a `.git` directory, a `.git` file (a worktree; a submodule's `.git` file,
/// whose `gitdir` is under `modules/`, continues to the superproject) or `.hg`.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    let mut found = None;
    while let Some(d) = cur {
        let git = d.join(".git");
        if git.is_dir() || d.join(".hg").is_dir() {
            return Some(d.to_path_buf());
        }
        if git.is_file() {
            let submodule = std::fs::read_to_string(&git).is_ok_and(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("gitdir:"))
                    .is_some_and(|g| g.contains("/modules/"))
            });
            if !submodule {
                return Some(d.to_path_buf());
            }
            found = Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    found
}

/// Lexical normalisation (see `workspace::normalize`).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn lexical_abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        normalize(&std::env::current_dir().unwrap_or_default().join(p))
    }
}

fn canonical_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| normalize(p))
}

/// Equal after resolving symlinks where the paths exist.
pub fn same_path(a: &Path, b: &Path) -> bool {
    canonical_or(a) == canonical_or(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_merge_keys_as_pyyaml() {
        let text = "base: &b\n  a: 1\n  b: 2\nother: &o\n  b: 20\n  c: 30\none:\n  <<: *b\n  b: own\ntwo:\n  <<: [*b, *o]\n  d: 4\n";
        let doc = parse_yaml(text, 0).unwrap();
        let kv = |n: &Node| -> Vec<(String, String)> {
            let Value::Map(e) = &n.value else { panic!() };
            e.iter()
                .map(|(k, v)| (k.text.clone(), v.as_str().unwrap_or("?").to_string()))
                .collect()
        };
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        // The mapping's own key wins; merged keys come first.
        assert_eq!(kv(doc.get("one").unwrap()), [s("a", "1"), s("b", "own")]);
        // Of a list, the first mapping wins (PyYAML reverses it before extending).
        assert_eq!(
            kv(doc.get("two").unwrap()),
            [s("b", "2"), s("c", "30"), s("a", "1"), s("d", "4")]
        );
    }

    #[test]
    fn yaml11_false_and_zero() {
        for t in [
            "no", "No", "NO", "false", "False", "FALSE", "off", "Off", "OFF", "0", "-0", "+0",
            "00", "0_0", "0x0", "0b0",
        ] {
            assert!(yaml11_false_or_zero(t), "{t}");
        }
        for t in [
            "yes", "on", "true", "n", "excluded", "1", "10", "0x10", "0b1", "0.0", "", "0b", "nO",
        ] {
            assert!(!yaml11_false_or_zero(t), "{t}");
        }
    }

    #[test]
    fn layers_values_as_kas_reads_them() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let f = root.join("kas.yaml");
        std::fs::write(
            &f,
            "header:\n  version: 11\nrepos:\n  missing:\n    path: r1\n  empty:\n    path: r2\n    layers: {}\n  null:\n    path: r3\n    layers:\n  yaml11:\n    path: r4\n    layers:\n      off-plain: off\n      no-cap: No\n      zero: 00\n      quoted-no: 'No'\n      kept:\n",
        )
        .unwrap();
        let k = load(&[f], KasSource::Config, &root, &root.join("build"));
        let layers: Vec<(String, String)> = k
            .layers
            .iter()
            .map(|l| (l.repo.clone(), l.name.clone()))
            .collect();
        // No `layers`: the repository itself; `{}`: no layer; null: refused by kas; plain
        // YAML 1.1 false or 0: disabled; a quoted 'No' is a string kas does not accept.
        assert_eq!(
            layers,
            [
                ("missing".to_string(), String::new()),
                ("yaml11".to_string(), "kept".to_string())
            ]
        );
        assert!(
            k.problems
                .iter()
                .any(|p| p.starts_with("layers of repo null is null")),
            "{:?}",
            k.problems
        );
        assert!(
            k.problems
                .iter()
                .any(|p| p
                    .starts_with("layer quoted-no of repo yaml11 has a value kas does not accept")),
            "{:?}",
            k.problems
        );
    }

    #[test]
    fn yaml_positions() {
        let text = "header:\n  version: 11\nlocal_conf_header:\n  a: |\n    X = \"1\"\n    Y = \"2\"\n  b: Z = \"3\"\nlayers:\n  x:\n  y: disabled\n";
        let doc = parse_yaml(text, 0).unwrap();
        let a = doc.get("local_conf_header").unwrap().get("a").unwrap();
        assert!(a.block);
        assert_eq!(a.as_str(), Some("X = \"1\"\nY = \"2\"\n"));
        assert_eq!(a.content_line(), 5);
        let b = doc.get("local_conf_header").unwrap().get("b").unwrap();
        assert_eq!((b.line, b.content_line()), (7, 7));
        let layers = doc.get("layers").unwrap();
        assert!(matches!(layers.get("x").unwrap().value, Value::Null));
        assert_eq!(layers.last_line(), 10);
        let Value::Map(e) = &layers.value else {
            panic!()
        };
        assert_eq!((e[0].0.line, e[0].0.col), (9, 2));
        assert!(!layers.flow);
        let flow = parse_yaml("a: {x: 1}\n", 0).unwrap();
        assert!(flow.get("a").unwrap().flow);
    }

    #[test]
    fn merge_follows_kas() {
        let a = parse_yaml(
            "repos:\n  x:\n    url: u\n    layers:\n      l1:\nmachine: m1\n",
            0,
        )
        .unwrap();
        let b = parse_yaml(
            "repos:\n  x:\n    layers:\n      l2:\n  y:\nmachine: m2\n",
            1,
        )
        .unwrap();
        let m = merge(&merge(&Node::map(Vec::new()), &a), &b);
        assert_eq!(m.get("machine").unwrap().as_str(), Some("m2"));
        assert_eq!(m.get("machine").unwrap().file, 1);
        let x = m.get("repos").unwrap().get("x").unwrap();
        assert_eq!(x.get("url").unwrap().as_str(), Some("u"));
        let Value::Map(l) = &x.get("layers").unwrap().value else {
            panic!()
        };
        let names: Vec<&str> = l.iter().map(|(k, _)| k.text.as_str()).collect();
        assert_eq!(names, ["l1", "l2"]);
    }

    #[test]
    fn generated_signatures() {
        assert!(looks_generated_local_conf(
            "# a\nX = \"1\"\nMACHINE ??= \"m\"\nDISTRO ??= \"d\"\nBBMULTICONFIG ?= \"\"\n"
        ));
        assert!(!looks_generated_local_conf(
            "MACHINE ??= \"m\"\nDISTRO ??= \"d\"\n"
        ));
        assert!(!looks_generated_local_conf(
            "MACHINE ??= \"m\"\nDISTRO ??= \"d\"\nBBMULTICONFIG ?= \"\"\nEXTRA = \"1\"\n"
        ));
        assert!(looks_generated_bblayers(
            "BBLAYERS ?= \" \\\n    ${TOPDIR}/../a\"\nBBPATH ?= \"${TOPDIR}\"\nBBFILES ??= \"\"\n"
        ));
    }

    #[test]
    fn lock_file_names() {
        assert_eq!(
            lock_file_for(Path::new("/p/kas/kiss.yaml")),
            PathBuf::from("/p/kas/kiss.lock.yaml")
        );
    }

    #[test]
    fn bad_yaml_is_an_error_not_a_panic() {
        for t in ["a: [", "{", "a: *x", "- a\nb: c", "\u{0}", "a:\n\t- b"] {
            let _ = parse_yaml(t, 0);
        }
        assert!(parse_yaml("a: [", 0).is_err());
    }
}
