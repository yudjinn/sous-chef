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
#[allow(dead_code)]
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
