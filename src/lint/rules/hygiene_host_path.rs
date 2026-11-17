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

/// Commands whose arguments are text to print or write, not paths they open: a path among
/// them is data (`echo "d root root 0755 /var/run/nscd none" > ${D}...`), except after a
/// redirection.
const TEXT_COMMANDS: &[&str] = &[
    "echo", "printf", "bbplain", "bbnote", "bbwarn", "bberror", "bbfatal", "bbdebug",
];

/// Commands whose quoted arguments are expressions or patterns (`sed -e 's,/usr/lib,...'`,
/// `grep -q -x "/bin/sh" f`); their unquoted arguments are files and are still checked.
const PATTERN_COMMANDS: &[&str] = &["sed", "grep", "egrep", "fgrep", "awk", "gawk", "perl"];

/// Shell words that come before a command without being one.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{", "(", "time", "exec",
];

/// What is known about the shell command a word belongs to; carried over a `\` line
/// continuation.
#[derive(Debug, Clone, Default)]
struct Command {
    /// The command word (`sed`), once seen.
    name: Option<String>,
    /// Whether an `-s` option was seen (for `ln`).
    symbolic: bool,
    /// Operands (words not starting with `-`) seen after the command word.
    operands: usize,
    /// The next word is the target of a redirection (`>`, `>>`, `2>`).
    redirect: bool,
}

/// Splits a line into words at unquoted whitespace: `(byte offset, text)`.
fn words(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut start: Option<usize> = None;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c.is_whitespace() => {
                if let Some(s) = start.take() {
                    out.push((s, &line[s..i]));
                }
                continue;
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None => {}
        }
        start.get_or_insert(i);
    }
    if let Some(s) = start {
        out.push((s, &line[s..]));
    }
    out
}

/// Host paths at the start of a word in `line` (not the command word), where the path is
/// something the command reads or writes; `cmd` is the command carried over from the
/// previous line when it ended with `\`, and is left as it stands at the end of this line.
fn host_paths_in(line: &str, cmd: &mut Command) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    for (_, word) in words(line) {
        let (bare, ends_segment) = match word.strip_suffix(';') {
            Some(w) => (w, true),
            None => (word, false),
        };
        if matches!(bare, "|" | "||" | "&&" | ";" | "&") {
            *cmd = Command::default();
            continue;
        }
        if cmd.name.is_none() {
            let assignment = bare.split_once('=').is_some_and(|(n, _)| {
                !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            if !KEYWORDS.contains(&bare) && !assignment && !bare.is_empty() {
                // The command word itself: running a host tool by path is another problem.
                cmd.name = Some(bare.to_string());
            }
        } else if matches!(bare, ">" | ">>" | "1>" | "2>" | "&>" | "1>>" | "2>>") {
            cmd.redirect = true;
        } else {
            let target = std::mem::take(&mut cmd.redirect);
            let name = cmd.name.as_deref().unwrap_or("");
            let option = bare.starts_with('-');
            if option && name == "ln" && !bare.starts_with("--") && bare.contains('s') {
                cmd.symbolic = true;
            }
            if !option {
                cmd.operands += 1;
            }
            let symlink_target = name == "ln" && cmd.symbolic && !option && cmd.operands == 1;
            for (dir, instead, quoted) in paths_in_word(bare) {
                let data = !target
                    && (TEXT_COMMANDS.contains(&name)
                        || (quoted && PATTERN_COMMANDS.contains(&name))
                        || symlink_target);
                if !data {
                    out.push((dir, instead));
                }
            }
        }
        if ends_segment {
            *cmd = Command::default();
        }
    }
    out
}

/// Host directories that begin a word inside `word` (at its start, after an opening quote
/// at its start, or after whitespace inside quotes), with whether they are inside quotes.
fn paths_in_word(word: &str) -> Vec<(&'static str, &'static str, bool)> {
    let bytes = word.as_bytes();
    let mut out = Vec::new();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'/' {
            let at_start = match i.checked_sub(1).map(|j| bytes[j]) {
                None => true,
                Some(p) if p.is_ascii_whitespace() => true,
                Some(b'"' | b'\'') => i == 1,
                _ => false,
            };
            if let Some((dir, instead)) = host_dir(&word[i..]).filter(|_| at_start) {
                out.push((dir, instead, quote.is_some()));
            }
        }
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None => {}
        }
    }
    out
}

/// [`host_paths_in`] for a line on its own.
#[cfg(test)]
fn host_paths(line: &str) -> Vec<(&'static str, &'static str)> {
    host_paths_in(line, &mut Command::default())
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
            let mut cmd = Command::default();
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
                    cmd = Command::default();
                    continue;
                }
                let continued = code.trim_end().ends_with('\\');
                let code = code.trim_end().trim_end_matches('\\');
                let first = code.split_whitespace().next().unwrap_or("");
                if WRITERS.contains(&first) {
                    commands += 1;
                }
                mentions_d |= code.contains("${D}") || code.contains("$D");
                delegated |= code.contains("_do_install")
                    || code.contains("oe_runmake")
                    || code.contains("DESTDIR");
                let found = host_paths_in(code, &mut cmd);
                if !continued {
                    cmd = Command::default();
                }
                for (dir, instead) in found {
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
        // Data, not host access (B13, OE-core busybox.inc:207, glibc-package.inc:143,
        // ncurses.inc:194, help2man_1.49.3.bb:17).
        assert!(host_paths("\t\tif grep -q -x \"/bin/sh\" busybox.links.suid; then").is_empty());
        assert!(host_paths("\techo \"d root root 0755 /var/run/nscd none\" \\").is_empty());
        assert!(host_paths("\tln -s /etc/terminfo/${basedir}/${x} \\").is_empty());
        assert!(
            host_paths("\tsed -i -e \"1s:#!.*:#! /usr/bin/env perl:\" ${D}${bindir}/x").is_empty()
        );
        assert!(host_paths("\tprintf '#!/bin/sh\\n' > ${D}${bindir}/x").is_empty());
        // Still host access: a redirection target, a sed / grep file operand, the link
        // name of `ln -s`, a command after a pipe.
        assert_eq!(host_paths("\techo x > /etc/x").len(), 1);
        assert_eq!(host_paths("\tsed -i 's,a,b,' /etc/x").len(), 1);
        assert_eq!(host_paths("\tgrep -q x /etc/passwd").len(), 1);
        assert_eq!(host_paths("\tln -sf ../x /usr/bin/y").len(), 1);
        assert_eq!(host_paths("\tln /usr/bin/a ${D}/b").len(), 1);
        assert_eq!(host_paths("\techo x | tee /etc/x").len(), 1);
        assert_eq!(host_paths("\tcp \"/etc/x\" ${D}").len(), 1);
        // A continued command keeps its name.
        let mut cmd = Command::default();
        assert!(host_paths_in("\tsed -i \\", &mut cmd).is_empty());
        assert!(
            host_paths_in("\t    -e 's|#! /bin/gawk|#! ${bindir}/gawk|g' \\", &mut cmd).is_empty()
        );
        assert_eq!(host_paths_in("\t    /etc/x", &mut cmd).len(), 1);
        assert_eq!(strip_comment("a # /etc"), "a ");
        assert_eq!(strip_comment("a '#' /etc"), "a '#' /etc");
        assert_eq!(
            helpers::heredoc_delimiter("cat > x <<-'EOF'"),
            Some("EOF".into())
        );
        assert_eq!(helpers::heredoc_delimiter("cat <<< x"), None);
    }
}
