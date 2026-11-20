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
//!   are conventionally unversioned, and recipes whose static tree (their own lines and the
//!   files they require or include) sets `PV` itself.
//! - **PN** (warning): upper-case letters (OE-core's `uppercase-pn` QA check).
//!
//! A `.bb` BitBake does not collect because no `BBFILES` glob of any layer matches it
//! (`files.not_collected = 'bbfiles'`) gets exactly one finding instead of the checks
//! above, naming the layer's globs: **info** when it sits in a directory that is clearly no
//! recipe tree (a `lib/`, `scripts/`, `files/`, `wic/` or `templates/` component, or a
//! recipe's own `<pn>/` / `<pn>-<pv>/` directory next to a collected `.bb`), else
//! **warning** (under `recipes-*/` or anywhere else, it looks like a recipe someone expects
//! to build). A `.bb` not collected for another reason (an inactive `BBFILES_DYNAMIC` entry,
//! `BBMASK`) is left alone: that is what the configuration asks for. The engine runs no
//! other rule on such a file (`lint::run_with`).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use crate::index::query;
use crate::index::schema::NOT_COLLECTED_BBFILES;
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

/// Whether the recipe's own lines or a file it requires or includes (its static tree, not
/// classes) set `PV` with a plain assignment: the file name's version is then not where
/// `PV` comes from (meta-oe's `colord-native.bb` requires `colord.inc`, which sets
/// `PV = "1.4.7"`: lint-precision).
fn tree_sets_pv(cx: &LintContext<'_>, file: &query::FileRow) -> bool {
    let scope = query::StatementFilter {
        kinds: vec!["assign".into()],
        ..query::StatementFilter::default()
    };
    let Ok(nodes) = query::tree(cx.index, file.id) else {
        return false;
    };
    nodes
        .iter()
        .filter_map(|n| n.file.as_ref())
        .filter(|f| matches!(f.kind, FileKind::Recipe | FileKind::Include))
        .any(|f| {
            query::statements_in(cx.index, f.id, &scope)
                .unwrap_or_default()
                .iter()
                .any(|st| {
                    st.base.as_deref() == Some("PV")
                        && st.overrides.is_empty()
                        && st.flag.is_none()
                        && !st.legacy
                        && matches!(st.op.as_deref(), Some("=" | ":=" | "?=" | "??="))
                })
        })
}

/// Directory names under which a `.bb` is clearly not a recipe meant to be built.
const NOT_RECIPE_DIRS: &[&str] = &["lib", "scripts", "files", "wic", "templates"];

impl OrgRecipePath {
    /// The one finding for a `.bb` no `BBFILES` glob matches.
    fn not_collected(
        &self,
        cx: &LintContext<'_>,
        layer: &str,
        f: &query::FileRow,
        files: &[query::FileRow],
    ) -> Finding {
        let comps = components(&f.path);
        let dirs = &comps[..comps.len().saturating_sub(1)];
        // A recipe's own `<pn>/` or `<pn>-<pv>/` directory, next to a collected `.bb`.
        let in_recipe_dir = (0..dirs.len()).any(|i| {
            let parent: std::path::PathBuf = dirs[..i].iter().collect();
            files.iter().any(|o| {
                o.id != f.id
                    && o.kind == FileKind::Recipe
                    && o.path.parent() == Some(parent.as_path())
                    && o.pn.as_deref().is_some_and(|pn| {
                        dirs[i] == pn
                            || o.pv
                                .as_deref()
                                .is_some_and(|pv| dirs[i] == format!("{pn}-{pv}"))
                    })
            })
        });
        let data_place = in_recipe_dir
            || dirs
                .iter()
                .any(|d| NOT_RECIPE_DIRS.contains(&d.as_str()) || d.ends_with("-templates"));
        let globs: Vec<String> = cx
            .ws
            .layer(layer)
            .map(|l| {
                let root = l.root.to_string_lossy();
                l.bbfiles
                    .iter()
                    .map(|g| g.replacen(root.as_ref(), "${LAYERDIR}", 1))
                    .collect()
            })
            .unwrap_or_default();
        let globs = if globs.is_empty() {
            "it sets none".to_string()
        } else {
            globs.join(" ")
        };
        let file_name = comps.last().map(String::as_str).unwrap_or("");
        let pn = f.pn.as_deref().unwrap_or("<pn>");
        let (severity, advice) = if data_place {
            (
                Severity::Info,
                "it is in a directory that holds no recipes, so this is expected if it is a \
                 template or data; otherwise move it under recipes-<category>/<pn>/"
                    .to_string(),
            )
        } else {
            (
                Severity::Warning,
                format!(
                    "move it to recipes-<category>/{pn}/{file_name} or add a glob that matches \
                     it to BBFILES in conf/layer.conf"
                ),
            )
        };
        Finding {
            rule: self.id(),
            severity,
            loc: f.loc(LineSpan::line(1)),
            message: format!(
                "BitBake does not collect {}: no BBFILES glob of layer {layer} matches it \
                 (BBFILES: {globs}), so it is never parsed or built and no other check runs on \
                 it; {advice}",
                f.path.display()
            ),
        }
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
        let uncollected: HashMap<i64, String> = query::uncollected_recipe_files(cx.index)
            .map(|v| v.into_iter().map(|(f, r)| (f.id, r)).collect())
            .unwrap_or_default();
        let all_recipes: Vec<&query::FileRow> = files
            .iter()
            .filter(|f| f.kind == FileKind::Recipe)
            .collect();
        let mut recipes: Vec<&query::FileRow> = Vec::new();
        for f in all_recipes {
            match uncollected.get(&f.id).map(String::as_str) {
                None => recipes.push(f),
                Some(NOT_COLLECTED_BBFILES) => {
                    out.push(self.not_collected(cx, layer, f, &files));
                }
                Some(_) => {}
            }
        }
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
            } else if parts.len() == 1 && kind == Kind::Other && !tree_sets_pv(cx, f) {
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
