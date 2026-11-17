//! Tool `slice`: read part of a stored output handle. See docs/mcp-tools.md.
//!
//! Exactly one selector per call (`lines`, `grep` (+ `context`), `var`, `failures`, `page`);
//! none means `page: 1`. Output is a `line  text` table through `render.rs`, capped at
//! [`CAP_BYTES`] (or `budget_tokens * 4` when that is smaller; the server-wide default budget
//! does not apply, a slice is meant to be read). When the selection does not fit, a note
//! says what was cut and gives the call that returns the next part.

use std::ops::Range;

use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::handles::{Handle, HandleKind, HandleMeta};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table, escape_cell};
use crate::types::LineSpan;

/// The cap on one slice, in bytes of rendered output.
pub const CAP_BYTES: usize = 8 * 1024;

/// Bytes kept back from the cap for the header row and the notes.
const RESERVE_BYTES: usize = 640;

/// A single line longer than this (escaped) is shortened in the slice.
const MAX_LINE_BYTES: usize = 2048;

/// Upper bound for `context`.
const MAX_CONTEXT: u32 = 50;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `h:<id>`.
    pub handle: String,
    /// Line range `A-B`.
    pub lines: Option<String>,
    /// Regular expression; matching lines with `context`.
    pub grep: Option<String>,
    /// Lines of context for `grep`.
    pub context: Option<u32>,
    /// Variable name (env dumps): its history block and final value.
    pub var: Option<String>,
    /// Error blocks (log and build handles).
    pub failures: Option<bool>,
    /// Page number, 1-based.
    pub page: Option<u32>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

/// The error blocks of a log, as 0-based, end-exclusive line ranges in order.
///
/// The blocks come from the failure detector in `crate::bitbake::logs`. Output it does not
/// recognise falls back to one block per line containing `ERROR:`, so a log with errors
/// never comes back empty.
pub fn failure_blocks(lines: &[&str]) -> Vec<Range<usize>> {
    let text = lines.join("\n");
    let mut blocks: Vec<Range<usize>> = crate::bitbake::logs::detect(&text)
        .iter()
        .map(|f| {
            let start = (f.span.start.max(1) as usize - 1).min(lines.len());
            let end = (f.span.end as usize).clamp(start, lines.len());
            start..end
        })
        .filter(|r| !r.is_empty())
        .collect();
    if blocks.is_empty() {
        return error_lines(lines);
    }
    blocks.sort_by_key(|r| (r.start, r.end));
    blocks
}

fn error_lines(lines: &[&str]) -> Vec<Range<usize>> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("ERROR:"))
        .map(|(i, _)| i..i + 1)
        .collect()
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let handle: Handle = args.handle.parse()?;
    let selector = Selector::from_args(&args)?;
    let meta = ctx.handles.meta(&handle)?;
    let bytes = ctx.handles.get(&handle)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let cap = args
        .budget_tokens
        .map_or(CAP_BYTES, |b| b.saturating_mul(4).min(CAP_BYTES));
    let table = slice(&meta, &lines, &selector, cap)?;
    // Already capped above; the table budget only has to keep every row.
    Ok(table.render_as(args.format.unwrap_or_default(), usize::MAX / 8))
}

/// What one call selects.
#[derive(Debug, Clone)]
enum Selector {
    Lines(LineSpan),
    Grep { re: Regex, context: u32 },
    Var(String),
    Failures,
    Page(u32),
}

impl Selector {
    fn from_args(a: &Args) -> Result<Selector> {
        let mut given = Vec::new();
        if a.lines.is_some() {
            given.push("lines");
        }
        if a.grep.is_some() {
            given.push("grep");
        }
        if a.var.is_some() {
            given.push("var");
        }
        if a.failures == Some(true) {
            given.push("failures");
        }
        if a.page.is_some() {
            given.push("page");
        }
        if given.len() > 1 {
            return Err(Error::Invalid {
                msg: format!(
                    "slice takes exactly one selector, got {}; make one call per selector",
                    given.join(", ")
                ),
            });
        }
        if a.context.is_some() && a.grep.is_none() {
            return Err(Error::Invalid {
                msg: "context applies only to grep; add grep=RE or drop context".into(),
            });
        }
        if let Some(l) = &a.lines {
            return Ok(Selector::Lines(l.parse()?));
        }
        if let Some(g) = &a.grep {
            let re = Regex::new(g).map_err(|e| Error::Invalid {
                msg: format!("bad grep pattern: {}", one_line(&e.to_string())),
            })?;
            return Ok(Selector::Grep {
                re,
                context: a.context.unwrap_or(0).min(MAX_CONTEXT),
            });
        }
        if let Some(v) = &a.var {
            let v = v.trim();
            if v.is_empty() {
                return Err(Error::Invalid {
                    msg: "var is empty; give a variable name such as SRC_URI".into(),
                });
            }
            return Ok(Selector::Var(v.to_string()));
        }
        if a.failures == Some(true) {
            return Ok(Selector::Failures);
        }
        match a.page {
            Some(0) => Err(Error::Invalid {
                msg: "page is 1-based; use page=1".into(),
            }),
            Some(p) => Ok(Selector::Page(p)),
            None => Ok(Selector::Page(1)),
        }
    }
}

/// A selected line (0-based index) or a `--` separator between non-adjacent groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Line(usize),
    Gap,
}

/// Builds the slice table for already-loaded output.
fn slice(meta: &HandleMeta, lines: &[&str], sel: &Selector, cap: usize) -> Result<Table> {
    let h = &meta.handle;
    let budget = cap.saturating_sub(RESERVE_BYTES).max(256);
    let total = lines.len();
    let mut notes = vec![format!(
        "{h} {} {total} lines {} bytes: {}",
        meta.kind,
        meta.bytes,
        render::snippet(&meta.command)
    )];

    let (rows, what): (Vec<Row>, String) = match sel {
        Selector::Page(p) => {
            let pages = paginate(lines, budget);
            let n = pages.len().max(1);
            let p = *p as usize;
            if p > n {
                return Err(Error::Invalid {
                    msg: format!("{h} has {n} page(s); ask for page 1-{n}"),
                });
            }
            let range = pages.get(p - 1).cloned().unwrap_or(0..0);
            let mut desc = format!("page {p}/{n}");
            if !range.is_empty() {
                desc.push_str(&format!(" (lines {}-{})", range.start + 1, range.end));
            }
            if p < n {
                desc.push_str(&format!("; next: slice handle={h} page={}", p + 1));
            }
            // A page is built to fit, so it is never cut below.
            (range.map(Row::Line).collect(), desc)
        }
        Selector::Lines(span) => {
            let start = span.start as usize;
            if start > total {
                return Err(Error::Invalid {
                    msg: format!("{h} has {total} lines; ask for lines within 1-{total}"),
                });
            }
            let end = (span.end as usize).min(total);
            (
                (start - 1..end).map(Row::Line).collect(),
                format!("lines {start}-{end}"),
            )
        }
        Selector::Grep { re, context } => {
            let hits: Vec<usize> = (0..total).filter(|&i| re.is_match(lines[i])).collect();
            let c = *context as usize;
            let blocks: Vec<Range<usize>> = hits
                .iter()
                .map(|&i| i.saturating_sub(c)..(i + c + 1).min(total))
                .collect();
            (
                grouped(&blocks),
                format!(
                    "grep {:?}: {} matching line(s), context {c}",
                    re.as_str(),
                    hits.len()
                ),
            )
        }
        Selector::Failures => {
            if !matches!(meta.kind, HandleKind::Log | HandleKind::Build) {
                return Err(Error::Invalid {
                    msg: format!(
                        "failures needs a log or build handle; {h} is {}; use grep=RE",
                        meta.kind
                    ),
                });
            }
            let blocks = failure_blocks(lines);
            (
                grouped(&blocks),
                format!("failures: {} error block(s)", blocks.len()),
            )
        }
        Selector::Var(name) => {
            if meta.kind != HandleKind::Env {
                return Err(Error::Invalid {
                    msg: format!(
                        "var needs an env handle (bitbake -e output); {h} is {}; use grep=RE",
                        meta.kind
                    ),
                });
            }
            let block = var_block(lines, name).ok_or_else(|| Error::NotFound {
                what: format!("variable {name} in {h}; names are case-sensitive, try grep={name}"),
            })?;
            (
                block.clone().map(Row::Line).collect(),
                format!("var {name}: lines {}-{}", block.start + 1, block.end),
            )
        }
    };

    let (kept, cut) = fit(lines, &rows, budget);
    let mut table = Table::new(&["line", "text"]);
    for row in &rows[..kept] {
        match *row {
            Row::Line(i) => {
                let n = (i + 1).to_string();
                table.row(&[&n, &shorten(lines[i])]);
            }
            Row::Gap => table.row(&["--", ""]),
        }
    }
    notes.push(what);
    if rows.is_empty() {
        notes.push(match sel {
            Selector::Failures => "no error blocks found (no line contains ERROR:)".into(),
            Selector::Grep { .. } => "no line matches".into(),
            _ => "nothing selected".into(),
        });
    }
    if cut.is_some() {
        notes.push(cut_note(h, sel, &rows[kept..], cap));
    }
    for n in &notes {
        table.note(n);
    }
    Ok(table)
}

/// The note for rows that did not fit: what was cut and the call for the next part.
fn cut_note(h: &Handle, sel: &Selector, rest: &[Row], cap: usize) -> String {
    let cut_lines: Vec<usize> = rest
        .iter()
        .filter_map(|r| match r {
            Row::Line(i) => Some(*i),
            Row::Gap => None,
        })
        .collect();
    let first = cut_lines.first().map_or(0, |i| i + 1);
    let last = cut_lines.last().map_or(0, |i| i + 1);
    let extra = match sel {
        Selector::Grep { .. } => ", or narrow the pattern",
        _ => "",
    };
    format!(
        "cut {} line(s) from line {first} over the {cap}-byte cap; next: slice handle={h} lines={first}-{last}{extra}",
        cut_lines.len()
    )
}

/// Merges overlapping or adjacent blocks and puts a gap row between the rest.
fn grouped(blocks: &[Range<usize>]) -> Vec<Row> {
    let mut merged: Vec<Range<usize>> = Vec::new();
    for b in blocks {
        match merged.last_mut() {
            Some(m) if b.start <= m.end => m.end = m.end.max(b.end),
            _ => merged.push(b.clone()),
        }
    }
    let mut rows = Vec::new();
    for (k, b) in merged.into_iter().enumerate() {
        if k > 0 {
            rows.push(Row::Gap);
        }
        rows.extend(b.map(Row::Line));
    }
    rows
}

/// Rendered size of one row.
fn row_bytes(lines: &[&str], row: Row) -> usize {
    match row {
        Row::Line(i) => digits(i + 1) + 1 + shorten(lines[i]).len() + 1,
        Row::Gap => 4,
    }
}

fn digits(n: usize) -> usize {
    n.to_string().len()
}

/// How many rows fit in `budget` bytes, and how many were cut (if any).
fn fit(lines: &[&str], rows: &[Row], budget: usize) -> (usize, Option<usize>) {
    let mut used = 0;
    for (k, row) in rows.iter().enumerate() {
        used += row_bytes(lines, *row);
        if used > budget {
            return (k, Some(rows.len() - k));
        }
    }
    (rows.len(), None)
}

/// Splits all lines into pages of whole lines that each fit in `budget` bytes.
fn paginate(lines: &[&str], budget: usize) -> Vec<Range<usize>> {
    let mut pages = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for i in 0..lines.len() {
        let b = row_bytes(lines, Row::Line(i));
        if used + b > budget && i > start {
            pages.push(start..i);
            start = i;
            used = 0;
        }
        used += b;
    }
    if start < lines.len() {
        pages.push(start..lines.len());
    }
    pages
}

/// The line as it appears in the slice: escaped length at most [`MAX_LINE_BYTES`].
fn shorten(line: &str) -> String {
    if escape_cell(line).len() <= MAX_LINE_BYTES {
        return line.to_string();
    }
    let mut kept = String::new();
    let mut size = 0;
    for c in line.chars() {
        let s = escape_cell(c.encode_utf8(&mut [0; 4])).len();
        if size + s > MAX_LINE_BYTES - 40 {
            break;
        }
        size += s;
        kept.push(c);
    }
    let cut = line.len() - kept.len();
    format!("{kept} ... [+{cut} bytes of this line]")
}

/// The `bitbake -e` block of variable or function `name`, as 0-based end-exclusive lines.
///
/// The blocks are found by the dump parser, [`crate::bitbake::envdump`] (the format is in
/// "Build output" in docs/bitbake-semantics.md): for a variable, from `# $NAME` through
/// its history, its pre-expansion value and its final assignment (`NAME="..."`,
/// `export NAME="..."` or `unset NAME`, none when the value is `None`); for a shell
/// function, from its `# line: N, file: F` comment to the closing `}`; for a python
/// function, from `python NAME () {` to the closing `}`. An override variant
/// (`T:task-clean`) is a block of its own.
fn var_block(lines: &[&str], name: &str) -> Option<Range<usize>> {
    let mut found = None;
    crate::bitbake::envdump::for_each_line_entry(lines.iter().copied(), |e| {
        if found.is_none() && e.name() == Some(name) {
            found = e.span();
        }
    });
    let span = found?;
    let start = (span.start.max(1) as usize - 1).min(lines.len());
    let end = (span.end as usize).clamp(start, lines.len());
    Some(start..end)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The layout of BitBake 2.18.0's `bitbake -e` (see docs/bitbake-semantics.md): one
    // operation has no count and no pre-expansion value; a value of `None` prints no
    // assignment; functions follow the variables, shell then python.
    const ENV: &str = "\
#
# $A [2 operations]
#   set /x/a.conf:1
#     \"one\"
#   set /x/doc.conf:2
#     [doc] \"about A\"
# pre-expansion value:
#   \"one\"
A=\"one\"
#
# $AB
#   set /x/a.conf:3
#     \"two\"
AB=\"two\"
#
# $FLAGONLY
#   set /x/doc.conf:4
#     [doc] \"no value\"
#
# $GONE [2 operations]
#   set /x/a.conf:5
#     \"x\"
#   set /x/a.conf:6
#     [unexport] \"1\"
# pre-expansion value:
#   \"x\"
unset GONE
#
# $LONG
#   set /x/a.conf:7
#     \"a
#     b\"
LONG=\"a \\
b\"
#
# $PATH
#   set /x/a.conf:9
#     \"/bin\"
export PATH=\"/bin\"
# line: 3, file: /x/base.bbclass
do_compile() {
\toe_runmake
}

# line: 9, file: /x/base.bbclass
do_install() {
\tif true; then
\t\t:
\tfi
}



python do_fetch () {
    pass
}


python do_unpack () {
    pass}
";

    fn lines() -> Vec<&'static str> {
        ENV.lines().collect()
    }

    #[test]
    fn var_block_takes_the_history_and_assignment() {
        let l = lines();
        assert_eq!(var_block(&l, "A"), Some(1..9));
        assert_eq!(var_block(&l, "AB"), Some(10..14));
        assert_eq!(var_block(&l, "FLAGONLY"), Some(15..18));
        assert_eq!(var_block(&l, "GONE"), Some(19..27));
        assert_eq!(var_block(&l, "LONG"), Some(28..34));
        assert_eq!(var_block(&l, "PATH"), Some(35..39));
        assert_eq!(var_block(&l, "do_compile"), Some(39..43));
        assert_eq!(var_block(&l, "do_install"), Some(44..50));
        assert_eq!(var_block(&l, "do_fetch"), Some(53..56));
        assert_eq!(var_block(&l, "do_unpack"), Some(58..60));
        assert_eq!(var_block(&l, "B"), None);
    }

    #[test]
    fn grouping_merges_overlaps() {
        let rows = grouped(&[0..2, 1..3, 5..6]);
        assert_eq!(
            rows,
            vec![
                Row::Line(0),
                Row::Line(1),
                Row::Line(2),
                Row::Gap,
                Row::Line(5)
            ]
        );
    }

    #[test]
    fn pages_cover_every_line_once() {
        let text: Vec<String> = (0..3000)
            .map(|i| format!("line {i} {}", "x".repeat(40)))
            .collect();
        let l: Vec<&str> = text.iter().map(String::as_str).collect();
        let pages = paginate(&l, 4000);
        assert!(pages.len() > 1);
        assert_eq!(pages[0].start, 0);
        assert_eq!(pages.last().map(|p| p.end), Some(3000));
        for w in pages.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn long_lines_are_shortened() {
        let s = "y".repeat(10_000);
        let out = shorten(&s);
        assert!(out.len() <= MAX_LINE_BYTES, "{}", out.len());
        assert!(out.ends_with("bytes of this line]"));
    }
}
