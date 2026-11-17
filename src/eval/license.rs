//! OE-core's license expressions, as far as the `INCOMPATIBLE_LICENSE` check needs them
//! (OE-core Wrynose `33a6bd6c`, `meta/lib/oe/license.py`).
//!
//! `LicenseVisitor.get_elements` splits the string on the operator characters `&|() `,
//! drops blank elements, quotes each license name (`[a-zA-Z0-9.+_\-]+`, inserting `&`
//! between two names in a row) and rejects anything else (`InvalidLicense`); the result is
//! parsed as a Python expression and walked by `FlattenVisitor`: a name is a license, `a |
//! b` keeps the licenses of the side `choose_licenses(left, right)` picks, anything else
//! (`&`, parentheses) keeps both sides in order. Python's precedence applies: `&` binds
//! tighter than `|`, both left to right.
//!
//! [`flatten`] parses the plain grammar `expr := term ('|' term)*`, `term := factor ('&'
//! factor)*`, `factor := NAME | '(' expr ')'` and returns an error for anything else (an
//! invalid character, which BitBake reports with `bb.fatal`, or a form Python would parse
//! some other way, such as `A (B)`, a call): the caller then says it cannot tell.

/// `license_operator_chars` plus the name pattern.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Name(String),
    And,
    Or,
    Open,
    Close,
}

fn tokens(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<Tok>| -> Result<(), String> {
        if cur.is_empty() {
            return Ok(());
        }
        if !cur.chars().all(is_name_char) {
            return Err(format!("invalid characters in license '{cur}'"));
        }
        if matches!(out.last(), Some(Tok::Name(_))) {
            out.push(Tok::And);
        }
        out.push(Tok::Name(std::mem::take(cur)));
        Ok(())
    };
    for c in s.chars() {
        let op = match c {
            '&' => Some(Tok::And),
            '|' => Some(Tok::Or),
            '(' => Some(Tok::Open),
            ')' => Some(Tok::Close),
            ' ' => None,
            _ => {
                cur.push(c);
                continue;
            }
        };
        flush(&mut cur, &mut out)?;
        if let Some(t) = op {
            out.push(t);
        }
    }
    flush(&mut cur, &mut out)?;
    Ok(out)
}

/// `flattened_licenses(s, choose)`: `choose(left, right)` returns true to keep the left side
/// of an `|`.
pub(crate) fn flatten(
    s: &str,
    choose: &dyn Fn(&[String], &[String]) -> bool,
) -> Result<Vec<String>, String> {
    let toks = tokens(s)?;
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        choose,
    };
    let out = p.expr()?;
    if p.pos != toks.len() {
        return Err(format!("cannot parse the license expression '{s}'"));
    }
    Ok(out)
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    choose: &'a dyn Fn(&[String], &[String]) -> bool,
}

impl Parser<'_> {
    fn expr(&mut self) -> Result<Vec<String>, String> {
        let mut left = self.term()?;
        while self.toks.get(self.pos) == Some(&Tok::Or) {
            self.pos += 1;
            let right = self.term()?;
            if !(self.choose)(&left, &right) {
                left = right;
            }
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Vec<String>, String> {
        let mut out = self.factor()?;
        while self.toks.get(self.pos) == Some(&Tok::And) {
            self.pos += 1;
            out.extend(self.factor()?);
        }
        Ok(out)
    }

    fn factor(&mut self) -> Result<Vec<String>, String> {
        match self.toks.get(self.pos) {
            Some(Tok::Name(n)) => {
                self.pos += 1;
                Ok(vec![n.clone()])
            }
            Some(Tok::Open) => {
                self.pos += 1;
                let e = self.expr()?;
                if self.toks.get(self.pos) != Some(&Tok::Close) {
                    return Err("unbalanced parentheses in the license expression".into());
                }
                self.pos += 1;
                Ok(e)
            }
            _ => Err("cannot parse the license expression".into()),
        }
    }
}

/// Python's `fnmatch.fnmatchcase`: `*`, `?`, `[seq]`, `[!seq]`; everything else literal.
pub(crate) fn fnmatch(name: &str, pat: &str) -> bool {
    let mut re = String::from("(?s)^");
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                let mut j = i;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    re.push_str("\\[");
                } else {
                    let mut set: String = chars[i..j].iter().collect();
                    i = j + 1;
                    if let Some(rest) = set.strip_prefix('!') {
                        set = format!("^{rest}");
                    }
                    re.push('[');
                    re.push_str(&set.replace('\\', "\\\\"));
                    re.push(']');
                }
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).is_ok_and(|r| r.is_match(name))
}

/// `obsolete_license_list()`.
pub(crate) const OBSOLETE: &[&str] = &[
    "AGPL-3",
    "AGPL-3+",
    "AGPLv3",
    "AGPLv3+",
    "AGPLv3.0",
    "AGPLv3.0+",
    "AGPL-3.0",
    "AGPL-3.0+",
    "BSD-0-Clause",
    "GPL-1",
    "GPL-1+",
    "GPLv1",
    "GPLv1+",
    "GPLv1.0",
    "GPLv1.0+",
    "GPL-1.0",
    "GPL-1.0+",
    "GPL-2",
    "GPL-2+",
    "GPLv2",
    "GPLv2+",
    "GPLv2.0",
    "GPLv2.0+",
    "GPL-2.0",
    "GPL-2.0+",
    "GPL-3",
    "GPL-3+",
    "GPLv3",
    "GPLv3+",
    "GPLv3.0",
    "GPLv3.0+",
    "GPL-3.0",
    "GPL-3.0+",
    "LGPLv2",
    "LGPLv2+",
    "LGPLv2.0",
    "LGPLv2.0+",
    "LGPL-2.0",
    "LGPL-2.0+",
    "LGPL2.1",
    "LGPL2.1+",
    "LGPLv2.1",
    "LGPLv2.1+",
    "LGPL-2.1",
    "LGPL-2.1+",
    "LGPLv3",
    "LGPLv3+",
    "LGPL-3.0",
    "LGPL-3.0+",
    "MPL-1",
    "MPLv1",
    "MPLv1.1",
    "MPLv2",
    "MIT-X",
    "MIT-style",
    "openssl",
    "PSF",
    "PSFv2",
    "Python-2",
    "Apachev2",
    "Apache-2",
    "Artisticv1",
    "Artistic-1",
    "AFL-2",
    "AFL-1",
    "AFLv2",
    "AFLv1",
    "CDDLv1",
    "CDDL-1",
    "EPLv1.0",
    "FreeType",
    "Nauman",
    "tcl",
    "vim",
    "SGIv1",
];

/// `expand_wildcard_licenses`: the three wildcards OE-core knows replaced by their two
/// SPDX names; `Err` with BitBake's fatal message for an obsolete name or another `*`.
pub(crate) fn expand_wildcards(bad: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for b in bad {
        let add: Vec<String> = match b.as_str() {
            "AGPL-3.0*" => vec!["AGPL-3.0-only".into(), "AGPL-3.0-or-later".into()],
            "GPL-3.0*" => vec!["GPL-3.0-only".into(), "GPL-3.0-or-later".into()],
            "LGPL-3.0*" => vec!["LGPL-3.0-only".into(), "LGPL-3.0-or-later".into()],
            _ => vec![b.clone()],
        };
        for a in add {
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    for l in &out {
        if OBSOLETE.contains(&l.as_str()) {
            return Err(format!(
                "Error, {l} is an obsolete license, please use an SPDX reference in INCOMPATIBLE_LICENSE"
            ));
        }
        if l.contains('*') {
            return Err(format!("Error, {l} is an invalid license wildcard entry"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(_: &[String], _: &[String]) -> bool {
        true
    }
    fn right(_: &[String], _: &[String]) -> bool {
        false
    }

    #[test]
    fn flatten_follows_python_precedence() {
        assert_eq!(flatten("MIT", &left).unwrap(), vec!["MIT"]);
        assert_eq!(
            flatten("MIT GPL-2.0-only", &left).unwrap(),
            vec!["MIT", "GPL-2.0-only"]
        );
        assert_eq!(flatten("A | B & C", &left).unwrap(), vec!["A"]);
        assert_eq!(flatten("A | B & C", &right).unwrap(), vec!["B", "C"]);
        assert_eq!(flatten("(A | B) & C", &right).unwrap(), vec!["B", "C"]);
        assert!(flatten("A ${@x}", &left).is_err());
        assert!(flatten("A (B)", &left).is_err());
        assert!(flatten("A &", &left).is_err());
    }

    #[test]
    fn fnmatch_like_python() {
        assert!(fnmatch("GPL-3.0-only", "GPL-3.0*"));
        assert!(fnmatch("GPL-3.0-only", "GPL-3.0-only"));
        assert!(!fnmatch("gpl-3.0-only", "GPL-3.0-only"));
        assert!(fnmatch("GPL-2.0", "GPL-?.0"));
        assert!(fnmatch("GPL-2.0", "GPL-[12].0"));
        assert!(!fnmatch("GPL-3.0", "GPL-[!3].0"));
    }

    #[test]
    fn wildcards_expand_and_obsolete_names_are_fatal() {
        let w = expand_wildcards(&["GPL-3.0*".into(), "MIT".into()]).unwrap();
        assert_eq!(w, vec!["GPL-3.0-only", "GPL-3.0-or-later", "MIT"]);
        assert!(expand_wildcards(&["GPLv3".into()]).is_err());
        assert!(expand_wildcards(&["BSD-*".into()]).is_err());
    }
}
