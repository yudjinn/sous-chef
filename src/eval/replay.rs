//! Replays configuration and recipe files in BitBake's parse order (T3.4).
//!
//! Statements come from the index (`statements` rows, `value` column), never from a reparse.
//! The order and the side effects follow BitBake `master` (2026-11): `cookerdata.py`
//! (`parseConfigurationFiles`, `_parse_recipe`), `parse/parse_py/ConfHandler.py` (`handle`,
//! `include_single_file`), `parse/parse_py/BBHandler.py` (`handle`, `inherit`) and
//! `parse/ast.py` (`MethodNode`, `ExportFuncsNode`, `multi_finalize`, `finalize`); see
//! `docs/bitbake-semantics.md`, "Parse order" and "Replay".
//!
//! **Configuration pass** (cached, see [`ConfigCache`]): `TOPDIR` is the build directory;
//! `conf/bblayers.conf`; every layer's `conf/layer.conf` in `BBLAYERS` order with `LAYERDIR`
//! and `LAYERDIR_RE` set, each `${LAYERDIR}` / `${LAYERDIR_RE}` reference replaced in place
//! afterwards (`expandVarref`) and both deleted after the last layer; `conf/bitbake.conf`
//! found on `BBPATH`, following its includes as written; then `base` and every `INHERIT`
//! class, in that order, as global classes. `MACHINE` / `DISTRO` from the [`Target`] are set
//! first and set again after every configuration assignment to them, so they win over the
//! configured values (as an environment setting would win over `??=`, and more).
//!
//! **Recipe pass**, on a fork of the configuration: `FILE_LAYERNAME`; the `.bb`, statement by
//! statement, following `require` / `include` / `include_all` / `inherit` where they appear
//! (targets resolved with the store as it is at that point, along the current `BBPATH`; a
//! class is parsed once, `__inherit_cache`); each bbappend in application order; deferred
//! inherits; `expand_keys`. Anonymous python is stored, never run. On a release without
//! `Feature::InheritDefer` an `inherit_defer` line is not a directive (BitBake fails with
//! "unparsed line"): it is recorded in [`ReplayNotes::references`] as
//! [`RefProblem::NotInRelease`] and nothing is inherited.
//!
//! **`FILE` and the values computed from it.** BitBake sets `FILE` to every `.conf`, `.bb`,
//! `.inc` and `.bbappend` while it is parsed and restores it afterwards (not for classes).
//! OE-Core computes `PN`, `PV` (`bb.parse.vars_from_file(FILE)`), `BPN`
//! (`oe.utils.prune_suffix`), `FILE_DIRNAME` and `THISDIR` (`dirname(FILE)`) with python. In
//! the recipe pass the replay computes them the same way whenever the stored value is python
//! or absent, and again every time `FILE` changes, so `${PN}` inside a bbappend is the
//! append's name part and `${THISDIR}` with `:=` is the directory of the file being parsed.
//! `BP` is set to `${BPN}-${PV}` when absent. A plain assignment to one of them in the
//! metadata takes over.
//!
//! **Functions** are stored like variables, as BitBake does (`MethodNode.eval`): the name as
//! written (overrides included), flags `func`, `python`, `fakeroot`. The stored text is not
//! the body but a pointer, one line per piece (`layer:path:A-B`, or
//! `layer:path:A-B -> class_do_x` for an `EXPORT_FUNCTIONS` stub), so `value("do_install")`
//! lists the composed pieces in order; [`DataStore::function`] parses them. Anonymous
//! python is stored under BitBake's name `__anon_<line>_<file>` and listed in
//! [`ReplayNotes::anonymous`]; the variables its body changes by name (`d.setVar("X", ..)`
//! and the like) are recorded in [`ReplayNotes::python_vars`] and their values are flagged
//! `unevaluated_python`.
//!
//! Nothing here fails on bad metadata: a reference that needs python, names an undefined
//! variable, is not found or closes a cycle is recorded in [`ReplayNotes::references`] and
//! skipped.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::datastore::{Content, Var, base_of, full_name};
use super::expand::has_python;
use super::{DataStore, FileVisit, OpStatus, Target, VarOp};
use crate::error::{Error, Result};
use crate::index::query::{self, FileRow, StatementFilter, StatementRow};
use crate::index::{self, Index};
use crate::release::Feature;
use crate::types::{AssignOp, FileKind, LineSpan, Loc, VarRef};
use crate::workspace::Workspace;
use crate::workspace::resolve::{self, Resolver};

// ---------------------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------------------

/// What the replay recorded beside the variables. Read it with [`DataStore::replay`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReplayNotes {
    /// The recipe file (line 1); `None` for a configuration-only evaluation.
    pub recipe: Option<Loc>,
    /// The bbappends applied, in application order (line 1 of each).
    pub appends: Vec<Loc>,
    /// `FILE_LAYERNAME`: the collection whose `BBFILE_PATTERN` matches the recipe.
    pub layer_name: Option<String>,
    /// Anonymous python functions, in parse order (configuration classes included: BitBake
    /// runs those for every recipe too).
    pub anonymous: Vec<AnonPython>,
    /// Base names of variables that anonymous python changes by a literal name; their
    /// values carry `unevaluated_python`.
    pub python_vars: BTreeSet<String>,
    /// References (`include`, `require`, `include_all`, `inherit`, `inherit_defer`,
    /// `INHERIT`) that were not followed, and why.
    pub references: Vec<RefIssue>,
    /// Names with `${...}` that `expand_keys` could not expand (python or undefined
    /// variables); they stay under their written name.
    pub incomplete_keys: Vec<String>,
    /// `__inherit_cache`: class files already parsed (absolute paths).
    #[serde(skip)]
    pub(crate) inherit_cache: Vec<PathBuf>,
    /// `__BBDEFINHERITS`: inherits waiting for the end of the recipe.
    #[serde(skip)]
    pub(crate) deferred: Vec<Deferred>,
    /// Names whose current value the replay computed (see the module documentation).
    #[serde(skip)]
    pub(crate) computed: BTreeSet<String>,
}

/// One anonymous python function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnonPython {
    /// The whole function.
    pub loc: Loc,
    pub via: Vec<Loc>,
    /// BitBake's name for it (`__anon_<line>_<file>`).
    pub name: String,
    /// Variables it changes by a literal name (`d.setVar("X", ...)`, `d.appendVar`, ...),
    /// expanded, as written.
    pub sets: Vec<String>,
    /// It also changes variables through a computed name, or passes `d` to other code, so
    /// its effect is not known even by name.
    pub opaque: bool,
}

/// A reference statement the replay did not follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefIssue {
    /// The statement; `None` for a class from `INHERIT` or `base` that has no statement of
    /// its own, and for `conf/bitbake.conf`.
    pub loc: Option<Loc>,
    pub via: Vec<Loc>,
    /// `include`, `require`, `include_all`, `inherit`, `inherit_defer`, `INHERIT`, `conf`.
    pub kind: String,
    /// The argument as written.
    pub text: String,
    pub problem: RefProblem,
}

/// Why a reference was not followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefProblem {
    /// The argument contains `${@...}`.
    Python,
    /// The argument references variables that are not set.
    Undefined(Vec<String>),
    /// No file for this word (`require`, `inherit`: an error in BitBake; `include`: silent
    /// in BitBake, not recorded).
    NotFound(String),
    /// The file is already being parsed further up (BitBake would recurse until it fails).
    Cycle(Loc),
    /// The directive does not exist on the workspace release (`inherit_defer` before
    /// Scarthgap): BitBake fails to parse the file there ("unparsed line"). The replay skips
    /// the statement and goes on.
    NotInRelease(Feature),
}

/// One piece of a function's value (see [`DataStore::function`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FunctionPiece {
    /// The function statement, or the `EXPORT_FUNCTIONS` statement for a stub.
    pub loc: Loc,
    /// For an `EXPORT_FUNCTIONS` stub: the function it calls (`autotoolsish_do_install`).
    pub calls: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Deferred {
    text: String,
    loc: Option<Loc>,
    via: Vec<Loc>,
}

impl DataStore {
    /// What the replay recorded beside the variables.
    pub fn replay(&self) -> &ReplayNotes {
        &self.notes
    }

    /// Whether `name` is a function (`func` flag set).
    pub fn is_function(&self, name: &str) -> bool {
        self.raw_flag(name, "func").is_some()
    }

    /// The composed pieces of a function, in order (override variant, `:append`,
    /// `:prepend` applied as for any variable). `None` if `name` has no value; lines that
    /// are not pointers are skipped.
    pub fn function(&self, name: &str) -> Option<Vec<FunctionPiece>> {
        let v = self.value(name)?;
        Some(v.text.lines().filter_map(parse_piece).collect())
    }
}

fn parse_piece(line: &str) -> Option<FunctionPiece> {
    let line = line.trim();
    let (loc, calls) = match line.split_once(" -> ") {
        Some((l, c)) => (l, Some(c.trim().to_string())),
        None => (line, None),
    };
    Some(FunctionPiece {
        loc: loc.parse().ok()?,
        calls,
    })
}

// ---------------------------------------------------------------------------------------
// Cache and entry points
// ---------------------------------------------------------------------------------------

/// Configuration passes, cached per index file, workspace, machine and distro. An entry is
/// reused while every `.conf` file and every file the pass visited keep their sha256 and
/// the set of `.bbclass` files is unchanged; otherwise the pass runs again.
#[derive(Debug, Default)]
pub struct ConfigCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    passes: AtomicU64,
}

type CacheKey = (PathBuf, String, Option<String>, Option<String>);

#[derive(Debug)]
struct CacheEntry {
    fingerprint: String,
    ds: DataStore,
}

impl ConfigCache {
    pub fn new() -> ConfigCache {
        ConfigCache::default()
    }

    /// How many configuration passes this cache has run.
    pub fn passes(&self) -> u64 {
        self.passes.load(Ordering::Relaxed)
    }

    /// Drops every entry.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, CacheEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The process-wide cache [`evaluate`] uses.
pub fn global_cache() -> &'static ConfigCache {
    static CACHE: OnceLock<ConfigCache> = OnceLock::new();
    CACHE.get_or_init(ConfigCache::new)
}

/// [`super::evaluate`] with an explicit cache. The index must be refreshed by the caller.
pub fn evaluate_with(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    target: &Target,
) -> Result<DataStore> {
    let files = Files::load(index, ws)?;
    let recipe = match &target.recipe {
        Some(r) => Some(find_recipe(index, ws, r, target.version.as_deref())?),
        None => None,
    };
    let config = configuration(cache, index, ws, &files, target)?;
    match recipe {
        None => Ok(config),
        Some(r) => recipe_pass(index, ws, &files, &config, &r),
    }
}

fn configuration(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    files: &Files,
    target: &Target,
) -> Result<DataStore> {
    let key: CacheKey = (
        index.path().to_path_buf(),
        index::workspace_key(ws),
        target.machine.clone(),
        target.distro.clone(),
    );
    if let Some(e) = cache.lock().get(&key)
        && e.fingerprint == files.fingerprint(e.ds.files())
    {
        return Ok(e.ds.fork());
    }
    let ds = config_pass(index, ws, files, target)?;
    cache.passes.fetch_add(1, Ordering::Relaxed);
    let fingerprint = files.fingerprint(ds.files());
    cache.lock().insert(
        key,
        CacheEntry {
            fingerprint,
            ds: ds.fork(),
        },
    );
    Ok(ds)
}

/// The recipe a target names: a recipe name (with `version`, `PREFERRED_VERSION` style, `%`
/// allowed), an absolute path to a `.bb`, or `layer:path`.
fn find_recipe(
    index: &Index,
    ws: &Workspace,
    name: &str,
    version: Option<&str>,
) -> Result<FileRow> {
    let not_found = || Error::NotFound {
        what: match version {
            Some(v) => format!("recipe {name} version {v}; list recipes with the recipe tool"),
            None => format!("recipe {name}; list recipes with the recipe tool"),
        },
    };
    if name.ends_with(".bb") {
        let p = Path::new(name);
        let row = if p.is_absolute() {
            query::file_by_abs(index, ws, p)?
        } else if let Some((layer, path)) = name.split_once(':') {
            query::file_by_path(index, layer, Path::new(path))?
        } else {
            None
        };
        return row
            .filter(|f| f.kind == FileKind::Recipe)
            .ok_or_else(not_found);
    }
    query::preferred_recipe(index, name, version)?
        .map(|r| r.file)
        .ok_or_else(not_found)
}

// ---------------------------------------------------------------------------------------
// Indexed files
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FileInfo {
    id: i64,
    layer: String,
    path: PathBuf,
    kind: FileKind,
    sha256: String,
}

/// Every indexed file by absolute path (layer root joined with the indexed path).
struct Files {
    by_abs: HashMap<PathBuf, FileInfo>,
    by_loc: HashMap<(String, PathBuf), FileInfo>,
    known: HashSet<PathBuf>,
    /// The part of the cache fingerprint that does not depend on the visited files.
    base: String,
}

impl Files {
    fn load(index: &Index, ws: &Workspace) -> Result<Files> {
        // By layer name then path, compared as bytes (SQLite's text order): the order the
        // cache fingerprint is computed in.
        let mut rows: Vec<FileInfo> = query::all_files(index)?
            .into_iter()
            .map(|f| FileInfo {
                id: f.id,
                layer: f.layer,
                path: f.path,
                kind: f.kind,
                sha256: f.sha256,
            })
            .collect();
        rows.sort_by(|a, b| {
            a.layer.cmp(&b.layer).then_with(|| {
                a.path
                    .as_os_str()
                    .as_encoded_bytes()
                    .cmp(b.path.as_os_str().as_encoded_bytes())
            })
        });
        let mut by_abs = HashMap::new();
        let mut by_loc = HashMap::new();
        let mut h = Sha256::new();
        for f in rows {
            match f.kind {
                FileKind::Conf => {
                    h.update(format!("c {} {} {}\n", f.layer, f.path.display(), f.sha256))
                }
                FileKind::Class => h.update(format!("k {} {}\n", f.layer, f.path.display())),
                _ => {}
            }
            let loc = Loc {
                layer: f.layer.clone(),
                path: f.path.clone(),
                lines: LineSpan::line(1),
            };
            if let Some(abs) = ws.abs_path(&loc) {
                by_abs.insert(normalize(&abs), f.clone());
            }
            by_loc.insert((f.layer.clone(), f.path.clone()), f);
        }
        let known = by_abs.keys().cloned().collect();
        Ok(Files {
            by_abs,
            by_loc,
            known,
            base: hex(&h.finalize()),
        })
    }

    /// Cache fingerprint of a configuration pass that visited `visits`.
    fn fingerprint(&self, visits: &[FileVisit]) -> String {
        let mut h = Sha256::new();
        h.update(self.base.as_bytes());
        for v in visits {
            let sha = self
                .by_loc
                .get(&(v.loc.layer.clone(), v.loc.path.clone()))
                .map(|f| f.sha256.as_str())
                .unwrap_or("-");
            h.update(format!(
                "v {} {} {sha}\n",
                v.loc.layer,
                v.loc.path.display()
            ));
        }
        hex(&h.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lexical normalisation (`.` and `..` removed).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// The two passes
// ---------------------------------------------------------------------------------------

fn config_pass(index: &Index, ws: &Workspace, files: &Files, target: &Target) -> Result<DataStore> {
    let mut pins = Vec::new();
    if let Some(m) = &target.machine {
        pins.push(("MACHINE", m.clone()));
    }
    if let Some(d) = &target.distro {
        pins.push(("DISTRO", d.clone()));
    }
    let mut r = Replay::new(index, ws, files, DataStore::new(), true, false, pins);
    for (name, value) in r.pins.clone() {
        put(&mut r.ds, name, &value);
    }

    // bblayers.conf; TOPDIR is its build directory.
    if let Some(build) = &ws.build_dir {
        put(&mut r.ds, "TOPDIR", &build.to_string_lossy());
        let bblayers = normalize(&build.join("conf/bblayers.conf"));
        if r.files.by_abs.contains_key(&bblayers) {
            r.visit(&bblayers, FileKind::Conf, Vec::new(), true)?;
        }
    }

    // Each layer.conf, with LAYERDIR set; references replaced afterwards (expandVarref).
    let bblayers_via = r.via_of_word("BBLAYERS", None);
    for layer in &ws.layers {
        let conf = normalize(&layer.root.join("conf/layer.conf"));
        if !r.files.by_abs.contains_key(&conf) {
            continue;
        }
        let dir = layer.root.to_string_lossy().to_string();
        put(&mut r.ds, "LAYERDIR", &dir);
        put(&mut r.ds, "LAYERDIR_RE", &re_escape(&dir));
        r.visit(&conf, FileKind::Conf, bblayers_via.clone(), true)?;
        expand_varref(&mut r.ds, "LAYERDIR");
        expand_varref(&mut r.ds, "LAYERDIR_RE");
    }
    remove_var(&mut r.ds, "LAYERDIR_RE");
    remove_var(&mut r.ds, "LAYERDIR");

    // conf/bitbake.conf on BBPATH (ConfHandler.resolve_file: BBPATH only).
    let bitbake_conf = r
        .bbpath()
        .into_iter()
        .map(|e| normalize(&e.join("conf/bitbake.conf")))
        .find(|p| r.files.by_abs.contains_key(p));
    match bitbake_conf {
        Some(p) => r.visit(&p, FileKind::Conf, Vec::new(), true)?,
        None => r.issue(
            None,
            &[],
            "conf",
            "conf/bitbake.conf",
            RefProblem::NotFound("conf/bitbake.conf".into()),
        ),
    }

    // ["base"] + INHERIT, as global classes.
    let inherit = r.ds.value("INHERIT").map(|v| v.text).unwrap_or_default();
    let mut classes = vec!["base".to_string()];
    classes.extend(inherit.split_whitespace().map(str::to_string));
    for c in classes {
        let via = if c == "base" {
            Vec::new()
        } else {
            r.via_of_word("INHERIT", Some(&c))
        };
        r.inherit(&c, None, &via, false, "INHERIT")?;
    }
    Ok(r.ds)
}

fn recipe_pass(
    index: &Index,
    ws: &Workspace,
    files: &Files,
    config: &DataStore,
    recipe: &FileRow,
) -> Result<DataStore> {
    let mut r = Replay::new(index, ws, files, config.fork(), false, true, Vec::new());
    let recipe_loc = recipe.loc(LineSpan::line(1));
    let abs = ws
        .abs_path(&recipe_loc)
        .map(|p| normalize(&p))
        .ok_or_else(|| Error::NotFound {
            what: format!("layer of {recipe_loc}"),
        })?;
    let layername = file_layername(&r.ds, ws, &abs, &recipe.layer);
    if let Some(l) = &layername {
        put(&mut r.ds, "FILE_LAYERNAME", l);
    }
    {
        let notes = Arc::make_mut(&mut r.ds.notes);
        notes.recipe = Some(recipe_loc.clone());
        notes.layer_name = layername;
    }
    r.visit(&abs, FileKind::Recipe, Vec::new(), false)?;

    for a in query::appends_for(index, recipe.id)? {
        let loc = a.loc(LineSpan::line(1));
        let Some(aabs) = ws.abs_path(&loc).map(|p| normalize(&p)) else {
            continue;
        };
        Arc::make_mut(&mut r.ds.notes).appends.push(loc);
        r.visit(&aabs, FileKind::Append, vec![recipe_loc.clone()], true)?;
    }

    // Deferred inherits (ast.finalize), first in first out; more may be added meanwhile.
    loop {
        let next = {
            let notes = Arc::make_mut(&mut r.ds.notes);
            if notes.deferred.is_empty() {
                None
            } else {
                Some(notes.deferred.remove(0))
            }
        };
        let Some(d) = next else { break };
        r.inherit(&d.text, d.loc.as_ref(), &d.via, true, "inherit_defer")?;
    }

    let incomplete = r.ds.expand_keys();
    Arc::make_mut(&mut r.ds.notes).incomplete_keys = incomplete;
    Ok(r.ds)
}

/// `FILE_LAYERNAME` (`CookerCollectFiles.get_layername`): the collection whose
/// `BBFILE_PATTERN` regex matches the start of the recipe path, patterns tried in reverse
/// lexical order. Falls back to the first collection of the recipe's layer when no pattern
/// can be evaluated.
fn file_layername(ds: &DataStore, ws: &Workspace, abs: &Path, layer: &str) -> Option<String> {
    let mut pats: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for l in &ws.layers {
        for c in &l.collections {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(v) = ds.value(&format!("BBFILE_PATTERN_{c}"))
                && !v.unevaluated_python
                && v.unresolved.is_empty()
                && !v.text.is_empty()
            {
                pats.push((c.clone(), v.text));
            }
        }
    }
    pats.sort_by(|a, b| b.1.cmp(&a.1));
    let path = abs.to_string_lossy();
    for (c, p) in &pats {
        if let Ok(re) = Regex::new(&format!("^(?:{p})"))
            && re.is_match(&path)
        {
            return Some(c.clone());
        }
    }
    ws.layer(layer).and_then(|l| l.collections.first().cloned())
}

// ---------------------------------------------------------------------------------------
// Direct datastore writes (values BitBake sets itself, not metadata operations)
// ---------------------------------------------------------------------------------------

/// Sets the plain value of `name` (BitBake `setVar` from code), keeping the sources of the
/// value it replaces. Not recorded in the history.
fn put_keep(ds: &mut DataStore, name: &str, text: &str, src: Vec<u64>) {
    ds.var_mut(name).content = Some(Content {
        text: text.to_string(),
        src,
    });
    ds.ovr_cache = OnceLock::new();
}

fn put(ds: &mut DataStore, name: &str, text: &str) {
    put_keep(ds, name, text, Vec::new());
}

fn put_flag(ds: &mut DataStore, name: &str, flag: &str, text: &str) {
    ds.var_mut(name).flags.insert(
        flag.to_string(),
        Content {
            text: text.to_string(),
            src: Vec::new(),
        },
    );
}

fn remove_var(ds: &mut DataStore, name: &str) {
    if ds.vars.contains_key(name) {
        Arc::make_mut(&mut ds.vars).remove(name);
        ds.ovr_cache = OnceLock::new();
    }
}

/// `DataSmart.expandVarref`: every stored text containing `${var}` gets it replaced by the
/// value of `var`. (BitBake does this on the finalised value of each variable and stores
/// the result with a plain `setVar`, folding pending `:append`s into it; replacing inside
/// each piece gives the same final values.)
fn expand_varref(ds: &mut DataStore, var: &str) {
    let Some(value) = ds.raw(var) else { return };
    let pat = format!("${{{var}}}");
    let names: Vec<Arc<str>> = ds
        .vars
        .iter()
        .filter(|(k, v)| k.as_ref() != var && mentions(v, &pat))
        .map(|(k, _)| k.clone())
        .collect();
    for n in names {
        let v = ds.var_mut(&n);
        for c in v.content.iter_mut().chain(v.default.iter_mut()) {
            c.text = c.text.replace(&pat, &value);
        }
        for p in v
            .appends
            .iter_mut()
            .chain(v.prepends.iter_mut())
            .chain(v.removes.iter_mut())
        {
            p.text = p.text.replace(&pat, &value);
        }
    }
    ds.ovr_cache = OnceLock::new();
}

fn mentions(v: &Var, pat: &str) -> bool {
    v.content
        .iter()
        .chain(v.default.iter())
        .any(|c| c.text.contains(pat))
        || v.appends
            .iter()
            .chain(v.prepends.iter())
            .chain(v.removes.iter())
            .any(|p| p.text.contains(pat))
}

/// Python's `re.escape` (3.7 and later).
fn re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `bb.parse.vars_from_file`: `(PN, PV)` from a `.bb` / `.bbappend` name; nothing for any
/// other file or a name with more than three `_` parts (an error in BitBake).
fn vars_from_file(file: &str) -> (Option<String>, Option<String>) {
    let p = Path::new(file);
    let ext = p.extension().and_then(|e| e.to_str());
    if !matches!(ext, Some("bb") | Some("bbappend")) {
        return (None, None);
    }
    let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
        return (None, None);
    };
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() > 3 {
        return (None, None);
    }
    let get = |i: usize| {
        parts
            .get(i)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    (get(0), get(1))
}

/// `SPECIAL_PKGSUFFIX` of OE-Core's `bitbake.conf`, used when the variable is not set.
const SPECIAL_PKGSUFFIX: &[&str] = &[
    "-native",
    "-cross",
    "-initial",
    "-intermediate",
    "-crosssdk",
    "-cross-canadian",
];

/// Variables computed from `FILE` in the recipe pass, in dependency order.
const COMPUTED: &[&str] = &["PN", "PV", "BPN", "BP", "FILE_DIRNAME", "THISDIR"];

/// Assignments to these names make the computed values be looked at again.
const COMPUTED_INPUTS: &[&str] = &[
    "PN",
    "PV",
    "BPN",
    "BP",
    "FILE_DIRNAME",
    "THISDIR",
    "SPECIAL_PKGSUFFIX",
    "MLPREFIX",
];

// ---------------------------------------------------------------------------------------
// The replay engine
// ---------------------------------------------------------------------------------------

struct Replay<'a> {
    index: &'a Index,
    ws: &'a Workspace,
    files: &'a Files,
    ds: DataStore,
    /// Files being parsed, outermost first.
    stack: Vec<PathBuf>,
    /// `__bbclasstype == "global"`.
    global: bool,
    recipe_pass: bool,
    pins: Vec<(&'static str, String)>,
    resolver: Option<(Vec<PathBuf>, Resolver)>,
}

impl<'a> Replay<'a> {
    fn new(
        index: &'a Index,
        ws: &'a Workspace,
        files: &'a Files,
        ds: DataStore,
        global: bool,
        recipe_pass: bool,
        pins: Vec<(&'static str, String)>,
    ) -> Replay<'a> {
        Replay {
            index,
            ws,
            files,
            ds,
            stack: Vec::new(),
            global,
            recipe_pass,
            pins,
            resolver: None,
        }
    }

    // ----- files -----

    /// Parses one file: records the visit, sets `FILE` (not for classes) and feeds its
    /// statements. `restore`: put the previous `FILE` back afterwards (every file except
    /// the top-level recipe, which BitBake parses with `include == 0`).
    fn visit(&mut self, abs: &Path, kind: FileKind, via: Vec<Loc>, restore: bool) -> Result<()> {
        let Some(info) = self.files.by_abs.get(abs).cloned() else {
            return Ok(());
        };
        let stmts = query::statements_in(self.index, info.id, &StatementFilter::default())?;
        let end = stmts
            .iter()
            .map(|s| s.loc.lines.end)
            .max()
            .unwrap_or(1)
            .max(1);
        self.ds.record_visit(FileVisit {
            loc: Loc {
                layer: info.layer.clone(),
                path: info.path.clone(),
                lines: LineSpan::new(1, end),
            },
            kind,
            via: via.clone(),
        });
        let is_class = info.kind == FileKind::Class;
        let old_file = if is_class {
            None
        } else {
            let old = self.ds.raw("FILE");
            self.set_file(&abs.to_string_lossy());
            Some(old)
        };
        let classname = if is_class {
            abs.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.stack.push(abs.to_path_buf());
        for st in &stmts {
            self.statement(st, &via, abs, &classname)?;
        }
        self.stack.pop();
        if restore
            && let Some(Some(old)) = old_file
            && old != abs.to_string_lossy()
        {
            self.set_file(&old);
        }
        Ok(())
    }

    fn set_file(&mut self, file: &str) {
        let src = self.content_src("FILE");
        put_keep(&mut self.ds, "FILE", file, src);
        self.refresh_computed();
    }

    fn content_src(&self, name: &str) -> Vec<u64> {
        self.ds
            .vars
            .get(name)
            .and_then(|v| v.content.as_ref())
            .map(|c| c.src.clone())
            .unwrap_or_default()
    }

    /// Recomputes the values OE-Core derives from `FILE` with python (module docs).
    fn refresh_computed(&mut self) {
        if !self.recipe_pass {
            return;
        }
        let file = self.ds.raw("FILE").unwrap_or_default();
        let (pn, pv) = vars_from_file(&file);
        for &name in COMPUTED {
            let var = self.ds.vars.get(name);
            let content = var.and_then(|v| v.content.clone());
            let absent = var.is_none_or(|v| v.content.is_none() && v.default.is_none());
            let ours = self.ds.notes.computed.contains(name);
            let python = content.as_ref().is_some_and(|c| has_python(&c.text));
            if !(absent || ours || python) {
                continue;
            }
            if name == "BP" && !(absent || ours) {
                // OE-Core's BP is plain text; python here is somebody else's.
                continue;
            }
            let text = match name {
                "PN" => pn.clone().unwrap_or_else(|| "defaultpkgname".into()),
                "PV" => pv.clone().unwrap_or_else(|| "1.0".into()),
                "BPN" => self.prune_suffix(),
                "BP" => "${BPN}-${PV}".to_string(),
                _ => Path::new(&file)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
            };
            let src = content.map(|c| c.src).unwrap_or_default();
            put_keep(&mut self.ds, name, &text, src);
            Arc::make_mut(&mut self.ds.notes)
                .computed
                .insert(name.to_string());
        }
    }

    /// `oe.utils.prune_suffix(PN, SPECIAL_PKGSUFFIX, d)`, with the `MLPREFIX` strip.
    fn prune_suffix(&self) -> String {
        let mut pn = self.ds.value("PN").map(|v| v.text).unwrap_or_default();
        let suffixes: Vec<String> = match self.ds.value("SPECIAL_PKGSUFFIX") {
            Some(v) => v.text.split_whitespace().map(str::to_string).collect(),
            None => SPECIAL_PKGSUFFIX.iter().map(|s| s.to_string()).collect(),
        };
        for s in &suffixes {
            if !s.is_empty()
                && let Some(p) = pn.strip_suffix(s.as_str())
            {
                pn = p.to_string();
            }
        }
        if let Some(prefix) = self.ds.value("MLPREFIX").map(|v| v.text)
            && !prefix.is_empty()
            && let Some(p) = pn.strip_prefix(&prefix)
        {
            pn = p.to_string();
        }
        pn
    }

    // ----- statements -----

    fn statement(
        &mut self,
        st: &StatementRow,
        via: &[Loc],
        file: &Path,
        classname: &str,
    ) -> Result<()> {
        match st.kind.as_str() {
            "assign" => self.assign(st, via),
            "export" => {
                if let Some(n) = &st.name {
                    self.ds.mark_exported(n);
                }
            }
            "unset" => {
                if let Some(n) = &st.name {
                    self.ds
                        .unset(n, st.flag.as_deref(), st.loc.clone(), via.to_vec());
                    self.after_write(n, &[], st.flag.as_deref(), true);
                }
            }
            "function" => self.function(st, via, file),
            "pydef" => {
                if let Some(n) = &st.name {
                    put_flag(&mut self.ds, n, "func", "1");
                    put_flag(&mut self.ds, n, "python", "1");
                }
            }
            "inherit" => {
                let text = st.name.clone().unwrap_or_default();
                let deferred = st.op.as_deref() == Some("inherit_defer");
                if deferred && !self.ws.release.release.has(Feature::InheritDefer) {
                    // BitBake before 2.8 has no such directive: the line falls through to
                    // ConfHandler.feeder and fails with "unparsed line" (bitbake-semantics.md,
                    // "Directives"). Recorded, and the class is not inherited.
                    self.issue(
                        Some(&st.loc),
                        via,
                        "inherit_defer",
                        &text,
                        RefProblem::NotInRelease(Feature::InheritDefer),
                    );
                } else if deferred {
                    Arc::make_mut(&mut self.ds.notes).deferred.push(Deferred {
                        text,
                        loc: Some(st.loc.clone()),
                        via: via.to_vec(),
                    });
                } else {
                    self.inherit(&text, Some(&st.loc), via, false, "inherit")?;
                }
            }
            "include" => self.include(st, via, file)?,
            "export_functions" => self.export_functions(st, via, classname),
            // addtask, deltask, addhandler: task and handler bookkeeping, not values.
            _ => {}
        }
        Ok(())
    }

    fn assign(&mut self, st: &StatementRow, via: &[Loc]) {
        let (Some(base), Some(op)) = (&st.base, &st.op) else {
            return;
        };
        let Ok(op) = op.parse::<AssignOp>() else {
            return;
        };
        let var = VarRef {
            base: base.clone(),
            overrides: st.overrides.clone(),
            flag: st.flag.clone(),
            legacy_syntax: st.legacy,
        };
        let name = full_name(&var);
        let exported = is_exported(st);
        let overrides = var.overrides.clone();
        let flag = var.flag.clone();
        let before = self
            .ds
            .vars
            .get(base.as_str())
            .and_then(|v| v.content.clone());
        self.ds.apply(VarOp {
            var,
            op,
            value: st.value.clone().unwrap_or_default(),
            loc: st.loc.clone(),
            via: via.to_vec(),
        });
        if exported && !st.legacy {
            self.ds.mark_exported(&name);
        }
        let after = self
            .ds
            .vars
            .get(base.as_str())
            .and_then(|v| v.content.clone());
        self.after_write(base, &overrides, flag.as_deref(), before != after);
    }

    /// Keeps target pins and computed values in force after a write to `base` (`changed`:
    /// the plain value of `base` is different now).
    fn after_write(&mut self, base: &str, overrides: &[String], flag: Option<&str>, changed: bool) {
        if !overrides.is_empty() || flag.is_some() {
            return;
        }
        if let Some((n, v)) = self.pins.iter().find(|(n, _)| *n == base).cloned() {
            put(&mut self.ds, n, &v);
        }
        if self.recipe_pass && COMPUTED_INPUTS.contains(&base) {
            if changed && COMPUTED.contains(&base) {
                // A plain assignment takes over; python is computed again.
                Arc::make_mut(&mut self.ds.notes).computed.remove(base);
            }
            self.refresh_computed();
        }
    }

    /// `MethodNode.eval`.
    fn function(&mut self, st: &StatementRow, via: &[Loc], file: &Path) {
        let lang = st.op.clone().unwrap_or_default();
        let anonymous = st.name.as_deref() == Some("__anonymous");
        let (var, python) = if anonymous {
            let tr: String = file
                .to_string_lossy()
                .chars()
                .map(|c| if "/.+-@%&~".contains(c) { '_' } else { c })
                .collect();
            let name = format!("__anon_{}_{tr}", st.loc.lines.end);
            (
                VarRef {
                    base: name,
                    overrides: Vec::new(),
                    flag: None,
                    legacy_syntax: false,
                },
                true,
            )
        } else {
            let Some(base) = &st.base else { return };
            (
                VarRef {
                    base: base.clone(),
                    overrides: st.overrides.clone(),
                    flag: None,
                    legacy_syntax: st.legacy,
                },
                lang.contains("python"),
            )
        };
        let key = full_name(&var);
        if !var.legacy_syntax && self.ds.raw(&key).is_some() {
            let v = self.ds.var_mut(&key);
            v.flags.remove("python");
            v.flags.remove("fakeroot");
        }
        let legacy = var.legacy_syntax;
        self.ds.apply(VarOp {
            var,
            op: AssignOp::Set,
            value: format!("{}\n", st.loc),
            loc: st.loc.clone(),
            via: via.to_vec(),
        });
        if !legacy {
            if python {
                put_flag(&mut self.ds, &key, "python", "1");
            }
            if lang.starts_with("fakeroot") {
                put_flag(&mut self.ds, &key, "fakeroot", "1");
            }
            put_flag(&mut self.ds, &key, "func", "1");
        }
        if anonymous {
            self.anonymous(st, via, key);
        }
    }

    /// Records an anonymous python function and the variables its body changes by name.
    fn anonymous(&mut self, st: &StatementRow, via: &[Loc], name: String) {
        let body = self.body_text(&st.loc);
        let (sets, opaque) = match &body {
            Some(b) => python_writes(b),
            None => (Vec::new(), true),
        };
        let mut bases = Vec::new();
        for s in &sets {
            let e = self.ds.expand(s);
            bases.push(base_of(&e.text).to_string());
        }
        let notes = Arc::make_mut(&mut self.ds.notes);
        notes.python_vars.extend(bases);
        notes.anonymous.push(AnonPython {
            loc: st.loc.clone(),
            via: via.to_vec(),
            name,
            sets,
            opaque,
        });
        self.ds.ovr_cache = OnceLock::new();
    }

    /// The text of the lines `loc` covers, read from disk (the index has no bodies).
    fn body_text(&self, loc: &Loc) -> Option<String> {
        let abs = self.ws.abs_path(loc)?;
        let abs = self.ws.confine(&abs).ok()?;
        let text = fs::read_to_string(abs).ok()?;
        let lines: Vec<&str> = text
            .lines()
            .skip(loc.lines.start.saturating_sub(1) as usize)
            .take((loc.lines.end + 1).saturating_sub(loc.lines.start) as usize)
            .collect();
        Some(lines.join("\n"))
    }

    /// `ExportFuncsNode.eval`.
    fn export_functions(&mut self, st: &StatementRow, via: &[Loc], classname: &str) {
        const MARK: &str = " -> ";
        let names = st.name.clone().unwrap_or_default();
        for func in names.split_whitespace() {
            let called = format!("{classname}_{func}");
            let current = self.ds.raw(func);
            if current.as_ref().is_some_and(|b| !b.contains(MARK)) {
                continue;
            }
            if current.is_some() {
                let v = self.ds.var_mut(func);
                v.flags.remove("python");
                v.flags.remove("func");
            }
            for flag in ["func", "python"] {
                if let Some(f) = self.ds.raw_flag(&called, flag) {
                    put_flag(&mut self.ds, func, flag, &f);
                }
            }
            for flag in ["dirs", "cleandirs", "fakeroot"] {
                if let Some(f) = self.ds.raw_flag(func, flag) {
                    put_flag(&mut self.ds, &called, flag, &f);
                }
            }
            self.ds.apply(VarOp {
                var: VarRef {
                    base: func.to_string(),
                    overrides: Vec::new(),
                    flag: None,
                    legacy_syntax: false,
                },
                op: AssignOp::Set,
                value: format!("{}{MARK}{called}\n", st.loc),
                loc: st.loc.clone(),
                via: via.to_vec(),
            });
        }
    }

    // ----- references -----

    fn issue(
        &mut self,
        loc: Option<&Loc>,
        via: &[Loc],
        kind: &str,
        text: &str,
        problem: RefProblem,
    ) {
        Arc::make_mut(&mut self.ds.notes).references.push(RefIssue {
            loc: loc.cloned(),
            via: via.to_vec(),
            kind: kind.to_string(),
            text: text.to_string(),
            problem,
        });
    }

    /// Expands a reference argument; `None` (and an issue) if it needs python or undefined
    /// variables.
    fn expand_ref(
        &mut self,
        text: &str,
        loc: Option<&Loc>,
        via: &[Loc],
        kind: &str,
    ) -> Option<String> {
        let e = self.ds.expand(text);
        if e.unevaluated_python {
            self.issue(loc, via, kind, text, RefProblem::Python);
            return None;
        }
        if !e.unresolved.is_empty() {
            self.issue(loc, via, kind, text, RefProblem::Undefined(e.unresolved));
            return None;
        }
        Some(e.text)
    }

    /// `BBPATH` as the store has it now (absolute entries, canonical when they exist);
    /// the workspace's configured `BBPATH` if it is empty or cannot be expanded.
    fn bbpath(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        if let Some(v) = self.ds.value("BBPATH")
            && !v.unevaluated_python
        {
            for e in v
                .text
                .split(':')
                .filter(|e| !e.is_empty() && !e.contains("${"))
            {
                let p = normalize(Path::new(e));
                if !p.is_absolute() {
                    continue;
                }
                let p = fs::canonicalize(&p).unwrap_or(p);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        if out.is_empty() {
            out = resolve::bbpath(self.ws);
        }
        out
    }

    fn resolver(&mut self) -> &Resolver {
        let bbpath = self.bbpath();
        if self.resolver.as_ref().is_some_and(|(b, _)| *b != bbpath) {
            self.resolver = None;
        }
        let files = self.files;
        &self
            .resolver
            .get_or_insert_with(|| {
                let r = Resolver::with_bbpath(bbpath.clone()).with_known_files(files.known.clone());
                (bbpath, r)
            })
            .1
    }

    /// `include` / `require` / `include_all` (`ConfHandler.include`).
    fn include(&mut self, st: &StatementRow, via: &[Loc], from: &Path) -> Result<()> {
        let mode = st.op.clone().unwrap_or_else(|| "include".into());
        let text = st.name.clone().unwrap_or_default();
        let Some(expanded) = self.expand_ref(&text, Some(&st.loc), via, &mode) else {
            return Ok(());
        };
        let words: Vec<String> = if mode == "include_all" {
            let w = expanded.trim();
            if w.is_empty() {
                Vec::new()
            } else {
                vec![w.to_string()]
            }
        } else {
            expanded.split_whitespace().map(str::to_string).collect()
        };
        let mut child_via = via.to_vec();
        child_via.push(st.loc.clone());
        for word in words {
            let ws = self.ws;
            let targets: Vec<PathBuf> = if mode == "include_all" {
                self.resolver().include_all(ws, &word)
            } else {
                self.resolver()
                    .find_in_bbpath(ws, &word, from)
                    .into_iter()
                    .collect()
            };
            if targets.is_empty() && mode == "require" {
                self.issue(
                    Some(&st.loc),
                    via,
                    &mode,
                    &text,
                    RefProblem::NotFound(word.clone()),
                );
            }
            for t in targets {
                if self.cycle(&t, &st.loc, via, &mode, &text) {
                    continue;
                }
                let kind = FileKind::from_path(&t).unwrap_or(FileKind::Include);
                self.visit(&t, kind, child_via.clone(), true)?;
            }
        }
        Ok(())
    }

    /// True (and an issue) if `t` is already being parsed.
    fn cycle(&mut self, t: &Path, loc: &Loc, via: &[Loc], kind: &str, text: &str) -> bool {
        if !self.stack.iter().any(|s| s == t) {
            return false;
        }
        let at = self
            .files
            .by_abs
            .get(t)
            .map(|f| Loc {
                layer: f.layer.clone(),
                path: f.path.clone(),
                lines: LineSpan::line(1),
            })
            .unwrap_or_else(|| loc.clone());
        self.issue(Some(loc), via, kind, text, RefProblem::Cycle(at));
        true
    }

    /// `BBHandler.inherit`: each word of the expanded text; words in `BB_DEFER_BBCLASSES`
    /// wait for the end of the recipe unless `deferred`; a class already in
    /// `__inherit_cache` is skipped.
    fn inherit(
        &mut self,
        text: &str,
        loc: Option<&Loc>,
        via: &[Loc],
        deferred: bool,
        kind: &str,
    ) -> Result<()> {
        let Some(expanded) = self.expand_ref(text, loc, via, kind) else {
            return Ok(());
        };
        let mut child_via = via.to_vec();
        if let Some(l) = loc {
            child_via.push(l.clone());
        }
        for word in expanded.split_whitespace() {
            let defer = self
                .ds
                .value("BB_DEFER_BBCLASSES")
                .map(|v| v.text.split_whitespace().any(|w| w == word))
                .unwrap_or(false);
            if !deferred && defer {
                Arc::make_mut(&mut self.ds.notes).deferred.push(Deferred {
                    text: word.to_string(),
                    loc: loc.cloned(),
                    via: via.to_vec(),
                });
                continue;
            }
            let ws = self.ws;
            let global = self.global;
            let Some(f) = self.resolver().find_class(ws, word, global) else {
                self.issue(loc, via, kind, text, RefProblem::NotFound(word.to_string()));
                continue;
            };
            if self.ds.notes.inherit_cache.contains(&f) {
                continue;
            }
            Arc::make_mut(&mut self.ds.notes)
                .inherit_cache
                .push(f.clone());
            self.visit(&f, FileKind::Class, child_via.clone(), true)?;
        }
        Ok(())
    }

    /// The `via` chain for something a variable's value pulled in (`BBLAYERS` for the layer
    /// confs, `INHERIT` for a global class): the chain of the first applied operation whose
    /// value contains `word` (any applied operation when `word` is `None`, the last one),
    /// plus that operation.
    fn via_of_word(&self, var: &str, word: Option<&str>) -> Vec<Loc> {
        let mut applied = self
            .ds
            .history(var)
            .into_iter()
            .filter(|(_, s)| *s == OpStatus::Applied)
            .map(|(op, _)| op);
        let op = match word {
            None => applied.next_back(),
            Some(w) => applied.into_iter().find(|op| {
                self.ds
                    .expand(&op.value)
                    .text
                    .split_whitespace()
                    .any(|x| x == w)
            }),
        };
        match op {
            Some(op) => {
                let mut v = op.via.clone();
                v.push(op.loc.clone());
                v
            }
            None => Vec::new(),
        }
    }
}

/// Whether an assignment was written `export VAR = ...` (the index keeps the statement's
/// first line in `snippet`; the assignment regex allows `export` only at the start).
fn is_exported(st: &StatementRow) -> bool {
    let Some(rest) = st.snippet.strip_prefix("export") else {
        return false;
    };
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    st.name.as_deref() != Some("export") || rest.trim_start().starts_with("export")
}

/// Variables a python body changes: literal first arguments of the `d.setVar` family, and
/// whether it also writes through a computed name or hands `d` to other code.
fn python_writes(body: &str) -> (Vec<String>, bool) {
    static WRITE: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r#"\bd\.(?:setVar|appendVar|prependVar|delVar|renameVar|setVarFlag|setVarFlags|appendVarFlag|prependVarFlag|delVarFlag|delVarFlags)\s*\(\s*(?:'([^'\\]*)'|"([^"\\]*)")?"#,
        )
        .ok()
    });
    static PASS_D: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"[\w.\]]\s*\((?:[^()]*,)?\s*d\s*[,)]").ok());
    let (Some(write), Some(pass_d)) = (WRITE.as_ref(), PASS_D.as_ref()) else {
        return (Vec::new(), true);
    };
    let mut sets = Vec::new();
    let mut opaque = false;
    for c in write.captures_iter(body) {
        match c.get(1).or_else(|| c.get(2)) {
            Some(m) => {
                let n = m.as_str().to_string();
                if !sets.contains(&n) {
                    sets.push(n);
                }
            }
            None => opaque = true,
        }
    }
    if pass_d.is_match(body) {
        opaque = true;
    }
    (sets, opaque)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars_from_file_like_bitbake() {
        assert_eq!(
            vars_from_file("/a/hello_1.1.bb"),
            (Some("hello".into()), Some("1.1".into()))
        );
        assert_eq!(
            vars_from_file("/a/hello_%.bbappend"),
            (Some("hello".into()), Some("%".into()))
        );
        assert_eq!(vars_from_file("/a/app.bb"), (Some("app".into()), None));
        assert_eq!(vars_from_file("/a/hello.inc"), (None, None));
        assert_eq!(vars_from_file("/a/a_b_c_d.bb"), (None, None));
    }

    #[test]
    fn re_escape_like_python() {
        assert_eq!(re_escape("/a/meta-x.y"), "/a/meta\\-x\\.y");
        assert_eq!(re_escape("a_b/c"), "a_b/c");
    }

    #[test]
    fn python_writes_finds_literal_names() {
        let (sets, opaque) = python_writes(
            "python () {\n    if d.getVar('X'):\n        d.appendVar('EXTRA_OEMAKE', ' B=1')\n        d.setVar(\"RDEPENDS:${PN}\", 'x')\n}",
        );
        assert_eq!(sets, vec!["EXTRA_OEMAKE", "RDEPENDS:${PN}"]);
        assert!(!opaque);
        let (_, opaque) = python_writes("    d.setVar(name, 'x')");
        assert!(opaque);
        let (_, opaque) = python_writes("    oe.utils.something(d)");
        assert!(opaque);
        let (_, opaque) = python_writes("    x = d.getVar('A')");
        assert!(!opaque);
    }

    #[test]
    fn pieces_parse() {
        let p = parse_piece("meta-core:classes/base.bbclass:54 -> base_do_install").unwrap();
        assert_eq!(p.calls.as_deref(), Some("base_do_install"));
        assert_eq!(p.loc.to_string(), "meta-core:classes/base.bbclass:54");
        assert!(parse_piece("not a pointer").is_none());
    }
}
