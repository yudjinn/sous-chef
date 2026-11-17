//! `org/policy-in-recipe`: A recipe or bbappend sets distro or machine policy.
//!
//! See docs/lint-rules.md. Reported in `.bb` and `.bbappend` files, for assignments (any
//! operator, any override) to:
//!
//! - `DISTRO_FEATURES`, `MACHINE_FEATURES`: a recipe's copy changes only that recipe, so it
//!   and the rest of the build disagree about the feature set;
//! - `PREFERRED_PROVIDER_*`: providers are chosen from the configuration before recipes are
//!   parsed (`cooker.py` `handlePrefProviders` and `providers.findBestProvider` read the
//!   configuration datastore), so the recipe's value is not used for that choice;
//! - `IMAGE_INSTALL`, except in image recipes: only image recipes read it. A recipe counts as
//!   an image when it (or, for a bbappend, a recipe it applies to) inherits `image` or
//!   `core-image`, directly or through its static include/inherit tree, or a class whose name
//!   ends in `-image`. When that tree has an inherit that cannot be resolved statically, the
//!   rule cannot tell and does not report `IMAGE_INSTALL`.

use crate::index::query;
use crate::index::scan::written_name;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::FileKind;

pub struct OrgPolicyInRecipe;

/// Whether an inherited class name makes a recipe an image.
fn image_class(name: &str) -> bool {
    name == "image" || name == "core-image" || name.ends_with("-image")
}

/// What the static trees say about the file being an image recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Image {
    Yes,
    No,
    Unknown,
}

/// Image status from the index: the file's own tree, and for a bbappend the trees of the
/// recipes it applies to. `None` when the file is not indexed.
fn image_from_index(cx: &LintContext<'_>) -> Option<Image> {
    let row = query::file_by_path(cx.index, &cx.loc.layer, &cx.loc.path).ok()??;
    let mut roots = vec![row.id];
    if row.kind == FileKind::Append {
        for e in query::edges_from(cx.index, row.id).ok()? {
            if e.kind == "bbappend"
                && let Some(t) = e.target
            {
                roots.push(t.id);
            }
        }
    }
    // An append that applies to nothing tells nothing about the recipe.
    let mut unknown = row.kind == FileKind::Append && roots.len() == 1;
    for root in roots {
        for node in query::tree(cx.index, root).ok()? {
            match &node.file {
                Some(f) if node.kind == "inherit" => {
                    let stem = f.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if image_class(stem) {
                        return Some(Image::Yes);
                    }
                }
                Some(_) => {}
                None => unknown = true,
            }
        }
    }
    Some(if unknown { Image::Unknown } else { Image::No })
}

/// Image status from the file's own `inherit` statements (file not indexed).
fn image_from_text(cx: &LintContext<'_>) -> Image {
    let Some(file) = cx.file else {
        return Image::Unknown;
    };
    let mut unknown = cx.kind == Some(FileKind::Append);
    for st in &file.statements {
        if let StatementKind::Inherit { classes, .. } = &st.kind {
            if classes.contains("${") {
                unknown = true;
            }
            if classes.split_whitespace().any(image_class) {
                return Image::Yes;
            }
        }
        if let StatementKind::Include { .. } = &st.kind {
            unknown = true;
        }
    }
    if unknown { Image::Unknown } else { Image::No }
}

impl Rule for OrgPolicyInRecipe {
    fn id(&self) -> &'static str {
        "org/policy-in-recipe"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if !matches!(cx.kind, Some(FileKind::Recipe | FileKind::Append)) {
            return;
        }
        let Some(file) = cx.file else { return };
        let mut image: Option<Image> = None;
        for st in &file.statements {
            let StatementKind::Assign { var, .. } = &st.kind else {
                continue;
            };
            let name = written_name(var, helpers::statement_text(cx, st));
            let message = match var.base.as_str() {
                "DISTRO_FEATURES" => format!(
                    "`{name}` in a recipe changes the distro features for this recipe only, \
                     so it and the rest of the build disagree; set DISTRO_FEATURES in your \
                     distro conf (or local.conf) and test it here with \
                     bb.utils.contains / REQUIRED_DISTRO_FEATURES"
                ),
                "MACHINE_FEATURES" => format!(
                    "`{name}` in a recipe changes the machine features for this recipe only; \
                     set MACHINE_FEATURES in the machine conf and test it here with \
                     bb.utils.contains"
                ),
                "IMAGE_INSTALL" => {
                    let status = *image.get_or_insert_with(|| {
                        image_from_index(cx).unwrap_or_else(|| image_from_text(cx))
                    });
                    if status != Image::No {
                        continue;
                    }
                    format!(
                        "`{name}` in a recipe that is not an image: only image recipes read \
                         IMAGE_INSTALL; add the package to IMAGE_INSTALL in your image recipe \
                         (or a packagegroup it installs)"
                    )
                }
                b if b.starts_with("PREFERRED_PROVIDER_") && var.flag.is_none() => format!(
                    "`{name}` in a recipe: providers are chosen from the configuration before \
                     recipes are parsed, so this does not select a provider; set it in your \
                     distro or machine conf"
                ),
                _ => continue,
            };
            out.push(helpers::finding(self, cx, st.span, message));
        }
    }
}
