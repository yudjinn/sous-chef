//! Two of the reasons BitBake skips a recipe that are decided by comparing word lists:
//! OE-core's `features_check.bbclass` and the `RecipeParsed` branch of `base_eventhandler`
//! (`classes-global/base.bbclass`). Pure functions over [`Words`]: what is known of a value
//! (its known words, and whether that is all of it). The replay builds the [`Words`] from
//! the evaluated recipe (`replay::finalize`) and turns the outcome into the "would skip" /
//! "may skip" notes; [`features_skip`] and [`virtual_provider_skip`] run the same checks on
//! given values (the live comparison runs them with BitBake's).
//!
//! **`features_check.bbclass`** (OE-core Wrynose `33a6bd6cc8`, `classes-recipe/`,
//! anonymous python; the same on Whinlatter; Scarthgap `origin/scarthgap` has no `TUNE` kind,
//! tests `PARSE_ALL_RECIPES` as a plain string and words the `DISTRO` message like the
//! others): unless `PARSE_ALL_RECIPES` is true, for each `kind` of `DISTRO`, `MACHINE`,
//! `COMBINED`, `IMAGE`, `TUNE` in that order, with `features = set(<kind>_FEATURES.split())`:
//! `ANY_OF_<kind>_FEATURES` non-empty and disjoint from `features` raises
//! `SkipRecipe("one of '<any>' needs to be in <kind>_FEATURES")`; then the words of
//! `REQUIRED_<kind>_FEATURES` missing from `features` raise ("using DISTRO '<DISTRO>', which
//! is missing required DISTRO_FEATURES: '<missing>'" for `DISTRO`, else "missing required
//! <kind> feature[s] '<missing>' (not in <kind>_FEATURES)"); then the words of
//! `CONFLICT_<kind>_FEATURES` in `features` raise ("conflicting <kind> feature[s] '<x>' (in
//! <kind>_FEATURES)"). The first raise ends the check. (Python joins a `set`, so BitBake's
//! word order in the message is arbitrary; here it is sorted.) A kind with none of the
//! three variables set is passed over, which only matters for the class's "doesn't use it"
//! warning: empty lists check nothing.
//!
//! **`base_eventhandler` on `RecipeParsed`** (same file and release; the same code on
//! Kirkstone and Scarthgap except that `SOURCE_MIRROR_FETCH` is tested as a plain string
//! there): unless `SOURCE_MIRROR_FETCH` is true, for each word `p` of the recipe's
//! `PROVIDES` (expanded, after finalisation) that starts with `virtual/` and is not in
//! `BB_MULTI_PROVIDER_ALLOWED`, `PREFERRED_PROVIDER_<p>` **read from the recipe's own
//! datastore** (expanded, overrides applied) that is set, non-empty and not the recipe's
//! `PN` raises `SkipRecipe("PREFERRED_PROVIDER_<p> set to <x>, not <pn>")`. It does not
//! read `PREFERRED_PROVIDERS` and it does not look at other recipes.

use std::collections::BTreeSet;

/// What is known of a list value: its known words and whether they are all of it (no
/// python, no undefined variable, nothing that changes it unseen). `why` says what is not
/// known, for a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    pub known: BTreeSet<String>,
    pub complete: bool,
    pub why: Option<String>,
}

impl Default for Words {
    /// Unset: empty and known.
    fn default() -> Words {
        Words::of("")
    }
}

impl Words {
    /// A value known completely (split on whitespace).
    pub fn of(text: &str) -> Words {
        Words {
            known: text.split_whitespace().map(str::to_string).collect(),
            complete: true,
            why: None,
        }
    }

    fn is_empty_known(&self) -> bool {
        self.known.is_empty() && self.complete
    }
}

/// The outcome of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Pass,
    /// BitBake's `SkipRecipe` message.
    Skip(String),
    /// Not decidable; what is missing.
    Unknown(String),
}

/// The kinds `features_check.bbclass` goes through, in its order (Wrynose).
pub const FEATURE_KINDS: [&str; 5] = ["DISTRO", "MACHINE", "COMBINED", "IMAGE", "TUNE"];

/// The lists of one kind.
#[derive(Debug, Clone, Default)]
pub struct KindLists {
    pub any_of: Words,
    pub required: Words,
    pub conflict: Words,
    pub features: Words,
    /// `d.getVar(kind)` (`DISTRO`, ...), for the `DISTRO` message.
    pub kind_value: String,
}

fn joined(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(" ")
}

fn plural(n: usize) -> &'static str {
    if n > 1 { "s" } else { "" }
}

fn unknown(what: &str, w: &Words) -> String {
    format!(
        "{what} is not known statically ({})",
        w.why.as_deref().unwrap_or("not evaluated")
    )
}

/// The checks of one kind, in the class's order; stops at the first [`Check::Skip`].
/// `distro_message`: the release words the `DISTRO` message with the distro name.
pub fn features_kind(kind: &str, l: &KindLists, distro_message: bool) -> Vec<Check> {
    let fv = format!("{kind}_FEATURES");
    let mut out = Vec::new();
    let f = &l.features;
    // ANY_OF
    let a = &l.any_of;
    let any_name = format!("ANY_OF_{kind}_FEATURES");
    if !a.is_empty_known() {
        if a.known.iter().any(|w| f.known.contains(w)) {
            out.push(Check::Pass);
        } else if a.complete && f.complete {
            out.push(Check::Skip(format!(
                "one of '{}' needs to be in {fv}",
                joined(&a.known)
            )));
            return out;
        } else {
            let mut why = Vec::new();
            if !a.complete {
                why.push(unknown(&any_name, a));
            }
            if !f.complete {
                why.push(unknown(&fv, f));
            }
            out.push(Check::Unknown(format!(
                "features_check: {any_name} (known words: '{}') against {fv} (known words: '{}'); {}",
                joined(&a.known),
                joined(&f.known),
                why.join("; ")
            )));
        }
    }
    // REQUIRED
    let r = &l.required;
    let req_name = format!("REQUIRED_{kind}_FEATURES");
    if !r.is_empty_known() {
        let missing: BTreeSet<String> = r.known.difference(&f.known).cloned().collect();
        if !missing.is_empty() && f.complete {
            let m = joined(&missing);
            out.push(Check::Skip(if kind == "DISTRO" && distro_message {
                format!(
                    "using {kind} '{}', which is missing required {fv}: '{m}'",
                    l.kind_value
                )
            } else {
                format!(
                    "missing required {} feature{} '{m}' (not in {fv})",
                    kind.to_lowercase(),
                    plural(missing.len())
                )
            }));
            return out;
        } else if missing.is_empty() && r.complete {
            out.push(Check::Pass);
        } else {
            let mut why = Vec::new();
            if !missing.is_empty() {
                why.push(format!(
                    "'{}' not among the known words of {fv}, and {}",
                    joined(&missing),
                    unknown(&fv, f)
                ));
            }
            if !r.complete {
                why.push(unknown(&req_name, r));
            }
            out.push(Check::Unknown(format!(
                "features_check: {req_name} = '{}'; {}",
                joined(&r.known),
                why.join("; ")
            )));
        }
    }
    // CONFLICT
    let c = &l.conflict;
    let conf_name = format!("CONFLICT_{kind}_FEATURES");
    if !c.is_empty_known() {
        let hits: BTreeSet<String> = c.known.intersection(&f.known).cloned().collect();
        if !hits.is_empty() {
            out.push(Check::Skip(format!(
                "conflicting {} feature{} '{}' (in {fv})",
                kind.to_lowercase(),
                plural(hits.len()),
                joined(&hits)
            )));
            return out;
        } else if c.complete && f.complete {
            out.push(Check::Pass);
        } else {
            let mut why = Vec::new();
            if !c.complete {
                why.push(unknown(&conf_name, c));
            }
            if !f.complete {
                why.push(unknown(&fv, f));
            }
            out.push(Check::Unknown(format!(
                "features_check: {conf_name} = '{}' matches none of the known words of {fv}; {}",
                joined(&c.known),
                why.join("; ")
            )));
        }
    }
    out
}

/// `features_check.bbclass` on known values (whitespace-separated lists): BitBake's
/// `SkipRecipe` message, or `None` when the recipe passes. `kind_value` is `d.getVar(kind)`.
pub fn features_skip(
    kind: &str,
    kind_value: &str,
    any_of: &str,
    required: &str,
    conflict: &str,
    features: &str,
) -> Option<String> {
    let l = KindLists {
        any_of: Words::of(any_of),
        required: Words::of(required),
        conflict: Words::of(conflict),
        features: Words::of(features),
        kind_value: kind_value.to_string(),
    };
    features_kind(kind, &l, true)
        .into_iter()
        .find_map(|c| match c {
            Check::Skip(m) => Some(m),
            _ => None,
        })
}

/// The `RecipeParsed` check of `base_eventhandler` for one `virtual/` word `p` of
/// `PROVIDES` (not in `BB_MULTI_PROVIDER_ALLOWED`): `preferred` is the recipe's own
/// `PREFERRED_PROVIDER_<p>` (`None` or empty: unset). BitBake's message when skipped.
pub fn virtual_provider_skip(p: &str, preferred: Option<&str>, pn: &str) -> Option<String> {
    match preferred {
        Some(x) if !x.is_empty() && x != pn => {
            Some(format!("PREFERRED_PROVIDER_{p} set to {x}, not {pn}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_words(known: &str) -> Words {
        Words {
            complete: false,
            why: Some("it holds python".into()),
            ..Words::of(known)
        }
    }

    #[test]
    fn required_missing_is_a_skip_with_bitbakes_message() {
        assert_eq!(
            features_skip("DISTRO", "kiss", "", "systemd", "", "acl ipv4").as_deref(),
            Some("using DISTRO 'kiss', which is missing required DISTRO_FEATURES: 'systemd'")
        );
        assert_eq!(
            features_skip("MACHINE", "", "", "a b", "", "a").as_deref(),
            Some("missing required machine feature 'b' (not in MACHINE_FEATURES)")
        );
        assert_eq!(
            features_skip("DISTRO", "kiss", "", "systemd", "", "systemd usrmerge"),
            None
        );
    }

    #[test]
    fn any_of_and_conflict() {
        assert_eq!(
            features_skip("DISTRO", "d", "x11 wayland", "", "", "opengl").as_deref(),
            Some("one of 'wayland x11' needs to be in DISTRO_FEATURES")
        );
        assert_eq!(
            features_skip("DISTRO", "d", "x11 wayland", "", "", "x11"),
            None
        );
        assert_eq!(
            features_skip("COMBINED", "", "", "", "a b", "b a c").as_deref(),
            Some("conflicting combined features 'a b' (in COMBINED_FEATURES)")
        );
        // The first failing check ends it: ANY_OF before REQUIRED.
        assert_eq!(
            features_skip("DISTRO", "d", "q", "r", "", "").as_deref(),
            Some("one of 'q' needs to be in DISTRO_FEATURES")
        );
    }

    #[test]
    fn unknown_parts_make_it_undecided_never_a_guess() {
        // A required word not among the known words of a value with python: may skip.
        let l = KindLists {
            required: Words::of("systemd"),
            features: unknown_words("acl"),
            ..KindLists::default()
        };
        assert!(matches!(
            features_kind("DISTRO", &l, true).as_slice(),
            [Check::Unknown(m)] if m.contains("systemd") && m.contains("it holds python")
        ));
        // Found among the known words: passes whatever the rest is.
        let l = KindLists {
            required: Words::of("acl"),
            features: unknown_words("acl"),
            ..KindLists::default()
        };
        assert_eq!(features_kind("DISTRO", &l, true), [Check::Pass]);
        // A conflict on a known word is a skip even when the rest is unknown.
        let l = KindLists {
            conflict: Words::of("acl"),
            features: unknown_words("acl"),
            ..KindLists::default()
        };
        assert!(matches!(
            features_kind("DISTRO", &l, true).as_slice(),
            [Check::Skip(_)]
        ));
        // No conflict among the known words of an unknown value: undecided.
        let l = KindLists {
            conflict: Words::of("x11"),
            features: unknown_words("acl"),
            ..KindLists::default()
        };
        assert!(matches!(
            features_kind("DISTRO", &l, true).as_slice(),
            [Check::Unknown(_)]
        ));
        // Nothing set: nothing checked.
        assert!(features_kind("DISTRO", &KindLists::default(), true).is_empty());
    }

    #[test]
    fn virtual_provider() {
        assert_eq!(
            virtual_provider_skip("virtual/bootloader", Some("u-boot-ti"), "u-boot").as_deref(),
            Some("PREFERRED_PROVIDER_virtual/bootloader set to u-boot-ti, not u-boot")
        );
        assert_eq!(virtual_provider_skip("virtual/x", Some("a"), "a"), None);
        assert_eq!(virtual_provider_skip("virtual/x", Some(""), "a"), None);
        assert_eq!(virtual_provider_skip("virtual/x", None, "a"), None);
    }
}
