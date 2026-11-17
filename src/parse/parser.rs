//! Statement parser over the lexer's logical lines.
//!
//! Each logical line is matched in the order BitBake's `BBHandler.feeder` and
//! `ConfHandler.feeder` try their regular expressions (`lib/bb/parse/parse_py/`), so a line
//! means here what it means to BitBake:
//!
//! function header, `def`, `EXPORT_FUNCTIONS`, `addtask`, `deltask`, `addhandler`,
//! `inherit`, `inherit_defer`, assignment, `include`, `require`, `include_all`, `export`,
//! `unset`, `unset VAR[flag]`, `addpylib`, `addfragments`.
//!
//! The regular expressions are reimplemented by hand (they use look-ahead and
//! back-references) with the same backtracking result. See docs/bitbake-semantics.md.

use crate::parse::ast::{IncludeMode, Lang, ParseDiagnostic, ParsedFile, Statement, StatementKind};
use crate::parse::lexer::{self, LineKind, LogicalLine, py_space};
use crate::types::{AssignOp, LineSpan, VarRef};

/// Parses BitBake source. Never fails and never panics.
pub fn parse(src: &str) -> ParsedFile {
    let lexed = lexer::lex(src);
    let mut out = ParsedFile {
        statements: Vec::with_capacity(lexed.lines.len()),
        diagnostics: lexed.diagnostics,
    };
    for line in lexed.lines {
        let LogicalLine {
            kind,
            text,
            span,
            bytes,
        } = line;
        let kind = match kind {
            LineKind::Blank | LineKind::Comment => continue,
            LineKind::Function { header, body, .. } => {
                let anonymous = matches!(header.name.as_deref(), None | Some("__anonymous"));
                // BitBake runs every anonymous function as python (`MethodNode.eval`).
                let lang = if header.python || anonymous {
                    Lang::Python
                } else {
                    Lang::Shell
                };
                StatementKind::Function {
                    name: if anonymous {
                        None
                    } else {
                        header.name.as_deref().map(|n| split_var(n, None))
                    },
                    lang,
                    fakeroot: header.fakeroot,
                    body,
                }
            }
            LineKind::Def { name, body } => StatementKind::PyDef { name, body },
            LineKind::Directive | LineKind::Other => statement(&text, span, &mut out.diagnostics),
        };
        out.statements.push(Statement { kind, span, bytes });
    }
    // Lexer and parser diagnostics interleaved in line order (stable for equal lines).
    out.diagnostics.sort_by_key(|d| d.span.start);
    out
}

fn unknown(
    text: &str,
    span: LineSpan,
    diags: &mut Vec<ParseDiagnostic>,
    why: &str,
) -> StatementKind {
    diags.push(ParseDiagnostic {
        span,
        message: why.to_string(),
    });
    StatementKind::Unknown {
        text: text.to_string(),
    }
}

/// `keyword\s+(.+)` anchored at the start: the text after the keyword and its whitespace.
fn keyword_arg<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let r = s.strip_prefix(kw)?;
    if !r.starts_with(py_space) {
        return None;
    }
    let r = r.trim_start_matches(py_space);
    (!r.is_empty()).then_some(r)
}

/// `keyword\s+([^#\n]+)`: as [`keyword_arg`], stopping at the first `#`.
fn keyword_arg_to_hash<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let r = keyword_arg(s, kw)?;
    let r = r.split('#').next().unwrap_or("");
    (!r.is_empty()).then_some(r)
}

fn words(s: &str) -> Vec<String> {
    s.split(py_space)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn joined_words(s: &str) -> String {
    words(s).join(" ")
}

fn statement(text: &str, span: LineSpan, diags: &mut Vec<ParseDiagnostic>) -> StatementKind {
    // BBHandler.feeder order.
    if let Some(arg) = keyword_arg(text, "EXPORT_FUNCTIONS") {
        return StatementKind::ExportFunctions { names: words(arg) };
    }
    if let Some(arg) = keyword_arg_to_hash(text, "addtask") {
        return addtask(arg, span, diags);
    }
    if let Some(arg) = keyword_arg_to_hash(text, "deltask") {
        return StatementKind::DelTask {
            name: joined_words(arg),
        };
    }
    if let Some(arg) = keyword_arg(text, "addhandler") {
        return StatementKind::AddHandler {
            name: joined_words(arg),
        };
    }
    if let Some(arg) = keyword_arg(text, "inherit") {
        return StatementKind::Inherit {
            classes: arg.to_string(),
            deferred: false,
        };
    }
    if let Some(arg) = keyword_arg(text, "inherit_defer") {
        return StatementKind::Inherit {
            classes: arg.to_string(),
            deferred: true,
        };
    }
    // ConfHandler.feeder order.
    if let Some(m) = match_assignment(text) {
        if m.var.is_empty() {
            return unknown(text, span, diags, "empty variable name in assignment");
        }
        return StatementKind::Assign {
            var: split_var(m.var, m.flag.map(str::to_string)),
            op: m.op,
            value: m.value.to_string(),
            export: m.export,
        };
    }
    for (kw, mode) in [
        ("include", IncludeMode::Include),
        ("require", IncludeMode::Require),
        ("include_all", IncludeMode::IncludeAll),
    ] {
        if let Some(arg) = keyword_arg(text, kw) {
            return StatementKind::Include {
                path: arg.to_string(),
                mode,
            };
        }
    }
    if let Some(name) = keyword_arg(text, "export").filter(|n| n.chars().all(name_char)) {
        return StatementKind::Export {
            name: name.to_string(),
        };
    }
    if let Some(arg) = keyword_arg(text, "unset") {
        if arg.chars().all(name_char) {
            return StatementKind::Unset {
                name: arg.to_string(),
                flag: None,
            };
        }
        if let Some((name, flag)) = unset_flag(arg) {
            return StatementKind::Unset {
                name: name.to_string(),
                flag: Some(flag.to_string()),
            };
        }
    }
    let word = text.split(py_space).next().unwrap_or("");
    if let Some(af) = crate::parse::ast::add_fragments_line(text) {
        // Kept as an unknown statement (see `ast::AddFragments`); the replay reads it with
        // `Statement::add_fragments`. Fewer than three arguments match no release's
        // regular expression: BitBake reports "unparsed line".
        let why = if af.groups(3).is_some() {
            "`addfragments` directive (configuration fragments): kept as an unknown \
             statement; the evaluator applies it (`Statement::add_fragments`)"
        } else {
            "unparsed line: `addfragments` needs at least three arguments"
        };
        return unknown(text, span, diags, why);
    }
    if word == "addpylib" {
        // Valid BitBake (configuration only) that the AST does not model.
        return unknown(
            text,
            span,
            diags,
            &format!("`{word}` is not modelled by sous-chef"),
        );
    }
    let why = if text.starts_with(py_space) {
        "unparsed line: BitBake does not allow leading whitespace here"
    } else if text.ends_with('}') && text.trim_matches(py_space) == "}" {
        "unparsed line: `}` outside a function"
    } else {
        "unparsed line"
    };
    unknown(text, span, diags, why)
}

/// BitBake's `addtask` handling: split on the literal strings `" before "` and `" after "`
/// (BBHandler.feeder), reproduced exactly, including its quirk that the text from two
/// separate `after` (or `before`) groups is concatenated without a space.
fn addtask(arg: &str, span: LineSpan, diags: &mut Vec<ParseDiagnostic>) -> StatementKind {
    let tasks = arg
        .split(" before ")
        .next()
        .unwrap_or("")
        .split(" after ")
        .next()
        .unwrap_or("");
    let mut after = String::new();
    let mut before = String::new();
    let mut glued = false;
    for exp in arg.split(" before ") {
        let parts: Vec<&str> = exp.split(" after ").collect();
        if parts.len() > 1 {
            glued |= !after.is_empty();
            after.push_str(&parts[1..].join(" "));
        }
    }
    for exp in arg.split(" after ") {
        let parts: Vec<&str> = exp.split(" before ").collect();
        if parts.len() > 1 {
            glued |= !before.is_empty();
            before.push_str(&parts[1..].join(" "));
        }
    }
    if glued {
        diags.push(ParseDiagnostic {
            span,
            message: "addtask: `before`/`after` groups split by the other keyword are \
                      concatenated by BitBake without a space, joining two task names"
                .into(),
        });
    }
    StatementKind::AddTask {
        name: joined_words(tasks),
        before: words(&before),
        after: words(&after),
    }
}

/// Characters of a variable name in `__config_regexp__`: `[a-zA-Z0-9\-_+.${}/~:]`.
fn var_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-_+.${}/~:".contains(c)
}

/// Characters of a name in `__export_regexp__` / `__unset_regexp__` (no `:`).
fn name_char(c: char) -> bool {
    c != ':' && var_char(c)
}

fn flag_first_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-_+.".contains(c)
}

/// `unset\s+NAME\[([a-zA-Z0-9\-_+.][a-zA-Z0-9\-_+.@]+)\]$` after the keyword.
fn unset_flag(arg: &str) -> Option<(&str, &str)> {
    let (name, rest) = arg.split_once('[')?;
    let flag = rest.strip_suffix(']')?;
    let mut chars = flag.chars();
    let ok = !name.is_empty()
        && name.chars().all(name_char)
        && chars.next().is_some_and(flag_first_char)
        && flag.len() >= 2
        && chars.all(|c| flag_first_char(c) || c == '@');
    ok.then_some((name, flag))
}

struct AssignMatch<'a> {
    export: bool,
    var: &'a str,
    flag: Option<&'a str>,
    op: AssignOp,
    value: &'a str,
}

/// Operators in the order of the `__config_regexp__` alternation.
const OPS: [(&str, AssignOp); 8] = [
    (":=", AssignOp::Immediate),
    ("??=", AssignOp::WeakDefault),
    ("?=", AssignOp::Default),
    ("+=", AssignOp::Append),
    ("=+", AssignOp::Prepend),
    ("=.", AssignOp::PrependDot),
    (".=", AssignOp::AppendDot),
    ("=", AssignOp::Set),
];

fn skip_space(s: &str, pos: usize) -> usize {
    pos + (s[pos..].len() - s[pos..].trim_start_matches(py_space).len())
}

/// Reimplements BitBake's `ConfHandler.__config_regexp__`:
///
/// ```text
/// ^(?P<exp>export\s+)?(?P<var>[a-zA-Z0-9\-_+.${}/~:]*?)
/// (\[(?P<flag>[a-zA-Z0-9\-_+.][a-zA-Z0-9\-_+.@/]*)\])?
/// \s*(:=|\?\?=|\?=|\+=|=\+|=\.|\.=|=)\s*
/// (?!'[^']*'[^']*'$)(?!\"[^\"]*\"[^\"]*\"$)(?P<apo>['\"])(?P<value>.*)(?P=apo)$
/// ```
///
/// with the same backtracking order: export prefix first, then the shortest name, then flag
/// before no flag, then the operators in order.
fn match_assignment(s: &str) -> Option<AssignMatch<'_>> {
    let mut starts = Vec::with_capacity(2);
    if let Some(r) = s.strip_prefix("export")
        && r.starts_with(py_space)
    {
        starts.push((true, skip_space(s, "export".len())));
    }
    starts.push((false, 0));
    for (export, var_start) in starts {
        let max_end = s[var_start..]
            .find(|c: char| !var_char(c))
            .map_or(s.len(), |n| var_start + n);
        // var_char is ASCII only, so every index in var_start..=max_end is a char boundary.
        for var_end in var_start..=max_end {
            let var = &s[var_start..var_end];
            for with_flag in [true, false] {
                let mut pos = var_end;
                let mut flag = None;
                if with_flag {
                    let Some(rest) = s[pos..].strip_prefix('[') else {
                        continue;
                    };
                    let mut chars = rest.char_indices();
                    if !chars.next().is_some_and(|(_, c)| flag_first_char(c)) {
                        continue;
                    }
                    let n = rest
                        .find(|c: char| !(flag_first_char(c) || c == '@' || c == '/'))
                        .unwrap_or(rest.len());
                    if !rest[n..].starts_with(']') {
                        continue;
                    }
                    flag = Some(&rest[..n]);
                    pos += 1 + n + 1;
                }
                pos = skip_space(s, pos);
                for (text, op) in OPS {
                    if !s[pos..].starts_with(text) {
                        continue;
                    }
                    let vpos = skip_space(s, pos + text.len());
                    if let Some(value) = quoted_value(&s[vpos..]) {
                        return Some(AssignMatch {
                            export,
                            var,
                            flag,
                            op,
                            value,
                        });
                    }
                }
            }
        }
    }
    None
}

/// The quoted tail of an assignment: `(?!'[^']*'[^']*'$)(?!"[^"]*"[^"]*"$)(['"])(.*)\1$`.
fn quoted_value(rest: &str) -> Option<&str> {
    for q in ['\'', '"'] {
        if rest.starts_with(q) && rest.ends_with(q) && rest.matches(q).count() == 3 {
            return None;
        }
    }
    let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    if rest.len() < 2 || !rest.ends_with(q) {
        return None;
    }
    rest.get(1..rest.len() - 1)
}

/// Splits `s` on `sep` outside `${...}`.
fn split_outside(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut prev_dollar = false;
    for (i, c) in s.char_indices() {
        match c {
            '{' if prev_dollar || depth > 0 => depth += 1,
            '}' if depth > 0 => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
        prev_dollar = c == '$';
    }
    out.push(&s[start..]);
    out
}

/// Override operators that, written with `_`, mark legacy syntax.
const LEGACY_OPERATORS: [&str; 3] = ["append", "prepend", "remove"];

/// Package-scoped variables: `VAR_<package>` was the legacy form of `VAR:<package>`.
pub const PACKAGE_VARS: &[&str] = &[
    "FILES",
    "RDEPENDS",
    "RRECOMMENDS",
    "RSUGGESTS",
    "RPROVIDES",
    "RCONFLICTS",
    "RREPLACES",
    "ALLOW_EMPTY",
    "SUMMARY",
    "DESCRIPTION",
    "SECTION",
    "LICENSE",
    "CONFFILES",
    "INSANE_SKIP",
    "PKG",
    "pkg_preinst",
    "pkg_postinst",
    "pkg_postinst_ontarget",
    "pkg_prerm",
    "pkg_postrm",
];

/// A package name after a package-scoped variable: a `${...}` reference (`${PN}`,
/// `${PN}-dev`) or a lowercase name (OE package names are lowercase; upper-case suffixes such
/// as `FILES_SOLIBSDEV` are separate variables).
fn package_like(e: &str) -> bool {
    e.starts_with("${") || e.starts_with(|c: char| c.is_ascii_lowercase())
}

/// Detects legacy `_` override syntax in a base name (docs/bitbake-semantics.md, "Legacy
/// syntax"). Returns the legacy split: base and `_`-separated overrides.
fn legacy_split(base: &str) -> Option<(String, Vec<String>)> {
    let el = split_outside(base, '_');
    // `prefix(n)` is the text of the first `n` elements: a slice of `base`, no allocation.
    let prefix = |n: usize| -> &str {
        let len = el[..n].iter().map(|e| e.len()).sum::<usize>() + n.saturating_sub(1);
        base.get(..len).unwrap_or(base)
    };
    // The longest package variable has three `_`-separated elements.
    const MAX_PKG_ELEMENTS: usize = 3;
    for i in 1..el.len() {
        let is_op = LEGACY_OPERATORS.contains(&el[i]);
        let is_pkg = i <= MAX_PKG_ELEMENTS
            && PACKAGE_VARS.contains(&prefix(i))
            && package_like(el[i])
            && !(i + 1..=el.len().min(MAX_PKG_ELEMENTS)).any(|j| PACKAGE_VARS.contains(&prefix(j)));
        if !(is_op || is_pkg) {
            continue;
        }
        let p = prefix(i);
        if p.chars().any(|c| c.is_alphanumeric()) {
            return Some((
                p.to_string(),
                el[i..].iter().map(|e| e.to_string()).collect(),
            ));
        }
    }
    None
}

/// Splits a variable or function name into a [`VarRef`]: on `:` outside `${...}`, the first
/// element is the base. Legacy `_` syntax is detected on the base; then `base` and the
/// leading `overrides` reflect the legacy split and `legacy_syntax` is set.
pub fn split_var(name: &str, flag: Option<String>) -> VarRef {
    let parts = split_outside(name, ':');
    let first = parts.first().copied().unwrap_or("");
    let rest = parts.iter().skip(1).map(|p| p.to_string());
    match legacy_split(first) {
        Some((base, mut overrides)) => {
            overrides.extend(rest);
            VarRef {
                base,
                overrides,
                flag,
                legacy_syntax: true,
            }
        }
        None => VarRef {
            base: first.to_string(),
            overrides: rest.collect(),
            flag,
            legacy_syntax: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assign(s: &str) -> Option<(bool, String, Option<String>, AssignOp, String)> {
        match_assignment(s).map(|m| {
            (
                m.export,
                m.var.to_string(),
                m.flag.map(str::to_string),
                m.op,
                m.value.to_string(),
            )
        })
    }

    #[test]
    fn assignment_regex_backtracking() {
        let a = |s| assign(s).map(|(_, v, f, o, val)| (v, f, o, val));
        assert_eq!(
            a("A += \"x\""),
            Some(("A".into(), None, AssignOp::Append, "x".into()))
        );
        assert_eq!(
            a("A+=\"x\""),
            Some(("A".into(), None, AssignOp::Append, "x".into()))
        );
        assert_eq!(
            a("A:=\"x\""),
            Some(("A".into(), None, AssignOp::Immediate, "x".into()))
        );
        assert_eq!(
            a("A=+'x'"),
            Some(("A".into(), None, AssignOp::Prepend, "x".into()))
        );
        assert_eq!(
            a("A.=\"x\""),
            Some(("A".into(), None, AssignOp::AppendDot, "x".into()))
        );
        assert_eq!(
            a("A =. \"x\""),
            Some(("A".into(), None, AssignOp::PrependDot, "x".into()))
        );
        assert_eq!(
            a("MY.DOTTED+NAME = \"v\""),
            Some(("MY.DOTTED+NAME".into(), None, AssignOp::Set, "v".into()))
        );
        assert_eq!(
            a("V:append:x = \"v\""),
            Some(("V:append:x".into(), None, AssignOp::Set, "v".into()))
        );
        assert_eq!(
            a("V[doc] ??= \"v\""),
            Some((
                "V".into(),
                Some("doc".into()),
                AssignOp::WeakDefault,
                "v".into()
            ))
        );
        assert_eq!(
            a("A = \"a\" \"b\""),
            Some(("A".into(), None, AssignOp::Set, "a\" \"b".into()))
        );
        // Exactly three quotes of one kind is rejected by the look-ahead.
        assert_eq!(a("A = \"a\"b\""), None);
        assert_eq!(a("A = \"x\" # comment"), None);
        assert_eq!(a("A = \"x"), None);
        assert_eq!(a("A = \""), None);
        assert_eq!(a(" A = \"x\""), None);
        assert_eq!(a("A B = \"x\""), None);
        assert_eq!(
            a("= \"x\""),
            Some(("".into(), None, AssignOp::Set, "x".into()))
        );
        assert_eq!(assign("export  A ?= \"x\"").map(|m| m.0), Some(true));
        assert_eq!(
            assign("exportA = \"x\"").map(|m| (m.0, m.1)),
            Some((false, "exportA".into()))
        );
    }

    fn v(name: &str) -> (String, Vec<String>, bool) {
        let r = split_var(name, None);
        (r.base, r.overrides, r.legacy_syntax)
    }

    fn ov(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_on_colon_outside_braces() {
        assert_eq!(
            v("FILES:${PN}-dev:append"),
            ("FILES".into(), ov(&["${PN}-dev", "append"]), false)
        );
        assert_eq!(
            v("A:${@d.getVar('X') or 'a:b'}:x"),
            ("A".into(), ov(&["${@d.getVar('X') or 'a:b'}", "x"]), false)
        );
        assert_eq!(v("PLAIN"), ("PLAIN".into(), ov(&[]), false));
    }

    #[test]
    fn legacy_true_positives() {
        assert_eq!(
            v("DEPENDS_append"),
            ("DEPENDS".into(), ov(&["append"]), true)
        );
        assert_eq!(
            v("EXTRA_OECONF_append_qemux86"),
            ("EXTRA_OECONF".into(), ov(&["append", "qemux86"]), true)
        );
        assert_eq!(
            v("RDEPENDS_${PN}"),
            ("RDEPENDS".into(), ov(&["${PN}"]), true)
        );
        assert_eq!(
            v("FILES_${PN}-dev"),
            ("FILES".into(), ov(&["${PN}-dev"]), true)
        );
        assert_eq!(
            v("ALLOW_EMPTY_${PN}-doc"),
            ("ALLOW_EMPTY".into(), ov(&["${PN}-doc"]), true)
        );
        assert_eq!(
            v("pkg_postinst_${PN}"),
            ("pkg_postinst".into(), ov(&["${PN}"]), true)
        );
        assert_eq!(
            v("pkg_postinst_ontarget_${PN}"),
            ("pkg_postinst_ontarget".into(), ov(&["${PN}"]), true)
        );
        assert_eq!(
            v("RDEPENDS_libfoo-dev"),
            ("RDEPENDS".into(), ov(&["libfoo-dev"]), true)
        );
        assert_eq!(
            v("RDEPENDS_${PN}_append"),
            ("RDEPENDS".into(), ov(&["${PN}", "append"]), true)
        );
        assert_eq!(
            v("do_install_append"),
            ("do_install".into(), ov(&["append"]), true)
        );
        assert_eq!(
            v("FILESEXTRAPATHS_prepend"),
            ("FILESEXTRAPATHS".into(), ov(&["prepend"]), true)
        );
        assert_eq!(
            v("RDEPENDS_${PN}:append"),
            ("RDEPENDS".into(), ov(&["${PN}", "append"]), true)
        );
    }

    #[test]
    fn legacy_false_positives() {
        for name in [
            "do_install",
            "SRCREV_machine",
            "BBFILE_PRIORITY_core",
            "BBFILE_COLLECTIONS",
            "BBFILE_PATTERN_legacy",
            "LAYERSERIES_COMPAT_core",
            "LAYERDEPENDS_legacy",
            "PACKAGECONFIG_CONFARGS",
            "PACKAGE_BEFORE_PN",
            "FILES_SOLIBSDEV",
            "pkg_postinst_ontarget",
            "pkg_postinst",
            "EXTRA_OECONF",
            "__anonymous",
            "FILES:${PN}-dev",
            "${PACKAGECONFIG_CONFARGS}",
            "SSTATE_${PN}_dir",
            "PREFERRED_PROVIDER_virtual/kernel",
            "RDEPENDS:${PN}",
        ] {
            assert!(!v(name).2, "{name} flagged as legacy");
        }
    }
}
