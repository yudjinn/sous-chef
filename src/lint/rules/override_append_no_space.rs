//! `override/append-no-space`: `:append` without a leading space, or `:prepend` without a
//! trailing space, on a space-separated list variable.
//!
//! See docs/lint-rules.md. BitBake adds no space for `:append` / `:prepend`
//! (bitbake-semantics.md, "Overrides"), so `DEPENDS:append = "zlib"` glues `zlib` to the
//! last word. Only variables [`helpers::list_kind`] knows to be space lists are checked
//! (never colon lists such as `FILESEXTRAPATHS`, scalars, flags or unknown variables), only
//! assignments (function bodies are never checked), and not values that start with `${@`
//! (python may supply the space). `+=` / `=+` on an `:append` / `:prepend` name add the
//! space themselves and are skipped.

use crate::lint::helpers::{self, ListKind};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{AssignOp, LineSpan};

pub struct OverrideAppendNoSpace;

impl Rule for OverrideAppendNoSpace {
    fn id(&self) -> &'static str {
        "override/append-no-space"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        for s in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            if var.legacy_syntax
                || matches!(op, AssignOp::Append | AssignOp::Prepend)
                || helpers::list_kind(var) != ListKind::Space
                || value.trim().is_empty()
                || value.starts_with("${@")
            {
                continue;
            }
            // The keyword decides; `VAR:append:foo` and `VAR:foo:append` both append.
            let Some(kw) = var
                .overrides
                .iter()
                .find(|o| matches!(o.as_str(), "append" | "prepend" | "remove"))
            else {
                continue;
            };
            let name = helpers::colon_name(var);
            let msg = match kw.as_str() {
                "append" if !value.starts_with(char::is_whitespace) => format!(
                    "`{name}` value has no leading space, so it is glued to the last word of \
                     {base}; write `{name} = \" {v}\"`",
                    base = var.base,
                    v = helpers::short_value(value, false)
                ),
                "prepend" if !value.ends_with(char::is_whitespace) => format!(
                    "`{name}` value has no trailing space, so it is glued to the first word of \
                     {base}; write `{name} = \"{v} \"`",
                    base = var.base,
                    v = helpers::short_value(value, true)
                ),
                _ => continue,
            };
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                msg,
            ));
        }
    }
}
