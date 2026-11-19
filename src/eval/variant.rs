//! Class-extended variants (`BBCLASSEXTEND`): their names and their targets.
//!
//! BitBake 2.18.0 `lib/bb/parse/ast.py` `multi_finalize`: after the bbappends, the recipe's
//! datastore (`safe_d`) is copied and the copy finalised; `BBCLASSEXTEND` and `PN` are read
//! from that finalised copy. Each word `ext` of `BBCLASSEXTEND.split()` is split with
//! `ext.split(':', 2)`: `multilib:lib32` is class `multilib`, variant `lib32`; a plain word is
//! its own class. For each word a new copy of `safe_d` (not finalised) gets, from
//! `extendfunc`: for a word with an argument `BBEXTENDCURR = <class>` and `BBEXTENDVARIANT =
//! <variant>`, for a plain word `PN = "<pn>-<word>"`; then the class is inherited
//! (`BBHandler.inherit_defer` since BitBake 2.16, [`Feature::ClassExtendDeferred`]; at once
//! before) and the copy is finalised. The variant's file name is
//! `virtual:<word>:<recipe file>` (`bb.cache.realfn2virtual`); its `FILE` stays the base
//! recipe file.
//!
//! The names (what the variant's `PN` becomes): `<pn>-native` (word `native`;
//! `extendfunc`), `nativesdk-<pn>` (word `nativesdk`; OE-core Wrynose
//! `classes-recipe/nativesdk.bbclass` `nativesdk_virtclass_handler` renames `<pn>-nativesdk`),
//! `<variant>-<pn>` (word `multilib:<variant>`; `classes/multilib.bbclass`
//! `multilib_virtclass_handler`). Other words are not resolved: what their classes do to
//! `PN` was not checked.
//!
//! A target names a variant either by its name (resolved by
//! [`super::providers::Providers::best_version`]) or by a pointer `virtual:<word>:<layer>:<path>`
//! ([`split_virtual`]), the form BitBake uses for the file name.
//!
//! [`Feature::ClassExtendDeferred`]: crate::release::Feature::ClassExtendDeferred

use serde::Serialize;

use super::datastore::base_of;
use super::expand::certain;
use super::{DataStore, Value};

/// One `BBCLASSEXTEND` word a variant comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ClassExtend {
    /// The word as written in `BBCLASSEXTEND`: `native`, `nativesdk`, `multilib:lib32`.
    pub word: String,
}

impl ClassExtend {
    pub fn new(word: &str) -> ClassExtend {
        ClassExtend {
            word: word.to_string(),
        }
    }

    /// The class inherited: the part before the first `:` (`ext.split(':', 2)[0]`).
    pub fn class(&self) -> &str {
        self.word.split(':').next().unwrap_or(&self.word)
    }

    /// The argument after the class (`ext.split(':', 2)[1]`), `None` for a plain word.
    pub fn variant(&self) -> Option<&str> {
        let mut parts = self.word.splitn(3, ':');
        parts.next();
        parts.next()
    }

    /// The variant's `PN`, given the base recipe's finalised `PN`, for the words whose
    /// naming is known (module documentation); `None` for any other word.
    pub fn variant_name(&self, pn: &str) -> Option<String> {
        match (self.class(), self.variant()) {
            ("native", None) => Some(format!("{pn}-native")),
            ("nativesdk", None) => Some(format!("nativesdk-{pn}")),
            ("multilib", Some(v)) if !v.is_empty() => Some(format!("{v}-{pn}")),
            _ => None,
        }
    }

    /// `virtual:<word>:` (`bb.cache.realfn2virtual`), the prefix of a variant pointer.
    pub fn virtual_prefix(&self) -> String {
        format!("virtual:{}:", self.word)
    }

    /// `the \`native\` variant` / `the \`lib32\` multilib variant`.
    pub fn describe(&self) -> String {
        match (self.class(), self.variant()) {
            ("multilib", Some(v)) => format!("the `{v}` multilib variant"),
            (c, _) => format!("the `{c}` variant"),
        }
    }
}

/// `(base pn, word)` for each form `name` could have: `<pn>-native`, `nativesdk-<pn>`,
/// `<variant>-<pn>` (split at the first `-` only: a multilib variant name with a `-` is not
/// found by name).
pub fn name_forms(name: &str) -> Vec<(String, ClassExtend)> {
    let name = name.trim();
    let mut out = Vec::new();
    if let Some(base) = name.strip_suffix("-native") {
        out.push((base.to_string(), ClassExtend::new("native")));
    }
    if let Some(base) = name.strip_prefix("nativesdk-") {
        out.push((base.to_string(), ClassExtend::new("nativesdk")));
    }
    if let Some((variant, base)) = name.split_once('-') {
        out.push((
            base.to_string(),
            ClassExtend::new(&format!("multilib:{variant}")),
        ));
    }
    out.retain(|(b, _)| !b.is_empty());
    out
}

/// `bb.utils.explode_deps` (BitBake 2.18.0 `lib/bb/utils.py`): the whitespace-separated
/// words, without the version constraints in parentheses (`a (>= 1.0) b` is `a b`).
pub fn explode_deps(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_version = false;
    for w in s.split_whitespace() {
        if w.starts_with('(') {
            in_version = true;
        }
        if !in_version {
            out.push(w);
        }
        if in_version && w.ends_with(')') {
            in_version = false;
        }
    }
    out
}

/// `oe.classextend.native_filter(val, pn, bpn, regex=False, selfref=True)` (OE-core Wrynose
/// `meta/lib/oe/classextend.py`), the filter `native_virtclass_handler` sets on `PROVIDES`
/// (and others): each word of `explode_deps(val)` that is `pn` stays; one containing
/// `-cross-` gets `-cross` replaced by `-native`; one not ending in `-native` becomes
/// `word.replace(pn, bpn) + "-native"`; the rest stay; joined with one space. `None` for an
/// empty value (the filter is not applied: `getVar` filters only a non-empty value) and
/// `Some(Err(()))` when a word contains `pn` and `bpn` is not known.
pub fn native_filter(val: &str, pn: &str, bpn: Option<&str>) -> Option<Result<String, ()>> {
    if val.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for dep in explode_deps(val) {
        if dep == pn {
            out.push(dep.to_string());
        } else if dep.contains("-cross-") {
            out.push(dep.replace("-cross", "-native"));
        } else if !dep.ends_with("-native") {
            let replaced = if dep.contains(pn) {
                match bpn {
                    Some(b) => dep.replace(pn, b),
                    None => return Some(Err(())),
                }
            } else {
                dep.to_string()
            };
            out.push(format!("{replaced}-native"));
        } else {
            out.push(dep.to_string());
        }
    }
    Some(Ok(out.join(" ")))
}

/// `oe.classextend.native_filter(val, pn, bpn, regex, selfref)` in full (OE-core Wrynose
/// `meta/lib/oe/classextend.py`) on a non-empty value: each word of `explode_deps(val)`: with
/// `regex`, a `^...$` word gets `-native$` (its `pn` replaced by `bpn`) unless it ends with
/// `-native$`; the word `pn` stays (dropped when not `selfref`); a word with `-cross-` gets
/// `-cross` replaced by `-native`; one not ending in `-native` becomes `word.replace(pn,
/// bpn) + "-native"`; the rest stay; joined with one space.
pub fn native_filter_full(val: &str, pn: &str, bpn: &str, regex: bool, selfref: bool) -> String {
    let mut out = Vec::new();
    for dep in explode_deps(val) {
        if regex && dep.starts_with('^') && dep.ends_with('$') {
            if dep.ends_with("-native$") {
                out.push(dep.to_string());
            } else {
                out.push(format!("{}-native$", dep[..dep.len() - 1].replace(pn, bpn)));
            }
        } else if dep == pn {
            if selfref {
                out.push(dep.to_string());
            }
        } else if dep.contains("-cross-") {
            out.push(dep.replace("-cross", "-native"));
        } else if !dep.ends_with("-native") {
            out.push(format!("{}-native", dep.replace(pn, bpn)));
        } else {
            out.push(dep.to_string());
        }
    }
    out.join(" ")
}

/// `bb.utils.explode_dep_versions2(s)` (BitBake 2.18.0 `lib/bb/utils.py`, `sort=True`, the
/// default since BitBake 1.38): the names in sorted order (python's string order, which is
/// Rust's byte order of UTF-8), each with its version constraints (`">= 1.0"`), a repeated
/// name keeping the constraints it had unless it has none. `Err` where python raises
/// (`VersionStringException`: a `(` not followed by a comparison operator).
pub fn explode_dep_versions2(s: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut r: Vec<(String, Vec<String>)> = Vec::new();
    let text = s.replace(',', "");
    let mut lastdep: Option<String> = None;
    let mut lastcmp = String::new();
    let mut lastver = String::new();
    let mut incmp = false;
    let mut inversion = false;
    for word in text.split_whitespace() {
        let mut i = word.to_string();
        if i.starts_with('(') {
            incmp = true;
            i = i[1..].trim().to_string();
            if i.is_empty() {
                continue;
            }
        }
        if incmp {
            incmp = false;
            inversion = true;
            let two = ["<=", "=<", "<<", "==", "!=", ">=", "=>", ">>"];
            if two.iter().any(|p| i.starts_with(p)) {
                lastcmp = i[..2].to_string();
                i = i[2..].to_string();
            } else if i.starts_with(['<', '>', '=']) {
                lastcmp = i[..1].to_string();
                i = i[1..].to_string();
            } else {
                return Err(format!(
                    "invalid version specification \"({i}\" (a VersionStringException)"
                ));
            }
            if i.is_empty() {
                continue;
            }
        }
        if inversion {
            if let Some(stripped) = i.strip_suffix(')') {
                i = stripped.to_string();
                inversion = false;
                if !lastver.is_empty() && !i.is_empty() {
                    lastver.push(' ');
                }
            }
            if !i.is_empty() {
                lastver.push_str(&i);
                // `r[lastdep]`: python's `None` key when no name came first; not reproduced.
                let Some(dep) = &lastdep else {
                    return Err("a version constraint before any name".into());
                };
                let entry = format!("{lastcmp} {lastver}");
                match r.iter_mut().find(|(n, _)| n == dep) {
                    Some((_, v)) => v.push(entry),
                    None => r.push((dep.clone(), vec![entry])),
                }
            }
            continue;
        }
        lastver.clear();
        lastcmp.clear();
        match r.iter_mut().find(|(n, _)| *n == i) {
            Some((_, v)) if !v.is_empty() => {}
            Some((_, v)) => v.clear(),
            None => r.push((i.clone(), Vec::new())),
        }
        lastdep = Some(i);
    }
    r.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(r)
}

/// `bb.utils.join_deps(deps, commasep=False)`: `name` or `name (constraint)` per constraint,
/// joined with one space.
pub fn join_deps(deps: &[(String, Vec<String>)]) -> String {
    let mut out = Vec::new();
    for (dep, vers) in deps {
        if vers.is_empty() {
            out.push(dep.clone());
        } else {
            for v in vers {
                out.push(format!("{dep} ({v})"));
            }
        }
    }
    out.join(" ")
}

/// A read filter OE-core's class-extension code installs with `d.setVarFilter` (BitBake
/// 2.18.0 `data_smart.setVarFilter`; `getVarFlag` applies the filter of the name's base,
/// `var.split(":")[0]`, to the expanded `_content` of every read whose value is not empty).
/// The expressions are the ones OE-core Wrynose writes (`classes-recipe/native.bbclass`
/// `native_virtclass_handler`, `lib/oe/classextend.py` `ClassExtender.set_filter`), with
/// the arguments they were given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum VarFilter {
    /// `native_filter(val, '<pn>', '<bpn>'[, regex=True][, selfref=False])`.
    Native {
        pn: String,
        bpn: String,
        regex: bool,
        selfref: bool,
    },
    /// `suffix_filter(val, '<extname>', [<prefixes>])`.
    Suffix {
        extname: String,
        prefixes: Vec<String>,
    },
    /// `suffix_filter_deps(val, '<extname>', [<prefixes>])`.
    SuffixDeps {
        extname: String,
        prefixes: Vec<String>,
    },
}

impl VarFilter {
    /// The filter applied to a non-empty expanded value; `Err` where python would raise.
    pub fn apply(&self, val: &str) -> Result<String, String> {
        match self {
            VarFilter::Native {
                pn,
                bpn,
                regex,
                selfref,
            } => Ok(native_filter_full(val, pn, bpn, *regex, *selfref)),
            VarFilter::Suffix { extname, prefixes } => {
                let p: Vec<&str> = prefixes.iter().map(String::as_str).collect();
                Ok(val
                    .split_whitespace()
                    .map(|w| add_suffix(w, extname, &p))
                    .collect::<Vec<_>>()
                    .join(" "))
            }
            VarFilter::SuffixDeps { extname, prefixes } => {
                let p: Vec<&str> = prefixes.iter().map(String::as_str).collect();
                // `newdeps[add_suffix(dep)] = deps[dep]` into an OrderedDict: a later name
                // mapping to the same word replaces its constraints in place.
                let mut out: Vec<(String, Vec<String>)> = Vec::new();
                for (dep, vers) in explode_dep_versions2(val)? {
                    let n = add_suffix(&dep, extname, &p);
                    match out.iter_mut().find(|(x, _)| *x == n) {
                        Some((_, v)) => *v = vers,
                        None => out.push((n, vers)),
                    }
                }
                Ok(join_deps(&out))
            }
        }
    }

    /// What one dependency word reads as through the filter (`None`: dropped), for a list
    /// of words such as `deps` shows: the word-by-word part of [`VarFilter::apply`] (no
    /// sorting, no version constraints).
    pub fn map_word(&self, word: &str) -> Option<String> {
        let t = match self {
            VarFilter::SuffixDeps { extname, prefixes }
            | VarFilter::Suffix { extname, prefixes } => {
                let p: Vec<&str> = prefixes.iter().map(String::as_str).collect();
                add_suffix(word, extname, &p)
            }
            VarFilter::Native { .. } => self.apply(word).unwrap_or_default(),
        };
        (!t.is_empty()).then_some(t)
    }

    /// The expression as OE-core writes it, for notes.
    pub fn describe(&self) -> String {
        let list = |p: &[String]| {
            format!(
                "[{}]",
                p.iter()
                    .map(|x| format!("'{x}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        match self {
            VarFilter::Native {
                pn,
                bpn,
                regex,
                selfref,
            } => format!(
                "native_filter(val, '{pn}', '{bpn}'{}{})",
                if *regex { ", regex=True" } else { "" },
                if *selfref { "" } else { ", selfref=False" }
            ),
            VarFilter::Suffix { extname, prefixes } => {
                format!("suffix_filter(val, '{extname}', {})", list(prefixes))
            }
            VarFilter::SuffixDeps { extname, prefixes } => {
                format!("suffix_filter_deps(val, '{extname}', {})", list(prefixes))
            }
        }
    }
}

impl DataStore {
    /// The read filter class-extension code installed on `name`'s base, if any.
    pub fn var_filter(&self, name: &str) -> Option<&VarFilter> {
        self.notes.filters.get(base_of(name)).map(|(f, _)| f)
    }

    /// BitBake's read filter (`getVarFlag`: `bb.filter.apply_filters` on a non-empty
    /// expanded `_content`) applied to the finalised value `v` of `name`: computed when `v`
    /// is certain; when the filter would raise, `v` is flagged instead. An uncertain value
    /// is left as it is (it is flagged already).
    pub(crate) fn apply_read_filter(&self, name: &str, v: &mut Value) {
        if v.text.is_empty() || !certain(v) {
            return;
        }
        let Some(f) = self.var_filter(name) else {
            return;
        };
        match f.apply(&v.text) {
            Ok(t) => v.text = t,
            Err(_) => v.unevaluated_python = true,
        }
    }
}

/// `oe.classextend.add_suffix(val, extname, prefixes)` (OE-core Wrynose
/// `meta/lib/oe/classextend.py`): how `ClassExtender.set_filter(..., deps=False)`
/// (`suffix_filter`) renames one word for the `nativesdk` and multilib variants.
pub fn add_suffix(val: &str, extname: &str, prefixes: &[&str]) -> String {
    if val.starts_with(&format!("{extname}-")) {
        return val.to_string();
    }
    if val.ends_with("-native")
        || val.ends_with("-native-runtime")
        || val.contains("nativesdk-")
        || val.contains("-cross-")
        || val.contains("-crosssdk-")
    {
        return val.to_string();
    }
    if prefixes.iter().any(|p| val.starts_with(&format!("{p}-"))) {
        return val.to_string();
    }
    if val.starts_with("kernel-") || val == "virtual/kernel" {
        return val.to_string();
    }
    if val.starts_with("rtld") || val.ends_with("-crosssdk") {
        return val.to_string();
    }
    let mut val = val.to_string();
    let suffix = format!("-{extname}");
    if val.ends_with(&suffix) {
        val = val.replace(&suffix, "");
    }
    if let Some(subs) = val.strip_prefix("virtual/") {
        if val.matches('-').count() >= 3 && val.ends_with("-go") {
            return val;
        }
        if !subs.starts_with(extname) {
            return format!("virtual/{extname}-{subs}");
        }
        return val;
    }
    if val.starts_with('/') || (val.starts_with("${") && val.ends_with('}')) {
        return val;
    }
    if !val.starts_with(extname) {
        return format!("{extname}-{val}");
    }
    val
}

/// Whether [`add_suffix`] decides `val` before it looks at its `prefixes` (it returns early
/// for a word starting with `<extname>-`, ending in `-native` / `-native-runtime`, or
/// holding `nativesdk-`, `-cross-`, `-crosssdk-`), or the prefixes cannot match (a prefix
/// `p` matches only a word starting with `p-`, so a word without `-` is not affected).
fn prefixes_cannot_matter(val: &str, extname: &str) -> bool {
    !val.contains('-')
        || val.starts_with(&format!("{extname}-"))
        || val.ends_with("-native")
        || val.ends_with("-native-runtime")
        || val.contains("nativesdk-")
        || val.contains("-cross-")
        || val.contains("-crosssdk-")
}

/// `oe.classextend.suffix_filter(val, extname, prefixes)` (OE-core Wrynose
/// `meta/lib/oe/classextend.py`), the filter `ClassExtender.set_filter(var, deps=False)`
/// installs: [`add_suffix`] on each word of `val.split()`, joined with one space. BitBake
/// applies it only to a non-empty value (`None` for an empty one). `prefixes` `None`: not
/// known (the multilib classes read them from `MULTILIB_VARIANTS`, which OE-core computes
/// with python); then `Some(Err(word))` names the first word whose result depends on them.
pub fn suffix_filter(
    val: &str,
    extname: &str,
    prefixes: Option<&[&str]>,
) -> Option<Result<String, String>> {
    if val.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for w in val.split_whitespace() {
        match prefixes {
            Some(p) => out.push(add_suffix(w, extname, p)),
            None if prefixes_cannot_matter(w, extname) => out.push(add_suffix(w, extname, &[])),
            None => return Some(Err(w.to_string())),
        }
    }
    Some(Ok(out.join(" ")))
}

/// `(base word, multilib word)` pairs such that the multilib variant `<v>` (one of
/// `variants`, the multilib variants the configuration's `BBCLASSEXTEND` lists) of a recipe
/// providing (or named) `base word` may provide `name` through the `PROVIDES` its class
/// rewrites with [`suffix_filter`]: `x` becomes `<v>-x` (and `x-<v>` too), `virtual/x`
/// becomes `virtual/<v>-x`, and the words [`add_suffix`] passes through stay. Candidates
/// only: the variant is evaluated to confirm.
pub fn multilib_provides_forms(name: &str, variants: &[&str]) -> Vec<(String, ClassExtend)> {
    let name = name.trim();
    let mut out = Vec::new();
    for v in variants {
        if v.is_empty() {
            continue;
        }
        let ext = ClassExtend::new(&format!("multilib:{v}"));
        if add_suffix(name, v, variants) == name {
            out.push((name.to_string(), ext.clone()));
        }
        if let Some(rest) = name.strip_prefix(&format!("{v}-")) {
            out.push((rest.to_string(), ext.clone()));
            out.push((format!("{rest}-{v}"), ext.clone()));
        }
        if let Some(rest) = name.strip_prefix(&format!("virtual/{v}-")) {
            out.push((format!("virtual/{rest}"), ext.clone()));
            out.push((format!("virtual/{rest}-{v}"), ext.clone()));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|(b, e)| {
        !b.is_empty() && !b.ends_with('-') && seen.insert((b.clone(), e.word.clone()))
    });
    out
}

/// `(base word, variant word)` pairs such that the variant of a recipe providing (or named)
/// `base word` may provide `name` through the `PROVIDES` its class rewrites, or through its
/// `PN` ([`name_forms`]): for `native`, `native_filter` turns `x` into `x-native` and keeps
/// a word ending in `-native`, and turns `a-cross-b` into `a-native-b`; for `nativesdk`,
/// `add_suffix` turns `x` into `nativesdk-x` and `virtual/x` into `virtual/nativesdk-x`, and
/// keeps the words it passes through. Candidates only: the variant is evaluated to confirm.
/// The multilib forms here are by `PN` only ([`name_forms`]); the ones through `PROVIDES`
/// need the configuration's multilib variants ([`multilib_provides_forms`]).
pub fn provides_forms(name: &str) -> Vec<(String, ClassExtend)> {
    let name = name.trim();
    let mut out = name_forms(name);
    let native = ClassExtend::new("native");
    let sdk = ClassExtend::new("nativesdk");
    if name.ends_with("-native") {
        out.push((name.to_string(), native.clone()));
    }
    if name.contains("-native") {
        let cross = name.replace("-native", "-cross");
        if cross.contains("-cross-") && cross != name {
            out.push((cross, native));
        }
    }
    if let Some(rest) = name.strip_prefix("virtual/nativesdk-") {
        out.push((format!("virtual/{rest}"), sdk.clone()));
    }
    if add_suffix(name, "nativesdk", &[]) == name {
        out.push((name.to_string(), sdk));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|(b, e)| !b.is_empty() && seen.insert((b.clone(), e.word.clone())));
    out
}

/// A variant pointer `virtual:<word>:<recipe>` (`<recipe>` a `layer:path` or an absolute
/// path to a `.bb`; the word is `multilib:<variant>` or one name): the word and the recipe
/// part.
pub fn split_virtual(target: &str) -> Option<(ClassExtend, &str)> {
    let rest = target.strip_prefix("virtual:")?;
    let (first, after) = rest.split_once(':')?;
    let (word, recipe) = if first == "multilib" {
        let (v, recipe) = after.split_once(':')?;
        (format!("multilib:{v}"), recipe)
    } else {
        (first.to_string(), after)
    };
    if word.is_empty() || recipe.is_empty() {
        return None;
    }
    Some((ClassExtend::new(&word), recipe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_names() {
        let n = ClassExtend::new("native");
        assert_eq!((n.class(), n.variant()), ("native", None));
        assert_eq!(n.variant_name("gmp").as_deref(), Some("gmp-native"));
        let m = ClassExtend::new("multilib:lib32");
        assert_eq!((m.class(), m.variant()), ("multilib", Some("lib32")));
        assert_eq!(m.variant_name("zlib").as_deref(), Some("lib32-zlib"));
        assert_eq!(
            ClassExtend::new("nativesdk")
                .variant_name("zlib")
                .as_deref(),
            Some("nativesdk-zlib")
        );
        assert_eq!(ClassExtend::new("cross").variant_name("x"), None);
    }

    #[test]
    fn pointers() {
        let (e, r) = split_virtual("virtual:native:core:recipes/gmp_6.3.bb").unwrap();
        assert_eq!((e.word.as_str(), r), ("native", "core:recipes/gmp_6.3.bb"));
        let (e, r) = split_virtual("virtual:multilib:lib32:/abs/x.bb").unwrap();
        assert_eq!((e.word.as_str(), r), ("multilib:lib32", "/abs/x.bb"));
        assert!(split_virtual("core:x.bb").is_none());
        assert!(split_virtual("virtual:native:").is_none());
    }

    /// The read filters against OE-core Wrynose `lib/oe/classextend.py` with BitBake 2.18's
    /// `bb.utils`, run by python 3 (`native_filter`, `suffix_filter_deps`, `suffix_filter`).
    #[test]
    fn read_filters_like_classextend() {
        let native = |pn: &str, bpn: &str, regex, selfref| VarFilter::Native {
            pn: pn.into(),
            bpn: bpn.into(),
            regex,
            selfref,
        };
        let sdk_deps = VarFilter::SuffixDeps {
            extname: "nativesdk".into(),
            prefixes: Vec::new(),
        };
        let sdk = VarFilter::Suffix {
            extname: "nativesdk".into(),
            prefixes: Vec::new(),
        };
        let gmp = native("gmp-native", "gmp", false, false);
        let quilt = native("quilt-native", "quilt", false, false);
        let tool = native("tool-native", "tool", false, false);
        let pkgs = native("gmp-native", "gmp", false, true);
        let dynamic = native("gmp-native", "gmp", true, true);
        let cases: &[(&VarFilter, &str, Option<&str>)] = &[
            (
                &gmp,
                "autoconf-native automake-native libtool-native    texinfo-dummy-native",
                Some("autoconf-native automake-native libtool-native texinfo-dummy-native"),
            ),
            (&quilt, "   ", Some("")),
            (
                &tool,
                "zlib pkgconf-native binutils-cross-arm tool-native tool-native-dev (>= 1.0) gmp-native-x",
                Some(
                    "zlib-native pkgconf-native binutils-native-arm tool-dev-native gmp-native-x-native",
                ),
            ),
            (
                &pkgs,
                "libgmpxx gmp-native-src gmp-native-dbg gmp-native",
                Some("libgmpxx-native gmp-src-native gmp-dbg-native gmp-native"),
            ),
            (
                &dynamic,
                "^gmp-native-locale-.*$ ^x-native$ gmp-native-y",
                Some("^gmp-locale-.*-native$ ^x-native$ gmp-y-native"),
            ),
            (
                &sdk_deps,
                "virtual/cross-cc virtual/compilerlibs virtual/libc  chrpath-replacement-native",
                Some(
                    "chrpath-replacement-native virtual/nativesdk-compilerlibs virtual/nativesdk-cross-cc virtual/nativesdk-libc",
                ),
            ),
            (
                &sdk_deps,
                "chrpath-replacement-native virtual/nativesdk-compilerlibs gcc-crosssdk-x86_64-oesdk-linux virtual/nativesdk-libc",
                Some(
                    "chrpath-replacement-native gcc-crosssdk-x86_64-oesdk-linux virtual/nativesdk-compilerlibs virtual/nativesdk-libc",
                ),
            ),
            (
                &sdk_deps,
                "b (>= 1.0) a, c (< 2) (> 1) b nativesdk-b kernel-x",
                Some("nativesdk-a nativesdk-b nativesdk-c (< 2) nativesdk-c (> 2 1) kernel-x"),
            ),
            (
                &sdk_deps,
                "zz (>=1.0 foo) yy (= 2)",
                Some("nativesdk-yy (= 2) nativesdk-zz (>= 1.0) nativesdk-zz (>= 1.0 foo)"),
            ),
            // VersionStringException in python.
            (&sdk_deps, "a (1.0)", None),
            (
                &sdk,
                "ptest-runner virtual/foo rtld-x /bin/sh ${FOO} libz-nativesdk",
                Some(
                    "nativesdk-ptest-runner virtual/nativesdk-foo rtld-x /bin/sh ${FOO} nativesdk-libz",
                ),
            ),
        ];
        for (f, val, want) in cases {
            assert_eq!(
                f.apply(val).ok().as_deref(),
                *want,
                "{} on {val:?}",
                f.describe()
            );
        }
        assert_eq!(
            native("gmp-native", "gmp", true, false).describe(),
            "native_filter(val, 'gmp-native', 'gmp', regex=True, selfref=False)"
        );
        assert_eq!(
            sdk_deps.describe(),
            "suffix_filter_deps(val, 'nativesdk', [])"
        );
        // Word by word, as deps lists them.
        assert_eq!(tool.map_word("zlib").as_deref(), Some("zlib-native"));
        assert_eq!(tool.map_word("tool-native"), None);
        assert_eq!(
            sdk_deps.map_word("virtual/libc").as_deref(),
            Some("virtual/nativesdk-libc")
        );
    }

    #[test]
    fn class_extension_filters() {
        assert_eq!(explode_deps("a (>= 1.0) b (<< 2) c"), ["a", "b", "c"]);
        let f = |v: &str, bpn: Option<&str>| native_filter(v, "gmp-native", bpn);
        assert_eq!(f("", None), None);
        assert_eq!(
            f("virtual/gmp gmp-native x-cross-arm y-native", None),
            Some(Ok(
                "virtual/gmp-native gmp-native x-native-arm y-native".into()
            ))
        );
        // A word holding pn needs bpn.
        assert_eq!(f("gmp-native-extra", None), Some(Err(())));
        assert_eq!(
            f("gmp-native-extra", Some("gmp")),
            Some(Ok("gmp-extra-native".into()))
        );
        assert_eq!(add_suffix("zlib", "nativesdk", &[]), "nativesdk-zlib");
        assert_eq!(
            add_suffix("virtual/libc", "nativesdk", &[]),
            "virtual/nativesdk-libc"
        );
        assert_eq!(
            add_suffix("virtual/kernel", "nativesdk", &[]),
            "virtual/kernel"
        );
        assert_eq!(
            add_suffix("zlib-nativesdk", "nativesdk", &[]),
            "nativesdk-zlib"
        );
        assert_eq!(add_suffix("lib32-x", "lib32", &[]), "lib32-x");
        assert_eq!(add_suffix("x", "lib32", &["lib64"]), "lib32-x");
    }

    #[test]
    fn provides_forms_cover_the_filters() {
        let f = provides_forms("virtual/foo-native");
        assert!(f.contains(&("virtual/foo".into(), ClassExtend::new("native"))));
        assert!(f.contains(&("virtual/foo-native".into(), ClassExtend::new("native"))));
        let f = provides_forms("virtual/nativesdk-libc");
        assert!(f.contains(&("virtual/libc".into(), ClassExtend::new("nativesdk"))));
        let f = provides_forms("gcc-native-arm");
        assert!(f.contains(&("gcc-cross-arm".into(), ClassExtend::new("native"))));
    }

    #[test]
    fn forms() {
        let f = name_forms("nativesdk-zlib");
        assert!(f.contains(&("zlib".into(), ClassExtend::new("nativesdk"))));
        let f = name_forms("gmp-native");
        assert_eq!(f[0], ("gmp".into(), ClassExtend::new("native")));
    }
}
