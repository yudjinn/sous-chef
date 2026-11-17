//! T8.4: the release diff. A statement-level diff of a layer's recipe trees between two git
//! revisions, and what it means for the bbappends of the workspace's own (writable) layers.
//! See docs/mcp-tools.md, `diff`.
//!
//! Revisions are read with `git ls-tree` and `git cat-file --batch` (the working tree from
//! disk, with the file list from `git ls-files`) into the real parser; nothing is checked
//! out, nothing is written into the repository, nothing is fetched. Every git command is
//! read-only, run with argv only, a timeout, no optional locks and the `GIT_DIR` family
//! removed. A revision is validated before it reaches git (no leading `-`, no whitespace,
//! no `:`, no range) and resolved with `rev-parse --verify --end-of-options <rev>^{commit}`;
//! only the full commit hash is passed on.
//!
//! - **Recipe identity.** A recipe is its `PN` from the file name (`index::scan::pn_pv`);
//!   the `.bb` files a layer collects are those its current `BBFILES` globs match. Several
//!   versions of one `PN`: equal versions pair first; then each remaining old version,
//!   highest first (`query::vercmp`), pairs with the new version sharing the longest
//!   version prefix (the highest on a tie); a pair with different versions is a version
//!   change, the rest are added or removed.
//! - **The tree** of a recipe at a revision is its `.bb` plus the files it `require`s /
//!   `include`s inside the same layer at that revision (relative to the including file,
//!   then to the layer root; `${PN}`, `${BPN}`, `${PV}`, `${BP}` and `${THISDIR}` expanded
//!   from the file name). An include found elsewhere, or needing evaluation, is shown as
//!   not compared. Classes are not part of a tree: changed classes are listed once.
//! - **The diff** compares statements as written, never evaluated: assignments grouped by
//!   variable (override variants and flags each their own key), functions and tasks by
//!   name with their bodies, `inherit`, other directives, and the `SRC_URI` entries word by
//!   word (`parse::words`). Whitespace-only statement changes and files that differ only in
//!   comments, whitespace or line positions are counted, not listed.
//! - **Own bbappends**: for each bbappend of a writable layer whose `PN` is a recipe of the
//!   diffed layer: whether it matches at each revision (`resolve::append_matches`), the
//!   version it applies to, its patches, whether the recipe's source (version, `SRCREV`,
//!   fetch URLs, checksums) changed under them, and whether a patch of the same name
//!   appeared in or disappeared from the recipe upstream.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::index::{query, scan};
use crate::mcp::Ctx;
use crate::parse::{self, IncludeMode, Lang, ParsedFile, StatementKind, words};
use crate::render::Table;
use crate::types::{FileKind, LineSpan, Loc, VarRef};
use crate::workspace::{Layer, Workspace, resolve};

/// The `schema` field of the JSON form; bumped when a field is renamed or removed.
pub const SCHEMA: &str = "sous-chef-diff/1";

/// The `kind` of a [`RecipeDiff`] for the files an own file requires from the layer.
pub const INCLUDES_KIND: &str = "includes of your file";

/// More flag statements of one variable with one change than this are one row in the
/// table.
const COLLAPSE_FLAGS: usize = 20;

/// Longest value shown in a cell, in characters (each side of `old -> new`).
const VALUE_CHARS: usize = 70;

/// What to compare.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// A layer name of the workspace.
    pub layer: String,
    /// The old revision.
    pub from: String,
    /// The new revision; `None` is the working tree (`HEAD` plus uncommitted changes).
    pub to: Option<String>,
    /// One recipe by `PN`.
    pub recipe: Option<String>,
    /// Every recipe of the layer.
    pub all: bool,
}

// ---------------------------------------------------------------------------------------
// The result

/// The whole diff. Serialised as the CLI's JSON form.
#[derive(Debug, Clone, Serialize)]
pub struct Diff {
    pub schema: &'static str,
    pub layer: String,
    /// The layer's directory relative to the repository's top level (`.` at the top).
    pub layer_path: String,
    pub from: Revision,
    pub to: Revision,
    /// Which recipes were compared, in words.
    pub selection: String,
    pub summary: Summary,
    /// Own bbappends, most severe first.
    pub appends: Vec<AppendFinding>,
    /// Recipes (own-appended first), then files the own layers include.
    pub recipes: Vec<RecipeDiff>,
    /// Changed `.bbclass` files of the layer.
    pub classes: Vec<ClassChange>,
    /// Files of the layer that differ, by kind.
    pub layer_files: Vec<FileCount>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    /// As given (`working tree` for the default `to`).
    pub given: String,
    /// The full commit hash; `HEAD`'s for the working tree.
    pub commit: String,
    /// For the working tree: files of the layer that differ from `HEAD` or are untracked.
    pub uncommitted: Option<usize>,
}

impl Revision {
    fn label(&self) -> String {
        let short = &self.commit[..self.commit.len().min(12)];
        match self.uncommitted {
            Some(n) => format!("working tree (HEAD {short}, {n} uncommitted files in the layer)"),
            None if self.commit.starts_with(&self.given) => short.to_string(),
            None => format!("{} ({short})", self.given),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub line: String,
    pub compared: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub added: usize,
    pub removed: usize,
    /// Version changes and moves (a subset of `changed`).
    pub renamed: usize,
    pub included_files: usize,
    pub appends: AppendCounts,
    pub classes_changed: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AppendCounts {
    pub no_longer_applies: usize,
    pub different_version: usize,
    pub source_changed: usize,
    pub recipe_changed: usize,
    pub now_applies: usize,
    pub unchanged: usize,
}

/// How a change affects an own bbappend, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    NoLongerApplies,
    DifferentVersion,
    SourceChanged,
    RecipeChanged,
    NowApplies,
    Unchanged,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::NoLongerApplies => "no longer applies",
            Severity::DifferentVersion => "applies to a different version",
            Severity::SourceChanged => "source changed under a patch",
            Severity::RecipeChanged => "recipe changed",
            Severity::NowApplies => "now applies",
            Severity::Unchanged => "unchanged",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AppendFinding {
    /// Pointer to the bbappend (`layer:path:1`).
    pub append: String,
    pub recipe: String,
    pub severity: Severity,
    /// One sentence for a person.
    pub detail: String,
    /// The recipe file it applies to at `from` / `to` (pointers), if any.
    pub from_recipe: Option<String>,
    pub to_recipe: Option<String>,
    pub patches: Vec<PatchNote>,
    /// Variables and functions the append sets that changed in the recipe.
    pub touches: Vec<String>,
    /// What changed in the recipe's source (version, SRCREV, URLs, checksums).
    pub source_changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatchNote {
    /// The `SRC_URI` entry as written.
    pub entry: String,
    /// The patch file next to the append, when found (pointer).
    pub file: Option<String>,
    /// What the upstream layer did with a patch of the same name, if anything.
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeDiff {
    /// `PN`, or the own file (`layer:path`) whose includes these are.
    pub name: String,
    /// `recipe` or [`INCLUDES_KIND`].
    pub kind: &'static str,
    /// `changed`, `unchanged`, `added` or `removed`.
    pub status: &'static str,
    /// Appended by an own layer.
    pub own_appended: bool,
    /// Why it was selected (`appended by ...`, `required by ...`), empty for `all`.
    pub reason: String,
    pub from_file: Option<String>,
    pub to_file: Option<String>,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    /// The `.bb` moved to another directory.
    pub moved: bool,
    pub tree_from: Vec<String>,
    pub tree_to: Vec<String>,
    pub changes: Vec<Change>,
    /// Statements that differ only in whitespace.
    pub whitespace_only: usize,
    /// Tree files that differ only in comments, whitespace or line positions.
    pub cosmetic_files: usize,
    pub source_changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    /// `file`, `var`, `function`, `task`, `inherit`, `directive` or `SRC_URI entry`.
    pub kind: &'static str,
    /// The variable, function or task name the change groups under.
    pub group: String,
    /// The statement's key as written (`SRC_URI:append:qemux86 +=`, `do_install`).
    pub name: String,
    /// `added`, `removed`, `changed`, `renamed`, `moved` or `not compared`.
    pub change: &'static str,
    pub old: Option<String>,
    pub new: Option<String>,
    pub detail: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassChange {
    /// Layer-relative path.
    pub path: String,
    /// `added`, `removed` or `changed`.
    pub change: &'static str,
    /// Statements that differ (as in a recipe diff).
    pub statements: usize,
    /// Compared recipes that inherit it directly (at either revision).
    pub inherited_by: Vec<String>,
    /// Own files that inherit it, directly (the `inherit` pointer) or through other files
    /// (`file (through pointer)`), from the index.
    pub own_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileCount {
    /// `bb`, `bbappend`, `inc`, `bbclass`, `conf`, `patch` or `other`.
    pub kind: String,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

// ---------------------------------------------------------------------------------------
// git

// TODO: use the shared git helper once merged
mod git {
    use super::*;

    /// Removed from git's environment so the repository is the one `current_dir` names.
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
        "GIT_EXTERNAL_DIFF",
        "GIT_PAGER",
    ];

    /// For short commands.
    pub const TIMEOUT: Duration = Duration::from_secs(20);
    /// For `cat-file --batch` over a whole layer.
    pub const BATCH_TIMEOUT: Duration = Duration::from_secs(120);

    /// Runs a read-only git command in `dir`: argv only, stdin closed unless `input` is
    /// given, no prompts, no optional locks, no fsmonitor, the `GIT_DIR` family removed,
    /// killed after `timeout`. Its stdout when it exits 0, else the first line git wrote to
    /// stderr.
    pub fn run(
        dir: &Path,
        args: &[&str],
        input: Option<Vec<u8>>,
        timeout: Duration,
    ) -> std::result::Result<Vec<u8>, String> {
        let mut cmd = Command::new("git");
        cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
            .args(args)
            .current_dir(dir)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C");
        for v in GIT_ENV {
            cmd.env_remove(v);
        }
        let what = args.first().copied().unwrap_or("git");
        let mut child = cmd.spawn().map_err(|e| format!("cannot run git: {e}"))?;
        let writer = match (input, child.stdin.take()) {
            (Some(data), Some(mut stdin)) => Some(thread::spawn(move || {
                let _ = stdin.write_all(&data);
            })),
            _ => None,
        };
        let mut stdout = child.stdout.take().ok_or("git: no stdout")?;
        let mut stderr = child.stderr.take().ok_or("git: no stderr")?;
        let out_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let err_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(2)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        if let Some(w) = writer {
            let _ = w.join();
        }
        let out = out_reader
            .join()
            .map_err(|_| "git: reading stdout failed")?;
        let err = err_reader.join().unwrap_or_default();
        match status {
            Some(s) if s.success() => Ok(out),
            Some(_) => {
                let msg = String::from_utf8_lossy(&err);
                let line = msg.lines().map(str::trim).find(|l| !l.is_empty());
                Err(format!(
                    "git {what} failed: {}",
                    line.unwrap_or("no message")
                ))
            }
            None => Err(format!(
                "git {what} timed out after {} s",
                timeout.as_secs()
            )),
        }
    }

    /// [`run`] with the short timeout, stdout as text.
    pub fn text(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
        run(dir, args, None, TIMEOUT).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// NUL-separated output split into its fields.
    pub fn nul_fields(bytes: &[u8]) -> Vec<String> {
        bytes
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect()
    }
}

/// Refuses anything that git could read as an option, a path or a range, before it
/// reaches git: an empty value, a leading `-`, whitespace or control characters, `:` (a
/// `rev:path`), `..` (a range) and values over 200 characters.
pub fn validate_revision(rev: &str) -> Result<()> {
    let bad = |why: &str| Error::Invalid {
        msg: format!("revision {rev:?} refused: {why}; give a branch, tag or commit"),
    };
    if rev.is_empty() {
        return Err(bad("empty"));
    }
    if rev.len() > 200 {
        return Err(bad("longer than 200 characters"));
    }
    if rev.starts_with('-') {
        return Err(bad("it starts with '-'"));
    }
    if rev.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("it contains whitespace or a control character"));
    }
    if rev.contains(':') {
        return Err(bad("it contains ':' (a path inside a revision)"));
    }
    if rev.contains("..") {
        return Err(bad("it is a range; give from and to separately"));
    }
    Ok(())
}

/// The full commit hash `rev` names in the repository at `dir`.
fn resolve_commit(dir: &Path, rev: &str) -> Result<String> {
    validate_revision(rev)?;
    let spec = format!("{rev}^{{commit}}");
    let out = git::text(
        dir,
        &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec],
    )
    .map_err(|e| Error::Invalid {
        msg: format!("revision {rev:?} is not a commit of this repository ({e}); look with git log or git tag, nothing is fetched"),
    })?;
    let hash = out.trim().to_string();
    if hash.len() < 40 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Invalid {
            msg: format!("revision {rev:?}: git gave no commit"),
        });
    }
    Ok(hash)
}

// ---------------------------------------------------------------------------------------
// Reading a revision

/// One file's text and parse.
#[derive(Debug)]
struct FileData {
    text: Arc<str>,
    parsed: Arc<ParsedFile>,
}

/// Parses shared between the two sides: an unchanged file is parsed once.
#[derive(Debug, Default)]
struct Parses(HashMap<Arc<str>, Arc<ParsedFile>>);

impl Parses {
    fn get(&mut self, text: &Arc<str>) -> Arc<ParsedFile> {
        if let Some(p) = self.0.get(text) {
            return p.clone();
        }
        let p = Arc::new(parse::parse(text));
        self.0.insert(text.clone(), p.clone());
        p
    }
}

/// The layer at one revision.
#[derive(Debug)]
struct Side {
    /// `None`: the working tree.
    commit: Option<String>,
    /// The layer root on disk (where git runs).
    dir: PathBuf,
    /// Every file of the layer, layer-relative with `/`, with its blob id at a commit.
    files: BTreeMap<String, Option<String>>,
    texts: HashMap<String, Arc<str>>,
    data: HashMap<String, Arc<FileData>>,
}

/// Extensions read up front, in one batch.
fn is_metadata(path: &str) -> bool {
    [".bb", ".inc", ".bbclass", ".bbappend"]
        .iter()
        .any(|e| path.ends_with(e))
}

impl Side {
    fn at_commit(dir: &Path, commit: &str) -> Result<Side> {
        let out = git::run(
            dir,
            &["ls-tree", "-r", "-z", "--end-of-options", commit],
            None,
            git::TIMEOUT,
        )
        .map_err(|e| Error::Invalid { msg: e })?;
        let mut files = BTreeMap::new();
        for entry in git::nul_fields(&out) {
            // `<mode> SP <type> SP <object> TAB <path>`
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut it = meta.split(' ');
            let (_mode, kind, oid) = (it.next(), it.next(), it.next());
            if kind != Some("blob") {
                continue;
            }
            files.insert(path.to_string(), oid.map(str::to_string));
        }
        Ok(Side {
            commit: Some(commit.to_string()),
            dir: dir.to_path_buf(),
            files,
            texts: HashMap::new(),
            data: HashMap::new(),
        })
    }

    fn working_tree(dir: &Path) -> Result<Side> {
        let out = git::run(
            dir,
            &["ls-files", "-z", "-c", "-o", "--exclude-standard"],
            None,
            git::TIMEOUT,
        )
        .map_err(|e| Error::Invalid { msg: e })?;
        let mut files = BTreeMap::new();
        for path in git::nul_fields(&out) {
            if dir.join(&path).is_file() {
                files.insert(path, None);
            }
        }
        Ok(Side {
            commit: None,
            dir: dir.to_path_buf(),
            files,
            texts: HashMap::new(),
            data: HashMap::new(),
        })
    }

    /// Reads every metadata file of the layer (one `cat-file --batch` at a commit).
    fn preload(&mut self) -> Result<()> {
        let paths: Vec<String> = self
            .files
            .keys()
            .filter(|p| is_metadata(p))
            .cloned()
            .collect();
        self.read(&paths)
    }

    fn read(&mut self, paths: &[String]) -> Result<()> {
        let wanted: Vec<&String> = paths
            .iter()
            .filter(|p| self.files.contains_key(*p) && !self.texts.contains_key(*p))
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        if self.commit.is_none() {
            let root = fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
            for p in wanted {
                let abs = self.dir.join(p);
                // Confinement: a symlink out of the layer is not read.
                let inside = fs::canonicalize(&abs).is_ok_and(|c| c.starts_with(&root));
                if !inside {
                    continue;
                }
                if let Ok(bytes) = fs::read(&abs) {
                    let text: Arc<str> = Arc::from(parse::lexer::decode(&bytes).as_ref());
                    self.texts.insert(p.clone(), text);
                }
            }
            return Ok(());
        }
        let mut input = Vec::new();
        let mut order = Vec::new();
        for p in wanted {
            if let Some(Some(oid)) = self.files.get(p) {
                input.extend_from_slice(oid.as_bytes());
                input.push(b'\n');
                order.push(p.clone());
            }
        }
        let out = git::run(
            &self.dir,
            &["cat-file", "--batch"],
            Some(input),
            git::BATCH_TIMEOUT,
        )
        .map_err(|e| Error::Invalid { msg: e })?;
        let mut pos = 0;
        for p in order {
            let Some(nl) = out[pos..].iter().position(|b| *b == b'\n') else {
                break;
            };
            let header = String::from_utf8_lossy(&out[pos..pos + nl]).into_owned();
            pos += nl + 1;
            let mut parts = header.split(' ');
            let (_oid, kind, size) = (parts.next(), parts.next(), parts.next());
            let Some(size) = size.and_then(|s| s.parse::<usize>().ok()) else {
                // `<oid> missing`
                continue;
            };
            let end = (pos + size).min(out.len());
            if kind == Some("blob") {
                let text: Arc<str> = Arc::from(parse::lexer::decode(&out[pos..end]).as_ref());
                self.texts.insert(p, text);
            }
            pos = end + 1;
        }
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// The file's text and parse, reading it if it was not preloaded.
    fn get(&mut self, path: &str, parses: &mut Parses) -> Result<Option<Arc<FileData>>> {
        if let Some(d) = self.data.get(path) {
            return Ok(Some(d.clone()));
        }
        if !self.exists(path) {
            return Ok(None);
        }
        if !self.texts.contains_key(path) {
            self.read(&[path.to_string()])?;
        }
        let Some(text) = self.texts.get(path).cloned() else {
            return Ok(None);
        };
        let parsed = parses.get(&text);
        let d = Arc::new(FileData { text, parsed });
        self.data.insert(path.to_string(), d.clone());
        Ok(Some(d))
    }
}

// ---------------------------------------------------------------------------------------
// Recipes at one side

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecipeFile {
    path: String,
    pn: String,
    pv: String,
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Whether the layer's `BBFILES` globs (as the workspace has them now, with the active
/// `BBFILES_DYNAMIC` entries) collect the `.bb` at `rel`. No glob: every `.bb` counts.
struct Collector {
    root: PathBuf,
    globs: Vec<resolve::DynamicEntry>,
}

impl Collector {
    fn new(ws: &Workspace, layer: &Layer) -> Collector {
        let mut globs: Vec<resolve::DynamicEntry> = layer
            .bbfiles
            .iter()
            .map(|g| resolve::DynamicEntry {
                layer: layer.name.clone(),
                collection: String::new(),
                invert: false,
                pattern: g.clone(),
                active: true,
            })
            .collect();
        let (dynamic, _) = resolve::bbfiles_dynamic(ws);
        globs.extend(
            dynamic
                .into_iter()
                .filter(|d| d.active && Path::new(&d.pattern).starts_with(&layer.root)),
        );
        Collector {
            root: layer.root.clone(),
            globs,
        }
    }

    fn collects(&self, rel: &str) -> bool {
        let abs = self.root.join(rel);
        self.globs.is_empty() || self.globs.iter().any(|g| g.matches(&abs))
    }
}

fn recipes_at(side: &Side, collector: &Collector) -> Vec<RecipeFile> {
    side.files
        .keys()
        .filter(|p| p.ends_with(".bb") && collector.collects(p))
        .filter_map(|p| {
            let (pn, pv) = scan::pn_pv(FileKind::Recipe, file_name(p));
            Some(RecipeFile {
                path: p.clone(),
                pn: pn?,
                pv: pv?,
            })
        })
        .collect()
}

/// One recipe across the two revisions.
#[derive(Debug, Clone)]
struct Pair {
    pn: String,
    old: Option<RecipeFile>,
    new: Option<RecipeFile>,
}

/// Pairs the recipe files of the two sides by `PN` (see the module documentation).
fn pair_recipes(old: &[RecipeFile], new: &[RecipeFile]) -> Vec<Pair> {
    let mut by_pn: BTreeMap<&str, (Vec<&RecipeFile>, Vec<&RecipeFile>)> = BTreeMap::new();
    for r in old {
        by_pn.entry(&r.pn).or_default().0.push(r);
    }
    for r in new {
        by_pn.entry(&r.pn).or_default().1.push(r);
    }
    let mut out = Vec::new();
    for (pn, (mut o, mut n)) in by_pn {
        // Equal versions first, equal paths before other paths.
        let mut pairs: Vec<(Option<&RecipeFile>, Option<&RecipeFile>)> = Vec::new();
        for same_path in [true, false] {
            let mut i = 0;
            while i < o.len() {
                let found = n
                    .iter()
                    .position(|x| x.pv == o[i].pv && (!same_path || x.path == o[i].path));
                match found {
                    Some(j) => pairs.push((Some(o.remove(i)), Some(n.remove(j)))),
                    None => i += 1,
                }
            }
        }
        let by_version_desc = |a: &&RecipeFile, b: &&RecipeFile| {
            query::vercmp(&b.pv, &a.pv).then(a.path.cmp(&b.path))
        };
        o.sort_by(by_version_desc);
        n.sort_by(by_version_desc);
        // The rest: each old version, highest first, with the new version sharing the
        // longest version prefix (`2.10.4` with `2.10.30` before `2.14.1`), the highest on
        // a tie.
        for a in o {
            let common = |x: &RecipeFile| {
                a.pv.chars()
                    .zip(x.pv.chars())
                    .take_while(|(p, q)| p == q)
                    .count()
            };
            let best = (0..n.len()).max_by(|i, j| {
                common(n[*i])
                    .cmp(&common(n[*j]))
                    .then(query::vercmp(&n[*i].pv, &n[*j].pv))
            });
            match best {
                Some(j) => pairs.push((Some(a), Some(n.remove(j)))),
                None => pairs.push((Some(a), None)),
            }
        }
        for b in n {
            pairs.push((None, Some(b)));
        }
        for (a, b) in pairs {
            out.push(Pair {
                pn: pn.to_string(),
                old: a.cloned(),
                new: b.cloned(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.pn.cmp(&b.pn).then_with(|| {
            let v = |p: &Pair| {
                p.new
                    .as_ref()
                    .or(p.old.as_ref())
                    .map(|r| r.pv.clone())
                    .unwrap_or_default()
            };
            query::vercmp(&v(a), &v(b))
        })
    });
    out
}

// ---------------------------------------------------------------------------------------
// Trees

/// Values that follow from a recipe file's name, for include arguments.
#[derive(Debug, Clone, Default)]
struct NameVars {
    pn: Option<String>,
    pv: Option<String>,
}

impl NameVars {
    fn from_file(name: &str) -> NameVars {
        let kind = if name.ends_with(".bbappend") {
            FileKind::Append
        } else {
            FileKind::Recipe
        };
        let (pn, pv) = scan::pn_pv(kind, name);
        NameVars { pn, pv }
    }

    fn bpn(&self) -> Option<String> {
        let pn = self.pn.as_ref()?;
        let mut b = pn.as_str();
        for s in [
            "-native",
            "-cross",
            "-initial",
            "-intermediate",
            "-crosssdk",
            "-cross-canadian",
        ] {
            if let Some(x) = b.strip_suffix(s) {
                b = x;
            }
        }
        Some(b.to_string())
    }

    fn get(&self, name: &str) -> Option<String> {
        match name {
            "PN" => self.pn.clone(),
            "PV" => self.pv.clone(),
            "BPN" => self.bpn(),
            "BP" => Some(format!("{}-{}", self.bpn()?, self.pv.as_ref()?)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct TreeFile {
    path: String,
    data: Arc<FileData>,
}

#[derive(Debug, Clone)]
struct Outside {
    via: String,
    text: String,
    why: String,
}

#[derive(Debug, Clone, Default)]
struct Tree {
    files: Vec<TreeFile>,
    outside: Vec<Outside>,
}

/// `a/b/../c` -> `a/c`; `None` when it climbs out of the layer.
fn normalise(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(s) => out.push(s.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out.join("/"))
}

/// Shared context for tree building.
struct Env<'a> {
    ws: &'a Workspace,
    layer: &'a Layer,
}

fn loc_str(layer: &str, path: &str, span: LineSpan) -> String {
    Loc {
        layer: layer.to_string(),
        path: PathBuf::from(path),
        lines: span,
    }
    .to_string()
}

fn build_tree(
    side: &mut Side,
    parses: &mut Parses,
    env: &Env,
    root: &str,
    vars: &NameVars,
) -> Result<Tree> {
    let mut tree = Tree::default();
    let mut seen = BTreeSet::new();
    visit(side, parses, env, root, vars, &mut tree, &mut seen, 0)?;
    Ok(tree)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    side: &mut Side,
    parses: &mut Parses,
    env: &Env,
    path: &str,
    vars: &NameVars,
    tree: &mut Tree,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<()> {
    if depth > 32 || !seen.insert(path.to_string()) {
        return Ok(());
    }
    let Some(data) = side.get(path, parses)? else {
        return Ok(());
    };
    tree.files.push(TreeFile {
        path: path.to_string(),
        data: data.clone(),
    });
    let dir = parent_dir(path).to_string();
    for st in &data.parsed.statements {
        let StatementKind::Include { path: arg, mode } = &st.kind else {
            continue;
        };
        if *mode == IncludeMode::IncludeAll {
            continue;
        }
        let via = loc_str(&env.layer.name, path, LineSpan::line(st.span.start));
        let thisdir = env.layer.root.join(&dir);
        let expanded = resolve::expand_with(arg, &|name| match name {
            "THISDIR" => Some(thisdir.to_string_lossy().into_owned()),
            _ => vars.get(name),
        });
        let Some(expanded) = expanded else {
            tree.outside.push(Outside {
                via,
                text: arg.clone(),
                why: "needs evaluation: not compared".into(),
            });
            continue;
        };
        for word in words::words(&expanded) {
            let root_str = env.layer.root.to_string_lossy();
            let word_rel = match word.strip_prefix(root_str.as_ref()) {
                Some(rest) => rest.trim_start_matches('/').to_string(),
                None if word.starts_with('/') => {
                    tree.outside.push(Outside {
                        via: via.clone(),
                        text: word.to_string(),
                        why: "an absolute path outside the layer: not compared".into(),
                    });
                    continue;
                }
                None => word.to_string(),
            };
            if file_name(&word_rel) == file_name(path) && !word.starts_with('/') {
                // BitBake skips a file that includes a file of its own name.
                continue;
            }
            let candidates = [
                normalise(&format!("{dir}/{word_rel}")),
                normalise(&word_rel),
            ];
            let found = candidates.into_iter().flatten().find(|c| side.exists(c));
            match found {
                Some(c) => visit(side, parses, env, &c, vars, tree, seen, depth + 1)?,
                None => {
                    let from = env.layer.root.join(path);
                    let why = match resolve::find_in_bbpath(env.ws, word, &from)
                        .and_then(|p| env.ws.loc_for(&p, LineSpan::line(1)))
                    {
                        Some(loc) if loc.layer != env.layer.name => {
                            format!("found in {} (outside the layer): not compared", loc.layer)
                        }
                        _ => "not in the layer at this revision: not compared".to_string(),
                    };
                    tree.outside.push(Outside {
                        via: via.clone(),
                        text: word.to_string(),
                        why,
                    });
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Statements as comparable items

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Var,
    Function,
    Task,
    Inherit,
    Directive,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Var => "var",
            Kind::Function => "function",
            Kind::Task => "task",
            Kind::Inherit => "inherit",
            Kind::Directive => "directive",
        }
    }
}

#[derive(Debug, Clone)]
struct Item {
    kind: Kind,
    group: String,
    key: String,
    raw: String,
    norm: String,
    loc: String,
    /// The value is a function body (shown by line counts).
    body: bool,
    /// Set by an `:remove` (for `SRC_URI` entries).
    remove: bool,
    flag: bool,
}

fn lhs(var: &VarRef) -> String {
    let mut s = var.base.clone();
    for o in &var.overrides {
        s.push(':');
        s.push_str(o);
    }
    if let Some(f) = &var.flag {
        s.push('[');
        s.push_str(f);
        s.push(']');
    }
    s
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines of a body that are code: trimmed, without blank and comment lines.
fn code_lines(s: &str) -> Vec<&str> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn items_of(tree: &Tree, layer: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for f in &tree.files {
        for st in &f.data.parsed.statements {
            let loc = loc_str(layer, &f.path, st.span);
            let text = f.data.text.get(st.bytes.clone()).unwrap_or("");
            let item = |kind, group: &str, key: String, raw: String| Item {
                kind,
                group: group.to_string(),
                key,
                norm: collapse(&raw),
                raw,
                loc: loc.clone(),
                body: false,
                remove: false,
                flag: false,
            };
            match &st.kind {
                StatementKind::Assign {
                    var,
                    op,
                    value,
                    export,
                } => {
                    let key = format!(
                        "{}{} {}",
                        if *export { "export " } else { "" },
                        lhs(var),
                        op.as_str()
                    );
                    let mut it = item(Kind::Var, &var.base, key, value.clone());
                    it.remove = var.overrides.iter().any(|o| o == "remove");
                    it.flag = var.flag.is_some();
                    out.push(it);
                }
                StatementKind::Export { name } => {
                    out.push(item(
                        Kind::Var,
                        name,
                        format!("export {name}"),
                        String::new(),
                    ));
                }
                StatementKind::Unset { name, flag } => {
                    let key = match flag {
                        Some(f) => format!("unset {name}[{f}]"),
                        None => format!("unset {name}"),
                    };
                    out.push(item(Kind::Var, name, key, String::new()));
                }
                StatementKind::Function {
                    name,
                    lang,
                    fakeroot,
                    ..
                } => {
                    let (group, display) = match name {
                        Some(v) => (v.base.clone(), lhs(v)),
                        None => ("__anonymous".to_string(), "__anonymous".to_string()),
                    };
                    let mut key = String::new();
                    if *fakeroot {
                        key.push_str("fakeroot ");
                    }
                    if *lang == Lang::Python {
                        key.push_str("python ");
                    }
                    key.push_str(&display);
                    let mut it = item(Kind::Function, &group, key, text.to_string());
                    it.norm = code_lines(text).join("\n");
                    it.body = true;
                    out.push(it);
                }
                StatementKind::PyDef { name, .. } => {
                    let mut it = item(Kind::Function, name, format!("def {name}"), text.into());
                    it.norm = code_lines(text).join("\n");
                    it.body = true;
                    out.push(it);
                }
                StatementKind::AddTask {
                    name,
                    before,
                    after,
                } => {
                    let mut raw = String::new();
                    if !after.is_empty() {
                        raw.push_str(&format!("after {}", after.join(" ")));
                    }
                    if !before.is_empty() {
                        if !raw.is_empty() {
                            raw.push(' ');
                        }
                        raw.push_str(&format!("before {}", before.join(" ")));
                    }
                    let mut it = item(Kind::Task, name, format!("addtask {name}"), raw);
                    let mut a = after.clone();
                    a.sort();
                    let mut b = before.clone();
                    b.sort();
                    it.norm = format!("after {} before {}", a.join(" "), b.join(" "));
                    out.push(it);
                }
                StatementKind::DelTask { name } => {
                    out.push(item(
                        Kind::Task,
                        name,
                        format!("deltask {name}"),
                        String::new(),
                    ));
                }
                StatementKind::Inherit { classes, deferred } => {
                    let kw = if *deferred {
                        "inherit_defer"
                    } else {
                        "inherit"
                    };
                    for c in words::words(classes) {
                        out.push(item(Kind::Inherit, c, format!("{kw} {c}"), String::new()));
                    }
                }
                StatementKind::Include { path, mode } => {
                    let kw = match mode {
                        IncludeMode::Include => "include",
                        IncludeMode::Require => "require",
                        IncludeMode::IncludeAll => "include_all",
                    };
                    out.push(item(
                        Kind::Directive,
                        path,
                        format!("{kw} {path}"),
                        String::new(),
                    ));
                }
                StatementKind::AddHandler { name } => out.push(item(
                    Kind::Directive,
                    name,
                    format!("addhandler {name}"),
                    String::new(),
                )),
                StatementKind::ExportFunctions { names } => out.push(item(
                    Kind::Directive,
                    "EXPORT_FUNCTIONS",
                    "EXPORT_FUNCTIONS".into(),
                    names.join(" "),
                )),
                StatementKind::AddFragments { rest } => out.push(item(
                    Kind::Directive,
                    "addfragments",
                    "addfragments".into(),
                    rest.trim().to_string(),
                )),
                StatementKind::AddPyLib { libdir, namespace } => out.push(item(
                    Kind::Directive,
                    "addpylib",
                    format!("addpylib {namespace}"),
                    libdir.trim().to_string(),
                )),
                StatementKind::Unknown { text } => out.push(item(
                    Kind::Directive,
                    "unparsed",
                    "unparsed".into(),
                    text.clone(),
                )),
            }
        }
    }
    out
}

/// Shortens a value for a cell: whitespace collapsed, quoted, cut at [`VALUE_CHARS`].
fn short(s: &str) -> String {
    let c = words::abbreviate(&collapse(s));
    cut(&c, 0)
}

/// `s` (whitespace collapsed) quoted, from character `start` (`...` before it when it is
/// not the start), cut at [`VALUE_CHARS`] with its length when cut.
fn cut(s: &str, start: usize) -> String {
    let n = s.chars().count();
    let lead = if start > 0 { "..." } else { "" };
    let rest: String = s.chars().skip(start).collect();
    if rest.chars().count() <= VALUE_CHARS {
        return format!("\"{lead}{rest}\"");
    }
    let kept: String = rest.chars().take(VALUE_CHARS - 3).collect();
    format!("\"{lead}{}...\" ({n} chars)", kept.trim_end())
}

/// Both sides of a changed value, shortened so the difference shows: `${@...}` groups are
/// abbreviated unless that hides the difference, and when the two share a long prefix
/// the cells start shortly before the first differing character.
fn short_pair(a: &str, b: &str) -> (String, String) {
    let (ca, cb) = (collapse(a), collapse(b));
    let (aa, ab) = (words::abbreviate(&ca), words::abbreviate(&cb));
    let (x, y) = if aa != ab { (aa, ab) } else { (ca, cb) };
    let common = x.chars().zip(y.chars()).take_while(|(p, q)| p == q).count();
    let start = if common > 30 { common - 20 } else { 0 };
    (cut(&x, start), cut(&y, start))
}

fn body_summary(
    old: Option<&Item>,
    new: Option<&Item>,
) -> (Option<String>, Option<String>, Option<String>) {
    let lines = |i: &Item| format!("{} lines", i.raw.lines().count());
    let detail = match (old, new) {
        (Some(o), Some(n)) => {
            let a: Vec<&str> = o.norm.lines().collect();
            let b: Vec<&str> = n.norm.lines().collect();
            let first = (0..a.len().max(b.len())).find(|i| a.get(*i) != b.get(*i));
            first.map(|i| {
                let (x, y) = match (a.get(i), b.get(i)) {
                    (Some(x), Some(y)) => short_pair(x, y),
                    (Some(x), None) => (short(x), "(end)".into()),
                    (None, Some(y)) => ("(end)".into(), short(y)),
                    (None, None) => (String::new(), String::new()),
                };
                format!("first difference at code line {}: {x} -> {y}", i + 1)
            })
        }
        _ => None,
    };
    (old.map(lines), new.map(lines), detail)
}

fn change_of(
    kind: &'static str,
    change: &'static str,
    o: Option<&Item>,
    n: Option<&Item>,
) -> Change {
    let any = o.or(n);
    let (old, new, detail) = if any.is_some_and(|i| i.body) {
        body_summary(o, n)
    } else {
        let show = |i: &Item| (!i.raw.is_empty()).then(|| short(&i.raw));
        match (o, n) {
            (Some(a), Some(b)) if !a.raw.is_empty() && !b.raw.is_empty() => {
                let (x, y) = short_pair(&a.raw, &b.raw);
                (Some(x), Some(y), None)
            }
            _ => (o.and_then(show), n.and_then(show), None),
        }
    };
    Change {
        kind,
        group: any.map(|i| i.group.clone()).unwrap_or_default(),
        name: any.map(|i| i.key.clone()).unwrap_or_default(),
        change,
        old,
        new,
        detail,
        from: o.map(|i| i.loc.clone()),
        to: n.map(|i| i.loc.clone()),
    }
}

/// Compares two item lists. Returns the listed changes and the number of statements that
/// differ only in whitespace.
fn compare_items(old: &[Item], new: &[Item]) -> (Vec<Change>, usize) {
    type K = (Kind, String, String);
    let mut by_key: BTreeMap<K, (Vec<&Item>, Vec<&Item>)> = BTreeMap::new();
    for i in old {
        by_key
            .entry((i.kind, i.group.clone(), i.key.clone()))
            .or_default()
            .0
            .push(i);
    }
    for i in new {
        by_key
            .entry((i.kind, i.group.clone(), i.key.clone()))
            .or_default()
            .1
            .push(i);
    }
    let mut changes = Vec::new();
    let mut cosmetic = 0;
    for ((kind, _, _), (mut o, mut n)) in by_key {
        // Exact matches, then whitespace-only ones.
        for exact in [true, false] {
            let mut i = 0;
            while i < o.len() {
                let found = n.iter().position(|x| {
                    if exact {
                        x.raw == o[i].raw
                    } else {
                        x.norm == o[i].norm
                    }
                });
                match found {
                    Some(j) => {
                        if !exact {
                            cosmetic += 1;
                        }
                        o.remove(i);
                        n.remove(j);
                    }
                    None => i += 1,
                }
            }
        }
        let k = kind.as_str();
        let mut o = o.into_iter();
        let mut n = n.into_iter();
        loop {
            match (o.next(), n.next()) {
                (None, None) => break,
                (Some(a), Some(b)) => changes.push(change_of(k, "changed", Some(a), Some(b))),
                (Some(a), None) => changes.push(change_of(k, "removed", Some(a), None)),
                (None, Some(b)) => changes.push(change_of(k, "added", None, Some(b))),
            }
        }
    }
    (changes, cosmetic)
}

/// `SRC_URI` entries, word by word, from the assignments that are not `:remove` or a flag.
fn src_uri_entries(items: &[Item]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for i in items {
        if i.kind == Kind::Var && i.group == "SRC_URI" && !i.remove && !i.flag {
            for w in words::words(&i.raw) {
                out.push((w.to_string(), i.loc.clone()));
            }
        }
    }
    out
}

fn compare_entries(old: &[(String, String)], new: &[(String, String)]) -> Vec<Change> {
    let mut count: BTreeMap<&str, (Vec<&str>, Vec<&str>)> = BTreeMap::new();
    for (w, l) in old {
        count.entry(w).or_default().0.push(l);
    }
    for (w, l) in new {
        count.entry(w).or_default().1.push(l);
    }
    let mut out = Vec::new();
    for (w, (o, n)) in count {
        let entry = |change, from: Option<&str>, to: Option<&str>| Change {
            kind: "SRC_URI entry",
            group: "SRC_URI".into(),
            name: words::abbreviate(w),
            change,
            old: None,
            new: None,
            detail: None,
            from: from.map(str::to_string),
            to: to.map(str::to_string),
        };
        for extra in o.iter().skip(n.len()) {
            out.push(entry("removed", Some(extra), None));
        }
        for extra in n.iter().skip(o.len()) {
            out.push(entry("added", None, Some(extra)));
        }
    }
    out
}

/// `oe.patch.patch_path`: `apply=` decides; else the extension (after `.gz` / `.bz2` /
/// `.xz` / `.Z`) is `.patch` or `.diff`.
fn is_patch(entry: &str) -> bool {
    let mut parts = entry.split(';');
    let path = parts.next().unwrap_or("");
    for p in parts {
        if let Some(v) = p.strip_prefix("apply=") {
            return matches!(v.to_lowercase().as_str(), "yes" | "y" | "true" | "t" | "1");
        }
    }
    let base = file_name(path);
    let base = [".gz", ".bz2", ".xz", ".Z"]
        .iter()
        .find_map(|c| base.strip_suffix(c))
        .unwrap_or(base);
    base.ends_with(".patch") || base.ends_with(".diff")
}

/// The local path of a `file://` entry (before the first `;`).
fn file_entry_path(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix("file://")?;
    Some(rest.split(';').next().unwrap_or(rest))
}

/// Variables whose change means the fetched source changed.
fn is_source_var(group: &str) -> bool {
    matches!(
        group,
        "PV" | "LINUX_VERSION" | "KBRANCH" | "SRCBRANCH" | "BRANCH"
    ) || (group.starts_with("SRCREV") && group != "SRCREV_FORMAT")
}

fn is_checksum_key(key: &str) -> bool {
    key.starts_with("SRC_URI[")
        && [
            "sha256sum]",
            "md5sum]",
            "sha1sum]",
            "sha384sum]",
            "sha512sum]",
        ]
        .iter()
        .any(|s| key.contains(s))
}

/// What changed in a recipe's source, from its changes and versions.
fn source_changes(rd: &RecipeDiff) -> Vec<String> {
    let mut out = Vec::new();
    if rd.from_file.is_none() || rd.to_file.is_none() {
        return out;
    }
    if let (Some(a), Some(b)) = (&rd.from_version, &rd.to_version)
        && a != b
    {
        out.push(format!("version {a} -> {b} (file name)"));
    }
    for c in &rd.changes {
        let name = c.name.split(' ').next().unwrap_or(&c.name);
        if c.kind == "var" && (is_source_var(&c.group) || is_checksum_key(name)) {
            let what = match (&c.old, &c.new) {
                (Some(o), Some(n)) => format!("{} {o} -> {n}", c.name),
                (None, Some(n)) => format!("{} added {n}", c.name),
                (Some(o), None) => format!("{} removed (was {o})", c.name),
                (None, None) => format!("{} {}", c.name, c.change),
            };
            out.push(what);
        }
        if c.kind == "SRC_URI entry" && !c.name.starts_with("file://") {
            out.push(format!("fetch URL {} {}", c.change, c.name));
        }
    }
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------------------
// Own layers

/// A file of a writable layer (read from disk, as the workspace has it now).
#[derive(Debug, Clone)]
pub struct OwnFile {
    pub layer: String,
    /// Layer-relative.
    pub path: String,
    pub kind: FileKind,
    pub text: String,
    /// Absolute path of the file's directory.
    pub dir: PathBuf,
    /// `require` / `include` targets in the diffed layer (layer-relative paths) with the
    /// line of the statement, from the index.
    pub includes: Vec<(String, u32)>,
}

impl OwnFile {
    fn pointer(&self, line: u32) -> String {
        loc_str(&self.layer, &self.path, LineSpan::line(line))
    }
}

/// Reads the writable layers' files (other than `target`) and their includes into
/// `target`, from the index.
pub fn own_files(ctx: &Ctx, index: &crate::index::Index, target: &str) -> Result<Vec<OwnFile>> {
    let mut out = Vec::new();
    for layer in ctx
        .workspace
        .layers
        .iter()
        .filter(|l| l.writable && l.name != target)
    {
        for f in query::files_in_layer_in(index, &layer.name, query::FileSet::COLLECTED)? {
            let abs = layer.root.join(&f.path);
            let Ok(abs) = ctx.workspace.confine(&abs) else {
                continue;
            };
            let Ok(bytes) = fs::read(&abs) else {
                continue;
            };
            let includes = query::edges_from(index, f.id)?
                .into_iter()
                .filter(|e| e.kind == "require" || e.kind == "include")
                .filter_map(|e| {
                    let line = e.line.unwrap_or(1);
                    e.target
                        .filter(|t| t.layer == target)
                        .map(|t| (t.path.to_string_lossy().replace('\\', "/"), line))
                })
                .collect();
            out.push(OwnFile {
                layer: layer.name.clone(),
                path: f.path.to_string_lossy().into_owned(),
                kind: f.kind,
                text: parse::lexer::decode(&bytes).into_owned(),
                dir: abs.parent().map(Path::to_path_buf).unwrap_or_default(),
                includes,
            });
        }
    }
    out.sort_by(|a, b| (&a.layer, &a.path).cmp(&(&b.layer, &b.path)));
    Ok(out)
}

/// Variables whose static words name recipes an own layer uses.
fn names_recipes(group: &str) -> bool {
    matches!(
        group,
        "DEPENDS"
            | "RDEPENDS"
            | "RRECOMMENDS"
            | "IMAGE_INSTALL"
            | "CORE_IMAGE_EXTRA_INSTALL"
            | "PACKAGE_INSTALL"
            | "TOOLCHAIN_HOST_TASK"
            | "TOOLCHAIN_TARGET_TASK"
    ) || group.starts_with("PREFERRED_PROVIDER_")
}

/// Candidate recipe names for a dependency word.
fn dep_names(word: &str) -> Vec<String> {
    let mut out = vec![word.to_string()];
    for s in ["-native", "-cross", "-crosssdk"] {
        if let Some(x) = word.strip_suffix(s) {
            out.push(x.to_string());
        }
    }
    if let Some(x) = word.strip_prefix("nativesdk-") {
        out.push(x.to_string());
    }
    out
}

/// Why each recipe of the layer interests the own layers: `PN` -> reasons.
fn interest(own: &[OwnFile], pns: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in own {
        if f.kind == FileKind::Append {
            if let (Some(pn), _) = scan::pn_pv(FileKind::Append, file_name(&f.path))
                && pns.contains(&pn)
            {
                out.entry(pn)
                    .or_default()
                    .insert(format!("appended by {}", f.pointer(1)));
            }
            continue;
        }
        let parsed = parse::parse(&f.text);
        for st in &parsed.statements {
            let StatementKind::Assign { var, value, .. } = &st.kind else {
                continue;
            };
            if let Some(pn) = var.base.strip_prefix("PREFERRED_VERSION_")
                && pns.contains(pn)
            {
                out.entry(pn.to_string()).or_default().insert(format!(
                    "{} in {}",
                    var.base,
                    f.pointer(st.span.start)
                ));
            }
            if !names_recipes(&var.base) {
                continue;
            }
            for w in words::words(value) {
                if words::is_unevaluated(w) {
                    continue;
                }
                if let Some(pn) = dep_names(w).into_iter().find(|n| pns.contains(n)) {
                    out.entry(pn).or_default().insert(format!(
                        "{} in {}",
                        var.base,
                        f.pointer(st.span.start)
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Building the diff

/// Builds the diff (refreshes the index first, as every tool does).
pub async fn build(ctx: &Ctx, opts: &Options) -> Result<Diff> {
    let own = {
        let mut index = ctx.index.lock().await;
        index.refresh(&ctx.workspace)?;
        own_files(ctx, &index, &opts.layer)?
    };
    let mut d = compute(&ctx.workspace, &own, opts)?;
    // Own files that inherit a changed class, directly or through other files (the index's
    // edges, as the workspace is now).
    let own_layers: BTreeSet<&str> = ctx
        .workspace
        .layers
        .iter()
        .filter(|l| l.writable && l.name != d.layer)
        .map(|l| l.name.as_str())
        .collect();
    let index = ctx.index.lock().await;
    for c in &mut d.classes {
        let name = file_name(&c.path).trim_end_matches(".bbclass");
        let mut users = Vec::new();
        for u in query::inheritors(&index, name, true)? {
            if !own_layers.contains(u.file.layer.as_str()) {
                continue;
            }
            users.push(if u.depth == 1 {
                u.via.to_string()
            } else {
                format!(
                    "{}:{} (through {})",
                    u.file.layer,
                    u.file.path.display(),
                    u.via
                )
            });
        }
        users.sort();
        users.dedup();
        c.own_users = users;
    }
    Ok(d)
}

fn layer_or_error<'a>(ws: &'a Workspace, name: &str) -> Result<&'a Layer> {
    ws.layer(name).ok_or_else(|| {
        let names: Vec<&str> = ws.layers.iter().map(|l| l.name.as_str()).collect();
        Error::NotFound {
            what: format!("layer {name:?}; layers: {}", names.join(", ")),
        }
    })
}

/// The diff from already gathered own-layer files (no index access).
pub fn compute(ws: &Workspace, own: &[OwnFile], opts: &Options) -> Result<Diff> {
    let layer = layer_or_error(ws, &opts.layer)?;
    validate_revision(&opts.from)?;
    if let Some(to) = &opts.to {
        validate_revision(to)?;
    }
    let dir = layer.root.clone();
    let prefix = git::text(&dir, &["rev-parse", "--show-prefix"]).map_err(|e| Error::Invalid {
        msg: format!(
            "layer {} is not inside a git repository ({e}); diff needs the layer's history",
            layer.name
        ),
    })?;
    let prefix = prefix.trim().trim_end_matches('/').to_string();
    let from_commit = resolve_commit(&dir, &opts.from)?;
    let head = resolve_commit(&dir, "HEAD")?;
    let (to_rev, to_commit) = match &opts.to {
        Some(t) => (t.clone(), resolve_commit(&dir, t)?),
        None => ("working tree".to_string(), head.clone()),
    };

    let mut old = Side::at_commit(&dir, &from_commit).map_err(|e| Error::Invalid {
        msg: format!("listing the layer at {}: {e}", opts.from),
    })?;
    if old.files.is_empty() {
        return Err(Error::Invalid {
            msg: format!(
                "the layer directory {} does not exist at {} (nothing to compare)",
                if prefix.is_empty() { "." } else { &prefix },
                opts.from
            ),
        });
    }
    let mut new = match &opts.to {
        Some(_) => Side::at_commit(&dir, &to_commit)?,
        None => Side::working_tree(&dir)?,
    };
    old.preload()?;
    new.preload()?;

    // Files that differ, by kind, and the uncommitted count.
    let changed = changed_files(&old, &new)?;
    let uncommitted = match &opts.to {
        Some(_) => None,
        None => Some(uncommitted_count(&dir)),
    };

    let collector = Collector::new(ws, layer);
    let old_recipes = recipes_at(&old, &collector);
    let new_recipes = recipes_at(&new, &collector);
    let pairs = pair_recipes(&old_recipes, &new_recipes);
    let pns: BTreeSet<String> = pairs.iter().map(|p| p.pn.clone()).collect();

    let own_appends: Vec<&OwnFile> = own.iter().filter(|f| f.kind == FileKind::Append).collect();
    let appended: BTreeSet<String> = own_appends
        .iter()
        .filter_map(|f| scan::pn_pv(FileKind::Append, file_name(&f.path)).0)
        .collect();
    let reasons = interest(own, &pns);
    let own_layers: Vec<&str> = ws
        .layers
        .iter()
        .filter(|l| l.writable && l.name != layer.name)
        .map(|l| l.name.as_str())
        .collect();

    let selection: String;
    let selected: Vec<&Pair> = match (&opts.recipe, opts.all) {
        (Some(r), _) => {
            let sel: Vec<&Pair> = pairs.iter().filter(|p| &p.pn == r).collect();
            if sel.is_empty() {
                let like: Vec<&str> = pns
                    .iter()
                    .filter(|p| p.contains(r.as_str()))
                    .take(20)
                    .map(String::as_str)
                    .collect();
                return Err(Error::NotFound {
                    what: format!(
                        "recipe {r:?} in {} at {} or {}{}",
                        layer.name,
                        opts.from,
                        to_rev,
                        if like.is_empty() {
                            String::new()
                        } else {
                            format!("; similar: {}", like.join(", "))
                        }
                    ),
                });
            }
            selection = format!("recipe {r}");
            sel
        }
        (None, true) => {
            selection = "all recipes of the layer".into();
            pairs.iter().collect()
        }
        (None, false) => {
            selection = if own_layers.is_empty() {
                "recipes your layers append or use (no writable layer is configured: set writable_layers, or use all or recipe)".into()
            } else {
                format!(
                    "recipes your layers ({}) append or use",
                    own_layers.join(", ")
                )
            };
            pairs
                .iter()
                .filter(|p| reasons.contains_key(&p.pn))
                .collect()
        }
    };

    let env = Env { ws, layer };
    let mut parses = Parses::default();
    let mut recipes = Vec::new();
    let mut trees: Vec<(String, Tree, Tree)> = Vec::new();
    for p in &selected {
        let (rd, t_old, t_new) = recipe_diff(
            &mut old,
            &mut new,
            &mut parses,
            &env,
            p,
            &reasons,
            &appended,
        )?;
        trees.push((rd.name.clone(), t_old, t_new));
        recipes.push(rd);
    }

    // Own bbappends: every pair of the append's `PN` that it matches at either revision;
    // when it matches none, one finding for the first pair.
    let mut appends = Vec::new();
    for a in &own_appends {
        let Some(pn) = scan::pn_pv(FileKind::Append, file_name(&a.path)).0 else {
            continue;
        };
        let candidates: Vec<usize> = (0..selected.len())
            .filter(|i| selected[*i].pn == pn)
            .collect();
        let matching: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|i| {
                let (m_old, m_new) = matches_pair(a, selected[*i]);
                m_old || m_new
            })
            .collect();
        let chosen = if matching.is_empty() {
            candidates.into_iter().take(1).collect()
        } else {
            matching
        };
        for i in chosen {
            let (_, t_old, t_new) = &trees[i];
            appends.push(append_finding(
                a,
                selected[i],
                &recipes[i],
                &old,
                &new,
                t_old,
                t_new,
                &env,
            ));
        }
    }
    for a in &mut appends {
        a.detail = a
            .detail
            .replace("{from}", &opts.from)
            .replace("{to}", &to_rev);
    }
    appends.sort_by(|a, b| {
        (a.severity, &a.recipe, &a.append).cmp(&(b.severity, &b.recipe, &b.append))
    });

    // Per own file, the files it requires from this layer, as one tree (not for a single
    // recipe): a statement moved between two of them is then a change, not a removal and
    // an addition.
    let mut included = Vec::new();
    if opts.recipe.is_none() {
        for f in own {
            let incs: Vec<&(String, u32)> = f
                .includes
                .iter()
                .filter(|(p, _)| !p.ends_with(".bb"))
                .collect();
            if incs.is_empty() {
                continue;
            }
            let vars = NameVars::from_file(file_name(&f.path));
            let mut t_old = Tree::default();
            let mut t_new = Tree::default();
            for (path, _) in &incs {
                for (side, tree) in [(&mut old, &mut t_old), (&mut new, &mut t_new)] {
                    if !side.exists(path) || tree.files.iter().any(|x| &x.path == path) {
                        continue;
                    }
                    let t = build_tree(side, &mut parses, &env, path, &vars)?;
                    for file in t.files {
                        if !tree.files.iter().any(|x| x.path == file.path) {
                            tree.files.push(file);
                        }
                    }
                    tree.outside.extend(t.outside);
                }
            }
            let reason = format!(
                "requires {}",
                incs.iter()
                    .map(|(p, line)| format!("{p} (line {line})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let name = loc_str(&f.layer, &f.path, LineSpan::line(1));
            let name = name.trim_end_matches(":1").to_string();
            let mut rd = tree_diff(
                &layer.name,
                name.clone(),
                INCLUDES_KIND,
                reason,
                &t_old,
                &t_new,
                0,
                Some(String::new()),
                Some(String::new()),
            );
            rd.from_file = None;
            rd.to_file = None;
            trees.push((rd.name.clone(), t_old, t_new));
            included.push(rd);
        }
    }

    // Classes.
    let classes = class_changes(&changed, &mut old, &mut new, &mut parses, &env, &trees)?;

    // Order: own-appended recipes first, then the rest by name; unchanged recipes of an
    // `all` run are counted only.
    recipes.sort_by(|a, b| {
        (!a.own_appended, &a.name, &a.to_version).cmp(&(!b.own_appended, &b.name, &b.to_version))
    });
    let summary = summarise(&recipes, &included, &appends, &classes);
    if opts.all && opts.recipe.is_none() {
        recipes.retain(|r| r.status != "unchanged" || r.own_appended);
    }
    recipes.extend(included);

    let from = Revision {
        given: opts.from.clone(),
        commit: from_commit,
        uncommitted: None,
    };
    let to = Revision {
        given: to_rev,
        commit: to_commit,
        uncommitted,
    };
    let mut diff = Diff {
        schema: SCHEMA,
        layer: layer.name.clone(),
        layer_path: if prefix.is_empty() {
            ".".into()
        } else {
            prefix
        },
        from,
        to,
        selection,
        summary,
        appends,
        recipes,
        classes,
        layer_files: count_files(&changed),
        notes: Vec::new(),
    };
    diff.summary.line = summary_line(&diff);
    diff.notes = notes(&diff);
    Ok(diff)
}

#[allow(clippy::too_many_arguments)]
fn tree_diff(
    layer: &str,
    name: String,
    kind: &'static str,
    reason: String,
    t_old: &Tree,
    t_new: &Tree,
    skip_root: usize,
    from_file: Option<String>,
    to_file: Option<String>,
) -> RecipeDiff {
    let i_old = items_of(t_old, layer);
    let i_new = items_of(t_new, layer);
    let (mut changes, whitespace_only) = compare_items(&i_old, &i_new);
    changes.extend(compare_entries(
        &src_uri_entries(&i_old),
        &src_uri_entries(&i_new),
    ));

    // Tree files: added, removed, renamed (same content) or moved (same name).
    let old_paths: BTreeMap<&str, &TreeFile> = t_old
        .files
        .iter()
        .skip(skip_root)
        .map(|f| (f.path.as_str(), f))
        .collect();
    let new_paths: BTreeMap<&str, &TreeFile> = t_new
        .files
        .iter()
        .skip(skip_root)
        .map(|f| (f.path.as_str(), f))
        .collect();
    let gone: Vec<&TreeFile> = old_paths
        .iter()
        .filter(|(p, _)| !new_paths.contains_key(*p))
        .map(|(_, f)| *f)
        .collect();
    let mut came: Vec<&TreeFile> = new_paths
        .iter()
        .filter(|(p, _)| !old_paths.contains_key(*p))
        .map(|(_, f)| *f)
        .collect();
    let file_change = |change, from: Option<&str>, to: Option<&str>, name: &str| Change {
        kind: "file",
        group: String::new(),
        name: name.to_string(),
        change,
        old: from.map(str::to_string),
        new: to.map(str::to_string),
        detail: None,
        from: from.map(|p| loc_str(layer, p, LineSpan::line(1))),
        to: to.map(|p| loc_str(layer, p, LineSpan::line(1))),
    };
    let mut file_changes = Vec::new();
    for g in gone {
        let same = came
            .iter()
            .position(|c| c.data.text == g.data.text)
            .or_else(|| {
                came.iter()
                    .position(|c| file_name(&c.path) == file_name(&g.path))
            });
        match same {
            Some(j) => {
                let c = came.remove(j);
                let change = if file_name(&c.path) == file_name(&g.path) {
                    "moved"
                } else {
                    "renamed"
                };
                file_changes.push(file_change(change, Some(&g.path), Some(&c.path), &c.path));
            }
            None => file_changes.push(file_change("removed", Some(&g.path), None, &g.path)),
        }
    }
    for c in came {
        file_changes.push(file_change("added", None, Some(&c.path), &c.path));
    }
    let outside = |t: &Tree, old_side: bool| -> Vec<Change> {
        t.outside
            .iter()
            .map(|o| Change {
                kind: "file",
                group: String::new(),
                name: o.text.clone(),
                change: "not compared",
                old: None,
                new: None,
                detail: Some(o.why.clone()),
                from: old_side.then(|| o.via.clone()),
                to: (!old_side).then(|| o.via.clone()),
            })
            .collect()
    };
    let mut outs = outside(t_old, true);
    for o in outside(t_new, false) {
        match outs
            .iter_mut()
            .find(|x| x.name == o.name && x.detail == o.detail && x.to.is_none())
        {
            Some(x) => x.to = o.to,
            None => outs.push(o),
        }
    }
    file_changes.extend(outs);

    // Files that differ but give no listed change.
    let mut cosmetic_files = 0;
    for f in &t_old.files {
        if let Some(n) = t_new.files.iter().find(|n| n.path == f.path)
            && n.data.text != f.data.text
        {
            let a = items_of(
                &Tree {
                    files: vec![f.clone()],
                    outside: Vec::new(),
                },
                layer,
            );
            let b = items_of(
                &Tree {
                    files: vec![n.clone()],
                    outside: Vec::new(),
                },
                layer,
            );
            if compare_items(&a, &b).0.is_empty() {
                cosmetic_files += 1;
            }
        }
    }

    file_changes.extend(changes);
    let changes = file_changes;
    let status = match (&from_file, &to_file) {
        (None, Some(_)) => "added",
        (Some(_), None) => "removed",
        _ if changes.is_empty() && whitespace_only == 0 && cosmetic_files == 0 => "unchanged",
        _ => "changed",
    };
    let tree_paths = |t: &Tree| t.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
    RecipeDiff {
        name,
        kind,
        status,
        own_appended: false,
        reason,
        from_file: from_file.map(|p| loc_str(layer, &p, LineSpan::line(1))),
        to_file: to_file.map(|p| loc_str(layer, &p, LineSpan::line(1))),
        from_version: None,
        to_version: None,
        moved: false,
        tree_from: tree_paths(t_old),
        tree_to: tree_paths(t_new),
        changes,
        whitespace_only,
        cosmetic_files,
        source_changes: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn recipe_diff(
    old: &mut Side,
    new: &mut Side,
    parses: &mut Parses,
    env: &Env,
    p: &Pair,
    reasons: &BTreeMap<String, BTreeSet<String>>,
    appended: &BTreeSet<String>,
) -> Result<(RecipeDiff, Tree, Tree)> {
    let t_old = match &p.old {
        Some(r) => build_tree(
            old,
            parses,
            env,
            &r.path,
            &NameVars::from_file(file_name(&r.path)),
        )?,
        None => Tree::default(),
    };
    let t_new = match &p.new {
        Some(r) => build_tree(
            new,
            parses,
            env,
            &r.path,
            &NameVars::from_file(file_name(&r.path)),
        )?,
        None => Tree::default(),
    };
    let reason = reasons
        .get(&p.pn)
        .map(|r| r.iter().cloned().collect::<Vec<_>>().join("; "))
        .unwrap_or_default();
    let mut rd = tree_diff(
        &env.layer.name,
        p.pn.clone(),
        "recipe",
        reason,
        &t_old,
        &t_new,
        1,
        p.old.as_ref().map(|r| r.path.clone()),
        p.new.as_ref().map(|r| r.path.clone()),
    );
    rd.from_version = p.old.as_ref().map(|r| r.pv.clone());
    rd.to_version = p.new.as_ref().map(|r| r.pv.clone());
    rd.own_appended = appended.contains(&p.pn);
    if let (Some(a), Some(b)) = (&p.old, &p.new) {
        rd.moved = parent_dir(&a.path) != parent_dir(&b.path);
        if a.path != b.path {
            let change = if a.pv != b.pv { "renamed" } else { "moved" };
            rd.changes.insert(
                0,
                Change {
                    kind: "file",
                    group: String::new(),
                    name: b.path.clone(),
                    change,
                    old: Some(a.path.clone()),
                    new: Some(b.path.clone()),
                    detail: (a.pv != b.pv).then(|| format!("version {} -> {}", a.pv, b.pv)),
                    from: Some(loc_str(&env.layer.name, &a.path, LineSpan::line(1))),
                    to: Some(loc_str(&env.layer.name, &b.path, LineSpan::line(1))),
                },
            );
            rd.status = "changed";
        }
    }
    rd.source_changes = source_changes(&rd);
    Ok((rd, t_old, t_new))
}

/// Patch file names (basenames) of a recipe at one side: its `file://` patch entries and
/// the patch files under its directory.
fn patch_names(
    side: &Side,
    tree: &Tree,
    recipe_dir: Option<&str>,
    layer: &str,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (w, _) in src_uri_entries(&items_of(tree, layer)) {
        if let Some(p) = file_entry_path(&w)
            && is_patch(&w)
        {
            out.insert(file_name(p).to_string());
        }
    }
    if let Some(d) = recipe_dir {
        let prefix = if d.is_empty() {
            String::new()
        } else {
            format!("{d}/")
        };
        for f in side.files.keys().filter(|f| f.starts_with(&prefix)) {
            if is_patch(f) {
                out.insert(file_name(f).to_string());
            }
        }
    }
    out
}

/// Whether the append matches the pair's recipe file at `from` and at `to`
/// (`resolve::append_matches`, file names only).
fn matches_pair(a: &OwnFile, p: &Pair) -> (bool, bool) {
    let m = |r: &Option<RecipeFile>| {
        r.as_ref()
            .is_some_and(|r| resolve::append_matches(Path::new(&a.path), Path::new(&r.path)))
    };
    (m(&p.old), m(&p.new))
}

#[allow(clippy::too_many_arguments)]
fn append_finding(
    a: &OwnFile,
    p: &Pair,
    rd: &RecipeDiff,
    old: &Side,
    new: &Side,
    t_old: &Tree,
    t_new: &Tree,
    env: &Env,
) -> AppendFinding {
    let (mo, mn) = matches_pair(a, p);
    let m_old = p.old.as_ref().filter(|_| mo);
    let m_new = p.new.as_ref().filter(|_| mn);
    let wildcard = file_name(&a.path).contains('%');
    let parsed = parse::parse(&a.text);
    let lines: Vec<(String, u32)> = parsed
        .statements
        .iter()
        .filter_map(|st| match &st.kind {
            StatementKind::Assign { var, value, .. }
                if var.base == "SRC_URI"
                    && var.flag.is_none()
                    && !var.overrides.iter().any(|o| o == "remove") =>
            {
                Some((value.clone(), st.span.start))
            }
            _ => None,
        })
        .collect();
    let vars = NameVars::from_file(file_name(
        &p.new
            .as_ref()
            .or(p.old.as_ref())
            .map(|r| r.path.clone())
            .unwrap_or_default(),
    ));
    let mut patches = Vec::new();
    let old_names = p
        .old
        .as_ref()
        .map(|r| patch_names(old, t_old, Some(parent_dir(&r.path)), &env.layer.name));
    let new_names = p
        .new
        .as_ref()
        .map(|r| patch_names(new, t_new, Some(parent_dir(&r.path)), &env.layer.name));
    for (value, _line) in &lines {
        for w in words::words(value) {
            let Some(path) = file_entry_path(w) else {
                continue;
            };
            if !is_patch(w) {
                continue;
            }
            let base = file_name(path).to_string();
            let mut subdirs: Vec<String> = Vec::new();
            for v in ["PN", "BPN", "BP"] {
                if let Some(x) = vars.get(v) {
                    subdirs.push(x);
                }
            }
            subdirs.push("files".into());
            subdirs.push(String::new());
            let file = subdirs.iter().find_map(|s| {
                let cand = if s.is_empty() {
                    a.dir.join(path)
                } else {
                    a.dir.join(s).join(path)
                };
                cand.is_file().then(|| {
                    let rel_dir = parent_dir(&a.path);
                    let rel = [rel_dir, s.as_str(), path]
                        .iter()
                        .filter(|x| !x.is_empty())
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/");
                    loc_str(&a.layer, &rel, LineSpan::line(1))
                })
            });
            let in_old = old_names.as_ref().is_some_and(|s| s.contains(&base));
            let in_new = new_names.as_ref().is_some_and(|s| s.contains(&base));
            let upstream = match (in_old, in_new) {
                (false, true) => Some(format!(
                    "a patch named {base} was added to the recipe upstream: the fix may have been upstreamed; check whether this one is still needed"
                )),
                (true, false) => Some(format!(
                    "a patch named {base} was removed from the recipe upstream: the fix may now be in the source; check whether this one is still needed"
                )),
                (true, true) => Some(format!(
                    "the recipe upstream carries a patch named {base} at both revisions"
                )),
                (false, false) => None,
            };
            patches.push(PatchNote {
                entry: w.to_string(),
                file,
                upstream,
            });
        }
    }
    patches.sort_by(|x, y| x.entry.cmp(&y.entry));

    // What the append sets that changed in the recipe.
    let groups: BTreeSet<String> = parsed
        .statements
        .iter()
        .filter_map(|st| match &st.kind {
            StatementKind::Assign { var, .. } => Some(var.base.clone()),
            StatementKind::Function { name: Some(v), .. } => Some(v.base.clone()),
            _ => None,
        })
        .collect();
    let mut touches: Vec<String> = rd
        .changes
        .iter()
        .filter(|c| (c.kind == "var" || c.kind == "function") && groups.contains(&c.group))
        .map(|c| c.group.clone())
        .collect();
    touches.sort();
    touches.dedup();

    let source = rd.source_changes.clone();
    let pointer = a.pointer(1);
    let file_of = |r: &RecipeFile| file_name(&r.path).to_string();
    let (severity, detail) = match (m_old, m_new) {
        (Some(o), None) => {
            let now = match &p.new {
                Some(n) => format!("at {{to}} the recipe is {} (version {})", file_of(n), n.pv),
                None => "at {to} the recipe is gone from the layer".to_string(),
            };
            let pinned = if wildcard {
                String::new()
            } else {
                format!(
                    "; it is pinned to version {}: rename it (or use {}_%.bbappend) if it should still apply",
                    o.pv, p.pn
                )
            };
            (
                Severity::NoLongerApplies,
                format!(
                    "applied to {} (version {}) at {{from}}; {now}{pinned}; BitBake stops on a bbappend that matches no recipe (a warning only with BB_DANGLINGAPPENDS_WARNONLY, before Walnascar)",
                    file_of(o),
                    o.pv
                ),
            )
        }
        (None, None) => (
            Severity::NoLongerApplies,
            format!(
                "matches no {} recipe file at either revision ({} at {{from}}, {} at {{to}})",
                p.pn,
                p.old.as_ref().map_or("none".into(), file_of),
                p.new.as_ref().map_or("none".into(), file_of)
            ),
        ),
        (None, Some(n)) => (
            Severity::NowApplies,
            format!(
                "did not apply at {{from}}; applies to {} (version {}) at {{to}}",
                file_of(n),
                n.pv
            ),
        ),
        (Some(o), Some(n)) if o.pv != n.pv => (
            Severity::DifferentVersion,
            format!(
                "its % now applies to version {} ({}) instead of {} ({}): check the append against the new version",
                n.pv,
                file_of(n),
                o.pv,
                file_of(o)
            ),
        ),
        (Some(_), Some(n)) if !patches.is_empty() && !source.is_empty() => (
            Severity::SourceChanged,
            format!(
                "still applies to {}; the source its patches apply to changed: check them",
                file_of(n)
            ),
        ),
        (Some(_), Some(n)) if rd.status == "changed" => (
            Severity::RecipeChanged,
            format!("still applies to {}; the recipe changed", file_of(n)),
        ),
        (Some(_), Some(n)) => (
            Severity::Unchanged,
            format!(
                "still applies to {}; the recipe tree is unchanged",
                file_of(n)
            ),
        ),
    };
    let mut detail = detail;
    if severity == Severity::DifferentVersion && !patches.is_empty() {
        detail.push_str("; its patches now apply to different source");
    }
    AppendFinding {
        append: pointer,
        recipe: p.pn.clone(),
        severity,
        detail,
        from_recipe: m_old.map(|r| loc_str(&env.layer.name, &r.path, LineSpan::line(1))),
        to_recipe: m_new.map(|r| loc_str(&env.layer.name, &r.path, LineSpan::line(1))),
        patches,
        touches,
        source_changes: source,
    }
}

/// Paths of the layer that differ between the sides, from `git diff --name-status` (the
/// working tree: tracked files against `from`, plus untracked files), sorted.
fn changed_files(old: &Side, new: &Side) -> Result<Vec<(String, &'static str)>> {
    let Some(from) = &old.commit else {
        return Ok(Vec::new());
    };
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--relative",
        "--name-status",
        "-z",
        "--end-of-options",
        from.as_str(),
    ];
    if let Some(to) = &new.commit {
        args.push(to.as_str());
    }
    let out = git::run(&old.dir, &args, None, git::BATCH_TIMEOUT)
        .map_err(|e| Error::Invalid { msg: e })?;
    let fields = git::nul_fields(&out);
    let mut map: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut it = fields.into_iter();
    while let (Some(status), Some(path)) = (it.next(), it.next()) {
        let change = match status.chars().next() {
            Some('A') => "added",
            Some('D') => "removed",
            _ => "changed",
        };
        map.insert(path, change);
    }
    if new.commit.is_none() {
        // Untracked files (`ls-files -o`) are in the working tree's list; git diff does
        // not see them.
        for p in new.files.keys() {
            if !old.files.contains_key(p) {
                map.entry(p.clone()).or_insert("added");
            }
        }
    }
    Ok(map.into_iter().collect())
}

fn uncommitted_count(dir: &Path) -> usize {
    let mut paths = BTreeSet::new();
    if let Ok(out) = git::run(
        dir,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--relative",
            "--name-only",
            "-z",
            "HEAD",
        ],
        None,
        git::TIMEOUT,
    ) {
        paths.extend(git::nul_fields(&out));
    }
    if let Ok(out) = git::run(
        dir,
        &["ls-files", "-z", "-o", "--exclude-standard"],
        None,
        git::TIMEOUT,
    ) {
        paths.extend(git::nul_fields(&out));
    }
    paths.len()
}

fn kind_of(path: &str) -> &'static str {
    let name = file_name(path);
    if name.ends_with(".bbappend") {
        "bbappend"
    } else if name.ends_with(".bb") {
        "bb"
    } else if name.ends_with(".inc") {
        "inc"
    } else if name.ends_with(".bbclass") {
        "bbclass"
    } else if name.ends_with(".conf") {
        "conf"
    } else if is_patch(name) {
        "patch"
    } else {
        "other"
    }
}

fn count_files(changed: &[(String, &'static str)]) -> Vec<FileCount> {
    let order = ["bb", "bbappend", "inc", "bbclass", "conf", "patch", "other"];
    let mut out: Vec<FileCount> = Vec::new();
    for k in order {
        let mut c = FileCount {
            kind: k.into(),
            added: 0,
            removed: 0,
            changed: 0,
        };
        for (_, ch) in changed.iter().filter(|(p, _)| kind_of(p) == k) {
            match *ch {
                "added" => c.added += 1,
                "removed" => c.removed += 1,
                _ => c.changed += 1,
            }
        }
        if c.added + c.removed + c.changed > 0 {
            out.push(c);
        }
    }
    out
}

fn class_changes(
    changed: &[(String, &'static str)],
    old: &mut Side,
    new: &mut Side,
    parses: &mut Parses,
    env: &Env,
    trees: &[(String, Tree, Tree)],
) -> Result<Vec<ClassChange>> {
    // The classes each compared tree inherits directly, at either revision.
    let inherit_sets: Vec<(&String, BTreeSet<String>)> = trees
        .iter()
        .map(|(n, a, b)| {
            let set = items_of(a, &env.layer.name)
                .into_iter()
                .chain(items_of(b, &env.layer.name))
                .filter(|i| i.kind == Kind::Inherit)
                .map(|i| i.group)
                .collect();
            (n, set)
        })
        .collect();
    let mut out = Vec::new();
    for (path, change) in changed.iter().filter(|(p, _)| p.ends_with(".bbclass")) {
        let name = file_name(path).trim_end_matches(".bbclass");
        let one = |side: &mut Side, parses: &mut Parses| -> Result<Tree> {
            Ok(Tree {
                files: side
                    .get(path, parses)?
                    .map(|d| TreeFile {
                        path: path.clone(),
                        data: d,
                    })
                    .into_iter()
                    .collect(),
                outside: Vec::new(),
            })
        };
        let a = one(old, parses)?;
        let b = one(new, parses)?;
        let (c, _) = compare_items(
            &items_of(&a, &env.layer.name),
            &items_of(&b, &env.layer.name),
        );
        let mut inherited_by: Vec<String> = inherit_sets
            .iter()
            .filter(|(_, set)| set.contains(name))
            .map(|(n, _)| (*n).clone())
            .collect();
        inherited_by.sort();
        inherited_by.dedup();
        out.push(ClassChange {
            path: path.clone(),
            change,
            statements: c.len(),
            inherited_by,
            own_users: Vec::new(),
        });
    }
    Ok(out)
}

fn summarise(
    recipes: &[RecipeDiff],
    included: &[RecipeDiff],
    appends: &[AppendFinding],
    classes: &[ClassChange],
) -> Summary {
    let mut s = Summary {
        compared: recipes.len(),
        included_files: included.len(),
        classes_changed: classes.len(),
        ..Summary::default()
    };
    for r in recipes {
        match r.status {
            "added" => s.added += 1,
            "removed" => s.removed += 1,
            "unchanged" => s.unchanged += 1,
            _ => s.changed += 1,
        }
        if r.status == "changed"
            && (r.moved || r.from_version.is_some() && r.from_version != r.to_version)
        {
            s.renamed += 1;
        }
    }
    for a in appends {
        let c = &mut s.appends;
        match a.severity {
            Severity::NoLongerApplies => c.no_longer_applies += 1,
            Severity::DifferentVersion => c.different_version += 1,
            Severity::SourceChanged => c.source_changed += 1,
            Severity::RecipeChanged => c.recipe_changed += 1,
            Severity::NowApplies => c.now_applies += 1,
            Severity::Unchanged => c.unchanged += 1,
        }
    }
    s
}

fn summary_line(d: &Diff) -> String {
    let s = &d.summary;
    let c = &s.appends;
    let mut line = format!(
        "{}: {} compared ({}): {} changed ({} renamed or moved), {} added, {} removed, {} unchanged",
        d.layer,
        plural(s.compared, "recipe"),
        d.selection,
        s.changed,
        s.renamed,
        s.added,
        s.removed,
        s.unchanged
    );
    if s.included_files > 0 {
        let changed = d
            .recipes
            .iter()
            .filter(|r| r.kind == INCLUDES_KIND && r.status != "unchanged")
            .count();
        line.push_str(&format!(
            "; your files requiring files of the layer: {}, {changed} with changes",
            s.included_files
        ));
    }
    let affected = c.no_longer_applies + c.different_version + c.source_changed;
    line.push_str(&format!(
        "; bbappends affected: {affected} ({} no longer applies, {} applies to a different version, {} source changed under a patch)",
        c.no_longer_applies, c.different_version, c.source_changed
    ));
    let other = c.recipe_changed + c.now_applies + c.unchanged;
    if other > 0 {
        line.push_str(&format!(
            "; {} more: {} recipe changed, {} now applies, {} unchanged",
            other, c.recipe_changed, c.now_applies, c.unchanged
        ));
    }
    line.push_str(&format!("; {} changed", plural(s.classes_changed, "class")));
    line
}

fn notes(d: &Diff) -> Vec<String> {
    let mut n = vec![
        "statement-level diff: statements as written, not evaluated; a tree is the .bb and what it requires inside the layer (current BBFILES, no BBMASK); classes listed once".to_string(),
        format!(
            "pointers in column from are at {}: read them with `git show {}:<path>`; column to is at {}",
            d.from.label(),
            &d.from.commit[..d.from.commit.len().min(12)],
            d.to.label(),
        ),
    ];
    let cosmetic: usize = d.recipes.iter().map(|r| r.whitespace_only).sum();
    let files: usize = d.recipes.iter().map(|r| r.cosmetic_files).sum();
    if cosmetic + files > 0 {
        n.push(format!(
            "counted, not listed: {cosmetic} statements changed only in whitespace, {files} files only in comments, whitespace or line positions"
        ));
    }
    if d.appends.iter().any(|a| !a.patches.is_empty()) {
        n.push("patches are not checked against their source (nothing is fetched): \"check it\" means the source they apply to changed".into());
    }
    n
}

// ---------------------------------------------------------------------------------------
// Rendering

/// `1 change`, `2 changes`, `1 class`, `2 classes`.
fn plural(n: usize, word: &str) -> String {
    match (n, word.ends_with('s')) {
        (1, _) => format!("1 {word}"),
        (_, true) => format!("{n} {word}es"),
        _ => format!("{n} {word}s"),
    }
}

/// The table: summary rows, own bbappends, then per recipe its rows, then classes.
pub fn table(d: &Diff) -> Table {
    let mut t = Table::new(&["recipe", "kind", "name", "change", "value", "from", "to"]);
    let dash = |s: &Option<String>| s.clone().unwrap_or_else(|| "-".into());
    t.row(&["-", "summary", "-", "-", &d.summary.line, "-", "-"]);
    t.row(&[
        "-",
        "revisions",
        &d.layer_path,
        "-",
        &format!("from {} to {}", d.from.label(), d.to.label()),
        &d.from.commit[..d.from.commit.len().min(12)],
        &d.to.commit[..d.to.commit.len().min(12)],
    ]);
    let files: Vec<String> = d
        .layer_files
        .iter()
        .map(|f| {
            format!(
                "{} {} changed {} added {} removed",
                f.kind, f.changed, f.added, f.removed
            )
        })
        .collect();
    t.row(&[
        "-",
        "layer files",
        "-",
        "-",
        &if files.is_empty() {
            "no file differs".to_string()
        } else {
            files.join("; ")
        },
        "-",
        "-",
    ]);

    for a in &d.appends {
        let mut value = a.detail.clone();
        if !a.touches.is_empty() {
            value.push_str(&format!(
                "; it sets {}, which changed in the recipe",
                a.touches.join(", ")
            ));
        }
        if !a.source_changes.is_empty() && !a.patches.is_empty() {
            value.push_str(&format!("; source: {}", a.source_changes.join("; ")));
        }
        t.row(&[
            &a.recipe,
            "bbappend",
            &a.append,
            a.severity.as_str(),
            &value,
            &dash(&a.from_recipe),
            &dash(&a.to_recipe),
        ]);
        for p in &a.patches {
            let mut v = format!(
                "patch {}",
                p.file
                    .as_deref()
                    .unwrap_or("(file not found next to the append)")
            );
            if a.severity <= Severity::SourceChanged && !a.source_changes.is_empty() {
                v.push_str("; the source it applies to changed: check it");
            }
            if let Some(u) = &p.upstream {
                v.push_str(&format!("; {u}"));
            }
            t.row(&[
                &a.recipe,
                "bbappend patch",
                &p.entry,
                "-",
                &v,
                "-",
                &a.append,
            ]);
        }
    }
    for r in &d.recipes {
        let version = match (&r.from_version, &r.to_version) {
            (Some(a), Some(b)) if a != b => format!("version {a} -> {b}"),
            (Some(a), Some(_)) => format!("version {a}"),
            (Some(a), None) | (None, Some(a)) => format!("version {a}"),
            _ => String::new(),
        };
        let listed = r.changes.iter().filter(|c| c.kind != "file").count();
        let mut value = Vec::new();
        if !version.is_empty() {
            value.push(version);
        }
        value.push(format!("{} listed", plural(listed, "change")));
        if r.whitespace_only + r.cosmetic_files > 0 {
            value.push(format!(
                "counted: {} only in whitespace, {} only in comments",
                plural(r.whitespace_only, "statement"),
                plural(r.cosmetic_files, "file")
            ));
        }
        if !r.reason.is_empty() {
            value.push(r.reason.clone());
        }
        if r.own_appended && r.reason.is_empty() {
            value.push("appended by your layers".into());
        }
        if !r.source_changes.is_empty() {
            value.push(format!("source: {}", r.source_changes.join("; ")));
        }
        t.row(&[
            &r.name,
            r.kind,
            &format!(
                "tree: {} at from, {} at to",
                plural(r.tree_from.len(), "file"),
                r.tree_to.len()
            ),
            r.status,
            &value.join("; "),
            &dash(&r.from_file),
            &dash(&r.to_file),
        ]);
        // Many flag statements of one variable with the same change (CVE_STATUS[...] of a
        // kernel) are one row; the JSON form lists each.
        let flag_run = |c: &Change| {
            (c.kind == "var" && c.name.contains('[')).then(|| (c.group.clone(), c.change))
        };
        let mut runs: BTreeMap<(String, &str), usize> = BTreeMap::new();
        for c in &r.changes {
            if let Some(k) = flag_run(c) {
                *runs.entry(k).or_default() += 1;
            }
        }
        let mut collapsed: BTreeSet<(String, &str)> = BTreeSet::new();
        for c in &r.changes {
            if let Some(k) = flag_run(c)
                && runs.get(&k).is_some_and(|n| *n > COLLAPSE_FLAGS)
            {
                if collapsed.insert(k.clone()) {
                    let n = runs.get(&k).copied().unwrap_or(0);
                    t.row(&[
                        &r.name,
                        c.kind,
                        &format!("{}[...]", c.group),
                        c.change,
                        &format!(
                            "{} {} (collapsed; the first is {}; the JSON form lists each)",
                            plural(n, "flag statement"),
                            c.change,
                            c.name
                        ),
                        &dash(&c.from),
                        &dash(&c.to),
                    ]);
                }
                continue;
            }
            let mut v = match (&c.old, &c.new) {
                (Some(o), Some(n)) => format!("{o} -> {n}"),
                (Some(o), None) => o.clone(),
                (None, Some(n)) => n.clone(),
                (None, None) => String::new(),
            };
            if let Some(det) = &c.detail {
                if !v.is_empty() {
                    v.push_str("; ");
                }
                v.push_str(det);
            }
            if v.is_empty() {
                v = "-".into();
            }
            t.row(&[
                &r.name,
                c.kind,
                &c.name,
                c.change,
                &v,
                &dash(&c.from),
                &dash(&c.to),
            ]);
        }
    }
    for c in &d.classes {
        let verb = if c.statements == 1 {
            "differs"
        } else {
            "differ"
        };
        let mut v = format!("{} {verb}", plural(c.statements, "statement"));
        if c.path.starts_with("classes-global/") {
            v.push_str("; a global class (base or INHERIT): it can affect every recipe");
        }
        if !c.inherited_by.is_empty() {
            v.push_str(&format!(
                "; inherited directly by compared {}",
                c.inherited_by.join(", ")
            ));
        }
        if !c.own_users.is_empty() {
            v.push_str(&format!("; inherited by your {}", c.own_users.join(", ")));
        }
        let at = |side_has: bool| {
            if side_has {
                loc_str(&d.layer, &c.path, LineSpan::line(1))
            } else {
                "-".into()
            }
        };
        t.row(&[
            "-",
            "class",
            &c.path,
            c.change,
            &v,
            &at(c.change != "added"),
            &at(c.change != "removed"),
        ]);
    }
    for n in &d.notes {
        t.note(&format!("note: {n}"));
    }
    t
}

/// The whole table as text, never cut.
pub fn render_text(d: &Diff) -> String {
    table(d).render(usize::MAX / 8)
}

/// The JSON form (the CLI's `--format json`).
pub fn render_json(d: &Diff) -> String {
    serde_json::to_string_pretty(d).unwrap_or_else(|_| "{}".into())
}

// ---------------------------------------------------------------------------------------
// CLI

/// `sous-chef diff` output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DiffFormat {
    /// The tool's table, every row.
    #[default]
    Text,
    /// The whole diff as JSON (`schema` "sous-chef-diff/1").
    Json,
}

/// `sous-chef diff LAYER FROM [TO]`.
#[derive(Debug, Clone, clap::Args)]
pub struct DiffCliArgs {
    /// A layer name of the workspace (as `layers` lists it).
    pub layer: String,
    /// The old revision: a branch, tag or commit.
    pub from: String,
    /// The new revision (default: the working tree, HEAD plus uncommitted changes).
    pub to: Option<String>,
    /// Compare one recipe (PN) only.
    #[arg(long)]
    pub recipe: Option<String>,
    /// Compare every recipe of the layer.
    #[arg(long)]
    pub all: bool,
    #[arg(long, value_enum, default_value_t = DiffFormat::Text)]
    pub format: DiffFormat,
    /// Write the output to FILE instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

/// Runs `sous-chef diff`. Progress and the time go to stderr.
pub fn cli(cfg: &crate::config::Config, args: &DiffCliArgs) -> Result<i32> {
    let started = Instant::now();
    let ctx = Ctx::open(cfg.clone())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let opts = Options {
        layer: args.layer.clone(),
        from: args.from.clone(),
        to: args.to.clone(),
        recipe: args.recipe.clone(),
        all: args.all,
    };
    let d = rt.block_on(build(&ctx, &opts))?;
    let text = match args.format {
        DiffFormat::Text => render_text(&d),
        DiffFormat::Json => format!("{}\n", render_json(&d)),
    };
    match &args.output {
        Some(path) => fs::write(path, &text)?,
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(text.as_bytes())?;
            stdout.flush()?;
        }
    }
    eprintln!(
        "sous-chef diff: done in {:.1} s ({} bytes)",
        started.elapsed().as_secs_f64(),
        text.len()
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_are_validated() {
        for ok in [
            "v1",
            "HEAD",
            "HEAD~2",
            "origin/main",
            "a1b2c3",
            "v1^{commit}",
            "@{-1}",
        ] {
            assert!(validate_revision(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "--output=/tmp/x",
            "-p",
            "a b",
            "a\nb",
            "HEAD:file",
            "a..b",
            "a...b",
        ] {
            assert!(validate_revision(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn patches_and_paths() {
        assert!(is_patch("file://a.patch"));
        assert!(is_patch("file://a.diff.gz"));
        assert!(is_patch("file://a.txt;apply=yes"));
        assert!(!is_patch("file://a.patch;apply=no"));
        assert!(!is_patch("file://a.cfg"));
        assert_eq!(
            file_entry_path("file://x/a.patch;striplevel=2"),
            Some("x/a.patch")
        );
        assert_eq!(normalise("a/b/../c"), Some("a/c".into()));
        assert_eq!(normalise("../x"), None);
    }

    #[test]
    fn pairing_by_version() {
        let r = |p: &str| {
            let (pn, pv) = scan::pn_pv(FileKind::Recipe, file_name(p));
            RecipeFile {
                path: p.into(),
                pn: pn.unwrap(),
                pv: pv.unwrap(),
            }
        };
        let old = [r("a/foo_1.0.bb"), r("a/foo_2.0.bb"), r("a/bar_1.bb")];
        let new = [r("a/foo_2.0.bb"), r("a/foo_2.1.bb"), r("b/baz_1.bb")];
        let pairs = pair_recipes(&old, &new);
        let show: Vec<String> = pairs
            .iter()
            .map(|p| {
                format!(
                    "{} {:?} {:?}",
                    p.pn,
                    p.old.as_ref().map(|x| x.pv.as_str()),
                    p.new.as_ref().map(|x| x.pv.as_str())
                )
            })
            .collect();
        assert_eq!(
            show,
            [
                "bar Some(\"1\") None",
                "baz None Some(\"1\")",
                "foo Some(\"2.0\") Some(\"2.0\")",
                "foo Some(\"1.0\") Some(\"2.1\")",
            ]
        );
        // The longest shared version prefix wins (meta-arm's fiptool-native, 5.0 to 6.0).
        let old = [r("x/fip_2.10.4.bb")];
        let new = [
            r("x/fip_2.10.30.bb"),
            r("x/fip_2.12.10.bb"),
            r("x/fip_2.14.1.bb"),
        ];
        let pairs = pair_recipes(&old, &new);
        let paired: Vec<(Option<&str>, Option<&str>)> = pairs
            .iter()
            .map(|p| {
                (
                    p.old.as_ref().map(|x| x.pv.as_str()),
                    p.new.as_ref().map(|x| x.pv.as_str()),
                )
            })
            .collect();
        assert!(
            paired.contains(&(Some("2.10.4"), Some("2.10.30"))),
            "{paired:?}"
        );
        assert_eq!(paired.len(), 3);
    }
}
