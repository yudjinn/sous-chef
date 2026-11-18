//! `override/plus-equals-on-default`: `VAR += ` in a conf file for a variable that is given
//! its value with `?=` elsewhere in the workspace, so the default is lost.
//!
//! See docs/lint-rules.md and bitbake-semantics.md ("Datastore"): `+=` on an unset variable
//! gives it a value (`" x"`), and a `?=` parsed later sees that value and does nothing. So
//! `DISTRO_FEATURES += "wayland"` in `local.conf` (parsed before the distro conf) leaves
//! `DISTRO_FEATURES` as just `wayland`. `:append` is applied at the end and keeps the
//! default.
//!
//! Reported for `+=`, `=+`, `.=`, `=.` on a plain name (no override, no flag) in a `.conf`
//! file when a `?=` of the same plain name is parsed **later** and nothing gave the variable
//! a value **before** (then the `?=` would have done nothing anyway). The order comes from
//! the evaluated configuration (`eval::evaluate` with no recipe): its history orders every
//! configuration file and global class. A `?=` in a recipe, bbappend, recipe class or
//! non-configuration `.inc` is always parsed after every `.conf`. A `?=` later in the same
//! file is after by line. When the order cannot be told (a `.conf` the configuration does
//! not reach, compared with another such file), nothing is reported.

use crate::eval::{self, DataStore, VarOp};
use crate::index::query::{self, StatementFilter, StatementRow};
use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParsedFile, StatementKind};
use crate::types::{AssignOp, FileKind, LineSpan, Loc};

pub struct OverridePlusEqualsOnDefault;

impl Rule for OverridePlusEqualsOnDefault {
    fn id(&self) -> &'static str {
        "override/plus-equals-on-default"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    /// Suggested: `VAR += "x"` becomes `VAR:append = " x"` (`=+` a `:prepend` with the
    /// trailing space; `.=` / `=.` the same keyword without adding a space), the line the
    /// message gives. The default is then kept, which is the rule's point; the text is also
    /// added at the end of parsing, so it now survives a later plain `VAR =` too, which needs
    /// a look. A finding in the build directory's `local.conf` is never written (not a
    /// writable layer); the fix is shown.
    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        self.check(cx, &mut found);
        found
            .into_iter()
            .filter_map(|finding| {
                let s = file
                    .statements
                    .iter()
                    .find(|s| s.span.start == finding.loc.lines.start)?;
                let StatementKind::Assign { var, op, .. } = &s.kind else {
                    return None;
                };
                let keyword = match op {
                    AssignOp::Append | AssignOp::AppendDot => "append",
                    AssignOp::Prepend | AssignOp::PrependDot => "prepend",
                    _ => return None,
                };
                let p = fix::assign_parts(source, s)?;
                let name = format!("{}:{keyword}", var.base);
                let mut edits = vec![
                    Edit::replace(p.name.clone(), name.clone()),
                    Edit::replace(p.op.clone(), "="),
                ];
                match op {
                    AssignOp::Append => edits.push(Edit::insert(p.open + 1, " ")),
                    AssignOp::Prepend => edits.push(Edit::insert(p.close, " ")),
                    _ => {}
                }
                let fix = Fix::in_file(
                    cx,
                    edits,
                    format!(
                        "`{} {}` becomes `{name} =`, which keeps the `?=` default (and is also \
                         kept by a later plain `{} =`)",
                        var.base,
                        op.as_str(),
                        var.base
                    ),
                    Safety::Suggested,
                )?;
                Some(FixedFinding { finding, fix })
            })
            .collect()
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        if cx.kind != Some(FileKind::Conf) {
            return;
        }
        // Evaluated lazily, once per file (the configuration pass itself is cached).
        let mut config: Option<Option<DataStore>> = None;
        for s in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            if !matches!(
                op,
                AssignOp::Append | AssignOp::Prepend | AssignOp::AppendDot | AssignOp::PrependDot
            ) || var.legacy_syntax
                || var.flag.is_some()
                || !var.overrides.is_empty()
            {
                continue;
            }
            let base = var.base.as_str();
            let defaults = defaults_of(cx, file, base);
            if defaults.is_empty() {
                continue;
            }
            let config = config.get_or_insert_with(|| {
                eval::evaluate(cx.index, cx.ws, &eval::Target::default()).ok()
            });
            let history: Vec<VarOp> = config
                .as_ref()
                .map(|ds| {
                    ds.history(base)
                        .into_iter()
                        .map(|(op, _)| op)
                        .filter(|op| op.var.overrides.is_empty() && !op.var.legacy_syntax)
                        .collect()
                })
                .unwrap_or_default();
            let here = Loc {
                lines: LineSpan::line(s.span.start),
                ..cx.loc.clone()
            };
            let pos = position(&history, &here);
            // Something gave the variable a value before this line: a later `?=` would have
            // done nothing anyway.
            let earlier_value = match pos {
                Some(p) => history[..p].iter().any(|op| op.op != AssignOp::WeakDefault),
                None => {
                    !history.is_empty()
                        || file.statements.iter().any(|t| {
                            t.span.start < s.span.start
                                && matches!(&t.kind, StatementKind::Assign { var: v, op: o, .. }
                                    if v.base == base && v.overrides.is_empty()
                                        && v.flag.is_none() && !v.legacy_syntax
                                        && *o != AssignOp::WeakDefault)
                        })
                }
            };
            if earlier_value {
                continue;
            }
            let Some(lost) = defaults
                .iter()
                .find(|d| parsed_after(d, &here, pos, &history))
            else {
                continue;
            };
            let fix_op = match op {
                AssignOp::Append | AssignOp::AppendDot => "append",
                _ => "prepend",
            };
            let v = helpers::short_value(value, false);
            // `+=` / `=+` add the space; `.=` / `=.` keep whatever the value has.
            let v = match op {
                AssignOp::Append => format!(" {v}"),
                AssignOp::Prepend => format!("{v} "),
                AssignOp::AppendDot if value.starts_with(char::is_whitespace) => format!(" {v}"),
                AssignOp::PrependDot if value.ends_with(char::is_whitespace) => format!("{v} "),
                _ => v,
            };
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`{base} {}` gives {base} a value before the `?=` default at {} is parsed, \
                     so that default is lost; write `{base}:{fix_op} = \"{v}\"`",
                    op.as_str(),
                    lost
                ),
            ));
        }
    }
}

/// Every `{base} ?= ...` on the plain name: in this file (from its parse, so a file the
/// index does not hold counts too) and in the other files of the workspace (index).
fn defaults_of(cx: &LintContext<'_>, file: &ParsedFile, base: &str) -> Vec<Loc> {
    let mut out: Vec<Loc> = file
        .statements
        .iter()
        .filter(|s| {
            matches!(&s.kind, StatementKind::Assign { var, op: AssignOp::Default, .. }
                if var.base == base && var.overrides.is_empty() && var.flag.is_none()
                    && !var.legacy_syntax)
        })
        .map(|s| Loc {
            lines: LineSpan::line(s.span.start),
            ..cx.loc.clone()
        })
        .collect();
    let scope = StatementFilter {
        kinds: vec!["assign".into()],
        ..StatementFilter::default()
    };
    out.extend(
        query::statements_by_base(cx.index, base, &scope)
            .unwrap_or_default()
            .into_iter()
            .filter(|r: &StatementRow| {
                r.op.as_deref() == Some("?=")
                    && r.overrides.is_empty()
                    && r.flag.is_none()
                    && !r.legacy
                    && !(r.loc.layer == cx.loc.layer && r.loc.path == cx.loc.path)
            })
            .map(|r| Loc {
                lines: LineSpan::line(r.loc.lines.start),
                ..r.loc
            }),
    );
    out
}

fn same_line(a: &Loc, b: &Loc) -> bool {
    a.layer == b.layer && a.path == b.path && a.lines.start == b.lines.start
}

fn position(history: &[VarOp], at: &Loc) -> Option<usize> {
    history.iter().position(|op| same_line(&op.loc, at))
}

/// Whether the `?=` statement `d` is parsed after the statement at `here` (`pos` is the
/// latter's position in the configuration history, if it is there).
fn parsed_after(d: &Loc, here: &Loc, pos: Option<usize>, history: &[VarOp]) -> bool {
    if d.layer == here.layer && d.path == here.path {
        return d.lines.start > here.lines.start;
    }
    let kind = FileKind::from_path(&d.path);
    let in_conf_dir = d.path.components().any(|c| c.as_os_str() == "conf");
    match position(history, d) {
        Some(dp) => match pos {
            Some(p) => dp > p,
            // This file is not reached by the configuration; a global class still comes
            // after every configuration file.
            None => kind == Some(FileKind::Class),
        },
        None => match kind {
            Some(FileKind::Recipe | FileKind::Append | FileKind::Class) => true,
            Some(FileKind::Include) => !in_conf_dir,
            _ => false,
        },
    }
}
