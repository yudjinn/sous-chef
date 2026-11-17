//! `hygiene/tabs`: mixed tab and space indentation in shell bodies; python bodies not
//! indented with 4 spaces.
//!
//! See docs/lint-rules.md. At most one finding per function, on the first line that
//! deviates:
//!
//! - **Shell** function: the first indented line sets the style (a tab or a space as its
//!   first character); the first later line whose indentation starts with the other one is
//!   reported. Only the first character counts, so tab indentation followed by spaces for
//!   alignment is fine. Here-document bodies are skipped (their indentation is content).
//! - **Python** function or `def`: a tab anywhere in the indentation is reported; otherwise
//!   the first indented code line (not blank, not a comment) must be indented by exactly 4
//!   spaces. Deeper lines are not checked (continuation lines align freely).

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{Lang, StatementKind};
use crate::types::LineSpan;

pub struct HygieneTabs;

fn indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn shell_finding(lines: &[&str], body: LineSpan) -> Option<(u32, String)> {
    let mut style: Option<char> = None;
    let mut heredoc: Option<String> = None;
    for n in body.start..=body.end {
        let line = lines.get((n as usize).wrapping_sub(1))?;
        if let Some(d) = &heredoc {
            if line.trim() == d {
                heredoc = None;
            }
            continue;
        }
        heredoc = helpers::heredoc_delimiter(line);
        if line.trim().is_empty() {
            continue;
        }
        let Some(first) = indentation(line).chars().next() else {
            continue;
        };
        match style {
            None => style = Some(first),
            Some(s) if s != first => {
                let (was, now) = if s == '\t' {
                    ("tabs", "spaces")
                } else {
                    ("spaces", "tabs")
                };
                return Some((
                    n,
                    format!(
                        "shell body indented with {was} above and {now} here; indent the whole \
                         body the same way (the OE style guide uses tabs in shell functions)"
                    ),
                ));
            }
            Some(_) => {}
        }
    }
    None
}

fn python_finding(lines: &[&str], body: LineSpan) -> Option<(u32, String)> {
    let mut first_checked = false;
    for n in body.start..=body.end {
        let line = lines.get((n as usize).wrapping_sub(1))?;
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let ind = indentation(line);
        if ind.contains('\t') {
            return Some((
                n,
                "tab in python indentation; indent python with 4 spaces per level".into(),
            ));
        }
        if !first_checked && !trimmed.starts_with('#') && !ind.is_empty() {
            first_checked = true;
            if ind.len() != 4 {
                return Some((
                    n,
                    format!(
                        "python body indented with {} spaces; indent python with 4 spaces per \
                         level",
                        ind.len()
                    ),
                ));
            }
        }
    }
    None
}

impl Rule for HygieneTabs {
    fn id(&self) -> &'static str {
        "hygiene/tabs"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return;
        };
        let lines: Vec<&str> = source.lines().collect();
        for s in &file.statements {
            let found = match &s.kind {
                StatementKind::Function {
                    lang: Lang::Shell,
                    body,
                    ..
                } => shell_finding(&lines, *body),
                StatementKind::Function {
                    lang: Lang::Python,
                    body,
                    ..
                }
                | StatementKind::PyDef { body, .. } => python_finding(&lines, *body),
                _ => None,
            };
            if let Some((line, msg)) = found {
                out.push(helpers::finding(self, cx, LineSpan::line(line), msg));
            }
        }
    }
}
