//! Splitting list values without breaking `${...}` apart.
//!
//! BitBake splits a list value (`DEPENDS`, `PROVIDES`, `PACKAGES`, `RDEPENDS:<pkg>`, a
//! `:remove` text, ...) on whitespace and `OVERRIDES` on `:` **after** expanding it, python
//! included (`d.getVar(...).split()`, `data_smart.py` `need_overrides`: `overrides =
//! (self.getVar("OVERRIDES") or "").split(":")`). A static value can still hold an
//! unevaluated `${@...}` (or an undefined `${VAR}`), whose text has spaces, quotes, commas and
//! colons of its own (`${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '',
//! d)}`). Splitting that text cuts the expression into fragments (`'1',`, `'',`) that are
//! not words of the real value. Every split of a value goes through this module instead: a
//! `${...}` group is opaque, so separators inside it do not split, and any word that holds a
//! group is an **unevaluated word** ([`is_unevaluated`]): it stands for text not known
//! statically (possibly several words, possibly none) and is never an override name, a
//! dependency or a provider.
//!
//! The end of a group: braces nest (`${@'${PN}' if x else '{}'}`), and inside `${@...}` a
//! quoted string (`'...'`, `"..."`, with `\` escapes) is skipped, so a brace in a string
//! does not count. (BitBake's own `__expand_python_regexp__`, `\$\{@(?:\{.*?\}|.)+?\}`, ends
//! the expression at the first `}` not closing a one-level `{...}`; for the python OE-core
//! writes the two agree.) An unterminated group runs to the end of the text.

/// The byte length of the `${...}` group at the start of `s` (which starts with `${`); the
/// whole rest when it is not terminated.
fn group_len(s: &str) -> usize {
    let b = s.as_bytes();
    let python = b.get(2) == Some(&b'@');
    let mut depth = 0usize;
    let mut i = 1; // at the `{`
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            q @ (b'\'' | b'"') if python => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// Splits `s` at every character for which `sep` is true, outside `${...}` groups. Empty
/// pieces are kept (as Python's `str.split(":")` keeps them).
pub fn split_outside(s: &str, sep: impl Fn(char) -> bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("${") {
            i += group_len(rest);
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        if sep(c) {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
        i += c.len_utf8();
    }
    out.push(&s[start..]);
    out
}

/// The whitespace-separated words of `s` (Python's `str.split()`), a `${...}` group kept
/// whole.
pub fn words(s: &str) -> Vec<&str> {
    split_outside(s, char::is_whitespace)
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect()
}

/// The `:`-separated entries of `s` (Python's `str.split(":")`, empty entries kept), a
/// `${...}` group kept whole.
pub fn colon_parts(s: &str) -> Vec<&str> {
    split_outside(s, |c| c == ':')
}

/// The `,`-separated fields of `s` (a `PACKAGECONFIG[f]` flag), a `${...}` group kept whole.
pub fn comma_fields(s: &str) -> Vec<&str> {
    split_outside(s, |c| c == ',')
}

/// Python's `re.split(r'(\s)', s)` with groups kept whole: runs of other characters and
/// single whitespace characters, in order (joining them gives `s` back).
pub fn split_keep_whitespace(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("${") {
            i += group_len(rest);
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        if c.is_whitespace() {
            if start < i {
                out.push(&s[start..i]);
            }
            out.push(&s[i..i + c.len_utf8()]);
            start = i + c.len_utf8();
        }
        i += c.len_utf8();
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// A short form of a word for a table cell: each `${@...}` group becomes `${@name(...)}`
/// (the text up to its first `(`, else `${@...}`); other text, `${VAR}` included, is kept.
/// The full text is at the statement's pointer.
pub fn abbreviate(word: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < word.len() {
        let rest = &word[i..];
        if rest.starts_with("${@") {
            let n = group_len(rest);
            let end = if rest[..n].ends_with('}') { n - 1 } else { n };
            let inner = &rest[3..end.max(3)];
            match inner.find('(') {
                Some(p) if p > 0 && !inner[..p].contains(char::is_whitespace) => {
                    out.push_str("${@");
                    out.push_str(&inner[..p]);
                    out.push_str("(...)}");
                }
                _ => out.push_str("${@...}"),
            }
            i += n;
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The text of `s` that names something statically: the pieces outside `${...}` groups, and
/// the contents of the quoted strings inside each `${@...}` group (the values python may
/// add, never its code), in order. A `${VAR}` group gives nothing.
/// `pn-${PN}` -> `["pn-"]`; `linux${@['', 'eabi'][x]}` -> `["linux", "", "eabi"]`.
pub fn visible_text(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("${") {
            if start < i {
                out.push(&s[start..i]);
            }
            let n = group_len(rest);
            if rest.as_bytes().get(2) == Some(&b'@') {
                quoted_strings(&rest[..n], &mut out);
            }
            i += n;
            start = i;
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        i += c.len_utf8();
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The contents of the `'...'` / `"..."` strings in python text, `\` escapes skipped.
fn quoted_strings<'a>(py: &'a str, out: &mut Vec<&'a str>) {
    let b = py.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let q = b[i];
        if q == b'\'' || q == b'"' {
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != q {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            out.push(&py[start..i.min(py.len())]);
        }
        i += 1;
    }
}

/// A word that holds an unevaluated `${...}` group: not a known name.
pub fn is_unevaluated(word: &str) -> bool {
    word.contains("${")
}

/// The known words of `s`: [`words`] without the unevaluated ones.
pub fn known_words(s: &str) -> Vec<&str> {
    words(s)
        .into_iter()
        .filter(|w| !is_unevaluated(w))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_with_spaces_is_one_word() {
        // curl_8.19.0.bb:175 (B6).
        let v = "pkgconf-native ${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '', d)} zlib";
        assert_eq!(
            words(v),
            [
                "pkgconf-native",
                "${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '', d)}",
                "zlib"
            ]
        );
        assert_eq!(known_words(v), ["pkgconf-native", "zlib"]);
    }

    #[test]
    fn colons_inside_python_do_not_split() {
        // OE-core's ti33x MACHINEOVERRIDES (B4).
        let v = "pn-curl:${@['', 'ti-soc:ti33x:'][d.getVar('X') != '']}dogbonedark:kiss";
        assert_eq!(
            colon_parts(v),
            [
                "pn-curl",
                "${@['', 'ti-soc:ti33x:'][d.getVar('X') != '']}dogbonedark",
                "kiss"
            ]
        );
        assert_eq!(colon_parts("a::b"), ["a", "", "b"]);
        assert_eq!(colon_parts(""), [""]);
    }

    #[test]
    fn nested_braces_and_quoted_braces() {
        assert_eq!(
            words("${@'}' if x else '{'} b"),
            ["${@'}' if x else '{'}", "b"]
        );
        assert_eq!(
            words("${@{'a': 'b c'}['a']} d"),
            ["${@{'a': 'b c'}['a']}", "d"]
        );
        assert_eq!(words("${A} ${B}x y"), ["${A}", "${B}x", "y"]);
        // Unterminated: the rest is one word.
        assert_eq!(words("a ${@foo( b c"), ["a", "${@foo( b c"]);
    }

    #[test]
    fn keep_whitespace_round_trips() {
        let v = " a  ${@x(' ', y)}\tb ";
        let parts = split_keep_whitespace(v);
        assert_eq!(parts.concat(), v);
        assert!(parts.contains(&"${@x(' ', y)}"));
    }

    #[test]
    fn abbreviated_python() {
        assert_eq!(
            abbreviate("${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '', d)}"),
            "${@bb.utils.contains(...)}"
        );
        assert_eq!(
            abbreviate("${@['', 'ti-soc:ti33x:'][d.getVar('X') != '']}dogbonedark"),
            "${@...}dogbonedark"
        );
        assert_eq!(abbreviate("pn-${PN}"), "pn-${PN}");
    }

    #[test]
    fn visible_text_keeps_literals_and_python_strings() {
        assert_eq!(visible_text("pn-${PN}"), ["pn-"]);
        assert_eq!(
            visible_text("linux${@['', 'eabi'][d.getVar('X') != '']}x"),
            ["linux", "", "eabi", "X", "", "x"]
        );
        assert_eq!(
            visible_text("${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}"),
            ["TUNE_FEATURES", "armv7a", "armv7a:", ""]
        );
        assert_eq!(visible_text("a:b"), ["a:b"]);
        assert!(visible_text("${A}").is_empty());
    }

    #[test]
    fn commas_inside_python_do_not_split() {
        assert_eq!(
            comma_fields("--a,--b,${@bb.utils.contains('X', 'y', 'z', '', d)},r"),
            [
                "--a",
                "--b",
                "${@bb.utils.contains('X', 'y', 'z', '', d)}",
                "r"
            ]
        );
    }
}
