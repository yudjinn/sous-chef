//! `override/append-vs-plus-equals`: `:append` in a `.bb` where the variable is set with `=`
//! earlier in the same file and `+=` would do.
//!
//! See docs/lint-rules.md. Only `.bb` files (in bbappends and classes `:append` is usually
//! right: it survives a later `=`). Reported: an unconditional `VAR:append` (no override
//! before or after the keyword, no flag) after a plain `VAR = ` / `VAR := ` in the same
//! file, when no later statement of the file assigns `VAR` again or is an `inherit` /
//! `require` / `include` (which may assign it; then `:append` is what keeps the text) and no
//! `unset VAR` comes in between. A bbappend that sets `VAR =` later would also drop a `+=`;
//! that is why this is only info. The suggestion is `+=` when the
//! appended text starts with a space (it adds one) and `.=` otherwise.

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
            let (op, text) = if value.starts_with(char::is_whitespace) {
                ("+=", value.trim_start())
            } else {
                (".=", value.as_str())
            };
            let text: String = if text.chars().count() > 40 {
                text.chars().take(37).collect::<String>() + "..."
            } else {
                text.to_string()
            };
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

/// Whether `s` gives `VAR` (no override, no flag) a value with a parse-time operator.
fn assigns(s: &Statement, base: &str) -> bool {
    matches!(&s.kind, StatementKind::Assign { var, .. }
        if !var.legacy_syntax && var.base == base && var.overrides.is_empty() && var.flag.is_none())
}
