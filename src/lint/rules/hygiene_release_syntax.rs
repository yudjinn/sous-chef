//! `hygiene/release-syntax`: a statement, directive or location that one of the releases a
//! file is checked against does not have.
//!
//! See docs/lint-rules.md and "Releases" in docs/bitbake-semantics.md. Driven by the
//! `Feature` table in `src/release.rs`: each check names a feature, and a finding names the
//! releases among [`LintContext::releases`] that lack it ("`inherit_defer` is not available
//! in kirkstone, which this layer is checked against"). No codename is compared here.
//!
//! - `inherit_defer` (`Feature::InheritDefer`), error. Not in a `.conf` file, where
//!   `hygiene/parse-error` already reports it on every release.
//! - `include_all` (`Feature::IncludeAll`), error.
//! - A class file in `classes-recipe/` or `classes-global/` (`Feature::SplitClassDirs`,
//!   classified by `workspace::resolve::class_dir`), error on line 1: BitBake before 2.2
//!   never searches those directories, so `inherit` of the class fails there.
//! - `${UNPACKDIR}` referenced (`Feature::UnpackDir`), error: OE-core sets no `UNPACKDIR`
//!   before Styhead (checked: `meta/conf/bitbake.conf` on the `kirkstone` and `scarthgap`
//!   branches has no `UNPACKDIR`, and `base_do_unpack` there unpacks into `WORKDIR`; on
//!   `styhead` `UNPACKDIR ??= "${WORKDIR}/sources-unpack"`), so the reference stays
//!   unexpanded. Not reported when any indexed file in the workspace assigns `UNPACKDIR`
//!   (a layer may define it for older releases).
//! - The other direction of the same change, warning: `${WORKDIR}/<file>` where `<file>` is a
//!   `file://` entry of `SRC_URI` in the same file without a `subdir=` parameter. From
//!   Styhead on such files are unpacked into `UNPACKDIR`, not `WORKDIR` (5.1 migration guide:
//!   recipes that reference `SRC_URI` files through `WORKDIR` "likely need changing to
//!   UNPACKDIR"). Reported for the releases that have `Feature::UnpackDir`, unless the file
//!   assigns `UNPACKDIR` itself.

use std::collections::BTreeSet;

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::words;
use crate::parse::{IncludeMode, StatementKind};
use crate::release::{Feature, Release};
use crate::types::{FileKind, LineSpan};
use crate::workspace::resolve::{self, ClassDir};

pub struct HygieneReleaseSyntax;

impl HygieneReleaseSyntax {
    fn lacking(
        &self,
        cx: &LintContext<'_>,
        feature: Feature,
        line: u32,
        what: &str,
        instead: &str,
        out: &mut Vec<Finding>,
    ) {
        let lacking = helpers::releases_lacking(cx, feature);
        if lacking.is_empty() {
            return;
        }
        out.push(helpers::finding(
            self,
            cx,
            LineSpan::line(line),
            format!(
                "{what} is not available in {}, which this layer is checked against; {instead}",
                helpers::release_list(&lacking)
            ),
        ));
    }
}

/// Whether `text` references `${UNPACKDIR}` (or `${UNPACKDIR}` inside a longer path).
fn mentions_unpackdir(text: &str) -> bool {
    text.contains("${UNPACKDIR}")
}

/// The `file://` entries of a `SRC_URI` value that land directly in the unpack directory
/// (no `subdir=`), as written after `file://`.
fn local_files(value: &str) -> Vec<String> {
    words::words(value)
        .into_iter()
        .filter_map(|w| w.strip_prefix("file://"))
        .filter(|w| !w.split(';').skip(1).any(|p| p.starts_with("subdir=")))
        .map(|w| w.split(';').next().unwrap_or(w).to_string())
        .filter(|p| !p.is_empty() && !p.contains("${") && !p.contains('*'))
        .collect()
}

/// The first `${WORKDIR}/<file>` reference to one of `files` in `text`.
fn workdir_ref<'a>(text: &str, files: &'a BTreeSet<String>) -> Option<&'a str> {
    let mut rest = text;
    while let Some(at) = rest.find("${WORKDIR}/") {
        let after = &rest[at + "${WORKDIR}/".len()..];
        let path: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | ';' | ')' | '`'))
            .collect();
        if let Some(f) = files.iter().find(|f| path == **f) {
            return Some(f.as_str());
        }
        rest = after;
    }
    None
}

/// Whether any indexed file assigns `UNPACKDIR`. An index error counts as "yes": when it
/// cannot tell, the rule stays silent.
fn unpackdir_defined(cx: &LintContext<'_>) -> bool {
    let filter = query::StatementFilter {
        kinds: vec!["assign".into()],
        ..query::StatementFilter::default()
    };
    match query::statements_by_base(cx.index, "UNPACKDIR", &filter) {
        Ok(rows) => !rows.is_empty(),
        Err(_) => true,
    }
}

impl Rule for HygieneReleaseSyntax {
    fn id(&self) -> &'static str {
        "hygiene/release-syntax"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        if cx.kind == Some(FileKind::Class)
            && matches!(
                resolve::class_dir(cx.ws, &cx.loc.path),
                ClassDir::Recipe | ClassDir::Global | ClassDir::Unsearched
            )
        {
            let dir = cx
                .loc
                .path
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            self.lacking(
                cx,
                Feature::SplitClassDirs,
                1,
                &format!("the `{dir}/` class directory"),
                "BitBake there finds classes only in `classes/`; move the class to `classes/` \
                 while the layer claims that release",
                out,
            );
        }

        let mut assigns_unpackdir = false;
        let mut files: BTreeSet<String> = BTreeSet::new();
        for s in &file.statements {
            if let StatementKind::Assign { var, value, .. } = &s.kind {
                if var.base == "UNPACKDIR" {
                    assigns_unpackdir = true;
                }
                if var.base == "SRC_URI" && var.flag.is_none() {
                    files.extend(local_files(value));
                }
            }
        }

        let old_unpack: Vec<Release> = helpers::releases_lacking(cx, Feature::UnpackDir);
        let mut defined_elsewhere: Option<bool> = None;
        let new_unpack: Vec<Release> = helpers::releases_having(cx, Feature::UnpackDir);

        for s in &file.statements {
            let line = s.span.start;
            match &s.kind {
                StatementKind::Inherit { deferred: true, .. }
                    if cx.kind != Some(FileKind::Conf) =>
                {
                    self.lacking(
                        cx,
                        Feature::InheritDefer,
                        line,
                        "`inherit_defer`",
                        "BitBake fails to parse the file there; use `inherit` (with a value \
                         that is final at this point) while the layer claims that release",
                        out,
                    );
                }
                StatementKind::Include {
                    mode: IncludeMode::IncludeAll,
                    ..
                } => {
                    self.lacking(
                        cx,
                        Feature::IncludeAll,
                        line,
                        "`include_all`",
                        "BitBake fails to parse the file there; use `include` or `require` of \
                         one path while the layer claims that release",
                        out,
                    );
                }
                _ => {}
            }
            let text = match &s.kind {
                StatementKind::Assign { var, .. } if var.base == "UNPACKDIR" => continue,
                StatementKind::Assign { .. }
                | StatementKind::Function { .. }
                | StatementKind::PyDef { .. } => helpers::statement_text(cx, s),
                _ => continue,
            };
            if mentions_unpackdir(text)
                && !assigns_unpackdir
                && !old_unpack.is_empty()
                && !*defined_elsewhere.get_or_insert_with(|| unpackdir_defined(cx))
            {
                let at = text
                    .lines()
                    .position(mentions_unpackdir)
                    .and_then(|i| u32::try_from(i).ok())
                    .map_or(line, |i| line + i);
                self.lacking(
                    cx,
                    Feature::UnpackDir,
                    at,
                    "`${UNPACKDIR}`",
                    "OE-core does not set it there and sources are unpacked into ${WORKDIR}; \
                     use ${WORKDIR} while the layer claims that release",
                    out,
                );
            }
            if !new_unpack.is_empty()
                && !assigns_unpackdir
                && let Some((i, f)) = text
                    .lines()
                    .enumerate()
                    .find_map(|(i, l)| workdir_ref(l, &files).map(|f| (i, f)))
            {
                let at = u32::try_from(i).map_or(line, |i| line + i);
                let mut finding = helpers::finding(
                    self,
                    cx,
                    LineSpan::line(at),
                    format!(
                        "`${{WORKDIR}}/{f}`: from {} on, `file://` sources are unpacked into \
                         ${{UNPACKDIR}}, not ${{WORKDIR}}, and this layer is checked against {}; \
                         write `${{UNPACKDIR}}/{f}` (or `${{S}}/{f}` with S = \"${{UNPACKDIR}}\")",
                        Feature::UnpackDir.first().codename(),
                        helpers::release_list(&new_unpack)
                    ),
                );
                finding.severity = Severity::Warning;
                out.push(finding);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_entries() {
        assert_eq!(
            local_files("file://a.c file://b.patch;subdir=x https://x/y.tgz file://d/e.cfg"),
            ["a.c", "d/e.cfg"]
        );
        let files: BTreeSet<String> = ["a.c".to_string()].into();
        assert_eq!(workdir_ref("cc ${WORKDIR}/a.c -o x", &files), Some("a.c"));
        assert_eq!(workdir_ref("cc ${WORKDIR}/a.cc", &files), None);
        assert_eq!(workdir_ref("cp ${WORKDIR}/git/a.c", &files), None);
    }
}
