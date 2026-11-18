//! `hygiene/missing-metadata`: a recipe without `SUMMARY`, `LICENSE` or `LIC_FILES_CHKSUM`
//! (warning), or without `HOMEPAGE` (info).
//!
//! See docs/lint-rules.md. Recipe scope: the values come from the evaluated recipe (its
//! `.inc` files, bbappends and classes count). Defaults from configuration do not count:
//! OE-core's `bitbake.conf` sets `SUMMARY ?= "${PN} version ${PV}-${PR}"`,
//! `LICENSE ??= "INVALID"` and `HOMEPAGE = ""` (checked against openembedded-core master,
//! 2026-11), so a variable counts as set only when an operation from a recipe, `.inc`,
//! bbappend or class contributes to it (any status but an inactive override), and its value
//! is not empty.
//!
//! `LIC_FILES_CHKSUM` is required only when `LICENSE` is not `CLOSED` and `SRC_URI` is not
//! empty, as OE-core's `populate_lic_qa_checksum` (`insane.bbclass`, master 2026-11) checks
//! it: `if lic == "CLOSED": return` and `if not lic_files and d.getVar('SRC_URI')`.
//!
//! Not reported when the value depends on python the evaluator cannot run: the value is
//! marked `unevaluated_python`, or anonymous python sets the variable.
//!
//! **`HOMEPAGE`** is the upstream project's page, so it is asked for only when the recipe
//! has an upstream: its evaluated `SRC_URI` has a word with a URL scheme other than
//! `file://` (python pieces of the value do not matter then; without such a word, a value
//! with python says nothing and `HOMEPAGE` is not asked for). Not asked for: an image (a class [`helpers::image_class`] names
//! among the classes the recipe pass visits), a packagegroup (`packagegroup.bbclass`
//! visited), a recipe with no `SRC_URI` or only `file://` entries (`connman-conf`,
//! `base-files`: files of the layer itself). OE-core
//! Wrynose `33a6bd6c` sets `SUMMARY` in nearly every image and packagegroup of
//! `recipes-*/images/` and `recipes-*/packagegroups/`, but `HOMEPAGE` in only four of 37
//! (`build-appliance-image`, `core-image-ptest`, `core-image-ptest-all`,
//! `packagegroup-core-x11-sato`); `LICENSE` comes from `image.bbclass:31` /
//! `packagegroup.bbclass:23` (`LICENSE ?= "MIT"`), which counts as set (a class is not
//! configuration). `SUMMARY` and `LICENSE` are asked of every recipe.

use crate::eval::{DataStore, OpStatus};
use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::words;
use crate::types::{FileKind, LineSpan};

pub struct HygieneMissingMetadata;

/// How a variable stands in the evaluated recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Set,
    Missing,
    /// Depends on python; say nothing.
    Unknown,
}

fn state(data: &DataStore, name: &str) -> State {
    if data.replay().python_vars.contains(name) {
        return State::Unknown;
    }
    let value = data.value(name);
    if value.as_ref().is_some_and(|v| v.unevaluated_python) {
        return State::Unknown;
    }
    let from_metadata = data.history(name).iter().any(|(op, status)| {
        *status != OpStatus::InactiveOverride
            && FileKind::from_path(&op.loc.path) != Some(FileKind::Conf)
    });
    let non_empty = value.is_some_and(|v| !v.text.trim().is_empty());
    if from_metadata && non_empty {
        State::Set
    } else {
        State::Missing
    }
}

/// The evaluation says BitBake would skip the recipe (decided from static values).
pub(crate) fn helpers_would_skip(data: &DataStore) -> bool {
    data.replay()
        .target_notes
        .iter()
        .any(|n| n.starts_with("note: BitBake would skip this recipe"))
}

/// Whether the recipe fetches from an upstream (the `HOMEPAGE` condition): not an image or a
/// packagegroup, and a statically known `SRC_URI` with an entry whose scheme is not `file`.
fn has_upstream(cx: &LintContext<'_>, data: &DataStore) -> bool {
    let kind_class = |c: &str| helpers::image_class(c) || c == "packagegroup";
    let (tree, all) = helpers::recipe_pass_files(cx, data);
    let visited = all.iter().any(|(_, path)| {
        FileKind::from_path(path) == Some(FileKind::Class)
            && path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(kind_class)
    });
    // Also an `inherit` in the recipe's own files whose class is not in the workspace.
    let inherits = || {
        let scope = query::StatementFilter {
            kinds: vec!["inherit".into()],
            ..query::StatementFilter::default()
        };
        tree.iter().any(|(layer, path)| {
            let Ok(Some(f)) = query::file_by_path(cx.index, layer, path) else {
                return false;
            };
            query::statements_in(cx.index, f.id, &scope)
                .unwrap_or_default()
                .iter()
                .any(|s| {
                    words::words(s.name.as_deref().unwrap_or(""))
                        .into_iter()
                        .any(kind_class)
                })
        })
    };
    if visited || inherits() {
        return false;
    }
    let Some(src_uri) = data.value("SRC_URI") else {
        return false;
    };
    // A remote URL written out in the value is an upstream whatever the python pieces add;
    // without one, a value with python cannot be judged (and says nothing).
    words::words(&src_uri.text).into_iter().any(|w| {
        w.split_once("://").is_some_and(|(scheme, _)| {
            scheme != "file"
                && !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-')
        })
    })
}

impl Rule for HygieneMissingMetadata {
    fn id(&self) -> &'static str {
        "hygiene/missing-metadata"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(data) = cx.data else { return };
        let mut missing: Vec<&str> = Vec::new();
        for name in ["SUMMARY", "LICENSE"] {
            if state(data, name) == State::Missing {
                missing.push(name);
            }
        }
        let license = data.value("LICENSE");
        let closed = license
            .as_ref()
            .is_some_and(|v| !v.unevaluated_python && v.text.trim() == "CLOSED");
        let license_known = license.as_ref().is_none_or(|v| !v.unevaluated_python);
        let src_uri = data.value("SRC_URI");
        let fetches = src_uri
            .as_ref()
            .is_some_and(|v| !v.unevaluated_python && !v.text.trim().is_empty());
        // BitBake skips the recipe under the configuration evaluated (a static "would skip"
        // note): it runs no license QA there, and a value that differs per machine
        // (`LIC_FILES_CHKSUM:x86-64`) says nothing about the others.
        let skipped = helpers_would_skip(data);
        if license_known
            && !closed
            && fetches
            && !skipped
            && state(data, "LIC_FILES_CHKSUM") == State::Missing
        {
            missing.push("LIC_FILES_CHKSUM");
        }
        let line = LineSpan::line(1);
        if !missing.is_empty() {
            out.push(helpers::finding(
                self,
                cx,
                line,
                format!(
                    "recipe has no {}; set {} in the recipe (or its .inc){}",
                    missing.join(", "),
                    if missing.len() == 1 { "it" } else { "them" },
                    if missing.contains(&"LIC_FILES_CHKSUM") {
                        ", or LICENSE = \"CLOSED\" for closed source"
                    } else {
                        ""
                    }
                ),
            ));
        }
        if has_upstream(cx, data) && state(data, "HOMEPAGE") == State::Missing {
            let mut f = helpers::finding(
                self,
                cx,
                line,
                "recipe has no HOMEPAGE; set HOMEPAGE to the upstream project's web page",
            );
            f.severity = Severity::Info;
            out.push(f);
        }
    }
}
