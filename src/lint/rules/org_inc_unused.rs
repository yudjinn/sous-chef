//! `org/inc-unused`: A `.inc` that nothing requires or includes.
//!
//! See docs/lint-rules.md. An `.inc` of the layer is unused when no index edge
//! (`require`, `include`, `include_all`, from any file of the workspace, the build
//! directory included) resolves to it. An edge that is unresolved because its argument needs
//! evaluation (`require ${FOO}.inc`) might point at it: its text, with every `${...}`
//! replaced by `*`, is matched against the `.inc`'s file name (last path component), and a
//! match keeps the file from being reported. A fully dynamic argument (`include ${@...}`)
//! therefore hides every `.inc`: misses rather than false positives.

use std::path::Path;

use crate::index::query;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{FileKind, LineSpan};

pub struct OrgIncUnused;

/// `text` with every `${...}` (nested braces included) replaced by `*`.
fn wildcard(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut depth = 1;
            for c in chars.by_ref() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if !out.ends_with('*') {
                out.push('*');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Glob match with `*` only.
fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for p in &parts[1..parts.len() - 1] {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    true
}

/// The last path component of each unresolved include-type edge in the workspace that needs
/// evaluation, as a `*` pattern.
fn dynamic_patterns(cx: &LintContext<'_>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Ok(files) = query::all_files(cx.index) else {
        return out;
    };
    for f in files {
        let Ok(edges) = query::edges_from(cx.index, f.id) else {
            continue;
        };
        for e in edges {
            if !matches!(e.kind.as_str(), "require" | "include" | "include_all")
                || e.target.is_some()
                || !e.target_text.contains("${")
            {
                continue;
            }
            let w = wildcard(&e.target_text);
            // The last component; a `*` that spans a `/` makes it `*` as a whole.
            let last = match w.rsplit_once('/') {
                Some((_, l)) => l.to_string(),
                None => w,
            };
            if !out.contains(&last) {
                out.push(last);
            }
        }
    }
    out
}

impl Rule for OrgIncUnused {
    fn id(&self) -> &'static str {
        "org/inc-unused"
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
        let mut patterns: Option<Vec<String>> = None;
        for f in files.iter().filter(|f| f.kind == FileKind::Include) {
            let Ok(users) = query::edges_to(cx.index, f.id) else {
                continue;
            };
            if users
                .iter()
                .any(|e| matches!(e.kind.as_str(), "require" | "include" | "include_all"))
            {
                continue;
            }
            let name = Path::new(&f.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let patterns = patterns.get_or_insert_with(|| dynamic_patterns(cx));
            if patterns.iter().any(|p| matches(p, &name)) {
                continue;
            }
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: f.loc(LineSpan::line(1)),
                message: format!(
                    "nothing in the workspace requires or includes {name}; remove it, or \
                     `require` it from the recipes that need it"
                ),
            });
        }
    }
}
