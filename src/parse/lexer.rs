//! Logical lines: continuation, comments, function and `def` bodies.
//!
//! The lexer mirrors the line handling of BitBake's `BBHandler.get_statements` / `feeder`
//! (`lib/bb/parse/parse_py/BBHandler.py`), see docs/bitbake-semantics.md ("Lines"):
//!
//! - Each physical line is right-stripped (Python `str.rstrip()`), so `\r`, trailing blanks
//!   and trailing whitespace after a `\` are removed before anything else.
//! - Outside function and `def` bodies, a line ending in `\` is joined with the next one (the
//!   `\` is dropped, nothing is inserted). Trailing whitespace after the `\` is stripped
//!   first, so the line *does* continue; we add a diagnostic because the source is
//!   misleading.
//! - Inside a function body nothing is joined: the body runs to the first line that is
//!   exactly `}` (after right-stripping). Inside a `def` body, lines that start with
//!   whitespace, are empty, or start with `#` belong to the body.
//!
//! The lexer never fails: problems become diagnostics.

use std::borrow::Cow;
use std::ops::Range;

use crate::parse::ast::ParseDiagnostic;
use crate::types::LineSpan;

/// A parsed function header (`[python] [fakeroot] name() {`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncHeader {
    /// The name as written; `None` for `python () {`. (`__anonymous` is kept as written; the
    /// parser maps it to an anonymous function.)
    pub name: Option<String>,
    pub python: bool,
    pub fakeroot: bool,
}

/// Classification of a logical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// Empty after right-stripping.
    Blank,
    /// Starts with `#` (also an indented comment, with a diagnostic).
    Comment,
    /// Function header plus body. `body` covers the lines strictly between the header and
    /// the closing `}` (to end of file when `closed` is false). An empty body is the empty
    /// span `start = header end + 1, end = header end`.
    Function {
        header: FuncHeader,
        body: LineSpan,
        closed: bool,
    },
    /// Top-level python `def`. `body` covers the indented lines after the header, without
    /// trailing blank or comment lines; empty as for `Function`.
    Def { name: String, body: LineSpan },
    /// First word is a directive keyword (`inherit`, `require`, `export`, ...).
    Directive,
    /// Anything else (usually an assignment).
    Other,
}

/// One logical line: a statement candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLine {
    pub kind: LineKind,
    /// Joined, right-stripped text without the continuation backslashes. For functions and
    /// `def`s, the header only.
    pub text: String,
    /// Physical lines covered, including the body of a function or `def`.
    pub span: LineSpan,
    /// Byte range in the source, from the start of the first line to the end of the last
    /// line's content (line terminator excluded, a leading BOM excluded).
    pub bytes: Range<usize>,
}

/// Lexer output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lexed {
    pub lines: Vec<LogicalLine>,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Directive keywords (first word of a line) recognised by BitBake's `BBHandler` and
/// `ConfHandler`.
pub const DIRECTIVES: &[&str] = &[
    "inherit",
    "inherit_defer",
    "require",
    "include",
    "include_all",
    "addtask",
    "deltask",
    "addhandler",
    "EXPORT_FUNCTIONS",
    "export",
    "unset",
    "addpylib",
    "addfragments",
];

/// Decodes file bytes for [`lex`] / [`crate::parse::parse`]. Invalid UTF-8 is replaced with
/// U+FFFD; this never fails. Byte offsets in the output refer to the decoded text.
pub fn decode(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// Whitespace as Python's `str.isspace()` / regex `\s` see it.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub(crate) fn rstrip(s: &str) -> &str {
    s.trim_end_matches(py_space)
}

fn line_no(idx: usize) -> u32 {
    u32::try_from(idx + 1).unwrap_or(u32::MAX)
}

/// A physical line.
struct Phys<'a> {
    /// Byte offset of the first character.
    start: usize,
    /// Byte offset of the end of the content (before the terminator).
    end: usize,
    /// Right-stripped content.
    text: &'a str,
}

/// Splits on `\n`, `\r\n` and lone `\r` (Python universal newlines, as BitBake's
/// `open(fn, 'r')` reads). A leading UTF-8 BOM is excluded from line 1.
fn physical(src: &str) -> Vec<Phys<'_>> {
    fn push<'a>(src: &'a str, s: usize, e: usize, out: &mut Vec<Phys<'a>>) {
        let raw = src.get(s..e).unwrap_or("");
        out.push(Phys {
            start: s,
            end: e,
            text: rstrip(raw),
        });
    }
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut start = if src.starts_with('\u{feff}') { 3 } else { 0 };
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                push(src, start, i, &mut out);
                i += 1;
                start = i;
            }
            b'\r' => {
                push(src, start, i, &mut out);
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        push(src, start, bytes.len(), &mut out);
    }
    out
}

/// Matches BitBake's `__func_start_regexp__`:
/// `(((?P<py>python(?=(\s|\()))|(?P<fr>fakeroot(?=\s)))\s*)*(?P<func>[\w\.\-\+\{\}\$:]+)?\s*\(\s*\)\s*{$`
pub fn func_header(s: &str) -> Option<FuncHeader> {
    let mut rest = s;
    let (mut python, mut fakeroot) = (false, false);
    loop {
        if let Some(r) = rest.strip_prefix("python")
            && (r.starts_with(py_space) || r.starts_with('('))
        {
            python = true;
            rest = r.trim_start_matches(py_space);
            continue;
        }
        if let Some(r) = rest.strip_prefix("fakeroot")
            && r.starts_with(py_space)
        {
            fakeroot = true;
            rest = r.trim_start_matches(py_space);
            continue;
        }
        break;
    }
    let name_len = rest
        .find(|c: char| !func_name_char(c))
        .unwrap_or(rest.len());
    let name = &rest[..name_len];
    let r = rest[name_len..].trim_start_matches(py_space);
    let r = r.strip_prefix('(')?.trim_start_matches(py_space);
    let r = r.strip_prefix(')')?.trim_start_matches(py_space);
    if r != "{" {
        return None;
    }
    Some(FuncHeader {
        name: (!name.is_empty()).then(|| name.to_string()),
        python,
        fakeroot,
    })
}

fn func_name_char(c: char) -> bool {
    word_char(c) || ".-+{}$:".contains(c)
}

/// Python regex `\w` (approximately: Unicode alphanumerics and `_`).
pub(crate) fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Matches BitBake's `__def_regexp__` (`def\s+(\w+).*:`, anchored at the start). Returns the
/// function name.
pub fn def_header(s: &str) -> Option<String> {
    let r = s.strip_prefix("def")?;
    if !r.starts_with(py_space) {
        return None;
    }
    let r = r.trim_start_matches(py_space);
    let n = r.find(|c: char| !word_char(c)).unwrap_or(r.len());
    if n == 0 || !r[n..].contains(':') {
        return None;
    }
    Some(r[..n].to_string())
}

/// BitBake's `__python_func_regexp__` (`(\s+.*)|(^$)|(^#)`): a line that continues a `def`.
fn def_body_line(t: &str) -> bool {
    t.is_empty() || t.starts_with(py_space) || t.starts_with('#')
}

fn first_word(s: &str) -> &str {
    s.split(py_space).next().unwrap_or("")
}

fn empty_after(line: u32) -> LineSpan {
    LineSpan::new(line.saturating_add(1), line)
}

/// Splits `src` into logical lines. Never fails, never panics.
pub fn lex(src: &str) -> Lexed {
    let phys = physical(src);
    let mut out = Lexed::default();
    let diag = |out: &mut Lexed, span: LineSpan, msg: String| {
        out.diagnostics.push(ParseDiagnostic { span, message: msg });
    };

    if src.starts_with('\u{feff}') && !phys.is_empty() {
        diag(
            &mut out,
            LineSpan::line(1),
            "file starts with a UTF-8 byte order mark; BitBake does not strip it, so line 1 \
             fails to parse there"
                .into(),
        );
    }

    let mut i = 0;
    while i < phys.len() {
        // Build one logical line from continuation lines.
        let first = i;
        let is_comment = phys[first].text.starts_with('#');
        let mut joined = String::new();
        let mut warned_mixed = false;
        loop {
            let p = &phys[i];
            let t = p.text;
            if i > first && !warned_mixed {
                if t.starts_with('#') && !is_comment {
                    diag(
                        &mut out,
                        LineSpan::line(line_no(i)),
                        "comment line inside a continued statement (BitBake rejects this)".into(),
                    );
                    warned_mixed = true;
                } else if is_comment && !t.starts_with('#') {
                    diag(
                        &mut out,
                        LineSpan::new(line_no(first), line_no(i)),
                        "comment ending in `\\` continues onto a line that is not a comment \
                         (BitBake rejects this); treated as a comment"
                            .into(),
                    );
                    warned_mixed = true;
                }
            }
            if let Some(head) = t.strip_suffix('\\') {
                if p.start + t.len() < p.end {
                    diag(
                        &mut out,
                        LineSpan::line(line_no(i)),
                        "whitespace after `\\`: BitBake strips it and still continues the line"
                            .into(),
                    );
                }
                joined.push_str(head);
                if i + 1 >= phys.len() {
                    diag(
                        &mut out,
                        LineSpan::new(line_no(first), line_no(i)),
                        "line continuation `\\` at end of file".into(),
                    );
                    break;
                }
                i += 1;
                continue;
            }
            joined.push_str(t);
            break;
        }
        let last = i;
        i += 1;
        let span = LineSpan::new(line_no(first), line_no(last));
        let start_byte = phys[first].start;
        let mut end_byte = phys[last].end;

        let kind = if joined.is_empty() {
            LineKind::Blank
        } else if is_comment {
            LineKind::Comment
        } else if joined.starts_with(py_space)
            && joined.trim_start_matches(py_space).starts_with('#')
        {
            diag(
                &mut out,
                span,
                "indented comment: BitBake only treats `#` in column 0 as a comment and \
                 reports this line as unparsed"
                    .into(),
            );
            LineKind::Comment
        } else if let Some(header) = func_header(&joined) {
            // Body: up to the first line that is exactly `}`.
            let mut j = i;
            while j < phys.len() && phys[j].text != "}" {
                j += 1;
            }
            let (body, closed, stmt_end) = if j < phys.len() {
                let body = if j > i {
                    LineSpan::new(line_no(i), line_no(j - 1))
                } else {
                    empty_after(line_no(last))
                };
                (body, true, j)
            } else {
                let body = if phys.len() > i {
                    LineSpan::new(line_no(i), line_no(phys.len() - 1))
                } else {
                    empty_after(line_no(last))
                };
                (body, false, phys.len().saturating_sub(1).max(last))
            };
            let whole = LineSpan::new(span.start, line_no(stmt_end));
            if !closed {
                let name = header.name.as_deref().unwrap_or("(anonymous)");
                diag(
                    &mut out,
                    whole,
                    format!(
                        "function {name} is never closed: no `}}` in column 0 before end of file"
                    ),
                );
            }
            end_byte = phys[stmt_end].end;
            i = stmt_end + 1;
            out.lines.push(LogicalLine {
                kind: LineKind::Function {
                    header,
                    body,
                    closed,
                },
                text: joined,
                span: whole,
                bytes: start_byte..end_byte,
            });
            continue;
        } else if let Some(name) = def_header(&joined) {
            let mut j = i;
            while j < phys.len() && def_body_line(phys[j].text) {
                j += 1;
            }
            // Leave trailing blank and column-0 comment lines out of the body.
            let mut k = j;
            while k > i && (phys[k - 1].text.is_empty() || phys[k - 1].text.starts_with('#')) {
                k -= 1;
            }
            let body = if k > i {
                LineSpan::new(line_no(i), line_no(k - 1))
            } else {
                empty_after(line_no(last))
            };
            let stmt_last = if k > i { k - 1 } else { last };
            end_byte = phys[stmt_last].end;
            i = k.max(i);
            out.lines.push(LogicalLine {
                kind: LineKind::Def { name, body },
                text: joined,
                span: LineSpan::new(span.start, line_no(stmt_last)),
                bytes: start_byte..end_byte,
            });
            continue;
        } else if DIRECTIVES.contains(&first_word(&joined)) {
            LineKind::Directive
        } else {
            LineKind::Other
        };
        out.lines.push(LogicalLine {
            kind,
            text: joined,
            span,
            bytes: start_byte..end_byte,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn func_header_forms() {
        let h = |s: &str| func_header(s).map(|h| (h.name, h.python, h.fakeroot));
        assert_eq!(h("do_x() {"), Some((Some("do_x".into()), false, false)));
        assert_eq!(h("do_x () {"), Some((Some("do_x".into()), false, false)));
        assert_eq!(h("python () {"), Some((None, true, false)));
        assert_eq!(h("python() {"), Some((None, true, false)));
        assert_eq!(
            h("python __anonymous() {"),
            Some((Some("__anonymous".into()), true, false))
        );
        assert_eq!(
            h("fakeroot python do_b() {"),
            Some((Some("do_b".into()), true, true))
        );
        assert_eq!(
            h("fakeroot() {"),
            Some((Some("fakeroot".into()), false, false))
        );
        assert_eq!(
            h("python_helper() {"),
            Some((Some("python_helper".into()), false, false))
        );
        assert_eq!(
            h("do_install:append:class-target() {"),
            Some((Some("do_install:append:class-target".into()), false, false))
        );
        assert_eq!(h("do_x() {}"), None);
        assert_eq!(h(" do_x() {"), None);
        assert_eq!(h("VAR = \"() {\""), None);
    }

    #[test]
    fn def_header_forms() {
        assert_eq!(def_header("def f(d):"), Some("f".into()));
        assert_eq!(def_header("def  g(d, x=1): return x"), Some("g".into()));
        assert_eq!(def_header("def h(d)"), None);
        assert_eq!(def_header("define = \"a:b\""), None);
    }

    #[test]
    fn universal_newlines() {
        let l = lex("A = \"1\"\rB = \"2\"\r\nC = \"3\"");
        let spans: Vec<_> = l.lines.iter().map(|l| l.span).collect();
        assert_eq!(
            spans,
            [LineSpan::line(1), LineSpan::line(2), LineSpan::line(3)]
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(lex(""), Lexed::default());
        assert!(lex("\u{feff}").lines.is_empty());
    }
}
