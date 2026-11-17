//! `hygiene/variable-order`: statements of a recipe out of the OE style guide order.
//!
//! See docs/lint-rules.md. Off by default ([`Rule::default_enabled`] is false): it runs only
//! when selected by its id, by the alias `style/variable-order` or by the alias group
//! `style`.
//!
//! Only `.bb` files. Each statement that belongs to one of the groups below gets the group's
//! rank; others (unknown variables, anonymous python, `def`, `addhandler`, includes) are
//! ignored. The first statement whose rank is lower than that of a statement before it is
//! reported, once per file, naming the statement it should precede.
//!
//! Order (the catalogue's): description block, license, dependencies, `SRC_URI`, `S`,
//! `inherit`, `PACKAGECONFIG` and build options, tasks, packaging.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{FileKind, LineSpan};

pub struct HygieneVariableOrder;

const GROUPS: &[(&str, &[&str])] = &[
    (
        "the description block",
        &[
            "SUMMARY",
            "DESCRIPTION",
            "AUTHOR",
            "HOMEPAGE",
            "BUGTRACKER",
            "SECTION",
        ],
    ),
    ("the license", &["LICENSE", "LIC_FILES_CHKSUM"]),
    ("the dependencies", &["DEPENDS", "PROVIDES"]),
    ("SRC_URI", &["SRC_URI", "SRCREV"]),
    ("S", &["S", "B"]),
    ("inherit", &[]),
    (
        "PACKAGECONFIG and build options",
        &[
            "PACKAGECONFIG",
            "EXTRA_OECONF",
            "EXTRA_OECMAKE",
            "EXTRA_OEMESON",
            "EXTRA_OEMAKE",
            "EXTRA_OESCONS",
        ],
    ),
    ("the tasks", &[]),
    (
        "packaging",
        &[
            "PACKAGES",
            "PACKAGES_DYNAMIC",
            "FILES",
            "RDEPENDS",
            "RRECOMMENDS",
            "RSUGGESTS",
            "RPROVIDES",
            "RCONFLICTS",
            "RREPLACES",
            "CONFFILES",
            "ALLOW_EMPTY",
            "INSANE_SKIP",
            "BBCLASSEXTEND",
        ],
    ),
];

const INHERIT: usize = 5;
const TASKS: usize = 7;

fn var_rank(base: &str) -> Option<usize> {
    let base = if base.starts_with("SRCREV_") && base != "SRCREV_FORMAT" {
        "SRCREV"
    } else {
        base
    };
    GROUPS.iter().position(|(_, vars)| vars.contains(&base))
}

fn group_name(r: usize) -> &'static str {
    GROUPS.get(r).map_or("", |g| g.0)
}

fn rank(kind: &StatementKind) -> Option<(usize, String)> {
    match kind {
        StatementKind::Assign { var, .. } => var_rank(&var.base).map(|r| (r, var.base.clone())),
        StatementKind::Inherit { .. } => Some((INHERIT, "inherit".into())),
        StatementKind::Function {
            name: Some(name), ..
        } if name.base.starts_with("do_") => Some((TASKS, name.base.clone())),
        StatementKind::Function {
            name: Some(name), ..
        } if name.base.starts_with("pkg_") => Some((GROUPS.len() - 1, name.base.clone())),
        StatementKind::AddTask { .. } | StatementKind::DelTask { .. } => {
            Some((TASKS, "addtask".into()))
        }
        _ => None,
    }
}

impl Rule for HygieneVariableOrder {
    fn id(&self) -> &'static str {
        "hygiene/variable-order"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        if cx.kind != Some(FileKind::Recipe) {
            return;
        }
        // The highest-ranked statement so far: (rank, what, line).
        let mut max: Option<(usize, String, u32)> = None;
        for s in &file.statements {
            let Some((r, what)) = rank(&s.kind) else {
                continue;
            };
            match &max {
                Some((m, before, line)) if r < *m => {
                    out.push(helpers::finding(
                        self,
                        cx,
                        LineSpan::line(s.span.start),
                        format!(
                            "`{what}` ({}) comes after `{before}` ({}, line {line}); the OE \
                             style guide order is description, license, dependencies, SRC_URI, \
                             S, inherit, PACKAGECONFIG, tasks, packaging: move it up",
                            group_name(r),
                            group_name(*m)
                        ),
                    ));
                    return;
                }
                Some((m, _, _)) if r <= *m => {}
                _ => max = Some((r, what, s.span.start)),
            }
        }
    }
}
