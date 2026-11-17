//! `org/recipe-path`: Recipe not under `recipes-<category>/<pn>/`, or badly named.
//!
//! See docs/lint-rules.md. For every `.bb` of the layer (pointer: the recipe, line 1):
//!
//! - **Layout** (warning): the path is not `recipes-<category>/<dir>/<file>.bb`. Files under
//!   `dynamic-layers/` are skipped (they are collected through `BBFILES_DYNAMIC`).
//! - **Directory** (info): `<dir>` holds recipes of only this `PN` and is neither the `PN`
//!   nor a prefix of it (`linux/linux-foo_6.6.bb` and `u-boot/u-boot-foo_*.bb` are the usual
//!   BSP layout and are not reported). Directories that group several recipes (`images/`,
//!   `packagegroups/`) hold several `PN`s and are not reported either, and neither is an
//!   image recipe in `images/` or a packagegroup in `packagegroups/` when it is the only
//!   one there: that is OE-core's layout (`recipes-core/images/`,
//!   `recipes-core/packagegroups/`), the directory names the kind, not the recipe.
//! - **File name**: more than one `_` (warning: BitBake splits the name into `PN`, `PV` and
//!   `PR`, `bb.parse.vars_from_file`); no `_<pv>` at all (info, `PV` then defaults to `1.0`),
//!   except for `packagegroup-*` recipes and recipes whose static tree inherits an image class
//!   ([`helpers::image_class`]: `image`, `core-image`, `*-image`) or `packagegroup`, which
//!   are conventionally unversioned.
//! - **PN** (warning): upper-case letters (OE-core's `uppercase-pn` QA check).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{FileKind, LineSpan};

pub struct OrgRecipePath;

fn components(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// A recipe kind that is conventionally unversioned and kept in a directory named after
/// the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Image,
    Packagegroup,
    Other,
}

/// The recipe's kind from the classes its static tree inherits ([`query::tree`]: its own
/// `inherit` lines and those of the files it requires, so `core-image-minimal-dev.bb`,
/// which only `require`s `core-image-minimal.bb`, is an image), and a `packagegroup-` name.
fn recipe_kind(cx: &LintContext<'_>, file: &query::FileRow, pn: &str) -> Kind {
    let inherits: Vec<String> = query::tree(cx.index, file.id)
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| n.kind == "inherit")
                .map(|n| match &n.file {
                    Some(f) => f
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    None => n.target_text.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    if inherits.iter().any(|c| helpers::image_class(c)) {
        Kind::Image
    } else if pn.starts_with("packagegroup-") || inherits.iter().any(|c| c == "packagegroup") {
        Kind::Packagegroup
    } else {
        Kind::Other
    }
}

impl Rule for OrgRecipePath {
    fn id(&self) -> &'static str {
        "org/recipe-path"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(layer) = cx.layer else { return };
        let Ok(files) = query::files_in_layer(cx.index, layer) else {
            return;
        };
        let recipes: Vec<&query::FileRow> = files
            .iter()
            .filter(|f| f.kind == FileKind::Recipe)
            .collect();
        // PNs per directory.
        let mut by_dir: HashMap<&Path, HashSet<&str>> = HashMap::new();
        for f in &recipes {
            if let (Some(dir), Some(pn)) = (f.path.parent(), f.pn.as_deref()) {
                by_dir.entry(dir).or_default().insert(pn);
            }
        }
        for f in recipes {
            let comps = components(&f.path);
            if comps.first().map(String::as_str) == Some("dynamic-layers") {
                continue;
            }
            let Some(file_name) = comps.last() else {
                continue;
            };
            let stem = file_name.strip_suffix(".bb").unwrap_or(file_name);
            let parts: Vec<&str> = stem.split('_').collect();
            let pn = parts.first().copied().unwrap_or(stem);
            let kind = recipe_kind(cx, f, pn);
            let mut push = |severity: Severity, message: String| {
                out.push(Finding {
                    rule: self.id(),
                    severity,
                    loc: f.loc(LineSpan::line(1)),
                    message,
                });
            };
            let layout_ok = comps.len() == 3 && comps[0].starts_with("recipes-");
            if !layout_ok {
                push(
                    Severity::Warning,
                    format!(
                        "recipe {} is not at recipes-<category>/<pn>/<pn>_<pv>.bb; move it to \
                         recipes-<category>/{pn}/{file_name}",
                        f.path.display()
                    ),
                );
            } else {
                let dir = comps[1].as_str();
                let only_this = f
                    .path
                    .parent()
                    .and_then(|d| by_dir.get(d))
                    .is_some_and(|pns| pns.len() == 1);
                let kind_dir = matches!(
                    (kind, dir),
                    (Kind::Image, "images") | (Kind::Packagegroup, "packagegroups")
                );
                if only_this && !kind_dir && dir != pn && !pn.starts_with(&format!("{dir}-")) {
                    push(
                        Severity::Info,
                        format!(
                            "recipe {pn} is in directory {}/{dir}/, which holds no other \
                             recipe; name the directory after the recipe: {}/{pn}/",
                            comps[0], comps[0]
                        ),
                    );
                }
            }
            if parts.len() > 2 {
                let fixed = format!(
                    "{}_{}.bb",
                    parts[..parts.len() - 1].join("-"),
                    parts[parts.len() - 1]
                );
                push(
                    Severity::Warning,
                    format!(
                        "file name {file_name} has more than one `_`: BitBake reads PN `{}`, \
                         PV `{}` and the rest as PR; use `-` inside the name: {fixed}",
                        parts[0], parts[1]
                    ),
                );
            } else if parts.len() == 1 && kind == Kind::Other {
                push(
                    Severity::Info,
                    format!(
                        "file name {file_name} has no version, so PV defaults to 1.0; name it \
                         {pn}_<version>.bb"
                    ),
                );
            }
            if pn.chars().any(|c| c.is_ascii_uppercase()) {
                let lower = stem.replacen(pn, &pn.to_ascii_lowercase(), 1);
                push(
                    Severity::Warning,
                    format!(
                        "PN `{pn}` has upper-case letters, which OE-core's uppercase-pn QA check \
                         reports; rename the file to {lower}.bb"
                    ),
                );
            }
        }
    }
}
