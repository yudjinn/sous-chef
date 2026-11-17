//! `override/weak-after-default`: `??=` for a variable that already has `?=` or `=` earlier
//! in the same file.
//!
//! See docs/lint-rules.md. A `??=` default is used only when the variable has no plain value
//! at the end of parsing (bitbake-semantics.md, "Datastore"), so after `=`, `:=` or a `?=`
//! that took effect it never applies. Names are compared exactly as written (overrides and
//! flag included); an `unset` of the variable in between starts over.

use std::collections::HashMap;

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{AssignOp, LineSpan, VarRef};

pub struct OverrideWeakAfterDefault;

impl Rule for OverrideWeakAfterDefault {
    fn id(&self) -> &'static str {
        "override/weak-after-default"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        // name as written -> (line, operator) of the first plain setting.
        let mut set: HashMap<String, (u32, AssignOp)> = HashMap::new();
        for s in &file.statements {
            match &s.kind {
                StatementKind::Unset { name, flag } => {
                    match flag {
                        Some(f) => {
                            set.remove(&format!("{name}[{f}]"));
                        }
                        None => set.retain(|k, _| k != name && !k.starts_with(&format!("{name}["))),
                    }
                    continue;
                }
                StatementKind::Assign { var, op, .. } if !var.legacy_syntax => {
                    let key = key(var);
                    match op {
                        AssignOp::Set | AssignOp::Default | AssignOp::Immediate => {
                            set.entry(key).or_insert((s.span.start, *op));
                        }
                        AssignOp::WeakDefault => {
                            if let Some((line, first)) = set.get(&key) {
                                out.push(helpers::finding(
                                    self,
                                    cx,
                                    LineSpan::line(s.span.start),
                                    format!(
                                        "`{key} ??=` has no effect: line {line} already gives \
                                         {key} a value with `{}`, and a `??=` default applies \
                                         only to a variable with no value; remove it",
                                        first.as_str()
                                    ),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn key(var: &VarRef) -> String {
    let mut s = var.base.clone();
    for o in &var.overrides {
        s.push(':');
        s.push_str(o);
    }
    if let Some(f) = &var.flag {
        s.push('[');
        s.push_str(f);
        s.push(']');
    }
    s
}
