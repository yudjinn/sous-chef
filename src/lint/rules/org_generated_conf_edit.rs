//! `org/generated-conf-edit` (T9.3): a setting in a kas-generated `local.conf` or
//! `bblayers.conf` that no kas file provides.
//!
//! See docs/lint-rules.md. kas rewrites `<build>/conf/local.conf` and `bblayers.conf` on
//! every `kas checkout`, `kas shell` and `kas build`: each `local_conf_header` /
//! `bblayers_conf_header` entry under a `# <id>` comment line, in id order, then its own
//! lines (`MACHINE ??=`, `DISTRO ??=`, `BBMULTICONFIG ?=` in `local.conf`; the
//! `BBLAYERS ?=` list, `BBPATH ?=` and `BBFILES ??=` in `bblayers.conf`). A statement made
//! of other lines will be lost the next time kas runs.
//!
//! Runs only when all of these hold, because false positives are worse than misses:
//!
//! - the workspace is a kas project with known files that were read without problems
//!   (an unreadable include could hold the line), whose build directory is this file's;
//! - the build directory is marked as kas's (`CACHEDIR.TAG`), or the file has kas's own
//!   lines (`MACHINE ??=` / `DISTRO ??=` / `BBMULTICONFIG ?=` in a row; `BBLAYERS ?= " \`
//!   and `BBPATH ?= "${TOPDIR}"` / `BBFILES ??= ""`).
//!
//! A statement is kas's when every one of its lines is a line of one of the matching header
//! entries or one of kas's own lines (`MACHINE ??=` and the others with any value, since
//! `KAS_MACHINE` and the like can change it). In `bblayers.conf` the `BBLAYERS ?=` list is
//! kas's; an entry in it that expands (with `${TOPDIR}`) to no kas layer is reported on its
//! line, and an entry still holding `${` after that is not; nor is any entry when the kas
//! work directory (where kas clones the remote repositories) is a guess and the project
//! has a remote repository ([`kas::WorkDirSource::is_guess`]). The message names the kas file
//! to put the setting in: the first own file with such a header, else the last file on the
//! command line.

use std::path::{Path, PathBuf};

use crate::index::scan::written_name;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::parse::words;
use crate::types::{AssignOp, FileKind, LineSpan};
use crate::workspace::BUILD_LAYER;
use crate::workspace::kas::{self, KasProject};

pub struct OrgGeneratedConfEdit;

impl Rule for OrgGeneratedConfEdit {
    fn id(&self) -> &'static str {
        "org/generated-conf-edit"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if cx.kind != Some(FileKind::Conf) || cx.loc.layer != BUILD_LAYER {
            return;
        }
        let bblayers = match cx.loc.path.to_str() {
            Some("conf/local.conf") => false,
            Some("conf/bblayers.conf") => true,
            _ => return,
        };
        let (Some(k), Some(build)) = (cx.ws.kas.as_ref(), cx.ws.build_dir.as_ref()) else {
            return;
        };
        if !k.generates(build) || !k.problems.is_empty() {
            return;
        }
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return;
        };
        let marked = kas::has_kas_cachedir_tag(build)
            || if bblayers {
                has_bblayers_lines(source)
            } else {
                has_local_conf_lines(source)
            };
        if !marked {
            return;
        }
        let target = k
            .file_for_header(bblayers)
            .map(|f| shown(k, f))
            .unwrap_or_else(|| "your kas file".into());
        let header = if bblayers {
            "bblayers_conf_header"
        } else {
            "local_conf_header"
        };
        let conf = if bblayers {
            "bblayers.conf"
        } else {
            "local.conf"
        };
        for st in &file.statements {
            let text = helpers::statement_text(cx, st);
            if bblayers
                && let StatementKind::Assign { var, op, .. } = &st.kind
                && var.base == "BBLAYERS"
                && var.overrides.is_empty()
                && var.flag.is_none()
                && *op == AssignOp::Default
            {
                // The repositories kas clones live in its work directory; when that is a
                // guess, where their layers are is too, so their entries are not judged.
                if !(k.work_dir_source.is_guess() && k.repos.iter().any(|r| !r.own)) {
                    self.check_bblayers(cx, k, build, st.span, text, &target, out);
                }
                continue;
            }
            let from_kas = text.lines().all(|l| {
                let l = l.trim_end();
                l.is_empty()
                    || kas::is_kas_trailer_line(bblayers, l)
                    || k.header_line_source(bblayers, l).is_some()
            });
            if from_kas {
                continue;
            }
            let what = match &st.kind {
                StatementKind::Assign { var, .. } => format!("`{}`", written_name(var, text)),
                _ => format!(
                    "`{}`",
                    helpers::clip(text.lines().next().unwrap_or("").trim())
                ),
            };
            out.push(helpers::finding(
                self,
                cx,
                st.span,
                format!(
                    "{what} in {conf} comes from no kas file, and kas rewrites {conf} on every checkout, shell and build, so it will be lost; add it to {header} in {target}"
                ),
            ));
        }
    }
}

impl OrgGeneratedConfEdit {
    #[allow(clippy::too_many_arguments)]
    fn check_bblayers(
        &self,
        cx: &LintContext<'_>,
        k: &KasProject,
        build: &Path,
        span: LineSpan,
        text: &str,
        target: &str,
        out: &mut Vec<Finding>,
    ) {
        let topdir = build.display().to_string();
        for (i, line) in text.lines().enumerate() {
            for word in words::words(line)
                .into_iter()
                .map(|w| w.trim_matches(|c| c == '"' || c == '\\'))
                .filter(|w| !w.is_empty() && !w.starts_with("BBLAYERS") && *w != "?=")
            {
                let expanded = word.replace("${TOPDIR}", &topdir);
                if expanded.contains("${") || !expanded.starts_with('/') {
                    continue;
                }
                let path = normalize(Path::new(&expanded));
                if k.layer_at(&path).is_some() {
                    continue;
                }
                let line_no = span.start + u32::try_from(i).unwrap_or(0);
                out.push(helpers::finding(
                    self,
                    cx,
                    LineSpan::new(line_no, line_no),
                    format!(
                        "layer {word} in BBLAYERS is in no kas file, and kas rewrites bblayers.conf on every checkout, shell and build, so it will be dropped; add it under repos in {target}"
                    ),
                ));
            }
        }
    }
}

fn shown(k: &KasProject, f: &Path) -> String {
    f.strip_prefix(&k.root).unwrap_or(f).display().to_string()
}

/// `MACHINE ??=`, `DISTRO ??=`, `BBMULTICONFIG ?=` on three lines in a row, anywhere.
fn has_local_conf_lines(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.windows(3).any(|w| {
        w[0].starts_with("MACHINE ??= \"")
            && w[1].starts_with("DISTRO ??= \"")
            && w[2].starts_with("BBMULTICONFIG ?= \"")
    })
}

/// `BBLAYERS ?= " \`, and `BBPATH ?= "${TOPDIR}"` followed by `BBFILES ??= ""`.
fn has_bblayers_lines(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.contains(&"BBLAYERS ?= \" \\")
        && lines
            .windows(2)
            .any(|w| w[0] == "BBPATH ?= \"${TOPDIR}\"" && w[1] == "BBFILES ??= \"\"")
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}
