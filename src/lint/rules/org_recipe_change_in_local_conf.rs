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
            let kas = kas_note(cx, helpers::statement_text(cx, st));
            out.push(helpers::finding(
                self,
                cx,
                st.span,
                format!(
                    "`{name}` in local.conf changes only recipe {pn}, and only in this build \
                     directory; move it to a {pn}_%.bbappend in your layer as `{plain}`, or to \
                     your distro conf if it is policy{kas}"
                ),
            ));
        }
    }
}

/// In a kas project whose build directory this `local.conf` is in: where the line comes
/// from (a `local_conf_header` entry) or that kas rewrites the file. Empty otherwise, so the
/// message outside kas is unchanged.
fn kas_note(cx: &LintContext<'_>, text: &str) -> String {
    let (Some(k), Some(build)) = (cx.ws.kas.as_ref(), cx.ws.build_dir.as_ref()) else {
        return String::new();
    };
    if cx.loc.layer != crate::workspace::BUILD_LAYER || !k.generates(build) {
        return String::new();
    }
    let first = text.lines().next().unwrap_or("");
    match k.header_line_source(false, first) {
        Some(h) => format!(
            "; kas generated it from local_conf_header entry `{}` at {}, so change it there",
            h.id,
            h.loc.display_in(&k.root)
        ),
        None => {
            let file = k
                .file_for_header(false)
                .map(|f| f.strip_prefix(&k.root).unwrap_or(f).display().to_string())
                .unwrap_or_else(|| "your kas file".into());
            format!(
                "; kas rewrites this local.conf on its next run, so if it must stay a local setting put it in local_conf_header in {file}"
            )
        }
    }
}
