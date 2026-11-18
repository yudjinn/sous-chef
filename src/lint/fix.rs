//! Automatic fixes for lint findings (`sous-chef lint --fix`, the `lint` tool's `fix`).
//!
//! # Model
//!
//! A rule that can correct what it reports implements [`crate::lint::Rule::fixes`]: on the
//! same [`LintContext`] its `check` sees, it returns each finding exactly as `check` pushes
//! it (rule, pointer, message) together with a [`Fix`]. [`crate::lint::Finding`] itself is
//! unchanged, so the default lint output and every snapshot of it stay as they were.
//!
//! A [`Fix`] is one or more [`Edit`]s of one file (a byte range of the file's text as it was
//! read and its replacement), a one-line description and a [`Safety`] class:
//!
//! - [`Safety::Safe`]: the edit preserves what the metadata means, or is exactly what BitBake
//!   requires (the rule's own correction). `--fix` applies it.
//! - [`Safety::Suggested`]: the edit changes meaning in the way the rule recommends, or needs
//!   a human look. Shown, and applied only with `--fix-suggested`.
//!
//! Each fix carries the sha256 of the text its edits were computed on. The engine refuses
//! a fix when that is not the file's current content, or not the content the index holds.
//!
//! # Applying ([`run`])
//!
//! 1. The ordinary lint run ([`crate::lint::run`]) on the target, with the options given.
//! 2. For every file with findings of a rule that has fixes, the file is read again, parsed
//!    and handed to that rule's `fixes` in the context the engine uses; a fix counts only
//!    when its finding is one the run reported (after suppression, severity and line
//!    filters).
//! 3. Per file: fixes in a layer that is not writable (or in the build directory) are
//!    skipped and listed; suggested fixes are left out unless asked for; fixes whose edits
//!    overlap an edit already taken are deferred ("re-run after applying"). The rest
//!    become one [`crate::scaffold::PlannedFile`] with `Modify { before_sha }`, checked by
//!    [`crate::scaffold::check_plan`] (confinement, writable layer, unchanged since read).
//! 4. Dry run (default): a unified diff per file and a summary. With `write`:
//!    [`crate::scaffold::apply_plan`] (temporary file and rename, condition re-checked just
//!    before), an index refresh, and a lint of exactly the changed files. Fixes that become
//!    available in those files (deferred ones, a finding a fix revealed) are applied in
//!    further rounds, at most [`MAX_ROUNDS`], so a second `--fix --write` changes nothing.
//!    The report compares the findings of the changed files before and after: fixed,
//!    remaining, and new (a new finding is a fix's doing and is reported as such).
//!
//! Everything outside an edit's byte range is kept byte for byte: line endings, trailing
//! whitespace, comments.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::{Index, query};
use crate::lint::{self, Finding, LintContext, Options, Report, Rule, Scope, Target};
use crate::parse::{Statement, StatementKind};
use crate::release::{LintReach, Release};
use crate::scaffold::{self, FileAction, Plan, PlannedFile};
use crate::types::{FileKind, LineSpan, Loc, VarRef};
use crate::workspace::Workspace;

/// How far a fix can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    /// Meaning-preserving, or exactly what BitBake requires. Applied by `--fix`.
    Safe,
    /// Changes meaning the way the rule recommends, or needs a human look. Applied only with
    /// `--fix-suggested`.
    Suggested,
}

impl Safety {
    pub fn as_str(self) -> &'static str {
        match self {
            Safety::Safe => "safe",
            Safety::Suggested => "suggested",
        }
    }
}

/// One replacement in a file's text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Edit {
    /// Byte range of the text the fix was computed on (empty: an insertion).
    pub bytes: Range<usize>,
    pub replacement: String,
}

impl Edit {
    pub fn replace(bytes: Range<usize>, replacement: impl Into<String>) -> Edit {
        Edit {
            bytes,
            replacement: replacement.into(),
        }
    }

    pub fn insert(at: usize, text: impl Into<String>) -> Edit {
        Edit::replace(at..at, text)
    }

    pub fn delete(bytes: Range<usize>) -> Edit {
        Edit::replace(bytes, "")
    }
}

/// A fix of one finding: edits of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fix {
    /// The file the edits apply to.
    pub layer: String,
    pub path: PathBuf,
    /// sha256 (lower-case hex) of the text the edits were computed on.
    pub sha256: String,
    /// Non-overlapping, in any order.
    pub edits: Vec<Edit>,
    /// One line: what the fix does.
    pub description: String,
    pub safety: Safety,
}

impl Fix {
    /// A fix of the context's own file (`cx.source`); `None` without a source or edits.
    pub fn in_file(
        cx: &LintContext<'_>,
        edits: Vec<Edit>,
        description: impl Into<String>,
        safety: Safety,
    ) -> Option<Fix> {
        let source = cx.source?;
        Fix::on_text(
            &cx.loc.layer,
            &cx.loc.path,
            source,
            edits,
            description,
            safety,
        )
    }

    /// A fix of file `layer:path` whose text is `text`; `None` without edits or when an edit
    /// is out of range, not on a character boundary, or overlaps another.
    pub fn on_text(
        layer: &str,
        path: &Path,
        text: &str,
        mut edits: Vec<Edit>,
        description: impl Into<String>,
        safety: Safety,
    ) -> Option<Fix> {
        if edits.is_empty() {
            return None;
        }
        edits.sort_by_key(|e| (e.bytes.start, e.bytes.end));
        for (i, e) in edits.iter().enumerate() {
            if e.bytes.start > e.bytes.end
                || !text.is_char_boundary(e.bytes.start)
                || !text.is_char_boundary(e.bytes.end)
                || e.bytes.end > text.len()
            {
                return None;
            }
            if i > 0 && overlaps(&edits[i - 1].bytes, &e.bytes) {
                return None;
            }
        }
        Some(Fix {
            layer: layer.to_string(),
            path: path.to_path_buf(),
            sha256: scaffold::sha256_hex(text.as_bytes()),
            edits,
            description: description.into(),
            safety,
        })
    }

    /// `text` with the edits applied; `None` when `text` is not the text the fix was made
    /// for.
    pub fn apply_to(&self, text: &str) -> Option<String> {
        if scaffold::sha256_hex(text.as_bytes()) != self.sha256 {
            return None;
        }
        apply_edits(text, &self.edits.iter().collect::<Vec<_>>())
    }
}

/// A finding as its rule's `check` reports it, with its fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedFinding {
    pub finding: Finding,
    pub fix: Fix,
}

/// Whether two edit ranges touch the same text. Two insertions at one offset overlap (their
/// order would be a guess); an insertion at the boundary of a replacement does not.
pub fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    if a.is_empty() && b.is_empty() {
        return a.start == b.start;
    }
    if a.is_empty() {
        return a.start > b.start && a.start < b.end;
    }
    if b.is_empty() {
        return b.start > a.start && b.start < a.end;
    }
    a.start < b.end && b.start < a.end
}

/// `text` with non-overlapping `edits` applied; `None` when an edit is out of range or two
/// overlap.
pub fn apply_edits(text: &str, edits: &[&Edit]) -> Option<String> {
    let mut sorted: Vec<&Edit> = edits.to_vec();
    sorted.sort_by_key(|e| (e.bytes.start, e.bytes.end));
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for e in sorted {
        if e.bytes.start < at || e.bytes.end > text.len() || e.bytes.start > e.bytes.end {
            return None;
        }
        out.push_str(text.get(at..e.bytes.start)?);
        out.push_str(&e.replacement);
        at = e.bytes.end;
    }
    out.push_str(text.get(at..)?);
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Locating parts of statements

/// A physical line of a text: `start..content_end` is its content (terminator excluded),
/// `next` the offset of the following line (the terminator included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysLine {
    pub start: usize,
    pub content_end: usize,
    pub next: usize,
}

/// The physical lines of `text`, split as BitBake reads them (`\n`, `\r\n`, lone `\r`).
pub fn physical_lines(text: &str) -> Vec<PhysLine> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                out.push(PhysLine {
                    start,
                    content_end: i,
                    next: i + 1,
                });
                i += 1;
                start = i;
            }
            b'\r' => {
                let n = if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                out.push(PhysLine {
                    start,
                    content_end: i,
                    next: i + n,
                });
                i += n;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < b.len() {
        out.push(PhysLine {
            start,
            content_end: b.len(),
            next: b.len(),
        });
    }
    out
}

/// Line `n` (1-based) of `text`.
pub fn physical_line(text: &str, n: u32) -> Option<PhysLine> {
    physical_lines(text)
        .get((n as usize).checked_sub(1)?)
        .copied()
}

/// The line terminator `text` uses: that of its first line, `\n` when it has none.
pub fn line_ending(text: &str) -> &'static str {
    match physical_lines(text).first() {
        Some(l) => match &text.as_bytes()[l.content_end..l.next] {
            b"\r\n" => "\r\n",
            b"\r" => "\r",
            _ => "\n",
        },
        None => "\n",
    }
}

/// Whitespace as BitBake's regular expressions (`\s`) and `str.rstrip()` see it.
pub fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Byte offsets (absolute, in the file's text) of the parts of an assignment statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignParts {
    /// The variable name as written, without `export` and without the flag.
    pub name: Range<usize>,
    /// `[flag]`, when there is one.
    pub flag: Option<Range<usize>>,
    /// The operator.
    pub op: Range<usize>,
    /// The opening quote.
    pub open: usize,
    /// The closing quote (the last character of the statement).
    pub close: usize,
}

impl AssignParts {
    /// The text between the quotes (continuation lines included, as written).
    pub fn value(&self) -> Range<usize> {
        self.open + 1..self.close
    }
}

/// BitBake's assignment name characters (`[a-zA-Z0-9\-_+.${}/~:]`).
fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-_+.${}/~:".contains(c)
}

/// The operators in the order of BitBake's alternation (`:=|\?\?=|\?=|\+=|=\+|=\.|\.=|=`).
const OPS: [&str; 8] = [":=", "??=", "?=", "+=", "=+", "=.", ".=", "="];

/// The parts of assignment statement `stmt` in `source`, found as BitBake's
/// `__config_regexp__` matches them (the lazy name, the optional flag, the operator, the
/// quotes). `None` unless the statement is an assignment and the parts read back as exactly
/// what the parser recorded (name, flag, operator and value with continuation lines joined),
/// so an edit built from them cannot land elsewhere.
pub fn assign_parts(source: &str, stmt: &Statement) -> Option<AssignParts> {
    let StatementKind::Assign { var, op, value, .. } = &stmt.kind else {
        return None;
    };
    let base = stmt.bytes.start;
    let text = source.get(stmt.bytes.clone())?;
    let mut i = 0usize;
    if let Some(rest) = text.strip_prefix("export")
        && rest.starts_with(py_space)
    {
        let ws = rest.len() - rest.trim_start_matches(py_space).len();
        i = "export".len() + ws;
    }
    let after_export = &text[i..];
    let max_name = after_export
        .char_indices()
        .find(|(_, c)| !name_char(*c))
        .map_or(after_export.len(), |(k, _)| k);
    // The lazy name: the shortest one after which the rest matches.
    for n in 0..=max_name {
        if !after_export.is_char_boundary(n) {
            continue;
        }
        let mut j = n;
        let rest = &after_export[j..];
        let mut flag = None;
        if let Some(r) = rest.strip_prefix('[') {
            let close = r.find(']')?;
            flag = Some(i + j..i + j + close + 2);
            j += close + 2;
        }
        let rest = &after_export[j..];
        let ws = rest.len() - rest.trim_start_matches(py_space).len();
        j += ws;
        let rest = &after_export[j..];
        let Some(o) = OPS.iter().find(|o| rest.starts_with(**o)) else {
            continue;
        };
        let op_range = i + j..i + j + o.len();
        j += o.len();
        let rest = &after_export[j..];
        let ws = rest.len() - rest.trim_start_matches(py_space).len();
        j += ws;
        let open = i + j;
        let quote = text[open..].chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let trimmed = text.trim_end_matches(py_space);
        let close = trimmed.len().checked_sub(1)?;
        if close <= open || !trimmed.ends_with(quote) {
            return None;
        }
        let parts = AssignParts {
            name: base + i..base + i + n,
            flag: flag.map(|f| base + f.start..base + f.end),
            op: base + op_range.start..base + op_range.end,
            open: base + open,
            close: base + close,
        };
        // Read back: the parts must say what the parser recorded.
        let name = source.get(parts.name.clone())?;
        let flag_text = parts
            .flag
            .as_ref()
            .and_then(|f| source.get(f.start + 1..f.end - 1))
            .map(str::to_string);
        let parsed = crate::parse::split_var(name, flag_text);
        if parsed != *var || source.get(parts.op.clone())? != op.as_str() {
            return None;
        }
        if joined_value(source.get(parts.value())?) != *value {
            return None;
        }
        return Some(parts);
    }
    None
}

/// A raw value as BitBake reads it: each physical line but the last right-stripped and its
/// trailing `\` removed, then joined.
fn joined_value(raw: &str) -> String {
    let lines = physical_lines(raw);
    let mut out = String::with_capacity(raw.len());
    let ends_with_terminator = lines.last().is_some_and(|l| l.next > l.content_end);
    for (k, l) in lines.iter().enumerate() {
        let content = &raw[l.start..l.content_end];
        let last = k + 1 == lines.len() && !ends_with_terminator;
        if last {
            out.push_str(content);
        } else {
            let t = content.trim_end_matches(py_space);
            out.push_str(t.strip_suffix('\\').unwrap_or(t));
        }
    }
    out
}

/// The byte range (absolute) of a function statement's name in its header, after `python`
/// and `fakeroot`; `None` for anonymous functions or when it does not read back as the
/// parser's name.
pub fn function_name(source: &str, stmt: &Statement) -> Option<Range<usize>> {
    let StatementKind::Function {
        name: Some(var), ..
    } = &stmt.kind
    else {
        return None;
    };
    let text = source.get(stmt.bytes.clone())?;
    let mut rest = text;
    loop {
        if let Some(r) = rest.strip_prefix("python")
            && (r.starts_with(py_space) || r.starts_with('('))
        {
            rest = r.trim_start_matches(py_space);
            continue;
        }
        if let Some(r) = rest.strip_prefix("fakeroot")
            && r.starts_with(py_space)
        {
            rest = r.trim_start_matches(py_space);
            continue;
        }
        break;
    }
    let start = text.len() - rest.len();
    let len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || ".-+{}$:".contains(c)))
        .unwrap_or(rest.len());
    let range = stmt.bytes.start + start..stmt.bytes.start + start + len;
    let name = source.get(range.clone())?;
    (crate::parse::split_var(name, None) == *var).then_some(range)
}

/// The range of a variable name (assignment or function) as written in `stmt`.
pub fn statement_name(source: &str, stmt: &Statement) -> Option<Range<usize>> {
    match &stmt.kind {
        StatementKind::Assign { .. } => assign_parts(source, stmt).map(|p| p.name),
        StatementKind::Function { .. } => function_name(source, stmt),
        _ => None,
    }
}

/// The whole lines of `stmt` with their terminators: deleting this range removes the
/// statement and nothing else.
pub fn statement_lines(source: &str, stmt: &Statement) -> Option<Range<usize>> {
    let lines = physical_lines(source);
    let first = lines.get((stmt.span.start as usize).checked_sub(1)?)?;
    let last = lines.get((stmt.span.end as usize).checked_sub(1)?)?;
    (first.start == stmt.bytes.start).then_some(first.start..last.next)
}

/// An insertion of `line` (no terminator) as a line of its own right after statement
/// `stmt`, with the file's line terminator ([`line_ending`]); after a last line without a
/// terminator, the terminator goes before the new line instead.
pub fn insert_line_after(source: &str, stmt: &Statement, line: &str) -> Option<Edit> {
    let range = statement_lines(source, stmt)?;
    let eol = line_ending(source);
    let last = physical_line(source, stmt.span.end)?;
    Some(if last.next > last.content_end {
        Edit::insert(range.end, format!("{line}{eol}"))
    } else {
        Edit::insert(range.end, format!("{eol}{line}"))
    })
}

/// The name of `var` in the current syntax with its flag: `RDEPENDS:${PN}[doc]`.
pub fn written(var: &VarRef) -> String {
    let mut s = crate::lint::helpers::colon_name(var);
    if let Some(f) = &var.flag {
        s.push('[');
        s.push_str(f);
        s.push(']');
    }
    s
}

// ---------------------------------------------------------------------------------------
// Engine

/// Most rounds of a `--fix --write` run (see the module documentation).
pub const MAX_ROUNDS: usize = 5;

/// How to fix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FixOptions {
    /// Also apply [`Safety::Suggested`] fixes.
    pub suggested: bool,
    /// Write the files (default: dry run).
    pub write: bool,
}

/// One file a round changes (or, in a dry run, would change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    /// `layer:path`.
    pub file: String,
    /// Unified diff of the file before and after the round's fixes.
    pub diff: String,
    /// The fixes applied, by position in the file.
    pub fixes: Vec<FixedFinding>,
    /// The text before the round and the edits applied to it (to follow lines across
    /// rounds).
    #[serde(skip)]
    pub before: String,
    #[serde(skip)]
    pub edits: Vec<Edit>,
}

/// What one round did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Round {
    pub changes: Vec<FileChange>,
    /// Fixes whose edits overlap an edit already taken in the same file: applied in a later
    /// round, or (dry run) after re-running.
    pub deferred: Vec<FixedFinding>,
    /// Files whose fixes were refused, one line each (changed since read or indexed,
    /// refused by the scaffold checks).
    pub refused: Vec<String>,
}

/// The findings of the changed files after the last round, against those before the first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct After {
    /// `layer:path` of every file written.
    pub files: Vec<String>,
    /// Findings of the first run in those files that are gone.
    pub fixed: Vec<Finding>,
    /// Findings of the first run in those files that are still there.
    pub remaining: Vec<Finding>,
    /// Findings in those files that the first run did not have, on a line no
    /// `override/legacy-syntax` fix renamed: a fix introduced them, which is a bug in that
    /// fix.
    pub new: Vec<Finding>,
    /// Findings the first run did not have, on a line whose legacy name a fix converted:
    /// the other rules skip legacy names, so the old spelling hid them (a `_remove` that
    /// is now `override/remove-in-recipe`'s `:remove`). Not introduced by a fix.
    pub revealed: Vec<Finding>,
    /// Problems after writing (index refresh or lint failed), one line each.
    pub problems: Vec<String>,
}

/// A fix shown but not applied, with the diff it alone would make (empty when the file no
/// longer has the text the fix was made for).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShownFix {
    pub fixed: FixedFinding,
    pub diff: String,
}

/// What [`run`] returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FixReport {
    pub write: bool,
    pub suggested: bool,
    /// The lint run the fixes are for.
    pub report: Report,
    /// Every fix found for a reported finding (first round), whatever is done with it.
    pub available: Vec<FixedFinding>,
    pub rounds: Vec<Round>,
    /// Suggested fixes not applied (no `--fix-suggested`), each with the diff it alone
    /// would make.
    pub suggested_skipped: Vec<ShownFix>,
    /// Fixes of files in layers that are not writable (or in the build directory): never
    /// applied.
    pub not_writable: Vec<FixedFinding>,
    /// After writing.
    pub after: Option<After>,
}

impl FixReport {
    /// Fixes applied (or, in a dry run, that would be) over all rounds.
    pub fn applied(&self) -> impl Iterator<Item = &FixedFinding> {
        self.rounds
            .iter()
            .flat_map(|r| r.changes.iter().flat_map(|c| c.fixes.iter()))
    }

    /// The findings that remain after the run: the first run's, with the changed files'
    /// replaced by their findings after the last round (a dry run changes nothing).
    pub fn final_findings(&self) -> Vec<Finding> {
        let Some(after) = &self.after else {
            return self.report.findings.clone();
        };
        let changed: HashSet<&str> = after.files.iter().map(String::as_str).collect();
        let mut out: Vec<Finding> = self
            .report
            .findings
            .iter()
            .filter(|f| !changed.contains(file_key(&f.loc).as_str()))
            .cloned()
            .collect();
        out.extend(after.remaining.iter().cloned());
        out.extend(after.new.iter().cloned());
        out.extend(after.revealed.iter().cloned());
        lint::sort_findings(&mut out);
        out
    }
}

fn file_key(loc: &Loc) -> String {
    format!("{}:{}", loc.layer, loc.path.display())
}

/// Lints `target`, then fixes what it can: see the module documentation. `index` must be
/// refreshed; with `write` it is refreshed again after each round.
pub fn run(
    ws: &Workspace,
    index: &mut Index,
    config: &Config,
    target: &Target,
    options: &Options,
    fix: &FixOptions,
) -> Result<FixReport> {
    run_with(
        ws,
        index,
        config,
        target,
        options,
        fix,
        lint::all_rules(),
        &lint::Replay,
    )
}

/// [`run`] with the rule set and the evaluator given (tests).
#[allow(clippy::too_many_arguments)]
pub fn run_with(
    ws: &Workspace,
    index: &mut Index,
    config: &Config,
    target: &Target,
    options: &Options,
    fix: &FixOptions,
    rules: &[&dyn Rule],
    eval: &dyn lint::Evaluate,
) -> Result<FixReport> {
    let report = lint::run_with(ws, index, config, target, options, rules, eval)?;
    let reach = match target {
        Target::Workspace => LintReach::Sweep,
        _ => LintReach::Target,
    };
    let available = collect(ws, index, &report, rules, reach);
    let mut out = FixReport {
        write: fix.write,
        suggested: fix.suggested,
        report,
        available: available.clone(),
        ..FixReport::default()
    };
    let mut todo = available;
    let mut written: Vec<PathBuf> = Vec::new();
    let mut written_keys: Vec<String> = Vec::new();
    let mut last: Option<Report> = None;
    let mut problems: Vec<String> = Vec::new();
    // Per file (`layer:path`), the lines (current numbering) whose legacy name was renamed.
    let mut legacy_lines: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for round_no in 1..=MAX_ROUNDS {
        let (round, plan, skipped, not_writable) = plan_round(ws, index, todo, fix);
        if round_no == 1 {
            out.suggested_skipped = skipped
                .into_iter()
                .map(|f| {
                    let diff = read_text(ws, &f.fix.layer, &f.fix.path)
                        .and_then(|text| f.fix.apply_to(&text).map(|new| (text, new)))
                        .map(|(text, new)| {
                            let name = format!("{}:{}", f.fix.layer, f.fix.path.display());
                            unified_diff(&name, &text, &new)
                        })
                        .unwrap_or_default();
                    ShownFix { fixed: f, diff }
                })
                .collect();
            out.not_writable = not_writable;
        }
        let empty = plan.files.is_empty();
        if !empty || round_no == 1 {
            out.rounds.push(round);
        }
        if !fix.write || empty {
            break;
        }
        let checked = scaffold::check_plan(ws, &plan)?;
        let files = scaffold::apply_plan(&plan, &checked)?;
        // Follow the converted legacy lines into the new text.
        if let Some(round) = out.rounds.last() {
            for c in &round.changes {
                let lines = legacy_lines.entry(c.file.clone()).or_default();
                for l in lines.iter_mut() {
                    *l = map_line(&c.before, &c.edits, *l);
                }
                for f in c
                    .fixes
                    .iter()
                    .filter(|f| f.finding.rule == "override/legacy-syntax")
                {
                    lines.push(map_line(&c.before, &c.edits, f.finding.loc.lines.start));
                }
            }
        }
        for (f, c) in files.iter().zip(&checked) {
            if !written.contains(f) {
                written.push(f.clone());
                written_keys.push(c.display.clone());
            }
        }
        if let Err(e) = index.refresh(ws) {
            problems.push(format!("index refresh after writing failed: {e}"));
            break;
        }
        let relint_target = Target::Files {
            files: written.clone(),
            lines: None,
        };
        let relint_options = Options {
            all: false,
            ..options.clone()
        };
        match lint::run_with(
            ws,
            index,
            config,
            &relint_target,
            &relint_options,
            rules,
            eval,
        ) {
            Ok(r) => {
                todo = collect(ws, index, &r, rules, LintReach::Target);
                last = Some(r);
            }
            Err(e) => {
                problems.push(format!("lint after writing failed: {e}"));
                last = None;
                break;
            }
        }
    }
    if fix.write && !written.is_empty() {
        out.after = Some(compare(
            &out.report,
            last.as_ref(),
            written_keys,
            problems,
            &legacy_lines,
        ));
    }
    Ok(out)
}

/// The fixes for the findings of `report`, from the rules that reported them.
fn collect(
    ws: &Workspace,
    index: &Index,
    report: &Report,
    rules: &[&dyn Rule],
    reach: LintReach,
) -> Vec<FixedFinding> {
    let by_id: HashMap<&str, &dyn Rule> = rules.iter().map(|r| (r.id(), *r)).collect();
    // Findings per file (file scope) and per layer (layer scope), by rule.
    let mut files: BTreeMap<(String, PathBuf), Vec<&str>> = BTreeMap::new();
    let mut layers: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for f in &report.findings {
        let Some(rule) = by_id.get(f.rule) else {
            continue;
        };
        match rule.scope() {
            Scope::File => {
                let ids = files
                    .entry((f.loc.layer.clone(), f.loc.path.clone()))
                    .or_default();
                if !ids.contains(&f.rule) {
                    ids.push(f.rule);
                }
            }
            Scope::Layer => {
                let ids = layers.entry(f.loc.layer.clone()).or_default();
                if !ids.contains(&f.rule) {
                    ids.push(f.rule);
                }
            }
            Scope::Recipe => {}
        }
    }
    let mut candidates: Vec<FixedFinding> = Vec::new();
    for ((layer, path), ids) in &files {
        let Some(src) = read_text(ws, layer, path) else {
            continue;
        };
        let parsed = crate::parse::parse(&src);
        let releases = releases_of(ws, layer, reach);
        let lines = u32::try_from(src.lines().count().max(1)).unwrap_or(u32::MAX);
        let cx = LintContext {
            file: Some(&parsed),
            source: Some(&src),
            kind: FileKind::from_path(path),
            data: None,
            layer: None,
            index,
            ws,
            loc: Loc {
                layer: layer.clone(),
                path: path.clone(),
                lines: LineSpan::new(1, lines),
            },
            releases: &releases,
        };
        for id in ids {
            if let Some(rule) = by_id.get(id) {
                candidates.extend(rule.fixes(&cx));
            }
        }
    }
    for (layer, ids) in &layers {
        let releases = releases_of(ws, layer, reach);
        let cx = LintContext {
            file: None,
            source: None,
            kind: None,
            data: None,
            layer: Some(layer),
            index,
            ws,
            loc: Loc {
                layer: layer.clone(),
                path: PathBuf::from("conf/layer.conf"),
                lines: LineSpan::line(1),
            },
            releases: &releases,
        };
        for id in ids {
            if let Some(rule) = by_id.get(id) {
                candidates.extend(rule.fixes(&cx));
            }
        }
    }
    // Keep a fix only for a finding the run reported (each finding once), and show the
    // finding as reported (its severity may be configured).
    let mut used: HashSet<usize> = HashSet::new();
    let mut out = Vec::new();
    for c in candidates {
        let hit = report.findings.iter().enumerate().find(|(k, f)| {
            !used.contains(k)
                && f.rule == c.finding.rule
                && f.loc == c.finding.loc
                && f.message == c.finding.message
        });
        if let Some((k, f)) = hit {
            used.insert(k);
            out.push(FixedFinding {
                finding: f.clone(),
                fix: c.fix,
            });
        }
    }
    out.sort_by(|a, b| {
        (
            &a.fix.layer,
            &a.fix.path,
            first_offset(&a.fix),
            a.finding.rule,
        )
            .cmp(&(
                &b.fix.layer,
                &b.fix.path,
                first_offset(&b.fix),
                b.finding.rule,
            ))
    });
    out
}

fn first_offset(f: &Fix) -> usize {
    f.edits.iter().map(|e| e.bytes.start).min().unwrap_or(0)
}

fn releases_of(ws: &Workspace, layer: &str, reach: LintReach) -> Vec<Release> {
    match ws.layer(layer) {
        Some(l) => l.lint_releases_as(ws, reach),
        None => vec![ws.release.release],
    }
}

/// A file's text through [`Workspace::confine`]; `None` when it cannot be read or is not
/// valid UTF-8 (byte offsets would not be the file's).
fn read_text(ws: &Workspace, layer: &str, path: &Path) -> Option<String> {
    let abs = abs_of(ws, layer, path).ok()?;
    let bytes = std::fs::read(abs).ok()?;
    String::from_utf8(bytes).ok()
}

fn abs_of(ws: &Workspace, layer: &str, path: &Path) -> Result<PathBuf> {
    let loc = Loc {
        layer: layer.to_string(),
        path: path.to_path_buf(),
        lines: LineSpan::line(1),
    };
    let abs = ws.abs_path(&loc).ok_or_else(|| Error::NotFound {
        what: format!("file {layer}:{}", path.display()),
    })?;
    ws.confine(&abs)
}

/// Splits the fixes of one round: per file, the edits to make (a scaffold [`Plan`] of
/// `Modify` files), the deferred and refused ones, the suggested ones left out and those
/// in layers that are not writable.
fn plan_round(
    ws: &Workspace,
    index: &Index,
    fixes: Vec<FixedFinding>,
    opts: &FixOptions,
) -> (Round, Plan, Vec<FixedFinding>, Vec<FixedFinding>) {
    let mut round = Round::default();
    let mut plan = Plan::default();
    let mut skipped = Vec::new();
    let mut not_writable = Vec::new();
    let mut per_file: BTreeMap<(String, PathBuf), Vec<FixedFinding>> = BTreeMap::new();
    for f in fixes {
        if !ws.layer(&f.fix.layer).is_some_and(|l| l.writable) {
            not_writable.push(f);
        } else if f.fix.safety == Safety::Suggested && !opts.suggested {
            skipped.push(f);
        } else {
            per_file
                .entry((f.fix.layer.clone(), f.fix.path.clone()))
                .or_default()
                .push(f);
        }
    }
    for ((layer, path), list) in per_file {
        let display = format!("{layer}:{}", path.display());
        let refuse = |why: String| format!("{display}: {why}; nothing changed in it");
        let abs = match abs_of(ws, &layer, &path) {
            Ok(a) => a,
            Err(e) => {
                round.refused.push(refuse(e.to_string()));
                continue;
            }
        };
        let Some(text) = read_text(ws, &layer, &path) else {
            round
                .refused
                .push(refuse("cannot be read as UTF-8 text".to_string()));
            continue;
        };
        let sha = scaffold::sha256_hex(text.as_bytes());
        let indexed = query::file_by_path(index, &layer, &path)
            .ok()
            .flatten()
            .map(|r| r.sha256);
        if indexed.as_deref() != Some(sha.as_str()) {
            round.refused.push(refuse(
                "changed since it was indexed; re-run lint --fix".to_string(),
            ));
            continue;
        }
        let mut taken: Vec<&Edit> = Vec::new();
        let mut applied: Vec<FixedFinding> = Vec::new();
        let mut deferred: Vec<FixedFinding> = Vec::new();
        let mut stale = false;
        for f in &list {
            if f.fix.sha256 != sha {
                stale = true;
                continue;
            }
            let clash = f
                .fix
                .edits
                .iter()
                .any(|e| taken.iter().any(|t| overlaps(&t.bytes, &e.bytes)));
            if clash {
                deferred.push(f.clone());
            } else {
                taken.extend(f.fix.edits.iter());
                applied.push(f.clone());
            }
        }
        if stale {
            round.refused.push(format!(
                "{display}: changed since its findings were produced; those fixes were left out"
            ));
        }
        round.deferred.extend(deferred);
        if applied.is_empty() {
            continue;
        }
        let Some(new) = apply_edits(&text, &taken) else {
            round.refused.push(refuse(
                "its edits do not apply (a bug in a fix)".to_string(),
            ));
            continue;
        };
        if new == text {
            continue;
        }
        let one = Plan {
            files: vec![PlannedFile {
                path: abs.clone(),
                content: new.clone(),
                action: FileAction::Modify {
                    before_sha: sha.clone(),
                },
            }],
            ..Plan::default()
        };
        if let Err(e) = scaffold::check_plan(ws, &one) {
            round.refused.push(refuse(e.to_string()));
            continue;
        }
        round.changes.push(FileChange {
            diff: unified_diff(&display, &text, &new),
            file: display,
            fixes: applied,
            edits: taken.iter().map(|e| (*e).clone()).collect(),
            before: text,
        });
        plan.files.extend(one.files);
    }
    (round, plan, skipped, not_writable)
}

/// A finding as compared across a fix: rule, file and message, with the line numbers a
/// message quotes (`line 5`) taken out, since edits above them move them.
fn compare_key(f: &Finding) -> (String, String, String) {
    let mut msg = String::with_capacity(f.message.len());
    let mut rest = f.message.as_str();
    while let Some(at) = rest.find("line ") {
        msg.push_str(&rest[..at + 5]);
        rest = &rest[at + 5..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            msg.push('#');
            rest = &rest[digits..];
        }
    }
    msg.push_str(rest);
    (f.rule.to_string(), file_key(&f.loc), msg)
}

/// Line `line` of `old` after `edits` (applied to `old`): moved by the line terminators the
/// edits before it add or remove.
fn map_line(old: &str, edits: &[Edit], line: u32) -> u32 {
    let terminators = |s: &str| {
        physical_lines(s)
            .iter()
            .filter(|l| l.next > l.content_end)
            .count() as i64
    };
    let Some(start) = physical_line(old, line).map(|l| l.start) else {
        return line;
    };
    let delta: i64 = edits
        .iter()
        .filter(|e| e.bytes.end <= start)
        .map(|e| terminators(&e.replacement) - terminators(old.get(e.bytes.clone()).unwrap_or("")))
        .sum();
    u32::try_from((i64::from(line) + delta).max(1)).unwrap_or(line)
}

fn compare(
    before: &Report,
    after: Option<&Report>,
    files: Vec<String>,
    mut problems: Vec<String>,
    legacy_lines: &BTreeMap<String, Vec<u32>>,
) -> After {
    let set: HashSet<&str> = files.iter().map(String::as_str).collect();
    let old: Vec<&Finding> = before
        .findings
        .iter()
        .filter(|f| set.contains(file_key(&f.loc).as_str()))
        .collect();
    let Some(after) = after else {
        problems.push("the changed files could not be linted again".to_string());
        return After {
            files,
            problems,
            ..After::default()
        };
    };
    let mut pool: HashMap<(String, String, String), usize> = HashMap::new();
    for f in &old {
        *pool.entry(compare_key(f)).or_default() += 1;
    }
    let mut remaining = Vec::new();
    let mut new = Vec::new();
    let mut revealed = Vec::new();
    for f in &after.findings {
        match pool.get_mut(&compare_key(f)) {
            Some(n) if *n > 0 => {
                *n -= 1;
                remaining.push(f.clone());
            }
            // The legacy rule itself on a converted line is the fix's failure, not news.
            _ if f.rule != "override/legacy-syntax"
                && legacy_lines
                    .get(&file_key(&f.loc))
                    .is_some_and(|l| l.contains(&f.loc.lines.start)) =>
            {
                revealed.push(f.clone());
            }
            _ => new.push(f.clone()),
        }
    }
    let mut pool2: HashMap<(String, String, String), usize> = HashMap::new();
    for f in &after.findings {
        *pool2.entry(compare_key(f)).or_default() += 1;
    }
    let mut fixed = Vec::new();
    for f in old {
        match pool2.get_mut(&compare_key(f)) {
            Some(n) if *n > 0 => *n -= 1,
            _ => fixed.push(f.clone()),
        }
    }
    After {
        files,
        fixed,
        remaining,
        new,
        revealed,
        problems,
    }
}

// ---------------------------------------------------------------------------------------
// In memory (rule fixture tests)

/// The fixes one file-scope rule offers for `text`, as the engine would get them for a file
/// at `loc` checked against `releases` (empty: the workspace release); the counterpart of
/// [`lint::check_text`]. Nothing is read or written.
#[allow(clippy::too_many_arguments)]
pub fn fixes_in_text(
    rule: &dyn Rule,
    ws: &Workspace,
    index: &Index,
    loc: Loc,
    text: &str,
    kind: Option<FileKind>,
    releases: &[Release],
) -> Vec<FixedFinding> {
    let default = [ws.release.release];
    let releases = if releases.is_empty() {
        &default[..]
    } else {
        releases
    };
    if !lint::applies(rule, releases) {
        return Vec::new();
    }
    let parsed = crate::parse::parse(text);
    let lines = u32::try_from(text.lines().count().max(1)).unwrap_or(u32::MAX);
    let cx = LintContext {
        file: Some(&parsed),
        source: Some(text),
        kind,
        data: None,
        layer: None,
        index,
        ws,
        loc: Loc {
            lines: LineSpan::new(1, lines),
            ..loc
        },
        releases,
    };
    // Only fixes of findings `check` reports.
    let reported = {
        let mut v = Vec::new();
        rule.check(&cx, &mut v);
        v
    };
    rule.fixes(&cx)
        .into_iter()
        .filter(|f| reported.contains(&f.finding))
        .collect()
}

/// `text` after applying, in rounds as `--fix --write` does (at most [`MAX_ROUNDS`]), the
/// fixes of `rules` (file scope) for a file at `loc`: safe ones, and suggested ones too
/// when `suggested`. Overlapping fixes wait for the next round.
#[allow(clippy::too_many_arguments)]
pub fn fix_text(
    rules: &[&dyn Rule],
    ws: &Workspace,
    index: &Index,
    loc: &Loc,
    text: &str,
    kind: Option<FileKind>,
    releases: &[Release],
    suggested: bool,
) -> String {
    let mut text = text.to_string();
    for _ in 0..MAX_ROUNDS {
        let mut all: Vec<FixedFinding> = rules
            .iter()
            .filter(|r| r.scope() == Scope::File)
            .flat_map(|r| fixes_in_text(*r, ws, index, loc.clone(), &text, kind, releases))
            .filter(|f| suggested || f.fix.safety == Safety::Safe)
            .collect();
        all.sort_by_key(|f| first_offset(&f.fix));
        let mut taken: Vec<&Edit> = Vec::new();
        for f in &all {
            if !f
                .fix
                .edits
                .iter()
                .any(|e| taken.iter().any(|t| overlaps(&t.bytes, &e.bytes)))
            {
                taken.extend(f.fix.edits.iter());
            }
        }
        match apply_edits(&text, &taken) {
            Some(new) if new != text => text = new,
            _ => break,
        }
    }
    text
}

// ---------------------------------------------------------------------------------------
// Rendering

/// Counts of fixes: `(safe, suggested)`.
fn counts<'a>(fixes: impl Iterator<Item = &'a FixedFinding>) -> (usize, usize) {
    fixes.fold((0, 0), |(s, g), f| match f.fix.safety {
        Safety::Safe => (s + 1, g),
        Safety::Suggested => (s, g + 1),
    })
}

fn by_rule(fixes: &[FixedFinding]) -> String {
    let mut m: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for f in fixes {
        let e = m.entry(f.finding.rule).or_default();
        match f.fix.safety {
            Safety::Safe => e.0 += 1,
            Safety::Suggested => e.1 += 1,
        }
    }
    let mut v: Vec<(&str, (usize, usize))> = m.into_iter().collect();
    v.sort_by(|a, b| (b.1.0 + b.1.1).cmp(&(a.1.0 + a.1.1)).then(a.0.cmp(b.0)));
    v.iter()
        .map(|(r, (s, g))| match (s, g) {
            (_, 0) => format!("{r} {s} safe"),
            (0, _) => format!("{r} {g} suggested"),
            _ => format!("{r} {s} safe, {g} suggested"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn fix_line(f: &FixedFinding) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        f.finding.loc,
        f.finding.rule,
        f.fix.safety.as_str(),
        f.fix.description
    )
}

/// The text report of a fix run: what it found, the diffs, what it left alone and, after
/// writing, the comparison of findings. Suggested fixes not applied are shown each with its
/// own diff.
pub fn render_text(r: &FixReport) -> String {
    let mut out = String::new();
    let mode = if r.write {
        "lint --fix --write"
    } else {
        "lint --fix (dry run: nothing written; pass --write to apply)"
    };
    let _ = writeln!(out, "{mode}");
    let _ = writeln!(out, "lint: {}", lint::summary_line(&r.report));
    let (s, g) = counts(r.available.iter());
    if r.available.is_empty() {
        let _ = writeln!(out, "fixes available: none");
    } else {
        let _ = writeln!(
            out,
            "fixes available: {} for {} ({s} safe, {g} suggested); by rule: {}",
            r.available.len(),
            plural(r.report.findings.len(), "finding", "findings"),
            by_rule(&r.available)
        );
    }
    for (k, round) in r.rounds.iter().enumerate() {
        let n: usize = round.changes.iter().map(|c| c.fixes.len()).sum();
        let verb = if r.write { "applied" } else { "would apply" };
        let label = if r.rounds.len() > 1 || k > 0 {
            format!("round {}: ", k + 1)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "{label}{verb} {} in {}",
            plural(n, "fix", "fixes"),
            plural(round.changes.len(), "file", "files")
        );
        for c in &round.changes {
            out.push_str(&c.diff);
        }
        if !round.deferred.is_empty() {
            let _ = writeln!(
                out,
                "deferred: {} overlap another fix in the same file; {}",
                plural(round.deferred.len(), "fix", "fixes"),
                if r.write {
                    "they are retried in the next round"
                } else {
                    "re-run after applying"
                }
            );
            for f in &round.deferred {
                let _ = writeln!(out, "  {}", fix_line(f));
            }
        }
        for line in &round.refused {
            let _ = writeln!(out, "refused: {line}");
        }
    }
    if !r.suggested_skipped.is_empty() {
        let _ = writeln!(
            out,
            "suggested, not applied ({}; --fix-suggested applies them):",
            plural(r.suggested_skipped.len(), "fix", "fixes")
        );
        for s in &r.suggested_skipped {
            let _ = writeln!(out, "  {}", fix_line(&s.fixed));
            out.push_str(&s.diff);
        }
    }
    if !r.not_writable.is_empty() {
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &r.not_writable {
            *per.entry(f.fix.layer.as_str()).or_default() += 1;
        }
        let per: Vec<String> = per.iter().map(|(l, n)| format!("{l} {n}")).collect();
        let _ = writeln!(
            out,
            "skipped: {} in layers that are not writable, never applied ({}):",
            plural(r.not_writable.len(), "fix", "fixes"),
            per.join(", ")
        );
        for f in &r.not_writable {
            let _ = writeln!(out, "  {}", fix_line(f));
        }
    }
    if let Some(a) = &r.after {
        let _ = writeln!(
            out,
            "after: {} written; {} fixed, {} remaining, {} new, {} revealed in them",
            plural(a.files.len(), "file", "files"),
            plural(a.fixed.len(), "finding", "findings"),
            a.remaining.len(),
            a.new.len(),
            a.revealed.len()
        );
        if !a.revealed.is_empty() {
            let _ = writeln!(
                out,
                "revealed (on lines whose legacy name a fix converted; the rules skip legacy \
                 names, so the old spelling hid these):"
            );
            for f in &a.revealed {
                let _ = writeln!(
                    out,
                    "  {}\t{}\t{}\t{}",
                    f.severity.as_str(),
                    f.rule,
                    f.loc,
                    f.message
                );
            }
        }
        if !a.new.is_empty() {
            let _ = writeln!(
                out,
                "new findings (a fix introduced them: a bug in that fix):"
            );
            for f in &a.new {
                let _ = writeln!(
                    out,
                    "  {}\t{}\t{}\t{}",
                    f.severity.as_str(),
                    f.rule,
                    f.loc,
                    f.message
                );
            }
        }
        for p in &a.problems {
            let _ = writeln!(out, "problem: {p}");
        }
    }
    for n in &r.report.notes {
        let _ = writeln!(out, "note: {n}");
    }
    out
}

// ---------------------------------------------------------------------------------------
// Unified diff

/// A unified diff of `old` and `new` (3 lines of context), headed `--- name` / `+++ name`;
/// empty when they are equal. Lines are compared with their terminators but shown without
/// them; a last line without one is marked `\ No newline at end of file`.
pub fn unified_diff(name: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a = split_keep(old);
    let b = split_keep(new);
    let ops = myers(&a, &b);
    let mut out = format!("--- {name}\n+++ {name}\n");
    // Group ops into hunks with 3 lines of context.
    const CTX: usize = 3;
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, DiffOp::Same(..)))
        .map(|(k, _)| k)
        .collect();
    let mut k = 0usize;
    while k < changed.len() {
        let lo = changed[k].saturating_sub(CTX);
        let mut hi = (changed[k] + CTX).min(ops.len() - 1);
        k += 1;
        // Changes whose context touches this hunk's join it.
        while k < changed.len() && changed[k] <= hi + CTX + 1 {
            hi = (changed[k] + CTX).min(ops.len() - 1);
            k += 1;
        }
        let (mut a0, mut b0) = (None, None);
        let (mut na, mut nb) = (0usize, 0usize);
        let mut body = String::new();
        for op in &ops[lo..=hi] {
            match *op {
                DiffOp::Same(i, j) => {
                    a0.get_or_insert(i);
                    b0.get_or_insert(j);
                    na += 1;
                    nb += 1;
                    push_line(&mut body, ' ', a[i]);
                }
                DiffOp::Del(i, j) => {
                    a0.get_or_insert(i);
                    b0.get_or_insert(j);
                    na += 1;
                    push_line(&mut body, '-', a[i]);
                }
                DiffOp::Ins(i, j) => {
                    a0.get_or_insert(i);
                    b0.get_or_insert(j);
                    nb += 1;
                    push_line(&mut body, '+', b[j]);
                }
            }
        }
        let start = |x: Option<usize>, n: usize| x.map_or(0, |v| if n == 0 { v } else { v + 1 });
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            start(a0, na),
            na,
            start(b0, nb),
            nb
        );
        out.push_str(&body);
    }
    out
}

fn push_line(out: &mut String, mark: char, line: &str) {
    out.push(mark);
    let content = line.trim_end_matches(['\n', '\r']);
    out.push_str(content);
    if line.len() > content.len() {
        out.push('\n');
    } else {
        out.push_str("\n\\ No newline at end of file\n");
    }
}

/// Lines with their terminators.
fn split_keep(text: &str) -> Vec<&str> {
    physical_lines(text)
        .iter()
        .map(|l| &text[l.start..l.next])
        .collect()
}

/// One step of an edit script: indices into the old and the new lines (for `Del` the new
/// index is where the deletion sits, for `Ins` the old one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    Same(usize, usize),
    Del(usize, usize),
    Ins(usize, usize),
}

/// Myers' O(ND) shortest edit script, with the common prefix and suffix taken off first.
fn myers(a: &[&str], b: &[&str]) -> Vec<DiffOp> {
    let pre = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ops: Vec<DiffOp> = (0..pre).map(|i| DiffOp::Same(i, i)).collect();
    let n = ma.len() as isize;
    let m = mb.len() as isize;
    let max = n + m;
    let off = max + 1;
    let at = |k: isize| (k + off) as usize;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    'search: for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && ma[x as usize] == mb[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                break 'search;
            }
            k += 2;
        }
    }
    let mut mid: Vec<DiffOp> = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        let prev_k = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[at(prev_k)];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            mid.push(DiffOp::Same(x as usize, y as usize));
        }
        if d > 0 {
            if x == prev_x {
                mid.push(DiffOp::Ins(x as usize, (y - 1) as usize));
            } else {
                mid.push(DiffOp::Del((x - 1) as usize, y as usize));
            }
            x = prev_x;
            y = prev_y;
        }
    }
    mid.reverse();
    ops.extend(mid.into_iter().map(|o| match o {
        DiffOp::Same(i, j) => DiffOp::Same(i + pre, j + pre),
        DiffOp::Del(i, j) => DiffOp::Del(i + pre, j + pre),
        DiffOp::Ins(i, j) => DiffOp::Ins(i + pre, j + pre),
    }));
    let (ea, eb) = (a.len() - suf, b.len() - suf);
    ops.extend((0..suf).map(|s| DiffOp::Same(ea + s, eb + s)));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(src: &str) -> Statement {
        crate::parse::parse(src).statements.remove(0)
    }

    #[test]
    fn edits_apply_and_overlap() {
        let t = "abcdef";
        let e = [
            Edit::insert(0, ">"),
            Edit::replace(1..3, "X"),
            Edit::delete(4..5),
            Edit::insert(6, "<"),
        ];
        let refs: Vec<&Edit> = e.iter().collect();
        assert_eq!(apply_edits(t, &refs).as_deref(), Some(">aXdf<"));
        assert!(overlaps(&(1..3), &(2..4)));
        assert!(!overlaps(&(1..3), &(3..4)));
        assert!(overlaps(&(2..2), &(2..2)));
        assert!(!overlaps(&(1..1), &(1..3)));
        assert!(overlaps(&(2..2), &(1..3)));
        let bad: Vec<&Edit> = vec![&e[1], &e[1]];
        assert_eq!(apply_edits(t, &bad), None);
    }

    #[test]
    fn assignment_parts_read_back() {
        let src = "export A[doc] ?= \"x \\  \n  y\"  ";
        let s = first(src);
        let p = assign_parts(src, &s).unwrap();
        assert_eq!(&src[p.name.clone()], "A");
        assert_eq!(&src[p.flag.clone().unwrap()], "[doc]");
        assert_eq!(&src[p.op.clone()], "?=");
        assert_eq!(&src[p.value()], "x \\  \n  y");
        let src = "A.= 'x'";
        let p = assign_parts(src, &first(src)).unwrap();
        assert_eq!((&src[p.name.clone()], &src[p.op.clone()]), ("A", ".="));
        let src = "RDEPENDS:${PN}:append = \" a\"";
        let p = assign_parts(src, &first(src)).unwrap();
        assert_eq!(&src[p.name.clone()], "RDEPENDS:${PN}:append");
        let src = "A:= \"x\"\r\n";
        let p = assign_parts(src, &first(src)).unwrap();
        assert_eq!((&src[p.name.clone()], &src[p.op.clone()]), ("A", ":="));
        assert_eq!(p.close, 6);
    }

    #[test]
    fn function_names() {
        let src = "fakeroot python do_x_append () {\n    pass\n}\n";
        let r = function_name(src, &first(src)).unwrap();
        assert_eq!(&src[r], "do_x_append");
        let src = "python () {\n}\n";
        assert_eq!(function_name(src, &first(src)), None);
    }

    #[test]
    fn statement_line_ranges() {
        let src = "A = \"1\"\r\nB ??= \"2\"\r\nC = \"3\"";
        let f = crate::parse::parse(src);
        let r = statement_lines(src, &f.statements[1]).unwrap();
        assert_eq!(&src[r], "B ??= \"2\"\r\n");
        let r = statement_lines(src, &f.statements[2]).unwrap();
        assert_eq!(&src[r], "C = \"3\"");
        assert_eq!(line_ending(src), "\r\n");
        assert_eq!(line_ending("x"), "\n");
    }

    #[test]
    fn diffs() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk";
        assert_eq!(
            unified_diff("x", old, new),
            "--- x\n+++ x\n@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n d\n e\n@@ -8,3 +8,4 @@\n h\n i\n j\n+k\n\\ No newline at end of file\n"
        );
        assert_eq!(unified_diff("x", old, old), "");
        assert_eq!(
            unified_diff("x", "a\nb\n", "b\n"),
            "--- x\n+++ x\n@@ -1,2 +1,1 @@\n-a\n b\n"
        );
        assert_eq!(
            unified_diff("x", "", "a\n"),
            "--- x\n+++ x\n@@ -0,0 +1,1 @@\n+a\n"
        );
    }

    #[test]
    fn compare_keys_ignore_quoted_lines() {
        let f = |m: &str| Finding {
            rule: "override/weak-after-default",
            severity: crate::lint::Severity::Info,
            loc: "l:p:3".parse().unwrap(),
            message: m.to_string(),
        };
        assert_eq!(
            compare_key(&f("line 12 sets it")),
            compare_key(&f("line 3 sets it"))
        );
        assert_ne!(compare_key(&f("line 3 a")), compare_key(&f("line 3 b")));
    }
}
