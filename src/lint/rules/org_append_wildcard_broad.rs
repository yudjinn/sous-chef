//! `org/append-wildcard-broad`: A `%` bbappend that matches several versions while pinning
//! version-specific data.
//!
//! See docs/lint-rules.md. For each `.bbappend` of the layer with `%` in its name whose
//! `bbappend` edges reach recipes of at least two different `PV`s, the first statement that
//! pins version-specific data:
//!
//! - `SRC_URI` (any operator or override) adding a `file://` patch (`.patch` / `.diff`);
//! - `SRCREV` / `SRCREV_<name>` (not `SRCREV_FORMAT`), unless it uses `AUTOREV`;
//! - `PV` set with `=`, `?=`, `??=` or `:=`;
//! - a checksum flag of `SRC_URI`.
//!
//! One finding per append, on that statement.

use crate::index::query;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::words;
use crate::types::FileKind;

pub struct OrgAppendWildcardBroad;

fn pinned(s: &query::StatementRow) -> Option<&'static str> {
    if s.kind != "assign" {
        return None;
    }
    let base = s.base.as_deref().unwrap_or("");
    let value = s.value.as_deref().unwrap_or("");
    let flag = s.flag.as_deref();
    match (base, flag) {
        ("SRC_URI", None)
            if words::words(value).into_iter().any(|w| {
                let w = w.split(';').next().unwrap_or(w);
                w.starts_with("file://") && (w.ends_with(".patch") || w.ends_with(".diff"))
            }) =>
        {
            Some("a patch")
        }
        ("SRC_URI", Some(f)) if f.ends_with("sum") => Some("a source checksum"),
        ("PV", None)
            if s.overrides.is_empty()
                && matches!(s.op.as_deref(), Some("=" | "?=" | "??=" | ":=")) =>
        {
            Some("the version")
        }
        (b, None)
            if (b == "SRCREV" || (b.starts_with("SRCREV_") && b != "SRCREV_FORMAT"))
                && !value.contains("AUTOREV") =>
        {
            Some("a source revision")
        }
        _ => None,
    }
}

impl Rule for OrgAppendWildcardBroad {
    fn id(&self) -> &'static str {
        "org/append-wildcard-broad"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(layer) = cx.layer else { return };
        let Ok(files) = query::files_in_layer(cx.index, layer) else {
            return;
        };
        for f in files.iter().filter(|f| f.kind == FileKind::Append) {
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.contains('%') {
                continue;
            }
            let Ok(edges) = query::edges_from(cx.index, f.id) else {
                continue;
            };
            let mut versions: Vec<String> = edges
                .iter()
                .filter(|e| e.kind == "bbappend")
                .filter_map(|e| e.target.as_ref()?.pv.clone())
                .collect();
            versions.sort_by(|a, b| query::vercmp(a, b));
            versions.dedup();
            if versions.len() < 2 {
                continue;
            }
            let Ok(stmts) =
                query::statements_in(cx.index, f.id, &query::StatementFilter::default())
            else {
                continue;
            };
            let Some((s, what)) = stmts.iter().find_map(|s| pinned(s).map(|w| (s, w))) else {
                continue;
            };
            let pn = f.pn.as_deref().unwrap_or("the recipe");
            let newest = versions.last().cloned().unwrap_or_default();
            let list = match versions.as_slice() {
                [init @ .., last] => format!("{} and {last}", init.join(", ")),
                [] => String::new(),
            };
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: s.loc.clone(),
                message: format!(
                    "{name} applies to {pn} {list} but pins {what}, which usually fits one \
                     version; name the append after that version ({pn}_{newest}.bbappend), \
                     or check that it fits every version"
                ),
            });
        }
    }
}
