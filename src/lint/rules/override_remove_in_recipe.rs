//! `override/remove-in-recipe`: `:remove` in a `.bb`, `.inc` or `.bbclass`.
//!
//! See docs/lint-rules.md. `:remove` is applied last, after every append and override
//! (bitbake-semantics.md, "Overrides"), so nothing later (a bbappend, a distro or local
//! conf) can add the removed item back. Warning in recipes, includes and classes; info in
//! bbappends and configuration files (and in `.inc` files under a `conf/` directory, such
//! as `conf/machine/include/*.inc`, which are configuration), where it is the usual tool.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{FileKind, LineSpan};

pub struct OverrideRemoveInRecipe;

impl Rule for OverrideRemoveInRecipe {
    fn id(&self) -> &'static str {
        "override/remove-in-recipe"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        let Some(kind) = cx.kind else { return };
        let in_conf_dir = cx.loc.path.components().any(|c| c.as_os_str() == "conf");
        let metadata = match kind {
            FileKind::Recipe | FileKind::Class => true,
            FileKind::Include => !in_conf_dir,
            FileKind::Append | FileKind::Conf => false,
        };
        for s in &file.statements {
            let StatementKind::Assign { var, .. } = &s.kind else {
                continue;
            };
            if var.legacy_syntax
                || var.flag.is_some()
                || !var.overrides.iter().any(|o| o == "remove")
            {
                continue;
            }
            let name = format!("{}:{}", var.base, var.overrides.join(":"));
            let line = LineSpan::line(s.span.start);
            if metadata {
                let what = match kind {
                    FileKind::Class => "class",
                    FileKind::Include => "shared include",
                    _ => "recipe",
                };
                out.push(helpers::finding(
                    self,
                    cx,
                    line,
                    format!(
                        "`{name}` in a {what} cannot be undone by a bbappend or configuration \
                         (removes apply last); leave the item out of the value instead, or \
                         make it optional through a variable or PACKAGECONFIG"
                    ),
                ));
            } else {
                let mut f = helpers::finding(
                    self,
                    cx,
                    line,
                    format!(
                        "`{name}`: acceptable here, but nothing parsed later can add the item \
                         back (removes apply last); prefer changing the value where it is set \
                         if you own that layer"
                    ),
                );
                f.severity = Severity::Info;
                out.push(f);
            }
        }
    }
}
