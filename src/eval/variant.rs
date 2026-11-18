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
