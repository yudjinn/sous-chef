//! `override/dead`: Override that matches no known machine, distro, class, package or
//! `OVERRIDES` entry.
//!
//! See docs/lint-rules.md. Recipe scope: the rule reads the evaluated [`DataStore`] of the
//! recipe and looks at the operations that come from the recipe's own files (the `.bb`, the
//! `.inc` files it pulls in, its bbappends; not classes or configuration, which every recipe
//! shares) and are inactive ([`OpStatus::InactiveOverride`]). An override element of such an
//! operation is dead when it is none of:
//!
//! - an entry of the recipe's evaluated `OVERRIDES` ([`DataStore::overrides`]);
//! - a name [`KnownOverrides::contains`] accepts (machine and distro conf stems, static words
//!   of `OVERRIDES` / `MACHINEOVERRIDES` / `DISTROOVERRIDES`, `class-*`, `pn-*`, the target
//!   OS and architectures, ...) or a class stem;
//! - a word that appears anywhere in the value of an `*OVERRIDES` / `*OVERRIDE` assignment of
//!   the workspace, python included (OE-core's tune files add `armv7a` and the like to
//!   `MACHINEOVERRIDES` from `${@...}`).
//!
//! Never reported: `append` / `prepend` / `remove`, an element containing `${`, an element
//! that does not start with `[a-z0-9]` (BitBake does not treat it as an override), the
//! package position of a package-scoped variable (`RDEPENDS:<pkg>`: packages can come from
//! python at build time), legacy `_` names.
//!
//! When the override list cannot be known completely (`OVERRIDES` needs python or has
//! unresolved references, or anonymous python changes an `*OVERRIDES` variable or is
//! opaque), the finding is info instead of a warning: the evidence is only that the name is
//! nowhere in the workspace.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use crate::eval::{DataStore, OpStatus};
use crate::index::query::{self, StatementFilter};
use crate::lint::helpers::{self, KnownOverrides};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{Loc, VarRef};

pub struct OverrideDead;

/// Variables whose values feed `OVERRIDES` in OE-core.
const OVERRIDE_VARS: &[&str] = &[
    "OVERRIDES",
    "MACHINEOVERRIDES",
    "MACHINEOVERRIDES_EXTENDER",
    "DISTROOVERRIDES",
    "CLASSOVERRIDE",
    "LIBCOVERRIDE",
    "TCOVERRIDE",
    "TUNEOVERRIDES",
    "FILESOVERRIDES",
];

impl Rule for OverrideDead {
    fn id(&self) -> &'static str {
        "override/dead"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(data) = cx.data else { return };
        let (tree, _) = helpers::recipe_pass_files(cx, data);
        if tree.is_empty() {
            return;
        }
        let bases = bases_with_overrides(cx, &tree);
        if bases.is_empty() {
            return;
        }
        let known = Known::load(cx, data);
        for base in &bases {
            for (op, status) in data.history(base) {
                if status != OpStatus::InactiveOverride
                    || op.var.legacy_syntax
                    || !tree.contains(&(op.loc.layer.clone(), op.loc.path.clone()))
                {
                    continue;
                }
                let Some(dead) = dead_element(&op.var, &known) else {
                    continue;
                };
                let name = helpers::colon_name(&op.var);
                let hint = known
                    .closest(dead)
                    .map(|c| format!(" (`{c}`?)"))
                    .unwrap_or_default();
                let mut f = helpers::finding(
                    self,
                    cx,
                    op.loc.lines,
                    format!(
                        "override `{dead}` in `{name}` is no machine, distro, class, package or \
                         OVERRIDES entry in the workspace, so this never applies; fix the \
                         name{hint} or remove the line"
                    ),
                );
                f.loc = Loc {
                    lines: crate::types::LineSpan::line(op.loc.lines.start),
                    ..op.loc.clone()
                };
                if known.partial {
                    f.severity = Severity::Info;
                }
                out.push(f);
            }
        }
    }
}

/// Bases of the assignments and functions with overrides in the tree's files (index).
fn bases_with_overrides(
    cx: &LintContext<'_>,
    tree: &HashSet<(String, PathBuf)>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let filter = StatementFilter {
        kinds: vec!["assign".into(), "function".into()],
        ..StatementFilter::default()
    };
    for (layer, path) in tree {
        let Ok(Some(f)) = query::file_by_path(cx.index, layer, path) else {
            continue;
        };
        for s in query::statements_in(cx.index, f.id, &filter).unwrap_or_default() {
            if let Some(b) = s.base
                && !s.overrides.is_empty()
                && !s.legacy
            {
                out.insert(b);
            }
        }
    }
    out
}

/// Everything an override may legitimately be.
struct Known {
    index: KnownOverrides,
    active: BTreeSet<String>,
    words: BTreeSet<String>,
    partial: bool,
}

impl Known {
    fn load(cx: &LintContext<'_>, data: &DataStore) -> Known {
        let index = KnownOverrides::from_index(cx.index).unwrap_or_default();
        let active: BTreeSet<String> = data.overrides().into_iter().collect();
        let mut words = BTreeSet::new();
        let filter = StatementFilter {
            kinds: vec!["assign".into()],
            ..StatementFilter::default()
        };
        for base in OVERRIDE_VARS {
            for s in query::statements_by_base(cx.index, base, &filter).unwrap_or_default() {
                words.extend(tokens(s.value.as_deref().unwrap_or("")));
            }
            for (op, _) in data.history(base) {
                words.extend(tokens(&op.value));
            }
        }
        let notes = data.replay();
        let partial = data
            .overrides_value()
            .is_none_or(|v| v.unevaluated_python || !v.unresolved.is_empty())
            || notes.anonymous.iter().any(|a| a.opaque)
            || notes
                .python_vars
                .iter()
                .any(|v| OVERRIDE_VARS.contains(&v.as_str()));
        Known {
            index,
            active,
            words,
            partial,
        }
    }

    fn contains(&self, o: &str) -> bool {
        self.active.contains(o)
            || self.index.contains(o)
            || self.index.classes.contains(o)
            || self.words.contains(o)
    }

    /// A known machine, distro or override at edit distance 1 or 2 (names of 4+ characters).
    fn closest(&self, o: &str) -> Option<&str> {
        if o.chars().count() < 4 {
            return None;
        }
        self.index
            .machines
            .iter()
            .chain(&self.index.distros)
            .chain(&self.index.declared)
            .chain(&self.active)
            .map(|c| (distance(o, c), c.as_str()))
            .filter(|(d, _)| *d > 0 && *d <= 2)
            .min()
            .map(|(_, c)| c)
    }
}

/// Words of an `OVERRIDES`-like value: runs of `[a-z0-9_+.-]`, `${...}` names excluded.
fn tokens(value: &str) -> Vec<String> {
    value
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "-_+.".contains(c)))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// The first override element of `var` that is dead, if any.
fn dead_element<'a>(var: &'a VarRef, known: &Known) -> Option<&'a str> {
    let package_var = helpers::is_package_var(&var.base);
    for (i, o) in var.overrides.iter().enumerate() {
        let o = o.as_str();
        if matches!(o, "append" | "prepend" | "remove")
            || o.contains("${")
            || !o.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            continue;
        }
        if package_var && i == 0 {
            continue;
        }
        if !known.contains(o) {
            return Some(o);
        }
    }
    None
}

/// Levenshtein distance.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance() {
        assert_eq!(distance("boradx", "boardx"), 2);
        assert_eq!(distance("qemux86", "qemux86"), 0);
        assert_eq!(distance("", "abc"), 3);
    }

    #[test]
    fn tokens_include_python() {
        let t = tokens("${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}");
        assert!(t.contains(&"armv7a".to_string()), "{t:?}");
        assert!(!t.contains(&"TUNE_FEATURES".to_string()));
    }
}
