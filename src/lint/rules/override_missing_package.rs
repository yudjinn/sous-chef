//! `override/missing-package`: package-scoped variables assigned without a package override.
//!
//! See docs/lint-rules.md. The variables are those of OE-core's `pkgvarcheck` QA check
//! ([`helpers::PACKAGE_ONLY_VARS`]: `RDEPENDS`, `RRECOMMENDS`, `RSUGGESTS`, `RCONFLICTS`,
//! `RPROVIDES`, `RREPLACES`, `FILES`, `pkg_preinst`, `pkg_postinst`, `pkg_prerm`,
//! `pkg_postrm`, `ALLOW_EMPTY`), which reports each one set without a package as an error.
//! Reported: an assignment (any operator, also `:append` / `:prepend`, which give the bare
//! variable a value) or a function (`pkg_postinst() {`) whose name has no override other than
//! `append` / `prepend`. Not reported: flags (`RDEPENDS[doc]`), `:remove` alone (it gives the
//! variable no value), legacy `_` names (`override/legacy-syntax` reports them).
//!
//! **Recipes without packages.** OE-core runs the check only when `PACKAGES` is not empty:
//! the `python ()` anonymous function of `insane.bbclass` guards the loop over these
//! variables with `if (d.getVar('PACKAGES') or "").split():` (checked 2026-11 on master,
//! `meta/classes-global/insane.bbclass`; scarthgap, same path; kirkstone,
//! `meta/classes/insane.bbclass`). `image.bbclass` sets `PACKAGES = ""` on all three, and
//! `core-image.bbclass` inherits `image`; `packagegroup.bbclass` sets `PACKAGES = "${PN}"`, so
//! packagegroups are checked. The rule is file scope, so it asks the index: nothing is
//! reported in a file when every recipe it belongs to (a `.bb` itself; the recipes a
//! `.bbappend` applies to; the recipes that `require` / `include` an `.inc`, through other
//! `.inc` files and bbappends) statically has an empty `PACKAGES` ([`packages_empty`]). A
//! file that belongs to no recipe, a class, or an `.inc` also pulled in by a class or a
//! `.conf` keeps its findings.

use std::collections::HashSet;

use crate::index::{Index, query};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{FileKind, LineSpan, VarRef};

pub struct OverrideMissingPackage;

impl Rule for OverrideMissingPackage {
    fn id(&self) -> &'static str {
        "override/missing-package"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        // Asked of the index only when there is something to report.
        let mut without_packages: Option<bool> = None;
        for s in &file.statements {
            let (var, is_fn) = match &s.kind {
                StatementKind::Assign { var, .. } => (var, false),
                StatementKind::Function {
                    name: Some(var), ..
                } => (var, true),
                _ => continue,
            };
            if !lacks_package(var) {
                continue;
            }
            if *without_packages.get_or_insert_with(|| in_recipes_without_packages(cx)) {
                return;
            }
            let written = {
                let mut n = var.base.clone();
                for o in &var.overrides {
                    n.push(':');
                    n.push_str(o);
                }
                n
            };
            let fixed = {
                let mut n = format!("{}:${{PN}}", var.base);
                for o in &var.overrides {
                    n.push(':');
                    n.push_str(o);
                }
                n
            };
            let parens = if is_fn { "()" } else { "" };
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`{written}{parens}` has no package override: {base} is per package, and \
                     OE-core's QA check (pkgvarcheck) rejects it set without one; write \
                     `{fixed}{parens}` (or the package it is for)",
                    base = var.base
                ),
            ));
        }
    }
}

/// Whether the linted file belongs to at least one recipe and every recipe it belongs to has
/// an empty `PACKAGES` ([`packages_empty`]). Any index error answers false (report).
fn in_recipes_without_packages(cx: &LintContext<'_>) -> bool {
    let Ok(Some(f)) = query::file_by_path(cx.index, &cx.loc.layer, &cx.loc.path) else {
        return false;
    };
    match recipes_of(cx.index, &f) {
        Ok(Some(recipes)) if !recipes.is_empty() => recipes
            .iter()
            .all(|r| packages_empty(cx.index, *r).unwrap_or(false)),
        _ => false,
    }
}

/// The recipe files (`.bb` ids) a file belongs to; `None` when it also belongs to something
/// that is not a recipe (a class, a `.conf`), or is one.
fn recipes_of(index: &Index, file: &query::FileRow) -> crate::error::Result<Option<Vec<i64>>> {
    let mut recipes: Vec<i64> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut todo = vec![file.clone()];
    while let Some(f) = todo.pop() {
        if !seen.insert(f.id) {
            continue;
        }
        match f.kind {
            FileKind::Recipe => recipes.push(f.id),
            FileKind::Append => recipes.extend(
                query::edges_from(index, f.id)?
                    .iter()
                    .filter(|e| e.kind == "bbappend")
                    .filter_map(|e| e.target.as_ref().map(|t| t.id)),
            ),
            FileKind::Include => {
                for e in query::edges_to(index, f.id)? {
                    if e.kind != "inherit" && e.kind != "bbappend" {
                        todo.push(e.src);
                    }
                }
            }
            FileKind::Class | FileKind::Conf => return Ok(None),
        }
    }
    recipes.sort_unstable();
    recipes.dedup();
    Ok(Some(recipes))
}

/// Whether recipe `recipe` (a `.bb` id) statically ends with an empty `PACKAGES`: its tree
/// ([`query::tree`]) and its bbappends' trees hold an unconditional `PACKAGES = ""` (or
/// `:=`), or inherit `image` / `core-image` where the class is not indexed, and no
/// statement that could give `PACKAGES` a value (any other assignment, conditional or
/// not, except the weak `?=` / `??=` and `:remove`). Order is not considered, so a recipe
/// that empties `PACKAGES` and fills it again is treated as having packages.
pub fn packages_empty(index: &Index, recipe: i64) -> crate::error::Result<bool> {
    let mut files: Vec<i64> = Vec::new();
    let mut emptied_by_class = false;
    let mut roots = vec![recipe];
    roots.extend(query::appends_for(index, recipe)?.iter().map(|a| a.id));
    for root in roots {
        for node in query::tree(index, root)? {
            match &node.file {
                Some(f) => files.push(f.id),
                None if node.kind == "inherit" => {
                    emptied_by_class |= matches!(node.target_text.as_str(), "image" | "core-image");
                }
                None => {}
            }
        }
    }
    files.sort_unstable();
    files.dedup();
    let scope = query::StatementFilter {
        kinds: vec!["assign".into()],
        files: Some(files),
        ..query::StatementFilter::default()
    };
    let mut emptied = emptied_by_class;
    for s in query::statements_by_base(index, "PACKAGES", &scope)? {
        if s.flag.is_some() {
            continue;
        }
        let op = s.op.as_deref().unwrap_or("");
        let empty = s.value.as_deref().unwrap_or("").trim().is_empty();
        if s.overrides.is_empty() && matches!(op, "=" | ":=") && empty {
            emptied = true;
            continue;
        }
        let weak = matches!(op, "?=" | "??=");
        let removes = s.overrides.first().is_some_and(|o| o == "remove");
        if !weak && !removes && !empty {
            return Ok(false);
        }
    }
    Ok(emptied)
}

fn lacks_package(var: &VarRef) -> bool {
    if var.legacy_syntax || var.flag.is_some() || !helpers::is_package_only(&var.base) {
        return false;
    }
    // What comes before the first keyword names the variable (the package would be there);
    // what follows `:append:` / `:prepend:` are only conditions. `:remove` alone gives the
    // bare variable no value.
    match var.overrides.first().map(String::as_str) {
        None | Some("append" | "prepend") => true,
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_detection() {
        let l = |n: &str| lacks_package(&crate::parse::split_var(n, None));
        assert!(l("RDEPENDS"));
        assert!(l("RDEPENDS:append"));
        assert!(l("RDEPENDS:append:qemux86"));
        assert!(l("pkg_postinst"));
        assert!(!l("RDEPENDS:remove"));
        assert!(!l("RDEPENDS:${PN}"));
        assert!(!l("RDEPENDS:${PN}:append"));
        assert!(!l("FILES:libfoo"));
        assert!(!l("SUMMARY"));
        assert!(!l("RDEPENDS_${PN}"));
    }
}
