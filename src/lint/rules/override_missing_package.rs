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

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{LineSpan, VarRef};

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
