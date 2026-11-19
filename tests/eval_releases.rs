//! Evaluator differences found by the multi-release differential (acc-releases,
//! docs/differential.md, "Other releases"): `FILE_LAYERNAME` is not set before BitBake 2.6
//! ([`Feature::FileLayerName`]), `base.bbclass`'s features backfill up to Whinlatter
//! ([`Feature::FeaturesBackfill`]), and a `??=` on an override name taking part in the
//! override selection (every release). Each test runs on a temporary copy of the fixture
//! workspace; the fixture itself is not changed.
//!
//! [`Feature::FileLayerName`]: sous_chef::release::Feature::FileLayerName
//! [`Feature::FeaturesBackfill`]: sous_chef::release::Feature::FeaturesBackfill
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

fn evaluate(ws: &Workspace, recipe: &str) -> DataStore {
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), ws).unwrap();
    idx.refresh(ws).unwrap();
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &idx, ws, &t).unwrap()
}

fn append_to(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

fn text(ds: &DataStore, name: &str) -> Option<String> {
    ds.value(name).map(|v| v.text)
}

#[test]
fn file_layername_is_set_from_nanbield_on() {
    // BitBake 2.0 cookerdata.py `_parse_recipe` sets only __BBMULTICONFIG; 2.6 and later also
    // FILE_LAYERNAME (7090a14b0).
    for (rel, want) in [
        ("kirkstone", None),
        ("scarthgap", Some("core")),
        ("wrynose", Some("core")),
    ] {
        let (_tmp, _root, _cfg, ws) = temp_ws_release(rel, |_| {});
        let ds = evaluate(&ws, "libfoo");
        assert_eq!(
            text(&ds, "FILE_LAYERNAME").as_deref(),
            want,
            "FILE_LAYERNAME on {rel}"
        );
    }
}

const BACKFILL_ANON: &str = r#"
python () {
    # Handle backfilling
    oe.utils.features_backfill("DISTRO_FEATURES", d)
    oe.utils.features_backfill("MACHINE_FEATURES", d)
}
"#;

fn backfill_ws(rel: &str, considered: &str) -> Workspace {
    let (tmp, _root, _cfg, ws) = temp_ws_release(rel, |root| {
        append_to(root, "meta-core/classes/base.bbclass", BACKFILL_ANON);
        append_to(
            root,
            "build/conf/local.conf",
            &format!(
                "MACHINE_FEATURES = \"x86 pci\"\nMACHINE_FEATURES_BACKFILL = \"rtc qemu-usermode pci\"\nMACHINE_FEATURES_BACKFILL_CONSIDERED = \"{considered}\"\n"
            ),
        );
    });
    // Keep the copy alive for the rest of the test.
    std::mem::forget(tmp);
    ws
}

#[test]
fn features_backfill_up_to_whinlatter() {
    // OE-core lib/oe/utils.py features_backfill (kirkstone, scarthgap): every word of
    // <VAR>_BACKFILL in neither <VAR> nor <VAR>_BACKFILL_CONSIDERED, appended.
    for rel in ["kirkstone", "scarthgap"] {
        let ds = evaluate(&backfill_ws(rel, "qemu-usermode"), "libfoo");
        let v = ds.value("MACHINE_FEATURES").unwrap();
        // The fixture machine appends "ext2 alsa" to the local.conf value.
        assert_eq!(v.text, "x86 pci ext2 alsa rtc", "{rel}");
        assert!(!v.unevaluated_python && v.unresolved.is_empty(), "{rel}");
    }
    // Wrynose has no backfill (DISTRO/MACHINE_FEATURES_DEFAULTS instead): the call is left
    // alone, as before.
    let ds = evaluate(&backfill_ws("wrynose", "qemu-usermode"), "libfoo");
    assert_eq!(
        text(&ds, "MACHINE_FEATURES").as_deref(),
        Some("x86 pci ext2 alsa")
    );
}

#[test]
fn features_backfill_with_an_unknown_input_is_flagged() {
    let ds = evaluate(&backfill_ws("scarthgap", "${@os.getcwd()}"), "libfoo");
    let v = ds.value("MACHINE_FEATURES").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
}

#[test]
fn weak_default_on_an_override_name_is_selected() {
    // BitBake data_smart.py setVarFlag: `_defaultval` on a name with ':' runs
    // _setvar_update_overrides; getVarFlag of the override variant falls back to its
    // `_defaultval` (the same on branches 2.0, 2.8 and 2.18).
    for rel in ["kirkstone", "wrynose"] {
        let (_tmp, _root, _cfg, ws) = temp_ws_release(rel, |root| {
            append_to(
                root,
                "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
                "WEAKSEL ??= \"plain\"\nWEAKSEL:pn-libfoo ??= \"selected\"\nWEAKOFF:pn-other ??= \"no\"\n",
            );
        });
        let ds = evaluate(&ws, "libfoo");
        assert_eq!(text(&ds, "WEAKSEL").as_deref(), Some("selected"), "{rel}");
        assert_eq!(text(&ds, "WEAKOFF"), None, "{rel}");
    }
}

#[test]
fn pr_from_the_file_name_as_kirkstone_bitbake_conf_computes_it() {
    // OE-core bitbake.conf on kirkstone .. mickledore:
    // PR = "${@bb.parse.vars_from_file(d.getVar('FILE', False),d)[2] or 'r0'}".
    let (_tmp, _root, _cfg, ws) = temp_ws_release("kirkstone", |root| {
        append_to(
            root,
            "meta-core/conf/bitbake.conf",
            "PR = \"${@bb.parse.vars_from_file(d.getVar('FILE', False),d)[2] or 'r0'}\"\n",
        );
        fs::write(
            root.join("meta-core/recipes-core/libfoo/libbar_1.0_r7.bb"),
            "LICENSE = \"MIT\"\n",
        )
        .unwrap();
    });
    for (recipe, want) in [("libfoo", "r0"), ("libbar", "r7")] {
        let ds = evaluate(&ws, recipe);
        let v = ds.value("PR").unwrap();
        assert_eq!(v.text, want, "{recipe}");
        assert!(!v.unevaluated_python, "{recipe}: {v:?}");
    }
}
