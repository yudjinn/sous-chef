//! `override/plus-equals-with-override`: `VAR:foo += "x"` (and `=+`, `.=`, `=.`).
//!
//! See docs/lint-rules.md and bitbake-semantics.md ("Overrides", "Datastore"): the operator
//! reads and writes exactly the name written, so `VAR:foo += "x"` builds `VAR:foo` (from
//! nothing, `" x"`), which then *replaces* `VAR` when `foo` is active. The intent is almost
//! always `VAR:append:foo = " x"`.
//!
//! Package overrides are idiomatic and not reported: `FILES:${PN} +=`,
//! `RDEPENDS:${PN}-dev +=`, `RDEPENDS:libfoo +=`. For a package-scoped variable (OE-core's
//! `PACKAGEVARS` plus `INSANE_SKIP`) the first override is taken as the package unless it is
//! a known machine, distro, class or other conditional override; any later override is
//! conditional (`RDEPENDS:${PN}:qemux86 +=` is reported). An override containing `${` is
//! never reported (it cannot be classified statically). Names with `:append` / `:prepend` /
//! `:remove` are another matter (BitBake warns) and are not reported here.

use crate::lint::helpers::{self, KnownOverrides};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{AssignOp, LineSpan, VarRef};

pub struct OverridePlusEqualsWithOverride;

/// Package-scoped variables beyond [`helpers::PACKAGE_VARS`] that commonly take a package
/// override with `+=`.
const MORE_PACKAGE_VARS: &[&str] = &["INSANE_SKIP", "QA_EMPTY_DIRS"];

impl Rule for OverridePlusEqualsWithOverride {
    fn id(&self) -> &'static str {
        "override/plus-equals-with-override"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        let mut known: Option<KnownOverrides> = None;
        for s in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            let keyword = match op {
                AssignOp::Append => "append",
                AssignOp::AppendDot => "append",
                AssignOp::Prepend => "prepend",
                AssignOp::PrependDot => "prepend",
                _ => continue,
            };
            if var.legacy_syntax
                || var.flag.is_some()
                || var.overrides.is_empty()
                || var
                    .overrides
                    .iter()
                    .any(|o| matches!(o.as_str(), "append" | "prepend" | "remove"))
            {
                continue;
            }
            let k = known
                .get_or_insert_with(|| KnownOverrides::from_index(cx.index).unwrap_or_default());
            let Some(conditional) = conditional_overrides(var, k) else {
                continue;
            };
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                message(var, *op, keyword, &conditional, value),
            ));
        }
    }
}

/// The split of `var`'s overrides into the part that names the variable (a package: it
/// stays before `:append`) and the conditions, or `None` when nothing is conditional (only
/// a package override, or only `${...}` overrides).
fn conditional_overrides(var: &VarRef, known: &KnownOverrides) -> Option<Split> {
    let package_var =
        helpers::is_package_var(&var.base) || MORE_PACKAGE_VARS.contains(&var.base.as_str());
    let is_conditional = |o: &str| !o.contains("${") && known.contains(o);
    let mut found = false;
    let mut package = false;
    for (i, o) in var.overrides.iter().enumerate() {
        let package_slot = package_var && i == 0;
        if package_slot && !is_conditional(o) {
            // A package name (or a `${...}` one).
            package = true;
            continue;
        }
        // BitBake registers a variant only while the override starts with `[a-z0-9]`
        // (`_setvar_update_overrides`); `VAR:Foo` is just another variable.
        if o.contains("${")
            || !o.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            continue;
        }
        found = true;
    }
    found.then(|| {
        let n = usize::from(package);
        Split {
            name: var.overrides[..n].to_vec(),
            conditions: var.overrides[n..].to_vec(),
        }
    })
}

struct Split {
    name: Vec<String>,
    conditions: Vec<String>,
}

fn message(var: &VarRef, op: AssignOp, keyword: &str, split: &Split, value: &str) -> String {
    let mut target = var.base.clone();
    for o in &split.name {
        target.push(':');
        target.push_str(o);
    }
    let name = format!("{target}:{}", split.conditions.join(":"));
    let spaced = matches!(op, AssignOp::Append | AssignOp::Prepend);
    let v: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let v = if v.chars().count() > 40 {
        v.chars().take(37).collect::<String>() + "..."
    } else {
        v
    };
    // `+=` / `=+` add the space; `.=` / `=.` keep whatever the value has.
    let lead = spaced || value.starts_with(char::is_whitespace);
    let trail = spaced || value.ends_with(char::is_whitespace);
    let new_value = match keyword {
        "append" if lead => format!(" {v}"),
        "prepend" if trail => format!("{v} "),
        _ => v,
    };
    format!(
        "`{name} {op}` changes the variable `{name}`, which then replaces {target} when `{cond}` \
         is active, instead of adding to {target}; write \
         `{target}:{keyword}:{cond} = \"{new_value}\"`",
        cond = split.conditions.join(":"),
        op = op.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> KnownOverrides {
        let mut k = KnownOverrides::default();
        k.machines.insert("qemux86".into());
        k
    }

    #[test]
    fn classifies_overrides() {
        let k = known();
        let c = |n: &str| conditional_overrides(&crate::parse::split_var(n, None), &k).is_some();
        assert!(c("EXTRA_OECONF:qemux86"));
        assert!(c("EXTRA_OECONF:somethingelse"));
        assert!(!c("FILES:${PN}"));
        assert!(!c("RDEPENDS:libfoo"));
        assert!(!c("INSANE_SKIP:libfoo-dev"));
        assert!(c("RDEPENDS:qemux86"));
        assert!(c("RDEPENDS:${PN}:qemux86"));
        assert!(!c("EXTRA_OECONF:${MACHINE}"));
    }
}
