//! Tool `search`: search statement names, file content (regex) or full text. See docs/mcp-tools.md.
//!
//! - `name` (default): statement names containing `query` (case-insensitive), exact first.
//! - `fts`: SQLite FTS5 over statement names and snippets.
//! - `regex`: file content line by line. Every file is read through
//!   `Workspace::confine`; each hit carries the statement that encloses the line.
//!
//! Each hit is one row: `loc`, `kind` (the statement kind; for a regex hit the enclosing
//! statement's kind, `-` outside any statement), `item` (`[name A-B]`, the enclosing item
//! and its range) and `snippet`. Filters: `kind`, `layer`, `file_kind`, `recipe` (that
//! recipe's tree: the `.bb`, its includes and classes, its bbappends). In regex mode `kind`
//! filters on the enclosing statement, so lines outside any statement are dropped. `limit`
//! (default 30) cuts the hits with a `+N more (limit=L)` marker; the budget cuts rows after.
//!
//! Data files (`files.not_collected` = `data`: a `.conf` / `.inc` in a recipe's files
//! directory that nothing includes, see `index/scan.rs`) are not searched in any mode. A hit
//! in a `.bb` / `.bbappend` that no `BBFILES` glob collects gets a note saying so.

use std::collections::HashMap;

use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;

use super::item::read_confined;
use super::recipe;
use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, StatementFilter, StatementRow};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::Workspace;

/// Search mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Statement names, substring (index); `NAME[flag]` matches flag statements.
    #[default]
    Name,
    /// File content line by line: a literal substring, no escaping needed.
    Text,
    /// File content line by line: a regular expression.
    Regex,
    /// Full-text search over names and snippets (index).
    Fts,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// A name (`SRCREV`, `PACKAGECONFIG[gnutls]`), literal text or a regex, as `mode` says.
    pub query: String,
    /// `name` (default): statement names; `text`: literal line text; `regex`; `fts`.
    #[serde(default)]
    pub mode: Mode,
    /// Statement kinds: `assign`, `function`, `task`, `inherit`, `include`, ...
    pub kind: Option<Vec<String>>,
    /// Only these layers (comma-separated names).
    pub layer: Option<String>,
    /// Only this file kind: `bb`, `bbappend`, `inc`, `bbclass`, `conf`.
    pub file_kind: Option<String>,
    /// Only this recipe's tree.
    pub recipe: Option<String>,
    /// Maximum hits (default 30).
    pub limit: Option<usize>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

/// Default `limit`.
pub const DEFAULT_LIMIT: usize = 30;

/// Upper bound for `limit`.
const MAX_LIMIT: usize = 1000;

/// Index hits fetched to count what `limit` cut (the marker is a lower bound beyond this).
const COUNT_CAP: usize = 5000;

/// Every statement kind the scanner writes into `statements.kind` (`index::scan`), the
/// values of the `kind` filter; the aliases `task` (`addtask`) and `def` (`pydef`) are
/// accepted too.
pub const KINDS: &[&str] = &[
    "assign",
    "function",
    "pydef",
    "inherit",
    "include",
    "addtask",
    "deltask",
    "addhandler",
    "export_functions",
    "export",
    "unset",
    "addfragments",
    "addpylib",
    "unknown",
];

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let query = args.query.trim();
    if query.is_empty() {
        return Err(Error::Invalid {
            msg: "query is empty; give a name fragment, a regex or a full-text query".into(),
        });
    }
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let filter = build_filter(&index, &ctx.workspace, &args)?;

    let mut t = Table::new(&["loc", "kind", "item", "snippet"]);
    match args.mode {
        Mode::Name | Mode::Fts => {
            let mut rows = match (args.mode, name_flag(query)) {
                (Mode::Name, Some((name, flag))) => {
                    query::search_name_flags(&index, name, flag, &filter, COUNT_CAP)?
                }
                (Mode::Name, None) => query::search_names(&index, query, &filter, COUNT_CAP)?,
                _ => query::search_fts(&index, query, &filter, COUNT_CAP)?,
            };
            let more = rows.len().saturating_sub(limit);
            rows.truncate(limit);
            let mut files: Vec<i64> = Vec::new();
            for s in &rows {
                t.row(&[&s.loc.to_string(), &s.kind, &item_label(s), &s.snippet]);
                if !files.contains(&s.file_id) {
                    files.push(s.file_id);
                }
            }
            t.limited(more, limit);
            collection_notes(&index, &files, &mut t)?;
        }
        Mode::Regex => regex_search(&index, ctx, query, &filter, limit, &mut t)?,
        Mode::Text => regex_search(&index, ctx, &regex::escape(query), &filter, limit, &mut t)?,
    }
    if t.is_empty() {
        t.note("no hits");
    }
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    let budget = args.budget_tokens.unwrap_or_else(|| {
        // The default budget is raised so that the default `limit` of hits fits (a hit row
        // is about 150 bytes); a budget set in sous-chef.toml is kept as it is.
        let configured = ctx.config.output.budget_tokens;
        if configured == render::DEFAULT_BUDGET_TOKENS {
            DEFAULT_BUDGET
        } else {
            configured
        }
    });
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

/// `search`'s own default `budget_tokens`: room for [`DEFAULT_LIMIT`] hits.
pub const DEFAULT_BUDGET: usize = 1200;

/// `NAME[flag]` split into its name and flag (both non-empty, no further brackets).
fn name_flag(query: &str) -> Option<(&str, &str)> {
    let (name, rest) = query.split_once('[')?;
    let flag = rest.strip_suffix(']')?;
    let plain = |s: &str| !s.is_empty() && !s.contains(['[', ']']);
    (plain(name) && plain(flag)).then_some((name, flag))
}

/// A note for each file of the hits that BitBake does not parse as indexed (a `.bb` no
/// `BBFILES` glob matches; see [`super::item::collection_note`]), in hit order.
fn collection_notes(index: &Index, files: &[i64], t: &mut Table) -> Result<()> {
    for id in files {
        if let Some(f) = query::file(index, *id)?
            && let Some(n) = super::item::collection_note(index, &f)?
        {
            t.note(&n);
        }
    }
    Ok(())
}

/// `[name A-B]`.
pub fn item_label(s: &StatementRow) -> String {
    let name = s.name.as_deref().unwrap_or(s.kind.as_str());
    match &s.flag {
        Some(f) => format!("[{name}[{f}] {}]", s.loc.lines),
        None => format!("[{name} {}]", s.loc.lines),
    }
}

fn build_filter(index: &Index, ws: &Workspace, args: &Args) -> Result<StatementFilter> {
    let mut f = StatementFilter::default();
    for k in args.kind.iter().flatten() {
        let k = match k.trim() {
            "task" => "addtask",
            "def" => "pydef",
            k => k,
        };
        if !KINDS.contains(&k) {
            return Err(Error::Invalid {
                msg: format!(
                    "unknown kind {k:?}; use task, def or one of {}",
                    KINDS.join(", ")
                ),
            });
        }
        f.kinds.push(k.to_string());
    }
    let mut layers: Vec<&str> = Vec::new();
    for l in args.layer.iter().flat_map(|l| l.split(',')).map(str::trim) {
        if !l.is_empty() && !layers.contains(&l) {
            layers.push(l);
        }
    }
    if !layers.is_empty() {
        let known = query::layer_stats(index)?;
        for l in &layers {
            if !known.iter().any(|s| s.name == *l) {
                let names: Vec<&str> = known.iter().map(|s| s.name.as_str()).collect();
                return Err(Error::NotFound {
                    what: format!("layer {l}; layers: {}", names.join(", ")),
                });
            }
        }
    }
    if let [one] = layers.as_slice() {
        f.layer = Some(one.to_string());
    }
    if let Some(k) = &args.file_kind {
        f.file_kind = Some(k.trim().parse::<FileKind>()?);
    }
    if let Some(r) = &args.recipe {
        // The file BitBake uses for a bare `pn`, as in `recipe` (provider selection).
        let (rec, _) = recipe::resolve_selected(index, ws, r, None)?;
        f.files = Some(recipe::walk_recipe(index, &rec)?.files);
    }
    if layers.len() > 1 {
        // Several layers: only their files, in layer then path order (or, with `recipe`,
        // that recipe's files in those layers, in tree order).
        let in_layers: Vec<i64> = query::all_files(index)?
            .into_iter()
            .filter(|file| layers.contains(&file.layer.as_str()))
            .map(|file| file.id)
            .collect();
        f.files = Some(match f.files.take() {
            Some(tree) => tree
                .into_iter()
                .filter(|id| in_layers.contains(id))
                .collect(),
            None => in_layers,
        });
    }
    Ok(f)
}

fn regex_search(
    index: &Index,
    ctx: &Ctx,
    pattern: &str,
    filter: &StatementFilter,
    limit: usize,
    t: &mut Table,
) -> Result<()> {
    let re = RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| Error::Invalid {
            msg: format!(
                "bad regex: {}",
                e.to_string().lines().last().unwrap_or("invalid pattern")
            ),
        })?;
    let mut files = query::all_files(index)?;
    files.retain(|f| {
        filter.layer.as_ref().is_none_or(|l| f.layer == *l)
            && filter.file_kind.is_none_or(|k| f.kind == k)
    });
    if let Some(order) = &filter.files {
        let rank: HashMap<i64, usize> = order.iter().enumerate().map(|(i, f)| (*f, i)).collect();
        files.retain(|f| rank.contains_key(&f.id));
        files.sort_by_key(|f| rank.get(&f.id).copied().unwrap_or(usize::MAX));
    }
    let mut hits = 0usize;
    let mut skipped = 0usize;
    let mut hit_files: Vec<i64> = Vec::new();
    for f in &files {
        let loc = f.loc(LineSpan::line(1));
        let bytes = match read_confined(&ctx.workspace, &loc) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut items: Option<Vec<StatementRow>> = None;
        for (i, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if !re.is_match(line) {
                continue;
            }
            let n = (i + 1) as u32;
            let stmts = match &items {
                Some(s) => s,
                None => items.insert(query::statements_in(
                    index,
                    f.id,
                    &StatementFilter::default(),
                )?),
            };
            let enclosing = enclosing(stmts, n);
            if !filter.kinds.is_empty()
                && !enclosing.is_some_and(|s| filter.kinds.contains(&s.kind))
            {
                continue;
            }
            hits += 1;
            if hits > limit {
                continue;
            }
            if !hit_files.contains(&f.id) {
                hit_files.push(f.id);
            }
            let at = Loc {
                lines: LineSpan::line(n),
                ..loc.clone()
            };
            let (kind, item) = match enclosing {
                Some(s) => (s.kind.clone(), item_label(s)),
                None => ("-".to_string(), "-".to_string()),
            };
            t.row(&[&at.to_string(), &kind, &item, &render::snippet(line)]);
        }
    }
    if hits > limit {
        t.limited(hits - limit, limit);
    }
    collection_notes(index, &hit_files, t)?;
    if skipped > 0 {
        t.note(&format!(
            "note: {skipped} indexed {} could not be read and {} skipped",
            if skipped == 1 { "file" } else { "files" },
            if skipped == 1 { "was" } else { "were" }
        ));
    }
    Ok(())
}

/// The innermost statement covering `line` (what `query::enclosing_item` returns, from the
/// file's statements already loaded).
fn enclosing(stmts: &[StatementRow], line: u32) -> Option<&StatementRow> {
    stmts
        .iter()
        .filter(|s| s.loc.lines.start <= line && s.loc.lines.end >= line)
        .max_by_key(|s| s.loc.lines.start)
}
