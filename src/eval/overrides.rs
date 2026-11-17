//! Override finalisation: `OVERRIDES`, winning variant, `:append` / `:prepend` / `:remove`.
//!
//! A port of `DataSmart.getVarFlag(var, "_content")` and `need_overrides` from BitBake's
//! `lib/bb/data_smart.py` (`master`, 2026-11). For a variable `VAR`:
//!
//! 1. Among the registered variants of `VAR` (`VAR:o`, `VAR:o1:o2`, ...), keep those whose
//!    override is active (every `:`-separated element in `OVERRIDES`), and pick one with
//!    BitBake's selection loop ([`select_variant`]): in effect the variant whose override is
//!    furthest right in `OVERRIDES` wins, and a chained variant beats its shorter prefix. The
//!    winner's value is computed the same way, recursively (its own variants and appends), and
//!    the `:remove`s that matched inside it are carried to the end.
//! 2. No winner (or the winner has no value at all): the plain value, else the `??=` default.
//! 3. Every active `:append` in order, then every active `:prepend` in order (each one goes in
//!    front of the previous result), on the **unexpanded** text.
//! 4. Expand.
//! 5. If the result is not empty: every active `:remove` (and the carried ones) is expanded
//!    and split on whitespace; every whitespace-separated word of the value equal to one of
//!    those words is dropped. Whitespace around a removed word is kept.
//!
//! `OVERRIDES` itself is computed like BitBake's `need_overrides`: expanded once with no
//! overrides active, then again with that result; if the two differ BitBake fails ("could not
//! be expanded into a stable state"); here the second result is used and every
//! override-dependent value says so in `unresolved` (`"OVERRIDES"`).
//!
//! Accuracy: when `OVERRIDES` contains unevaluated python or undefined variables, its real
//! entries are unknown. A value whose override decisions could change because of that (an
//! override was consulted and found inactive, or more than one variant was active) carries
//! `unevaluated_python` and the unresolved names of `OVERRIDES`. When `OVERRIDES` is unstable,
//! every value that consulted an override carries `"OVERRIDES"` in `unresolved`.

use std::collections::HashSet;
use std::sync::Arc;

use super::datastore::{Keyword, Piece, split_keyword};
use super::expand::{Expander, Source};
use super::{DataStore, OpStatus, Value, VarOp};

/// Nesting limit for variant-of-variant recursion (each level is a strictly longer name in
/// normal data; the limit guards against odd renames).
const MAX_VARIANT_DEPTH: usize = 32;

/// The active override list and what is uncertain about it.
#[derive(Debug, Clone, Default)]
pub(crate) struct OverrideState {
    /// Entries of `OVERRIDES`, lowest priority first, empty entries dropped.
    pub(crate) list: Vec<String>,
    pub(crate) set: HashSet<String>,
    /// `OVERRIDES` had an empty entry (`a::b`); BitBake keeps it, so an empty override
    /// element counts as active.
    pub(crate) has_empty: bool,
    /// `OVERRIDES` contains `${@...}`.
    pub(crate) python: bool,
    /// Undefined variables referenced by `OVERRIDES`.
    pub(crate) unresolved: Vec<String>,
    /// The two-pass computation did not reach a fixed point (fatal in BitBake).
    pub(crate) unstable: bool,
}

impl OverrideState {
    fn from_value(v: Option<Value>) -> OverrideState {
        let mut s = OverrideState::default();
        if let Some(v) = v {
            for e in v.text.split(':') {
                if e.is_empty() {
                    s.has_empty = true;
                } else {
                    s.list.push(e.to_string());
                    s.set.insert(e.to_string());
                }
            }
            s.python = v.unevaluated_python;
            s.unresolved = v.unresolved;
        }
        s
    }

    fn has(&self, element: &str) -> bool {
        if element.is_empty() {
            self.has_empty
        } else {
            self.set.contains(element)
        }
    }

    /// A variant `VAR:<o>` is active: `o in overridesset`, or every element of a chained `o`
    /// is.
    fn variant_active(&self, o: &str) -> bool {
        self.set.contains(o) || (o.contains(':') && o.split(':').all(|p| self.has(p)))
    }

    /// An `:append:<cond>` / `:prepend:<cond>` / `:remove:<cond>` applies: no condition, or
    /// every element of it is in `OVERRIDES`.
    fn cond_active(&self, cond: Option<&str>) -> bool {
        cond.is_none_or(|c| c.split(':').all(|p| self.has(p)))
    }

    /// Some entries are unknown (python or undefined variables).
    fn uncertain(&self) -> bool {
        self.python || !self.unresolved.is_empty()
    }
}

/// What one finalisation did, for statuses and accuracy flags.
#[derive(Debug, Default)]
struct Trace {
    /// Sequence numbers of the operations that contributed.
    applied: HashSet<u64>,
    /// Words dropped by `:remove` from the final value.
    removed: HashSet<String>,
    /// An override condition was evaluated.
    consulted: bool,
    /// An override condition was found inactive.
    inactive_seen: bool,
    /// More than one variant was active at some level.
    multi_active: bool,
}

/// A `:remove` text with the operations it came from (BitBake keeps a set of texts).
#[derive(Debug, Clone)]
struct Rm {
    text: String,
    src: Vec<u64>,
}

struct Composed {
    /// Unexpanded value before removes (BitBake's `value` for `expand=False`).
    raw: Option<String>,
    /// Expanded value with removes applied.
    parsed: Option<Value>,
    /// Removes that dropped at least one word (BitBake `parser.removes`).
    matched_removes: Vec<Rm>,
}

/// A datastore viewed under one override state; the expansion source.
struct Eval<'a> {
    ds: &'a DataStore,
    ovr: &'a OverrideState,
}

impl Source for Eval<'_> {
    fn lookup(&self, name: &str, exp: &mut Expander) -> Option<Value> {
        // The expander has already marked `name` as in progress.
        self.finalise(name, exp, &mut Trace::default())
    }
}

impl Eval<'_> {
    /// The finalised value of `name`; `name` must already be entered on `exp`.
    fn finalise(&self, name: &str, exp: &mut Expander, trace: &mut Trace) -> Option<Value> {
        let c = self.compose(name, true, exp, trace, 0);
        let mut v = c.parsed?;
        let unknown_entries = (trace.inactive_seen || trace.multi_active) && self.ovr.uncertain();
        if trace.consulted && (self.ovr.unstable || unknown_entries) {
            if self.ovr.python {
                v.unevaluated_python = true;
            }
            for u in &self.ovr.unresolved {
                push_unique(&mut v.unresolved, u);
            }
            if self.ovr.unstable {
                push_unique(&mut v.unresolved, "OVERRIDES");
            }
        }
        Some(v)
    }

    fn piece_active(&self, p: &Piece, trace: &mut Trace) -> bool {
        if p.cond.is_some() {
            trace.consulted = true;
        }
        let a = self.ovr.cond_active(p.cond.as_deref());
        if !a {
            trace.inactive_seen = true;
        }
        a
    }

    /// `getVarFlag(var, "_content", expand=top, retparser=True)`.
    fn compose(
        &self,
        var: &str,
        top: bool,
        exp: &mut Expander,
        trace: &mut Trace,
        depth: usize,
    ) -> Composed {
        let local = self.ds.vars.get(var);
        let mut value: Option<String> = None;
        let mut removes: Vec<Rm> = Vec::new();

        if let Some(od) = self.ds.overridedata.get(var) {
            if !od.is_empty() {
                trace.consulted = true;
            }
            let mut active = Vec::new();
            for (r, o) in od {
                if self.ovr.variant_active(o) {
                    active.push((o.clone(), r.clone()));
                } else {
                    trace.inactive_seen = true;
                }
            }
            if active.len() > 1 {
                trace.multi_active = true;
            }
            if let Some(m) = select_variant(active, &self.ovr.list)
                && depth < MAX_VARIANT_DEPTH
            {
                let entered = exp.enter(&m);
                let sub = self.compose(&m, false, exp, trace, depth + 1);
                exp.leave(entered);
                value = sub.raw;
                removes = sub.matched_removes;
            }
        }

        if let Some(l) = local {
            if value.is_none()
                && let Some(c) = l.content.as_ref().or(l.default.as_ref())
            {
                value = Some(c.text.clone());
                trace.applied.extend(c.src.iter().copied());
            }
            for p in &l.appends {
                if self.piece_active(p, trace) {
                    let mut v = value.take().unwrap_or_default();
                    v.push_str(&p.text);
                    value = Some(v);
                    trace.applied.extend(p.src.iter().copied());
                }
            }
            for p in &l.prepends {
                if self.piece_active(p, trace) {
                    let v = value.take().unwrap_or_default();
                    value = Some(format!("{}{v}", p.text));
                    trace.applied.extend(p.src.iter().copied());
                }
            }
        }

        let mut parsed = value.as_ref().map(|v| exp.expand(v, self));

        let truthy = if top {
            parsed.as_ref().is_some_and(|p| !p.text.is_empty())
        } else {
            value.as_ref().is_some_and(|v| !v.is_empty())
        };
        if truthy && let Some(l) = local {
            for p in &l.removes {
                if self.piece_active(p, trace) {
                    trace.applied.extend(p.src.iter().copied());
                    match removes.iter_mut().find(|r| r.text == p.text) {
                        Some(r) => r.src.extend(p.src.iter().copied()),
                        None => removes.push(Rm {
                            text: p.text.clone(),
                            src: p.src.clone(),
                        }),
                    }
                }
            }
        }

        let mut matched = Vec::new();
        if truthy
            && !removes.is_empty()
            && let Some(pv) = parsed.as_mut()
        {
            let mut words: Vec<Vec<String>> = Vec::with_capacity(removes.len());
            for r in &removes {
                let e = exp.expand(&r.text, self);
                if e.unevaluated_python {
                    pv.unevaluated_python = true;
                }
                for u in &e.unresolved {
                    push_unique(&mut pv.unresolved, u);
                }
                words.push(e.text.split_whitespace().map(str::to_string).collect());
            }
            let mut hit = vec![false; removes.len()];
            let mut out = String::with_capacity(pv.text.len());
            for tok in split_keep_whitespace(&pv.text) {
                let mut skip = false;
                for (i, w) in words.iter().enumerate() {
                    if w.iter().any(|x| x == tok) {
                        hit[i] = true;
                        skip = true;
                    }
                }
                if skip {
                    if top {
                        trace.removed.insert(tok.to_string());
                    }
                } else {
                    out.push_str(tok);
                }
            }
            pv.text = out;
            matched = removes
                .into_iter()
                .zip(hit)
                .filter(|(_, h)| *h)
                .map(|(r, _)| r)
                .collect();
        }

        Composed {
            raw: value,
            parsed,
            matched_removes: matched,
        }
    }
}

/// BitBake's variant selection loop in `getVarFlag`, reproduced step by step. `active` holds
/// `(override, variant name)` pairs in registration order (a Python dict); `overrides` is the
/// `OVERRIDES` list. Returns the winning variant name.
///
/// ```text
/// mod = True
/// while mod:
///     mod = False
///     for o in self.overrides:
///         for a in active.copy():
///             if a.endswith(":" + o):
///                 t = active[a]; del active[a]
///                 active[a.replace(":" + o, "")] = t
///                 mod = True
///             elif a == o:
///                 match = active[a]; del active[a]
/// ```
pub(crate) fn select_variant(
    mut active: Vec<(String, String)>,
    overrides: &[String],
) -> Option<String> {
    let mut matched: Option<String> = None;
    let mut modified = true;
    let mut rounds = 0;
    while modified && rounds < 10_000 {
        rounds += 1;
        modified = false;
        for o in overrides {
            let suffix = format!(":{o}");
            let snapshot: Vec<String> = active.iter().map(|(k, _)| k.clone()).collect();
            for a in snapshot {
                let Some(idx) = active.iter().position(|(k, _)| *k == a) else {
                    continue;
                };
                if a.ends_with(&suffix) {
                    let (_, t) = active.remove(idx);
                    let nk = a.replace(&suffix, "");
                    match active.iter_mut().find(|(k, _)| *k == nk) {
                        Some(e) => e.1 = t,
                        None => active.push((nk, t)),
                    }
                    modified = true;
                } else if a == *o {
                    matched = Some(active.remove(idx).1);
                }
            }
        }
    }
    matched.filter(|m| !m.is_empty())
}

/// Python `re.split(r'(\s)', s)`: runs of non-whitespace and single whitespace characters.
fn split_keep_whitespace(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if start < i {
                out.push(&s[start..i]);
            }
            out.push(&s[i..i + c.len_utf8()]);
            start = i + c.len_utf8();
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

impl DataStore {
    /// Finalised and expanded value of `name` (`getVar(name)`): override variant, appends,
    /// prepends, expansion, removes. `None` if the variable has no value at all.
    ///
    /// `name` may contain `${...}` (`FILES:${PN}-dev`); it is expanded first. `name` may be
    /// a variant (`FILES:hello-dev`), which is finalised the same way.
    pub fn value(&self, name: &str) -> Option<Value> {
        let (name, name_flags) = if name.contains("${") {
            let e = self.expand(name);
            let flags = (e.unevaluated_python, e.unresolved);
            (e.text, Some(flags))
        } else {
            (name.to_string(), None)
        };
        let ovr = self.override_state();
        let mut v = self.value_with(&name, &ovr)?;
        if let Some((py, unresolved)) = name_flags {
            v.unevaluated_python |= py;
            for u in &unresolved {
                push_unique(&mut v.unresolved, u);
            }
        }
        Some(v)
    }

    /// `OVERRIDES` entries, lowest priority first (empty entries dropped).
    pub fn overrides(&self) -> Vec<String> {
        self.override_state().list.clone()
    }

    /// The finalised value of `OVERRIDES` the override list was split from, with its python
    /// and unresolved flags.
    pub fn overrides_value(&self) -> Option<Value> {
        let ovr = self.override_state();
        let mut v = self.value_with("OVERRIDES", &ovr)?;
        if ovr.unstable {
            push_unique(&mut v.unresolved, "OVERRIDES");
        }
        Some(v)
    }

    /// Expands `text` against the store (each `${VAR}` becomes the finalised value of `VAR`).
    pub fn expand(&self, text: &str) -> Value {
        let ovr = self.override_state();
        Expander::new().expand(
            text,
            &Eval {
                ds: self,
                ovr: &ovr,
            },
        )
    }

    /// Expanded value of `name[flag]` (flags have no overrides or appends; a `??=` flag
    /// default is used when the flag is not set).
    pub fn flag(&self, name: &str, flag: &str) -> Option<Value> {
        let raw = self.raw_flag(name, flag)?;
        Some(self.expand(&raw))
    }

    pub(crate) fn override_state(&self) -> Arc<OverrideState> {
        self.ovr_cache
            .get_or_init(|| Arc::new(self.compute_overrides()))
            .clone()
    }

    fn compute_overrides(&self) -> OverrideState {
        let first =
            OverrideState::from_value(self.value_with("OVERRIDES", &OverrideState::default()));
        let second = OverrideState::from_value(self.value_with("OVERRIDES", &first));
        if first.list == second.list && first.has_empty == second.has_empty {
            first
        } else {
            OverrideState {
                unstable: true,
                ..second
            }
        }
    }

    fn value_with(&self, name: &str, ovr: &OverrideState) -> Option<Value> {
        let ev = Eval { ds: self, ovr };
        let mut exp = Expander::new();
        let entered = exp.enter(name);
        let v = ev.finalise(name, &mut exp, &mut Trace::default());
        exp.leave(entered);
        v
    }

    /// Statuses of `ops` (recorded for `name`) for the final value of `name`.
    pub(crate) fn statuses(&self, name: &str, ops: Vec<(u64, VarOp)>) -> Vec<(VarOp, OpStatus)> {
        let ovr = self.override_state();
        let ev = Eval {
            ds: self,
            ovr: &ovr,
        };
        let mut exp = Expander::new();
        let entered = exp.enter(name);
        let mut trace = Trace::default();
        let _ = ev.compose(name, true, &mut exp, &mut trace, 0);
        exp.leave(entered);
        ops.into_iter()
            .map(|(seq, op)| {
                let st = if op.var.legacy_syntax {
                    OpStatus::InactiveOverride
                } else if trace.applied.contains(&seq) {
                    if self.fully_removed(&op, &trace) {
                        OpStatus::Removed
                    } else {
                        OpStatus::Applied
                    }
                } else if self.conditions_hold(name, &op, &ovr) {
                    OpStatus::Shadowed
                } else {
                    OpStatus::InactiveOverride
                };
                (op, st)
            })
            .collect()
    }

    /// Every word `op` contributed was dropped by a `:remove`.
    fn fully_removed(&self, op: &VarOp, trace: &Trace) -> bool {
        if trace.removed.is_empty() {
            return false;
        }
        let full = self.expanded_name(op);
        if matches!(split_keyword(&full), Some((_, Keyword::Remove, _))) {
            return false;
        }
        let text = self.expand(&op.value).text;
        let mut words = text.split_whitespace().peekable();
        words.peek().is_some() && words.all(|w| trace.removed.contains(w))
    }

    /// Every override element of `op`'s name after `name` (ignoring `append` / `prepend` /
    /// `remove`) is in `OVERRIDES`.
    fn conditions_hold(&self, name: &str, op: &VarOp, ovr: &OverrideState) -> bool {
        let full = self.expanded_name(op);
        let rest = if full == name {
            ""
        } else {
            match full.strip_prefix(name).and_then(|r| r.strip_prefix(':')) {
                Some(r) => r,
                None => return false,
            }
        };
        rest.split(':')
            .filter(|e| !e.is_empty() && !matches!(*e, "append" | "prepend" | "remove"))
            .all(|e| ovr.has(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(o, r)| (o.to_string(), r.to_string()))
            .collect()
    }

    fn ovr(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_rightmost_wins() {
        let a = act(&[("a", "V:a"), ("b", "V:b")]);
        assert_eq!(
            select_variant(a.clone(), &ovr(&["a", "b"])).as_deref(),
            Some("V:b")
        );
        assert_eq!(select_variant(a, &ovr(&["b", "a"])).as_deref(), Some("V:a"));
    }

    #[test]
    fn select_chained_beats_prefix() {
        let a = act(&[("a", "V:a"), ("a:b", "V:a:b")]);
        assert_eq!(
            select_variant(a.clone(), &ovr(&["a", "b"])).as_deref(),
            Some("V:a:b")
        );
        assert_eq!(
            select_variant(a, &ovr(&["b", "a"])).as_deref(),
            Some("V:a:b")
        );
    }

    #[test]
    fn select_nothing() {
        assert_eq!(select_variant(Vec::new(), &ovr(&["a"])), None);
    }

    #[test]
    fn split_whitespace_like_python() {
        assert_eq!(split_keep_whitespace("a  b"), vec!["a", " ", " ", "b"]);
        assert_eq!(
            split_keep_whitespace(" a\tb "),
            vec![" ", "a", "\t", "b", " "]
        );
        assert!(split_keep_whitespace("").is_empty());
    }
}
