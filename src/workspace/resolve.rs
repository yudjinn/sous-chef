//! `BBPATH` lookup, class lookup, bbappend matching and `BBFILES` expansion.
//!
//! BitBake behaviour reproduced here (checked against BitBake `master`, 2026-11):
//!
//! - **`include` / `require`** (`parse/parse_py/ConfHandler.py` `include_single_file`, used
//!   for every file kind): an absolute path is used as is. A relative path is searched with
//!   `bb.utils.which` along `"<dirname of the including file>:" + BBPATH`, so the including
//!   file's directory first, then each `BBPATH` entry left to right; the first path that
//!   exists wins. A file including itself (same path text) is skipped.
//! - **`include_all`** (`parse/ast.py` `IncludeAllNode`): `<entry>/<path>` for every distinct
//!   `BBPATH` entry, left to right; the including file's directory is not searched.
//! - **`inherit`** (`parse/parse_py/BBHandler.py` `inherit`): for a name without `.bbclass`,
//!   `classes-<type>/<name>.bbclass` is searched along the whole `BBPATH` first, and only
//!   then `classes/<name>.bbclass` along the whole `BBPATH`. `<type>` is `recipe` for
//!   `inherit` in recipes and `global` for `INHERIT` (and `base`) in the configuration
//!   (`__bbclasstype`, `cookerdata.py`). A name ending in `.bbclass` is searched on `BBPATH`
//!   as given; an absolute one is used as is. BitBake 2.0 (Kirkstone) has no
//!   `classes-<type>/` step: only `classes/<name>.bbclass` along `BBPATH` (branch `2.0`).
//!   The step is taken when `ws.release` has [`Feature::SplitClassDirs`].
//! - **bbappends** (`cooker.py` `get_file_appends`): with `b` the append's file name with
//!   `.bbappend` replaced by `.bb` and `f` the recipe's file name, the append applies when
//!   `b == f`, or when `b` contains `%` and `b` starts with the first `i` characters of `f`,
//!   `i` being the position of the first `%` in `b`.
//! - **`BBFILES`** (`cooker.py` `collect_bbfiles`): the globs are taken in order of their
//!   layer's priority (lowest first, stable); each is used as is when the path exists,
//!   a directory is walked for `.bb` / `.bbappend` files; anything else is expanded with
//!   Python's `glob.glob` and the matches sorted with `sorted()`, a string sort (see
//!   [`py_str_cmp`]: `foo-bar/` before `foo/`), and a pattern that matches nothing but exists
//!   is used as is; duplicates are dropped, first occurrence kept. `BBMASK` is not applied
//!   (it is evaluated configuration).

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{Workspace, layers};
use crate::parse::IncludeMode;
use crate::release::Feature;

/// `BBPATH` of the workspace: the value the layer configuration leaves
/// ([`layers::read_bbpath`]), entries made canonical when they exist, relative entries and
/// duplicates dropped. If the configuration cannot be read or leaves `BBPATH` empty, the
/// conventional value is used: the build directory, then every layer root in `BBLAYERS`
/// order.
pub fn bbpath(ws: &Workspace) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = ws.layers.iter().map(|l| l.root.clone()).collect();
    let configured = layers::read_bbpath(ws.build_dir.as_deref(), &roots).unwrap_or_default();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_absolute() && !out.contains(&p) {
            out.push(p);
        }
    };
    if configured.is_empty() {
        for p in ws.build_dir.iter().chain(roots.iter()) {
            push(p.clone());
        }
    } else {
        for e in configured {
            let p = normalize(Path::new(&e));
            push(fs::canonicalize(&p).unwrap_or(p));
        }
    }
    out
}

/// What a reference statement is: an include of some mode, or an inherit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Include(IncludeMode),
    /// `global`: `INHERIT` / configuration context (`classes-global/`), else recipe
    /// context (`classes-recipe/`).
    Inherit {
        global: bool,
    },
}

/// One target of a reference statement after expansion: the expanded word and the file it
/// resolves to (`None`: not found).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub text: String,
    pub file: Option<PathBuf>,
}

/// Resolves paths along a fixed `BBPATH`. Build one per batch of lookups: [`Resolver::new`]
/// reads the layer configuration.
///
/// By default candidates are checked on disk (through [`Workspace::confine`], so nothing
/// outside the workspace is ever found). [`Resolver::with_known_files`] checks them against
/// a set of files instead (the scanner uses the indexed files), which needs no file system
/// access; directories are then never candidates.
#[derive(Debug, Clone)]
pub struct Resolver {
    bbpath: Vec<PathBuf>,
    known: Option<HashSet<PathBuf>>,
}

impl Resolver {
    /// A resolver over the workspace's [`bbpath`].
    pub fn new(ws: &Workspace) -> Resolver {
        Resolver::with_bbpath(bbpath(ws))
    }

    /// A resolver over an explicit `BBPATH`.
    pub fn with_bbpath(bbpath: Vec<PathBuf>) -> Resolver {
        Resolver {
            bbpath,
            known: None,
        }
    }

    /// Checks candidates against `files` (absolute, lexically normalised paths) instead of
    /// the disk.
    pub fn with_known_files(mut self, files: HashSet<PathBuf>) -> Resolver {
        self.known = Some(files);
        self
    }

    pub fn bbpath(&self) -> &[PathBuf] {
        &self.bbpath
    }

    /// `os.path.exists`, confined. `Some(true)` for a file, `Some(false)` for something that
    /// exists but is not a regular file, `None` if nothing is there.
    fn probe(&self, ws: &Workspace, p: &Path) -> Option<bool> {
        match &self.known {
            Some(set) => set.contains(p).then_some(true),
            None => {
                ws.confine(p).ok()?;
                let meta = fs::metadata(p).ok()?;
                Some(meta.is_file())
            }
        }
    }

    /// `bb.utils.which(path, item)` followed by the open: the first existing candidate wins,
    /// and it must be a file.
    fn first<I: IntoIterator<Item = PathBuf>>(&self, ws: &Workspace, cands: I) -> Option<PathBuf> {
        for c in cands {
            let c = normalize(&c);
            match self.probe(ws, &c) {
                Some(true) => return Some(c),
                Some(false) => return None,
                None => {}
            }
        }
        None
    }

    /// `include` / `require` target: absolute, else `from`'s directory, then each `BBPATH`
    /// entry.
    pub fn find_in_bbpath(&self, ws: &Workspace, rel: &str, from: &Path) -> Option<PathBuf> {
        if rel.is_empty() || from.as_os_str() == rel {
            return None;
        }
        let p = Path::new(rel);
        if p.is_absolute() {
            return self.first(ws, [p.to_path_buf()]);
        }
        let dir = from.parent().map(Path::to_path_buf).unwrap_or_default();
        let cands = std::iter::once(dir)
            .chain(self.bbpath.iter().cloned())
            .filter(|d| d.is_absolute())
            .map(|d| d.join(p));
        self.first(ws, cands)
    }

    /// `include_all` targets: `<entry>/<rel>` for every `BBPATH` entry where it is a file.
    pub fn include_all(&self, ws: &Workspace, rel: &str) -> Vec<PathBuf> {
        if rel.is_empty() {
            return Vec::new();
        }
        self.bbpath
            .iter()
            .map(|e| normalize(&e.join(rel)))
            .filter(|c| self.probe(ws, c) == Some(true))
            .collect()
    }

    /// The class file `inherit name` (or `INHERIT`, when `global`) loads.
    pub fn find_class(&self, ws: &Workspace, name: &str, global: bool) -> Option<PathBuf> {
        for group in class_paths(name, global, split_class_dirs(ws)) {
            let found = match group {
                ClassPath::Abs(p) => self.first(ws, [p]),
                ClassPath::Rel(rel) => self.first(ws, self.bbpath.iter().map(|e| e.join(&rel))),
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Every class file `name` could load, in BitBake's search order: the first one is the
    /// one [`Resolver::find_class`] returns, the rest are shadowed.
    pub fn class_candidates(&self, ws: &Workspace, name: &str, global: bool) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for group in class_paths(name, global, split_class_dirs(ws)) {
            let cands: Vec<PathBuf> = match group {
                ClassPath::Abs(p) => vec![p],
                ClassPath::Rel(rel) => self.bbpath.iter().map(|e| e.join(&rel)).collect(),
            };
            for c in cands {
                let c = normalize(&c);
                if self.probe(ws, &c) == Some(true) && !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// Resolves a reference statement whose argument may contain `${VAR}`. `vars` supplies
    /// the values the caller knows (it returns `None` for the others). Returns `None` when
    /// the text cannot be expanded with them (an unknown variable or python), otherwise one
    /// [`Target`] per word (BitBake splits the expanded argument on whitespace). An
    /// `include_all` gives one target per file found, or a single unresolved target.
    pub fn resolve_with(
        &self,
        ws: &Workspace,
        kind: RefKind,
        text: &str,
        from: &Path,
        vars: &dyn Fn(&str) -> Option<String>,
    ) -> Option<Vec<Target>> {
        let expanded = expand_with(text, vars)?;
        let out = match kind {
            RefKind::Include(IncludeMode::IncludeAll) => {
                let word = expanded.trim().to_string();
                let found = self.include_all(ws, &word);
                if found.is_empty() {
                    vec![Target {
                        text: word,
                        file: None,
                    }]
                } else {
                    found
                        .into_iter()
                        .map(|f| Target {
                            text: word.clone(),
                            file: Some(f),
                        })
                        .collect()
                }
            }
            RefKind::Include(_) => expanded
                .split_whitespace()
                .map(|w| Target {
                    text: w.to_string(),
                    file: self.find_in_bbpath(ws, w, from),
                })
                .collect(),
            RefKind::Inherit { global } => expanded
                .split_whitespace()
                .map(|w| Target {
                    text: w.to_string(),
                    file: self.find_class(ws, w, global),
                })
                .collect(),
        };
        Some(out)
    }
}

enum ClassPath {
    Abs(PathBuf),
    Rel(PathBuf),
}

/// Whether the workspace release searches `classes-recipe/` / `classes-global/` before
/// `classes/` ([`Feature::SplitClassDirs`]).
fn split_class_dirs(ws: &Workspace) -> bool {
    ws.release.release.has(Feature::SplitClassDirs)
}

/// The class directory a `.bbclass` file is in, as the class search of `ws.release` sees
/// it. `rel` is the path relative to the layer root (a `BBPATH` entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassDir {
    /// `classes/`: searched for `inherit` and `INHERIT` on every release.
    Shared,
    /// `classes-recipe/`, on a release with [`Feature::SplitClassDirs`]: searched for
    /// `inherit` in recipe context.
    Recipe,
    /// `classes-global/`, on a release with [`Feature::SplitClassDirs`]: searched for
    /// `INHERIT` (global context).
    Global,
    /// `classes-recipe/` or `classes-global/` on a release without
    /// [`Feature::SplitClassDirs`] (Kirkstone): BitBake never finds a class there by name.
    Unsearched,
    /// Any other directory: found only by an `inherit` that names the path
    /// (`inherit dir/x.bbclass`).
    Other,
}

/// Classifies a class file by its directory for the workspace release (see [`ClassDir`]).
/// Only the first component of `rel` counts, as the scanner and query layer have always
/// read it.
pub fn class_dir(ws: &Workspace, rel: &Path) -> ClassDir {
    let split = split_class_dirs(ws);
    let mut parts = rel.components();
    let first = match (parts.next(), parts.next()) {
        (Some(Component::Normal(d)), Some(_)) => d.to_string_lossy(),
        _ => return ClassDir::Other,
    };
    match first.as_ref() {
        "classes" => ClassDir::Shared,
        "classes-recipe" if split => ClassDir::Recipe,
        "classes-global" if split => ClassDir::Global,
        "classes-recipe" | "classes-global" => ClassDir::Unsearched,
        _ => ClassDir::Other,
    }
}

/// The search groups of `BBHandler.inherit`, in order. With `split` (BitBake 2.2 / Langdale
/// and later) `classes-<type>/` is searched through `BBPATH`, then `classes/`; without it
/// (BitBake 2.0 / Kirkstone) only `classes/`. See "Releases" in bitbake-semantics.md.
fn class_paths(name: &str, global: bool, split: bool) -> Vec<ClassPath> {
    let ty = if global {
        "classes-global"
    } else {
        "classes-recipe"
    };
    let p = Path::new(name);
    if p.is_absolute() {
        return vec![ClassPath::Abs(p.to_path_buf())];
    }
    if name.ends_with(".bbclass") {
        return vec![ClassPath::Rel(p.to_path_buf())];
    }
    let file = format!("{name}.bbclass");
    let classes = ClassPath::Rel(Path::new("classes").join(&file));
    if split {
        vec![ClassPath::Rel(Path::new(ty).join(&file)), classes]
    } else {
        vec![classes]
    }
}

/// Resolves an `include` / `require` target with the workspace's `BBPATH`. See
/// [`Resolver::find_in_bbpath`]; build a [`Resolver`] for many lookups.
pub fn find_in_bbpath(ws: &Workspace, rel: &str, from: &Path) -> Option<PathBuf> {
    Resolver::new(ws).find_in_bbpath(ws, rel, from)
}

/// Resolves `inherit name` (`global == false`: `classes-recipe/` then `classes/`) or an
/// `INHERIT` class (`global == true`: `classes-global/` then `classes/`). On a release
/// without [`Feature::SplitClassDirs`] (Kirkstone) only `classes/` is searched.
pub fn find_class(ws: &Workspace, name: &str, global: bool) -> Option<PathBuf> {
    Resolver::new(ws).find_class(ws, name, global)
}

/// Every candidate class file for `name`, winner first (see
/// [`Resolver::class_candidates`]).
pub fn class_candidates(ws: &Workspace, name: &str, global: bool) -> Vec<PathBuf> {
    Resolver::new(ws).class_candidates(ws, name, global)
}

/// Whether a bbappend applies to a recipe file (`cooker.py` `get_file_appends`, including
/// the `%` wildcard). Only the file names are compared.
pub fn append_matches(append_file: &Path, recipe_file: &Path) -> bool {
    let (Some(a), Some(f)) = (append_file.file_name(), recipe_file.file_name()) else {
        return false;
    };
    let (a, f) = (a.to_string_lossy(), f.to_string_lossy());
    if !a.ends_with(".bbappend") || !f.ends_with(".bb") {
        return false;
    }
    let b = a.replace(".bbappend", ".bb");
    if b == f {
        return true;
    }
    match b.chars().position(|c| c == '%') {
        Some(i) => {
            let prefix: String = f.chars().take(i).collect();
            b.starts_with(&prefix)
        }
        None => false,
    }
}

/// Expands `${VAR}` in `text` with `vars`, recursively (values may hold references).
/// `None` when any reference cannot be expanded: python (`${@...}`), a variable `vars`
/// does not know, or an unterminated `${`.
pub fn expand_with(text: &str, vars: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    expand_depth(text, vars, 0)
}

fn expand_depth(text: &str, vars: &dyn Fn(&str) -> Option<String>, depth: usize) -> Option<String> {
    if !text.contains("${") {
        return Some(text.to_string());
    }
    if depth > 16 {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let end = after.find('}')?;
        let name = &after[..end];
        let ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_+./~:".contains(c));
        if !ok {
            return None;
        }
        let v = vars(name)?;
        out.push_str(&expand_depth(&v, vars, depth + 1)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Expands every layer's `BBFILES` globs, and the `BBFILES_DYNAMIC` globs whose condition
/// holds, into recipe and append files, in BitBake's collection order (see the module
/// documentation). Only `.bb` and `.bbappend` files inside the workspace are returned.
pub fn recipe_files(ws: &Workspace) -> Vec<PathBuf> {
    collect(ws).files
}

/// One `BBFILES_DYNAMIC` entry `{!}<collection>:<glob>` (`cookerdata.py`
/// `parseConfigurationFiles`), with `${LAYERDIR}` replaced by the root of the layer whose
/// `conf/layer.conf` wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicEntry {
    /// The layer whose `layer.conf` sets it.
    pub layer: String,
    pub collection: String,
    /// `!<collection>`: the glob is added when the collection is absent.
    pub invert: bool,
    /// Absolute glob.
    pub pattern: String,
    /// Whether BitBake adds the glob to `BBFILES`: the collection is in some layer's
    /// `BBFILE_COLLECTIONS` (absent, for `invert`).
    pub active: bool,
}

impl DynamicEntry {
    /// Whether the glob matches `path` (component by component, as `glob.glob` finds it).
    pub fn matches(&self, path: &Path) -> bool {
        let p: Vec<&str> = self.pattern.split('/').filter(|c| !c.is_empty()).collect();
        let s = path.to_string_lossy();
        let f: Vec<&str> = s.split('/').filter(|c| !c.is_empty()).collect();
        p.len() == f.len()
            && p.iter().zip(&f).all(|(p, f)| {
                (!f.starts_with('.') || p.starts_with('.') || !has_magic(p)) && fnmatch(f, p)
            })
    }
}

/// What [`collect`] returns.
#[derive(Debug, Clone, Default)]
pub struct Collection {
    /// The collected `.bb` / `.bbappend` files, in collection order ([`recipe_files`]).
    pub files: Vec<PathBuf>,
    /// The `BBFILES_DYNAMIC` entries of the layers, in order.
    pub dynamic: Vec<DynamicEntry>,
    /// Files that only an inactive `BBFILES_DYNAMIC` glob matches (BitBake does not collect
    /// them), with the index of that entry in `dynamic`.
    pub inactive_dynamic: Vec<(PathBuf, usize)>,
    /// Collected files that no layer's `BBFILES` glob matches, only an active
    /// `BBFILES_DYNAMIC` one.
    pub via_dynamic: HashSet<PathBuf>,
    /// `BBFILES_DYNAMIC` words BitBake stops on ("BBFILES_DYNAMIC entries must be of the form
    /// {!}<collection name>:<filename pattern>") and words that cannot be expanded
    /// statically; both are left out.
    pub unusable: Vec<String>,
}

/// [`recipe_files`] with what the `BBFILES_DYNAMIC` entries did.
pub fn collect(ws: &Workspace) -> Collection {
    let (dynamic, unusable) = bbfiles_dynamic(ws);
    let mut globs: Vec<(i32, &str, bool)> = Vec::new();
    for l in &ws.layers {
        for g in &l.bbfiles {
            globs.push((l.priority, g.as_str(), false));
        }
    }
    // `data.appendVar("BBFILES", " " + f)` after every layer.conf. `collect_bbfiles` sorts
    // by the priority of the collection whose BBFILE_PATTERN matches the glob: here, as for
    // the static globs, the layer that wrote it.
    for d in dynamic.iter().filter(|d| d.active) {
        let prio = ws.layer(&d.layer).map(|l| l.priority).unwrap_or(0);
        globs.push((prio, d.pattern.as_str(), true));
    }
    globs.sort_by_key(|(p, _, _)| *p);
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut via_dynamic: HashSet<PathBuf> = HashSet::new();
    for (_, g, dynamic_glob) in globs {
        for p in expand_bbfiles_word(ws, g) {
            if seen.insert(p.clone()) {
                if dynamic_glob {
                    via_dynamic.insert(p.clone());
                }
                out.push(p);
            } else if !dynamic_glob {
                via_dynamic.remove(&p);
            }
        }
    }
    let mut inactive_dynamic: Vec<(PathBuf, usize)> = Vec::new();
    for (i, d) in dynamic.iter().enumerate().filter(|(_, d)| !d.active) {
        for p in expand_bbfiles_word(ws, &d.pattern) {
            if !seen.contains(&p) && !inactive_dynamic.iter().any(|(q, _)| *q == p) {
                inactive_dynamic.push((p, i));
            }
        }
    }
    Collection {
        files: out,
        dynamic,
        inactive_dynamic,
        via_dynamic,
        unusable,
    }
}

/// One `BBFILES` word as `collect_bbfiles` takes it: a directory is walked; anything else is
/// globbed, the matches sorted as Python sorts strings, and a pattern that matches nothing
/// but exists as a path is used as is. Only `.bb` / `.bbappend` files inside the workspace
/// are kept; a relative word gives nothing.
fn expand_bbfiles_word(ws: &Workspace, g: &str) -> Vec<PathBuf> {
    let gp = Path::new(g);
    if !gp.is_absolute() {
        return Vec::new();
    }
    let mut matched = if gp.is_dir() {
        walk_recipes(ws, gp)
    } else {
        let mut m = glob(ws, g);
        if m.is_empty() && fs::symlink_metadata(gp).is_ok() {
            m.push(gp.to_path_buf());
        }
        m.sort_by(|a, b| py_str_cmp(a, b));
        m
    };
    matched.retain(|p| {
        p.extension().is_some_and(|e| e == "bb" || e == "bbappend") && ws.confine(p).is_ok()
    });
    matched
}

/// The `BBFILES_DYNAMIC` entries of the layers' `conf/layer.conf` files, in `BBLAYERS`
/// order, and the words that cannot be used.
///
/// BitBake (2.18.0 `lib/bb/cookerdata.py` `CookerDataBuilder.parseConfigurationFiles`; the
/// same code on branch `2.0`) reads `BBFILES_DYNAMIC` once every layer.conf has been parsed
/// (each layer's `${LAYERDIR}` was replaced in every variable right after its file,
/// `expandVarref('LAYERDIR')`), splits it on whitespace, splits each word on its first `:`
/// (no `:` is fatal), and a leading `!` inverts the test; the glob is appended to `BBFILES`
/// when the collection is in `BBFILE_COLLECTIONS` (absent, inverted). Here each layer.conf
/// is parsed and its assignments to `BBFILES_DYNAMIC` applied in order (`=` / `:=` replace,
/// `?=` / `??=` only while unset, `+=` / `.=` add at the end, `=+` / `=.` at the start;
/// `:append` / `:prepend` after all of them, `:remove` drops words); `include` /
/// `require` in a layer.conf and conditional (override) assignments are not followed. A
/// word with `${...}` other than `${LAYERDIR}` cannot be expanded and is left out (listed
/// as unusable).
pub fn bbfiles_dynamic(ws: &Workspace) -> (Vec<DynamicEntry>, Vec<String>) {
    use crate::parse::StatementKind;
    use crate::types::AssignOp;
    const LAYERDIR_REF: &str = "${LAYERDIR}";
    // (word, layer that wrote it)
    let mut words: Option<Vec<(String, String)>> = None;
    let mut removes: Vec<String> = Vec::new();
    let mut appends: Vec<(String, String)> = Vec::new();
    let mut prepends: Vec<(String, String)> = Vec::new();
    for l in &ws.layers {
        let Ok(text) = fs::read(l.root.join("conf/layer.conf")) else {
            continue;
        };
        let root = l.root.to_string_lossy();
        let parsed = crate::parse::parse_bytes(&text);
        for st in &parsed.statements {
            let StatementKind::Assign { var, op, value, .. } = &st.kind else {
                continue;
            };
            if var.base != "BBFILES_DYNAMIC" || var.flag.is_some() || var.legacy_syntax {
                continue;
            }
            let new: Vec<(String, String)> = crate::parse::words::words(value)
                .into_iter()
                .map(|w| (w.replace(LAYERDIR_REF, &root), l.name.clone()))
                .collect();
            match var.overrides.as_slice() {
                [] => {}
                [k] if k == "append" => {
                    appends.extend(new);
                    continue;
                }
                [k] if k == "prepend" => {
                    let mut n = new;
                    n.append(&mut prepends);
                    prepends = n;
                    continue;
                }
                [k] if k == "remove" => {
                    removes.extend(new.into_iter().map(|(w, _)| w));
                    continue;
                }
                _ => continue,
            }
            let cur = words.get_or_insert_with(Vec::new);
            match op {
                AssignOp::Set | AssignOp::Immediate => *cur = new,
                AssignOp::Default | AssignOp::WeakDefault => {
                    if cur.is_empty() {
                        *cur = new;
                    }
                }
                AssignOp::Append | AssignOp::AppendDot => cur.extend(new),
                AssignOp::Prepend | AssignOp::PrependDot => {
                    let mut n = new;
                    n.append(cur);
                    *cur = n;
                }
            }
        }
    }
    let mut all = prepends;
    all.extend(words.unwrap_or_default());
    all.extend(appends);
    let collections: HashSet<&str> = ws
        .layers
        .iter()
        .flat_map(|l| l.collections.iter().map(String::as_str))
        .collect();
    let mut out = Vec::new();
    let mut unusable = Vec::new();
    for (word, layer) in all {
        if removes.contains(&word) {
            continue;
        }
        let Some((coll, pattern)) = word.split_once(':') else {
            unusable.push(word);
            continue;
        };
        let (collection, invert) = match coll.strip_prefix('!') {
            Some(c) => (c, true),
            None => (coll, false),
        };
        if collection.is_empty() || pattern.contains("${") || collection.contains("${") {
            unusable.push(word);
            continue;
        }
        out.push(DynamicEntry {
            layer,
            collection: collection.to_string(),
            invert,
            pattern: pattern.to_string(),
            active: collections.contains(collection) != invert,
        });
    }
    (out, unusable)
}

/// `cooker.find_bbfiles`: every `.bb` / `.bbappend` under `dir` (sorted as strings for
/// determinism; BitBake uses `os.walk` order, which depends on the file system).
fn walk_recipes(ws: &Workspace, dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if ws.confine(&d).is_err() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let n = e.file_name();
                if !matches!(n.to_str(), Some("SCCS" | "CVS" | ".svn")) {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "bb" || x == "bbappend") {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| py_str_cmp(a, b));
    out
}

/// The order of Python's `sorted()` on the path strings BitBake works with: code point by
/// code point. On Unix Python decodes a file name as UTF-8 with `surrogateescape`, so a byte
/// that is not valid UTF-8 becomes the code point `U+DC00 + byte`; that is reproduced here.
/// For valid UTF-8 this is byte order. It differs from `Path` order, which compares
/// components: `foo-bar/x` sorts before `foo/x` as a string (`-` is 0x2D, `/` 0x2F) but
/// after it as a path.
pub fn py_str_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    py_code_points(a.as_os_str().as_encoded_bytes())
        .cmp(py_code_points(b.as_os_str().as_encoded_bytes()))
}

fn py_code_points(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes.utf8_chunks().flat_map(|c| {
        c.valid()
            .chars()
            .map(u32::from)
            .chain(c.invalid().iter().map(|b| 0xDC00 + u32::from(*b)))
            .collect::<Vec<u32>>()
    })
}

/// Python `glob.glob(pattern)` (not recursive) for an absolute pattern. Directories outside
/// the workspace are not listed.
fn glob(ws: &Workspace, pattern: &str) -> Vec<PathBuf> {
    let mut cur: Vec<PathBuf> = vec![PathBuf::from("/")];
    for comp in pattern.split('/').filter(|c| !c.is_empty()) {
        let mut next = Vec::new();
        if has_magic(comp) {
            for dir in &cur {
                if ws.confine(dir).is_err() {
                    continue;
                }
                let Ok(entries) = fs::read_dir(dir) else {
                    continue;
                };
                let mut names: Vec<String> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                for n in names {
                    // `glob._ishidden`: a hidden name only matches a pattern starting with '.'.
                    if n.starts_with('.') && !comp.starts_with('.') {
                        continue;
                    }
                    if fnmatch(&n, comp) {
                        next.push(dir.join(n));
                    }
                }
            }
        } else {
            for dir in &cur {
                let p = dir.join(comp);
                if fs::symlink_metadata(&p).is_ok() {
                    next.push(p);
                }
            }
        }
        cur = next;
        if cur.is_empty() {
            break;
        }
    }
    cur
}

fn has_magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Python `fnmatch.fnmatchcase`: `*`, `?`, `[seq]`, `[!seq]`; a `[` without a closing `]`
/// is literal.
fn fnmatch(name: &str, pat: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    fn_match(&n, &p)
}

fn fn_match(n: &[char], p: &[char]) -> bool {
    let Some((&c, rest)) = p.split_first() else {
        return n.is_empty();
    };
    match c {
        '*' => (0..=n.len()).any(|i| fn_match(&n[i..], rest)),
        '?' => !n.is_empty() && fn_match(&n[1..], rest),
        '[' => match class_end(p) {
            Some(end) => {
                let Some((&ch, nrest)) = n.split_first() else {
                    return false;
                };
                class_matches(&p[1..end], ch) && fn_match(nrest, &p[end + 1..])
            }
            None => n.first() == Some(&'[') && fn_match(&n[1..], rest),
        },
        _ => n.first() == Some(&c) && fn_match(&n[1..], rest),
    }
}

/// Index of the `]` closing the class that starts at `p[0] == '['` (`fnmatch.translate`:
/// a `!` and then a `]` right after the `[` are part of the class).
fn class_end(p: &[char]) -> Option<usize> {
    let mut j = 1;
    if p.get(j) == Some(&'!') {
        j += 1;
    }
    if p.get(j) == Some(&']') {
        j += 1;
    }
    while j < p.len() && p[j] != ']' {
        j += 1;
    }
    (j < p.len()).then_some(j)
}

fn class_matches(class: &[char], ch: char) -> bool {
    let (neg, body) = match class.split_first() {
        Some((&'!', rest)) => (true, rest),
        _ => (false, class),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == '-' {
            if body[i] <= ch && ch <= body[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if body[i] == ch {
                hit = true;
            }
            i += 1;
        }
    }
    hit != neg
}

/// Lexical normalisation: drops `.`, applies `..` to the preceding component.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_entry_globs() {
        // Moved from the `layers` tool, whose private matcher this replaces.
        let e = |p: &str| DynamicEntry {
            layer: "l".into(),
            collection: "tpm2".into(),
            invert: false,
            pattern: p.into(),
            active: true,
        };
        let d = e("/l/dynamic-layers/meta-tpm2/*/*/*.bbappend");
        assert!(d.matches(Path::new(
            "/l/dynamic-layers/meta-tpm2/recipes-tpm/t/t_5.7.bbappend"
        )));
        assert!(!d.matches(Path::new(
            "/l/dynamic-layers/meta-tpm2/recipes-tpm/t/u/t.bbappend"
        )));
        assert!(!d.matches(Path::new("/l/dynamic-layers/meta-tpm2/recipes-tpm/t/t.bb")));
        assert!(e("/l/recipes-[a-c]*/x.bb").matches(Path::new("/l/recipes-bsp/x.bb")));
        assert!(!e("/l/recipes-[!a-c]*/x.bb").matches(Path::new("/l/recipes-bsp/x.bb")));
        assert!(e("/l/x?.bb").matches(Path::new("/l/x1.bb")));
    }

    #[test]
    fn fnmatch_like_python() {
        assert!(fnmatch("hello_1.0.bb", "*.bb"));
        assert!(!fnmatch("hello_1.0.bbappend", "*.bb"));
        assert!(fnmatch("recipes-core", "recipes-*"));
        assert!(fnmatch("a", "?"));
        assert!(!fnmatch("ab", "?"));
        assert!(fnmatch("b", "[abc]"));
        assert!(!fnmatch("d", "[abc]"));
        assert!(fnmatch("d", "[!abc]"));
        assert!(fnmatch("m", "[a-z]"));
        assert!(fnmatch("]", "[]]"));
        assert!(fnmatch("[x", "[x"));
    }

    #[test]
    fn expand_with_static_vars() {
        let vars = |n: &str| match n {
            "PN" => Some("hello".to_string()),
            "BP" => Some("${PN}-1.0".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_with("${BP}/x.inc", &vars).as_deref(),
            Some("hello-1.0/x.inc")
        );
        assert_eq!(expand_with("plain", &vars).as_deref(), Some("plain"));
        assert_eq!(expand_with("${NOPE}.inc", &vars), None);
        assert_eq!(expand_with("${@'a'}.inc", &vars), None);
        assert_eq!(expand_with("${PN", &vars), None);
    }

    #[test]
    fn append_name_matching() {
        let m = |a: &str, r: &str| append_matches(Path::new(a), Path::new(r));
        assert!(m("/x/hello_%.bbappend", "/y/hello_1.0.bb"));
        assert!(m("hello_1.1.bbappend", "hello_1.1.bb"));
        assert!(!m("hello_1.1.bbappend", "hello_1.0.bb"));
        assert!(m("busybox_1.%.bbappend", "busybox_1.36.bb"));
        assert!(!m("busybox_1.%.bbappend", "busybox_2.0.bb"));
        assert!(!m("hello_%.bbappend", "hello-extra_1.0.bb"));
        assert!(m("hello.bbappend", "hello.bb"));
        assert!(!m("hello.bbappend", "hello_1.0.bb"));
        assert!(!m("hello_%.bbappend", "hello_1.0.bbappend"));
    }
}
