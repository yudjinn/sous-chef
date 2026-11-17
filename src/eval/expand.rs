//! `${VAR}` expansion with cycle detection; `${@...}` is left unevaluated.
//!
//! Reproduces `DataSmart.expandWithRefs` (`lib/bb/data_smart.py`): every `${NAME}` whose name
//! matches `[a-zA-Z0-9\-_+./~:]+` is replaced by the *finalised* value of `NAME`, and the pass
//! is repeated until the text stops changing (so `${${A}}` resolves `A` first, then the result).
//! Differences, all reported in the returned [`Value`] instead of guessed:
//!
//! - `${@...}` is never evaluated. It stays as text and sets `unevaluated_python`. Variable
//!   references inside it are still expanded, as BitBake does before running the code.
//! - An undefined variable stays as `${NAME}` (BitBake does the same) and `NAME` is listed in
//!   `unresolved`.
//! - A reference cycle (`A = "${B}"`, `B = "${A}"`, or `A = "${A} x"`) is an error in BitBake.
//!   Here the reference that closes the cycle is left in place and listed in `unresolved`.
//!
//! Two entry points:
//!
//! - [`expand`] takes a closure returning the *unexpanded* text of a variable and expands
//!   recursively. Used for tests and for anything that has raw text only.
//! - [`Expander`] with a [`Source`] lets the lookup return an already finalised value (the
//!   datastore implements `Source`: a referenced variable gets its overrides, appends and
//!   removes before substitution, as in BitBake). The `Expander` carries the cycle stack and a
//!   cache across the recursion.

use std::collections::HashMap;

use super::Value;

/// Maximum number of substitution passes over one text. BitBake loops until the text stops
/// changing; the limit only guards against pathological growth.
const MAX_PASSES: usize = 64;

/// Maximum nesting depth of variable lookups (each nested reference adds one).
const MAX_DEPTH: usize = 200;

/// Where an [`Expander`] gets the value of a referenced variable.
pub trait Source {
    /// The finalised, expanded value of `name`, or `None` if it is undefined. Implementations
    /// that need to expand text must do it through `exp` so cycles are detected.
    fn lookup(&self, name: &str, exp: &mut Expander) -> Option<Value>;
}

/// Expansion state shared by one top-level expansion and all the lookups it triggers.
#[derive(Debug, Default)]
pub struct Expander {
    /// Variables whose value is being computed, outermost first.
    stack: Vec<String>,
    /// Finalised values already computed in this expansion. Only results computed without
    /// hitting a cycle are cached, since a cycle cuts the result differently depending on
    /// where it was entered.
    cache: HashMap<String, Option<Value>>,
    /// Number of cycles hit so far.
    cycles: usize,
    /// Variables found to be part of a cycle: their references are left in place from then
    /// on, so a cut value substituted into a later pass cannot grow without end.
    cyclic: Vec<String>,
}

impl Expander {
    pub fn new() -> Expander {
        Expander::default()
    }

    /// Expands `text`, which is not the value of any variable (no owner for cycle detection).
    pub fn expand(&mut self, text: &str, src: &dyn Source) -> Value {
        self.expand_text(text, src)
    }

    /// Expands `text` as the value of `owner`: a reference to `owner` inside it is a cycle.
    /// (BitBake: "variable X references itself!")
    pub fn expand_as(&mut self, owner: &str, text: &str, src: &dyn Source) -> Value {
        let pushed = !self.stack.iter().any(|s| s == owner);
        if pushed {
            self.stack.push(owner.to_string());
        }
        let v = self.expand_text(text, src);
        if pushed {
            self.stack.pop();
        }
        v
    }

    /// True if `name` is currently being computed (a lookup of it now would be a cycle).
    pub fn in_progress(&self, name: &str) -> bool {
        self.stack.iter().any(|s| s == name)
    }

    fn expand_text(&mut self, text: &str, src: &dyn Source) -> Value {
        let mut out = Value {
            text: text.to_string(),
            unevaluated_python: false,
            unresolved: Vec::new(),
        };
        for _ in 0..MAX_PASSES {
            if !out.text.contains("${") {
                break;
            }
            let cur = std::mem::take(&mut out.text);
            let next = self.substitute_once(&cur, src, &mut out);
            out.text = cur;
            if next == out.text {
                break;
            }
            out.text = next;
        }
        if has_python(&out.text) {
            out.unevaluated_python = true;
        }
        out
    }

    /// One pass of `__expand_var_regexp__.sub(var_sub, s)`.
    fn substitute_once(&mut self, s: &str, src: &dyn Source, acc: &mut Value) -> String {
        let mut result = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(pos) = rest.find("${") {
            result.push_str(&rest[..pos]);
            let after = &rest[pos + 2..];
            match var_ref_len(after) {
                Some(n) => {
                    let name = &after[..n];
                    match self.resolve(name, src) {
                        Some(v) => {
                            if v.unevaluated_python {
                                acc.unevaluated_python = true;
                            }
                            for u in &v.unresolved {
                                push_unique(&mut acc.unresolved, u);
                            }
                            result.push_str(&v.text);
                        }
                        None => {
                            push_unique(&mut acc.unresolved, name);
                            result.push_str(&rest[pos..pos + 3 + n]);
                        }
                    }
                    rest = &after[n + 1..];
                }
                None => {
                    // Not a variable reference here (`${@`, `${${`, `${}`, unterminated):
                    // keep the `$` and rescan from the next character, as a regex would.
                    result.push('$');
                    rest = &rest[pos + 1..];
                }
            }
        }
        result.push_str(rest);
        result
    }

    /// The value to substitute for `${name}`, or `None` to leave the reference in place
    /// (undefined, or a cycle).
    fn resolve(&mut self, name: &str, src: &dyn Source) -> Option<Value> {
        if self.in_progress(name) || self.stack.len() >= MAX_DEPTH {
            self.cycles += 1;
            push_unique(&mut self.cyclic, name);
            return None;
        }
        if self.cyclic.iter().any(|c| c == name) {
            return None;
        }
        if let Some(v) = self.cache.get(name) {
            return v.clone();
        }
        let before = self.cycles;
        self.stack.push(name.to_string());
        let v = src.lookup(name, self);
        self.stack.pop();
        if self.cycles == before {
            self.cache.insert(name.to_string(), v.clone());
        }
        v
    }
}

/// Expands `text` against `lookup`, which returns the **unexpanded** text of a variable (or
/// `None` if undefined); referenced values are expanded recursively.
pub fn expand(text: &str, lookup: impl Fn(&str) -> Option<String>) -> Value {
    struct Raw<F>(F);
    impl<F: Fn(&str) -> Option<String>> Source for Raw<F> {
        fn lookup(&self, name: &str, exp: &mut Expander) -> Option<Value> {
            let raw = (self.0)(name)?;
            Some(exp.expand_as(name, &raw, self))
        }
    }
    Expander::new().expand(text, &Raw(lookup))
}

/// Length of the variable name at the start of `s` (the text after `${`) if `s` starts with a
/// name matching `[a-zA-Z0-9\-_+./~:]+` followed by `}`.
fn var_ref_len(s: &str) -> Option<usize> {
    let n = s
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || b"-_+./~:".contains(b))
        .count();
    if n > 0 && s.as_bytes().get(n) == Some(&b'}') {
        Some(n)
    } else {
        None
    }
}

/// True if `s` contains a python expansion, i.e. matches BitBake's
/// `__expand_python_regexp__` = `\$\{@(?:\{.*?\}|.)+?\}`. With backtracking that regex
/// matches `${@` followed by at least one character and then a `}`, all on one line.
pub fn has_python(s: &str) -> bool {
    let mut rest = s;
    while let Some(pos) = rest.find("${@") {
        let after = &rest[pos + 3..];
        let line = after.split('\n').next().unwrap_or("");
        let mut chars = line.chars();
        if chars.next().is_some() && chars.as_str().contains('}') {
            return true;
        }
        rest = &rest[pos + 1..];
    }
    false
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn plain_text_is_unchanged() {
        let v = expand("no refs $ here {}", vars(&[]));
        assert_eq!(v.text, "no refs $ here {}");
        assert!(!v.unevaluated_python);
        assert!(v.unresolved.is_empty());
    }

    #[test]
    fn simple_and_nested_references() {
        let l = vars(&[("A", "a${B}"), ("B", "b${C}"), ("C", "c")]);
        assert_eq!(expand("${A}-${C}", &l).text, "abc-c");
    }

    #[test]
    fn reference_built_from_reference() {
        // `${${A}}`: the inner reference resolves first, the next pass resolves the result.
        let l = vars(&[("A", "B"), ("B", "final")]);
        let v = expand("${${A}}", &l);
        assert_eq!(v.text, "final");
        assert!(v.unresolved.is_empty());
    }

    #[test]
    fn undefined_stays_and_is_listed() {
        let l = vars(&[("A", "x ${MISSING} ${MISSING} ${OTHER}")]);
        let v = expand("${A}", &l);
        assert_eq!(v.text, "x ${MISSING} ${MISSING} ${OTHER}");
        assert_eq!(v.unresolved, vec!["MISSING", "OTHER"]);
    }

    #[test]
    fn two_variable_cycle_is_cut_and_reported() {
        let l = vars(&[("A", "a${B}"), ("B", "b${A}")]);
        let v = expand("${A}", &l);
        assert_eq!(v.text, "ab${A}");
        assert_eq!(v.unresolved, vec!["A"]);
    }

    #[test]
    fn self_reference_is_a_cycle() {
        let l = vars(&[("A", "${A} x")]);
        let v = expand("${A}", &l);
        assert_eq!(v.text, "${A} x");
        assert_eq!(v.unresolved, vec!["A"]);
    }

    #[test]
    fn python_is_left_as_text_and_flagged() {
        let l = vars(&[("V", "${@d.getVar('X') or ''}-${B}"), ("B", "b")]);
        let v = expand("${V}", &l);
        assert_eq!(v.text, "${@d.getVar('X') or ''}-b");
        assert!(v.unevaluated_python);
        assert!(v.unresolved.is_empty());
    }

    #[test]
    fn references_inside_python_are_expanded() {
        let l = vars(&[("B", "bee")]);
        let v = expand("${@'${B}'.upper()}", &l);
        assert_eq!(v.text, "${@'bee'.upper()}");
        assert!(v.unevaluated_python);
    }

    #[test]
    fn python_flag_propagates_through_references() {
        let l = vars(&[
            ("A", "${B}"),
            ("B", "${@bb.utils.contains('X', 'y', 'z', '', d)}"),
        ]);
        assert!(expand("${A}", &l).unevaluated_python);
    }

    #[test]
    fn python_regex_edges() {
        assert!(has_python("${@x}"));
        assert!(has_python("a ${@{'k': 1}['k']} b"));
        assert!(!has_python("${@}"));
        assert!(has_python("${@}}"));
        assert!(!has_python("${@x\n}"));
        assert!(!has_python("${X}"));
    }

    #[test]
    fn not_a_reference() {
        let l = vars(&[("A", "a")]);
        for s in ["${}", "${A", "${A B}", "$A", "${ A}"] {
            let v = expand(s, &l);
            assert_eq!(v.text, s, "{s}");
            assert!(v.unresolved.is_empty(), "{s}");
        }
    }

    #[test]
    fn allowed_name_characters() {
        let l = vars(&[("a-b_c+d.e/f~g:h", "ok"), ("X", "1")]);
        assert_eq!(expand("${a-b_c+d.e/f~g:h}${X}", &l).text, "ok1");
    }

    #[test]
    fn name_expansion_uses_the_same_function() {
        let l = vars(&[("PN", "hello"), ("BPN", "${PN}")]);
        let v = expand("FILES:${PN}-dev", &l);
        assert_eq!(v.text, "FILES:hello-dev");
        let v = expand("RDEPENDS:${BPN}-ptest:append", &l);
        assert_eq!(v.text, "RDEPENDS:hello-ptest:append");
        let v = expand("FILES:${UNSET}-dev", &l);
        assert_eq!(v.text, "FILES:${UNSET}-dev");
        assert_eq!(v.unresolved, vec!["UNSET"]);
    }

    #[test]
    fn diamond_is_not_a_cycle() {
        let l = vars(&[("A", "${B}${C}"), ("B", "${D}"), ("C", "${D}"), ("D", "d")]);
        let v = expand("${A}", &l);
        assert_eq!(v.text, "dd");
        assert!(v.unresolved.is_empty());
    }

    #[test]
    fn unicode_text_survives() {
        let l = vars(&[("A", "ü")]);
        assert_eq!(expand("é${A}ß${", &l).text, "éüß${");
    }
}
