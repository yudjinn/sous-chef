//! `org/recipe-change-in-local-conf`: `local.conf` carries `:pn-<recipe>` changes.
//!
//! See docs/lint-rules.md. A statement in a file named `local.conf` whose overrides include
//! `pn-<name>` (a literal name; `pn-${...}` is skipped) changes one recipe from the build
//! directory, where nobody else sees it. The message names the bbappend form (the same
//! statement without the `pn-` override) and the distro conf as the alternatives.

use crate::index::scan::written_name;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::FileKind;

pub struct OrgRecipeChangeInLocalConf;

impl Rule for OrgRecipeChangeInLocalConf {
    fn id(&self) -> &'static str {
        "org/recipe-change-in-local-conf"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if cx.kind != Some(FileKind::Conf)
            || cx.loc.path.file_name().and_then(|n| n.to_str()) != Some("local.conf")
        {
            return;
        }
        let Some(file) = cx.file else { return };
        for st in &file.statements {
            let StatementKind::Assign { var, .. } = &st.kind else {
                continue;
            };
            let Some(pn) = var
                .overrides
                .iter()
                .find_map(|o| o.strip_prefix("pn-"))
                .filter(|pn| !pn.is_empty() && !pn.contains("${"))
            else {
                continue;
            };
            let name = written_name(var, helpers::statement_text(cx, st));
            let mut rest = var.clone();
            rest.overrides.retain(|o| o.strip_prefix("pn-") != Some(pn));
            rest.legacy_syntax = false;
            let plain = written_name(&rest, "");
            out.push(helpers::finding(
                self,
                cx,
                st.span,
                format!(
                    "`{name}` in local.conf changes only recipe {pn}, and only in this build \
                     directory; move it to a {pn}_%.bbappend in your layer as `{plain}`, or to \
                     your distro conf if it is policy"
                ),
            ));
        }
    }
}
