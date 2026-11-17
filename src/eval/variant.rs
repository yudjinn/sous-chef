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
    fn forms() {
        let f = name_forms("nativesdk-zlib");
        assert!(f.contains(&("zlib".into(), ClassExtend::new("nativesdk"))));
        let f = name_forms("gmp-native");
        assert_eq!(f[0], ("gmp".into(), ClassExtend::new("native")));
    }
}
