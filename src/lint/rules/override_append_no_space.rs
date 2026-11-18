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
//!
//! A value that begins (`:append`) or ends (`:prepend`) with a `${NAME}` reference cannot
//! be judged from the statement: `NAME`'s value may carry the space (OE-core's
//! `no-static-libs.inc`: `DISABLE_STATIC ?= " --disable-static"`, then
//! `EXTRA_OECONF:append = "${DISABLE_STATIC}"`). It is reported only when the file itself
//! gives `NAME` (by `=`, `:=`, `?=` or `??=`, any condition) a non-empty literal value
//! without the space; an empty value adds nothing and does not count. A python end
//! (`${@...}` or anything ending in a `}` that is not a plain reference) is never judged.

use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers::{self, ListKind, ValueEdge};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParsedFile, StatementKind};
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
        out.extend(self.scan(cx, false).into_iter().map(|(f, _)| f));
    }

    /// Safe: one space inserted right after the opening quote (`:append`) or right before the
    /// closing quote (`:prepend`), the separator BitBake does not add; nothing else changes.
    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        self.scan(cx, true)
            .into_iter()
            .filter_map(|(finding, fix)| fix.map(|fix| FixedFinding { finding, fix }))
            .collect()
    }
}

impl OverrideAppendNoSpace {
    fn scan(&self, cx: &LintContext<'_>, want_fix: bool) -> Vec<(Finding, Option<Fix>)> {
        let mut out = Vec::new();
        let Some(file) = cx.file else { return out };
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
            let appends = kw == "append";
            if kw != "remove" && !glued(file, value, appends) {
                continue;
            }
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
            let finding = helpers::finding(self, cx, LineSpan::line(s.span.start), msg);
            let fix = want_fix
                .then(|| {
                    let p = fix::assign_parts(cx.source?, s)?;
                    let (at, what) = if appends {
                        (p.open + 1, "leading")
                    } else {
                        (p.close, "trailing")
                    };
                    Fix::in_file(
                        cx,
                        vec![Edit::insert(at, " ")],
                        format!("add the {what} space to the `{name}` value"),
                        Safety::Safe,
                    )
                })
                .flatten();
            out.push((finding, fix));
        }
        out
    }
}

/// Whether the value lacks its separating space: at the start for an append, at the end
/// for a prepend. Literal text decides; a `${NAME}` there decides only when this file gives
/// `NAME` a literal value that starts (ends) without a space; python never.
fn glued(file: &ParsedFile, value: &str, appends: bool) -> bool {
    fn edge(s: &str, appends: bool) -> ValueEdge<'_> {
        if appends {
            helpers::leading_edge(s)
        } else {
            helpers::trailing_edge(s)
        }
    }
    let spaced = |s: &str| {
        if appends {
            s.starts_with(char::is_whitespace)
        } else {
            s.ends_with(char::is_whitespace)
        }
    };
    match edge(value, appends) {
        ValueEdge::Literal => !spaced(value),
        ValueEdge::Unknown => false,
        ValueEdge::Ref(name) => helpers::plain_values(file, name)
            .iter()
            .any(|(_, v)| !v.is_empty() && edge(v, appends) == ValueEdge::Literal && !spaced(v)),
    }
}
