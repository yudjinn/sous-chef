//! Variable store with full operation history.
//!
//! The representation follows BitBake's `DataSmart` (`lib/bb/data_smart.py`) and the
//! parse-time operators of `DataNode.eval` (`lib/bb/parse/ast.py`), checked against BitBake
//! `master` as of 2026-11; see `docs/bitbake-semantics.md` ("Overrides", "Datastore"):
//!
//! - Per variable name **as stored** (`VAR`, `VAR:foo`, `FILES:${PN}-dev`): the plain value
//!   (`_content`), the `??=` weak default (`_defaultval`, last one wins), flags (and `??=`
//!   flag defaults), and the `:append` / `:prepend` / `:remove` lists. A name containing
//!   `:append`, `:prepend` or `:remove` is never stored itself: `VAR:append:foo = " x"` adds
//!   `(" x", "foo")` to the append list of `VAR`, and `VAR:foo:append` adds `(…, none)` to
//!   the list of `VAR:foo`. These lists are stored, not applied; finalisation
//!   (`overrides.rs`) applies them.
//! - "Override data": for every name with override suffixes, which shorter name it is a
//!   variant of (`VAR:a:b` is a variant of `VAR:a` with override `b` and of `VAR` with `a:b`).
//! - Parse-time operators read the current plain value of exactly the name written (no
//!   overrides, no appends, ignoring a `??=` default) and store the result.
//! - Every operation is also recorded, with its [`Loc`] and `via` chain, in a per-base-name
//!   history; [`DataStore::history`] returns it with an [`OpStatus`] per operation.
//!
//! # Use from the replay (T3.4)
//!
//! ```text
//! let mut ds = DataStore::new();              // or config.fork() for a recipe
//! ds.record_visit(FileVisit { .. });          // each file, in visit order
//! ds.apply(VarOp { var, op, value, loc, via });   // each assignment, in parse order
//! ds.mark_exported("VAR");                    // `export VAR` and `export VAR = ...`
//! ds.unset("VAR", None, loc, via);            // `unset VAR`, `unset VAR[flag]`
//! ds.expand_keys();                           // once, at the end of the recipe (finalise)
//! ds.value("VAR"); ds.history("VAR");         // queries
//! ```
//!
//! `fork` is O(1): the maps are shared behind `Arc`s and copied on the first write to either
//! copy (the copy duplicates the map of pointers, not the variables). Each variable is itself
//! copied on its first write.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde::Serialize;

use crate::types::{AssignOp, Loc, VarRef};

use super::FileVisit;
use super::overrides::OverrideState;
use super::replay::ReplayNotes;

/// One recorded operation on a variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VarOp {
    pub var: VarRef,
    pub op: AssignOp,
    pub value: String,
    pub loc: Loc,
    /// The include / inherit / bbappend statements that led here, outermost first.
    pub via: Vec<Loc>,
}

/// What finalisation did with an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum OpStatus {
    /// Contributes to the final value.
    Applied,
    /// Conditional on an override that is not in `OVERRIDES` (or written with the legacy `_`
    /// syntax, which modern BitBake never treats as an override).
    InactiveOverride,
    /// Its conditions hold but it does not contribute: overwritten by a later assignment,
    /// lost to a higher-priority override variant, a `?=` / `??=` that did not take effect,
    /// or cleared by `unset`.
    Shadowed,
    /// Applied, but every word it contributed was taken out again by a `:remove`.
    Removed,
}

/// An `unset VAR` / `unset VAR[flag]` statement, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnsetRecord {
    pub name: String,
    pub flag: Option<String>,
    pub loc: Loc,
    pub via: Vec<Loc>,
}

/// A stored text and the recorded operations it came from (sequence numbers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Content {
    pub(crate) text: String,
    pub(crate) src: Vec<u64>,
}

/// One `:append` / `:prepend` / `:remove` entry: the text and the override condition
/// (`VAR:append:a:b` -> `Some("a:b")`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) text: String,
    pub(crate) cond: Option<String>,
    pub(crate) src: Vec<u64>,
}

/// Everything stored under one variable name (BitBake: `dict[var]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Var {
    /// `_content`
    pub(crate) content: Option<Content>,
    /// `_defaultval` (`??=`)
    pub(crate) default: Option<Content>,
    pub(crate) flags: BTreeMap<String, Content>,
    /// `_defaultval_flag_<flag>` (`VAR[flag] ??= ...`)
    pub(crate) default_flags: BTreeMap<String, Content>,
    pub(crate) appends: Vec<Piece>,
    pub(crate) prepends: Vec<Piece>,
    pub(crate) removes: Vec<Piece>,
}

/// `:append`, `:prepend` or `:remove`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keyword {
    Append,
    Prepend,
    Remove,
}

#[derive(Debug, Clone)]
pub(crate) enum Event {
    Assign { seq: u64, op: VarOp },
    Unset { rec: UnsetRecord },
}

/// `(variant name, override)`: `("VAR:a:b", "a:b")` registered under `VAR`.
pub(crate) type Variant = (String, String);

/// The result of an evaluation: every operation on every variable, plus the files visited.
#[derive(Debug, Clone, Default)]
pub struct DataStore {
    pub(crate) visits: Vec<FileVisit>,
    /// Stored variables by name.
    pub(crate) vars: Arc<BTreeMap<Arc<str>, Arc<Var>>>,
    /// BitBake `overridedata`: name -> `(variant name, override)` in registration order.
    pub(crate) overridedata: Arc<BTreeMap<Arc<str>, Vec<Variant>>>,
    /// History per base name (the part before the first `:`), in parse order.
    pub(crate) events: Arc<BTreeMap<Arc<str>, Arc<Vec<Event>>>>,
    pub(crate) next_seq: u64,
    /// `OVERRIDES` as computed from the current contents; reset on every write.
    pub(crate) ovr_cache: OnceLock<Arc<OverrideState>>,
    /// What the replay recorded beside the variables (T3.4, `replay.rs`): references it
    /// could not follow, anonymous python, names python may change.
    pub(crate) notes: Arc<ReplayNotes>,
}

impl DataStore {
    pub fn new() -> DataStore {
        DataStore::default()
    }

    /// A copy that shares everything with `self` until either is written (copy on write).
    /// Evaluate the configuration once, then `fork` it for every recipe.
    pub fn fork(&self) -> DataStore {
        self.clone()
    }

    /// Records a file visit (replay order).
    pub fn record_visit(&mut self, visit: FileVisit) {
        self.visits.push(visit);
    }

    /// Files in replay order, for `recipe show=tree`.
    pub fn files(&self) -> &[FileVisit] {
        &self.visits
    }

    /// Applies one parsed assignment and records it. `:=` is expanded with the store as it is
    /// now ([`DataStore::expand`]); `${@...}` in it stays unevaluated text.
    pub fn apply(&mut self, op: VarOp) {
        self.apply_with(op, |ds, text| ds.expand(text).text);
    }

    /// Like [`DataStore::apply`], with the `:=` expansion supplied by the caller. `immediate`
    /// is called only for `:=`, before the operation changes anything.
    pub fn apply_with<F>(&mut self, op: VarOp, immediate: F)
    where
        F: FnOnce(&DataStore, &str) -> String,
    {
        let seq = self.next_seq;
        self.next_seq += 1;
        let key = full_name(&op.var);
        let flag = op.var.flag.clone();
        let kind = op.op;
        let value = op.value.clone();
        let legacy = op.var.legacy_syntax;
        let imm = if kind == AssignOp::Immediate && !legacy {
            Some(immediate(self, &value))
        } else {
            None
        };
        let base = op.var.base.clone();
        self.push_event(&base, Event::Assign { seq, op });
        if legacy {
            // Legacy `_` syntax is a different variable (or a fatal error) in modern BitBake;
            // it never affects the modern variable, so it is recorded but not applied.
            return;
        }
        self.touch();
        let new = Content {
            text: value,
            src: vec![seq],
        };
        match flag {
            Some(flag) => self.apply_flag(&key, &flag, kind, new, imm),
            None => self.apply_content(&key, kind, new, imm),
        }
    }

    fn apply_content(&mut self, key: &str, kind: AssignOp, new: Content, imm: Option<String>) {
        // `getVar(key, False, noweakdefault=True, parsing=True)`: the plain value of exactly
        // this name.
        let cur = self.vars.get(key).and_then(|v| v.content.clone());
        let result = match kind {
            AssignOp::WeakDefault => {
                self.var_mut(key).default = Some(new);
                return;
            }
            _ => combine(kind, cur, new, imm),
        };
        self.set_var(key, result);
    }

    fn apply_flag(
        &mut self,
        key: &str,
        flag: &str,
        kind: AssignOp,
        new: Content,
        imm: Option<String>,
    ) {
        // `getVarFlag(key, flag, expand=False, noweakdefault=True)`
        let cur = self.vars.get(key).and_then(|v| v.flags.get(flag).cloned());
        if kind == AssignOp::WeakDefault {
            self.var_mut(key)
                .default_flags
                .insert(flag.to_string(), new);
            return;
        }
        let result = combine(kind, cur, new, imm);
        self.var_mut(key).flags.insert(flag.to_string(), result);
    }

    /// `export VAR` (and the `export` of `export VAR = ...`): sets the `export` flag.
    pub fn mark_exported(&mut self, name: &str) {
        self.touch();
        self.var_mut(name).flags.insert(
            "export".to_string(),
            Content {
                text: "1".to_string(),
                src: Vec::new(),
            },
        );
    }

    /// `unset VAR` (BitBake `delVar`): clears the value, the weak default, every flag and the
    /// `:append` / `:prepend` / `:remove` lists of `VAR`, and detaches its override variants
    /// (`VAR:foo` keeps its own value but no longer replaces `VAR` until it is assigned
    /// again). `unset VAR[flag]` (`delVarFlag`) removes the flag and its `??=` default.
    pub fn unset(&mut self, name: &str, flag: Option<&str>, loc: Loc, via: Vec<Loc>) {
        let base = base_of(name).to_string();
        self.push_event(
            &base,
            Event::Unset {
                rec: UnsetRecord {
                    name: name.to_string(),
                    flag: flag.map(str::to_string),
                    loc,
                    via,
                },
            },
        );
        self.touch();
        match flag {
            None => self.del_var(name),
            Some(f) => {
                let has = self.vars.get(name).is_some_and(|v| v.flags.contains_key(f));
                if has {
                    let v = self.var_mut(name);
                    v.flags.remove(f);
                    v.default_flags.remove(f);
                }
            }
        }
    }

    /// BitBake `expandKeys` (run by `ast.finalize` after deferred inherits, before anonymous
    /// python): every stored name containing `${` is expanded and the variable renamed
    /// (`renameVar(key, newkey, recurse=False)`), in sorted order of the original names. The
    /// value moves (replacing any value already under the new name), the append / prepend /
    /// remove lists are added to the new name's, other flags are dropped.
    ///
    /// Returns the names that could not be fully expanded (python or undefined variables);
    /// those stay under their unexpanded name.
    pub fn expand_keys(&mut self) -> Vec<String> {
        let mut todo: BTreeMap<String, String> = BTreeMap::new();
        let mut incomplete = Vec::new();
        let keys: Vec<String> = self
            .vars
            .keys()
            .filter(|k| k.contains("${"))
            .map(|k| k.to_string())
            .collect();
        for key in keys {
            let e = self.expand(&key);
            if e.unevaluated_python || !e.unresolved.is_empty() {
                incomplete.push(key.clone());
            }
            if e.text != key {
                todo.insert(key, e.text);
            }
        }
        for (key, newkey) in todo {
            self.rename_var(&key, &newkey);
        }
        incomplete
    }

    /// Operations on `name` and all of its override variants, in parse order, with their
    /// status for the final value of `name`.
    ///
    /// `name` is normally a base name (`SRC_URI`): every assignment whose left-hand side
    /// starts with it is returned (`SRC_URI`, `SRC_URI:append:foo`, `SRC_URI:qemux86`, ...).
    /// A name with overrides (`FILES:hello-dev`) returns the operations on that name and its
    /// own variants, with names written `FILES:${PN}-dev` matched after expansion. Flag
    /// assignments are not included (see [`DataStore::flag_history`]); neither are `unset`
    /// statements (see [`DataStore::unsets`]).
    pub fn history(&self, name: &str) -> Vec<(VarOp, OpStatus)> {
        let ops = self.ops_for(name, None);
        self.statuses(name, ops)
    }

    /// Assignments to `name[flag]`, in parse order. `Applied` marks the operations the final
    /// flag value comes from; the others are `Shadowed`.
    pub fn flag_history(&self, name: &str, flag: &str) -> Vec<(VarOp, OpStatus)> {
        let ops = self.ops_for(name, Some(flag));
        let var = self.vars.get(name);
        let src: Vec<u64> = var
            .and_then(|v| v.flags.get(flag).or_else(|| v.default_flags.get(flag)))
            .map(|c| c.src.clone())
            .unwrap_or_default();
        ops.into_iter()
            .map(|(seq, op)| {
                let st = if src.contains(&seq) {
                    OpStatus::Applied
                } else {
                    OpStatus::Shadowed
                };
                (op, st)
            })
            .collect()
    }

    /// `unset` statements on `base` (any flag), in parse order.
    pub fn unsets(&self, base: &str) -> Vec<UnsetRecord> {
        self.events
            .get(base_of(base))
            .map(|evs| {
                evs.iter()
                    .filter_map(|e| match e {
                        Event::Unset { rec } => Some(rec.clone()),
                        Event::Assign { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The stored, unexpanded plain value of exactly `name` (no overrides, appends or
    /// removes), falling back to its `??=` default. BitBake `getVar(name, False, parsing=True)`.
    pub fn raw(&self, name: &str) -> Option<String> {
        let v = self.vars.get(name)?;
        v.content
            .as_ref()
            .or(v.default.as_ref())
            .map(|c| c.text.clone())
    }

    /// The unexpanded value of `name[flag]`, falling back to its `??=` default.
    pub fn raw_flag(&self, name: &str, flag: &str) -> Option<String> {
        let v = self.vars.get(name)?;
        v.flags
            .get(flag)
            .or_else(|| v.default_flags.get(flag))
            .map(|c| c.text.clone())
    }

    /// Flag names set on `name`, sorted.
    pub fn flag_names(&self, name: &str) -> Vec<String> {
        let Some(v) = self.vars.get(name) else {
            return Vec::new();
        };
        let mut out: Vec<String> = v
            .flags
            .keys()
            .chain(v.default_flags.keys())
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every stored name that has a value, a weak default or append / prepend / remove
    /// entries, sorted.
    pub fn names(&self) -> Vec<String> {
        self.vars
            .iter()
            .filter(|(_, v)| {
                v.content.is_some()
                    || v.default.is_some()
                    || !v.appends.is_empty()
                    || !v.prepends.is_empty()
                    || !v.removes.is_empty()
            })
            .map(|(k, _)| k.to_string())
            .collect()
    }

    // ----- internals -----

    /// Invalidates derived state after a write.
    fn touch(&mut self) {
        self.ovr_cache = OnceLock::new();
    }

    fn push_event(&mut self, base: &str, ev: Event) {
        let events = Arc::make_mut(&mut self.events);
        match events.get_mut(base) {
            Some(list) => Arc::make_mut(list).push(ev),
            None => {
                events.insert(Arc::from(base), Arc::new(vec![ev]));
            }
        }
    }

    pub(crate) fn var_mut(&mut self, key: &str) -> &mut Var {
        let vars = Arc::make_mut(&mut self.vars);
        Arc::make_mut(vars.entry(Arc::from(key)).or_default())
    }

    /// BitBake `setVar(key, value, parsing=True)`.
    fn set_var(&mut self, key: &str, content: Content) {
        if let Some((base, kw, add)) = split_keyword(key) {
            let piece = Piece {
                text: content.text,
                cond: add,
                src: content.src,
            };
            let v = self.var_mut(&base);
            match kw {
                Keyword::Append => v.appends.push(piece),
                Keyword::Prepend => v.prepends.push(piece),
                Keyword::Remove => v.removes.push(piece),
            }
            self.update_overrides(&base);
            return;
        }
        self.var_mut(key).content = Some(content);
        if key.contains(':') {
            self.update_overrides(key);
        }
    }

    /// BitBake `_setvar_update_overrides`: registers `var` as a variant of each shorter name,
    /// from the right, while the override text starts with `[a-z0-9]`.
    ///
    /// (For a name without `:` BitBake runs the loop on the whole name and may register it
    /// under the name minus its last character; nothing ever reads that, so it is skipped.)
    fn update_overrides(&mut self, var: &str) {
        for (short, ovr) in override_chain(var) {
            let od = Arc::make_mut(&mut self.overridedata);
            let entry = (var.to_string(), ovr);
            match od.get_mut(short.as_str()) {
                Some(list) => {
                    if !list.contains(&entry) {
                        list.push(entry);
                    }
                }
                None => {
                    od.insert(Arc::from(short.as_str()), vec![entry]);
                }
            }
        }
    }

    /// BitBake `delVar`.
    fn del_var(&mut self, var: &str) {
        let vars = Arc::make_mut(&mut self.vars);
        vars.insert(Arc::from(var), Arc::new(Var::default()));
        let od = Arc::make_mut(&mut self.overridedata);
        od.remove(var);
        for (short, ovr) in override_chain(var) {
            if let Some(list) = od.get_mut(short.as_str()) {
                list.retain(|(v, o)| !(v == var && *o == ovr));
            }
        }
    }

    /// BitBake `renameVar(key, newkey, recurse=False)`.
    fn rename_var(&mut self, key: &str, newkey: &str) {
        if key == newkey {
            return;
        }
        let Some(local) = self.vars.get(key).cloned() else {
            return;
        };
        let mut found = false;
        // getVar(key, False, parsing=True): the plain value, else the weak default.
        let val = local.content.clone().or_else(|| local.default.clone());
        if let Some(v) = &val {
            self.set_var(newkey, v.clone());
            found = true;
        }
        if !local.appends.is_empty() || !local.prepends.is_empty() || !local.removes.is_empty() {
            let dest = self.var_mut(newkey);
            dest.appends.extend(local.appends.iter().cloned());
            dest.prepends.extend(local.prepends.iter().cloned());
            dest.removes.extend(local.removes.iter().cloned());
            found = true;
        }
        if let Some(list) = self.overridedata.get(key).cloned() {
            let renamed = list
                .into_iter()
                .map(|(v, o)| (v.replace(key, newkey), o))
                .collect();
            Arc::make_mut(&mut self.overridedata).insert(Arc::from(newkey), renamed);
            found = true;
        }
        if !found {
            return;
        }
        if newkey.contains(':') && val.is_none() {
            self.update_overrides(newkey);
        }
        self.del_var(key);
    }

    /// Recorded assignments for `name` (see [`DataStore::history`]): non-flag ones when
    /// `flag` is `None`, else those on exactly `name[flag]`.
    fn ops_for(&self, name: &str, flag: Option<&str>) -> Vec<(u64, VarOp)> {
        let Some(evs) = self.events.get(base_of(name)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in evs.iter() {
            let Event::Assign { seq, op } = e else {
                continue;
            };
            if op.var.flag.as_deref() != flag {
                continue;
            }
            // For a base name every assignment recorded under it belongs to it.
            let hit = (flag.is_none() && !name.contains(':')) || {
                let full = self.expanded_name(op);
                match flag {
                    Some(_) => full == name,
                    None => full == name || full.starts_with(&format!("{name}:")),
                }
            };
            if hit {
                out.push((*seq, op.clone()));
            }
        }
        out
    }

    /// The full left-hand side of `op` (without flag), with `${...}` expanded when present.
    pub(crate) fn expanded_name(&self, op: &VarOp) -> String {
        let full = full_name(&op.var);
        if full.contains("${") {
            self.expand(&full).text
        } else {
            full
        }
    }
}

/// The parse-time result of `kind` given the current value `cur` and the new text `new`
/// (`DataNode.eval`). `+=` on an unset variable gives `" x"` (a leading space), as BitBake does.
fn combine(kind: AssignOp, cur: Option<Content>, new: Content, imm: Option<String>) -> Content {
    let join = |cur: Option<Content>, f: &dyn Fn(&str) -> String| match cur {
        Some(c) => {
            let mut src = c.src;
            src.extend(new.src.iter().copied());
            Content {
                text: f(&c.text),
                src,
            }
        }
        None => Content {
            text: f(""),
            src: new.src.clone(),
        },
    };
    match kind {
        AssignOp::Set | AssignOp::WeakDefault => new.clone(),
        AssignOp::Default => cur.unwrap_or_else(|| new.clone()),
        AssignOp::Immediate => Content {
            text: imm.unwrap_or_else(|| new.text.clone()),
            src: new.src.clone(),
        },
        AssignOp::Append => join(cur, &|c| format!("{c} {}", new.text)),
        AssignOp::Prepend => join(cur, &|c| format!("{} {c}", new.text)),
        AssignOp::AppendDot => join(cur, &|c| format!("{c}{}", new.text)),
        AssignOp::PrependDot => join(cur, &|c| format!("{}{c}", new.text)),
    }
}

/// `VarRef` back to the name as written: `base[:override...]` (no flag).
pub fn full_name(v: &VarRef) -> String {
    let mut s = v.base.clone();
    for o in &v.overrides {
        s.push(':');
        s.push_str(o);
    }
    s
}

/// The part of `name` before the first `:` outside `${...}`.
pub(crate) fn base_of(name: &str) -> &str {
    let mut depth = 0usize;
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'$' if b.get(i + 1) == Some(&b'{') => {
                depth += 1;
                i += 1;
            }
            b'}' if depth > 0 => depth -= 1,
            b':' if depth == 0 => return &name[..i],
            _ => {}
        }
        i += 1;
    }
    name
}

/// BitBake `__setvar_regexp__` =
/// `(?P<base>.*?)(?P<keyword>:append|:prepend|:remove)(:(?P<add>[^A-Z]*))?$`:
/// the first `:append` / `:prepend` / `:remove` that is followed by the end of the name or by
/// `:` and text without uppercase letters. Returns `(base, keyword, add)`; an empty `add` is
/// `None` (BitBake treats it as no override).
pub(crate) fn split_keyword(name: &str) -> Option<(String, Keyword, Option<String>)> {
    const KWS: [(&str, Keyword); 3] = [
        (":append", Keyword::Append),
        (":prepend", Keyword::Prepend),
        (":remove", Keyword::Remove),
    ];
    for (i, _) in name.match_indices(':') {
        for (text, kw) in KWS {
            let Some(rest) = name[i..].strip_prefix(text) else {
                continue;
            };
            if rest.is_empty() {
                return Some((name[..i].to_string(), kw, None));
            }
            if let Some(add) = rest.strip_prefix(':')
                && !add.chars().any(|c| c.is_ascii_uppercase())
            {
                let add = (!add.is_empty()).then(|| add.to_string());
                return Some((name[..i].to_string(), kw, add));
            }
        }
    }
    None
}

/// The `(shorter name, override)` pairs `var` is registered under, following
/// `_setvar_update_overrides`: `VAR:a:b` -> `[("VAR:a", "b"), ("VAR", "a:b")]`, stopping at
/// the first override (from the right) that does not start with `[a-z0-9]`.
pub(crate) fn override_chain(var: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(mut cut) = var.rfind(':') else {
        return out;
    };
    loop {
        let ovr = &var[cut + 1..];
        let short = &var[..cut];
        let ok = ovr
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !ok {
            break;
        }
        out.push((short.to_string(), ovr.to_string()));
        match short.rfind(':') {
            Some(c) if c > 0 => cut = c,
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LineSpan;

    fn vr(name: &str) -> VarRef {
        let (name, flag) = match name.split_once('[') {
            Some((n, f)) => (n, Some(f.trim_end_matches(']').to_string())),
            None => (name, None),
        };
        crate::parse::split_var(name, flag)
    }

    fn loc(line: u32) -> Loc {
        Loc {
            layer: "meta-test".into(),
            path: "recipes/t/t.bb".into(),
            lines: LineSpan::line(line),
        }
    }

    /// Applies `(name, op, value)` triples, one per line.
    fn store(ops: &[(&str, &str, &str)]) -> DataStore {
        let mut ds = DataStore::new();
        for (i, (name, op, value)) in ops.iter().enumerate() {
            apply(&mut ds, name, op, value, i as u32 + 1);
        }
        ds
    }

    fn apply(ds: &mut DataStore, name: &str, op: &str, value: &str, line: u32) {
        ds.apply(VarOp {
            var: vr(name),
            op: op.parse().unwrap(),
            value: value.into(),
            loc: loc(line),
            via: vec![],
        });
    }

    fn raw(ds: &DataStore, name: &str) -> Option<String> {
        ds.raw(name)
    }

    #[test]
    fn set() {
        let ds = store(&[("A", "=", "x"), ("A", "=", "y")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("y"));
    }

    #[test]
    fn default_only_if_unset() {
        let ds = store(&[
            ("A", "?=", "x"),
            ("A", "?=", "y"),
            ("B", "=", "b"),
            ("B", "?=", "z"),
        ]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("x"));
        assert_eq!(raw(&ds, "B").as_deref(), Some("b"));
    }

    #[test]
    fn weak_default_last_wins_and_only_if_nothing_else() {
        let ds = store(&[("A", "??=", "x"), ("A", "??=", "y")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("y"));
        let ds = store(&[("A", "??=", "x"), ("A", "=", "set")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("set"));
        let ds = store(&[("A", "=", "set"), ("A", "??=", "x")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("set"));
    }

    #[test]
    fn default_ignores_weak_default() {
        // `?=` reads the value with noweakdefault=True: a `??=` does not count as set.
        let ds = store(&[("A", "??=", "weak"), ("A", "?=", "normal")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("normal"));
        let ds = store(&[("A", "?=", "normal"), ("A", "??=", "weak")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("normal"));
    }

    #[test]
    fn append_ops_ignore_weak_default() {
        let ds = store(&[("A", "??=", "weak"), ("A", "+=", "x")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some(" x"));
    }

    #[test]
    fn immediate_expands_now() {
        let ds = store(&[
            ("B", "=", "one"),
            ("A", ":=", "${B}-${C}"),
            ("B", "=", "two"),
            ("C", "=", "c"),
        ]);
        // ${B} was expanded at the `:=`; ${C} was undefined then and stays as text, so it
        // still expands later (as in BitBake).
        assert_eq!(raw(&ds, "A").as_deref(), Some("one-${C}"));
    }

    #[test]
    fn immediate_self_reference_uses_old_value() {
        let ds = store(&[("A", "=", "a"), ("A", ":=", "${A} b")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("a b"));
    }

    #[test]
    fn immediate_with_callback() {
        let mut ds = DataStore::new();
        ds.apply_with(
            VarOp {
                var: vr("A"),
                op: AssignOp::Immediate,
                value: "${X}".into(),
                loc: loc(1),
                via: vec![],
            },
            |_, t| format!("<{t}>"),
        );
        assert_eq!(raw(&ds, "A").as_deref(), Some("<${X}>"));
    }

    #[test]
    fn immediate_leaves_python() {
        let ds = store(&[("A", ":=", "${@d.getVar('B')}")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("${@d.getVar('B')}"));
    }

    #[test]
    fn append_prepend_with_and_without_space() {
        let ds = store(&[
            ("A", "=", "m"),
            ("A", "+=", "a"),
            ("A", "=+", "p"),
            ("A", ".=", "d"),
            ("A", "=.", "q"),
        ]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("qp m ad"));
    }

    #[test]
    fn plus_equals_on_unset_variable_keeps_leading_space() {
        let ds = store(&[
            ("A", "+=", "x"),
            ("B", "=+", "y"),
            ("C", ".=", "z"),
            ("D", "=.", "w"),
        ]);
        assert_eq!(raw(&ds, "A").as_deref(), Some(" x"));
        assert_eq!(raw(&ds, "B").as_deref(), Some("y "));
        assert_eq!(raw(&ds, "C").as_deref(), Some("z"));
        assert_eq!(raw(&ds, "D").as_deref(), Some("w"));
    }

    #[test]
    fn override_names_are_separate_variables_at_parse_time() {
        let ds = store(&[("A", "=", "a"), ("A:foo", "+=", "x")]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("a"));
        assert_eq!(raw(&ds, "A:foo").as_deref(), Some(" x"));
    }

    #[test]
    fn append_prepend_remove_are_stored_not_applied() {
        let ds = store(&[
            ("A", "=", "a"),
            ("A:append", "=", " x"),
            ("A:prepend:foo", "=", "p "),
            ("A:remove", "=", "a"),
            ("A:foo:append", "=", " y"),
        ]);
        assert_eq!(raw(&ds, "A").as_deref(), Some("a"));
        let v = ds.vars.get("A").unwrap();
        assert_eq!(v.appends.len(), 1);
        assert_eq!(v.appends[0].text, " x");
        assert_eq!(v.appends[0].cond, None);
        assert_eq!(v.prepends[0].cond.as_deref(), Some("foo"));
        assert_eq!(v.removes[0].text, "a");
        let f = ds.vars.get("A:foo").unwrap();
        assert_eq!(f.appends[0].text, " y");
        assert!(f.content.is_none());
        // `A:foo:append` makes `A:foo` a variant of `A`.
        assert_eq!(
            ds.overridedata.get("A").unwrap(),
            &vec![("A:foo".to_string(), "foo".to_string())]
        );
    }

    #[test]
    fn plus_equals_on_append_name_adds_a_space() {
        // `A:append += "x"`: the current value of the name `A:append` is always unset, so
        // the stored append text is " x".
        let ds = store(&[("A:append", "+=", "x")]);
        assert_eq!(ds.vars.get("A").unwrap().appends[0].text, " x");
    }

    #[test]
    fn set_does_not_clear_appends_at_parse_time() {
        let ds = store(&[("A:append", "=", " x"), ("A", "=", "a")]);
        assert_eq!(ds.vars.get("A").unwrap().appends.len(), 1);
    }

    #[test]
    fn unset_clears_everything() {
        let mut ds = store(&[
            ("A", "=", "a"),
            ("A", "??=", "w"),
            ("A[doc]", "=", "d"),
            ("A:append", "=", " x"),
            ("A:foo", "=", "f"),
        ]);
        ds.unset("A", None, loc(10), vec![]);
        assert_eq!(raw(&ds, "A"), None);
        assert_eq!(ds.raw_flag("A", "doc"), None);
        assert!(ds.vars.get("A").unwrap().appends.is_empty());
        // The variant keeps its value but is detached from A.
        assert_eq!(raw(&ds, "A:foo").as_deref(), Some("f"));
        assert!(!ds.overridedata.contains_key("A"));
        assert_eq!(ds.unsets("A").len(), 1);
        // Assigning again works normally.
        apply(&mut ds, "A", "+=", "z", 11);
        assert_eq!(raw(&ds, "A").as_deref(), Some(" z"));
    }

    #[test]
    fn unset_flag() {
        let mut ds = store(&[("A[doc]", "=", "d"), ("A[x]", "??=", "w"), ("A", "=", "v")]);
        ds.unset("A", Some("doc"), loc(5), vec![]);
        assert_eq!(ds.raw_flag("A", "doc"), None);
        // No normal `x` flag: delVarFlag does nothing, the weak default stays.
        ds.unset("A", Some("x"), loc(6), vec![]);
        assert_eq!(ds.raw_flag("A", "x").as_deref(), Some("w"));
        assert_eq!(raw(&ds, "A").as_deref(), Some("v"));
    }

    #[test]
    fn flags_use_all_operators() {
        let ds = store(&[
            ("A[f]", "=", "a"),
            ("A[f]", "+=", "b"),
            ("A[f]", "?=", "no"),
            ("A[g]", "??=", "w1"),
            ("A[g]", "??=", "w2"),
            ("A[h]", "?=", "h"),
            ("A[i]", ".=", "i"),
        ]);
        assert_eq!(ds.raw_flag("A", "f").as_deref(), Some("a b"));
        assert_eq!(ds.raw_flag("A", "g").as_deref(), Some("w2"));
        assert_eq!(ds.raw_flag("A", "h").as_deref(), Some("h"));
        assert_eq!(ds.raw_flag("A", "i").as_deref(), Some("i"));
        assert_eq!(raw(&ds, "A"), None);
        assert_eq!(ds.flag_names("A"), vec!["f", "g", "h", "i"]);
        let h = ds.flag_history("A", "f");
        let st: Vec<_> = h.iter().map(|(_, s)| *s).collect();
        assert_eq!(
            st,
            vec![OpStatus::Applied, OpStatus::Applied, OpStatus::Shadowed]
        );
    }

    #[test]
    fn export_sets_flag() {
        let mut ds = store(&[("A", "=", "a")]);
        ds.mark_exported("A");
        assert_eq!(ds.raw_flag("A", "export").as_deref(), Some("1"));
    }

    #[test]
    fn history_keeps_loc_via_and_order_across_variants() {
        let mut ds = DataStore::new();
        let via = vec![loc(100), loc(200)];
        for (i, name) in ["A", "B", "A:append:foo", "A:foo", "AB"].iter().enumerate() {
            ds.apply(VarOp {
                var: vr(name),
                op: AssignOp::Set,
                value: format!("v{i}"),
                loc: loc(i as u32 + 1),
                via: via.clone(),
            });
        }
        let h = ds.history("A");
        let names: Vec<String> = h.iter().map(|(op, _)| full_name(&op.var)).collect();
        assert_eq!(names, vec!["A", "A:append:foo", "A:foo"]);
        assert_eq!(h[1].0.loc, loc(3));
        assert_eq!(h[1].0.via, via);
        assert!(ds.history("NOPE").is_empty());
    }

    #[test]
    fn fork_isolation() {
        let base = store(&[("A", "=", "a"), ("B", "=", "b")]);
        let mut f1 = base.fork();
        let mut f2 = base.fork();
        apply(&mut f1, "A", "+=", "one", 10);
        apply(&mut f2, "A", "=", "two", 10);
        apply(&mut f2, "C", "=", "c", 11);
        f2.unset("B", None, loc(12), vec![]);
        assert_eq!(raw(&base, "A").as_deref(), Some("a"));
        assert_eq!(raw(&base, "B").as_deref(), Some("b"));
        assert_eq!(raw(&base, "C"), None);
        assert_eq!(base.history("A").len(), 1);
        assert_eq!(raw(&f1, "A").as_deref(), Some("a one"));
        assert_eq!(raw(&f1, "B").as_deref(), Some("b"));
        assert_eq!(f1.history("A").len(), 2);
        assert_eq!(raw(&f2, "A").as_deref(), Some("two"));
        assert_eq!(raw(&f2, "B"), None);
        assert_eq!(raw(&f2, "C").as_deref(), Some("c"));
        assert!(base.unsets("B").is_empty());
        // Unwritten maps are still shared.
        let f3 = base.fork();
        assert!(Arc::ptr_eq(&base.vars, &f3.vars));
        assert!(Arc::ptr_eq(&base.events, &f3.events));
    }

    #[test]
    fn fork_shares_visits_copy() {
        let mut base = DataStore::new();
        base.record_visit(FileVisit {
            loc: loc(1),
            kind: crate::types::FileKind::Conf,
            via: vec![],
        });
        let mut f = base.fork();
        f.record_visit(FileVisit {
            loc: loc(2),
            kind: crate::types::FileKind::Recipe,
            via: vec![],
        });
        assert_eq!(base.files().len(), 1);
        assert_eq!(f.files().len(), 2);
    }

    #[test]
    fn legacy_syntax_is_recorded_not_applied() {
        let ds = store(&[("A", "=", "a"), ("A_append", "=", " x")]);
        assert!(ds.vars.get("A").unwrap().appends.is_empty());
        let h = ds.history("A");
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].1, OpStatus::InactiveOverride);
    }

    #[test]
    fn expand_keys_renames() {
        let mut ds = store(&[
            ("PN", "=", "hello"),
            ("FILES:${PN}-dev", "=", "/usr/include"),
            ("FILES:${PN}-dev:append", "=", " /x"),
            ("RDEPENDS:${PN}", "+=", "a"),
            ("RDEPENDS:hello", "=", "lost"),
            ("SUMMARY:${UNDEFINED}", "=", "s"),
        ]);
        let incomplete = ds.expand_keys();
        assert_eq!(incomplete, vec!["SUMMARY:${UNDEFINED}"]);
        assert_eq!(raw(&ds, "FILES:hello-dev").as_deref(), Some("/usr/include"));
        assert_eq!(ds.vars.get("FILES:hello-dev").unwrap().appends.len(), 1);
        assert_eq!(raw(&ds, "FILES:${PN}-dev"), None);
        // The renamed value replaces the one already under the new name.
        assert_eq!(raw(&ds, "RDEPENDS:hello").as_deref(), Some(" a"));
        assert_eq!(raw(&ds, "SUMMARY:${UNDEFINED}").as_deref(), Some("s"));
        assert!(
            ds.overridedata
                .get("FILES")
                .unwrap()
                .contains(&("FILES:hello-dev".to_string(), "hello-dev".to_string()))
        );
    }

    #[test]
    fn keyword_split() {
        let k = split_keyword;
        assert_eq!(k("A:append"), Some(("A".into(), Keyword::Append, None)));
        assert_eq!(
            k("A:append:foo:bar"),
            Some(("A".into(), Keyword::Append, Some("foo:bar".into())))
        );
        assert_eq!(
            k("A:foo:remove"),
            Some(("A:foo".into(), Keyword::Remove, None))
        );
        assert_eq!(k("A:appendix"), None);
        assert_eq!(k("A:append:Foo"), None);
        assert_eq!(k("A:foo"), None);
        assert_eq!(
            k("A:remove:append"),
            Some(("A".into(), Keyword::Remove, Some("append".into())))
        );
        assert_eq!(k("A:prepend:"), Some(("A".into(), Keyword::Prepend, None)));
    }

    #[test]
    fn override_chain_registration() {
        let c = |s: &str| override_chain(s);
        assert_eq!(
            c("A:a:b"),
            vec![("A:a".into(), "b".into()), ("A".into(), "a:b".into())]
        );
        assert_eq!(c("A:Foo:bar"), vec![("A:Foo".into(), "bar".into())]);
        assert!(c("A:foo:Bar").is_empty());
        assert!(c("A").is_empty());
        assert!(c("FILES:${PN}-dev").is_empty());
        assert_eq!(
            c("FILES:hello-dev"),
            vec![("FILES".into(), "hello-dev".into())]
        );
    }

    #[test]
    fn base_of_skips_braces() {
        assert_eq!(base_of("A:b"), "A");
        assert_eq!(base_of("${X:y}:z"), "${X:y}");
        assert_eq!(base_of("A"), "A");
    }
}
