//! Tool `item`: expand a pointer or fetch a named item, byte-exact. See docs/mcp-tools.md.
//!
//! Three ways to name the text: `loc` (`layer:path:A-B`), `recipe` + `name` (searched across
//! the recipe tree, bbappends included), or `file` + `name` (one file). A name matches a
//! statement whose name is the same, or the same followed by more overrides: `do_install`
//! matches `do_install` and `do_install:append`. `VAR[flag]` matches that flag only. When
//! several statements match, they are listed as pointers instead of guessing.
//!
//! Text output is the header line `layer:path:A-B sha256=<12 hex>` (the range actually
//! returned, the hash of the file as read) followed by the lines exactly as they are in the
//! file, line endings included, so they can be used as Edit strings. The output is capped at
//! [`CAP_BYTES`] (or `budget_tokens * 4` when given and smaller); lines that do not fit are
//! replaced by `+N more lines (lines=A-B)`. `format: "json"` returns the same as an object.

use std::fs;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::recipe::{self, file_label, file_ref};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, FileRow, StatementFilter, StatementRow};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::{LineSpan, Loc};
use crate::workspace::Workspace;

/// The cap on the returned text, in bytes, header and marker included.
pub const CAP_BYTES: usize = 8 * 1024;

/// Upper bound for `context`.
const MAX_CONTEXT: u32 = 200;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Pointer `layer:path:A-B`.
    pub loc: Option<String>,
    /// With `name`: search this recipe's tree.
    pub recipe: Option<String>,
    /// With `name`: search a single file (pointer-style `layer:path`).
    pub file: Option<String>,
    /// Item name, e.g. `do_install:append`.
    pub name: Option<String>,
    /// Extra lines around the item (default 0).
    #[serde(default)]
    pub context: u32,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

/// JSON form of the text output.
#[derive(Debug, Clone, Serialize)]
struct JsonItem {
    loc: String,
    sha256: String,
    text: String,
    more_lines: usize,
    /// The lines cut by the cap, `A-B`.
    rest: Option<String>,
    notes: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let format = args.format.unwrap_or_default();
    let cap = args
        .budget_tokens
        .map_or(CAP_BYTES, |b| b.saturating_mul(4).min(CAP_BYTES));
    let context = args.context.min(MAX_CONTEXT);
    // For the candidate list when a name matches several statements.
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    let name = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());

    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let mut notes: Vec<String> = render::freshness_line(&report).into_iter().collect();

    let loc: Loc = match (&args.loc, &args.recipe, &args.file, name) {
        (Some(l), None, None, None) => l.trim().parse()?,
        (None, Some(r), None, Some(n)) => {
            let rec = recipe::resolve_recipe(&index, r, None)?;
            let walk = recipe::walk_recipe(&index, &rec)?;
            let hits = matching(&walk.statements, n);
            let what = format!("item {n} in the tree of recipe {}_{}", rec.pn, rec.pv);
            match pick(hits, &what, n)? {
                Picked::One(l) => l,
                Picked::Many(t) => return Ok(many(t, notes, format, budget)),
            }
        }
        (None, None, Some(f), Some(n)) => {
            let file = file_arg(&index, f)?;
            let stmts = query::statements_in(&index, file.id, &StatementFilter::default())?;
            let hits = matching(&stmts, n);
            let what = format!("item {n} in {}", file_label(&file));
            match pick(hits, &what, n)? {
                Picked::One(l) => l,
                Picked::Many(t) => return Ok(many(t, notes, format, budget)),
            }
        }
        _ => {
            return Err(Error::Invalid {
                msg: "give exactly one of: loc, recipe + name, file + name".into(),
            });
        }
    };

    let bytes = read_confined(&ctx.workspace, &loc)?;
    let sha = hex12(&bytes);
    let lines = split_lines(&bytes);
    let total = lines.len() as u32;
    if loc.lines.start == 0 || loc.lines.start > total.max(1) || loc.lines.end < loc.lines.start {
        return Err(Error::Invalid {
            msg: format!(
                "{}:{} has {total} lines; ask for lines within 1-{total}",
                loc.layer,
                loc.path.display()
            ),
        });
    }
    let start = loc.lines.start.saturating_sub(context).max(1);
    let end = loc.lines.end.saturating_add(context).min(total);
    let cut = cut_lines(&lines, start, end, cap, &loc, &sha);
    if cut.kept_end < start {
        notes.push(format!(
            "line {start} alone is longer than the {cap}-byte cap; nothing returned"
        ));
    }
    let shown = Loc {
        lines: LineSpan::new(start, cut.kept_end.max(start)),
        ..loc.clone()
    };
    let text = String::from_utf8_lossy(&cut.body).into_owned();
    let rest = (cut.kept_end < end).then(|| LineSpan::new(cut.kept_end + 1, end).to_string());
    let more = (end - cut.kept_end) as usize;

    if format == Format::Json {
        let j = JsonItem {
            loc: shown.to_string(),
            sha256: sha,
            text,
            more_lines: if rest.is_some() { more } else { 0 },
            rest,
            notes,
        };
        return serde_json::to_string(&j).map_err(|e| Error::Invalid {
            msg: format!("serialising the item: {e}"),
        });
    }
    let mut out = format!("{shown} sha256={sha}\n");
    out.push_str(&text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if let Some(r) = rest {
        out.push_str(&format!("+{more} more lines (lines={r})\n"));
    }
    for n in notes {
        out.push_str(&n);
        out.push('\n');
    }
    Ok(out)
}

enum Picked {
    One(Loc),
    Many(Table),
}

/// Statements named `name`, or `name` followed by more overrides. `name[flag]` matches
/// that flag; a name without a flag does not match flag statements.
pub fn matching(stmts: &[StatementRow], name: &str) -> Vec<StatementRow> {
    let (base, flag) = match name.strip_suffix(']').and_then(|n| n.split_once('[')) {
        Some((b, f)) => (b, Some(f)),
        None => (name, None),
    };
    let prefix = format!("{base}:");
    stmts
        .iter()
        .filter(|s| s.kind != "unknown")
        .filter(|s| {
            s.name
                .as_deref()
                .is_some_and(|n| n == base || n.starts_with(&prefix))
        })
        .filter(|s| s.flag.as_deref() == flag)
        .cloned()
        .collect()
}

fn pick(hits: Vec<StatementRow>, what: &str, name: &str) -> Result<Picked> {
    match hits.len() {
        0 => Err(Error::NotFound {
            what: format!("{what}; names are as written, e.g. do_install:append or SRC_URI"),
        }),
        1 => Ok(Picked::One(hits[0].loc.clone())),
        n => {
            let mut t = Table::new(&["loc", "kind", "name", "snippet"]);
            for s in &hits {
                t.row(&[
                    &s.loc.to_string(),
                    &s.kind,
                    s.name.as_deref().unwrap_or("-"),
                    &s.snippet,
                ]);
            }
            t.note(&format!(
                "{n} statements match {name}; call item with loc=<one of the pointers>"
            ));
            Ok(Picked::Many(t))
        }
    }
}

fn many(mut t: Table, notes: Vec<String>, format: Format, budget: usize) -> String {
    for n in notes {
        t.note(&n);
    }
    t.render_as(format, budget)
}

/// The `file` argument: `layer:path` (a trailing `:A-B` is ignored).
fn file_arg(index: &Index, f: &str) -> Result<FileRow> {
    let (layer, path) = file_ref(f).ok_or_else(|| Error::Invalid {
        msg: format!("bad file {f:?}; expected layer:path"),
    })?;
    query::file_by_path(index, &layer, &path)?.ok_or_else(|| Error::NotFound {
        what: format!("file {layer}:{} in the index", path.display()),
    })
}

/// Reads the file a pointer names, through the confinement check.
pub fn read_confined(ws: &Workspace, loc: &Loc) -> Result<Vec<u8>> {
    let abs = ws.abs_path(loc).ok_or_else(|| Error::NotFound {
        what: format!(
            "layer {} (or the path {} is not relative to it)",
            loc.layer,
            loc.path.display()
        ),
    })?;
    let path = ws.confine(&abs)?;
    match fs::read(&path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound {
            what: format!("file {}:{}", loc.layer, loc.path.display()),
        }),
        Err(e) => Err(e.into()),
    }
}

/// The first 12 hex digits of the sha256 of `bytes`.
pub fn hex12(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Lines with their terminators (`\n`, so CRLF keeps its `\r`). A final line without a
/// newline is a line.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            out.push(&bytes[start..=i]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
}

struct Cut {
    body: Vec<u8>,
    /// Last line kept (`start - 1` when none fit).
    kept_end: u32,
}

/// Lines `start..=end` (1-based) that fit in `cap` bytes together with the header and the
/// marker line.
fn cut_lines(lines: &[&[u8]], start: u32, end: u32, cap: usize, loc: &Loc, sha: &str) -> Cut {
    let header = format!("{loc} sha256={sha}\n").len() + 16;
    let marker = format!("+{} more lines (lines={start}-{end})\n", end).len() + 8;
    let all: usize = (start..=end).map(|i| lines[(i - 1) as usize].len()).sum();
    let budget = if header + all <= cap {
        cap - header
    } else {
        cap.saturating_sub(header + marker)
    };
    let mut body = Vec::new();
    let mut kept_end = start - 1;
    for i in start..=end {
        let l = lines[(i - 1) as usize];
        if body.len() + l.len() > budget {
            break;
        }
        body.extend_from_slice(l);
        kept_end = i;
    }
    Cut { body, kept_end }
}
