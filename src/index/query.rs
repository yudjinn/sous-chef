//! Read queries returning row structs. Tools never write SQL themselves.
//!
//! Every function takes the [`Index`] (refreshed by the caller) and reads through
//! [`Index::conn`]. Column conventions are documented in `scan.rs`. Pointers ([`Loc`]) are
//! built from the `layers.name` and `files.path` columns.
//!
//! Orders: files of one layer are listed by path; layers by their row id, which is the
//! order the scanner first saw them in (`BBLAYERS` order, build directory last) unless
//! layers were added or removed later. Where BitBake's order matters (bbappend application,
//! class precedence, preferred version) the function says how it is obtained.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use rusqlite::types::ToSql;
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::Index;
use crate::error::{Error, Result};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::Workspace;
use crate::workspace::resolve;

/// One indexed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRow {
    pub id: i64,
    pub layer: String,
    pub layer_id: i64,
    /// Relative to the layer root.
    pub path: PathBuf,
    pub kind: FileKind,
    pub sha256: String,
    /// From the file name (`.bb` / `.bbappend` only).
    pub pn: Option<String>,
    pub pv: Option<String>,
    /// The layer's `BBFILE_PRIORITY`.
    pub priority: i32,
}

impl FileRow {
    /// A pointer into this file.
    pub fn loc(&self, lines: LineSpan) -> Loc {
        Loc {
            layer: self.layer.clone(),
            path: self.path.clone(),
            lines,
        }
    }
}

/// A recipe (`.bb`) with its name and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecipeRow {
    pub file: FileRow,
    pub pn: String,
    pub pv: String,
}

/// One statement. `loc` covers the whole statement (function bodies included).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatementRow {
    pub id: i64,
    pub file_id: i64,
    pub loc: Loc,
    /// 0-based position in the file.
    pub ord: u32,
    /// `assign`, `function`, `pydef`, `inherit`, `include`, `addtask`, `deltask`,
    /// `addhandler`, `export_functions`, `export`, `unset`, `unknown`.
    pub kind: String,
    pub name: Option<String>,
    pub base: Option<String>,
    pub overrides: Vec<String>,
    pub flag: Option<String>,
    pub op: Option<String>,
    pub legacy: bool,
    pub value: Option<String>,
    pub snippet: String,
}

/// What [`enclosing_item`] returns.
pub type ItemRow = StatementRow;

/// One edge (`require`, `include`, `include_all`, `inherit`, `bbappend`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgeRow {
    pub id: i64,
    pub kind: String,
    pub target_text: String,
    /// Line of the statement in `src`; `None` for `bbappend` edges.
    pub line: Option<u32>,
    pub src: FileRow,
    /// `None` when unresolved (not found, or needs evaluation).
    pub target: Option<FileRow>,
}

impl EdgeRow {
    /// The pointer of the statement that made this edge (line 1 for `bbappend`).
    pub fn via(&self) -> Loc {
        self.src.loc(LineSpan::line(self.line.unwrap_or(1)))
    }
}

/// One node of [`tree`], in parse order (depth-first, statement order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeNode {
    /// 0 for the root.
    pub depth: u32,
    /// Edge kind that pulled the file in (`require`, `include`, `include_all`, `inherit`);
    /// `root` for the root.
    pub kind: String,
    pub target_text: String,
    /// The statement that pulled it in; `None` for the root.
    pub via: Option<Loc>,
    /// `None`: unresolved.
    pub file: Option<FileRow>,
    /// The file is already being parsed further up this branch (an include cycle); its
    /// children are not repeated.
    pub cycle: bool,
    /// A class inherited earlier in the tree: BitBake does not parse it again
    /// (`__inherit_cache`), so its children are not repeated.
    pub repeated: bool,
}

/// Restricts statement queries. Empty fields do not restrict.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatementFilter {
    /// Statement kinds (`assign`, `function`, ...).
    pub kinds: Vec<String>,
    /// Layer name.
    pub layer: Option<String>,
    pub file_kind: Option<FileKind>,
    /// Only these files; results follow this order (e.g. a recipe's [`tree`] files).
    pub files: Option<Vec<i64>>,
}

/// A file that inherits a class, directly (`depth` 1) or through an include, an inherited
/// class or a bbappend (`depth` > 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserRow {
    pub file: FileRow,
    /// The statement that links it to the class or to the previous user; for a recipe
    /// reached through its bbappend, the append's line 1.
    pub via: Loc,
    pub depth: u32,
}

/// A class file and its precedence (see [`class_files`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassFileRow {
    pub file: FileRow,
    /// Position in BitBake's search order (0 wins); `None` if the file is not on the search
    /// path for this kind of inherit (e.g. `classes-global/` for a recipe inherit).
    pub rank: Option<usize>,
}

/// Per-layer counts for `status` and `layers show=list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerStats {
    pub name: String,
    pub root: PathBuf,
    pub priority: i32,
    pub collections: Vec<String>,
    pub compat: Vec<String>,
    pub depends: Vec<String>,
    pub writable: bool,
    pub recipes: u32,
    pub appends: u32,
    pub includes: u32,
    pub classes: u32,
    pub confs: u32,
    pub statements: u32,
}

/// The same recipe or class provided by more than one layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowRow {
    /// `recipe` or `class`.
    pub kind: String,
    /// The file name (`hello_1.0.bb`, `base.bbclass`).
    pub name: String,
    /// Winner first.
    pub files: Vec<FileRow>,
}

// ---- row mapping ----

const FILE_COLS: &str =
    "f.id, l.name, f.layer_id, f.path, f.kind, f.sha256, f.pn, f.pv, l.priority";
const FILE_FROM: &str = "files f JOIN layers l ON l.id = f.layer_id";

fn file_row(r: &Row<'_>, at: usize) -> rusqlite::Result<FileRow> {
    let kind: String = r.get(at + 4)?;
    Ok(FileRow {
        id: r.get(at)?,
        layer: r.get(at + 1)?,
        layer_id: r.get(at + 2)?,
        path: PathBuf::from(r.get::<_, String>(at + 3)?),
        kind: kind.parse().unwrap_or(FileKind::Conf),
        sha256: r.get::<_, Option<String>>(at + 5)?.unwrap_or_default(),
        pn: r.get(at + 6)?,
        pv: r.get(at + 7)?,
        priority: r.get::<_, Option<i32>>(at + 8)?.unwrap_or(0),
    })
}

const STMT_COLS: &str = "s.id, s.file_id, l.name, f.path, s.ord, s.kind, s.name, s.base, \
                         s.overrides, s.flag, s.op, s.legacy, s.value, s.start_line, s.end_line, s.snippet";
const STMT_FROM: &str =
    "statements s JOIN files f ON f.id = s.file_id JOIN layers l ON l.id = f.layer_id";

fn stmt_row(r: &Row<'_>) -> rusqlite::Result<StatementRow> {
    let overrides: Option<String> = r.get(8)?;
    Ok(StatementRow {
        id: r.get(0)?,
        file_id: r.get(1)?,
        loc: Loc {
            layer: r.get(2)?,
            path: PathBuf::from(r.get::<_, String>(3)?),
            lines: LineSpan::new(r.get(13)?, r.get(14)?),
        },
        ord: r.get(4)?,
        kind: r.get(5)?,
        name: r.get(6)?,
        base: r.get(7)?,
        overrides: overrides
            .map(|o| o.split(':').map(str::to_string).collect())
            .unwrap_or_default(),
        flag: r.get(9)?,
        op: r.get(10)?,
        legacy: r.get::<_, Option<bool>>(11)?.unwrap_or(false),
        value: r.get(12)?,
        snippet: r.get::<_, Option<String>>(15)?.unwrap_or_default(),
    })
}

fn collect<T>(
    index: &Index,
    sql: &str,
    params: &[&dyn ToSql],
    f: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut st = index.conn().prepare(sql)?;
    let rows = st.query_map(params, f)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---- files ----

/// A file by id.
pub fn file(index: &Index, id: i64) -> Result<Option<FileRow>> {
    let sql = format!("SELECT {FILE_COLS} FROM {FILE_FROM} WHERE f.id = ?1");
    Ok(index
        .conn()
        .query_row(&sql, [id], |r| file_row(r, 0))
        .optional()?)
}

/// A file by layer name and path relative to the layer root.
pub fn file_by_path(index: &Index, layer: &str, path: &Path) -> Result<Option<FileRow>> {
    let sql = format!("SELECT {FILE_COLS} FROM {FILE_FROM} WHERE l.name = ?1 AND f.path = ?2");
    Ok(index
        .conn()
        .query_row(&sql, params![layer, path.to_string_lossy()], |r| {
            file_row(r, 0)
        })
        .optional()?)
}

/// A file by absolute path (as hooks see it), through [`Workspace::loc_for`].
pub fn file_by_abs(index: &Index, ws: &Workspace, abs: &Path) -> Result<Option<FileRow>> {
    match ws.loc_for(abs, LineSpan::line(1)) {
        Some(loc) => file_by_path(index, &loc.layer, &loc.path),
        None => Ok(None),
    }
}

/// Every indexed file (the build pseudo-layer included), by layer row id then path.
pub fn all_files(index: &Index) -> Result<Vec<FileRow>> {
    let sql = format!("SELECT {FILE_COLS} FROM {FILE_FROM} ORDER BY l.id, f.path");
    collect(index, &sql, &[], |r| file_row(r, 0))
}

/// Every indexed file of one of `kinds`, by layer row id then path. An empty `kinds`
/// returns nothing.
pub fn files_by_kind(index: &Index, kinds: &[FileKind]) -> Result<Vec<FileRow>> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<&'static str> = kinds.iter().map(|k| k.as_str()).collect();
    let marks: Vec<String> = (1..=names.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT {FILE_COLS} FROM {FILE_FROM} WHERE f.kind IN ({}) ORDER BY l.id, f.path",
        marks.join(", ")
    );
    let refs: Vec<&dyn ToSql> = names.iter().map(|n| n as &dyn ToSql).collect();
    collect(index, &sql, &refs, |r| file_row(r, 0))
}

/// The indexed files of the layer named `layer` (`build` for the build pseudo-layer), by
/// path. An unknown layer has none.
pub fn files_in_layer(index: &Index, layer: &str) -> Result<Vec<FileRow>> {
    let sql = format!("SELECT {FILE_COLS} FROM {FILE_FROM} WHERE l.name = ?1 ORDER BY f.path");
    collect(index, &sql, &[&layer], |r| file_row(r, 0))
}

/// `%` / `_` / `\` escaped for a `LIKE ... ESCAPE '\'` pattern.
fn like_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '%' | '_' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect()
}

/// Distinct recipe names (`PN` of `.bb` files) containing `q`, ASCII case-insensitive,
/// sorted, at most `limit`. For "did you mean" lists.
pub fn recipe_names_like(index: &Index, q: &str, limit: usize) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT DISTINCT pn FROM files WHERE kind = 'bb' AND pn LIKE ?1 ESCAPE '\\' \
         ORDER BY pn LIMIT {limit}"
    );
    let pattern = format!("%{}%", like_escape(q));
    collect(index, &sql, &[&pattern], |r| r.get::<_, String>(0))
}

/// Distinct class names (stems of indexed `.bbclass` files, any directory) containing `q`,
/// ASCII case-insensitive, sorted, at most `limit`. For "did you mean" lists.
pub fn class_names_like(index: &Index, q: &str, limit: usize) -> Result<Vec<String>> {
    let q = q.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    for f in files_by_kind(index, &[FileKind::Class])? {
        let path = f.path.to_string_lossy();
        let stem = path
            .rsplit('/')
            .next()
            .unwrap_or(&path)
            .trim_end_matches(".bbclass")
            .to_string();
        if stem.to_ascii_lowercase().contains(&q) && !out.contains(&stem) {
            out.push(stem);
        }
    }
    out.sort();
    out.truncate(limit);
    Ok(out)
}

// ---- recipes ----

/// Every recipe file whose `PN` is `name`: all versions, all layers. Ordered by layer
/// priority (highest first), then version (highest first), then layer and path.
pub fn find_recipes(index: &Index, name: &str) -> Result<Vec<RecipeRow>> {
    let sql = format!(
        "SELECT {FILE_COLS} FROM {FILE_FROM} WHERE f.kind = 'bb' AND f.pn = ?1 \
         ORDER BY l.priority DESC, l.id, f.path"
    );
    let mut rows: Vec<RecipeRow> = collect(index, &sql, &[&name], |r| file_row(r, 0))?
        .into_iter()
        .filter_map(|f| {
            Some(RecipeRow {
                pn: f.pn.clone()?,
                pv: f.pv.clone()?,
                file: f,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.file
            .priority
            .cmp(&a.file.priority)
            .then_with(|| vercmp(&b.pv, &a.pv))
            .then_with(|| a.file.layer_id.cmp(&b.file.layer_id))
            .then_with(|| a.file.path.cmp(&b.file.path))
    });
    Ok(rows)
}

/// The recipe BitBake would pick for `name` from layer priority and version alone
/// (`providers.findBestProvider` without `PREFERRED_VERSION` or `DEFAULT_PREFERENCE`; M3
/// refines this):
///
/// - with `version`: the first recipe, highest priority layer first, whose `PV` equals it, or
///   starts with it minus a trailing `%` (`preferredVersionMatch`). Within one priority the
///   first match in layer and path order wins, not the highest version (BitBake takes the
///   first match in its file order). A leading `N:` epoch is ignored (`PE` is not known
///   statically);
/// - without: the highest version (`vercmp`) among the recipes of the highest priority
///   layer; on equal versions the first one in layer and path order.
pub fn preferred_recipe(
    index: &Index,
    name: &str,
    version: Option<&str>,
) -> Result<Option<RecipeRow>> {
    let mut all = find_recipes(index, name)?;
    // Stable order within a priority: layer then path (BitBake: file collection order).
    all.sort_by(|a, b| {
        b.file
            .priority
            .cmp(&a.file.priority)
            .then_with(|| a.file.layer_id.cmp(&b.file.layer_id))
            .then_with(|| a.file.path.cmp(&b.file.path))
    });
    if let Some(v) = version {
        let v = strip_epoch(v);
        return Ok(all.into_iter().find(|r| version_matches(&r.pv, v)));
    }
    let Some(top) = all.first().map(|r| r.file.priority) else {
        return Ok(None);
    };
    let mut best: Option<RecipeRow> = None;
    for r in all.into_iter().filter(|r| r.file.priority == top) {
        let better = match &best {
            None => true,
            Some(b) => vercmp(&b.pv, &r.pv) == Ordering::Less,
        };
        if better {
            best = Some(r);
        }
    }
    Ok(best)
}

fn strip_epoch(v: &str) -> &str {
    match v.split_once(':') {
        Some((e, rest)) if !e.is_empty() && e.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => v,
    }
}

/// `providers.preferredVersionMatch` on `PV`: equal, or a trailing `%` matches any rest.
pub fn version_matches(pv: &str, preferred: &str) -> bool {
    pv == preferred
        || preferred
            .strip_suffix('%')
            .is_some_and(|prefix| pv.starts_with(prefix))
}

/// The bbappends that apply to a recipe file, in the order BitBake applies them (the
/// `BBFILES` collection order: lowest layer priority first).
pub fn appends_for(index: &Index, recipe_file: i64) -> Result<Vec<FileRow>> {
    let sql = format!(
        "SELECT {FILE_COLS} FROM edges e JOIN files f ON f.id = e.src_file \
         JOIN layers l ON l.id = f.layer_id \
         WHERE e.kind = 'bbappend' AND e.target_file = ?1 ORDER BY e.id"
    );
    collect(index, &sql, &[&recipe_file], |r| file_row(r, 0))
}

/// bbappends that match no recipe.
pub fn orphan_appends(index: &Index) -> Result<Vec<FileRow>> {
    let sql = format!(
        "SELECT {FILE_COLS} FROM {FILE_FROM} WHERE f.kind = 'bbappend' AND EXISTS \
         (SELECT 1 FROM edges e WHERE e.src_file = f.id AND e.kind = 'bbappend' \
          AND e.target_file IS NULL) ORDER BY l.id, f.path"
    );
    collect(index, &sql, &[], |r| file_row(r, 0))
}

// ---- statements ----

fn filter_sql(filter: &StatementFilter, sql: &mut String, params: &mut Vec<Box<dyn ToSql>>) {
    if !filter.kinds.is_empty() {
        let marks: Vec<String> = filter
            .kinds
            .iter()
            .map(|k| {
                params.push(Box::new(k.clone()));
                format!("?{}", params.len())
            })
            .collect();
        sql.push_str(&format!(" AND s.kind IN ({})", marks.join(", ")));
    }
    if let Some(layer) = &filter.layer {
        params.push(Box::new(layer.clone()));
        sql.push_str(&format!(" AND l.name = ?{}", params.len()));
    }
    if let Some(k) = filter.file_kind {
        params.push(Box::new(k.as_str()));
        sql.push_str(&format!(" AND f.kind = ?{}", params.len()));
    }
}

/// Runs a statement query with `filter` appended to `where_` and orders the result:
/// in `filter.files` order when given, else by layer, path and position.
fn statements_where(
    index: &Index,
    where_: &str,
    mut params: Vec<Box<dyn ToSql>>,
    filter: &StatementFilter,
    order: &str,
    limit: Option<usize>,
) -> Result<Vec<StatementRow>> {
    let mut sql = format!("SELECT {STMT_COLS} FROM {STMT_FROM} WHERE {where_}");
    filter_sql(filter, &mut sql, &mut params);
    let rank: Option<HashMap<i64, usize>> = filter
        .files
        .as_ref()
        .map(|fs| fs.iter().enumerate().map(|(i, f)| (*f, i)).collect());
    if let Some(r) = &rank {
        // Restrict in SQL so the limit counts only kept rows.
        let ids: Vec<String> = r.keys().map(|k| k.to_string()).collect();
        sql.push_str(&format!(" AND s.file_id IN ({})", ids.join(", ")));
    }
    sql.push_str(&format!(" ORDER BY {order}"));
    if let Some(n) = limit
        && rank.is_none()
    {
        sql.push_str(&format!(" LIMIT {n}"));
    }
    let refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = collect(index, &sql, &refs, stmt_row)?;
    if let Some(r) = &rank {
        rows.sort_by_key(|s| (r.get(&s.file_id).copied().unwrap_or(usize::MAX), s.ord));
        if let Some(n) = limit {
            rows.truncate(n);
        }
    }
    Ok(rows)
}

const DEFAULT_ORDER: &str = "l.id, f.path, s.ord";

/// The statements of one file, in file order.
pub fn statements_in(
    index: &Index,
    file: i64,
    filter: &StatementFilter,
) -> Result<Vec<StatementRow>> {
    statements_where(
        index,
        "s.file_id = ?1",
        vec![Box::new(file)],
        filter,
        "s.ord",
        None,
    )
}

/// Statements of one file whose name is exactly `name` (e.g. `do_install:append`). Several
/// statements can share a name.
pub fn statement_by_name(index: &Index, file: i64, name: &str) -> Result<Vec<StatementRow>> {
    statements_where(
        index,
        "s.file_id = ?1 AND s.name = ?2",
        vec![Box::new(file), Box::new(name.to_string())],
        &StatementFilter::default(),
        "s.ord",
        None,
    )
}

/// Every statement on variable `base` (assignments, functions, `export`, `unset` with that
/// base) within `scope`.
pub fn statements_by_base(
    index: &Index,
    base: &str,
    scope: &StatementFilter,
) -> Result<Vec<StatementRow>> {
    statements_where(
        index,
        "s.base = ?1",
        vec![Box::new(base.to_string())],
        scope,
        DEFAULT_ORDER,
        None,
    )
}

/// The statement covering `line` of `file` (a function body counts), if any.
pub fn enclosing_item(index: &Index, file: i64, line: u32) -> Result<Option<ItemRow>> {
    let sql = format!(
        "SELECT {STMT_COLS} FROM {STMT_FROM} WHERE s.file_id = ?1 AND s.start_line <= ?2 \
         AND s.end_line >= ?2 ORDER BY s.start_line DESC LIMIT 1"
    );
    Ok(index
        .conn()
        .query_row(&sql, params![file, line], stmt_row)
        .optional()?)
}

/// Statements whose name contains `query` (ASCII case-insensitive). Exact matches first,
/// then by layer, path and position.
pub fn search_names(
    index: &Index,
    query: &str,
    filter: &StatementFilter,
    limit: usize,
) -> Result<Vec<StatementRow>> {
    let pattern = format!("%{}%", like_escape(query));
    statements_where(
        index,
        "s.name LIKE ?1 ESCAPE '\\'",
        vec![Box::new(pattern), Box::new(query.to_string())],
        filter,
        "(lower(s.name) = lower(?2)) DESC, l.id, f.path, s.ord",
        Some(limit),
    )
}

/// Full-text search over statement names and snippets (SQLite FTS5 query syntax), best
/// matches first. A malformed query is an `Invalid` error.
pub fn search_fts(
    index: &Index,
    query: &str,
    filter: &StatementFilter,
    limit: usize,
) -> Result<Vec<StatementRow>> {
    let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(query.to_string())];
    let mut sql = format!(
        "SELECT {STMT_COLS} FROM statements_fts \
         JOIN statements s ON s.id = statements_fts.rowid \
         JOIN files f ON f.id = s.file_id JOIN layers l ON l.id = f.layer_id \
         WHERE statements_fts MATCH ?1"
    );
    filter_sql(filter, &mut sql, &mut params);
    if let Some(fs) = &filter.files {
        let ids: Vec<String> = fs.iter().map(|k| k.to_string()).collect();
        sql.push_str(&format!(" AND s.file_id IN ({})", ids.join(", ")));
    }
    sql.push_str(&format!(
        " ORDER BY rank, l.id, f.path, s.ord LIMIT {limit}"
    ));
    let refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    collect(index, &sql, &refs, stmt_row).map_err(|e| match e {
        // SQLITE_ERROR from MATCH: a syntax error in the query (`fts5: syntax error near`,
        // `unterminated string`, `no such column`...).
        Error::Sqlite(rusqlite::Error::SqliteFailure(f, Some(msg)))
            if f.code == rusqlite::ErrorCode::Unknown =>
        {
            Error::Invalid {
                msg: format!("bad full-text query {query:?}: {msg}; quote words with \"...\""),
            }
        }
        other => other,
    })
}

// ---- edges and trees ----

const EDGE_SELECT: &str = "SELECT e.id, e.kind, e.target_text, e.line, \
     fs.id, ls.name, fs.layer_id, fs.path, fs.kind, fs.sha256, fs.pn, fs.pv, ls.priority, \
     ft.id, lt.name, ft.layer_id, ft.path, ft.kind, ft.sha256, ft.pn, ft.pv, lt.priority \
     FROM edges e JOIN files fs ON fs.id = e.src_file JOIN layers ls ON ls.id = fs.layer_id \
     LEFT JOIN files ft ON ft.id = e.target_file LEFT JOIN layers lt ON lt.id = ft.layer_id";

fn edge_row(r: &Row<'_>) -> rusqlite::Result<EdgeRow> {
    let target = match r.get::<_, Option<i64>>(13)? {
        Some(_) => Some(file_row(r, 13)?),
        None => None,
    };
    Ok(EdgeRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        target_text: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        line: r.get(3)?,
        src: file_row(r, 4)?,
        target,
    })
}

/// Edges out of `file`, in statement order (bbappend edges last).
pub fn edges_from(index: &Index, file: i64) -> Result<Vec<EdgeRow>> {
    let sql = format!("{EDGE_SELECT} WHERE e.src_file = ?1 ORDER BY e.line IS NULL, e.line, e.id");
    collect(index, &sql, &[&file], edge_row)
}

/// Edges pointing at `file`, by source layer and path.
pub fn edges_to(index: &Index, file: i64) -> Result<Vec<EdgeRow>> {
    let sql =
        format!("{EDGE_SELECT} WHERE e.target_file = ?1 ORDER BY ls.id, fs.path, e.line, e.id");
    collect(index, &sql, &[&file], edge_row)
}

/// The include / inherit closure of `root_file` in parse order: depth first, each file's
/// `require` / `include` / `include_all` / `inherit` edges in statement order. Static
/// resolution only: unresolved targets are leaves. Cycle safe (a file already open on the
/// current branch is marked `cycle`); a class inherited a second time is marked `repeated`
/// and not expanded again, as BitBake parses it only once.
pub fn tree(index: &Index, root_file: i64) -> Result<Vec<TreeNode>> {
    let Some(root) = file(index, root_file)? else {
        return Err(Error::NotFound {
            what: format!("file id {root_file} in the index"),
        });
    };
    let mut out = vec![TreeNode {
        depth: 0,
        kind: "root".into(),
        target_text: root.path.to_string_lossy().into_owned(),
        via: None,
        file: Some(root),
        cycle: false,
        repeated: false,
    }];
    let mut stack = vec![root_file];
    let mut inherited: HashSet<i64> = HashSet::new();
    walk_tree(index, root_file, 1, &mut stack, &mut inherited, &mut out)?;
    Ok(out)
}

/// Guards against pathological depth (BitBake has no limit; a cycle is caught by `stack`).
const MAX_TREE_DEPTH: u32 = 64;

fn walk_tree(
    index: &Index,
    file_id: i64,
    depth: u32,
    stack: &mut Vec<i64>,
    inherited: &mut HashSet<i64>,
    out: &mut Vec<TreeNode>,
) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Ok(());
    }
    for e in edges_from(index, file_id)? {
        if e.kind == "bbappend" {
            continue;
        }
        let via = Some(e.via());
        let target_id = e.target.as_ref().map(|t| t.id);
        let cycle = target_id.is_some_and(|t| stack.contains(&t));
        let repeated = e.kind == "inherit" && target_id.is_some_and(|t| inherited.contains(&t));
        out.push(TreeNode {
            depth,
            kind: e.kind.clone(),
            target_text: e.target_text.clone(),
            via,
            file: e.target.clone(),
            cycle,
            repeated: repeated && !cycle,
        });
        let Some(t) = target_id else { continue };
        if cycle || repeated {
            continue;
        }
        if e.kind == "inherit" {
            inherited.insert(t);
        }
        stack.push(t);
        walk_tree(index, t, depth + 1, stack, inherited, out)?;
        stack.pop();
    }
    Ok(())
}

// ---- classes ----

/// Every indexed `<name>.bbclass`, in BitBake's precedence for an `inherit` (`global ==
/// false`) or an `INHERIT` (`global == true`): files on the search path first, ranked as
/// `workspace::resolve` searches (`classes-<type>/` along all of `BBPATH`, then `classes/`;
/// only `classes/` on a release without `Feature::SplitClassDirs`), then the others with
/// `rank == None`. A file BitBake cannot find by name on this release (for example
/// `classes-recipe/` on Kirkstone) is listed, never ranked.
pub fn class_files(
    index: &Index,
    ws: &Workspace,
    name: &str,
    global: bool,
) -> Result<Vec<ClassFileRow>> {
    let file_name = format!("{name}.bbclass");
    let sql = format!(
        "SELECT {FILE_COLS} FROM {FILE_FROM} WHERE f.kind = 'bbclass' \
         AND (f.path = ?1 OR f.path LIKE ?2 ESCAPE '\\') ORDER BY l.id, f.path"
    );
    let like = format!(
        "%/{}",
        file_name
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let files = collect(index, &sql, &[&file_name, &like], |r| file_row(r, 0))?;
    let order = resolve::class_candidates(ws, name, global);
    let mut ranked: Vec<ClassFileRow> = files
        .into_iter()
        .map(|f| {
            let abs = ws.abs_path(&f.loc(LineSpan::line(1)));
            let rank = abs.and_then(|a| order.iter().position(|o| *o == a));
            ClassFileRow { file: f, rank }
        })
        .collect();
    ranked.sort_by_key(|c| c.rank.unwrap_or(usize::MAX));
    Ok(ranked)
}

/// Files that inherit class `class`: directly (`depth` 1), and with `transitive` also every
/// file that pulls a user in (through `require` / `include` / `include_all` / `inherit`) and
/// every recipe whose bbappend is a user, breadth first. Only resolved edges count.
pub fn inheritors(index: &Index, class: &str, transitive: bool) -> Result<Vec<UserRow>> {
    let file_name = format!("{class}.bbclass");
    let sql = format!(
        "{EDGE_SELECT} WHERE e.kind = 'inherit' AND ft.kind = 'bbclass' \
         AND (ft.path = ?1 OR ft.path LIKE ?2 ESCAPE '\\') ORDER BY ls.id, fs.path, e.line"
    );
    let like = format!(
        "%/{}",
        file_name
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let direct = collect(index, &sql, &[&file_name, &like], edge_row)?;
    let mut out: Vec<UserRow> = Vec::new();
    let mut seen: HashSet<i64> = direct
        .iter()
        .filter_map(|e| e.target.as_ref().map(|t| t.id))
        .collect();
    let mut queue: VecDeque<(i64, u32)> = VecDeque::new();
    for e in direct {
        if seen.insert(e.src.id) {
            queue.push_back((e.src.id, 1));
            out.push(UserRow {
                via: e.via(),
                file: e.src,
                depth: 1,
            });
        }
    }
    if !transitive {
        return Ok(out);
    }
    while let Some((id, depth)) = queue.pop_front() {
        let mut next: Vec<UserRow> = Vec::new();
        for e in edges_to(index, id)? {
            if e.kind != "bbappend" {
                next.push(UserRow {
                    via: e.via(),
                    file: e.src,
                    depth: depth + 1,
                });
            }
        }
        for e in edges_from(index, id)? {
            if e.kind == "bbappend"
                && let Some(t) = e.target.clone()
            {
                next.push(UserRow {
                    via: e.via(),
                    file: t,
                    depth: depth + 1,
                });
            }
        }
        for u in next {
            if seen.insert(u.file.id) {
                queue.push_back((u.file.id, u.depth));
                out.push(u);
            }
        }
    }
    Ok(out)
}

// ---- layers ----

/// One row per indexed layer (the build pseudo-layer included), with file counts by kind.
pub fn layer_stats(index: &Index) -> Result<Vec<LayerStats>> {
    let sql = "SELECT l.id, l.name, l.root, l.priority, l.collections, l.compat, l.depends, l.writable, \
         (SELECT count(*) FROM files f WHERE f.layer_id = l.id AND f.kind = 'bb'), \
         (SELECT count(*) FROM files f WHERE f.layer_id = l.id AND f.kind = 'bbappend'), \
         (SELECT count(*) FROM files f WHERE f.layer_id = l.id AND f.kind = 'inc'), \
         (SELECT count(*) FROM files f WHERE f.layer_id = l.id AND f.kind = 'bbclass'), \
         (SELECT count(*) FROM files f WHERE f.layer_id = l.id AND f.kind = 'conf'), \
         (SELECT count(*) FROM statements s JOIN files f ON f.id = s.file_id WHERE f.layer_id = l.id) \
         FROM layers l ORDER BY l.id";
    let words = |s: Option<String>| -> Vec<String> {
        s.unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    collect(index, sql, &[], |r| {
        Ok(LayerStats {
            name: r.get(1)?,
            root: PathBuf::from(r.get::<_, String>(2)?),
            priority: r.get::<_, Option<i32>>(3)?.unwrap_or(0),
            collections: words(r.get(4)?),
            compat: words(r.get(5)?),
            depends: words(r.get(6)?),
            writable: r.get::<_, Option<bool>>(7)?.unwrap_or(false),
            recipes: r.get(8)?,
            appends: r.get(9)?,
            includes: r.get(10)?,
            classes: r.get(11)?,
            confs: r.get(12)?,
            statements: r.get(13)?,
        })
    })
}

/// Recipes and classes provided by more than one layer.
///
/// - Recipes: the same `.bb` file name in several layers (`cooker.collect_bbfiles`
///   "overlayed" recipes). The winner is the file in the highest priority layer; on equal
///   priority the later one in collection order (`l.id`, path).
/// - Classes: the same `.bbclass` file name in several layers, ranked by
///   [`class_files`] for a recipe inherit (a class only in `classes-global/` by the global
///   order). Files off the search path come last. A class file BitBake cannot find by name
///   on the workspace release (`classes-recipe/` or `classes-global/` before Langdale, see
///   [`resolve::class_dir`]) is not a candidate and takes no part.
pub fn shadowed(index: &Index, ws: &Workspace) -> Result<Vec<ShadowRow>> {
    let sql = format!(
        "SELECT {FILE_COLS} FROM {FILE_FROM} WHERE f.kind IN ('bb', 'bbclass') ORDER BY l.id, f.path"
    );
    let files = collect(index, &sql, &[], |r| file_row(r, 0))?;
    let mut groups: Vec<(String, String, Vec<FileRow>)> = Vec::new();
    for f in files {
        if f.kind == FileKind::Class
            && resolve::class_dir(ws, &f.path) == resolve::ClassDir::Unsearched
        {
            continue;
        }
        let name = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let kind = if f.kind == FileKind::Recipe {
            "recipe"
        } else {
            "class"
        };
        match groups.iter_mut().find(|g| g.0 == kind && g.1 == name) {
            Some(g) => g.2.push(f),
            None => groups.push((kind.to_string(), name, vec![f])),
        }
    }
    let mut out = Vec::new();
    for (kind, name, mut files) in groups {
        let layers: HashSet<i64> = files.iter().map(|f| f.layer_id).collect();
        if layers.len() < 2 {
            continue;
        }
        if kind == "recipe" {
            // Highest priority first; equal priority: later collection order wins.
            let pos: HashMap<i64, usize> =
                files.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
            files.sort_by(|a, b| {
                b.priority
                    .cmp(&a.priority)
                    .then_with(|| pos.get(&b.id).cmp(&pos.get(&a.id)))
            });
        } else {
            let class = name.trim_end_matches(".bbclass");
            let only_global = files
                .iter()
                .all(|f| resolve::class_dir(ws, &f.path) == resolve::ClassDir::Global);
            files = class_files(index, ws, class, only_global)?
                .into_iter()
                .map(|c| c.file)
                .filter(|f| files.iter().any(|g| g.id == f.id))
                .collect();
        }
        out.push(ShadowRow { kind, name, files });
    }
    Ok(out)
}

// ---- versions ----

/// `bb.utils.vercmp_part`: BitBake's version comparison (no epoch or revision).
///
/// The string is split into numeric runs, alphabetic runs and single other characters;
/// `~` sorts below everything (also below the end of the string), the end of the string
/// below a number, numbers below letters, letters below other characters (`+`, `.`, ...).
/// So `1.9 < 1.10`, `1.0 < 1.0+git`, `1.0~rc1 < 1.0`, `1.0 < git`.
pub fn vercmp(a: &str, b: &str) -> Ordering {
    let mut va = explode_version(a).into_iter();
    let mut vb = explode_version(b).into_iter();
    loop {
        let pa = va.next();
        let pb = vb.next();
        let (oa, ca) = pa.map_or((0, None), |(o, c)| (o, Some(c)));
        let (ob, cb) = pb.map_or((0, None), |(o, c)| (o, Some(c)));
        if ca.is_none() && cb.is_none() && oa == 0 && ob == 0 {
            return Ordering::Equal;
        }
        match oa.cmp(&ob) {
            Ordering::Equal => {}
            other => return other,
        }
        match (ca, cb) {
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => {}
                other => return other,
            },
        }
    }
}

/// A part of an exploded version: numbers compare numerically, the rest as text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Num(String),
    Text(String),
}

impl Ord for Part {
    fn cmp(&self, other: &Part) -> Ordering {
        match (self, other) {
            (Part::Num(a), Part::Num(b)) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (Part::Text(a), Part::Text(b)) => a.cmp(b),
            // Never compared: parts of different types have different orders.
            (Part::Num(_), Part::Text(_)) => Ordering::Less,
            (Part::Text(_), Part::Num(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Part {
    fn partial_cmp(&self, other: &Part) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `bb.utils.explode_version`: `(order, part)` with order 0 for numbers, 1 for letters,
/// -1 for `~`, 2 for any other character.
fn explode_version(s: &str) -> Vec<(i32, Part)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let digits: String = chars[start..i].iter().collect();
            let trimmed = digits.trim_start_matches('0');
            out.push((0, Part::Num(trimmed.to_string())));
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            out.push((1, Part::Text(chars[start..i].iter().collect())));
        } else {
            out.push((if c == '~' { -1 } else { 2 }, Part::Text(c.to_string())));
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vercmp_like_bitbake() {
        use Ordering::*;
        assert_eq!(vercmp("1.10", "1.9"), Greater);
        assert_eq!(vercmp("1.9", "1.10"), Less);
        assert_eq!(vercmp("1.0", "1.0"), Equal);
        assert_eq!(vercmp("1.0+git", "1.0"), Greater);
        assert_eq!(vercmp("1.0+gitAUTOINC+abc", "1.0+gitAUTOINC+abd"), Less);
        assert_eq!(vercmp("1.1", "1.0+git"), Greater);
        assert_eq!(vercmp("git", "1.0"), Greater);
        assert_eq!(vercmp("1.0~rc1", "1.0"), Less);
        assert_eq!(vercmp("1.0", "1.0.1"), Less);
        assert_eq!(vercmp("1.0a", "1.0"), Greater);
        assert_eq!(vercmp("1.01", "1.1"), Equal);
        assert_eq!(vercmp("2.0", "10.0"), Less);
        assert_eq!(
            vercmp(
                "123456789012345678901234567890",
                "123456789012345678901234567891"
            ),
            Less
        );
    }

    #[test]
    fn preferred_version_match() {
        assert!(version_matches("1.0", "1.%"));
        assert!(version_matches("1.0", "1.0"));
        assert!(!version_matches("2.0", "1.%"));
        assert!(version_matches("anything", "%"));
        assert_eq!(strip_epoch("1:2.0"), "2.0");
        assert_eq!(strip_epoch("2.0"), "2.0");
    }
}
