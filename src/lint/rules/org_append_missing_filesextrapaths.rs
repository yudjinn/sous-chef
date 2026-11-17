//! `org/append-missing-filesextrapaths`: A bbappend adds `file://` to `SRC_URI` but never
//! extends `FILESEXTRAPATHS`.
//!
//! See docs/lint-rules.md. In a `.bbappend`, any assignment to `SRC_URI` (any operator or
//! override, no flag) whose value has a `file://` entry, when no statement of the append, and
//! of no file it requires or includes (index tree), touches `FILESEXTRAPATHS`. `file://`
//! entries are looked up along `FILESPATH`, which without `FILESEXTRAPATHS` holds only the
//! recipe's own directories (bitbake-semantics.md, "Files and patches").
//!
//! Severity: **error** when a named file exists below the append's directory (so it is
//! meant to come from this layer and will not be found); **warning** when none does (the
//! file may come from the recipe's own `files/` directory, which needs no extra path).
//! Entries whose name needs expansion (`${...}`) are ignored.

use std::fs;
use std::path::{Path, PathBuf};

use crate::index::query;
use crate::index::scan::written_name;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{FileKind, LineSpan};

pub struct OrgAppendMissingFilesextrapaths;

/// The `file://` names in a `SRC_URI` value that need no expansion.
fn file_names(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("file://"))
        .map(|w| w.split(';').next().unwrap_or(w).to_string())
        .filter(|n| !n.is_empty() && !n.contains("${"))
        .collect()
}

/// A file below `dir` (at most four levels) whose path ends with `name`: the directory
/// `FILESEXTRAPATHS` must name, as `${THISDIR}` plus a suffix (`/hello`, or empty for a file
/// right next to the append). Override subdirectories below it are found by `FILESPATH`
/// itself, so only the first component counts.
fn find_below(dir: &Path, name: &str) -> Option<String> {
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            if !p.ends_with(name) {
                continue;
            }
            let Ok(rel) = p.strip_prefix(dir) else {
                continue;
            };
            let below = rel.components().count() > Path::new(name).components().count();
            return Some(match rel.components().next() {
                Some(first) if below => format!("/{}", first.as_os_str().to_string_lossy()),
                _ => String::new(),
            });
        }
    }
    None
}

/// Whether `FILESEXTRAPATHS` is touched in a file the append requires or includes.
fn extended_in_includes(cx: &LintContext<'_>) -> bool {
    let Ok(Some(row)) = query::file_by_path(cx.index, &cx.loc.layer, &cx.loc.path) else {
        return false;
    };
    let Ok(tree) = query::tree(cx.index, row.id) else {
        return false;
    };
    tree.iter()
        .skip(1)
        .filter_map(|n| n.file.as_ref())
        .any(|f| {
            query::statements_in(cx.index, f.id, &query::StatementFilter::default())
                .map(|s| {
                    s.iter()
                        .any(|s| s.base.as_deref() == Some("FILESEXTRAPATHS"))
                })
                .unwrap_or(false)
        })
}

impl Rule for OrgAppendMissingFilesextrapaths {
    fn id(&self) -> &'static str {
        "org/append-missing-filesextrapaths"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if cx.kind != Some(FileKind::Append) {
            return;
        }
        let Some(file) = cx.file else { return };
        let mut uses: Vec<(LineSpan, String, Vec<String>)> = Vec::new();
        for st in &file.statements {
            match &st.kind {
                StatementKind::Assign { var, .. }
                | StatementKind::Function {
                    name: Some(var), ..
                } if var.base == "FILESEXTRAPATHS" => {
                    return;
                }
                StatementKind::Assign { var, value, .. }
                    if var.base == "SRC_URI" && var.flag.is_none() =>
                {
                    let names = file_names(value);
                    if !names.is_empty() {
                        let name = written_name(var, helpers::statement_text(cx, st));
                        uses.push((st.span, name, names));
                    }
                }
                StatementKind::Unset { name, .. } if name.starts_with("FILESEXTRAPATHS") => {
                    return;
                }
                _ => {}
            }
        }
        if uses.is_empty() || extended_in_includes(cx) {
            return;
        }
        let dir = cx
            .ws
            .abs_path(&cx.loc)
            .and_then(|p| p.parent().map(Path::to_path_buf));
        for (span, name, files) in uses {
            let found = dir.as_ref().and_then(|d| {
                files
                    .iter()
                    .find_map(|f| find_below(d, f).map(|sub| (f.clone(), sub)))
            });
            let f = match found {
                Some((file, sub)) => helpers::finding(
                    self,
                    cx,
                    span,
                    format!(
                        "`{name}` adds file://{file}, which is in ${{THISDIR}}{sub} next to \
                         this bbappend, but the append never extends FILESEXTRAPATHS, so BitBake \
                         looks only in the recipe's own directories; add \
                         `FILESEXTRAPATHS:prepend := \"${{THISDIR}}{sub}:\"`"
                    ),
                ),
                None => {
                    let mut f = helpers::finding(
                        self,
                        cx,
                        span,
                        format!(
                            "`{name}` adds file://{} but the append never extends \
                             FILESEXTRAPATHS, so the file is looked up only in the recipe's own \
                             directories; if it is in this layer, add \
                             `FILESEXTRAPATHS:prepend := \"${{THISDIR}}/${{PN}}:\"`",
                            files.join(", file://")
                        ),
                    );
                    f.severity = Severity::Warning;
                    f
                }
            };
            out.push(f);
        }
    }
}
