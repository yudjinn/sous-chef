//! `Index::refresh`: stat walk, hashing, reparse of changed files.
//!
//! 1. The `layers` table is synced with the workspace (plus the pseudo-layer `build` for the
//!    build directory). A layer whose root changed is dropped and re-added.
//! 2. Every layer is walked (hidden directories, nested layers, the build directory, other
//!    build directories — anything containing `conf/bblayers.conf` — and `tmp`,
//!    `sstate-cache`, `downloads` are skipped) for `.bb`, `.bbappend`, `.inc`, `.bbclass` and
//!    `.conf` files; `<build>/conf/*.conf` is indexed under `build`. Every path goes through
//!    [`Workspace::confine`].
//! 3. A file whose mtime (nanoseconds) and size equal its row is not read. Otherwise it is
//!    hashed; if the sha256 equals the row's only the stat columns are updated, else the file
//!    is reparsed (on a thread pool) and its rows replaced.
//! 4. Rows of files that disappeared are deleted (foreign keys cascade).
//! 5. Rows: `statements` (full `value`), `tasks`, `provides`, `depends`, `files.pn/pv`. The FTS
//!    table follows `statements` through triggers.
//! 6. After any change every edge is recomputed: `require` / `include` / `include_all` /
//!    `inherit` from the statements, resolved against the indexed files with
//!    `workspace::resolve` where no evaluation is needed (otherwise `target_file` is NULL),
//!    and `bbappend` edges from the `BBFILES` collection. This is a superset of "re-resolve
//!    edges whose target was unresolved or changed" and keeps edge order meaningful.
//!
//! All database writes run in one `Index::write` transaction.
//!
//! Column conventions (see `query.rs` for the readers):
//!
//! - `statements.ord` is the 0-based position in the file; `start_line` / `end_line` the
//!   statement's span, function bodies included.
//! - `statements.name` is the full name as written for assignments, functions (anonymous
//!   python: `__anonymous`), `def`, `export`, `unset`; the argument text for `inherit` and
//!   `include`; the task names for `addtask` / `deltask`; the handler names for
//!   `addhandler`; the space-joined names for `export_functions`; NULL for `unknown`.
//! - `statements.op` is the operator for assignments; `include` / `require` / `include_all`
//!   for includes; `inherit` / `inherit_defer` for inherits; `shell` / `python`, prefixed
//!   with `fakeroot ` when so declared, for functions; `python` for `def`.
//! - `statements.value` is the raw value for assignments and the text of `unknown` lines.
//! - `edges.target_text` is the argument as written when it resolves to one target, else the
//!   expanded word; for `bbappend` edges it is the recipe file name the append stands for
//!   (`hello_%.bb`), and `line` is NULL.
//! - `depends`: `DEPENDS` and `RDEPENDS:<pkg>` assignments without a condition (only
//!   `:append` / `:prepend` allowed after the package), and the third field (build
//!   dependencies) of `PACKAGECONFIG[<flag>]` with `pkg` = the flag. Words containing `${` are
//!   skipped and version constraints dropped.
//! - `provides`: a recipe's `PN`, plus the words of unconditional `PROVIDES` assignments that
//!   need no expansion, in any file kind except `.conf`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::{Index, RefreshReport};
use crate::error::Result;
use crate::parse::{IncludeMode, Lang, StatementKind, lexer};
use crate::render;
use crate::types::{FileKind, LineSpan, Loc, VarRef};
use crate::workspace::resolve::{self, RefKind, Resolver};
use crate::workspace::{BUILD_LAYER, Workspace};

/// Directory names never walked inside a layer: build output.
const SKIP_DIRS: &[&str] = &["tmp", "sstate-cache", "downloads"];

/// `SPECIAL_PKGSUFFIX` in OE-Core's `bitbake.conf`, pruned from `PN` to get `BPN`.
const SPECIAL_PKGSUFFIX: &[&str] = &[
    "-native",
    "-cross",
    "-initial",
    "-intermediate",
    "-crosssdk",
    "-cross-canadian",
];

/// Below this many files to read, hashing and parsing stay on the calling thread.
const PARALLEL_MIN: usize = 16;

#[derive(Debug, Clone)]
struct LayerRow {
    id: i64,
    name: String,
    root: PathBuf,
}

/// A file found by the walk.
#[derive(Debug, Clone)]
struct Found {
    layer: usize,
    rel: String,
    /// The path the file is read from (confined, symlinks resolved).
    real: PathBuf,
    kind: FileKind,
    mtime: i64,
    size: i64,
}

#[derive(Debug, Clone)]
struct Existing {
    id: i64,
    mtime: i64,
    size: i64,
    sha: String,
}

/// Everything stored for one file, computed off the database thread.
#[derive(Debug, Clone, Default)]
struct FileRows {
    statements: Vec<StmtRow>,
    tasks: Vec<(String, String, String, u32)>,
    provides: Vec<String>,
    depends: Vec<(&'static str, Option<String>, String, u32)>,
    lines: u32,
}

#[derive(Debug, Clone, Default)]
struct StmtRow {
    kind: &'static str,
    name: Option<String>,
    base: Option<String>,
    overrides: Option<String>,
    flag: Option<String>,
    op: Option<String>,
    legacy: bool,
    value: Option<String>,
    start: u32,
    end: u32,
    snippet: String,
}

enum Analysed {
    /// Content unchanged: only the stat columns move.
    Same,
    Changed {
        sha: String,
        rows: FileRows,
    },
    /// Could not be read: dropped from the index.
    Unreadable,
}

/// [`Index::refresh`].
pub fn refresh(index: &mut Index, ws: &Workspace) -> Result<RefreshReport> {
    let start = Instant::now();
    let mut removed: u32 = 0;
    let mut changed_any = false;

    let (layers, layers_changed, layer_removed) = sync_layers(index, ws)?;
    removed += layer_removed;
    changed_any |= layers_changed;

    let found = walk(ws, &layers);
    let existing = load_existing(index.conn())?;

    let mut seen: HashSet<i64> = HashSet::new();
    let mut work: Vec<(Found, Option<Existing>)> = Vec::new();
    for f in &found {
        let key = (layers[f.layer].id, f.rel.clone());
        match existing.get(&key) {
            Some(e) => {
                seen.insert(e.id);
                if e.mtime != f.mtime || e.size != f.size {
                    work.push((f.clone(), Some(e.clone())));
                }
            }
            None => work.push((f.clone(), None)),
        }
    }
    let gone: Vec<i64> = existing
        .values()
        .filter(|e| !seen.contains(&e.id))
        .map(|e| e.id)
        .collect();

    let analysed = analyse_all(&work);
    let mut reindexed: Vec<Loc> = Vec::new();
    for ((f, _), a) in work.iter().zip(&analysed) {
        if let Analysed::Changed { rows, .. } = a {
            reindexed.push(Loc {
                layer: layers[f.layer].name.clone(),
                path: PathBuf::from(&f.rel),
                lines: LineSpan::new(1, rows.lines.max(1)),
            });
        }
    }
    let unreadable = analysed
        .iter()
        .zip(&work)
        .filter(|(a, (_, e))| matches!(a, Analysed::Unreadable) && e.is_some())
        .count() as u32;
    removed += gone.len() as u32 + unreadable;
    changed_any |= !reindexed.is_empty() || removed > 0;

    let needs_write = changed_any || analysed.iter().any(|a| matches!(a, Analysed::Same));
    if needs_write {
        index.write(|tx| {
            for id in &gone {
                tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
            }
            for ((f, e), a) in work.iter().zip(&analysed) {
                let layer_id = layers[f.layer].id;
                match a {
                    Analysed::Same => {
                        if let Some(e) = e {
                            tx.execute(
                                "UPDATE files SET mtime = ?1, size = ?2 WHERE id = ?3",
                                params![f.mtime, f.size, e.id],
                            )?;
                        }
                    }
                    Analysed::Unreadable => {
                        if let Some(e) = e {
                            tx.execute("DELETE FROM files WHERE id = ?1", [e.id])?;
                        }
                    }
                    Analysed::Changed { sha, rows } => {
                        write_file(tx, layer_id, f, sha, rows)?;
                    }
                }
            }
            if changed_any {
                rebuild_edges(tx, ws)?;
            }
            Ok(())
        })?;
    }

    Ok(RefreshReport {
        scanned: found.len() as u32,
        reindexed,
        removed,
        millis: start.elapsed().as_millis() as u64,
    })
}

// ---- 1. layers ----

struct LayerSpec {
    name: String,
    root: PathBuf,
    priority: i32,
    collections: String,
    compat: String,
    depends: String,
    writable: bool,
}

/// Syncs the `layers` table; returns the rows in workspace order (build last), whether
/// anything changed, and how many file rows were removed with dropped layers.
fn sync_layers(index: &mut Index, ws: &Workspace) -> Result<(Vec<LayerRow>, bool, u32)> {
    let mut specs: Vec<LayerSpec> = ws
        .layers
        .iter()
        .map(|l| LayerSpec {
            name: l.name.clone(),
            root: l.root.clone(),
            priority: l.priority,
            collections: l.collections.join(" "),
            compat: l.compat.join(" "),
            depends: l.depends.join(" "),
            writable: l.writable,
        })
        .collect();
    if let Some(b) = &ws.build_dir
        && !specs.iter().any(|s| s.name == BUILD_LAYER)
    {
        specs.push(LayerSpec {
            name: BUILD_LAYER.to_string(),
            root: b.clone(),
            priority: 0,
            collections: String::new(),
            compat: String::new(),
            depends: String::new(),
            writable: false,
        });
    }

    type Row = (i64, String, String, i32, String, String, String, bool);
    let current: Vec<Row> = {
        let mut st = index.conn().prepare(
            "SELECT id, name, root, priority, collections, compat, depends, writable FROM layers",
        )?;
        st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, Option<i32>>(3)?.unwrap_or(0),
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                r.get::<_, Option<bool>>(7)?.unwrap_or(false),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let same = current.len() == specs.len()
        && specs.iter().all(|s| {
            current.iter().any(|c| {
                c.1 == s.name
                    && c.2 == s.root.to_string_lossy()
                    && c.3 == s.priority
                    && c.4 == s.collections
                    && c.5 == s.compat
                    && c.6 == s.depends
                    && c.7 == s.writable
            })
        });
    if same {
        let rows = specs
            .iter()
            .filter_map(|s| {
                current.iter().find(|c| c.1 == s.name).map(|c| LayerRow {
                    id: c.0,
                    name: s.name.clone(),
                    root: s.root.clone(),
                })
            })
            .collect();
        return Ok((rows, false, 0));
    }

    index.write(|tx| {
        let mut removed = 0u32;
        let mut rows = Vec::with_capacity(specs.len());
        for c in &current {
            let keep = specs
                .iter()
                .any(|s| s.name == c.1 && s.root.to_string_lossy() == c.2);
            if !keep {
                let n: i64 =
                    tx.query_row("SELECT count(*) FROM files WHERE layer_id = ?1", [c.0], |r| {
                        r.get(0)
                    })?;
                removed += n as u32;
                tx.execute("DELETE FROM layers WHERE id = ?1", [c.0])?;
            }
        }
        for s in &specs {
            let root = s.root.to_string_lossy().into_owned();
            let id: Option<i64> = tx
                .query_row("SELECT id FROM layers WHERE name = ?1", [&s.name], |r| {
                    r.get(0)
                })
                .optional()?;
            let id = match id {
                Some(id) => {
                    tx.execute(
                        "UPDATE layers SET root = ?1, priority = ?2, collections = ?3, \
                         compat = ?4, depends = ?5, writable = ?6 WHERE id = ?7",
                        params![
                            root,
                            s.priority,
                            s.collections,
                            s.compat,
                            s.depends,
                            s.writable,
                            id
                        ],
                    )?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO layers(name, root, priority, collections, compat, depends, writable) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            s.name,
                            root,
                            s.priority,
                            s.collections,
                            s.compat,
                            s.depends,
                            s.writable
                        ],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            rows.push(LayerRow {
                id,
                name: s.name.clone(),
                root: s.root.clone(),
            });
        }
        Ok((rows, true, removed))
    })
}

// ---- 2. walk ----

fn walk(ws: &Workspace, layers: &[LayerRow]) -> Vec<Found> {
    let mut out = Vec::new();
    let roots: Vec<PathBuf> = layers.iter().map(|l| l.root.clone()).collect();
    for (i, layer) in layers.iter().enumerate() {
        if layer.name == BUILD_LAYER && ws.layer(BUILD_LAYER).is_none() {
            walk_build_conf(ws, i, &layer.root, &mut out);
            continue;
        }
        let own = layer.root.clone();
        let others: Vec<PathBuf> = roots
            .iter()
            .filter(|r| **r != own && r.starts_with(&own))
            .cloned()
            .collect();
        let walker = ignore::WalkBuilder::new(&layer.root)
            .standard_filters(false)
            .hidden(true)
            .follow_links(false)
            .sort_by_file_name(|a, b| a.cmp(b))
            .filter_entry(move |e| {
                if !e.file_type().is_some_and(|t| t.is_dir()) || e.depth() == 0 {
                    return true;
                }
                let p = e.path();
                let name = e.file_name().to_string_lossy();
                !(SKIP_DIRS.contains(&name.as_ref())
                    || others.iter().any(|o| o == p)
                    || p.join("conf").join("bblayers.conf").is_file())
            })
            .build();
        for entry in walker.flatten() {
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            if let Some(f) = found_file(ws, i, &layer.root, path) {
                out.push(f);
            }
        }
    }
    out
}

/// `<build>/conf/*.conf`.
fn walk_build_conf(ws: &Workspace, i: usize, build: &Path, out: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(build.join("conf")) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if FileKind::from_path(&p) == Some(FileKind::Conf)
            && let Some(f) = found_file(ws, i, build, &p)
        {
            out.push(f);
        }
    }
}

fn found_file(ws: &Workspace, layer: usize, root: &Path, path: &Path) -> Option<Found> {
    let kind = FileKind::from_path(path)?;
    let rel = path.strip_prefix(root).ok()?.to_str()?.to_string();
    let real = ws.confine(path).ok()?;
    let meta = fs::metadata(&real).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    Some(Found {
        layer,
        rel,
        real,
        kind,
        mtime,
        size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
    })
}

// ---- 3. compare, hash, parse ----

fn load_existing(conn: &rusqlite::Connection) -> Result<HashMap<(i64, String), Existing>> {
    let mut st = conn.prepare("SELECT id, layer_id, path, mtime, size, sha256 FROM files")?;
    let rows = st.query_map([], |r| {
        Ok((
            (r.get::<_, i64>(1)?, r.get::<_, String>(2)?),
            Existing {
                id: r.get(0)?,
                mtime: r.get::<_, Option<i64>>(3)?.unwrap_or(-1),
                size: r.get::<_, Option<i64>>(4)?.unwrap_or(-1),
                sha: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn analyse_all(work: &[(Found, Option<Existing>)]) -> Vec<Analysed> {
    if work.len() < PARALLEL_MIN {
        return work.iter().map(|(f, e)| analyse(f, e.as_ref())).collect();
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 16);
    let chunk = work.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = work
            .chunks(chunk)
            .map(|c| {
                let h = s.spawn(move || {
                    c.iter()
                        .map(|(f, e)| analyse(f, e.as_ref()))
                        .collect::<Vec<_>>()
                });
                (c.len(), h)
            })
            .collect();
        // Results stay aligned with `work`: a chunk whose thread panicked counts as
        // unreadable.
        handles
            .into_iter()
            .flat_map(|(n, h)| {
                h.join()
                    .unwrap_or_else(|_| (0..n).map(|_| Analysed::Unreadable).collect())
            })
            .collect()
    })
}

fn analyse(f: &Found, existing: Option<&Existing>) -> Analysed {
    let Ok(bytes) = fs::read(&f.real) else {
        return Analysed::Unreadable;
    };
    let sha = hex(&Sha256::digest(&bytes));
    if existing.is_some_and(|e| e.sha == sha) {
        return Analysed::Same;
    }
    let text = lexer::decode(&bytes);
    let name = Path::new(&f.rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Analysed::Changed {
        sha,
        rows: file_rows(&text, f.kind, &name),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Statements, tasks, provides and depends of one file.
fn file_rows(text: &str, kind: FileKind, file_name: &str) -> FileRows {
    let parsed = crate::parse::parse(text);
    let mut out = FileRows {
        lines: text.lines().count() as u32,
        ..FileRows::default()
    };
    if kind == FileKind::Recipe
        && let (Some(pn), _) = pn_pv(kind, file_name)
    {
        out.provides.push(pn);
    }
    for st in &parsed.statements {
        let src = text.get(st.bytes.clone()).unwrap_or("");
        let (start, end) = (st.span.start, st.span.end);
        let mut row = StmtRow {
            start,
            end,
            snippet: render::snippet(src),
            ..StmtRow::default()
        };
        match &st.kind {
            StatementKind::Assign { var, op, value, .. } => {
                row.kind = "assign";
                set_var(&mut row, var, src);
                row.op = Some(op.as_str().to_string());
                row.value = Some(value.clone());
                if kind != FileKind::Conf {
                    collect_provides(&mut out.provides, var, value);
                }
                collect_depends(&mut out.depends, var, value, start);
            }
            StatementKind::Export { name } => {
                row.kind = "export";
                row.name = Some(name.clone());
                row.base = Some(name.clone());
            }
            StatementKind::Unset { name, flag } => {
                row.kind = "unset";
                row.name = Some(name.clone());
                row.base = Some(name.clone());
                row.flag = flag.clone();
            }
            StatementKind::Function {
                name,
                lang,
                fakeroot,
                ..
            } => {
                row.kind = "function";
                match name {
                    Some(v) => set_var(&mut row, v, src),
                    None => {
                        row.name = Some("__anonymous".into());
                        row.base = Some("__anonymous".into());
                    }
                }
                let l = match lang {
                    Lang::Shell => "shell",
                    Lang::Python => "python",
                };
                row.op = Some(if *fakeroot {
                    format!("fakeroot {l}")
                } else {
                    l.to_string()
                });
            }
            StatementKind::PyDef { name, .. } => {
                row.kind = "pydef";
                row.name = Some(name.clone());
                row.base = Some(name.clone());
                row.op = Some("python".into());
            }
            StatementKind::Inherit { classes, deferred } => {
                row.kind = "inherit";
                row.name = Some(classes.clone());
                row.op = Some(
                    if *deferred {
                        "inherit_defer"
                    } else {
                        "inherit"
                    }
                    .into(),
                );
            }
            StatementKind::Include { path, mode } => {
                row.kind = "include";
                row.name = Some(path.clone());
                row.op = Some(include_op(*mode).into());
            }
            StatementKind::AddTask {
                name,
                before,
                after,
            } => {
                row.kind = "addtask";
                row.name = Some(name.clone());
                for t in name.split_whitespace() {
                    out.tasks
                        .push((t.to_string(), before.join(" "), after.join(" "), start));
                }
            }
            StatementKind::DelTask { name } => {
                row.kind = "deltask";
                row.name = Some(name.clone());
            }
            StatementKind::AddHandler { name } => {
                row.kind = "addhandler";
                row.name = Some(name.clone());
            }
            StatementKind::ExportFunctions { names } => {
                row.kind = "export_functions";
                row.name = Some(names.join(" "));
            }
            StatementKind::Unknown { text } => {
                row.kind = "unknown";
                row.value = Some(text.clone());
            }
        }
        out.statements.push(row);
    }
    out.provides.dedup();
    let mut seen = HashSet::new();
    out.provides.retain(|p| seen.insert(p.clone()));
    out
}

fn include_op(mode: IncludeMode) -> &'static str {
    match mode {
        IncludeMode::Include => "include",
        IncludeMode::Require => "require",
        IncludeMode::IncludeAll => "include_all",
    }
}

fn set_var(row: &mut StmtRow, var: &VarRef, src: &str) {
    row.name = Some(written_name(var, src));
    row.base = Some(var.base.clone());
    row.overrides = (!var.overrides.is_empty()).then(|| var.overrides.join(":"));
    row.flag = var.flag.clone();
    row.legacy = var.legacy_syntax;
}

/// The left-hand side as written (without the flag). For legacy `_` syntax the parser split
/// the part before the first `:` on `_`; the written form is found in the statement text.
pub fn written_name(var: &VarRef, statement_text: &str) -> String {
    let colon =
        |parts: &[String]| -> String { parts.iter().map(|o| format!(":{o}")).collect::<String>() };
    if !var.legacy_syntax {
        return format!("{}{}", var.base, colon(&var.overrides));
    }
    for k in (1..=var.overrides.len()).rev() {
        let cand = format!(
            "{}_{}{}",
            var.base,
            var.overrides[..k].join("_"),
            colon(&var.overrides[k..])
        );
        if statement_text.contains(&cand) {
            let back = crate::parse::split_var(&cand, var.flag.clone());
            if back == *var {
                return cand;
            }
        }
    }
    format!("{}_{}", var.base, var.overrides.join("_"))
}

/// `PN` / `PV` from a recipe or append file name (`bb.parse.vars_from_file`): the name
/// without extension split on `_`; more than three parts is an error (no values). A recipe
/// without a version has `PV = "1.0"` (OE-Core `bitbake.conf`); an append keeps `None`.
pub fn pn_pv(kind: FileKind, file_name: &str) -> (Option<String>, Option<String>) {
    let stem = match kind {
        FileKind::Recipe => file_name.strip_suffix(".bb"),
        FileKind::Append => file_name.strip_suffix(".bbappend"),
        _ => None,
    };
    let Some(stem) = stem else {
        return (None, None);
    };
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() > 3 || parts[0].is_empty() {
        return (None, None);
    }
    let pv = match parts.get(1) {
        Some(v) => Some(v.to_string()),
        None if kind == FileKind::Recipe => Some("1.0".to_string()),
        None => None,
    };
    (Some(parts[0].to_string()), pv)
}

/// Overrides that keep an assignment unconditional for `provides` / `depends`.
fn unconditional(overrides: &[String]) -> bool {
    overrides.iter().all(|o| o == "append" || o == "prepend")
}

fn static_words(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    for w in value.split_whitespace() {
        // Version constraints: `foo (>= 1.0)`.
        if w.starts_with('(') {
            depth += 1;
        }
        let closes = w.ends_with(')');
        if depth == 0 && !w.contains("${") && !w.contains('(') && !w.contains(')') {
            out.push(w.to_string());
        }
        if closes && depth > 0 {
            depth -= 1;
        }
    }
    out
}

fn collect_provides(out: &mut Vec<String>, var: &VarRef, value: &str) {
    if var.base != "PROVIDES" || var.flag.is_some() || !unconditional(&var.overrides) {
        return;
    }
    out.extend(static_words(value));
}

fn collect_depends(
    out: &mut Vec<(&'static str, Option<String>, String, u32)>,
    var: &VarRef,
    value: &str,
    line: u32,
) {
    match (var.base.as_str(), &var.flag) {
        ("DEPENDS", None) if unconditional(&var.overrides) => {
            for w in static_words(value) {
                out.push(("DEPENDS", None, w, line));
            }
        }
        ("RDEPENDS", None) => {
            let Some((pkg, rest)) = var.overrides.split_first() else {
                return;
            };
            if pkg == "append" || pkg == "prepend" || pkg == "remove" || !unconditional(rest) {
                return;
            }
            for w in static_words(value) {
                out.push(("RDEPENDS", Some(pkg.clone()), w, line));
            }
        }
        ("PACKAGECONFIG", Some(flag)) if var.overrides.is_empty() => {
            if let Some(field) = value.split(',').nth(2) {
                for w in static_words(field) {
                    out.push(("PACKAGECONFIG", Some(flag.clone()), w, line));
                }
            }
        }
        _ => {}
    }
}

// ---- 5. rows ----

fn write_file(
    tx: &Transaction<'_>,
    layer_id: i64,
    f: &Found,
    sha: &str,
    rows: &FileRows,
) -> Result<()> {
    let name = Path::new(&f.rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pn, pv) = pn_pv(f.kind, &name);
    let id: Option<i64> = tx
        .query_row(
            "SELECT id FROM files WHERE layer_id = ?1 AND path = ?2",
            params![layer_id, f.rel],
            |r| r.get(0),
        )
        .optional()?;
    let id = match id {
        Some(id) => {
            tx.execute(
                "UPDATE files SET kind = ?1, mtime = ?2, size = ?3, sha256 = ?4, pn = ?5, pv = ?6 \
                 WHERE id = ?7",
                params![f.kind.as_str(), f.mtime, f.size, sha, pn, pv, id],
            )?;
            for table in ["statements", "tasks", "provides", "depends"] {
                tx.execute(&format!("DELETE FROM {table} WHERE file_id = ?1"), [id])?;
            }
            tx.execute("DELETE FROM edges WHERE src_file = ?1", [id])?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO files(layer_id, path, kind, mtime, size, sha256, pn, pv) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    layer_id,
                    f.rel,
                    f.kind.as_str(),
                    f.mtime,
                    f.size,
                    sha,
                    pn,
                    pv
                ],
            )?;
            tx.last_insert_rowid()
        }
    };
    let mut ins = tx.prepare_cached(
        "INSERT INTO statements(file_id, ord, kind, name, base, overrides, flag, op, legacy, \
         value, start_line, end_line, snippet) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for (ord, s) in rows.statements.iter().enumerate() {
        ins.execute(params![
            id,
            ord as i64,
            s.kind,
            s.name,
            s.base,
            s.overrides,
            s.flag,
            s.op,
            s.legacy,
            s.value,
            s.start,
            s.end,
            s.snippet
        ])?;
    }
    let mut ins = tx.prepare_cached(
        "INSERT INTO tasks(file_id, name, before, after, line) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (n, b, a, l) in &rows.tasks {
        ins.execute(params![id, n, b, a, l])?;
    }
    let mut ins = tx.prepare_cached("INSERT INTO provides(file_id, name) VALUES (?1, ?2)")?;
    for p in &rows.provides {
        ins.execute(params![id, p])?;
    }
    let mut ins = tx.prepare_cached(
        "INSERT INTO depends(file_id, kind, pkg, target, line) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (k, pkg, t, l) in &rows.depends {
        ins.execute(params![id, k, pkg, t, l])?;
    }
    Ok(())
}

// ---- 6. edges ----

struct FileInfo {
    abs: PathBuf,
    rel: String,
    kind: Option<FileKind>,
    pn: Option<String>,
    pv: Option<String>,
}

/// Recomputes every edge from the indexed statements and files.
fn rebuild_edges(tx: &Transaction<'_>, ws: &Workspace) -> Result<()> {
    tx.execute("DELETE FROM edges", [])?;

    let mut files: HashMap<i64, FileInfo> = HashMap::new();
    let mut by_path: HashMap<PathBuf, i64> = HashMap::new();
    {
        let mut st = tx.prepare(
            "SELECT f.id, l.root, f.path, f.kind, f.pn, f.pv FROM files f JOIN layers l ON l.id = f.layer_id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, root, rel, kind, pn, pv) = row?;
            let abs = normalize(&Path::new(&root).join(&rel));
            by_path.insert(abs.clone(), id);
            files.insert(
                id,
                FileInfo {
                    abs,
                    rel,
                    kind: kind.and_then(|k| k.parse().ok()),
                    pn,
                    pv,
                },
            );
        }
    }

    let resolver = Resolver::new(ws).with_known_files(by_path.keys().cloned().collect());
    let corebase = ws
        .layers
        .iter()
        .find(|l| l.collections.iter().any(|c| c == "core"))
        .and_then(|l| l.root.parent())
        .map(|p| p.to_string_lossy().into_owned());

    let refs: Vec<(i64, String, String, String, u32)> = {
        let mut st = tx.prepare(
            "SELECT file_id, kind, name, op, start_line FROM statements \
             WHERE kind IN ('include', 'inherit') AND name IS NOT NULL ORDER BY file_id, ord",
        )?;
        st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };

    let mut ins = tx.prepare_cached(
        "INSERT INTO edges(src_file, kind, target_text, target_file, line) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (file_id, kind, text, op, line) in refs {
        let Some(info) = files.get(&file_id) else {
            continue;
        };
        let (edge_kind, ref_kind) = if kind == "inherit" {
            // A class in `classes-global/` inherits in global context, but only on a release
            // that has that directory (`resolve::class_dir`, as the resolver searches).
            let global = info.kind == Some(FileKind::Conf)
                || (info.kind == Some(FileKind::Class)
                    && resolve::class_dir(ws, Path::new(&info.rel)) == resolve::ClassDir::Global);
            ("inherit", RefKind::Inherit { global })
        } else {
            let mode = match op.as_str() {
                "require" => IncludeMode::Require,
                "include_all" => IncludeMode::IncludeAll,
                _ => IncludeMode::Include,
            };
            (include_op(mode), RefKind::Include(mode))
        };
        let vars = |name: &str| static_var(name, info, corebase.as_deref());
        match resolver.resolve_with(ws, ref_kind, &text, &info.abs, &vars) {
            Some(targets) if !targets.is_empty() => {
                let single = targets.len() == 1;
                for t in targets {
                    let target_text = if single { text.clone() } else { t.text };
                    let target = t.file.and_then(|p| by_path.get(&p).copied());
                    ins.execute(params![file_id, edge_kind, target_text, target, line])?;
                }
            }
            Some(_) => {}
            None => {
                ins.execute(params![file_id, edge_kind, text, Option::<i64>::None, line])?;
            }
        }
    }

    // bbappend edges, in BBFILES collection order (the order BitBake applies appends in).
    let collected = resolve::recipe_files(ws);
    let ids: Vec<(i64, PathBuf)> = collected
        .iter()
        .filter_map(|p| {
            let n = normalize(p);
            by_path.get(&n).map(|id| (*id, n))
        })
        .collect();
    let recipes: Vec<&(i64, PathBuf)> = ids
        .iter()
        .filter(|(_, p)| p.extension().is_some_and(|e| e == "bb"))
        .collect();
    for (aid, apath) in ids
        .iter()
        .filter(|(_, p)| p.extension().is_some_and(|e| e == "bbappend"))
    {
        let text = apath
            .file_name()
            .map(|n| n.to_string_lossy().replace(".bbappend", ".bb"))
            .unwrap_or_default();
        let mut any = false;
        for (rid, rpath) in &recipes {
            if resolve::append_matches(apath, rpath) {
                any = true;
                ins.execute(params![aid, "bbappend", text, rid, Option::<u32>::None])?;
            }
        }
        if !any {
            ins.execute(params![
                aid,
                "bbappend",
                text,
                Option::<i64>::None,
                Option::<u32>::None
            ])?;
        }
    }
    Ok(())
}

/// The variables that follow from a file's own path (see docs/bitbake-semantics.md,
/// "Static expansion of reference arguments").
fn static_var(name: &str, info: &FileInfo, corebase: Option<&str>) -> Option<String> {
    let pn = || {
        matches!(info.kind, Some(FileKind::Recipe | FileKind::Append))
            .then(|| info.pn.clone())
            .flatten()
    };
    let pv = || match info.kind {
        Some(FileKind::Recipe) => info.pv.clone(),
        Some(FileKind::Append) => info.pv.clone().filter(|v| !v.contains('%')),
        _ => None,
    };
    let bpn = || {
        let mut p = pn()?;
        if p.starts_with("nativesdk-") {
            return None;
        }
        for s in SPECIAL_PKGSUFFIX {
            if let Some(stripped) = p.strip_suffix(s) {
                p = stripped.to_string();
            }
        }
        Some(p)
    };
    match name {
        "PN" => pn(),
        "PV" => pv(),
        "BPN" => bpn(),
        "BP" => Some(format!("{}-{}", bpn()?, pv()?)),
        "THISDIR" => info.abs.parent().map(|p| p.to_string_lossy().into_owned()),
        "LAYERDIR" if info.rel == "conf/layer.conf" => info
            .abs
            .parent()
            .and_then(Path::parent)
            .map(|p| p.to_string_lossy().into_owned()),
        "COREBASE" => corebase.map(str::to_string),
        _ => None,
    }
}

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
    fn pn_pv_from_file_names() {
        let r = |n| pn_pv(FileKind::Recipe, n);
        assert_eq!(
            r("hello_1.0.bb"),
            (Some("hello".into()), Some("1.0".into()))
        );
        assert_eq!(r("hello.bb"), (Some("hello".into()), Some("1.0".into())));
        assert_eq!(r("a_b_c_d.bb"), (None, None));
        assert_eq!(
            pn_pv(FileKind::Append, "hello_%.bbappend"),
            (Some("hello".into()), Some("%".into()))
        );
        assert_eq!(
            pn_pv(FileKind::Append, "hello.bbappend"),
            (Some("hello".into()), None)
        );
        assert_eq!(pn_pv(FileKind::Include, "hello.inc"), (None, None));
    }

    #[test]
    fn written_name_recovers_legacy_form() {
        let v = crate::parse::split_var("RDEPENDS_${PN}-dev", None);
        assert!(v.legacy_syntax);
        assert_eq!(
            written_name(&v, "RDEPENDS_${PN}-dev = \"x\""),
            "RDEPENDS_${PN}-dev"
        );
        let v = crate::parse::split_var("EXTRA_OECONF_append_qemux86", None);
        assert_eq!(
            written_name(&v, "EXTRA_OECONF_append_qemux86 = \"x\""),
            "EXTRA_OECONF_append_qemux86"
        );
        let v = crate::parse::split_var("FILES:${PN}-dev:append", None);
        assert_eq!(written_name(&v, ""), "FILES:${PN}-dev:append");
    }

    #[test]
    fn static_words_drop_versions_and_variables() {
        assert_eq!(
            static_words("a (>= 1.0) ${PN}-b c (= 2) d"),
            ["a", "c", "d"]
        );
    }

    #[test]
    fn rows_for_a_recipe() {
        let src = "PROVIDES = \"virtual/x ${PN}-y\"\nDEPENDS:append = \" zlib\"\n\
                   DEPENDS:class-native = \"no\"\nRDEPENDS:${PN} += \"bash (>= 5)\"\n\
                   PACKAGECONFIG[ssl] = \"--with,--without,openssl,libssl\"\n\
                   addtask a b after c before d\n";
        let r = file_rows(src, FileKind::Recipe, "foo_1.0.bb");
        assert_eq!(r.provides, ["foo", "virtual/x"]);
        let deps: Vec<_> = r
            .depends
            .iter()
            .map(|(k, p, t, l)| (*k, p.as_deref(), t.as_str(), *l))
            .collect();
        assert_eq!(
            deps,
            [
                ("DEPENDS", None, "zlib", 2),
                ("RDEPENDS", Some("${PN}"), "bash", 4),
                ("PACKAGECONFIG", Some("ssl"), "openssl", 5),
            ]
        );
        assert_eq!(
            r.tasks,
            [
                ("a".into(), "d".into(), "c".into(), 6),
                ("b".into(), "d".into(), "c".into(), 6)
            ]
        );
    }
}
