//! Evaluator gaps closed in round w12: the `PROVIDES` filters OE-core's `nativesdk` and
//! multilib classes install (`oe.classextend.suffix_filter`), computed where their inputs are
//! static, so those variants are found as providers through their rewritten `PROVIDES`; and
//! the `features_check` skip check deciding on words whose presence is certain only. Each
//! test runs on a temporary copy of the fixture workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    #[allow(dead_code)]
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _cache: cache,
        _tmp: tmp,
        root,
        ws,
        idx,
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append_to(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

fn providers_of(fx: &Fx, name: &str) -> sous_chef::eval::providers::ProviderSet {
    sous_chef::eval::providers::Providers::new(&fx.idx, &fx.ws, None, None)
        .unwrap()
        .providers(name)
        .unwrap()
}

fn provides_notes(ds: &DataStore) -> Vec<String> {
    ds.replay()
        .var_notes
        .get("PROVIDES")
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// Item 2: the nativesdk and multilib PROVIDES filters
// ---------------------------------------------------------------------------------------

/// OE-core Wrynose `classes-recipe/nativesdk.bbclass`, its anonymous python as written there
/// (the handler is a stand-in: the replay reproduces it by name).
const NATIVESDK_ANON: &str = r#"python () {
    pn = d.getVar("PN")
    if not pn.startswith("nativesdk-"):
        return

    import oe.classextend

    clsextend = oe.classextend.ClassExtender("nativesdk", [], d)
    clsextend.rename_package_variables((d.getVar("PACKAGEVARS") or "").split())

    clsextend.set_filter("DEPENDS", deps=True)
    clsextend.set_filter("PACKAGE_WRITE_DEPS", deps=False)
    clsextend.set_filter("PROVIDES", deps=False)

    if "nativesdk" in (d.getVar("BBCLASSEXTEND") or ""):
        clsextend.map_packagevars()

    d.setVar("LIBCEXTENSION", "")
    d.setVar("ABIEXTENSION", "")
}
"#;

/// OE-core Wrynose `classes/multilib.bbclass` `multilib_virtclass_handler_postkeyexp` as
/// written there (up to its `PROVIDES` filter and the `cross-canadian` return).
const MULTILIB_POSTKEYEXP: &str = r#"python multilib_virtclass_handler_postkeyexp () {
    cls = d.getVar("BBEXTENDCURR")
    variant = d.getVar("BBEXTENDVARIANT")
    if cls != "multilib" or not variant:
        return

    variant = d.getVar("BBEXTENDVARIANT")

    import oe.classextend

    if bb.data.inherits_class('image', d):
        return

    prefixes = (d.getVar("MULTILIB_VARIANTS") or "").split()
    clsextend = oe.classextend.ClassExtender(variant, prefixes, d)

    clsextend.set_filter("DEPENDS", deps=True)
    clsextend.set_filter("PACKAGE_WRITE_DEPS", deps=False)

    clsextend.set_filter("PROVIDES", deps=False)

    if bb.data.inherits_class('cross-canadian', d):
        return

    clsextend.rename_package_variables((d.getVar("PACKAGEVARS") or "").split())

    clsextend.map_packagevars()
}
addhandler multilib_virtclass_handler_postkeyexp
multilib_virtclass_handler_postkeyexp[eventmask] = "bb.event.RecipePostKeyExpansion"
"#;

/// The two classes, libfoo extended to `native nativesdk`, and the configuration's
/// `BBCLASSEXTEND:append = " multilib:lib32"` (as OE-core's `conf/multilib.conf` appends
/// `${MULTILIBS}`), with `multilib_variants` as `MULTILIB_VARIANTS`.
fn sdk_and_multilib(root: &Path, multilib_variants: &str, provides: &str) {
    write(
        root,
        "meta-core/classes-recipe/nativesdk.bbclass",
        &format!(
            "CLASSOVERRIDE = \"class-nativesdk\"\nMACHINEOVERRIDES = \"\"\n\
             python nativesdk_virtclass_handler () {{\n    pn = e.data.getVar(\"PN\")\n}}\n\
             addhandler nativesdk_virtclass_handler\n\
             nativesdk_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n\
             {NATIVESDK_ANON}"
        ),
    );
    write(
        root,
        "meta-core/classes/multilib.bbclass",
        &format!(
            "python multilib_virtclass_handler () {{\n    cls = d.getVar(\"BBEXTENDCURR\")\n}}\n\
             addhandler multilib_virtclass_handler\n\
             multilib_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n\
             {MULTILIB_POSTKEYEXP}"
        ),
    );
    append_to(
        root,
        "meta-core/conf/bitbake.conf",
        &format!(
            "NON_MULTILIB_RECIPES = \"\"\nMULTILIB_VARIANTS = \"{multilib_variants}\"\n\
             BBCLASSEXTEND:append = \" multilib:lib32\"\n"
        ),
    );
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        &format!("BBCLASSEXTEND = \"nativesdk\"\nPROVIDES += \"{provides}\"\n"),
    );
}

#[test]
fn the_nativesdk_variant_provides_what_suffix_filter_makes_of_provides() {
    let fx = modified(|r| sdk_and_multilib(r, "lib32", "libfoo-cross-tools"));
    // suffix_filter(val, 'nativesdk', []): virtual/foo -> virtual/nativesdk-foo; a word
    // holding -cross- stays.
    let ds = eval(&fx, "nativesdk-libfoo");
    let p = ds.value("PROVIDES").unwrap();
    assert_eq!(p.text, "virtual/nativesdk-foo libfoo-cross-tools");
    assert!(!p.unevaluated_python, "{p:?}");
    assert!(
        provides_notes(&ds)
            .iter()
            .any(|n| n.contains("nativesdk.bbclass") && n.contains("suffix_filter")),
        "{:?}",
        provides_notes(&ds)
    );
    // The anonymous python's other filters stay flagged.
    assert!(ds.value("DEPENDS").unwrap().unevaluated_python);
    // The base recipe is not filtered (its PN does not start with nativesdk-).
    let base = eval(&fx, "libfoo");
    let p = base.value("PROVIDES").unwrap();
    assert_eq!(p.text, "virtual/foo libfoo-cross-tools");
    assert!(!p.unevaluated_python);

    let set = providers_of(&fx, "virtual/nativesdk-foo");
    let sel = set.selected().expect("a provider");
    assert_eq!(sel.pn, "nativesdk-libfoo");
    assert_eq!(
        sel.selection.variant.as_ref().map(|v| v.word.as_str()),
        Some("nativesdk")
    );
    assert!(!sel.python, "{sel:?}");
    assert!(
        set.notes.iter().any(|n| n
            .contains("provides virtual/nativesdk-foo through the PROVIDES its class rewrites")),
        "{:?}",
        set.notes
    );
}

#[test]
fn a_multilib_variant_provides_what_suffix_filter_makes_of_provides() {
    let fx = modified(|r| sdk_and_multilib(r, "lib32", "libfoo-extra"));
    let ds = eval(&fx, "lib32-libfoo");
    let p = ds.value("PROVIDES").unwrap();
    // suffix_filter(val, 'lib32', ['lib32']).
    assert_eq!(p.text, "virtual/lib32-foo lib32-libfoo-extra");
    assert!(!p.unevaluated_python, "{p:?}");
    assert!(
        provides_notes(&ds)
            .iter()
            .any(|n| n.contains("multilib_virtclass_handler_postkeyexp")
                && n.contains("suffix_filter")),
        "{:?}",
        provides_notes(&ds)
    );
    // The handler's other filters stay flagged.
    assert!(ds.value("DEPENDS").unwrap().unevaluated_python);

    // Found through its rewritten PROVIDES, beside the target recipe's own name.
    for name in ["virtual/lib32-foo", "lib32-libfoo-extra"] {
        let set = providers_of(&fx, name);
        let sel = set.selected().expect("a provider");
        assert_eq!(sel.pn, "lib32-libfoo", "{name}");
        assert_eq!(
            sel.selection.variant.as_ref().map(|v| v.word.as_str()),
            Some("multilib:lib32")
        );
        assert!(!sel.python, "{sel:?}");
    }
    // The base recipe still provides virtual/foo; the variant does not.
    let set = providers_of(&fx, "virtual/foo");
    assert!(set.providers.iter().all(|p| p.selection.variant.is_none()));
}

#[test]
fn multilib_prefixes_from_python_leave_the_words_they_decide_flagged() {
    // OE-core computes MULTILIB_VARIANTS with python (`extend_variants`): a word with a `-`
    // that add_suffix decides through the prefixes is not known.
    let python = "${@oe.utils.read_file('variants') or 'lib32'}";
    let fx = modified(|r| sdk_and_multilib(r, python, "libfoo-extra"));
    let ds = eval(&fx, "lib32-libfoo");
    let p = ds.value("PROVIDES").unwrap();
    assert!(p.unevaluated_python, "{p:?}");
    assert!(
        provides_notes(&ds)
            .iter()
            .any(|n| n.contains("MULTILIB_VARIANTS") && n.contains("libfoo-extra")),
        "{:?}",
        provides_notes(&ds)
    );
    let set = providers_of(&fx, "virtual/lib32-foo");
    let sel = set.selected().expect("a candidate");
    assert_eq!(sel.pn, "lib32-libfoo");
    assert!(sel.python, "{sel:?}");

    // Words the prefixes cannot touch (no `-`, or decided before them) are computed.
    let fx = modified(|r| sdk_and_multilib(r, python, "libfoo-cross-tools"));
    let ds = eval(&fx, "lib32-libfoo");
    let p = ds.value("PROVIDES").unwrap();
    assert_eq!(p.text, "virtual/lib32-foo libfoo-cross-tools");
    assert!(!p.unevaluated_python, "{p:?}");
}

#[test]
fn suffix_filter_follows_classextend() {
    use sous_chef::eval::variant::{multilib_provides_forms, suffix_filter};
    assert_eq!(suffix_filter("", "nativesdk", Some(&[])), None);
    assert_eq!(
        suffix_filter(
            "virtual/libc zlib nativesdk-x x-native gcc-cross-arm virtual/kernel kernel-module-y",
            "nativesdk",
            Some(&[])
        ),
        Some(Ok(
            "virtual/nativesdk-libc nativesdk-zlib nativesdk-x x-native gcc-cross-arm virtual/kernel kernel-module-y"
                .to_string()
        ))
    );
    // A word starting with another variant's prefix passes through.
    assert_eq!(
        suffix_filter("lib64-a b", "lib32", Some(&["lib32", "lib64"])),
        Some(Ok("lib64-a lib32-b".to_string()))
    );
    assert_eq!(
        suffix_filter("lib64-a b", "lib32", None),
        Some(Err("lib64-a".to_string()))
    );
    let forms = multilib_provides_forms("virtual/lib32-foo", &["lib32"]);
    assert!(
        forms
            .iter()
            .any(|(b, e)| b == "virtual/foo" && e.word == "multilib:lib32")
    );
    let forms = multilib_provides_forms("lib32-x", &["lib32"]);
    assert!(
        forms
            .iter()
            .any(|(b, e)| b == "x" && e.word == "multilib:lib32")
    );
    assert!(multilib_provides_forms("lib32-x", &[]).is_empty());
}

// ---------------------------------------------------------------------------------------
// Item 3: features_check decides on certain words only
// ---------------------------------------------------------------------------------------

/// OE-core Wrynose `classes-recipe/features_check.bbclass`, its anonymous python verbatim
/// (as in tests/eval_w11.rs).
const FEATURES_CHECK: &str = r#"python () {
    if bb.utils.to_boolean(d.getVar('PARSE_ALL_RECIPES', False)):
        return

    unused = True

    for kind in ['DISTRO', 'MACHINE', 'COMBINED', 'IMAGE', 'TUNE']:
        if d.getVar('ANY_OF_' + kind + '_FEATURES') is None and not d.hasOverrides('ANY_OF_' + kind + '_FEATURES') and \
           d.getVar('REQUIRED_' + kind + '_FEATURES') is None and not d.hasOverrides('REQUIRED_' + kind + '_FEATURES') and \
           d.getVar('CONFLICT_' + kind + '_FEATURES') is None and not d.hasOverrides('CONFLICT_' + kind + '_FEATURES'):
            continue

        unused = False

        # Assume at least one var is set.
        features = set((d.getVar(kind + '_FEATURES') or '').split())

        any_of_features = set((d.getVar('ANY_OF_' + kind + '_FEATURES') or '').split())
        if any_of_features:
            if set.isdisjoint(any_of_features, features):
                raise bb.parse.SkipRecipe("one of '%s' needs to be in %s_FEATURES"
                    % (' '.join(any_of_features), kind))

        required_features = set((d.getVar('REQUIRED_' + kind + '_FEATURES') or '').split())
        if required_features:
            missing = set.difference(required_features, features)
            if missing:
                if kind == 'DISTRO':
                    raise bb.parse.SkipRecipe("using %s '%s', which is missing required %s_FEATURES: '%s'"
                        % (kind, d.getVar(kind), kind, ' '.join(missing)))
                else:
                    raise bb.parse.SkipRecipe("missing required %s feature%s '%s' (not in %s_FEATURES)"
                        % (kind.lower(), 's' if len(missing) > 1 else '', ' '.join(missing), kind))

        conflict_features = set((d.getVar('CONFLICT_' + kind + '_FEATURES') or '').split())
        if conflict_features:
            conflicts = set.intersection(conflict_features, features)
            if conflicts:
                raise bb.parse.SkipRecipe("conflicting %s feature%s '%s' (in %s_FEATURES)"
                    % (kind.lower(), 's' if len(conflicts) > 1 else '', ' '.join(conflicts), kind))

    if unused:
        bb.warn("Recipe inherits features_check but doesn't use it")
}
"#;

fn recipe(root: &Path, pn: &str, body: &str) {
    write(
        root,
        &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
        &format!("LICENSE = \"CLOSED\"\ninherit features_check\n{body}"),
    );
}

fn skip_notes(fx: &Fx, recipe: &str) -> Vec<String> {
    eval(fx, recipe)
        .replay()
        .target_notes
        .iter()
        .filter(|n| n.contains("skip this recipe"))
        .cloned()
        .collect()
}

/// `OVERRIDES` holds python (through `DISTROOVERRIDES`), so an override not written in it
/// may still be active: `DISTRO_FEATURES:remove:maybe` may take `x11` out, and with
/// `replace` a `DISTRO_FEATURES:maybe` variant may replace the whole value. Without them
/// the value is the same, and certain apart from the override decisions.
fn uncertain_features(root: &Path, extra: &str) {
    write(
        root,
        "meta-core/classes-recipe/features_check.bbclass",
        FEATURES_CHECK,
    );
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        &format!(
            "DISTROOVERRIDES = \"testdistro:${{@'maybe' if oe.utils.read_file('x') else 'other'}}\"\n\
             DISTRO_FEATURES = \"acl ipv4 x11\"\n\
             DISTRO_FEATURES:append:testdistro = \" wayland\"\n{extra}"
        ),
    );
    recipe(root, "nox11", "CONFLICT_DISTRO_FEATURES = \"x11\"\n");
    recipe(root, "needsx11", "REQUIRED_DISTRO_FEATURES = \"x11\"\n");
    recipe(
        root,
        "anyx11",
        "ANY_OF_DISTRO_FEATURES = \"x11 directfb\"\n",
    );
    recipe(
        root,
        "needsacl",
        "REQUIRED_DISTRO_FEATURES = \"acl wayland\"\n",
    );
    recipe(root, "noopengl", "CONFLICT_DISTRO_FEATURES = \"opengl\"\n");
    recipe(root, "noacl", "CONFLICT_DISTRO_FEATURES = \"acl\"\n");
}

#[test]
fn a_word_an_uncertain_remove_may_take_out_is_only_possibly_present() {
    let fx = modified(|r| uncertain_features(r, "DISTRO_FEATURES:remove:maybe = \"x11\"\n"));
    let ds = eval(&fx, "nox11");
    let v = ds.value("DISTRO_FEATURES").unwrap();
    assert!(
        v.unevaluated_python,
        "flagged for the override decision: {v:?}"
    );
    // w11: "would skip" (x11 is a known word). It may be removed: may skip.
    let n = skip_notes(&fx, "nox11");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].starts_with(
            "note: BitBake may skip this recipe: features_check: CONFLICT_DISTRO_FEATURES = 'x11'"
        ) && n[0].contains("'x11' only possibly in DISTRO_FEATURES"),
        "{n:?}"
    );
    // w11: silence (passes on the known word). Now may skip, naming it.
    for r in ["needsx11", "anyx11"] {
        let n = skip_notes(&fx, r);
        assert_eq!(n.len(), 1, "{r}: {n:?}");
        assert!(
            n[0].starts_with("note: BitBake may skip this recipe: features_check:")
                && n[0].contains("'x11' only possibly in DISTRO_FEATURES"),
            "{r}: {n:?}"
        );
    }
    // Words no uncertain operation touches are certain: acl from the plain assignment,
    // wayland from an append whose override is written in OVERRIDES.
    assert!(skip_notes(&fx, "needsacl").is_empty());
    assert_eq!(
        skip_notes(&fx, "noacl"),
        [
            "note: BitBake would skip this recipe: conflicting distro feature 'acl' (in DISTRO_FEATURES) (features_check.bbclass; decided from static values)"
        ]
    );
}

#[test]
fn a_variant_that_may_replace_the_value_leaves_no_word_certain() {
    let fx = modified(|r| uncertain_features(r, "DISTRO_FEATURES:maybe = \"systemd\"\n"));
    // Every word may be replaced: no pass, no "would skip" on any of them.
    for r in ["needsacl", "noacl", "nox11"] {
        let n = skip_notes(&fx, r);
        assert_eq!(n.len(), 1, "{r}: {n:?}");
        assert!(
            n[0].starts_with("note: BitBake may skip this recipe: features_check:")
                && n[0].contains(
                    "only possibly in DISTRO_FEATURES (an override decision on it depends on OVERRIDES, which is not known statically)"
                ),
            "{r}: {n:?}"
        );
    }
}

#[test]
fn without_uncertain_operations_the_known_words_still_decide() {
    // The same value with only an uncertain :append (which can add, never take out): the
    // known words are certain, as before.
    let fx = modified(|r| uncertain_features(r, "DISTRO_FEATURES:append:maybe = \" opengl\"\n"));
    let v = eval(&fx, "nox11").value("DISTRO_FEATURES").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
    assert_eq!(
        skip_notes(&fx, "nox11"),
        [
            "note: BitBake would skip this recipe: conflicting distro feature 'x11' (in DISTRO_FEATURES) (features_check.bbclass; decided from static values)"
        ]
    );
    assert!(skip_notes(&fx, "needsx11").is_empty());
    assert!(skip_notes(&fx, "needsacl").is_empty());
    // opengl may be appended: the conflict is undecided (as in w11).
    let n = skip_notes(&fx, "noopengl");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].contains("may skip"), "{n:?}");
}
