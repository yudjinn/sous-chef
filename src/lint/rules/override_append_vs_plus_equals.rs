//! `override/append-vs-plus-equals`: `:append` in a `.bb` where the variable is set with `=`
//! earlier in the same file and `+=` would do.
//!
//! See docs/lint-rules.md. Only `.bb` files (in bbappends and classes `:append` is usually
//! right: it survives a later `=`). Reported: an unconditional `VAR:append` (no override
//! before or after the keyword, no flag) after a plain `VAR = ` / `VAR := ` in the same
//! file, when no later statement of the file assigns `VAR` again or is an `inherit` /
//! `require` / `include` (which may assign it; then `:append` is what keeps the text), no
//! statement of the file assigns an override variant `VAR:<o>` (which replaces `VAR` and
//! its `+=` text but still gets the `:append`), and no `unset VAR` comes in between. A bbappend that sets `VAR =` later would also drop a `+=`;
//! that is why this is only info. The suggestion is `+=` when the
//! appended text starts with a space (it adds one) and `.=` otherwise.

use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{Statement, StatementKind};
use crate::types::{AssignOp, FileKind, LineSpan};

pub struct OverrideAppendVsPlusEquals;

impl Rule for OverrideAppendVsPlusEquals {
    fn id(&self) -> &'static str {
        "override/append-vs-plus-equals"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    /// Suggested: `VAR:append = " x"` becomes `VAR += "x"` (one leading space taken out, the
    /// one `+=` puts back), and a value without a leading space `VAR .= "x"`. The value
    /// reached at the end of this file is the same; it is not the same when something
    /// parsed later sets `VAR` (a bbappend's `VAR =` drops a `+=` but keeps an `:append`) or
    /// an override variant of it (`VAR:qemux86 =` wins over the `+=` text and gets the
    /// `:append`), which is why this is a style suggestion and needs a look.
    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        self.check(cx, &mut found);
        found
            .into_iter()
            .filter_map(|finding| {
                let s = file.statements.iter().find(|s| {
                    s.span.start == finding.loc.lines.start
                        && matches!(&s.kind, StatementKind::Assign { var, op: AssignOp::Set, .. }
                            if var.overrides == ["append"])
                })?;
                let StatementKind::Assign { var, .. } = &s.kind else {
                    return None;
                };
                let p = fix::assign_parts(source, s)?;
                let base = var.base.as_str();
                let spaced = source.as_bytes().get(p.open + 1) == Some(&b' ');
                let mut edits = vec![
                    Edit::replace(p.name.clone(), base),
                    Edit::replace(p.op.clone(), if spaced { "+=" } else { ".=" }),
                ];
                if spaced {
                    edits.push(Edit::delete(p.open + 1..p.open + 2));
                }
                let fix = Fix::in_file(
                    cx,
                    edits,
                    format!(
                        "`{base}:append =` becomes `{base} {}` (the same value here; not when a \
                         bbappend or a conf sets {base} or an override of it later)",
                        if spaced { "+=" } else { ".=" }
                    ),
                    Safety::Suggested,
                )?;
                Some(FixedFinding { finding, fix })
            })
            .collect()
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        if cx.kind != Some(FileKind::Recipe) {
            return;
        }
        let stmts = &file.statements;
        for (i, s) in stmts.iter().enumerate() {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            if var.legacy_syntax
                || var.flag.is_some()
                || var.overrides != ["append"]
                || *op != AssignOp::Set
                || value.is_empty()
            {
                continue;
            }
            let base = var.base.as_str();
            let Some(set_line) = last_plain_set_before(&stmts[..i], base) else {
                continue;
            };
            // A later assignment, or a later inherit / include that may assign it: there
            // `:append` is what keeps the text.
            if stmts[i + 1..].iter().any(|t| {
                assigns(t, base)
                    || matches!(
                        t.kind,
                        StatementKind::Inherit { .. } | StatementKind::Include { .. }
                    )
            }) {
                continue;
            }
            // An override variant of VAR anywhere in the file (`SRC_URI:class-devupstream =`
            // with `BBCLASSEXTEND = "devupstream:target"`): it replaces VAR, `+=` text with
            // it, while `:append` still applies there (OE-core lttng-modules: lint-precision).
            if stmts.iter().any(|t| sets_variant(t, base)) {
                continue;
            }
            let (op, text) = if value.starts_with(char::is_whitespace) {
                ("+=", value.trim_start())
            } else {
                (".=", value.as_str())
            };
            let text = helpers::clip(text);
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`{base}:append` where line {set_line} of the same recipe sets {base} with \
                     `=` and nothing later assigns it: a parse-time operator says the same \
                     more plainly; write `{base} {op} \"{text}\"`"
                ),
            ));
        }
    }
}

/// The line of the last `VAR =` / `VAR :=` (no override, no flag) before the end of
/// `stmts`, unless an `unset VAR` follows it.
fn last_plain_set_before(stmts: &[Statement], base: &str) -> Option<u32> {
    for s in stmts.iter().rev() {
        match &s.kind {
            StatementKind::Unset { name, flag: None } if name == base => return None,
            StatementKind::Assign { var, op, .. }
                if !var.legacy_syntax
                    && var.base == base
                    && var.overrides.is_empty()
                    && var.flag.is_none()
                    && matches!(op, AssignOp::Set | AssignOp::Immediate) =>
            {
                return Some(s.span.start);
            }
            _ => {}
        }
    }
    None
}

/// Whether `s` assigns a conditional variant `VAR:<override>` (no flag): the first
/// override is no `append` / `prepend` / `remove`.
fn sets_variant(s: &Statement, base: &str) -> bool {
    matches!(&s.kind, StatementKind::Assign { var, .. }
        if !var.legacy_syntax && var.base == base && var.flag.is_none()
            && var.overrides.first().is_some_and(|o| !matches!(o.as_str(), "append" | "prepend" | "remove")))
}

/// Whether `s` gives `VAR` (no override, no flag) a value with a parse-time operator.
fn assigns(s: &Statement, base: &str) -> bool {
    matches!(&s.kind, StatementKind::Assign { var, .. }
        if !var.legacy_syntax && var.base == base && var.overrides.is_empty() && var.flag.is_none())
}
