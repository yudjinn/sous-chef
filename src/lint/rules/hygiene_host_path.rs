//! `hygiene/host-path`: absolute host paths in task bodies; `do_install` without `${D}`.
//!
//! See docs/lint-rules.md. Only shell functions whose name is a task (`do_<task>`, with any
//! override such as `:append` or `:class-target`) are scanned; `pkg_postinst` and friends
//! run on the target (or with `$D` set) where absolute paths are right.
//!
//! A path is reported only where it is unambiguously a host path: a word that starts with
//! one of the host directories below (`/usr`, `/etc`, `/home`, ...), at the start of a word
//! (after whitespace, or as the first character inside quotes that start a word), and not
//! the first word of the line (running a host tool by path is a different problem). Not
//! scanned: comments, here-document bodies (they usually write a file's content, such as a
//! config file under `${D}`), and words after `=` (`--prefix=/usr` is an argument value; a
//! configure-time default the agent may well want). `/dev`, `/proc`, `/sys` and `/tmp` are
//! never reported. A path after `${D}`, `$D`, `${S}` or any other variable is not at the
//! start of a word, so it is not reported.
//!
//! `do_install` (a whole definition, not `:append` / `:prepend`) that never mentions `${D}`
//! or `$D` is reported only when it runs a command that puts files somewhere (`install`,
//! `cp`, `mkdir`, `ln`, `mv`, `rsync`, `tar`, `touch`, `cat`, `echo` as the first word of a
//! line), and not when it calls something that installs on its behalf (a `*_do_install`
//! function from a class, `oe_runmake`, or anything setting `DESTDIR`). A body that only
//! calls other functions, or is `:` or empty, is not reported.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{Lang, StatementKind};
use crate::types::LineSpan;

pub struct HygieneHostPath;

/// Host directories and what to write instead.
const HOST_DIRS: &[(&str, &str)] = &[
    ("/usr/lib", "${libdir}"),
    ("/usr/bin", "${bindir}"),
    ("/usr/sbin", "${sbindir}"),
    ("/usr/share", "${datadir}"),
    ("/usr/include", "${includedir}"),
    ("/usr/libexec", "${libexecdir}"),
    ("/usr/local", "${prefix}"),
    ("/usr", "${prefix}"),
    ("/etc", "${sysconfdir}"),
    ("/var", "${localstatedir}"),
    ("/bin", "${base_bindir}"),
    ("/sbin", "${base_sbindir}"),
    ("/lib64", "${base_libdir}"),
    ("/lib", "${base_libdir}"),
    ("/opt", "${D}/opt"),
    ("/home", "${S}, ${B} or ${WORKDIR}"),
    ("/root", "${S}, ${B} or ${WORKDIR}"),
    ("/srv", "${D}/srv"),
];

/// Commands that put files somewhere: a `do_install` that runs one of them without `${D}`
/// is reported. A body that only calls other functions is not (they may use `${D}`).
const WRITERS: &[&str] = &[
    "install", "cp", "mkdir", "ln", "mv", "rsync", "tar", "touch", "cat", "echo",
];

/// The host directory `word` starts with (exactly, or followed by `/`).
fn host_dir(word: &str) -> Option<(&'static str, &'static str)> {
    HOST_DIRS.iter().copied().find(|(dir, _)| {
        word.strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The part of a shell line before an unquoted comment.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev_space => return &line[..i],
            None => {}
        }
        prev_space = c.is_whitespace();
    }
    line
}

/// Host paths at the start of a word in `line` (not the first word).
fn host_paths(line: &str) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let first_word_end = line
        .trim_start()
        .find(char::is_whitespace)
        .map(|n| n + (line.len() - line.trim_start().len()))
        .unwrap_or(line.len());
    for (i, _) in line.match_indices('/') {
        if i < first_word_end {
            continue;
        }
        let before = i.checked_sub(1).map(|j| bytes[j]);
        let word_start = match before {
            None => true,
            Some(b) if b.is_ascii_whitespace() => true,
            Some(b'"' | b'\'') => i
                .checked_sub(2)
                .map(|j| bytes[j])
                .is_none_or(|b| b.is_ascii_whitespace()),
            _ => false,
        };
        if !word_start {
            continue;
        }
        if let Some(found) = host_dir(&line[i..]) {
            out.push(found);
        }
    }
    out
}

/// Whether a task function name is a task (`do_x`, `do_x:append`, ...).
fn task_name(full: &str) -> Option<&str> {
    let base = full.split(':').next().unwrap_or(full);
    base.starts_with("do_").then_some(base)
}

impl Rule for HygieneHostPath {
    fn id(&self) -> &'static str {
        "hygiene/host-path"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return;
        };
        let lines: Vec<&str> = source.lines().collect();
        for s in &file.statements {
            let StatementKind::Function {
                name: Some(name),
                lang: Lang::Shell,
                body,
                ..
            } = &s.kind
            else {
                continue;
            };
            let Some(task) = task_name(&name.base) else {
                continue;
            };
            let mut heredoc: Option<String> = None;
            let mut commands = 0usize;
            let mut mentions_d = false;
            let mut delegated = false;
            for n in body.start..=body.end {
                let Some(raw) = lines.get((n as usize).wrapping_sub(1)) else {
                    continue;
                };
                if let Some(delim) = &heredoc {
                    if raw.trim() == delim {
                        heredoc = None;
                    }
                    continue;
                }
                let code = strip_comment(raw);
                if code.trim().is_empty() {
                    continue;
                }
                let first = code.split_whitespace().next().unwrap_or("");
                if WRITERS.contains(&first) {
                    commands += 1;
                }
                mentions_d |= code.contains("${D}") || code.contains("$D");
                delegated |= code.contains("_do_install")
                    || code.contains("oe_runmake")
                    || code.contains("DESTDIR");
                for (dir, instead) in host_paths(code) {
                    out.push(helpers::finding(
                        self,
                        cx,
                        LineSpan::line(n),
                        format!(
                            "absolute host path `{dir}` in {task}: it names the build \
                             host, not the target; write `${{D}}{instead}` when installing \
                             (or `{instead}` / `${{STAGING_DIR_HOST}}{instead}` when reading)"
                        ),
                    ));
                }
                heredoc = helpers::heredoc_delimiter(code);
            }
            let whole = name.base == "do_install"
                && !name
                    .overrides
                    .iter()
                    .any(|o| matches!(o.as_str(), "append" | "prepend" | "remove"));
            if whole && commands > 0 && !mentions_d && !delegated {
                out.push(helpers::finding(
                    self,
                    cx,
                    LineSpan::line(s.span.start),
                    "do_install never references ${D}: files installed elsewhere do not reach \
                     the package; install into ${D} (e.g. `install -d ${D}${bindir}`)",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(host_paths("\tinstall -m 0644 x /etc/foo").len(), 1);
        assert_eq!(host_paths("\tinstall -m 0644 x \"/usr/lib/foo\"").len(), 1);
        assert!(host_paths("\tinstall -m 0644 x ${D}/usr/lib").is_empty());
        assert!(host_paths("\t./configure --prefix=/usr").is_empty());
        assert!(host_paths("\tsed -i 's,/usr/lib,${libdir},' x").is_empty());
        assert!(host_paths("\t/usr/bin/env python3 x").is_empty());
        assert!(host_paths("\techo x > /dev/null").is_empty());
        assert!(host_paths("\tcp a /usrx/b").is_empty());
        assert_eq!(strip_comment("a # /etc"), "a ");
        assert_eq!(strip_comment("a '#' /etc"), "a '#' /etc");
        assert_eq!(
            helpers::heredoc_delimiter("cat > x <<-'EOF'"),
            Some("EOF".into())
        );
        assert_eq!(helpers::heredoc_delimiter("cat <<< x"), None);
    }
}
