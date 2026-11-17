//! Hook handler for a `grep -n` / `rg -n` run through Bash: the same index context the Grep
//! handler adds (`grep_read::annotate_hits`), as `additionalContext`. The tool output is
//! never changed.
//!
//! Why: on Linux and macOS Claude Code has no Grep tool by default (tools reference, checked
//! for 2.1.326, recorded in `plugin/README.md`); the model searches with `grep` or `rg`
//! through Bash, so the Grep annotation would never run in a default session.
//!
//! **What is recognised** ([`search_command`]), and nothing else: one simple command (no
//! pipe, list, redirection other than a trailing `2>/dev/null`, substitution, expansion in
//! double quotes, or variable assignment), whose program is `grep` or `rg` (by file name),
//! with line numbers asked for explicitly (`-n` / `--line-number`) and only options that
//! keep the `<path>:<line>:<text>` shape (a fixed list per program). `rg` without `-n` is
//! left alone: ripgrep 15.2.0's `--help` says line numbers are on by default only when
//! stdout is a tty, and are off otherwise, so its output is `<path>:<text>` and cannot be
//! read safely. GNU grep 3.12 and ugrep 7.8.4 (`--help`) print the file name by default
//! when more than one file is searched, so `grep -rn` over a directory gives
//! `<path>:<line>:<text>`; a one-file `grep -n` prints `<line>:<text>`, which this handler
//! skips (it does not guess the file).
//!
//! **Lines**: `stdout` is read with the Grep handler's parser (`grep_read::ContentParser`,
//! relative paths against the event's `cwd`): a line is a hit only when exactly one split
//! names an existing Yocto file; context lines (`-A` / `-B` / `-C`, `<path>-<line>-<text>`)
//! and anything that does not parse are skipped. At most `grep_read::MAX_HITS` hits are
//! looked at; the context says when that capped the output. Files outside the workspace,
//! not indexed or changed since indexing are skipped (`Env::fresh_file`).
//!
//! The `[hooks]` switch for this handler is `grep_read` (it is the Grep annotation), not
//! `bash` (which switches the BitBake digest).

use std::path::Path;

use serde_json::Value;

use super::grep_read::{ContentParser, annotate_hits};
use super::{Ctx, Event, Handler, POST_TOOL_USE, Response};
use crate::error::Result;

/// `PostToolUse` on `Bash` for a `grep -n` / `rg -n` command.
pub struct BashGrep;

impl Handler for BashGrep {
    fn name(&self) -> &'static str {
        "bash_grep"
    }

    fn switch(&self) -> &'static str {
        "grep_read"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Bash"]
    }

    fn handle(&self, cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>> {
        let Some(command) = ev.input_str("command") else {
            return Ok(None);
        };
        let Some(resp) = ev.tool_response.as_object() else {
            return Ok(None);
        };
        if resp.get("isImage").and_then(Value::as_bool) == Some(true)
            || search_command(command).is_none()
        {
            return Ok(None);
        }
        let Some(cwd) = ev.cwd.clone() else {
            return Ok(None);
        };
        let stdout = resp.get("stdout").and_then(Value::as_str).unwrap_or("");
        if stdout.is_empty() {
            return Ok(None);
        }
        let is_file = |p: &Path| p.is_file();
        let parser = ContentParser::new(vec![cwd], None, &is_file);
        let hits = parser.hits(stdout);
        if hits.hits.is_empty() {
            return Ok(None);
        }
        cx.note(format!(
            "{} hits{}",
            hits.hits.len(),
            if hits.unread > 0 {
                format!(", {} lines not read (cap)", hits.unread)
            } else {
                String::new()
            }
        ));
        let mut env = cx.env()?;
        Ok(annotate_hits(&mut env, &hits)?.map(Response::context))
    }
}

/// The search programs recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Grep,
    Rg,
}

/// `Some(program)` when `command` is a search this handler reads (see the module
/// documentation); `None` for anything it does not confidently recognise.
pub fn search_command(command: &str) -> Option<Program> {
    let words = shell_words(command)?;
    let (first, args) = words.split_first()?;
    let name = Path::new(first).file_name()?.to_str()?;
    let program = match name {
        "grep" => Program::Grep,
        "rg" => Program::Rg,
        _ => return None,
    };
    let spec = match program {
        Program::Grep => &GREP,
        Program::Rg => &RG,
    };
    let mut numbered = false;
    let mut pattern_given = false;
    let mut operands = 0usize;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if a == "--" {
            operands += args.len() - i;
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (key, value) = match long.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (long, None),
            };
            if spec.long_flags.contains(&key) && value.is_none() {
                if key == "line-number" {
                    numbered = true;
                }
                continue;
            }
            let value = if spec.long_values.contains(&key) {
                match value {
                    Some(v) => v,
                    None => {
                        let v = args.get(i)?;
                        i += 1;
                        v.as_str()
                    }
                }
            } else if spec.long_optional_values.contains(&key) {
                value?
            } else {
                return None;
            };
            if spec
                .long_bad_values
                .iter()
                .any(|(bk, bv)| *bk == key && *bv == value)
            {
                return None;
            }
            if key == "regexp" || key == "file" {
                pattern_given = true;
            }
            continue;
        }
        if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.is_empty()) {
            let chars: Vec<char> = cluster.chars().collect();
            let mut j = 0;
            while let Some(&c) = chars.get(j) {
                j += 1;
                if spec.short_flags.contains(c) {
                    if c == 'n' {
                        numbered = true;
                    }
                    continue;
                }
                if spec.short_values.contains(c) {
                    if j == chars.len() {
                        args.get(i)?;
                        i += 1;
                    }
                    if c == 'e' || c == 'f' {
                        pattern_given = true;
                    }
                    break;
                }
                return None;
            }
            continue;
        }
        operands += 1;
    }
    // The pattern is the first operand unless `-e` / `-f` gave it.
    let pattern_operand = usize::from(!pattern_given);
    if !numbered || operands < pattern_operand {
        return None;
    }
    Some(program)
}

/// Options of one program that keep `<path>:<line>:<text>` output.
struct Spec {
    /// Single-letter options without a value.
    short_flags: &'static str,
    /// Single-letter options with a value (attached or the next word).
    short_values: &'static str,
    /// `--name` options without a value.
    long_flags: &'static [&'static str],
    /// `--name=value` / `--name value` options.
    long_values: &'static [&'static str],
    /// `--name[=value]` options whose value is optional (only the `=` form is accepted).
    long_optional_values: &'static [&'static str],
    /// `--name=value` pairs that change the output shape.
    long_bad_values: &'static [(&'static str, &'static str)],
}

/// GNU grep 3.12 / ugrep 7.8.4 options (`grep --help`). Left out on purpose: `-c`, `-l`,
/// `-L`, `-h`, `-b`, `-Z` / `--null`, `-z`, `-T`, `-q`, `--label` and anything else not
/// listed (they drop or reshape the prefix), and `--color=always` (escape codes).
const GREP: Spec = Spec {
    short_flags: "rRniwxEFGPsIHvoay",
    short_values: "ABCefm",
    long_flags: &[
        "line-number",
        "recursive",
        "dereference-recursive",
        "ignore-case",
        "no-ignore-case",
        "word-regexp",
        "line-regexp",
        "extended-regexp",
        "fixed-strings",
        "basic-regexp",
        "perl-regexp",
        "no-messages",
        "with-filename",
        "invert-match",
        "only-matching",
        "text",
        "color",
        "colour",
    ],
    long_values: &[
        "include",
        "exclude",
        "exclude-dir",
        "after-context",
        "before-context",
        "context",
        "max-count",
        "regexp",
        "file",
        "binary-files",
    ],
    long_optional_values: &["color", "colour"],
    long_bad_values: &[("color", "always"), ("colour", "always")],
};

/// ripgrep 15.2.0 options (`rg --help`). Left out on purpose: `-N`, `--heading`, `-l`,
/// `-c`, `--json`, `--vimgrep`, `-I` / `--no-filename`, `-b`, `-0`, `--files`, `-r`
/// (replaces the text), `--color=always` and anything else not listed.
const RG: Spec = Spec {
    short_flags: "niSswxFPUHuL.",
    short_values: "ABCefgtTmd",
    long_flags: &[
        "line-number",
        "no-heading",
        "ignore-case",
        "smart-case",
        "case-sensitive",
        "word-regexp",
        "line-regexp",
        "fixed-strings",
        "pcre2",
        "multiline",
        "with-filename",
        "hidden",
        "no-ignore",
        "follow",
        "no-messages",
    ],
    long_values: &[
        "glob",
        "iglob",
        "type",
        "type-not",
        "after-context",
        "before-context",
        "context",
        "max-count",
        "max-depth",
        "regexp",
        "file",
        "color",
        "colors",
    ],
    long_optional_values: &[],
    long_bad_values: &[("color", "always"), ("color", "ansi")],
};

/// Splits one simple shell command into words: `'...'`, `"..."` (with `\` escapes) and
/// `\x` outside quotes. `None` for anything that is not certainly one simple command with
/// literal words: an unquoted `|`, `&`, `;`, `<`, `>`, `(`, `)`, `` ` ``, `$` or newline, a
/// `$` or `` ` `` inside double quotes, an unterminated quote, a leading `NAME=value`. A
/// trailing `2>/dev/null` (or `2> /dev/null`) is allowed: it only drops stderr.
pub fn shell_words(command: &str) -> Option<Vec<String>> {
    let trimmed = command.trim();
    let trimmed = ["2>/dev/null", "2> /dev/null"]
        .iter()
        .find_map(|t| trimmed.strip_suffix(t))
        .map(str::trim_end)
        .unwrap_or(trimmed);
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = trimmed.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => {
                            let n = chars.next()?;
                            if !matches!(n, '"' | '\\' | '$' | '`') {
                                cur.push('\\');
                            }
                            cur.push(n);
                        }
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next()? {
                    '\n' => return None,
                    n => cur.push(n),
                }
            }
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '$' | '\n' | '\r' => return None,
            ch => {
                in_word = true;
                cur.push(ch);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    let first = words.first()?;
    if first.contains('=') {
        return None;
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(
            shell_words(r#"grep -rn 'do_install() {' "meta x" a\ b 2>/dev/null"#).unwrap(),
            ["grep", "-rn", "do_install() {", "meta x", "a b"]
        );
        assert_eq!(
            shell_words(r#"rg -n "a\"b\\c\d""#).unwrap(),
            ["rg", "-n", r#"a"b\c\d"#]
        );
        for c in [
            "grep -rn x . | head",
            "grep -rn x . > out",
            "grep -rn x . && echo",
            "grep -rn x .; ls",
            "grep -rn \"$X\" .",
            "grep -rn $(cat p) .",
            "grep -rn `x` .",
            "LC_ALL=C grep -rn x .",
            "grep -rn 'x .",
            "grep -rn x .\nls",
        ] {
            assert_eq!(shell_words(c), None, "{c}");
        }
    }

    #[test]
    fn searches_recognised() {
        for (c, p) in [
            ("grep -rn 'install -d' meta-core meta-mine", Program::Grep),
            ("grep -Rni --include=*.bb SRC_URI .", Program::Grep),
            ("grep -n -r -e FOO -e BAR .", Program::Grep),
            ("grep -rn -C 2 do_install .", Program::Grep),
            ("grep -rnA2 do_install .", Program::Grep),
            ("grep --line-number --recursive x meta-core", Program::Grep),
            ("/usr/bin/grep -rn x .", Program::Grep),
            ("grep --color=auto -rn x .", Program::Grep),
            ("rg -n 'install -d'", Program::Rg),
            ("rg -n -g '*.bb' -t py SRC_URI meta-core", Program::Rg),
            ("rg --line-number --no-heading x", Program::Rg),
            ("rg -n -C1 x meta-core 2>/dev/null", Program::Rg),
            ("rg -n --glob '*.inc' x", Program::Rg),
        ] {
            assert_eq!(search_command(c), Some(p), "{c}");
        }
    }

    #[test]
    fn searches_left_alone() {
        for c in [
            "grep -r 'install -d' .",      // no line numbers
            "rg 'install -d'",             // no line numbers off a tty
            "rg -n -N x",                  // -N switches them off again
            "grep -rnc x .",               // counts
            "grep -rnl x .",               // file names
            "grep -rnh x .",               // no file names
            "grep -rnb x .",               // byte offsets
            "grep -rnZ x .",               // NUL after the name
            "grep --null -rn x .",         // likewise
            "grep --color=always -rn x .", // escape codes
            "rg -n --heading x",           // file name on its own line
            "rg -n --json x",              // JSON
            "rg -n --vimgrep x",           // not on the list
            "rg -n -r y x",                // replaces the text
            "rg -n --color always x",      // escape codes
            "grep -rn",                    // no pattern
            "grep -rn x . | grep -v test", // pipeline
            "ugrep -rn x .",               // not grep or rg by name
            "git grep -n x",               // git grep
            "bitbake -e hello",            // not a search
            "cat meta-core/recipes-core/hello/hello.inc",
        ] {
            assert_eq!(search_command(c), None, "{c}");
        }
    }
}
