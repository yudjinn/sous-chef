//! `bblayers.conf` and `layer.conf` reading.
//!
//! BitBake behaviour reproduced here (BitBake `lib/bb/cookerdata.py`, `lib/bb/cooker.py`,
//! `lib/bb/parse/ast.py`, `lib/bb/parse/parse_py/ConfHandler.py`, `lib/bb/data_smart.py`):
//!
//! - `bblayers.conf` is found by walking up from the working directory looking for
//!   `conf/bblayers.conf`; `TOPDIR` is the directory that contains that `conf/`.
//! - `BBLAYERS` is expanded and split on whitespace. Each entry has a trailing `/` stripped
//!   and must be an existing directory.
//! - Each layer's `conf/layer.conf` is parsed in `BBLAYERS` order into one shared datastore,
//!   with `LAYERDIR` (and `LAYERDIR_RE`) set to the layer path; after each file every
//!   `${LAYERDIR}` reference in the datastore is replaced by that layer's path
//!   (`expandVarref('LAYERDIR')`).
//! - `BBFILE_COLLECTIONS` is split on whitespace. `BBFILE_PRIORITY_<c>` must be an integer;
//!   an unset (or `0`) priority is computed as one more than the highest priority among the
//!   collection's `LAYERDEPENDS`, starting from the lowest explicit priority.
//! - `LAYERDEPENDS_<c>` uses the `explode_dep_versions2` format (`a (>= 2) b`): only the
//!   names are kept.
//! - Files are parsed with [`crate::parse::parse`]. `:append` / `:prepend` / `:remove`
//!   (`BBLAYERS:append = " ..."`), override-qualified names, `unset`, and `include` /
//!   `require` / `include_all` whose argument expands statically are handled (see
//!   [`Vars`] and `read_conf`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::parse::{IncludeMode, StatementKind};
use crate::types::{AssignOp, VarRef};

use super::Layer;

/// How deep [`scan_for_layers`] looks below the start directory when there is no build
/// directory (the start directory itself is depth 0).
pub const SCAN_DEPTH: usize = 4;

/// Walks up from `start` (inclusive) looking for `conf/bblayers.conf`; returns the
/// directory containing `conf/` (BitBake's `TOPDIR`).
pub fn find_build_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("conf").join("bblayers.conf").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// What `bblayers.conf` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BbLayers {
    /// Layer roots, expanded, in `BBLAYERS` order, trailing `/` stripped (not canonicalised).
    pub layers: Vec<PathBuf>,
    /// `BBLAYERS` entries that could not be expanded statically (python `${@...}` or a
    /// variable not assigned in the file), as written after partial expansion.
    pub unresolved: Vec<String>,
    /// Variables as left by `bblayers.conf` (unexpanded values), plus `TOPDIR`. Seed for
    /// reading the layer confs.
    vars: Vars,
    /// The build directory (`TOPDIR`).
    topdir: PathBuf,
}

/// Reads `<build_dir>/conf/bblayers.conf`. `build_dir` becomes `TOPDIR`.
///
/// `include` / `require` / `include_all` inside the file are followed when their argument
/// expands statically; only files inside `build_dir` are read.
pub fn read_bblayers(build_dir: &Path) -> Result<BbLayers> {
    let path = build_dir.join("conf").join("bblayers.conf");
    let mut vars = Vars::default();
    vars.set("TOPDIR", &build_dir.to_string_lossy());
    read_conf(&path, &mut vars, &[canonical_or_self(build_dir)], 0)?;
    let mut out = BbLayers::default();
    let expanded = vars.value("BBLAYERS").unwrap_or_default();
    for word in split_words(&expanded) {
        if word.contains("${") {
            out.unresolved.push(word);
            continue;
        }
        let trimmed = word.trim_end_matches('/');
        let p = if trimmed.is_empty() { "/" } else { trimmed };
        out.layers.push(PathBuf::from(p));
    }
    out.vars = vars;
    out.topdir = build_dir.to_path_buf();
    Ok(out)
}

/// The per-layer data read from `conf/layer.conf` files. `name` is the directory name
/// (disambiguation is done by the caller) and `writable` is false.
///
/// `roots` must be absolute and existing. `seed` is the datastore left by `bblayers.conf`
/// (or `None` without a build directory).
pub fn read_layer_confs(roots: &[PathBuf], seed: Option<&BbLayers>) -> Result<Vec<Layer>> {
    Ok(read_layer_confs_vars(roots, seed)?.0)
}

/// [`read_layer_confs`], plus the words of `LAYERSERIES_CORENAMES` as left once every
/// `layer.conf` has been read (see [`read_corenames`]). Reads each file once.
pub fn read_layer_confs_with_corenames(
    roots: &[PathBuf],
    seed: Option<&BbLayers>,
) -> Result<(Vec<Layer>, Vec<String>)> {
    let (layers, vars) = read_layer_confs_vars(roots, seed)?;
    Ok((layers, words_of(&vars, "LAYERSERIES_CORENAMES")))
}

/// `LAYERSERIES_CORENAMES` once `bblayers.conf` (if `build_dir` is given) and every
/// `layer.conf` of `roots` (in order) have been read: the expanded value split on
/// whitespace. OE-core sets it in `meta/conf/layer.conf`; it is empty when no layer sets it
/// (a layer repository checked out without OE-core). Like [`read_bbpath`], this re-reads the
/// configuration files; `Workspace::discover` uses [`read_layer_confs_with_corenames`].
pub fn read_corenames(build_dir: Option<&Path>, roots: &[PathBuf]) -> Result<Vec<String>> {
    let seed = match build_dir {
        Some(b) => Some(read_bblayers(b)?),
        None => None,
    };
    let (_, vars) = read_layer_confs_vars(roots, seed.as_ref())?;
    Ok(words_of(&vars, "LAYERSERIES_CORENAMES"))
}

/// `BBPATH` as configured once `bblayers.conf` (if `build_dir` is given) and every
/// `layer.conf` of `roots` (in order) have been read: the expanded value split on `:`, with
/// empty entries dropped, in order. `roots` are the layer roots in `BBLAYERS` order, as
/// stored in `Layer.root`.
///
/// This re-reads the configuration files; callers that resolve many paths compute it once
/// (`workspace::resolve::Resolver`).
pub fn read_bbpath(build_dir: Option<&Path>, roots: &[PathBuf]) -> Result<Vec<String>> {
    let seed = match build_dir {
        Some(b) => Some(read_bblayers(b)?),
        None => None,
    };
    let (_, vars) = read_layer_confs_vars(roots, seed.as_ref())?;
    Ok(vars
        .value("BBPATH")
        .unwrap_or_default()
        .split(':')
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect())
}

fn read_layer_confs_vars(roots: &[PathBuf], seed: Option<&BbLayers>) -> Result<(Vec<Layer>, Vars)> {
    let mut vars = seed.map(|s| s.vars.clone()).unwrap_or_default();
    // Files the layer confs may include: anything in the build directory or a layer.
    let mut allowed: Vec<PathBuf> = roots.iter().map(|r| canonical_or_self(r)).collect();
    if let Some(s) = seed {
        allowed.push(canonical_or_self(&s.topdir));
    }
    let mut layers = Vec::with_capacity(roots.len());
    for root in roots {
        let conf = root.join("conf").join("layer.conf");
        let layerdir = root.to_string_lossy().into_owned();
        let layerdir_re = python_re_escape(&layerdir);
        let before_coll = words_of(&vars, "BBFILE_COLLECTIONS");
        let before_files = words_of(&vars, "BBFILES");
        vars.set("LAYERDIR", &layerdir);
        vars.set("LAYERDIR_RE", &layerdir_re);
        read_conf(&conf, &mut vars, &allowed, 0)?;
        vars.replace_ref("LAYERDIR", &layerdir);
        vars.replace_ref("LAYERDIR_RE", &layerdir_re);
        let collections = added_words(&before_coll, &words_of(&vars, "BBFILE_COLLECTIONS"));
        let bbfiles = added_words(&before_files, &words_of(&vars, "BBFILES"));
        layers.push(Layer {
            name: root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| layerdir.clone()),
            root: root.clone(),
            collections,
            priority: 0,
            compat: Vec::new(),
            depends: Vec::new(),
            bbfiles,
            writable: false,
        });
    }
    // cookerdata deletes both once every layer.conf has been read.
    vars.del("LAYERDIR_RE");
    vars.del("LAYERDIR");

    // Per-collection values, read once every layer.conf has been parsed (as BitBake does
    // in handleCollections).
    let mut explicit: HashMap<String, Option<i32>> = HashMap::new();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for layer in &mut layers {
        for c in &layer.collections {
            let prio = match vars.value(&format!("BBFILE_PRIORITY_{c}")) {
                None => None,
                Some(text) => {
                    let n: i32 = text.trim().parse().map_err(|_| Error::Config {
                        msg: format!(
                            "invalid value for BBFILE_PRIORITY_{c} ({text:?}) in {}; it must be an integer",
                            layer.root.join("conf/layer.conf").display()
                        ),
                    })?;
                    Some(n)
                }
            };
            explicit.insert(c.clone(), prio);
            let d = vars
                .value(&format!("LAYERDEPENDS_{c}"))
                .map(|v| dep_names(&v))
                .unwrap_or_default();
            deps.insert(c.clone(), d);
        }
        if let Some(c) = layer.collections.first() {
            layer.depends = deps.get(c).cloned().unwrap_or_default();
            layer.compat = vars
                .value(&format!("LAYERSERIES_COMPAT_{c}"))
                .map(|v| split_words(&v))
                .unwrap_or_default();
        }
    }
    let min_prio = explicit
        .values()
        .flatten()
        .copied()
        .filter(|&p| p != 0)
        .min()
        .unwrap_or(0);
    let mut memo: HashMap<String, i32> = HashMap::new();
    for layer in &mut layers {
        if let Some(c) = layer.collections.first() {
            layer.priority = collection_priority(c, &explicit, &deps, min_prio, &mut memo, 0);
        }
    }
    Ok((layers, vars))
}

/// BitBake `calc_layer_priority`: an explicit non-zero priority is used as is; otherwise
/// the highest priority among the dependencies (at least `min_prio`) plus one.
fn collection_priority(
    c: &str,
    explicit: &HashMap<String, Option<i32>>,
    deps: &HashMap<String, Vec<String>>,
    min_prio: i32,
    memo: &mut HashMap<String, i32>,
    depth: usize,
) -> i32 {
    if let Some(&Some(p)) = explicit.get(c)
        && p != 0
    {
        return p;
    }
    if let Some(&p) = memo.get(c) {
        return p;
    }
    let mut max_dep = min_prio;
    if depth < 64 {
        for d in deps.get(c).map(Vec::as_slice).unwrap_or_default() {
            if explicit.contains_key(d) {
                let p = collection_priority(d, explicit, deps, min_prio, memo, depth + 1);
                max_dep = max_dep.max(p);
            }
        }
    }
    let p = max_dep.saturating_add(1);
    memo.insert(c.to_string(), p);
    p
}

/// Every directory at most `depth` levels below `start` (inclusive) that contains
/// `conf/layer.conf`, sorted by path. Hidden directories are skipped and symlinks are not
/// followed.
pub fn scan_for_layers(start: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(start.to_path_buf(), 0usize)];
    while let Some((dir, d)) = stack.pop() {
        if dir.join("conf").join("layer.conf").is_file() {
            out.push(dir.clone());
        }
        if d >= depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            stack.push((entry.path(), d + 1));
        }
    }
    out.sort();
    out
}

/// How deep `include` / `require` may nest inside the layer configuration. BitBake itself
/// only refuses a file that includes itself; this stops include cycles.
const MAX_INCLUDE_DEPTH: usize = 32;

/// Parses one configuration file into `vars`, in statement order:
///
/// - assignments, with the operator semantics of `bb/parse/ast.py` (see [`Vars`]); flag
///   assignments (`VAR[flag]`) are ignored;
/// - `unset VAR` deletes the variable;
/// - `include` / `require` / `include_all` (`ConfHandler.include`) are followed when the
///   argument expands statically. An argument that still contains `${` after expansion
///   (python, or a variable not set here) is skipped with a warning on stderr. Only files
///   inside `allowed` (canonical roots) are read; others are skipped with a warning. A
///   `require` whose file does not exist is an error, as in BitBake.
///
/// Every other statement is ignored (BitBake rejects most of them in a `.conf` file).
fn read_conf(path: &Path, vars: &mut Vars, allowed: &[PathBuf], depth: usize) -> Result<()> {
    let src = read_text(path)?;
    let parsed = crate::parse::parse(&src);
    for st in parsed.statements {
        match st.kind {
            StatementKind::Assign { var, op, value, .. } => {
                if var.flag.is_none() {
                    vars.assign(&full_name(&var), op, &value);
                }
            }
            StatementKind::Unset { name, flag: None } => vars.del(&name),
            StatementKind::Include { path: what, mode } => {
                include(path, &what, mode, vars, allowed, depth)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The variable name as written, rebuilt from its split form (legacy `_` names are rebuilt
/// with `_`; BitBake rejects most of them anyway).
fn full_name(var: &VarRef) -> String {
    let sep = if var.legacy_syntax { "_" } else { ":" };
    let mut s = var.base.clone();
    for o in &var.overrides {
        s.push_str(sep);
        s.push_str(o);
    }
    s
}

/// `IncludeNode.eval` / `IncludeAllNode.eval`, then `ConfHandler.include`.
fn include(
    parent: &Path,
    what: &str,
    mode: IncludeMode,
    vars: &mut Vars,
    allowed: &[PathBuf],
    depth: usize,
) -> Result<()> {
    let expanded = vars.expand(what);
    if expanded.contains("${") {
        eprintln!(
            "sous-chef: warning: {} {what:?} in {} cannot be expanded statically; skipped",
            include_verb(mode),
            parent.display()
        );
        return Ok(());
    }
    if depth >= MAX_INCLUDE_DEPTH {
        return Err(Error::Config {
            msg: format!(
                "includes nested more than {MAX_INCLUDE_DEPTH} deep at {}; check for an include cycle",
                parent.display()
            ),
        });
    }
    match mode {
        IncludeMode::IncludeAll => {
            // A directory listed more than once in BBPATH is searched once. The joined path
            // is split on whitespace again by `ConfHandler.include`.
            let bbpath = vars.value("BBPATH").unwrap_or_default();
            let mut seen: Vec<&str> = Vec::new();
            for entry in bbpath.split(':') {
                if seen.contains(&entry) {
                    continue;
                }
                seen.push(entry);
                let joined = Path::new(entry).join(&expanded);
                for word in joined.to_string_lossy().split_whitespace() {
                    include_single(parent, word, false, vars, allowed, depth)?;
                }
            }
        }
        IncludeMode::Include | IncludeMode::Require => {
            let required = mode == IncludeMode::Require;
            for word in expanded.split_whitespace() {
                include_single(parent, word, required, vars, allowed, depth)?;
            }
        }
    }
    Ok(())
}

fn include_verb(mode: IncludeMode) -> &'static str {
    match mode {
        IncludeMode::Include => "include",
        IncludeMode::Require => "require",
        IncludeMode::IncludeAll => "include_all",
    }
}

/// `ConfHandler.include_single_file`: a relative name is searched in the including file's
/// directory, then along `BBPATH` (the first existing path wins, `bb.utils.which`).
fn include_single(
    parent: &Path,
    name: &str,
    required: bool,
    vars: &mut Vars,
    allowed: &[PathBuf],
    depth: usize,
) -> Result<()> {
    if parent.as_os_str() == name {
        return Ok(());
    }
    let found = if Path::new(name).is_absolute() {
        Some(PathBuf::from(name)).filter(|p| p.exists())
    } else {
        let dname = parent
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bbpath = vars.value("BBPATH").unwrap_or_default();
        std::iter::once(dname.as_str())
            .chain(bbpath.split(':'))
            .map(|dir| Path::new(dir).join(name))
            .find(|p| p.exists())
    };
    let Some(file) = found else {
        if required {
            return Err(Error::Config {
                msg: format!(
                    "could not include required file {name} (from {}); fix the require line or the layer list",
                    parent.display()
                ),
            });
        }
        return Ok(());
    };
    let inside = fs::canonicalize(&file)
        .map(|c| allowed.iter().any(|root| c.starts_with(root)))
        .unwrap_or(false);
    if !inside {
        eprintln!(
            "sous-chef: warning: {} includes {}, which is outside the build directory and the layers; skipped",
            parent.display(),
            file.display()
        );
        return Ok(());
    }
    read_conf(&file, vars, allowed, depth + 1)
}

fn canonical_or_self(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Python's `re.escape` (3.7+): escapes the characters that are special in a regular
/// expression.
fn python_re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\u{b}\u{c}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| Error::Config {
        msg: format!("cannot read {}: {e}", path.display()),
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn words_of(vars: &Vars, name: &str) -> Vec<String> {
    vars.value(name)
        .map(|v| split_words(&v))
        .unwrap_or_default()
}

/// The words of `after` that the file added to `before`: the suffix when `after` extends
/// `before` (the usual `+=`), otherwise the words not already present.
fn added_words(before: &[String], after: &[String]) -> Vec<String> {
    if after.starts_with(before) {
        return after[before.len()..].to_vec();
    }
    after
        .iter()
        .filter(|w| !before.contains(w))
        .cloned()
        .collect()
}

/// Names from an `explode_dep_versions2` string: `a (>= 2) b` -> `[a, b]`.
fn dep_names(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Splits on whitespace, keeping a `${...}` (which may contain spaces) inside one word.
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            depth += 1;
            cur.push(ch);
            if let Some(c) = chars.next() {
                cur.push(c);
            }
            continue;
        }
        if depth > 0 {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            cur.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `:append`, `:prepend` or `:remove`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keyword {
    Append,
    Prepend,
    Remove,
}

/// `data_smart.py` `__setvar_regexp__`
/// (`(?P<base>.*?)(?P<keyword>:append|:prepend|:remove)(:(?P<add>[^A-Z]*))?$`): the base
/// variable, the keyword and the override condition (`A:append:foo` -> `A`, append, `foo`).
fn split_keyword(var: &str) -> Option<(&str, Keyword, Option<&str>)> {
    const KEYWORDS: [(&str, Keyword); 3] = [
        (":append", Keyword::Append),
        (":prepend", Keyword::Prepend),
        (":remove", Keyword::Remove),
    ];
    for (i, _) in var.match_indices(':') {
        let rest = &var[i..];
        for (text, kw) in KEYWORDS {
            let Some(after) = rest.strip_prefix(text) else {
                continue;
            };
            if after.is_empty() {
                return Some((&var[..i], kw, None));
            }
            if let Some(add) = after.strip_prefix(':')
                && !add.chars().any(|c| c.is_ascii_uppercase())
            {
                return Some((&var[..i], kw, Some(add).filter(|a| !a.is_empty())));
            }
        }
    }
    None
}

/// `__override_regexp__.match`: starts with `[a-z0-9]`.
fn override_like(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// A small datastore modelled on `data_smart.py`, enough for the layer configuration:
///
/// - `content`: `_content` of every variable, keyed by the full name (`A`, `A:foo`);
/// - `weak`: `??=` weak defaults (`_defaultval`);
/// - `pending`: `:append` / `:prepend` / `:remove` operations of a variable, in order, with
///   their override condition. They are applied when the variable is read (not during
///   parsing, so `+=` and friends never see them), appends first, then prepends, then
///   removes on the expanded value;
/// - `variants`: `overridedata`, the override-qualified names (`A:foo`) of a variable. A
///   variant replaces the variable when its overrides are in `OVERRIDES`.
///
/// `OVERRIDES` itself is read without override processing (it is rarely set this early).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Vars {
    content: HashMap<String, String>,
    weak: HashMap<String, String>,
    pending: HashMap<String, Vec<(Keyword, String, Option<String>)>>,
    variants: HashMap<String, Vec<(String, String)>>,
}

impl Vars {
    /// `setVar(var, value, parsing=True)`.
    fn set(&mut self, var: &str, value: &str) {
        if let Some((base, kw, cond)) = split_keyword(var) {
            self.pending.entry(base.to_string()).or_default().push((
                kw,
                value.to_string(),
                cond.map(str::to_string),
            ));
            return;
        }
        self.content.insert(var.to_string(), value.to_string());
        if var.contains(':') {
            self.update_overrides(var);
        }
    }

    /// `setVar(var, value)` outside parsing (as `expandVarref` does): also drops the
    /// variable's pending `:append` / `:prepend` / `:remove` and its active variants.
    fn set_final(&mut self, var: &str, value: &str) {
        if split_keyword(var).is_none() {
            self.pending.remove(var);
            if let Some(list) = self.variants.remove(var) {
                let ov = self.active_overrides();
                for (r, o) in list {
                    if cond_active(&o, &ov) {
                        self.del(&r);
                    }
                }
            }
        }
        self.set(var, value);
    }

    /// `_setvar_update_overrides`: records `var` as a variant of each shorter name.
    fn update_overrides(&mut self, var: &str) {
        let Some(i) = var.rfind(':') else { return };
        let mut ov = var[i + 1..].to_string();
        let mut short = var[..i].to_string();
        while !ov.is_empty() && override_like(&ov) {
            let entry = (var.to_string(), ov.clone());
            let list = self.variants.entry(short.clone()).or_default();
            if !list.contains(&entry) {
                list.push(entry);
            }
            ov.clear();
            if let Some(j) = short.rfind(':') {
                ov = var[j + 1..].to_string();
                short = var[..j].to_string();
                if short.is_empty() {
                    ov.clear();
                }
            }
        }
    }

    /// `delVar`.
    fn del(&mut self, var: &str) {
        self.content.remove(var);
        self.weak.remove(var);
        self.pending.remove(var);
        self.variants.remove(var);
        let Some(i) = var.rfind(':') else { return };
        let mut ov = var[i + 1..].to_string();
        let mut short = var[..i].to_string();
        while !ov.is_empty() && override_like(&ov) {
            if let Some(list) = self.variants.get_mut(&short) {
                list.retain(|(r, o)| !(r == var && *o == ov));
            }
            ov.clear();
            if let Some(j) = short.rfind(':') {
                ov = var[j + 1..].to_string();
                short = var[..j].to_string();
                if short.is_empty() {
                    ov.clear();
                }
            }
        }
    }

    /// One assignment, with the operator semantics of `bb/parse/ast.py` (`DataNode`): the
    /// current value used by `?=`, `+=` and friends is read with `parsing=True,
    /// noweakdefault=True`, so it ignores weak defaults and pending `:append`s.
    fn assign(&mut self, name: &str, op: AssignOp, value: &str) {
        let cur = self.content.get(name).cloned();
        let new = match op {
            AssignOp::WeakDefault => {
                self.weak.insert(name.to_string(), value.to_string());
                if name.contains(':') {
                    self.update_overrides(name);
                }
                return;
            }
            AssignOp::Set => value.to_string(),
            AssignOp::Default => match cur {
                Some(_) => return,
                None => value.to_string(),
            },
            AssignOp::Immediate => self.expand(value),
            AssignOp::Append => format!("{} {}", cur.unwrap_or_default(), value),
            AssignOp::Prepend => format!("{} {}", value, cur.unwrap_or_default()),
            AssignOp::AppendDot => format!("{}{}", cur.unwrap_or_default(), value),
            AssignOp::PrependDot => format!("{}{}", value, cur.unwrap_or_default()),
        };
        self.set(name, &new);
    }

    /// The overrides in `OVERRIDES`, in order (lowest priority first).
    fn active_overrides(&self) -> Vec<String> {
        let Some(raw) = self
            .content
            .get("OVERRIDES")
            .or_else(|| self.weak.get("OVERRIDES"))
        else {
            return Vec::new();
        };
        self.expand_with(raw, &[], 0)
            .split(':')
            .map(str::to_string)
            .collect()
    }

    /// `getVar(name, expand=False)`: the winning variant, else the value, else the weak
    /// default; then active `:append`s and `:prepend`s.
    fn get(&self, name: &str) -> Option<String> {
        self.get_with(name, &self.active_overrides(), 0)
    }

    fn get_with(&self, name: &str, ov: &[String], depth: usize) -> Option<String> {
        let mut value = None;
        if depth < 16
            && let Some(m) = self.winning_variant(name, ov)
        {
            value = self.get_with(&m, ov, depth + 1);
        }
        if value.is_none() {
            value = self
                .content
                .get(name)
                .or_else(|| self.weak.get(name))
                .cloned();
        }
        if let Some(ops) = self.pending.get(name) {
            for (kw, r, cond) in ops {
                if *kw == Keyword::Append && cond_ok(cond.as_deref(), ov) {
                    value = Some(value.unwrap_or_default() + r);
                }
            }
            for (kw, r, cond) in ops {
                if *kw == Keyword::Prepend && cond_ok(cond.as_deref(), ov) {
                    value = Some(format!("{r}{}", value.unwrap_or_default()));
                }
            }
        }
        value
    }

    /// The `overridedata` selection loop of `getVarFlag`.
    fn winning_variant(&self, name: &str, ov: &[String]) -> Option<String> {
        let list = self.variants.get(name)?;
        // An insertion-ordered map, as the Python dict is.
        let mut active: Vec<(String, String)> = Vec::new();
        let put = |active: &mut Vec<(String, String)>, k: String, v: String| match active
            .iter_mut()
            .find(|(a, _)| *a == k)
        {
            Some(slot) => slot.1 = v,
            None => active.push((k, v)),
        };
        for (r, o) in list {
            if cond_active(o, ov) {
                put(&mut active, o.clone(), r.clone());
            }
        }
        let mut matched = None;
        let mut modified = true;
        while modified {
            modified = false;
            for o in ov {
                let suffix = format!(":{o}");
                let keys: Vec<String> = active.iter().map(|(a, _)| a.clone()).collect();
                for a in keys {
                    let Some(pos) = active.iter().position(|(k, _)| *k == a) else {
                        continue;
                    };
                    if a.ends_with(&suffix) {
                        let (_, t) = active.remove(pos);
                        put(&mut active, a.replace(&suffix, ""), t);
                        modified = true;
                    } else if a == *o {
                        matched = Some(active.remove(pos).1);
                    }
                }
            }
        }
        matched
    }

    /// `getVar(name)`: expanded, with active `:remove`s applied.
    fn value(&self, name: &str) -> Option<String> {
        let ov = self.active_overrides();
        let raw = self.get_with(name, &ov, 0)?;
        let v = self.expand_with(&raw, &ov, 0);
        let removes: HashSet<String> = self
            .pending
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(kw, _, cond)| *kw == Keyword::Remove && cond_ok(cond.as_deref(), &ov))
            .flat_map(|(_, r, _)| {
                self.expand_with(r, &ov, 0)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        if removes.is_empty() || v.is_empty() {
            return Some(v);
        }
        // `__whitespace_split__` keeps the whitespace; only the removed words go.
        let mut out = String::with_capacity(v.len());
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            if !removes.contains(word.as_str()) {
                out.push_str(word);
            }
            word.clear();
        };
        for c in v.chars() {
            if c.is_whitespace() {
                flush(&mut word, &mut out);
                out.push(c);
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut out);
        Some(out)
    }

    /// `expandVarref(name)`: every variable whose unexpanded value refers to `${name}` is
    /// set (outside parsing) to that value with the reference replaced.
    fn replace_ref(&mut self, name: &str, value: &str) {
        let pat = format!("${{{name}}}");
        let mut keys: Vec<String> = self
            .content
            .keys()
            .chain(self.weak.keys())
            .chain(self.pending.keys())
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            if let Some(v) = self.get(&key)
                && v.contains(&pat)
            {
                self.set_final(&key, &v.replace(&pat, value));
            }
        }
    }

    /// Expands `${VAR}` references recursively. Python (`${@...}`) and references to unset
    /// variables are left as written, as BitBake leaves unset references.
    fn expand(&self, s: &str) -> String {
        self.expand_with(s, &self.active_overrides(), 0)
    }

    fn expand_with(&self, s: &str, ov: &[String], depth: usize) -> String {
        if depth > 32 || !s.contains("${") {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("${") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let Some(end) = matching_brace(after) else {
                out.push_str(&rest[i..]);
                return out;
            };
            let inner = &after[..end];
            let whole = &rest[i..i + 2 + end + 1];
            let is_name = !inner.is_empty()
                && inner
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_+./~:".contains(c));
            let found = if is_name {
                self.get_with(inner, ov, 0)
            } else {
                None
            };
            match found {
                Some(v) => out.push_str(&self.expand_with(&v, ov, depth + 1)),
                None => out.push_str(whole),
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// A variant's override text is active when it is in `OVERRIDES`, or when it is a chain
/// (`a:b`) whose every element is.
fn cond_active(o: &str, ov: &[String]) -> bool {
    ov.iter().any(|x| x == o)
        || (o.contains(':') && o.split(':').all(|p| ov.iter().any(|x| x == p)))
}

/// An `:append:<cond>` condition: every element of `cond` must be in `OVERRIDES`.
fn cond_ok(cond: Option<&str>, ov: &[String]) -> bool {
    cond.is_none_or(|c| c.split(':').all(|p| ov.iter().any(|x| x == p)))
}

/// Byte index of the `}` closing a `${`, given the text after `${`.
fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(src: &str) -> Vars {
        let mut v = Vars::default();
        for st in crate::parse::parse(src).statements {
            if let StatementKind::Assign { var, op, value, .. } = st.kind {
                v.assign(&full_name(&var), op, &value);
            }
        }
        v
    }

    #[test]
    fn operator_semantics_follow_ast_py() {
        let mut v = Vars::default();
        v.assign("A", AssignOp::WeakDefault, "weak");
        // ?= ignores the weak default (noweakdefault=True), so it sets.
        v.assign("A", AssignOp::Default, "strong");
        assert_eq!(v.get("A").as_deref(), Some("strong"));
        v.assign("A", AssignOp::Default, "ignored");
        assert_eq!(v.get("A").as_deref(), Some("strong"));
        v.assign("B", AssignOp::Append, "x");
        assert_eq!(v.get("B").as_deref(), Some(" x"));
        v.assign("B", AssignOp::PrependDot, "p");
        assert_eq!(v.get("B").as_deref(), Some("p x"));
        v.set("X", "1");
        v.assign("C", AssignOp::Immediate, "${X}");
        v.set("X", "2");
        assert_eq!(v.get("C").as_deref(), Some("1"));
        v.assign("W", AssignOp::WeakDefault, "a");
        v.assign("W", AssignOp::WeakDefault, "b");
        assert_eq!(v.get("W").as_deref(), Some("b"));
    }

    #[test]
    fn append_prepend_remove_apply_on_read() {
        let v = conf(
            "L = \"a b\"\nL:append = \" c\"\nL += \"d\"\nL:prepend = \"z \"\n\
             L:remove = \"b\"\nL:append:nope = \" never\"\nL = \"${L} e\"\n",
        );
        // `+=` and `=` during parsing see only the plain value; appends apply on read.
        assert_eq!(v.get("L").as_deref(), Some("z ${L} e c"));
        let v = conf(
            "L = \"a b\"\nL:append = \" c\"\nL += \"d\"\nL:prepend = \"z \"\nL:remove = \"b\"\n",
        );
        assert_eq!(v.value("L").as_deref(), Some("z a  d c"));
        assert_eq!(
            v.value("L").unwrap().split_whitespace().collect::<Vec<_>>(),
            ["z", "a", "d", "c"]
        );
    }

    #[test]
    fn override_variants_need_overrides() {
        let v = conf("A = \"plain\"\nA:foo = \"foo\"\nA:bar = \"bar\"\n");
        assert_eq!(v.value("A").as_deref(), Some("plain"));
        let v = conf("OVERRIDES = \"foo:bar\"\nA = \"plain\"\nA:foo = \"foo\"\nA:bar = \"bar\"\n");
        // Entries to the right have higher priority.
        assert_eq!(v.value("A").as_deref(), Some("bar"));
        let v =
            conf("OVERRIDES = \"foo\"\nA = \"x\"\nA:append:foo = \" y\"\nA:append:no = \" n\"\n");
        assert_eq!(v.value("A").as_deref(), Some("x y"));
    }

    #[test]
    fn keyword_split_matches_setvar_regexp() {
        assert_eq!(
            split_keyword("A:append"),
            Some(("A", Keyword::Append, None))
        );
        assert_eq!(
            split_keyword("FILES:${PN}:append:foo"),
            Some(("FILES:${PN}", Keyword::Append, Some("foo")))
        );
        assert_eq!(
            split_keyword("A:remove:Upper"),
            None,
            "an upper-case condition does not match"
        );
        assert_eq!(split_keyword("A:appendix"), None);
        assert_eq!(split_keyword("A:foo"), None);
    }

    #[test]
    fn replace_ref_folds_appends() {
        let mut v = conf("B ?= \"\"\nB:append = \" ${LAYERDIR}/x\"\n");
        v.set("LAYERDIR", "/l1");
        v.replace_ref("LAYERDIR", "/l1");
        assert_eq!(v.value("B").as_deref(), Some(" /l1/x"));
        // The append was folded into the value, so it is not applied twice.
        v.assign("B", AssignOp::Append, "y");
        assert_eq!(v.value("B").as_deref(), Some(" /l1/x y"));
    }

    #[test]
    fn unset_deletes() {
        let mut v = conf("A = \"1\"\nA:append = \"2\"\n");
        v.del("A");
        assert_eq!(v.get("A"), None);
    }

    #[test]
    fn expansion_leaves_python_and_unset() {
        let mut v = Vars::default();
        v.set("T", "/b");
        v.set("R", "${T}/..");
        assert_eq!(v.expand("${R}/x ${NOPE}/y"), "/b/../x ${NOPE}/y");
        let py = "${@os.path.abspath(os.path.dirname(d.getVar('FILE')) + '/../..')}";
        assert_eq!(v.expand(py), py);
        assert_eq!(
            split_words(&format!(" {py}/meta  a ")),
            vec![format!("{py}/meta"), "a".into()]
        );
    }

    #[test]
    fn re_escape_like_python() {
        assert_eq!(python_re_escape("/a/b-c.d"), "/a/b\\-c\\.d");
    }

    #[test]
    fn dep_names_drop_versions() {
        assert_eq!(
            dep_names("core (>= 12) openembedded-layer"),
            ["core", "openembedded-layer"]
        );
        assert_eq!(dep_names(""), Vec::<String>::new());
    }

    #[test]
    fn unset_priority_is_one_above_dependencies() {
        let explicit: HashMap<String, Option<i32>> = [
            ("core".to_string(), Some(5)),
            ("a".to_string(), None),
            ("b".to_string(), Some(0)),
        ]
        .into();
        let deps: HashMap<String, Vec<String>> = [
            ("core".to_string(), vec![]),
            ("a".to_string(), vec!["core".to_string()]),
            ("b".to_string(), vec!["a".to_string()]),
        ]
        .into();
        let mut memo = HashMap::new();
        assert_eq!(
            collection_priority("a", &explicit, &deps, 0, &mut memo, 0),
            6
        );
        assert_eq!(
            collection_priority("b", &explicit, &deps, 0, &mut memo, 0),
            7
        );
    }
}
