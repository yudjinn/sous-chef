//! All tool output goes through here. Contract: `docs/architecture.md` ("Rendering contract").
//!
//! Text output is tab-separated, header row first, one line per row, every line ending in
//! `\n`. Tabs, newlines, carriage returns and backslashes inside cells are escaped
//! (`\t`, `\n`, `\r`, `\\`) so a row is always exactly one line.
//!
//! The budget covers the whole rendered table: header, the rows kept, the truncation marker
//! line and any notes. Rows that do not fit are dropped from the end and replaced by one
//! marker line (`+N more over budget_tokens (budget_tokens=B shows all)`, `+N more
//! (limit=L)`, or both joined by `; `), where B is [`Table::budget_for_all`]: the budget at
//! which the same call returns every row.
//! The header, the marker and the notes are always emitted, so a budget too small to hold
//! even those is exceeded by exactly them and nothing else.
//!
//! The JSON form ([`Table::render_json`]) carries the same rows as the text form at the same
//! budget (the text form decides which rows fit), unescaped, plus the truncation counts and
//! notes as fields.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::index::RefreshReport;

/// Default `budget_tokens` for every tool.
pub const DEFAULT_BUDGET_TOKENS: usize = 600;

/// Maximum length of a snippet, in characters.
pub const SNIPPET_MAX_CHARS: usize = 120;

/// How many files `freshness_line` names before summarising the rest.
pub const FRESHNESS_MAX_FILES: usize = 5;

/// Output format argument shared by every tool and the `lint` CLI.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Tab-separated text, header row first.
    #[default]
    Text,
    /// The same data as JSON.
    Json,
}

/// A table: header plus rows of cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    /// Rows that exist beyond the ones added, cut by a query limit: `(more, limit)`.
    limit_cut: Option<(usize, usize)>,
    /// Lines printed after the table (and after the marker), always kept.
    notes: Vec<String>,
}

/// JSON shape of a rendered table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonTable {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Rows dropped because they did not fit in `budget_tokens`.
    pub more_over_budget: usize,
    /// When rows were dropped: the `budget_tokens` at which all of them fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_for_all: Option<usize>,
    /// Rows not returned because of a query limit.
    pub more_over_limit: usize,
    /// The limit that cut `more_over_limit` rows, if any.
    pub limit: Option<usize>,
    pub notes: Vec<String>,
}

/// Which rows fit, and the marker line that goes with them.
struct Layout {
    kept: usize,
    dropped: usize,
    marker: Option<String>,
}

impl Table {
    pub fn new(header: &[&str]) -> Table {
        Table {
            header: header.iter().map(|s| s.to_string()).collect(),
            ..Table::default()
        }
    }

    pub fn row(&mut self, cells: &[&str]) {
        self.rows
            .push(cells.iter().map(|s| s.to_string()).collect());
    }

    /// Records that a query limit `limit` cut `more` further rows. Rendered as
    /// `+N more (limit=L)`. `more == 0` clears it.
    pub fn limited(&mut self, more: usize, limit: usize) {
        self.limit_cut = (more > 0).then_some((more, limit));
    }

    /// Adds a line printed after the table, e.g. the python note. Notes are never dropped
    /// and count against the budget before any row does.
    pub fn note(&mut self, line: &str) {
        self.notes.push(escape_line(line));
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Renders in the requested format.
    pub fn render_as(&self, format: Format, budget_tokens: usize) -> String {
        match format {
            Format::Text => self.render(budget_tokens),
            Format::Json => self.render_json(budget_tokens),
        }
    }

    /// Renders within the token budget, replacing rows that do not fit with a marker line.
    pub fn render(&self, budget_tokens: usize) -> String {
        let header = line_of(&self.header);
        let rows: Vec<String> = self.rows.iter().map(|r| line_of(r)).collect();
        let layout = self.layout(&header, &rows, budget_tokens);
        let mut out = header;
        for r in &rows[..layout.kept] {
            out.push_str(r);
        }
        if let Some(m) = layout.marker {
            out.push_str(&m);
            out.push('\n');
        }
        for n in &self.notes {
            out.push_str(n);
            out.push('\n');
        }
        out
    }

    /// The same data as [`Table::render`] at the same budget, as a [`JsonTable`].
    pub fn to_json(&self, budget_tokens: usize) -> JsonTable {
        let header = line_of(&self.header);
        let rows: Vec<String> = self.rows.iter().map(|r| line_of(r)).collect();
        let layout = self.layout(&header, &rows, budget_tokens);
        JsonTable {
            header: self.header.clone(),
            rows: self.rows[..layout.kept].to_vec(),
            more_over_budget: layout.dropped,
            budget_for_all: (layout.dropped > 0).then(|| self.budget_for_all()),
            more_over_limit: self.limit_cut.map_or(0, |(m, _)| m),
            limit: self.limit_cut.map(|(_, l)| l),
            notes: self.notes.clone(),
        }
    }

    /// [`Table::to_json`] serialised.
    pub fn render_json(&self, budget_tokens: usize) -> String {
        // Serialising plain strings and integers cannot fail; fall back to an empty object
        // rather than panic.
        serde_json::to_string(&self.to_json(budget_tokens)).unwrap_or_else(|_| "{}".into())
    }

    /// Largest prefix of rows such that header + rows + marker + notes fit the budget.
    fn layout(&self, header: &str, rows: &[String], budget_tokens: usize) -> Layout {
        let budget_bytes = budget_tokens.saturating_mul(4);
        let fixed = header.len() + self.notes.iter().map(|n| n.len() + 1).sum::<usize>();
        let marker_for = |dropped: usize| self.marker(dropped);
        let size = |used: usize, dropped: usize| {
            fixed + used + marker_for(dropped).map_or(0, |m| m.len() + 1)
        };

        let all: usize = rows.iter().map(String::len).sum();
        if rows.is_empty() || size(all, 0) <= budget_bytes {
            return Layout {
                kept: rows.len(),
                dropped: 0,
                marker: marker_for(0),
            };
        }

        // Some row is dropped, so the budget marker is present for every candidate. Bytes
        // grow by at least one per row kept (each row ends in '\n') while the marker shrinks
        // by at most one byte per row (one fewer digit), so the size is monotonic in the
        // number of rows kept: stop at the first row that does not fit.
        let mut kept = 0;
        let mut used = 0;
        for r in &rows[..rows.len() - 1] {
            let dropped = rows.len() - kept - 1;
            if size(used + r.len(), dropped) > budget_bytes {
                break;
            }
            used += r.len();
            kept += 1;
        }
        let dropped = rows.len() - kept;
        Layout {
            kept,
            dropped,
            marker: marker_for(dropped),
        }
    }

    /// The `budget_tokens` at which every row fits: the whole table (header, rows, limit
    /// marker, notes) in tokens, rounded up to a multiple of 100. Shown in the budget marker
    /// so one more call gets the rest.
    pub fn budget_for_all(&self) -> usize {
        let header = line_of(&self.header).len();
        let rows: usize = self.rows.iter().map(|r| line_of(r).len()).sum();
        let notes: usize = self.notes.iter().map(|n| n.len() + 1).sum();
        let limit = self.limit_cut.map_or(0, |(more, limit)| {
            format!("+{more} more (limit={limit})\n").len()
        });
        (header + rows + notes + limit).div_ceil(4).div_ceil(100) * 100
    }

    fn marker(&self, dropped: usize) -> Option<String> {
        let budget = (dropped > 0).then(|| {
            format!(
                "+{dropped} more over budget_tokens (budget_tokens={} shows all)",
                self.budget_for_all()
            )
        });
        let limit = self
            .limit_cut
            .map(|(more, limit)| format!("+{more} more (limit={limit})"));
        match (budget, limit) {
            (Some(b), Some(l)) => Some(format!("{b}; {l}")),
            (b, l) => b.or(l),
        }
    }
}

/// Escapes one cell so it cannot break the tab-separated layout.
pub fn escape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Inverse of [`escape_cell`] (an unknown escape is kept as written).
pub fn unescape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A free-standing line (note) must also stay one line; tabs are fine there.
fn escape_line(s: &str) -> String {
    s.replace('\r', "\\r").replace('\n', "\\n")
}

fn line_of(cells: &[String]) -> String {
    let mut line = cells
        .iter()
        .map(|c| escape_cell(c))
        .collect::<Vec<_>>()
        .join("\t");
    line.push('\n');
    line
}

/// Bytes / 4, rounded up.
pub fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// One-line snippet: the first non-empty line, whitespace trimmed, at most
/// [`SNIPPET_MAX_CHARS`] characters (ending in `...` when cut).
pub fn snippet(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= SNIPPET_MAX_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(SNIPPET_MAX_CHARS - 3).collect();
    out.truncate(out.trim_end().len());
    out.push_str("...");
    out
}

/// Truncates `value` to `max_chars` characters. When cut, appends
/// ` ... [+N chars]`, or ` ... [+N chars; slice handle=h:...]` when the full value is
/// stored under `handle`.
pub fn truncate_value(value: &str, max_chars: usize, handle: Option<&str>) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_string();
    }
    let kept: String = value.chars().take(max_chars).collect();
    let cut = total - max_chars;
    match handle {
        Some(h) => format!("{kept} ... [+{cut} chars; slice handle={h}]"),
        None => format!("{kept} ... [+{cut} chars]"),
    }
}

/// `reindexed N files: a, b, ...` (plus `; removed M files`), or `None` when nothing
/// changed. Files are shown as `layer:path`; at most [`FRESHNESS_MAX_FILES`] are named.
pub fn freshness_line(r: &RefreshReport) -> Option<String> {
    let mut parts = Vec::new();
    if !r.reindexed.is_empty() {
        let n = r.reindexed.len();
        let names: Vec<String> = r
            .reindexed
            .iter()
            .take(FRESHNESS_MAX_FILES)
            .map(|l| format!("{}:{}", l.layer, l.path.display()))
            .collect();
        let mut s = format!("reindexed {n} {}: {}", plural(n, "file"), names.join(", "));
        if n > FRESHNESS_MAX_FILES {
            s.push_str(&format!(", +{} more", n - FRESHNESS_MAX_FILES));
        }
        parts.push(s);
    }
    if r.removed > 0 {
        let n = r.removed as usize;
        parts.push(format!("removed {n} {}", plural(n, "file")));
    }
    (!parts.is_empty()).then(|| escape_line(&parts.join("; ")))
}

/// The note for static results that depend on python, or `None` when `n == 0`.
pub fn python_note(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some(
            "note: 1 value depends on python and was not evaluated; use authoritative=true".into(),
        ),
        n => Some(format!(
            "note: {n} values depend on python and were not evaluated; use authoritative=true"
        )),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_round_trip() {
        let s = "a\tb\nc\r\\d\\t";
        assert_eq!(unescape_cell(&escape_cell(s)), s);
        assert_eq!(escape_cell(s), "a\\tb\\nc\\r\\\\d\\\\t");
    }
}
