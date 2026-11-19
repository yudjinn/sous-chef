//! `${VAR}` expansion with cycle detection; `${@...}` evaluated for a strict subset.
//!
//! Reproduces `DataSmart.expandWithRefs` (`lib/bb/data_smart.py`): every `${NAME}` whose name
//! matches `[a-zA-Z0-9\-_+./~:]+` is replaced by the *finalised* value of `NAME`, then every
//! `${@code}` (`__expand_python_regexp__`) by `str(eval(code))`, and the pass is repeated
//! until the text stops changing (so `${${A}}` resolves `A` first, then the result).
//! Differences, all reported in the returned [`Value`] instead of guessed:
//!
//! - `${@code}` is evaluated only when `code` is in the subset of `pyexpr` (pure helpers such
//!   as `bb.utils.contains`) **and** every value it reads is certain: no python left in it, no
//!   undefined variable, no uncertain override decision, not changed by anonymous python, and
//!   no reference substituted into the code that is itself uncertain. Otherwise it stays as
//!   text and sets `unevaluated_python`, as before. Each attempt, evaluated or not (with the
//!   reason), is recorded in [`Expander::python_log`]. As in BitBake, code that still holds a
//!   `${VAR}` reference is not run, and when some code of the pass has an unterminated string
//!   (a `SyntaxError` BitBake swallows for the whole pass) nothing of that pass is replaced.
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
use std::sync::LazyLock;

use super::Value;
use super::classfn::ClassFn;
use super::pyexpr::{self, Stop};

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

    /// For python's `d.getVar(name)` of a variable [`Source::lookup`] found undefined: why it
    /// may be set after all (anonymous python, an override decision that is not certain, the
    /// environment), or `None` when it is certainly unset.
    fn unset_doubt(&self, _name: &str, _exp: &mut Expander) -> Option<String> {
        None
    }

    /// `d.getVarFlag(name, flag)`: `Some(Some(v))` the expanded flag, `Some(None)` unset,
    /// `None` when this source cannot answer (python reading a flag is then not evaluated).
    fn lookup_flag(&self, _name: &str, _flag: &str, _exp: &mut Expander) -> Option<Option<Value>> {
        None
    }

    /// Whether the class function `f` may be called here (see [`super::classfn`]): `Err`
    /// says why not. By default no definition is known.
    fn class_fn(&self, f: ClassFn) -> Result<(), String> {
        Err(format!(
            "{} is a class function whose definition is not known here",
            f.name()
        ))
    }

    /// `bb.data.inherits_class(klass, d)`; `None` when not certain.
    fn inherits_class(&self, _klass: &str) -> Option<bool> {
        None
    }
}

/// One `${@...}` the expansion met: evaluated statically or left, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyEvent {
    /// The code, as BitBake would run it (references already substituted).
    pub code: String,
    /// It was replaced by its value.
    pub evaluated: bool,
    /// Why it was not evaluated.
    pub why: Option<String>,
}

/// BitBake's `__expand_python_regexp__`.
static PYTHON_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"\$\{@(?:\{.*?\}|.)+?\}").ok());

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
    /// Every `${@...}` met, in order (see [`PyEvent`]).
    python_log: Vec<PyEvent>,
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

    /// Marks `name` as being computed, so a reference to it from now on is a cycle. Returns
    /// whether it was pushed; pass that to [`Expander::leave`].
    pub fn enter(&mut self, name: &str) -> bool {
        if self.in_progress(name) {
            false
        } else {
            self.stack.push(name.to_string());
            true
        }
    }

    /// Undoes [`Expander::enter`].
    pub fn leave(&mut self, entered: bool) {
        if entered {
            self.stack.pop();
        }
    }

    /// True if `name` is currently being computed (a lookup of it now would be a cycle).
    pub fn in_progress(&self, name: &str) -> bool {
        self.stack.iter().any(|s| s == name)
    }

    /// The `${@...}` expressions met so far, in order.
    pub fn python_log(&self) -> &[PyEvent] {
        &self.python_log
    }

    fn expand_text(&mut self, text: &str, src: &dyn Source) -> Value {
        let mut out = Value {
            text: text.to_string(),
            unevaluated_python: false,
            unresolved: Vec::new(),
        };
        // An uncertain value was substituted in an earlier pass: python of a later pass may
        // have been built from it, so it is not evaluated.
        let mut tainted_before = false;
        for _ in 0..MAX_PASSES {
            if !out.text.contains("${") {
                break;
            }
            let cur = std::mem::take(&mut out.text);
            let mut taints = Vec::new();
            let subst = self.substitute_once(&cur, src, &mut out, &mut taints);
            let next = self.python_once(&subst, src, &taints, tainted_before);
            tainted_before |= !taints.is_empty();
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

    /// One pass of `__expand_var_regexp__.sub(var_sub, s)`. Each substituted value that is
    /// not certain is recorded in `taints` (its byte range in the result, and the name).
    fn substitute_once(
        &mut self,
        s: &str,
        src: &dyn Source,
        acc: &mut Value,
        taints: &mut Vec<(usize, usize, String)>,
    ) -> String {
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
                            if !certain(&v) {
                                let start = result.len();
                                taints.push((start, start + v.text.len(), name.to_string()));
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

    /// One pass of `__expand_python_regexp__.sub(python_sub, s)`, for the expressions of the
    /// subset whose inputs are certain; the others are kept as they are.
    fn python_once(
        &mut self,
        s: &str,
        src: &dyn Source,
        taints: &[(usize, usize, String)],
        tainted_before: bool,
    ) -> String {
        if !s.contains("${@") {
            return s.to_string();
        }
        let Some(re) = PYTHON_RE.as_ref() else {
            return s.to_string();
        };
        let matches: Vec<(usize, usize)> = re.find_iter(s).map(|m| (m.start(), m.end())).collect();
        // `python_sub` leaves code with a `${VAR}` in it alone; any other code with an
        // unterminated string raises the `SyntaxError` that `expandWithRefs` swallows for the
        // whole pass, so nothing of the pass is replaced.
        let code = |&(a, b): &(usize, usize)| &s[a + 3..b - 1];
        let runnable: Vec<bool> = matches.iter().map(|m| !has_var_ref(code(m))).collect();
        let swallowed = matches
            .iter()
            .zip(&runnable)
            .any(|(m, r)| *r && pyexpr::has_unterminated_string(code(m)));
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for (m, run) in matches.iter().zip(runnable) {
            let (a, b) = *m;
            out.push_str(&s[last..a]);
            last = b;
            if !run {
                out.push_str(&s[a..b]);
                continue;
            }
            let taint = taints
                .iter()
                .find(|(ta, tb, _)| ta < tb && *ta < b && a < *tb)
                .map(|(_, _, n)| n.clone());
            let r = if swallowed {
                Err("some python in this text has an unterminated string, so BitBake replaces none of it".to_string())
            } else if let Some(n) = taint {
                Err(format!("{n}, substituted into it, is not certain"))
            } else if tainted_before {
                Err("it was built from a value that is not certain".to_string())
            } else {
                match self.run_python(code(m), src) {
                    Ok(t) => Ok(t),
                    Err(Stop::Uncertain(w)) => Err(w),
                    Err(Stop::Unsupported(w)) => Err(format!("not in the evaluated subset ({w})")),
                }
            };
            match r {
                Ok(t) => {
                    self.log(code(m), None);
                    out.push_str(&t);
                }
                Err(w) => {
                    self.log(code(m), Some(w));
                    out.push_str(&s[a..b]);
                }
            }
        }
        out.push_str(&s[last..]);
        out
    }

    fn log(&mut self, code: &str, why: Option<String>) {
        self.python_log.push(PyEvent {
            code: code.trim_matches(pyexpr::py_space).to_string(),
            evaluated: why.is_none(),
            why,
        });
    }

    fn run_python(&mut self, code: &str, src: &dyn Source) -> Result<String, Stop> {
        let v = self.python_value(code, src)?;
        pyexpr::to_text(&v)
    }

    /// The python value of the expression `code` (the [`pyexpr`] subset), reading only
    /// certain values from `src`, as `${@code}` would compute it before `str()`. Used for
    /// the conditions of anonymous python (`eval::anon`).
    pub(crate) fn python_value(
        &mut self,
        code: &str,
        src: &dyn Source,
    ) -> Result<pyexpr::PyVal, Stop> {
        let e = pyexpr::parse(code)?;
        let mut env = ExpEnv { exp: self, src };
        pyexpr::eval(&e, &mut env)
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

/// What python's `d` reads during an expansion: certain values only.
struct ExpEnv<'a, 'b> {
    exp: &'a mut Expander,
    src: &'b dyn Source,
}

impl pyexpr::Env for ExpEnv<'_, '_> {
    fn get_var(&mut self, name: &str) -> Result<Option<String>, Stop> {
        let exp = &mut *self.exp;
        if exp.in_progress(name)
            || exp.cyclic.iter().any(|c| c == name)
            || exp.stack.len() >= MAX_DEPTH
        {
            // `d.getVar` of the variable being expanded: a recursion error in BitBake.
            exp.cycles += 1;
            return Err(Stop::Uncertain(format!(
                "{name} refers back to the value being expanded"
            )));
        }
        match exp.resolve(name, self.src) {
            Some(v) if certain(&v) => Ok(Some(v.text)),
            Some(v) => Err(Stop::Uncertain(why_uncertain(name, &v))),
            None => match self.src.unset_doubt(name, exp) {
                Some(w) => Err(Stop::Uncertain(format!("{name} is unset here, but {w}"))),
                None => Ok(None),
            },
        }
    }

    fn get_var_flag(&mut self, name: &str, flag: &str) -> Result<Option<String>, Stop> {
        match self.src.lookup_flag(name, flag, self.exp) {
            None => Err(Stop::Unsupported("d.getVarFlag here".into())),
            Some(None) => Ok(None),
            Some(Some(v)) if certain(&v) => Ok(Some(v.text)),
            Some(Some(v)) => Err(Stop::Uncertain(why_uncertain(
                &format!("{name}[{flag}]"),
                &v,
            ))),
        }
    }

    fn expand(&mut self, text: &str) -> Result<String, Stop> {
        let v = self.exp.expand_text(text, self.src);
        if certain(&v) {
            Ok(v.text)
        } else {
            Err(Stop::Uncertain(why_uncertain("the text of d.expand", &v)))
        }
    }

    fn class_fn(&mut self, f: ClassFn) -> Result<(), Stop> {
        self.src.class_fn(f).map_err(Stop::Unsupported)
    }

    fn inherits_class(&mut self, klass: &str) -> Result<bool, Stop> {
        self.src.inherits_class(klass).ok_or_else(|| {
            Stop::Uncertain(format!(
                "whether the recipe inherits {klass} is not certain (a reference that could inherit a class was not followed, or followed with an uncertain value)"
            ))
        })
    }
}

/// No python left, no undefined variable, no uncertain decision.
pub(crate) fn certain(v: &Value) -> bool {
    !v.unevaluated_python && v.unresolved.is_empty()
}

fn why_uncertain(name: &str, v: &Value) -> String {
    if v.unevaluated_python && has_python(&v.text) {
        format!("{name} holds python that is not evaluated")
    } else if v.unevaluated_python {
        format!(
            "{name} is not certain (anonymous python may change it, or an override decision on it depends on OVERRIDES, which is not known statically)"
        )
    } else {
        format!(
            "{name} uses undefined variables: {}",
            v.unresolved.join(", ")
        )
    }
}

/// `__expand_var_regexp__.findall(code)` is not empty.
fn has_var_ref(code: &str) -> bool {
    let mut rest = code;
    while let Some(p) = rest.find("${") {
        if var_ref_len(&rest[p + 2..]).is_some() {
            return true;
        }
        rest = &rest[p + 1..];
    }
    false
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

/// The names of the plain `${NAME}` references in `text`, in order, each once.
pub(crate) fn var_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(p) = rest.find("${") {
        let after = &rest[p + 2..];
        if let Some(n) = var_ref_len(after) {
            push_unique(&mut out, &after[..n]);
        }
        rest = &rest[p + 1..];
    }
    out
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
        let l = vars(&[("V", "${@os.uname()[4]}-${B}"), ("B", "b")]);
        let v = expand("${V}", &l);
        assert_eq!(v.text, "${@os.uname()[4]}-b");
        assert!(v.unevaluated_python);
        assert!(v.unresolved.is_empty());
    }

    #[test]
    fn python_of_the_subset_is_evaluated() {
        let l = vars(&[("V", "${@d.getVar('X') or 'none'}-${B}"), ("B", "b")]);
        let v = expand("${V}", &l);
        assert_eq!(v.text, "none-b");
        assert!(!v.unevaluated_python);
        let l = vars(&[
            ("F", "x11 pam"),
            ("V", "a${@bb.utils.contains('F', 'pam', ' p', '', d)}"),
        ]);
        assert_eq!(expand("${V}", &l).text, "a p");
    }

    #[test]
    fn references_inside_python_are_expanded() {
        let l = vars(&[("B", "bee")]);
        let v = expand("${@'${B}'.upper()}", &l);
        assert_eq!(v.text, "BEE");
        assert!(!v.unevaluated_python);
        let v = expand("${@'${B}'.title()}", &l);
        assert_eq!(v.text, "${@'bee'.title()}");
        assert!(v.unevaluated_python);
    }

    #[test]
    fn python_flag_propagates_through_references() {
        let l = vars(&[("A", "${B}"), ("B", "${@bb.utils.which('X', 'y')}")]);
        assert!(expand("${A}", &l).unevaluated_python);
        // python reading a value that holds python left unevaluated is not evaluated
        let l = vars(&[
            ("A", "${@bb.utils.contains('B', 'y', 'z', '', d)}"),
            ("B", "${@bb.utils.which('X', 'y')}"),
        ]);
        let mut exp = Expander::new();
        struct Raw<'a>(&'a dyn Fn(&str) -> Option<String>);
        impl Source for Raw<'_> {
            fn lookup(&self, name: &str, exp: &mut Expander) -> Option<Value> {
                let raw = (self.0)(name)?;
                Some(exp.expand_as(name, &raw, self))
            }
        }
        let v = exp.expand("${A}", &Raw(&l));
        assert!(v.unevaluated_python);
        assert_eq!(v.text, "${@bb.utils.contains('B', 'y', 'z', '', d)}");
        let log = exp.python_log();
        let a = log.iter().find(|e| e.code.starts_with("bb.utils.contains"));
        assert!(
            a.is_some_and(|e| !e.evaluated
                && e.why.as_deref() == Some("B holds python that is not evaluated"))
        );
    }

    #[test]
    fn substituted_uncertain_reference_blocks_python() {
        // `${U}` is substituted into the code; U is not certain (it holds python).
        let l = vars(&[("U", "${@os.getpid()}"), ("A", "${@'${U}' + 'x'}")]);
        let v = expand("${A}", &l);
        assert!(v.unevaluated_python);
        assert!(v.text.contains("+ 'x'"));
    }

    #[test]
    fn python_with_an_undefined_reference_is_not_run() {
        // BitBake's python_sub: code that still holds `${VAR}` is returned as it is.
        let l = vars(&[("A", "${@'${NOPE}' or 'x'}")]);
        let v = expand("${A}", &l);
        assert_eq!(v.text, "${@'${NOPE}' or 'x'}");
        assert_eq!(v.unresolved, vec!["NOPE"]);
    }

    #[test]
    fn python_reading_itself_is_a_cycle() {
        let l = vars(&[("A", "${@d.getVar('A') or 'x'}")]);
        let v = expand("${A}", &l);
        assert!(v.unevaluated_python);
        let l = vars(&[("A", "${@d.getVar('B')}"), ("B", "${@d.getVar('A')}")]);
        assert!(expand("${A}", &l).unevaluated_python);
    }

    #[test]
    fn unterminated_string_stops_the_whole_pass() {
        // `${@'}'}` is cut at the first `}`: code `'`, an unterminated string; BitBake swallows
        // that SyntaxError for the whole substitution, so the other expression stays too.
        let l = vars(&[("A", "${@'a'} ${@'}'}")]);
        let v = expand("${A}", &l);
        assert_eq!(v.text, "${@'a'} ${@'}'}");
        assert!(v.unevaluated_python);
    }

    #[test]
    fn python_result_is_expanded_again() {
        let l = vars(&[("A", "${@'$' + '{B}'}"), ("B", "bee")]);
        assert_eq!(expand("${A}", &l).text, "bee");
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
