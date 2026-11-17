//! Workspace audit report (T8.6): one findings report over a whole build repository.
//!
//! The audit adds no analysis of its own. It runs what the rest of sous-chef already does
//! over everything and assembles the result:
//!
//! - layer discovery ([`crate::workspace::Workspace::discover`]) plus a walk of every
//!   directory under the root for `conf/layer.conf` ([`find_layers`]), following
//!   `.gitmodules` and the kas repositories, so layers on disk that the build does not
//!   enable are listed too (from their `conf/layer.conf` alone: they are not in the index);
//! - git facts per repository ([`git`]: read-only commands, argv only, a timeout, the
//!   `GIT_DIR` family removed; any failure is "unknown");
//! - release detection ([`crate::release`]) and each layer's `LAYERSERIES_COMPAT`;
//! - `org/layer-conf`'s checks ([`org_layer_conf::layer_problems`]), the `layers` tool's
//!   `compat` and `appends` views (`BBLAYERS` order rows, bbappends `BBFILES` does not
//!   collect), [`query::shadowed`] and [`query::orphan_appends`];
//! - every enabled lint rule over every layer, as `sous-chef lint --all` ([`lint::run`]);
//! - the index's parse and resolution results (unparsed statements, unresolved `include` /
//!   `require` / `inherit`, files BitBake does not collect).
//!
//! [`build`] produces an [`Audit`]; [`render_markdown`] and [`render_json`] write it. The
//! report is deterministic for a given repository state apart from the `Generated:` line:
//! everything is sorted, and paths are relative to the root (a path outside the root is
//! shown as `(outside ROOT)/<name>`). It is read-only: it writes nothing into the
//! repository, runs no BitBake and no kas, and fetches nothing; the index is refreshed in
//! the cache directory as every tool does.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, FileSet};
use crate::lint::rules::org_layer_conf::{self, LayerProblem};
use crate::lint::{self, Finding, Severity};
use crate::mcp::Ctx;
use crate::mcp::tools::layers as layers_tool;
use crate::release::Release;
use crate::render::{Format, JsonTable, unescape_cell};
use crate::types::FileKind;
use crate::workspace::{BUILD_LAYER, Workspace, kas, layers};

/// The `schema` field of the JSON report. Bumped when a field is renamed or removed.
pub const SCHEMA: &str = "sous-chef-audit/1";

/// How deep [`find_layers`] looks below the root (the root is depth 0). Deep enough for
/// `sources/poky/meta` and `layers/meta-openembedded/meta-oe`; BitBake's own test layers
/// (`bitbake/lib/layerindexlib/tests/testdata/layer1`, depth 6) stay out.
pub const SCAN_DEPTH: usize = 5;

/// Findings shown per rule for an upstream layer; the rest are counted.
pub const UPSTREAM_EXAMPLES: usize = 3;

/// Entries shown per layer of an upstream layer's parse problems; the rest are counted.
pub const UPSTREAM_PARSE_EXAMPLES: usize = 5;

/// Items in "Most worth attention".
pub const MAX_ATTENTION: usize = 10;

/// Per git invocation.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Shown for a path outside the root, before its last component.
pub const OUTSIDE_ROOT: &str = "(outside ROOT)";

/// How to build the report.
#[derive(Debug, Clone)]
pub struct Options {
    /// The repository root: the layer search is bounded by it and paths are shown relative
    /// to it. Need not be canonical.
    pub root: PathBuf,
    /// The `Generated:` value; `None`: now (UTC).
    pub generated: Option<String>,
    /// Progress lines (the CLI prints them to stderr; never part of the report).
    pub progress: Option<fn(&str)>,
}

impl Options {
    pub fn new(root: &Path) -> Options {
        Options {
            root: root.to_path_buf(),
            generated: None,
            progress: None,
        }
    }

    fn say(&self, msg: &str) {
        if let Some(p) = self.progress {
            p(msg);
        }
    }
}

// ---------------------------------------------------------------------------------------
// The report

/// The whole report. Serialised as is for `--format json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Audit {
    /// [`SCHEMA`].
    pub schema: String,
    /// The only line that changes between two runs on the same repository state.
    pub generated: String,
    pub sous_chef_version: String,
    pub summary: Summary,
    pub inventory: Inventory,
    pub releases: Releases,
    pub structure: Structure,
    pub lint: LintSection,
    pub parse: ParseSection,
    pub not_checked: NotChecked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// One line.
    pub line: String,
    /// Always `.`: every path in the report is relative to it.
    pub root: String,
    pub build_dir: Option<String>,
    /// The kas files, as kas takes them (relative to the root), when this is a kas project.
    pub kas_files: Vec<String>,
    pub release: String,
    /// Where the release came from (`LAYERSERIES_CORENAMES`, `sous-chef.toml`, ...).
    pub release_source: String,
    pub release_note: Option<String>,
    pub counts: Counts,
    /// How [`Summary::attention`] is chosen.
    pub attention_rule: String,
    pub attention: Vec<Attention>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub layers_enabled: usize,
    pub layers_not_enabled: usize,
    /// Enabled and writable (sous-chef.toml `writable_layers`, or a kas project's own).
    pub layers_own: usize,
    pub layers_upstream: usize,
    /// Indexed files by kind (`bb`, `bbappend`, `inc`, `bbclass`, `conf`), the build
    /// directory's included.
    pub files: BTreeMap<String, u32>,
    pub files_total: u32,
    /// Lint findings in the own layers and the build directory.
    pub findings_own: SeverityCounts,
    pub findings_upstream: SeverityCounts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SeverityCounts {
    pub error: usize,
    pub warning: usize,
    pub info: usize,
}

impl SeverityCounts {
    fn add(&mut self, s: Severity) {
        match s {
            Severity::Error => self.error += 1,
            Severity::Warning => self.warning += 1,
            Severity::Info => self.info += 1,
        }
    }

    fn total(&self) -> usize {
        self.error + self.warning + self.info
    }

    fn describe(&self) -> String {
        format!(
            "{} error, {} warning, {} info",
            self.error, self.warning, self.info
        )
    }
}

/// One of the things most worth a person's attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attention {
    /// `lint` or `structure`.
    pub section: String,
    pub severity: String,
    /// The rule id, or the structure check (`compat`, `depends`, `append-no-match`, ...).
    pub what: String,
    pub count: usize,
    /// Where (layers).
    pub layers: Vec<String>,
    /// The first occurrence.
    pub pointer: String,
    /// Its message.
    pub message: String,
}

// ---- 1. Inventory

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inventory {
    pub summary: String,
    /// Enabled layers in `BBLAYERS` order, then layers present but not enabled by path.
    pub layers: Vec<LayerEntry>,
    /// Git repositories holding a layer or named by the kas files, by path.
    pub repositories: Vec<Repository>,
    pub kas: Option<KasInfo>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerEntry {
    /// The workspace's layer name (the name in pointers); for a layer that is not enabled,
    /// its directory name.
    pub name: String,
    /// Relative to the root.
    pub path: String,
    pub enabled: bool,
    /// 1-based position in `BBLAYERS` (discovery order without a build directory).
    pub position: Option<usize>,
    /// `own` (writable), `upstream`, or `not enabled`.
    pub role: String,
    pub collections: Vec<String>,
    pub priority: Option<i32>,
    pub compat: Vec<String>,
    pub depends: Vec<String>,
    /// The repository (a [`Repository::path`]) the layer is in, when git says so.
    pub repository: Option<String>,
    /// Indexed file counts; `None` for a layer that is not enabled (not indexed).
    pub files: Option<FileCounts>,
    /// Why something is missing (an unreadable `layer.conf`).
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct FileCounts {
    pub bb: u32,
    pub bbappend: u32,
    pub inc: u32,
    pub bbclass: u32,
    pub conf: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repository {
    /// Relative to the root (`.` for the root itself).
    pub path: String,
    /// The `origin` remote (else the first remote), credentials removed.
    pub remote: Option<String>,
    /// The full commit of `HEAD`.
    pub revision: Option<String>,
    /// The branch checked out; `None` when detached or unknown.
    pub branch: Option<String>,
    /// Entries of `git status --porcelain` (modified, staged and untracked files); `None`:
    /// unknown.
    pub dirty: Option<usize>,
    /// `<superproject path>: <submodule name>` when a `.gitmodules` declares this path.
    pub submodule: Option<String>,
    /// The kas repo id, when the kas files name this repository.
    pub kas_repo: Option<String>,
    /// The commit the kas files (or their lock file) pin.
    pub pin: Option<String>,
    /// Where the pin is (a kas file or lock file, relative to the root).
    pub pin_from: Option<String>,
    /// The checkout is at the pin; `None` when there is no pin or the revision is unknown.
    pub at_pin: Option<bool>,
    /// The repository is on disk (a kas repo may not be checked out yet).
    pub checked_out: bool,
    /// Names of the layers in it (enabled or not).
    pub layers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KasInfo {
    /// The kas files (relative to the root) and how they were found.
    pub files: Vec<String>,
    pub source: String,
    /// Lock files read (relative to the root).
    pub lock_files: Vec<String>,
    /// kas files without a lock file next to them.
    pub without_lock: Vec<String>,
    /// Layers the kas files name that `bblayers.conf` does not list (paths), and the
    /// reverse (layer names).
    pub not_in_bblayers: Vec<String>,
    pub not_in_kas: Vec<String>,
    pub problems: Vec<String>,
}

// ---- 2. Releases

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Releases {
    pub summary: String,
    pub workspace: String,
    pub source: String,
    pub note: Option<String>,
    /// Every supported release, oldest first, as `codename version[ LTS]`.
    pub supported: Vec<String>,
    pub floor: String,
    /// The matrix columns: the supported releases some layer claims, plus the workspace's.
    pub columns: Vec<String>,
    pub layers: Vec<ReleaseRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseRow {
    pub layer: String,
    pub enabled: bool,
    /// Supported releases it claims, oldest first.
    pub claims: Vec<String>,
    /// Names older than the floor (`dunfell`).
    pub older_than_floor: Vec<String>,
    /// Names that are no known release (a typo, a newer release, or an unexpanded copy).
    pub unknown: Vec<String>,
    pub claims_workspace: bool,
}

// ---- 3. Layer structure

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Structure {
    pub summary: String,
    /// `org/layer-conf`'s checks per enabled layer, in `BBLAYERS` order then check order.
    pub problems: Vec<StructureProblem>,
    /// `BBLAYERS` order rows of `layers show=compat`.
    pub order: Vec<StructureProblem>,
    pub shadowed: Vec<Shadowed>,
    /// bbappends `BBFILES` collects that match no recipe (pointers).
    pub orphan_appends: Vec<String>,
    /// bbappends BitBake does not collect, or collects only through `BBFILES_DYNAMIC`.
    pub appends_not_collected: Vec<AppendStatus>,
    /// `org/append-wildcard-broad` and `org/duplicate-versions` findings, from the lint run.
    pub lint: Vec<FindingOut>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StructureProblem {
    pub layer: String,
    /// `collections`, `compat`, `depends`, `bbfiles`, `bbpath` or `order`.
    pub check: String,
    pub severity: String,
    pub pointer: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shadowed {
    /// `recipe` or `class`.
    pub kind: String,
    pub name: String,
    /// `layer:path` of the file BitBake uses.
    pub wins: String,
    pub shadowed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppendStatus {
    pub append: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingOut {
    pub severity: String,
    pub rule: String,
    pub pointer: String,
    pub message: String,
}

impl FindingOut {
    fn of(f: &Finding) -> FindingOut {
        FindingOut {
            severity: f.severity.as_str().into(),
            rule: f.rule.into(),
            pointer: f.loc.to_string(),
            message: f.message.clone(),
        }
    }
}

// ---- 4. Lint findings

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LintSection {
    pub summary: String,
    /// The command whose findings these are.
    pub command: String,
    pub files: usize,
    pub own_total: SeverityCounts,
    pub upstream_total: SeverityCounts,
    /// The own layers (and the build directory, as `build`): every finding.
    pub own: Vec<LayerFindings>,
    /// Upstream layers: counts per rule and the first [`UPSTREAM_EXAMPLES`] findings.
    pub upstream: Vec<LayerFindings>,
    /// Lint's notes (evaluation failures, configuration problems).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerFindings {
    pub layer: String,
    pub counts: SeverityCounts,
    /// Worst severity first, then most findings, then id.
    pub rules: Vec<RuleFindings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleFindings {
    pub rule: String,
    /// The worst severity among its findings.
    pub severity: String,
    pub count: usize,
    pub findings: Vec<FindingOut>,
    /// Findings not listed.
    pub more: usize,
    /// How to get every finding of this rule, when some are not listed.
    pub command: Option<String>,
}

// ---- 5. Parse and resolution

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseSection {
    pub summary: String,
    /// Statements the parser could not read (`unknown` statements).
    pub unparsed: Listing,
    pub unresolved_includes: Listing,
    pub unresolved_inherits: Listing,
    /// Indexed files BitBake does not collect, per reason.
    pub not_collected: Vec<NotCollected>,
}

/// A list of problems: own layers in full, upstream layers cut per layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub total: usize,
    /// Count per layer, in workspace order.
    pub by_layer: Vec<LayerCount>,
    pub items: Vec<Item>,
    /// Items not listed.
    pub more: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerCount {
    pub layer: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub pointer: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotCollected {
    pub reason: String,
    pub files: Listing,
}

// ---- 6. Not checked

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotChecked {
    pub summary: String,
    /// `found: <mode>` or `not found: <why>`.
    pub build_environment: String,
    /// Files with anonymous python (`python () { ... }`, `python __anonymous`), by kind.
    pub anonymous_python: BTreeMap<String, usize>,
    /// Layers on disk the audit did not analyse (not enabled).
    pub layers_not_analysed: Vec<String>,
    /// What static analysis never does.
    pub limits: Vec<String>,
}

// ---------------------------------------------------------------------------------------
// Building

/// Builds the report for the workspace of `ctx`. Refreshes the index, calls the `layers`
/// tool's `compat` and `appends` views, then assembles the rest with the index locked.
pub async fn build(ctx: &Ctx, opts: &Options) -> Result<Audit> {
    let root = canonical_dir(&opts.root)?;
    opts.say("refreshing the index");
    {
        let mut index = ctx.index.lock().await;
        index.refresh(&ctx.workspace)?;
    }
    opts.say("reading the layers views (compat, appends)");
    let compat = view(ctx, layers_tool::Show::Compat).await;
    let appends = view(ctx, layers_tool::Show::Appends).await;
    let index = ctx.index.lock().await;
    let views = Views { compat, appends };
    assemble(&ctx.workspace, &index, &ctx.config, &root, opts, &views)
}

/// What the `layers` tool's views returned (or why they failed).
struct Views {
    compat: std::result::Result<JsonTable, String>,
    appends: std::result::Result<JsonTable, String>,
}

async fn view(ctx: &Ctx, show: layers_tool::Show) -> std::result::Result<JsonTable, String> {
    let out = layers_tool::run(
        ctx,
        layers_tool::Args {
            show,
            budget_tokens: Some(usize::MAX / 8),
            format: Some(Format::Json),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

/// Everything after the views, on a refreshed index.
fn assemble(
    ws: &Workspace,
    index: &Index,
    cfg: &Config,
    root: &Path,
    opts: &Options,
    views: &Views,
) -> Result<Audit> {
    let paths = Paths::new(root);
    opts.say("looking for layers under the root and reading git facts");
    let on_disk = find_layers(root, ws);
    let mut git = GitCache::default();
    let inventory = inventory(ws, index, root, &paths, &on_disk, &mut git)?;
    let releases = releases(ws, &inventory);

    let n_layers = ws.layers.len();
    opts.say(&format!(
        "linting every layer ({n_layers} layers, as `sous-chef lint --all`; about 15 s on a large workspace)"
    ));
    let started = Instant::now();
    let report = lint::run(
        ws,
        index,
        cfg,
        &lint::Target::Workspace,
        &lint::Options {
            all: true,
            ..lint::Options::default()
        },
    )?;
    opts.say(&format!(
        "lint done: {} findings in {} files ({:.1} s)",
        report.findings.len(),
        report.files,
        started.elapsed().as_secs_f64()
    ));
    let own: BTreeSet<String> = ws
        .layers
        .iter()
        .filter(|l| l.writable)
        .map(|l| l.name.clone())
        .chain(std::iter::once(BUILD_LAYER.to_string()))
        .collect();
    let structure = structure(ws, index, views, &report, &own)?;
    let lint = lint_section(ws, &report, &own);
    opts.say("collecting parse and resolution problems");
    let parse = parse_section(ws, index, &own)?;
    let not_checked = not_checked(ws, index, cfg, &inventory)?;
    let summary = summary(
        ws,
        index,
        &paths,
        &SummaryInput {
            inv: &inventory,
            st: &structure,
            lint: &lint,
            findings: &report.findings,
            own: &own,
        },
    )?;

    let generated = opts.generated.clone().unwrap_or_else(now_utc);
    let mut audit = Audit {
        schema: SCHEMA.into(),
        generated,
        sous_chef_version: env!("CARGO_PKG_VERSION").into(),
        summary,
        inventory,
        releases,
        structure,
        lint,
        parse,
        not_checked,
    };
    scrub_audit(&mut audit, &paths);
    Ok(audit)
}

// ---- paths

/// Shows paths relative to the root.
struct Paths {
    root: PathBuf,
}

impl Paths {
    fn new(root: &Path) -> Paths {
        Paths {
            root: root.to_path_buf(),
        }
    }

    /// `p` relative to the root (`.` for the root), or `(outside ROOT)/<last component>`.
    fn show(&self, p: &Path) -> String {
        let canon = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        match canon.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => format!(
                "{OUTSIDE_ROOT}/{}",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
        }
    }

    /// Replaces the root's absolute path in free text (messages, notes) with a relative
    /// form, so no absolute path inside the root reaches the report.
    fn scrub(&self, text: &str) -> String {
        let r = self.root.display().to_string();
        if r.len() <= 1 || !text.contains(&r) {
            return text.to_string();
        }
        text.replace(&format!("{r}/"), "").replace(&r, ".")
    }
}

fn canonical_dir(p: &Path) -> Result<PathBuf> {
    let c = fs::canonicalize(p).map_err(|e| Error::Invalid {
        msg: format!(
            "audit root {}: {e}; give an existing directory",
            p.display()
        ),
    })?;
    if !c.is_dir() {
        return Err(Error::Invalid {
            msg: format!("audit root {} is not a directory", p.display()),
        });
    }
    Ok(c)
}

// ---- layer discovery under the root

/// A directory with `conf/layer.conf` found under the root, or an enabled layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundLayer {
    /// Canonical.
    pub root: PathBuf,
    /// The workspace layer it is, if enabled.
    pub enabled: Option<String>,
}

/// Whether the walk skips a directory: hidden ones (`.git`), build output (`tmp*`,
/// `sstate-cache`, `downloads`) and build directories (a `conf/bblayers.conf` or kas's
/// `CACHEDIR.TAG`).
fn skip_dir(name: &str, path: &Path) -> bool {
    name.starts_with('.')
        || name.starts_with("tmp")
        || name == "sstate-cache"
        || name == "downloads"
        || path.join("conf").join("bblayers.conf").is_file()
        || path.join("CACHEDIR.TAG").is_file()
}

/// Every directory at most `depth` levels below `start` (inclusive) with
/// `conf/layer.conf`, skipping [`skip_dir`] directories and not following symlinks.
fn walk_layers(start: &Path, depth: usize, out: &mut BTreeSet<PathBuf>) {
    let mut stack = vec![(start.to_path_buf(), 0usize)];
    while let Some((dir, d)) = stack.pop() {
        if dir.join("conf").join("layer.conf").is_file() {
            out.insert(dir.clone());
        }
        if d >= depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if skip_dir(&name, &path) {
                continue;
            }
            stack.push((path, d + 1));
        }
    }
}

/// Every layer under `root` (see [`SCAN_DEPTH`]; submodule paths declared in a
/// `.gitmodules` and kas repositories are walked from their own directory too), plus every
/// enabled layer of `ws` wherever it is. Sorted: enabled layers in workspace order first,
/// then the others by path. Layers found outside the root (through a symlink) are dropped.
pub fn find_layers(root: &Path, ws: &Workspace) -> Vec<FoundLayer> {
    let mut found = BTreeSet::new();
    walk_layers(root, SCAN_DEPTH, &mut found);
    for sub in gitmodules_paths(root) {
        if sub.is_dir() {
            walk_layers(&sub, SCAN_DEPTH, &mut found);
        }
    }
    if let Some(k) = &ws.kas {
        for r in &k.repos {
            if r.path.starts_with(root) && r.path.is_dir() {
                walk_layers(&r.path, SCAN_DEPTH, &mut found);
            }
        }
    }
    let canon: BTreeSet<PathBuf> = found
        .into_iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .filter(|p| p.starts_with(root))
        .collect();
    let mut out: Vec<FoundLayer> = ws
        .layers
        .iter()
        .map(|l| FoundLayer {
            root: l.root.clone(),
            enabled: Some(l.name.clone()),
        })
        .collect();
    for p in canon {
        if !ws.layers.iter().any(|l| l.root == p) {
            out.push(FoundLayer {
                root: p,
                enabled: None,
            });
        }
    }
    out
}

/// Submodule paths (absolute) declared by `.gitmodules` files of `root` and of the
/// directories one level below it that hold one, through `git config -f .gitmodules`.
fn gitmodules_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (top, subs) in gitmodules_under(root) {
        for s in subs {
            out.push(top.join(s.path));
        }
    }
    out
}

/// One `.gitmodules` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Submodule {
    name: String,
    path: String,
}

/// The `.gitmodules` of `root` (and of its direct subdirectories), by directory.
fn gitmodules_under(root: &Path) -> Vec<(PathBuf, Vec<Submodule>)> {
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(entries) = fs::read_dir(root) {
        let mut subs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|p| p.join(".gitmodules").is_file())
            .collect();
        subs.sort();
        dirs.extend(subs);
    }
    dirs.into_iter()
        .filter(|d| d.join(".gitmodules").is_file())
        .map(|d| {
            let subs = read_gitmodules(&d);
            (d, subs)
        })
        .collect()
}

/// `git config -f .gitmodules --get-regexp ^submodule\..*\.path$` in `dir`.
fn read_gitmodules(dir: &Path) -> Vec<Submodule> {
    let Some(out) = git(
        dir,
        &[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ],
    ) else {
        return Vec::new();
    };
    let mut subs: Vec<Submodule> = out
        .lines()
        .filter_map(|l| {
            let (key, value) = l.split_once(' ')?;
            let name = key.strip_prefix("submodule.")?.strip_suffix(".path")?;
            let path = value.trim().trim_end_matches('/');
            // Never leave the directory: a path with `..` or an absolute one is ignored.
            let ok = !path.is_empty()
                && Path::new(path)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)));
            ok.then(|| Submodule {
                name: name.to_string(),
                path: path.to_string(),
            })
        })
        .collect();
    subs.sort_by(|a, b| a.path.cmp(&b.path));
    subs
}

// ---- git

const GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
];

/// Runs a read-only git command in `dir`: argv only (never a shell), stdin closed, no
/// prompts, no optional locks, no fsmonitor, the `GIT_DIR` family removed, killed after
/// [`GIT_TIMEOUT`]. Its stdout when it exits 0; `None` on any failure (git missing, not a
/// repository, a timeout), which the report shows as unknown.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for v in GIT_ENV {
        cmd.env_remove(v);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < GIT_TIMEOUT => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()?;
    status
        .filter(|s| s.success())
        .map(|_| String::from_utf8_lossy(&out).into_owned())
}

/// A remote URL with any userinfo (`user:token@`) removed: `https://user:token@host/x`
/// becomes `https://host/x`. scp-style addresses (`git@host:path`) name a user, never a
/// secret, and are kept.
pub fn strip_credentials(url: &str) -> String {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://{}{tail}", &authority[at + 1..]),
        None => url.to_string(),
    }
}

/// What git says about one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GitFacts {
    remote: Option<String>,
    revision: Option<String>,
    branch: Option<String>,
    dirty: Option<usize>,
}

/// Repository roots and facts, each asked once.
#[derive(Default)]
struct GitCache {
    toplevel: HashMap<PathBuf, Option<PathBuf>>,
    facts: HashMap<PathBuf, GitFacts>,
}

impl GitCache {
    /// The repository `dir` is in, when it is inside `root` (an enclosing repository
    /// outside the root is not reported: its facts are not this repository's).
    fn toplevel(&mut self, dir: &Path, root: &Path) -> Option<PathBuf> {
        if let Some(t) = self.toplevel.get(dir) {
            return t.clone();
        }
        let t = git(dir, &["rev-parse", "--show-toplevel"])
            .map(|s| PathBuf::from(s.trim()))
            .and_then(|p| fs::canonicalize(p).ok())
            .filter(|p| p.starts_with(root));
        self.toplevel.insert(dir.to_path_buf(), t.clone());
        t
    }

    fn facts(&mut self, top: &Path) -> GitFacts {
        if let Some(f) = self.facts.get(top) {
            return f.clone();
        }
        let revision = git(top, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let branch = git(top, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "HEAD");
        let remote = git(top, &["remote", "get-url", "origin"])
            .or_else(|| {
                let first = git(top, &["remote"])?
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())?
                    .to_string();
                git(top, &["remote", "get-url", &first])
            })
            .map(|s| strip_credentials(&s))
            .filter(|s| !s.is_empty());
        let dirty = git(
            top,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count());
        let f = GitFacts {
            remote,
            revision,
            branch,
            dirty,
        };
        self.facts.insert(top.to_path_buf(), f.clone());
        f
    }
}

/// Whether a checkout at `revision` is at the kas pin `pin` (a full or abbreviated sha).
pub fn at_pin(revision: &str, pin: &str) -> bool {
    let (r, p) = (revision.trim(), pin.trim());
    !r.is_empty() && !p.is_empty() && (r.eq_ignore_ascii_case(p) || r.starts_with(p))
}

// ---- 1. Inventory

fn inventory(
    ws: &Workspace,
    index: &Index,
    root: &Path,
    paths: &Paths,
    on_disk: &[FoundLayer],
    git: &mut GitCache,
) -> Result<Inventory> {
    let stats: HashMap<String, query::LayerStats> = query::layer_stats(index)?
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    let mut notes = Vec::new();
    let mut layers_out = Vec::new();
    let mut repo_layers: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut not_enabled_names: BTreeSet<String> = BTreeSet::new();
    for (i, l) in ws.layers.iter().enumerate() {
        let top = git.toplevel(&l.root, root);
        if let Some(t) = &top {
            repo_layers
                .entry(t.clone())
                .or_default()
                .push(l.name.clone());
        }
        let files = stats.get(&l.name).map(|s| FileCounts {
            bb: s.recipes,
            bbappend: s.appends,
            inc: s.includes,
            bbclass: s.classes,
            conf: s.confs,
        });
        layers_out.push(LayerEntry {
            name: l.name.clone(),
            path: paths.show(&l.root),
            enabled: true,
            position: Some(i + 1),
            role: if l.writable { "own" } else { "upstream" }.into(),
            collections: l.collections.clone(),
            priority: Some(l.priority),
            compat: l.compat.clone(),
            depends: l.depends.clone(),
            repository: top.as_deref().map(|t| paths.show(t)),
            files,
            note: None,
        });
    }
    for f in on_disk.iter().filter(|f| f.enabled.is_none()) {
        let dir_name = f
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = paths.show(&f.root);
        // The directory name, unless an enabled layer or another one has it.
        let name = if ws.layer(&dir_name).is_some() || not_enabled_names.contains(&dir_name) {
            path.clone()
        } else {
            dir_name
        };
        not_enabled_names.insert(name.clone());
        let top = git.toplevel(&f.root, root);
        if let Some(t) = &top {
            repo_layers.entry(t.clone()).or_default().push(name.clone());
        }
        // Cheap and safe: its own conf/layer.conf, read alone (only files inside the layer).
        let (collections, priority, compat, depends, note) =
            match layers::read_layer_confs(std::slice::from_ref(&f.root), None) {
                Ok(mut v) if !v.is_empty() => {
                    let l = v.remove(0);
                    (l.collections, Some(l.priority), l.compat, l.depends, None)
                }
                Ok(_) => (vec![], None, vec![], vec![], None),
                Err(e) => (
                    vec![],
                    None,
                    vec![],
                    vec![],
                    Some(format!("conf/layer.conf could not be read: {e}")),
                ),
            };
        layers_out.push(LayerEntry {
            name,
            path,
            enabled: false,
            position: None,
            role: "not enabled".into(),
            collections,
            priority,
            compat,
            depends,
            repository: top.as_deref().map(|t| paths.show(t)),
            files: None,
            note,
        });
    }

    // Repositories: those holding a layer, the root's, and the kas repos.
    let mut tops: BTreeSet<PathBuf> = repo_layers.keys().cloned().collect();
    if let Some(t) = git.toplevel(root, root) {
        tops.insert(t);
    }
    let mut kas_by_path: BTreeMap<PathBuf, &kas::KasRepo> = BTreeMap::new();
    if let Some(k) = &ws.kas {
        for r in &k.repos {
            let p = fs::canonicalize(&r.path).unwrap_or_else(|_| r.path.clone());
            // A repo inside another repository's checkout (the own repo is the project
            // root): attach it to the repository git finds there.
            let key = if p.is_dir() {
                git.toplevel(&p, root).unwrap_or(p)
            } else {
                p
            };
            kas_by_path.entry(key).or_insert(r);
        }
        tops.extend(kas_by_path.keys().filter(|p| p.starts_with(root)).cloned());
    }
    let submodules = submodule_index(&tops, root);
    let mut repositories = Vec::new();
    for top in &tops {
        let checked_out = top.is_dir();
        let facts = if checked_out && git.toplevel(top, root).is_some() {
            git.facts(top)
        } else {
            GitFacts::default()
        };
        let kas_repo = kas_by_path.get(top);
        let (pin, pin_from) = match (kas_repo, &ws.kas) {
            (Some(r), Some(k)) => match &r.commit {
                Some(c) => (Some(c.clone()), Some(pin_source(k, r, paths))),
                None => (None, None),
            },
            _ => (None, None),
        };
        let at = match (&pin, &facts.revision) {
            (Some(p), Some(r)) => Some(at_pin(r, p)),
            _ => None,
        };
        let mut names = repo_layers.get(top).cloned().unwrap_or_default();
        names.sort();
        repositories.push(Repository {
            path: paths.show(top),
            remote: facts.remote.or_else(|| {
                kas_repo
                    .and_then(|r| r.url.as_deref())
                    .map(strip_credentials)
            }),
            revision: facts.revision,
            branch: facts.branch,
            dirty: facts.dirty,
            submodule: submodules.get(top).cloned(),
            kas_repo: kas_repo.map(|r| r.id.clone()),
            pin,
            pin_from,
            at_pin: at,
            checked_out,
            layers: names,
        });
    }
    repositories.sort_by(|a, b| a.path.cmp(&b.path));

    // Declared submodules that are not checked out.
    for (top, subs) in gitmodules_under(root) {
        for s in subs {
            let dir = top.join(&s.path);
            let empty = fs::read_dir(&dir).map_or(true, |mut d| d.next().is_none());
            if empty {
                notes.push(format!(
                    "submodule {} ({}) is declared in {} but not checked out; its layers, if any, are not listed (git submodule update --init)",
                    s.name,
                    paths.show(&dir),
                    paths.show(&top.join(".gitmodules"))
                ));
            }
        }
    }
    let unknown_git = repositories.iter().all(|r| r.revision.is_none());
    if repositories.is_empty() || unknown_git {
        notes.push(
            "git facts are unknown: no git repository was found inside the root (or git is not available); a repository enclosing the root is not reported".into(),
        );
    }
    let n_enabled = ws.layers.len();
    let n_not = layers_out.len() - n_enabled;
    if n_not > 0 {
        notes.push(format!(
            "{n_not} layer(s) present but not enabled: not in the index, so their files are not counted, linted or resolved; collection, priority, compat and depends come from their conf/layer.conf alone"
        ));
    }

    let kas = ws.kas.as_ref().map(|k| kas_info(k, paths));
    let summary = format!(
        "{} layers enabled ({} own, {} upstream), {} present but not enabled; {} git repositories{}",
        n_enabled,
        ws.layers.iter().filter(|l| l.writable).count(),
        ws.layers.iter().filter(|l| !l.writable).count(),
        n_not,
        repositories.len(),
        dirty_summary(&repositories)
    );
    Ok(Inventory {
        summary,
        layers: layers_out,
        repositories,
        kas,
        notes,
    })
}

fn dirty_summary(repos: &[Repository]) -> String {
    let dirty = repos
        .iter()
        .filter(|r| r.dirty.is_some_and(|n| n > 0))
        .count();
    let off_pin = repos.iter().filter(|r| r.at_pin == Some(false)).count();
    let mut parts = Vec::new();
    if dirty > 0 {
        parts.push(format!("{dirty} with uncommitted changes"));
    }
    if off_pin > 0 {
        parts.push(format!("{off_pin} not at their kas pin"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// `<superproject>: <name>` per checked-out submodule directory (canonical).
fn submodule_index(tops: &BTreeSet<PathBuf>, root: &Path) -> HashMap<PathBuf, String> {
    let paths = Paths::new(root);
    let mut supers: BTreeSet<PathBuf> = tops.clone();
    supers.insert(root.to_path_buf());
    let mut out = HashMap::new();
    for sup in supers {
        if !sup.join(".gitmodules").is_file() {
            continue;
        }
        for s in read_gitmodules(&sup) {
            let dir = sup.join(&s.path);
            if let Ok(c) = fs::canonicalize(&dir) {
                out.insert(c, format!("{}: {}", paths.show(&sup), s.name));
            }
        }
    }
    out
}

/// The lock file that pins repo `r`, else the kas file that names it.
fn pin_source(k: &kas::KasProject, r: &kas::KasRepo, paths: &Paths) -> String {
    for lock in &k.lock_files {
        if lock_pins(lock, &r.id) {
            return paths.show(lock);
        }
    }
    r.loc
        .as_ref()
        .map(|l| paths.show(&l.file))
        .unwrap_or_else(|| "kas files".into())
}

/// Whether a kas lock file sets `overrides: repos: <id>: commit:`.
fn lock_pins(lock: &Path, id: &str) -> bool {
    let Ok(text) = fs::read_to_string(lock) else {
        return false;
    };
    let Ok(docs) = yaml_rust2::YamlLoader::load_from_str(&text) else {
        return false;
    };
    docs.iter()
        .any(|d| d["overrides"]["repos"][id]["commit"].as_str().is_some())
}

fn kas_info(k: &kas::KasProject, paths: &Paths) -> KasInfo {
    let without_lock = k
        .files
        .iter()
        .filter(|f| !kas::lock_file_for(f).is_file())
        .map(|f| paths.show(f))
        .collect();
    KasInfo {
        files: k.files.iter().map(|f| paths.show(f)).collect(),
        source: k.source.describe(),
        lock_files: k.lock_files.iter().map(|f| paths.show(f)).collect(),
        without_lock,
        not_in_bblayers: k.not_in_bblayers.iter().map(|p| paths.show(p)).collect(),
        not_in_kas: k.not_in_kas.clone(),
        problems: k.problems.clone(),
    }
}

// ---- 2. Releases

fn releases(ws: &Workspace, inv: &Inventory) -> Releases {
    let wsr = ws.release.release;
    let mut claimed: BTreeSet<Release> = BTreeSet::new();
    claimed.insert(wsr);
    let mut rows = Vec::new();
    for l in &inv.layers {
        let mut claims: Vec<Release> = Vec::new();
        let mut older = Vec::new();
        let mut unknown = Vec::new();
        for c in &l.compat {
            match Release::from_codename(c) {
                Some(r) => claims.push(r),
                None if Release::is_older_than_floor(c) => older.push(c.clone()),
                None => unknown.push(c.clone()),
            }
        }
        claims.sort();
        claims.dedup();
        claimed.extend(claims.iter().copied());
        rows.push(ReleaseRow {
            layer: l.name.clone(),
            enabled: l.enabled,
            claims_workspace: claims.contains(&wsr),
            claims: claims.iter().map(|r| r.codename().to_string()).collect(),
            older_than_floor: older,
            unknown,
        });
    }
    let missing: Vec<&ReleaseRow> = rows
        .iter()
        .filter(|r| r.enabled && !r.claims_workspace)
        .collect();
    let missing_off = rows
        .iter()
        .filter(|r| !r.enabled && !r.claims_workspace)
        .count();
    let summary = if missing.is_empty() {
        format!(
            "workspace release {wsr} ({}); every enabled layer claims it{}",
            ws.release.source.as_str(),
            if missing_off > 0 {
                format!("; {missing_off} layer(s) present but not enabled do not")
            } else {
                String::new()
            }
        )
    } else {
        format!(
            "workspace release {wsr} ({}); {} enabled layer(s) do not claim it: {}",
            ws.release.source.as_str(),
            missing.len(),
            missing
                .iter()
                .map(|r| r.layer.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Releases {
        summary,
        workspace: wsr.codename().into(),
        source: ws.release.source.as_str().into(),
        note: ws.release.note.clone(),
        supported: Release::ALL
            .iter()
            .map(|r| {
                format!(
                    "{} {}{}",
                    r.codename(),
                    r.version(),
                    if r.is_lts() { " LTS" } else { "" }
                )
            })
            .collect(),
        floor: Release::FLOOR.codename().into(),
        columns: claimed.iter().map(|r| r.codename().to_string()).collect(),
        layers: rows,
    }
}

// ---- 3. Layer structure

/// Rules whose findings section 3 lists (they are about the layer structure).
const STRUCTURE_RULES: &[&str] = &["org/append-wildcard-broad", "org/duplicate-versions"];

fn structure(
    ws: &Workspace,
    index: &Index,
    views: &Views,
    report: &lint::Report,
    own: &BTreeSet<String>,
) -> Result<Structure> {
    let mut notes = Vec::new();
    let mut problems = Vec::new();
    for l in &ws.layers {
        for p in org_layer_conf::layer_problems(ws, index, &l.name) {
            problems.push(problem_out(&l.name, &p));
        }
    }
    let mut order = Vec::new();
    match &views.compat {
        Ok(t) => {
            for row in &t.rows {
                let cell = |i: usize| row.get(i).map(|c| unescape_cell(c)).unwrap_or_default();
                if cell(1) == "order" {
                    order.push(StructureProblem {
                        layer: cell(0),
                        check: "order".into(),
                        severity: cell(2),
                        pointer: cell(4),
                        message: cell(3),
                    });
                }
            }
            if ws.build_dir.is_none() {
                notes.push("no build directory, so no BBLAYERS order to check".into());
            }
        }
        Err(e) => notes.push(format!(
            "BBLAYERS order not checked: `layers show=compat` failed: {e}"
        )),
    }
    let mut shadowed = Vec::new();
    for s in query::shadowed(index, ws)? {
        let mut files = s
            .files
            .iter()
            .map(|f| format!("{}:{}", f.layer, f.path.display()));
        let Some(wins) = files.next() else { continue };
        shadowed.push(Shadowed {
            kind: s.kind,
            name: s.name,
            wins,
            shadowed: files.collect(),
        });
    }
    shadowed.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    let orphan_appends: Vec<String> = query::orphan_appends(index)?
        .iter()
        .map(|f| format!("{}:{}", f.layer, f.path.display()))
        .collect();
    let mut appends_not_collected = Vec::new();
    match &views.appends {
        Ok(t) => {
            for row in &t.rows {
                let cell = |i: usize| row.get(i).map(|c| unescape_cell(c)).unwrap_or_default();
                let status = cell(3);
                if status != "applies" && status != "no-match" {
                    appends_not_collected.push(AppendStatus {
                        append: cell(0),
                        status,
                    });
                }
            }
            appends_not_collected.sort_by(|a, b| a.append.cmp(&b.append));
            appends_not_collected.dedup();
        }
        Err(e) => notes.push(format!(
            "bbappends BitBake does not collect not listed: `layers show=appends` failed: {e}"
        )),
    }
    let lint: Vec<FindingOut> = report
        .findings
        .iter()
        .filter(|f| STRUCTURE_RULES.contains(&f.rule))
        .map(FindingOut::of)
        .collect();

    let mut sev = SeverityCounts::default();
    for p in problems.iter().chain(&order) {
        sev.add(parse_severity(&p.severity));
    }
    let own_orphans = orphan_appends
        .iter()
        .filter(|p| own.contains(p.split(':').next().unwrap_or_default()))
        .count();
    let summary = format!(
        "{} layer.conf and order problems ({}); {} shadowed recipe or class names; {} bbappends match no recipe ({} in own layers); {} bbappends not collected; {} over-broad bbappend and duplicate version findings",
        problems.len() + order.len(),
        sev.describe(),
        shadowed.len(),
        orphan_appends.len(),
        own_orphans,
        appends_not_collected.len(),
        lint.len()
    );
    Ok(Structure {
        summary,
        problems,
        order,
        shadowed,
        orphan_appends,
        appends_not_collected,
        lint,
        notes,
    })
}

fn problem_out(layer: &str, p: &LayerProblem) -> StructureProblem {
    StructureProblem {
        layer: layer.into(),
        check: p.check.group().into(),
        severity: p.severity.as_str().into(),
        pointer: p.loc.to_string(),
        message: p.message.clone(),
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    }
}

// ---- 4. Lint findings

/// The command an upstream rule's full list comes from.
fn rule_command(rule: &str) -> String {
    format!("sous-chef lint --all --rule {rule}")
}

fn lint_section(ws: &Workspace, report: &lint::Report, own: &BTreeSet<String>) -> LintSection {
    // Layer order: workspace order, the build directory first.
    let mut order: Vec<String> = vec![BUILD_LAYER.into()];
    order.extend(ws.layers.iter().map(|l| l.name.clone()));
    let mut by_layer: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        by_layer.entry(f.loc.layer.as_str()).or_default().push(f);
    }
    for l in by_layer.keys() {
        if !order.iter().any(|o| o == l) {
            order.push((*l).to_string());
        }
    }
    let mut own_out = Vec::new();
    let mut up_out = Vec::new();
    let mut own_total = SeverityCounts::default();
    let mut up_total = SeverityCounts::default();
    for layer in &order {
        let Some(fs) = by_layer.get(layer.as_str()) else {
            continue;
        };
        let is_own = own.contains(layer);
        let mut counts = SeverityCounts::default();
        let mut by_rule: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
        for f in fs {
            counts.add(f.severity);
            by_rule.entry(f.rule).or_default().push(f);
        }
        let mut rules: Vec<RuleFindings> = by_rule
            .into_iter()
            .map(|(rule, fs)| {
                let worst = fs
                    .iter()
                    .map(|f| f.severity)
                    .max()
                    .unwrap_or(Severity::Info);
                let shown = if is_own { fs.len() } else { UPSTREAM_EXAMPLES };
                let more = fs.len().saturating_sub(shown);
                RuleFindings {
                    rule: rule.into(),
                    severity: worst.as_str().into(),
                    count: fs.len(),
                    findings: fs.iter().take(shown).map(|f| FindingOut::of(f)).collect(),
                    more,
                    command: (more > 0).then(|| rule_command(rule)),
                }
            })
            .collect();
        rules.sort_by(|a, b| {
            parse_severity(&b.severity)
                .cmp(&parse_severity(&a.severity))
                .then(b.count.cmp(&a.count))
                .then(a.rule.cmp(&b.rule))
        });
        let entry = LayerFindings {
            layer: layer.clone(),
            counts,
            rules,
        };
        if is_own {
            own_total.add_all(&counts);
            own_out.push(entry);
        } else {
            up_total.add_all(&counts);
            up_out.push(entry);
        }
    }
    let summary = format!(
        "{} findings in {} files: own layers and build directory {} ({}); upstream layers {} ({})",
        report.findings.len(),
        report.files,
        own_total.total(),
        own_total.describe(),
        up_total.total(),
        up_total.describe()
    );
    LintSection {
        summary,
        command: "sous-chef lint --all".into(),
        files: report.files,
        own_total,
        upstream_total: up_total,
        own: own_out,
        upstream: up_out,
        notes: report.notes.clone(),
    }
}

impl SeverityCounts {
    fn add_all(&mut self, o: &SeverityCounts) {
        self.error += o.error;
        self.warning += o.warning;
        self.info += o.info;
    }
}

// ---- 5. Parse and resolution problems

/// Builds a [`Listing`] from `(layer, item)` pairs: own layers in full, the others cut to
/// [`UPSTREAM_PARSE_EXAMPLES`] per layer. Layers in workspace order (the build directory
/// first).
fn listing(ws: &Workspace, own: &BTreeSet<String>, items: Vec<(String, Item)>) -> Listing {
    let mut order: Vec<String> = vec![BUILD_LAYER.into()];
    order.extend(ws.layers.iter().map(|l| l.name.clone()));
    let mut by: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    for (l, i) in items {
        by.entry(l).or_default().push(i);
    }
    for l in by.keys() {
        if !order.contains(l) {
            order.push(l.clone());
        }
    }
    let mut out = Listing::default();
    for l in order {
        let Some(mut v) = by.remove(&l) else { continue };
        v.sort_by(|a, b| cmp_pointer(&a.pointer, &b.pointer).then(a.text.cmp(&b.text)));
        out.total += v.len();
        out.by_layer.push(LayerCount {
            layer: l.clone(),
            count: v.len(),
        });
        let keep = if own.contains(&l) {
            v.len()
        } else {
            UPSTREAM_PARSE_EXAMPLES
        };
        out.more += v.len().saturating_sub(keep);
        out.items.extend(v.into_iter().take(keep));
    }
    out
}

/// Pointers sorted by layer, path, then line numerically.
fn cmp_pointer(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |p: &str| -> (String, u64) {
        match p.rsplit_once(':') {
            Some((head, lines)) => {
                let first = lines.split('-').next().unwrap_or_default();
                match first.parse::<u64>() {
                    Ok(n) => (head.to_string(), n),
                    Err(_) => (p.to_string(), 0),
                }
            }
            None => (p.to_string(), 0),
        }
    };
    key(a).cmp(&key(b))
}

fn parse_section(ws: &Workspace, index: &Index, own: &BTreeSet<String>) -> Result<ParseSection> {
    let unparsed_rows = query::unknown_statements(index)?;
    let unparsed = listing(
        ws,
        own,
        unparsed_rows
            .into_iter()
            .map(|s| {
                let text = if s.snippet.split_whitespace().next() == Some("addpylib") {
                    format!(
                        "{} (not modelled by sous-chef; BitBake reads it)",
                        s.snippet
                    )
                } else {
                    s.snippet.clone()
                };
                (
                    s.loc.layer.clone(),
                    Item {
                        pointer: s.loc.to_string(),
                        text,
                    },
                )
            })
            .collect(),
    );
    // Data files are not metadata: their edges are not BitBake's.
    let metadata: BTreeSet<i64> = query::all_files(index)?.iter().map(|f| f.id).collect();
    let edge_item = |e: &query::EdgeRow| {
        let why = if e.target_text.contains("${") {
            "needs evaluation"
        } else {
            "not found"
        };
        (
            e.src.layer.clone(),
            Item {
                pointer: e.via().to_string(),
                text: format!("{} {}: {why}", e.kind, e.target_text),
            },
        )
    };
    let includes = listing(
        ws,
        own,
        query::unresolved_include_edges(index)?
            .iter()
            .filter(|e| metadata.contains(&e.src.id))
            .map(edge_item)
            .collect(),
    );
    let inherits = listing(
        ws,
        own,
        query::unresolved_inherit_edges(index)?
            .iter()
            .filter(|e| metadata.contains(&e.src.id))
            .map(edge_item)
            .collect(),
    );
    let mut reasons: BTreeMap<String, Vec<(String, Item)>> = BTreeMap::new();
    for f in query::all_files_in(index, FileSet::ALL)? {
        if let Some(reason) = query::not_collected(index, f.id)? {
            reasons.entry(reason).or_default().push((
                f.layer.clone(),
                Item {
                    pointer: format!("{}:{}", f.layer, f.path.display()),
                    text: f.kind.as_str().into(),
                },
            ));
        }
    }
    let not_collected: Vec<NotCollected> = reasons
        .into_iter()
        .map(|(reason, items)| NotCollected {
            reason,
            files: listing(ws, own, items),
        })
        .collect();
    let nc_total: usize = not_collected.iter().map(|n| n.files.total).sum();
    let summary = format!(
        "{} unparsed statements, {} unresolved include/require, {} unresolved inherits, {} indexed files BitBake does not collect",
        unparsed.total, includes.total, inherits.total, nc_total
    );
    Ok(ParseSection {
        summary,
        unparsed,
        unresolved_includes: includes,
        unresolved_inherits: inherits,
        not_collected,
    })
}

// ---- 6. Not checked

/// What static analysis never does (docs/mcp-tools.md, docs/overview.md "Non-goals").
const LIMITS: &[&str] = &[
    "Python is never run: `${@...}` expansions, anonymous python (`python () { ... }`) and `d.setVar` / `d.appendVar` in classes are not evaluated; values that depend on them are marked `depends on python, not evaluated` by `var`, `recipe` and the other evaluated tools, and `var authoritative=true` asks BitBake (needs a build environment).",
    "How many values depend on python is not counted here: that needs evaluating every recipe (`var` and `recipe` mark them per recipe).",
    "Recipe-scope lint rules evaluate each recipe for the default MACHINE and DISTRO only; other machines and distros are not checked.",
    "OVERRIDES entries that come from python are not known, so overrides on them may be reported as dead (`override/dead`).",
    "Tasks added or deleted by anonymous python, `do_x[deps]` set directly and dependencies on other recipes' tasks are not modelled (`recipe show=tasks`).",
    "bbappends collected only through BBFILES_DYNAMIC are matched by file name but not applied by the evaluation.",
    "Nothing was fetched, built or parsed by BitBake: SRC_URI reachability, checksums, licenses and QA checks are BitBake's.",
    "Layers present but not enabled are not indexed: only their conf/layer.conf was read.",
];

fn not_checked(ws: &Workspace, index: &Index, cfg: &Config, inv: &Inventory) -> Result<NotChecked> {
    let state = crate::bitbake::environment_state(cfg, ws);
    let build_environment = match &state.env {
        Some(env) => format!("found: {} mode", env.mode()),
        None => format!(
            "not found: {}",
            state.problem.as_deref().unwrap_or("no build environment")
        ),
    };
    let mut anonymous_python: BTreeMap<String, usize> = BTreeMap::new();
    for f in query::anonymous_python_files(index)? {
        *anonymous_python.entry(f.kind.as_str().into()).or_default() += 1;
    }
    let layers_not_analysed: Vec<String> = inv
        .layers
        .iter()
        .filter(|l| !l.enabled)
        .map(|l| l.path.clone())
        .collect();
    let recipes = anonymous_python
        .get(FileKind::Recipe.as_str())
        .copied()
        .unwrap_or(0);
    let classes = anonymous_python
        .get(FileKind::Class.as_str())
        .copied()
        .unwrap_or(0);
    let summary = format!(
        "build environment {}; {recipes} recipes and {classes} classes have anonymous python, which is not run; {} layers present but not enabled were not analysed",
        if state.env.is_some() {
            "found"
        } else {
            "not found"
        },
        layers_not_analysed.len()
    );
    Ok(NotChecked {
        summary,
        build_environment,
        anonymous_python,
        layers_not_analysed,
        limits: LIMITS.iter().map(|s| s.to_string()).collect(),
    })
}

// ---- Summary

/// The stated rule [`attention`] follows.
pub const ATTENTION_RULE: &str = "errors in own layers (by rule, most findings first); then layer structure errors outside own layers (org/layer-conf errors, bbappends that match no recipe: BitBake stops on both); then warnings in own layers (by rule, most findings first); if that gives fewer than 3, errors in upstream layers by rule, then layer structure warnings; at most 10";

/// What [`summary`] reads from the other sections.
struct SummaryInput<'a> {
    inv: &'a Inventory,
    st: &'a Structure,
    lint: &'a LintSection,
    findings: &'a [Finding],
    own: &'a BTreeSet<String>,
}

fn summary(
    ws: &Workspace,
    index: &Index,
    paths: &Paths,
    input: &SummaryInput<'_>,
) -> Result<Summary> {
    let SummaryInput {
        inv,
        st,
        lint,
        findings,
        own,
    } = *input;
    let mut counts = Counts {
        layers_enabled: ws.layers.len(),
        layers_not_enabled: inv.layers.iter().filter(|l| !l.enabled).count(),
        layers_own: ws.layers.iter().filter(|l| l.writable).count(),
        layers_upstream: ws.layers.iter().filter(|l| !l.writable).count(),
        findings_own: lint.own_total,
        findings_upstream: lint.upstream_total,
        ..Counts::default()
    };
    for s in query::layer_stats(index)? {
        for (k, n) in [
            ("bb", s.recipes),
            ("bbappend", s.appends),
            ("inc", s.includes),
            ("bbclass", s.classes),
            ("conf", s.confs),
        ] {
            *counts.files.entry(k.into()).or_default() += n;
            counts.files_total += n;
        }
    }
    let attention = attention(findings, own, st);
    let kas_files = ws
        .kas
        .as_ref()
        .map(|k| k.files.iter().map(|f| paths.show(f)).collect())
        .unwrap_or_default();
    let line = format!(
        "{} release {}: {} layers enabled ({} own), {} present but not enabled; {} files; findings in own layers: {}; upstream: {}; {} items worth attention",
        if ws.kas.is_some() {
            "kas project,"
        } else {
            "workspace,"
        },
        ws.release.release,
        counts.layers_enabled,
        counts.layers_own,
        counts.layers_not_enabled,
        counts.files_total,
        counts.findings_own.describe(),
        counts.findings_upstream.describe(),
        attention.len()
    );
    Ok(Summary {
        line,
        root: ".".into(),
        build_dir: ws.build_dir.as_deref().map(|b| paths.show(b)),
        kas_files,
        release: ws.release.release.codename().into(),
        release_source: ws.release.source.as_str().into(),
        release_note: ws.release.note.clone(),
        counts,
        attention_rule: ATTENTION_RULE.into(),
        attention,
    })
}

/// One item per rule with findings of `sev` in the layers `keep` accepts, most findings
/// first. Counted from the whole lint report, not from the cut lists of section 4.
fn rule_items(findings: &[Finding], sev: Severity, keep: impl Fn(&str) -> bool) -> Vec<Attention> {
    let mut by: BTreeMap<&str, Attention> = BTreeMap::new();
    for f in findings
        .iter()
        .filter(|f| f.severity == sev && keep(&f.loc.layer))
    {
        let e = by.entry(f.rule).or_insert_with(|| Attention {
            section: "lint".into(),
            severity: sev.as_str().into(),
            what: f.rule.into(),
            count: 0,
            layers: Vec::new(),
            pointer: f.loc.to_string(),
            message: f.message.clone(),
        });
        e.count += 1;
        if !e.layers.contains(&f.loc.layer) {
            e.layers.push(f.loc.layer.clone());
        }
    }
    let mut v: Vec<Attention> = by.into_values().collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.what.cmp(&b.what)));
    v
}

/// Structure problems of a severity outside own layers, one item per check.
fn structure_items(st: &Structure, sev: &str, own_layers: &BTreeSet<String>) -> Vec<Attention> {
    let mut by: BTreeMap<String, Attention> = BTreeMap::new();
    for p in st.problems.iter().chain(&st.order) {
        if p.severity != sev || own_layers.contains(&p.layer) {
            continue;
        }
        let key = format!("layer.conf {}", p.check);
        let e = by.entry(key.clone()).or_insert_with(|| Attention {
            section: "structure".into(),
            severity: sev.into(),
            what: key,
            count: 0,
            layers: Vec::new(),
            pointer: p.pointer.clone(),
            message: p.message.clone(),
        });
        e.count += 1;
        if !e.layers.contains(&p.layer) {
            e.layers.push(p.layer.clone());
        }
    }
    let mut v: Vec<Attention> = by.into_values().collect();
    if sev == "error" {
        let up: Vec<&String> = st
            .orphan_appends
            .iter()
            .filter(|p| !own_layers.contains(p.split(':').next().unwrap_or_default()))
            .collect();
        if let Some(first) = up.first() {
            let mut layers: Vec<String> = up
                .iter()
                .map(|p| p.split(':').next().unwrap_or_default().to_string())
                .collect();
            layers.dedup();
            v.push(Attention {
                section: "structure".into(),
                severity: "error".into(),
                what: "bbappend matches no recipe".into(),
                count: up.len(),
                layers,
                pointer: format!("{first}:1"),
                message: "BitBake stops with \"No recipes available for\" on a bbappend that matches no recipe".into(),
            });
        }
    }
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.what.cmp(&b.what)));
    v
}

fn attention(findings: &[Finding], own: &BTreeSet<String>, st: &Structure) -> Vec<Attention> {
    let is_own = |l: &str| own.contains(l);
    // Structure problems in own layers are org/layer-conf findings: lint counts them.
    let mut out = rule_items(findings, Severity::Error, is_own);
    out.extend(structure_items(st, "error", own));
    out.extend(rule_items(findings, Severity::Warning, is_own));
    if out.len() < 3 {
        out.extend(rule_items(findings, Severity::Error, |l| !is_own(l)));
    }
    if out.len() < 3 {
        out.extend(structure_items(st, "warning", own));
    }
    out.truncate(MAX_ATTENTION);
    out
}

// ---- scrubbing and time

fn scrub_audit(a: &mut Audit, paths: &Paths) {
    let s = |t: &mut String| *t = paths.scrub(t);
    for f in a.lint.own.iter_mut().chain(a.lint.upstream.iter_mut()) {
        for r in &mut f.rules {
            for x in &mut r.findings {
                s(&mut x.message);
            }
        }
    }
    a.lint.notes.iter_mut().for_each(s);
    for p in a
        .structure
        .problems
        .iter_mut()
        .chain(a.structure.order.iter_mut())
    {
        s(&mut p.message);
    }
    a.structure.lint.iter_mut().for_each(|f| s(&mut f.message));
    a.structure.notes.iter_mut().for_each(s);
    a.inventory.notes.iter_mut().for_each(s);
    for l in &mut a.inventory.layers {
        if let Some(n) = &mut l.note {
            s(n);
        }
    }
    if let Some(k) = &mut a.inventory.kas {
        k.problems.iter_mut().for_each(s);
        s(&mut k.source);
    }
    s(&mut a.not_checked.build_environment);
    for att in &mut a.summary.attention {
        s(&mut att.message);
    }
}

/// Now, as `YYYY-MM-DDTHH:MM:SSZ`.
fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (proleptic Gregorian, UTC).
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ---------------------------------------------------------------------------------------
// Rendering

/// The first line of the Markdown report; the only line that differs between two runs on
/// the same repository state.
pub const GENERATED_PREFIX: &str = "Generated: ";

/// `|` and line breaks escaped for a Markdown table cell.
fn cell(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

/// Text for a Markdown list item or paragraph (line breaks removed).
fn inline(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

fn or_dash(v: &[String]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(" ")
    }
}

fn short_rev(r: &str) -> String {
    r.chars().take(12).collect()
}

struct Md {
    out: String,
}

impl Md {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        if !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn table(&mut self, header: &[&str], rows: &[Vec<String>]) {
        self.blank();
        self.line(&format!("| {} |", header.join(" | ")));
        self.line(&format!(
            "|{}|",
            header.iter().map(|_| "---").collect::<Vec<_>>().join("|")
        ));
        for r in rows {
            let cells: Vec<String> = r.iter().map(|c| cell(c)).collect();
            self.line(&format!("| {} |", cells.join(" | ")));
        }
        self.blank();
    }
}

/// The Markdown report.
pub fn render_markdown(a: &Audit) -> String {
    let mut md = Md { out: String::new() };
    md.line(&format!(
        "{GENERATED_PREFIX}{} by sous-chef {}",
        a.generated, a.sous_chef_version
    ));
    md.blank();
    md.line("# Workspace audit");
    md.blank();
    md.line(
        "Paths are relative to the audited root; pointers are `layer:path:line`, the path relative to the layer. Each section starts with a one-line summary.",
    );
    render_summary(&mut md, &a.summary);
    render_inventory(&mut md, &a.inventory);
    render_releases(&mut md, &a.releases);
    render_structure(&mut md, &a.structure);
    render_lint(&mut md, &a.lint);
    render_parse(&mut md, &a.parse);
    render_not_checked(&mut md, &a.not_checked);
    let mut out = md.out;
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The JSON report (pretty-printed, stable field names; schema in docs/mcp-tools.md).
pub fn render_json(a: &Audit) -> String {
    serde_json::to_string_pretty(a).unwrap_or_else(|_| "{}".into())
}

fn render_summary(md: &mut Md, s: &Summary) {
    md.blank();
    md.line("## Summary");
    md.blank();
    md.line(&inline(&s.line));
    let mut rows = vec![
        vec!["root".into(), s.root.clone()],
        vec![
            "build directory".into(),
            s.build_dir.clone().unwrap_or_else(|| "none".into()),
        ],
    ];
    if !s.kas_files.is_empty() {
        rows.push(vec!["kas files".into(), s.kas_files.join(":")]);
    }
    let mut release = format!("{} (from {})", s.release, s.release_source);
    if let Some(n) = &s.release_note {
        release.push_str(&format!("; {n}"));
    }
    rows.push(vec!["release".into(), release]);
    let c = &s.counts;
    rows.push(vec![
        "layers".into(),
        format!(
            "{} enabled ({} own, {} upstream), {} present but not enabled",
            c.layers_enabled, c.layers_own, c.layers_upstream, c.layers_not_enabled
        ),
    ]);
    rows.push(vec![
        "files".into(),
        format!(
            "{} ({})",
            c.files_total,
            c.files
                .iter()
                .map(|(k, n)| format!("{n} {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ]);
    rows.push(vec![
        "findings, own layers".into(),
        c.findings_own.describe(),
    ]);
    rows.push(vec![
        "findings, upstream layers".into(),
        c.findings_upstream.describe(),
    ]);
    md.table(&["", ""], &rows);
    md.line("### Most worth attention");
    md.blank();
    md.line(&format!("Chosen by rule: {}.", s.attention_rule));
    md.blank();
    if s.attention.is_empty() {
        md.line("Nothing: no errors, no layer structure errors and no warnings in own layers.");
    }
    for (i, a) in s.attention.iter().enumerate() {
        md.line(&format!(
            "{}. **{}** `{}` ({} {}, in {}): `{}` {}",
            i + 1,
            a.severity,
            a.what,
            a.count,
            if a.count == 1 { "finding" } else { "findings" },
            a.layers.join(", "),
            a.pointer,
            inline(&a.message)
        ));
    }
}

fn render_inventory(md: &mut Md, inv: &Inventory) {
    md.blank();
    md.line("## 1. Inventory");
    md.blank();
    md.line(&inline(&inv.summary));
    md.blank();
    md.line("### Layers");
    let rows: Vec<Vec<String>> = inv
        .layers
        .iter()
        .map(|l| {
            let files = match &l.files {
                Some(f) => format!(
                    "{} / {} / {} / {} / {}",
                    f.bb, f.bbappend, f.inc, f.bbclass, f.conf
                ),
                None => "not indexed".into(),
            };
            let mut role = l.role.clone();
            if let Some(n) = &l.note {
                role.push_str(&format!(" ({n})"));
            }
            vec![
                l.position.map_or("-".into(), |p| p.to_string()),
                l.name.clone(),
                l.path.clone(),
                role,
                or_dash(&l.collections),
                l.priority.map_or("-".into(), |p| p.to_string()),
                or_dash(&l.compat),
                or_dash(&l.depends),
                l.repository.clone().unwrap_or_else(|| "-".into()),
                files,
            ]
        })
        .collect();
    md.table(
        &[
            "#",
            "layer",
            "path",
            "role",
            "collection",
            "priority",
            "LAYERSERIES_COMPAT",
            "LAYERDEPENDS",
            "repository",
            "bb / bbappend / inc / bbclass / conf",
        ],
        &rows,
    );
    md.line("### Repositories");
    if inv.repositories.is_empty() {
        md.blank();
        md.line("None found (git facts unknown).");
    } else {
        let any_kas = inv.repositories.iter().any(|r| r.kas_repo.is_some());
        let rows: Vec<Vec<String>> = inv
            .repositories
            .iter()
            .map(|r| {
                let rev = match (&r.revision, r.checked_out) {
                    (_, false) => "not checked out".into(),
                    (Some(rev), _) => match &r.branch {
                        Some(b) => format!("{} ({b})", short_rev(rev)),
                        None => format!("{} (detached)", short_rev(rev)),
                    },
                    (None, _) => "unknown".into(),
                };
                let dirty = match r.dirty {
                    Some(0) => "clean".into(),
                    Some(n) => format!("dirty ({n})"),
                    None => "unknown".into(),
                };
                let mut row = vec![
                    r.path.clone(),
                    r.remote.clone().unwrap_or_else(|| "-".into()),
                    rev,
                    dirty,
                    r.submodule.clone().unwrap_or_else(|| "-".into()),
                ];
                if any_kas {
                    let pin = match (&r.pin, &r.pin_from) {
                        (Some(p), Some(f)) => {
                            let at = match r.at_pin {
                                Some(true) => "at pin",
                                Some(false) => "**not at pin**",
                                None => "unknown",
                            };
                            format!("{} ({f}): {at}", short_rev(p))
                        }
                        _ if r.kas_repo.is_some() => "not pinned".into(),
                        _ => "-".into(),
                    };
                    row.push(r.kas_repo.clone().unwrap_or_else(|| "-".into()));
                    row.push(pin);
                }
                row.push(or_dash(&r.layers));
                row
            })
            .collect();
        let mut header = vec!["repository", "remote", "revision", "status", "submodule"];
        if any_kas {
            header.extend(["kas repo", "kas pin"]);
        }
        header.push("layers");
        md.table(&header, &rows);
    }
    if let Some(k) = &inv.kas {
        md.line("### kas project");
        md.blank();
        md.line(&format!(
            "- files: {} ({})",
            if k.files.is_empty() {
                "none".into()
            } else {
                k.files.join(":")
            },
            inline(&k.source)
        ));
        md.line(&format!(
            "- lock files: {}",
            if k.lock_files.is_empty() {
                "none (the layers are pinned only where the kas files give a commit)".into()
            } else {
                k.lock_files.join(", ")
            }
        ));
        if !k.without_lock.is_empty() && !k.lock_files.is_empty() {
            md.line(&format!(
                "- kas files without a lock file: {}",
                k.without_lock.join(", ")
            ));
        }
        if !k.not_in_bblayers.is_empty() {
            md.line(&format!(
                "- in the kas files but not in bblayers.conf (run kas checkout again): {}",
                k.not_in_bblayers.join(", ")
            ));
        }
        if !k.not_in_kas.is_empty() {
            md.line(&format!(
                "- in bblayers.conf but in no kas file: {}",
                k.not_in_kas.join(", ")
            ));
        }
        for p in &k.problems {
            md.line(&format!("- problem: {}", inline(p)));
        }
    }
    notes(md, &inv.notes);
}

fn notes(md: &mut Md, notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    md.blank();
    for n in notes {
        md.line(&format!("- Note: {}", inline(n)));
    }
}

fn render_releases(md: &mut Md, r: &Releases) {
    md.blank();
    md.line("## 2. Releases");
    md.blank();
    md.line(&inline(&r.summary));
    md.blank();
    let mut ws = format!("Workspace release: **{}** (from {})", r.workspace, r.source);
    if let Some(n) = &r.note {
        ws.push_str(&format!("; {n}"));
    }
    md.line(&format!("{ws}."));
    md.line(&format!(
        "Supported: {}; {} is the floor, and names older than it are not checked.",
        r.supported.join(", "),
        r.floor
    ));
    let mut header: Vec<String> = vec!["layer".into(), "enabled".into()];
    header.extend(r.columns.iter().map(|c| {
        if *c == r.workspace {
            format!("{c} (workspace)")
        } else {
            c.clone()
        }
    }));
    header.push("older than the floor / unknown".into());
    let rows: Vec<Vec<String>> = r
        .layers
        .iter()
        .map(|l| {
            let mut row = vec![l.layer.clone(), if l.enabled { "yes" } else { "no" }.into()];
            for c in &r.columns {
                let claimed = l.claims.contains(c);
                row.push(if claimed {
                    "x".into()
                } else if *c == r.workspace {
                    "**missing**".into()
                } else {
                    String::new()
                });
            }
            let mut other: Vec<String> = l.older_than_floor.clone();
            other.extend(l.unknown.iter().map(|u| format!("{u} (unknown)")));
            row.push(or_dash(&other));
            row
        })
        .collect();
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    md.table(&h, &rows);
}

fn render_structure(md: &mut Md, s: &Structure) {
    md.blank();
    md.line("## 3. Layer structure");
    md.blank();
    md.line(&inline(&s.summary));
    md.blank();
    md.line("### layer.conf checks (org/layer-conf) and BBLAYERS order");
    md.blank();
    if s.problems.is_empty() && s.order.is_empty() {
        md.line("None.");
    }
    for p in s.problems.iter().chain(&s.order) {
        md.line(&format!(
            "- **{}** {} `{}` {}: {}",
            p.severity,
            p.check,
            p.pointer,
            p.layer,
            inline(&p.message)
        ));
    }
    md.blank();
    md.line("### Shadowed recipes and classes");
    if s.shadowed.is_empty() {
        md.blank();
        md.line("None: no recipe or class file name is provided by more than one layer.");
    } else {
        let rows: Vec<Vec<String>> = s
            .shadowed
            .iter()
            .map(|x| {
                vec![
                    x.kind.clone(),
                    x.name.clone(),
                    x.wins.clone(),
                    x.shadowed.join(", "),
                ]
            })
            .collect();
        md.table(&["kind", "name", "used", "shadowed"], &rows);
    }
    md.blank();
    md.line("### bbappends");
    md.blank();
    if s.orphan_appends.is_empty() {
        md.line("- Every collected bbappend matches a recipe.");
    } else {
        md.line(&format!(
            "- Match no recipe (BitBake stops on these): {}",
            s.orphan_appends
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if s.appends_not_collected.is_empty() {
        md.line("- Every bbappend is collected by a BBFILES glob.");
    } else {
        for a in &s.appends_not_collected {
            md.line(&format!("- `{}`: {}", a.append, inline(&a.status)));
        }
    }
    md.blank();
    md.line("### Over-broad bbappends and duplicate versions (from the lint run)");
    md.blank();
    if s.lint.is_empty() {
        md.line("None.");
    }
    for f in &s.lint {
        md.line(&format!(
            "- **{}** {} `{}`: {}",
            f.severity,
            f.rule,
            f.pointer,
            inline(&f.message)
        ));
    }
    notes(md, &s.notes);
}

fn render_layer_findings(md: &mut Md, l: &LayerFindings) {
    md.blank();
    md.line(&format!(
        "#### {} ({} findings: {})",
        l.layer,
        l.counts.total(),
        l.counts.describe()
    ));
    for r in &l.rules {
        md.blank();
        let shown = if r.more > 0 {
            format!(", first {}", r.findings.len())
        } else {
            String::new()
        };
        md.line(&format!(
            "- `{}` ({}, {}{shown})",
            r.rule, r.severity, r.count
        ));
        for f in &r.findings {
            let sev = if f.severity != r.severity {
                format!("**{}** ", f.severity)
            } else {
                String::new()
            };
            md.line(&format!("  - {sev}`{}`: {}", f.pointer, inline(&f.message)));
        }
        if let (true, Some(c)) = (r.more > 0, &r.command) {
            md.line(&format!("  - {} more: `{c}`", r.more));
        }
    }
}

fn render_lint(md: &mut Md, l: &LintSection) {
    md.blank();
    md.line("## 4. Lint findings");
    md.blank();
    md.line(&inline(&l.summary));
    md.blank();
    md.line(&format!(
        "Every enabled rule over every layer, as `{}`: upstream layers are checked against the workspace release alone, own layers against every release they claim. Commands below assume the global flags this audit was run with (`--config`, `--build-dir`), run from the root.",
        l.command
    ));
    md.blank();
    md.line("### Own layers and the build directory");
    if l.own.is_empty() {
        md.blank();
        md.line("No findings.");
    }
    for x in &l.own {
        render_layer_findings(md, x);
    }
    md.blank();
    md.line("### Upstream layers");
    md.blank();
    md.line(&format!(
        "Per rule: the count and the first {UPSTREAM_EXAMPLES} findings; the command after them lists every finding of that rule. Naming a layer (`sous-chef lint <layer>`) checks it against every release it claims and may report more."
    ));
    if l.upstream.is_empty() {
        md.blank();
        md.line("No findings.");
    } else {
        // An overview table first, so the counts are visible before the examples.
        let rows: Vec<Vec<String>> = l
            .upstream
            .iter()
            .map(|x| {
                vec![
                    x.layer.clone(),
                    x.counts.error.to_string(),
                    x.counts.warning.to_string(),
                    x.counts.info.to_string(),
                    x.rules
                        .iter()
                        .take(5)
                        .map(|r| format!("{} {}", r.rule, r.count))
                        .collect::<Vec<_>>()
                        .join(", ")
                        + if x.rules.len() > 5 { ", ..." } else { "" },
                ]
            })
            .collect();
        md.table(
            &["layer", "error", "warning", "info", "most frequent rules"],
            &rows,
        );
    }
    for x in &l.upstream {
        render_layer_findings(md, x);
    }
    notes(md, &l.notes);
}

fn render_listing(md: &mut Md, title: &str, l: &Listing) {
    md.blank();
    md.line(&format!("### {title} ({})", l.total));
    md.blank();
    if l.total == 0 {
        md.line("None.");
        return;
    }
    md.line(&format!(
        "Per layer: {}.",
        l.by_layer
            .iter()
            .map(|c| format!("{} {}", c.layer, c.count))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    md.blank();
    for i in &l.items {
        md.line(&format!("- `{}`: {}", i.pointer, inline(&i.text)));
    }
    if l.more > 0 {
        md.line(&format!(
            "- {} more in upstream layers (at most {UPSTREAM_PARSE_EXAMPLES} listed per layer)",
            l.more
        ));
    }
}

fn render_parse(md: &mut Md, p: &ParseSection) {
    md.blank();
    md.line("## 5. Parse and resolution problems");
    md.blank();
    md.line(&inline(&p.summary));
    render_listing(md, "Unparsed statements", &p.unparsed);
    render_listing(md, "Unresolved include and require", &p.unresolved_includes);
    render_listing(md, "Unresolved inherit", &p.unresolved_inherits);
    md.blank();
    md.line("### Files BitBake does not collect");
    if p.not_collected.is_empty() {
        md.blank();
        md.line("None.");
    }
    for n in &p.not_collected {
        render_listing(md, &n.reason, &n.files);
    }
}

fn render_not_checked(md: &mut Md, n: &NotChecked) {
    md.blank();
    md.line("## 6. What was not checked");
    md.blank();
    md.line(&inline(&n.summary));
    md.blank();
    md.line(&format!(
        "- Build environment: {}.",
        inline(&n.build_environment)
    ));
    md.line(&format!(
        "- Files with anonymous python: {}.",
        if n.anonymous_python.is_empty() {
            "none".into()
        } else {
            n.anonymous_python
                .iter()
                .map(|(k, c)| format!("{c} {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    if !n.layers_not_analysed.is_empty() {
        md.line(&format!(
            "- Not analysed (present, not enabled): {}.",
            n.layers_not_analysed.join(", ")
        ));
    }
    for l in &n.limits {
        md.line(&format!("- {l}"));
    }
}

// ---------------------------------------------------------------------------------------
// CLI

/// `--format` of `sous-chef audit`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ReportFormat {
    /// Markdown for a person to read.
    #[default]
    Md,
    /// The same content as JSON (schema in docs/mcp-tools.md).
    Json,
}

/// `sous-chef audit` arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct AuditCliArgs {
    /// The repository root (default: the working directory). Discovery starts here.
    pub root: Option<PathBuf>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = ReportFormat::Md)]
    pub format: ReportFormat,
    /// Write the report to FILE instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

fn progress_to_stderr(msg: &str) {
    eprintln!("sous-chef audit: {msg}");
}

/// Runs `sous-chef audit`: discovers the workspace from `cfg` (whose `work_dir` is the
/// root), builds the report and writes it. Returns 0; an error means the report could not
/// be produced (`main.rs` exits 2).
pub fn cli(cfg: &Config, args: &AuditCliArgs) -> Result<i32> {
    let started = Instant::now();
    let root = cfg.work_dir.clone();
    progress_to_stderr(&format!(
        "discovering the workspace from {}",
        root.display()
    ));
    let ctx = Ctx::open(cfg.clone())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let opts = Options {
        progress: Some(progress_to_stderr),
        ..Options::new(&root)
    };
    let audit = rt.block_on(build(&ctx, &opts))?;
    let text = match args.format {
        ReportFormat::Md => render_markdown(&audit),
        ReportFormat::Json => render_json(&audit),
    };
    match &args.output {
        Some(path) => {
            fs::write(path, format!("{text}\n"))?;
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(text.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    progress_to_stderr(&format!(
        "done in {:.1} s ({} bytes)",
        started.elapsed().as_secs_f64(),
        text.len() + 1
    ));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_stripped() {
        assert_eq!(
            strip_credentials("https://user:tok3n@example.com/org/repo.git"),
            "https://example.com/org/repo.git"
        );
        assert_eq!(
            strip_credentials("https://tok@example.com"),
            "https://example.com"
        );
        assert_eq!(
            strip_credentials("ssh://git@host:22/x.git"),
            "ssh://host:22/x.git"
        );
        assert_eq!(
            strip_credentials("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(
            strip_credentials("https://example.com/a@b"),
            "https://example.com/a@b"
        );
        assert_eq!(strip_credentials("/local/path"), "/local/path");
    }

    #[test]
    fn pins() {
        assert!(at_pin("0123456789abcdef", "0123456789abcdef"));
        assert!(at_pin("0123456789abcdef", "01234567"));
        assert!(!at_pin("0123456789abcdef", "fedcba"));
        assert!(!at_pin("", "01"));
    }

    #[test]
    fn utc_format() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_795_392_000 + 3_661), "2026-11-23T01:01:01Z");
    }

    #[test]
    fn pointer_order_is_numeric() {
        let mut v = vec!["a:x.bb:10", "a:x.bb:9", "a:w.bb:20"];
        v.sort_by(|a, b| cmp_pointer(a, b));
        assert_eq!(v, ["a:w.bb:20", "a:x.bb:9", "a:x.bb:10"]);
    }

    #[test]
    fn cells_escape_pipes() {
        assert_eq!(cell("a|b\nc"), "a\\|b c");
    }
}
