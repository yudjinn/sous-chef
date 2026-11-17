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
//! file that belongs to no recipe, or an `.inc` also pulled in by a class or a `.conf`,
//! keeps its findings.
//!
//! **Empty values.** The check is `if d.getVar(var, False):` (insane.bbclass, Wrynose
//! `33a6bd6c`, the `python ()` after "virtual/ is meaningless"), so a variable whose value
//! is empty is never reported: `FILES = ""` (OE-core's `bitbake.conf:336`), or any `=`,
//! `:=`, `?=`, `??=`, `.=`, `=.`, `:append` or `:prepend` of an empty value adds nothing and
//! is not a finding. `+=` / `=+` of an empty value store a space, which is not empty, and
//! stay reported.
//!
//! **Classes.** A class is not reported when it is `image` or `core-image`, inherits one of
//! them directly, or itself sets an unconditional `PACKAGES = ""` (or `:=`) and nothing else in it gives
//! `PACKAGES` a value: its package variables are meant for recipes without packages
//! (OE-core's `image.bbclass:31-35`, `PACKAGES = ""` then `RDEPENDS +=`). A recipe that
//! inherits such a class and fills `PACKAGES` again would be checked by OE-core; that is
//! missed. Other classes keep their findings, `*-image` classes included: the name alone
//! does not empty `PACKAGES` (`kernel-fit-image.bbclass` sets `FILES:${PN}`).

use std::collections::HashSet;

use crate::index::{Index, query};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParsedFile, StatementKind};
use crate::types::{AssignOp, FileKind, LineSpan, VarRef};

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
        if cx.kind == Some(FileKind::Class) && class_without_packages(cx, file) {
            return;
        }
        for s in &file.statements {
            let (var, is_fn) = match &s.kind {
                StatementKind::Assign { var, op, value, .. } => {
                    if value.trim().is_empty()
                        && !matches!(op, AssignOp::Append | AssignOp::Prepend)
                    {
                        // Gives the variable no value: pkgvarcheck's getVar is falsy.
                        continue;
                    }
                    (var, false)
                }
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
            let written = helpers::colon_name(var);
            let fixed = helpers::joined(&format!("{}:${{PN}}", var.base), &var.overrides, ':');
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

/// Whether the linted class is for recipes without packages: `image` or `core-image`, a
/// class that inherits one of them directly, or one that sets an unconditional empty `PACKAGES`
/// (`=` / `:=`) and gives it no value anywhere else.
fn class_without_packages(cx: &LintContext<'_>, file: &ParsedFile) -> bool {
    let stem = cx
        .loc
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let empties = |c: &str| matches!(c, "image" | "core-image");
    if empties(stem) {
        return true;
    }
    let mut emptied = false;
    for s in &file.statements {
        match &s.kind {
            StatementKind::Inherit { classes, .. } if classes.split_whitespace().any(empties) => {
                return true;
            }
            StatementKind::Assign { var, op, value, .. }
                if var.base == "PACKAGES" && var.flag.is_none() =>
            {
                let empty = value.trim().is_empty();
                if var.overrides.is_empty()
                    && matches!(op, AssignOp::Set | AssignOp::Immediate)
                    && empty
                {
                    emptied = true;
                } else if !empty
                    && !matches!(op, AssignOp::Default | AssignOp::WeakDefault)
                    && var.overrides.first().is_none_or(|o| o != "remove")
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    emptied
}

/// Whether the linted file belongs to at least one recipe and every recipe it belongs to has
/// an empty `PACKAGES` ([`packages_empty`]). Any index error answers false (report).
fn in_recipes_without_packages(cx: &LintContext<'_>) -> bool {
    let Ok(Some(f)) = query::file_for_loc(cx.index, &cx.loc) else {
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
