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
    let python = "${@' '.join(['lib32'])}";
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
