//! `org/class-location`: Class in `classes/` in a layer whose releases split class
//! directories.
//!
//! See docs/lint-rules.md. For each `classes/<name>.bbclass` of the layer, when every
//! release the layer is checked against has `Feature::SplitClassDirs` (Langdale and later):
//! a layer that also claims Kirkstone keeps its classes in `classes/`, the only directory
//! Kirkstone searches, and gets no finding.
//!
//! Where to move it depends on how the workspace uses it (static index data):
//!
//! - global: `base`, or a word of a static `INHERIT`, `USER_CLASSES`, `PACKAGE_CLASSES` or
//!   `INHERIT_DISTRO` value (OE-core's `bitbake.conf` adds the last three to `INHERIT`), or
//!   inherited by a class in `classes-global/`;
//! - recipe: inherited (index `inherit` edges) by any other file;
//! - both: `classes/` is right, no finding;
//! - neither: the message says classes/ is right for a class meant for both contexts
//!   (OE-core keeps such classes there) and names both directories for the others.

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::words;
use crate::release::Feature;
use crate::types::{FileKind, LineSpan};
use crate::workspace::resolve::{ClassDir, class_dir};

pub struct OrgClassLocation;

const GLOBAL_LISTS: &[&str] = &[
    "INHERIT",
    "USER_CLASSES",
    "PACKAGE_CLASSES",
    "INHERIT_DISTRO",
];

fn global_names(cx: &LintContext<'_>) -> Vec<String> {
    let scope = query::StatementFilter {
        kinds: vec!["assign".into()],
        ..query::StatementFilter::default()
    };
    let mut out = vec!["base".to_string()];
    for base in GLOBAL_LISTS {
        for s in query::statements_by_base(cx.index, base, &scope).unwrap_or_default() {
            for w in words::words(&s.value.unwrap_or_default()) {
                if !w.contains("${") && !out.iter().any(|o| o == w) {
                    out.push(w.to_string());
                }
            }
        }
    }
    out
}

impl Rule for OrgClassLocation {
    fn id(&self) -> &'static str {
        "org/class-location"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(layer) = cx.layer else { return };
        if !helpers::releases_lacking(cx, Feature::SplitClassDirs).is_empty() {
            return;
        }
        let Ok(files) = query::files_in_layer(cx.index, layer) else {
            return;
        };
        let mut globals: Option<Vec<String>> = None;
        let releases = helpers::release_list(cx.releases);
        for f in files.iter().filter(|f| f.kind == FileKind::Class) {
            let mut comps = f.path.components();
            let (Some(dir), Some(_), None) = (comps.next(), comps.next(), comps.next()) else {
                continue;
            };
            if dir.as_os_str() != "classes" {
                continue;
            }
            let Some(name) = f.path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let globals = globals.get_or_insert_with(|| global_names(cx));
            let users = query::inheritors(cx.index, name, false).unwrap_or_default();
            let by_global_class = users.iter().any(|u| {
                u.file.kind == FileKind::Class && class_dir(cx.ws, &u.file.path) == ClassDir::Global
            });
            let global = globals.iter().any(|g| g == name) || by_global_class;
            let recipe = users.iter().any(|u| {
                !(u.file.kind == FileKind::Class
                    && class_dir(cx.ws, &u.file.path) == ClassDir::Global)
                    && u.file.kind != FileKind::Conf
            });
            let advice = match (global, recipe) {
                (true, true) => continue,
                (true, false) => {
                    "move it to classes-global/ (it is used through INHERIT) and keep the name"
                        .to_string()
                }
                (false, true) => {
                    "move it to classes-recipe/ (recipes inherit it) and keep the name".to_string()
                }
                // Nothing in the workspace uses it: classes/ is where a class meant for both
                // contexts belongs (OE-core keeps `archiver`, `externalsrc` there; BitBake
                // searches classes/ after classes-<type>/, BBHandler.py), so only a class
                // meant for one context should move (lint-precision).
                (false, false) => "nothing in the workspace inherits it, so keep it in classes/ \
                                   if it is meant for both INHERIT and recipes; otherwise move it \
                                   to classes-recipe/ (inherited by recipes) or classes-global/ \
                                   (used through INHERIT)"
                    .to_string(),
            };
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: f.loc(LineSpan::line(1)),
                message: format!(
                    "class {name} is in classes/, but every release this layer is checked \
                     against ({releases}) searches classes-recipe/ and classes-global/ first; \
                     {advice}"
                ),
            });
        }
    }
}
