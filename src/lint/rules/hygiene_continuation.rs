//! `hygiene/continuation`: problems with `\` line continuations.
//!
//! See docs/lint-rules.md and "Lines" in docs/bitbake-semantics.md. Two findings:
//!
//! - **Whitespace after a trailing `\`.** BitBake right-strips every line first, so the line
//!   still continues, but the `\` no longer looks like the last character and editors may
//!   drop it. Taken from the parser's diagnostic, which `hygiene/parse-error` deliberately
//!   skips. Lines inside function bodies never join, so the parser does not report them.
//! - **A blank line ending a continued statement.** `VAR = "a \` followed by an empty line
//!   joins the empty line and ends the statement there, before its closing quote; BitBake
//!   then fails on the rest. Reported on the blank line.
//!
//! A comment line inside a continued statement is not reported here: BitBake rejects it,
//! and `hygiene/parse-error` already reports the parser's diagnostic for it as an error.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::LineSpan;

pub struct HygieneContinuation;

impl Rule for HygieneContinuation {
    fn id(&self) -> &'static str {
        "hygiene/continuation"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        for d in &file.diagnostics {
            if d.message.starts_with("whitespace after `\\`") {
                out.push(helpers::finding(
                    self,
                    cx,
                    LineSpan::line(d.span.start),
                    "whitespace after the trailing `\\`: BitBake strips it and still \
                     continues the line; remove the whitespace so `\\` ends the line",
                ));
            }
        }
        let Some(source) = cx.source else { return };
        let lines: Vec<&str> = source.lines().collect();
        for s in &file.statements {
            if matches!(
                s.kind,
                StatementKind::Function { .. } | StatementKind::PyDef { .. }
            ) || s.span.end <= s.span.start
            {
                continue;
            }
            // Lines are 1-based; the last line of the statement is the one a `\` ran into.
            let last = s.span.end as usize;
            let (Some(prev), Some(blank)) = (lines.get(last - 2), lines.get(last - 1)) else {
                continue;
            };
            if blank.trim().is_empty() && prev.trim_end().ends_with('\\') {
                out.push(helpers::finding(
                    self,
                    cx,
                    LineSpan::line(s.span.end),
                    "blank line after a trailing `\\` ends the continued statement here; \
                     remove the blank line, or the `\\` on the line before it",
                ));
            }
        }
    }
}
