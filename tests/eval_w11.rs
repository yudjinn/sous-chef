//! Evaluator gaps closed in round w11: the skips of OE-core's `features_check.bbclass` and
//! of `base_eventhandler` on `RecipeParsed` in the skip notes; class-extended variants found
//! through the `PROVIDES` their class rewrites; the Scarthgap point-release note. Each test
//! runs on a temporary copy of the fixture workspace; the fixture itself is not changed.
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

fn index(tmp: tempfile::TempDir, root: PathBuf, ws: Workspace) -> Fx {
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

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    index(tmp, root, ws)
}

/// [`modified`] on a given release.
fn modified_release(codename: &str, edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root, _config, ws) = temp_ws_release(codename, edit);
    index(tmp, root, ws)
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

fn try_eval(fx: &Fx, recipe: &str) -> sous_chef::error::Result<DataStore> {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t)
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    try_eval(fx, recipe).unwrap()
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

fn recipe(root: &Path, pn: &str, body: &str) {
    write(
        root,
        &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
        body,
    );
}

// ---------------------------------------------------------------------------------------
// Item 2: features_check.bbclass and base_eventhandler on RecipeParsed
// ---------------------------------------------------------------------------------------

/// OE-core Wrynose `classes-recipe/features_check.bbclass`, its anonymous python verbatim.
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

fn features_check(root: &Path) {
    write(
        root,
        "meta-core/classes-recipe/features_check.bbclass",
        FEATURES_CHECK,
    );
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "DISTRO_FEATURES = \"acl ipv4 ipv6 usbhost\"\n\
         MACHINE_FEATURES = \"usbhost ${@'wifi' if d.getVar('X') else ''}\"\n",
    );
    recipe(
        root,
        "needsd",
        "LICENSE = \"CLOSED\"\ninherit features_check\nREQUIRED_DISTRO_FEATURES = \"systemd\"\n",
    );
    recipe(
        root,
        "hasacl",
        "LICENSE = \"CLOSED\"\ninherit features_check\nREQUIRED_DISTRO_FEATURES = \"acl ipv4\"\nCONFLICT_DISTRO_FEATURES = \"x11\"\nANY_OF_DISTRO_FEATURES = \"ipv6 wayland\"\n",
    );
    recipe(
        root,
        "conflicts",
        "LICENSE = \"CLOSED\"\ninherit features_check\nCONFLICT_DISTRO_FEATURES = \"x11 acl\"\n",
    );
    recipe(
        root,
        "anyof",
        "LICENSE = \"CLOSED\"\ninherit features_check\nANY_OF_DISTRO_FEATURES = \"x11 wayland\"\n",
    );
    recipe(
        root,
        "wifi",
        "LICENSE = \"CLOSED\"\ninherit features_check\nREQUIRED_MACHINE_FEATURES = \"wifi\"\n",
    );
    recipe(
        root,
        "both",
        "LICENSE = \"CLOSED\"\ninherit features_check\nREQUIRED_MACHINE_FEATURES = \"wifi\"\nCONFLICT_MACHINE_FEATURES = \"usbhost\"\n",
    );
    recipe(
        root,
        "usb",
        "LICENSE = \"CLOSED\"\ninherit features_check\nREQUIRED_MACHINE_FEATURES = \"usbhost\"\n",
    );
    recipe(
        root,
        "noclass",
        "LICENSE = \"CLOSED\"\nREQUIRED_DISTRO_FEATURES = \"systemd\"\n",
    );
    recipe(
        root,
        "parseall",
        "LICENSE = \"CLOSED\"\ninherit features_check\nREQUIRED_DISTRO_FEATURES = \"systemd\"\nPARSE_ALL_RECIPES = \"1\"\n",
    );
}

#[test]
fn features_check_would_skip_on_known_features() {
    let fx = modified(features_check);
    // DISTRO_FEATURES = "acl ipv4 ipv6 usbhost" (testdistro.conf): systemd is missing.
    assert_eq!(
        skip_notes(&fx, "needsd"),
        [
            "note: BitBake would skip this recipe: using DISTRO 'testdistro', which is missing required DISTRO_FEATURES: 'systemd' (features_check.bbclass; decided from static values)"
        ]
    );
    assert_eq!(
        skip_notes(&fx, "conflicts"),
        [
            "note: BitBake would skip this recipe: conflicting distro feature 'acl' (in DISTRO_FEATURES) (features_check.bbclass; decided from static values)"
        ]
    );
    assert_eq!(
        skip_notes(&fx, "anyof"),
        [
            "note: BitBake would skip this recipe: one of 'wayland x11' needs to be in DISTRO_FEATURES (features_check.bbclass; decided from static values)"
        ]
    );
    // Every check passes.
    assert!(skip_notes(&fx, "hasacl").is_empty());
    // Not inheriting the class: no check. PARSE_ALL_RECIPES true: no check.
    assert!(skip_notes(&fx, "noclass").is_empty());
    assert!(skip_notes(&fx, "parseall").is_empty());
}

#[test]
fn features_check_may_skip_when_the_features_hold_python() {
    let fx = modified(features_check);
    // MACHINE_FEATURES = "usbhost ${@...}": wifi is not among the known words.
    let n = skip_notes(&fx, "wifi");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].starts_with(
            "note: BitBake may skip this recipe: features_check: REQUIRED_MACHINE_FEATURES = 'wifi'; 'wifi' not among the known words of MACHINE_FEATURES, and MACHINE_FEATURES is not known statically (it holds python)"
        ),
        "{n:?}"
    );
    // usbhost is a known word: passes, whatever the python adds.
    assert!(skip_notes(&fx, "usb").is_empty());
    // A certain conflict after an undecided requirement: skipped for sure, but BitBake may
    // name the requirement first.
    let n = skip_notes(&fx, "both");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].starts_with(
            "note: BitBake would skip this recipe: conflicting machine feature 'usbhost' (in MACHINE_FEATURES) (features_check.bbclass; decided from static values); BitBake may give an earlier reason instead (features_check: REQUIRED_MACHINE_FEATURES = 'wifi';"
        ),
        "{n:?}"
    );
}

/// The fixture's `base_eventhandler` with OE-core Wrynose's `RecipeParsed` branch.
fn recipe_parsed_handler(root: &Path) {
    let p = root.join("meta-core/classes/base.bbclass");
    let s = fs::read_to_string(&p).unwrap().replace(
        "base_eventhandler[eventmask] = \"bb.event.ConfigParsed\"\npython base_eventhandler() {\n",
        "base_eventhandler[eventmask] = \"bb.event.ConfigParsed bb.event.RecipeParsed\"\npython base_eventhandler() {\n\
    if isinstance(e, bb.event.RecipeParsed):\n\
        pn = d.getVar('PN')\n\
        source_mirror_fetch = bb.utils.to_boolean(d.getVar('SOURCE_MIRROR_FETCH', False))\n\
        if not source_mirror_fetch:\n\
            provs = (d.getVar(\"PROVIDES\") or \"\").split()\n\
            multiprovidersallowed = (d.getVar(\"BB_MULTI_PROVIDER_ALLOWED\") or \"\").split()\n\
            for p in provs:\n\
                if p.startswith(\"virtual/\") and p not in multiprovidersallowed:\n\
                    profprov = d.getVar(\"PREFERRED_PROVIDER_\" + p)\n\
                    if profprov and pn != profprov:\n\
                        raise bb.parse.SkipRecipe(\"PREFERRED_PROVIDER_%s set to %s, not %s\" % (p, profprov, pn))\n",
    );
    assert!(s.contains("bb.event.RecipeParsed"));
    fs::write(p, s).unwrap();
}

#[test]
fn a_provider_of_a_virtual_another_recipe_is_preferred_for_would_be_skipped() {
    let fx = modified(|r| {
        recipe_parsed_handler(r);
        recipe(
            r,
            "pyprov",
            "LICENSE = \"CLOSED\"\nPROVIDES = \"virtual/py ${@'virtual/foo' if d.getVar('Z') else ''}\"\n",
        );
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "PREFERRED_PROVIDER_virtual/py = \"other\"\n",
        );
    });
    // testdistro.conf: PREFERRED_PROVIDER_virtual/foo ?= "libfoo-alt"; libfoo provides
    // virtual/foo (the fixture), libfoo-alt is preferred.
    assert_eq!(
        skip_notes(&fx, "libfoo"),
        [
            "note: BitBake would skip this recipe: PREFERRED_PROVIDER_virtual/foo set to libfoo-alt, not libfoo (base_eventhandler on RecipeParsed; decided from static values)"
        ]
    );
    assert!(skip_notes(&fx, "libfoo-alt").is_empty());
    // Known virtual/ word and a python word that may be virtual: the known one decides.
    let n = skip_notes(&fx, "pyprov");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].contains(
            "would skip this recipe: PREFERRED_PROVIDER_virtual/py set to other, not pyprov"
        ),
        "{n:?}"
    );
}

#[test]
fn the_virtual_provider_skip_is_undecided_on_unknown_values_and_off_when_allowed() {
    let fx = modified(|r| {
        recipe_parsed_handler(r);
        recipe(
            r,
            "pyonly",
            "LICENSE = \"CLOSED\"\nPROVIDES = \"${@'virtual/foo' if d.getVar('Z') else ''}\"\n",
        );
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "BB_MULTI_PROVIDER_ALLOWED = \"virtual/foo\"\n",
        );
    });
    // BB_MULTI_PROVIDER_ALLOWED lists virtual/foo: no skip.
    assert!(skip_notes(&fx, "libfoo").is_empty());
    let fx = modified(|r| {
        recipe_parsed_handler(r);
        recipe(
            r,
            "pyonly",
            "LICENSE = \"CLOSED\"\nPROVIDES = \"${@'virtual/foo' if d.getVar('Z') else ''}\"\n",
        );
    });
    let n = skip_notes(&fx, "pyonly");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].starts_with("note: BitBake may skip this recipe: PROVIDES is not known statically"),
        "{n:?}"
    );
    // The fixture without the RecipeParsed branch: nothing (no handler, no check).
    let plain = modified(|_| {});
    assert!(skip_notes(&plain, "libfoo").is_empty());
}

#[test]
fn the_pure_checks_are_public() {
    assert_eq!(
        eval::skips::features_skip("DISTRO", "kiss", "", "systemd", "", "acl").as_deref(),
        Some("using DISTRO 'kiss', which is missing required DISTRO_FEATURES: 'systemd'")
    );
    assert_eq!(
        eval::skips::virtual_provider_skip("virtual/libc", Some("glibc"), "musl").as_deref(),
        Some("PREFERRED_PROVIDER_virtual/libc set to glibc, not musl")
    );
}

// ---------------------------------------------------------------------------------------
// Item 3: variants found through the PROVIDES their class rewrites
// ---------------------------------------------------------------------------------------

/// Stand-ins for OE-core's `native` and `nativesdk` classes (handlers by name and event
/// mask; their bodies are never run), and `BBCLASSEXTEND` on libfoo (which provides
/// `virtual/foo`).
fn class_extension(root: &Path) {
    write(
        root,
        "meta-core/classes-recipe/native.bbclass",
        "CLASSOVERRIDE = \"class-native\"\nMACHINEOVERRIDES = \"\"\n\
         python native_virtclass_handler () {\n    pn = d.getVar(\"PN\")\n}\n\
         addhandler native_virtclass_handler\n\
         native_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n",
    );
    write(
        root,
        "meta-core/classes-recipe/nativesdk.bbclass",
        "CLASSOVERRIDE = \"class-nativesdk\"\nMACHINEOVERRIDES = \"\"\n\
         python nativesdk_virtclass_handler () {\n    pn = e.data.getVar(\"PN\")\n}\n\
         addhandler nativesdk_virtclass_handler\n\
         nativesdk_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n\
         python () {\n    clsextend = oe.classextend.ClassExtender(\"nativesdk\", [], d)\n    clsextend.set_filter(\"PROVIDES\", deps=False)\n}\n",
    );
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "BBCLASSEXTEND = \"native nativesdk\"\nPROVIDES += \"libfoo-cross-tools\"\n",
    );
}

fn providers_of(fx: &Fx, name: &str) -> sous_chef::eval::providers::ProviderSet {
    sous_chef::eval::providers::Providers::new(&fx.idx, &fx.ws, None, None)
        .unwrap()
        .providers(name)
        .unwrap()
}

#[test]
fn the_native_variant_provides_what_native_filter_makes_of_provides() {
    let fx = modified(class_extension);
    // native_filter on "virtual/foo libfoo-cross-tools": virtual/foo-native, and -cross-
    // becomes -native.
    let ds = eval(&fx, "libfoo-native");
    let p = ds.value("PROVIDES").unwrap();
    assert_eq!(p.text, "virtual/foo-native libfoo-native-tools");
    assert!(!p.unevaluated_python, "{p:?}");
    assert!(
        ds.replay().var_notes["PROVIDES"]
            .iter()
            .any(|n| n.contains("native_filter")),
        "{:?}",
        ds.replay().var_notes
    );
    // The base recipe keeps its own PROVIDES.
    assert_eq!(
        eval(&fx, "libfoo").value("PROVIDES").unwrap().text,
        "virtual/foo libfoo-cross-tools"
    );

    let set = providers_of(&fx, "virtual/foo-native");
    let sel = set.selected().expect("a provider");
    assert_eq!(sel.pn, "libfoo-native");
    assert_eq!(sel.selection.recipe.pn, "libfoo");
    assert_eq!(
        sel.selection.variant.as_ref().map(|v| v.word.as_str()),
        Some("native")
    );
    assert!(!sel.python);
    assert!(
        set.notes
            .iter()
            .any(|n| n
                .contains("provides virtual/foo-native through the PROVIDES its class rewrites")),
        "{:?}",
        set.notes
    );
    let set = providers_of(&fx, "libfoo-native-tools");
    assert_eq!(set.selected().map(|p| p.pn.as_str()), Some("libfoo-native"));
    // The target recipes still provide virtual/foo; the variant does not.
    let set = providers_of(&fx, "virtual/foo");
    assert!(set.providers.iter().all(|p| p.selection.variant.is_none()));
}

#[test]
fn the_nativesdk_variant_is_an_uncertain_provider_its_python_decides() {
    let fx = modified(class_extension);
    let set = providers_of(&fx, "virtual/nativesdk-foo");
    let sel = set.selected().expect("a candidate");
    assert_eq!(sel.pn, "nativesdk-libfoo");
    assert!(sel.python, "{sel:?}");
    assert!(
        set.notes
            .iter()
            .any(|n| n.contains("goes through python that was not run")),
        "{:?}",
        set.notes
    );
}

#[test]
fn a_real_recipe_of_the_same_name_does_not_hide_the_variant() {
    let fx = modified(|r| {
        class_extension(r);
        // A real libfoo-native 1.0 in meta-vendor (priority 6) beside the variant made
        // from meta-core's libfoo 2.0 (priority 5).
        write(
            r,
            "meta-vendor/recipes-vendor/libfoo-native/libfoo-native_1.0.bb",
            "LICENSE = \"CLOSED\"\n",
        );
    });
    let p = sous_chef::eval::providers::Providers::new(&fx.idx, &fx.ws, None, None).unwrap();
    // Both are files of the PN libfoo-native; the higher layer priority wins.
    let bv = p.best_version("libfoo-native", None).unwrap().unwrap();
    assert_eq!(bv.recipe.pn, "libfoo-native", "{bv:?}");
    assert!(bv.variant.is_none());
    // The variant is one of the files: asking for its version finds it.
    let v2 = p
        .best_version("libfoo-native", Some("2.0"))
        .unwrap()
        .unwrap();
    assert_eq!(v2.recipe.pn, "libfoo", "{v2:?}");
    assert!(v2.variant.is_some());
    // PREFERRED_VERSION of the variant's PN picks it, as findBestProvider does.
    let fx = modified(|r| {
        class_extension(r);
        write(
            r,
            "meta-vendor/recipes-vendor/libfoo-native/libfoo-native_1.0.bb",
            "LICENSE = \"CLOSED\"\n",
        );
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "PREFERRED_VERSION_libfoo-native = \"2.%\"\n",
        );
    });
    let p = sous_chef::eval::providers::Providers::new(&fx.idx, &fx.ws, None, None).unwrap();
    let bv = p.best_version("libfoo-native", None).unwrap().unwrap();
    assert!(bv.variant.is_some(), "{bv:?}");
    let set = p.providers("libfoo-native").unwrap();
    assert_eq!(set.providers.len(), 1, "{set:?}");
    assert!(
        set.selected().unwrap().selection.variant.is_some(),
        "{set:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Item 4: when a variant's class is inherited on Scarthgap
// ---------------------------------------------------------------------------------------

/// `inherit_defer ${DEFCLASS}` whose value depends on the PN it is expanded for: the
/// variant's (`pn-libfoo-native`) picks `defnative`, the base recipe's `deftarget`.
fn deferred_by_pn(root: &Path) {
    class_extension(root);
    write(
        root,
        "meta-core/classes-recipe/deftarget.bbclass",
        "PICK = \"target\"\n",
    );
    write(
        root,
        "meta-core/classes-recipe/defnative.bbclass",
        "PICK = \"native\"\n",
    );
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "DEFCLASS = \"deftarget\"\nDEFCLASS:pn-libfoo-native = \"defnative\"\ninherit_defer ${DEFCLASS}\n",
    );
}

/// A BitBake checkout next to the OE-core layer (`<root>/bitbake`, OE-core's
/// `$OEROOT/bitbake`): `__version__` and, when given, the `multi_finalize` line of
/// `ast.py`.
fn bitbake_checkout(root: &Path, version: &str, ast_line: Option<&str>) {
    write(
        root,
        "bitbake/lib/bb/__init__.py",
        &format!("__version__ = \"{version}\"\n"),
    );
    if let Some(l) = ast_line {
        write(
            root,
            "bitbake/lib/bb/parse/ast.py",
            &format!("def multi_finalize(fn, d):\n    {l}\n"),
        );
    }
}

fn order_notes(ds: &DataStore) -> Vec<String> {
    ds.replay()
        .target_notes
        .iter()
        .filter(|n| n.contains("order of `native`"))
        .cloned()
        .collect()
}

#[test]
fn scarthgap_without_bitbake_follows_5_0_and_says_5_0_11_differs() {
    let fx = modified_release("scarthgap", deferred_by_pn);
    let ds = eval(&fx, "libfoo-native");
    // 5.0 to 5.0.10: the deferred inherits ran on the base recipe.
    assert_eq!(ds.value("PICK").unwrap().text, "target");
    let n = order_notes(&ds);
    assert_eq!(n.len(), 1, "{:?}", ds.replay().target_notes);
    assert!(
        n[0].contains("Scarthgap 5.0.11 and later")
            && n[0].contains("no BitBake checkout was found"),
        "{n:?}"
    );
    // The base recipe gets no such note.
    assert!(order_notes(&eval(&fx, "libfoo")).is_empty());
}

#[test]
fn scarthgap_with_bitbake_2_8_1_or_the_new_ast_defers_per_variant() {
    for (version, line) in [
        ("2.8.1", None),
        ("2.8.0", Some("inherits.append((extendedmap[name], fn, 0))")),
    ] {
        let fx = modified_release("scarthgap", |r| {
            deferred_by_pn(r);
            bitbake_checkout(r, version, line);
        });
        let ds = eval(&fx, "libfoo-native");
        // Deferred per variant: `${DEFCLASS}` expanded with the variant's PN.
        assert_eq!(ds.value("PICK").unwrap().text, "native", "{version}");
        assert!(
            order_notes(&ds).is_empty(),
            "{:?}",
            ds.replay().target_notes
        );
    }
}

#[test]
fn scarthgap_with_the_old_ast_or_an_unreadable_2_8_0() {
    let fx = modified_release("scarthgap", |r| {
        deferred_by_pn(r);
        bitbake_checkout(
            r,
            "2.8.0",
            Some("bb.parse.BBHandler.inherit(extendedmap[name], fn, 0, d)"),
        );
    });
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(ds.value("PICK").unwrap().text, "target");
    assert!(
        order_notes(&ds).is_empty(),
        "{:?}",
        ds.replay().target_notes
    );
    // 2.8.0 and no ast.py: 5.0 to 5.0.12 all say 2.8.0, so not known; noted.
    let fx = modified_release("scarthgap", |r| {
        deferred_by_pn(r);
        bitbake_checkout(r, "2.8.0", None);
    });
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(ds.value("PICK").unwrap().text, "target");
    let n = order_notes(&ds);
    assert!(
        n.len() == 1 && n[0].contains("is 2.8.0, which Scarthgap 5.0 to 5.0.12 all report"),
        "{:?}",
        ds.replay().target_notes
    );
}

#[test]
fn styhead_walnascar_and_wrynose_get_no_order_note() {
    for rel in ["styhead", "walnascar"] {
        let fx = modified_release(rel, deferred_by_pn);
        let ds = eval(&fx, "libfoo-native");
        assert_eq!(ds.value("PICK").unwrap().text, "target", "{rel}");
        assert!(
            order_notes(&ds).is_empty(),
            "{rel}: {:?}",
            ds.replay().target_notes
        );
    }
    let fx = modified(deferred_by_pn);
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(ds.value("PICK").unwrap().text, "native");
    assert!(order_notes(&ds).is_empty());
}
