//! `org/inherit-defer`: `inherit ${VAR}` where the variable may be changed later.
//!
//! See docs/lint-rules.md. In a `.bb`, a plain `inherit` whose text references variables
//! (`${VAR}`, or `d.getVar('VAR')` inside `${@...}`): `inherit` expands its argument when it
//! is parsed (bitbake-semantics.md, "Directives"), so a later change does not choose the
//! class. "Later" is a statement on `VAR` (any operator or override) after the inherit line
//! in the same file, anonymous python there or in a bbappend that names `VAR` in a
//! `d.setVar` / `d.appendVar` / `d.prependVar` / `d.delVar` / `d.setVarFlag` call, or any
//! statement on `VAR` in a bbappend of this recipe (index; bbappends are parsed after the
//! recipe).
//!
//! Releases: `inherit_defer` exists from Scarthgap (`Feature::InheritDefer`); the rule runs
//! only for layers checked against such a release, and stays silent when any of the layer's
//! releases lacks it (the suggestion would not parse there).

use std::ops::RangeInclusive;

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParsedFile, StatementKind};
use crate::release::{Feature, Release};
use crate::types::{FileKind, LineSpan, Loc};

pub struct OrgInheritDefer;

const SETTERS: &[&str] = &[
    "setVar(",
    "appendVar(",
    "prependVar(",
    "delVar(",
    "setVarFlag(",
];

/// Variable names an inherit argument depends on.
fn referenced(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        if !n.is_empty()
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && !out.iter().any(|o| o == n)
        {
            out.push(n.to_string());
        }
    };
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        let after = &rest[i + 2..];
        if let Some(end) = after.find('}')
            && !after.starts_with('@')
        {
            push(&after[..end]);
        }
        rest = after;
    }
    let mut rest = text;
    while let Some(i) = rest.find("getVar(") {
        let after = rest[i + 7..].trim_start();
        if let Some(q) = after.chars().next().filter(|c| *c == '\'' || *c == '"')
            && let Some(end) = after[1..].find(q)
        {
            push(&after[1..1 + end]);
        }
        rest = &rest[i + 7..];
    }
    out
}

/// The first line at or after `after` (exclusive) where `parsed` changes `var`.
fn changes(parsed: &ParsedFile, text: &str, var: &str, after: u32) -> Option<u32> {
    let quoted = [format!("'{var}'"), format!("\"{var}\"")];
    for st in &parsed.statements {
        if st.span.start <= after {
            continue;
        }
        match &st.kind {
            StatementKind::Assign { var: v, .. } if v.base == var => return Some(st.span.start),
            StatementKind::Unset { name, .. } if name == var => return Some(st.span.start),
            StatementKind::Function { name: None, .. } => {
                let body = text.get(st.bytes.clone()).unwrap_or("");
                if SETTERS.iter().any(|s| body.contains(s))
                    && quoted.iter().any(|q| body.contains(q.as_str()))
                {
                    return Some(st.span.start);
                }
            }
            _ => {}
        }
    }
    None
}

/// Where a bbappend of the recipe changes `var`: its pointer.
fn changed_in_appends(cx: &LintContext<'_>, var: &str) -> Option<Loc> {
    let row = query::file_for_loc(cx.index, &cx.loc).ok()??;
    for a in query::appends_for(cx.index, row.id).ok()? {
        let Some((text, parsed)) = helpers::read_indexed(cx, &a) else {
            continue;
        };
        if let Some(line) = changes(&parsed, &text, var, 0) {
            return Some(a.loc(LineSpan::line(line)));
        }
    }
    None
}

impl Rule for OrgInheritDefer {
    fn id(&self) -> &'static str {
        "org/inherit-defer"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn releases(&self) -> RangeInclusive<Release> {
        helpers::feature_range(Feature::InheritDefer)
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if cx.kind != Some(FileKind::Recipe)
            || !helpers::releases_lacking(cx, Feature::InheritDefer).is_empty()
        {
            return;
        }
        let (Some(file), Some(text)) = (cx.file, cx.source) else {
            return;
        };
        for st in &file.statements {
            let StatementKind::Inherit {
                classes,
                deferred: false,
            } = &st.kind
            else {
                continue;
            };
            for var in referenced(classes) {
                let at = match changes(file, text, &var, st.span.end) {
                    Some(line) => Some(helpers::loc_at(cx, LineSpan::line(line))),
                    None => changed_in_appends(cx, &var),
                };
                let Some(at) = at else { continue };
                out.push(helpers::finding(
                    self,
                    cx,
                    st.span,
                    format!(
                        "`inherit {classes}` expands {var} when it is parsed, but {at} changes \
                         {var} later, so that change does not choose the class; write \
                         `inherit_defer {classes}`"
                    ),
                ));
                break;
            }
        }
    }
}
