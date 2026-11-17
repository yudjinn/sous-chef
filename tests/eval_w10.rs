//! Evaluator gaps closed in round w10: class-extended variants (`BBCLASSEXTEND`) evaluated
//! as BitBake makes them, `PREFERRED_PROVIDERS`, `BBMASK` recomputed when a file the
//! configuration pass read changes, and the skip checks callable with given values. Each
//! test runs on a temporary copy of the fixture workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::eval::providers::{ProviderReason, Providers};
use sous_chef::eval::{self, ConfigCache, DataStore, OpKind, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let config = ws_config(&root);
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        cache,
        _tmp: tmp,
        root,
        config,
        ws,
        idx,
    }
}

/// [`modified`] on a given release.
fn modified_release(codename: &str, edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root, config, ws) = temp_ws_release(codename, edit);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        cache,
        _tmp: tmp,
        root,
        config,
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
    try_eval(fx, recipe).unwrap()
}

fn try_eval(fx: &Fx, recipe: &str) -> sous_chef::error::Result<DataStore> {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t)
}

fn text(ds: &DataStore, name: &str) -> String {
    ds.value(name).map(|v| v.text).unwrap_or_default()
}

fn ctx_for(fx: &Fx) -> sous_chef::mcp::Ctx {
    let config = fx.config.clone();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(fx.cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles =
        sous_chef::handles::HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    sous_chef::mcp::Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: sous_chef::bitbake::JobTable::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Item 1: class-extended variants
// ---------------------------------------------------------------------------------------

/// Stand-ins for OE-core's class-extension classes: the statements the evaluator reads
/// and the handlers by name and event mask (their bodies are never run).
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
         nativesdk_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n",
    );
    write(
        root,
        "meta-core/classes/multilib.bbclass",
        "python multilib_virtclass_handler () {\n    cls = d.getVar(\"BBEXTENDCURR\")\n}\n\
         addhandler multilib_virtclass_handler\n\
         multilib_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n\
         python multilib_virtclass_handler_postkeyexp () {\n    cls = d.getVar(\"BBEXTENDCURR\")\n}\n\
         addhandler multilib_virtclass_handler_postkeyexp\n\
         multilib_virtclass_handler_postkeyexp[eventmask] = \"bb.event.RecipePostKeyExpansion\"\n",
    );
    append_to(
        root,
        "meta-core/conf/bitbake.conf",
        "NON_MULTILIB_RECIPES = \"\"\n",
    );
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "BBCLASSEXTEND = \"native nativesdk multilib:lib32\"\n\
         FOO = \"target\"\nFOO:class-native = \"native\"\nFOO:class-nativesdk = \"sdk\"\n\
         DEPENDS = \"zlib\"\n",
    );
}

#[test]
fn a_native_variant_is_evaluated_from_the_base_recipe() {
    let fx = modified(class_extension);
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(text(&ds, "PN"), "libfoo-native");
    // The fixture's BPN is `${PN}` (OE-core's prunes the suffix with python).
    assert_eq!(text(&ds, "BPN"), "libfoo-native");
    assert!(text(&ds, "FILE").ends_with("recipes-core/libfoo/libfoo_2.0.bb"));
    // native.bbclass's CLASSOVERRIDE applies to the recipe's own override variants.
    assert_eq!(text(&ds, "FOO"), "native");
    let ov = text(&ds, "OVERRIDES");
    assert!(
        ov.contains("pn-libfoo-native") && ov.contains("class-native"),
        "{ov}"
    );
    // PN is set by multi_finalize, recorded as such.
    assert!(
        ds.history("PN")
            .iter()
            .any(|(op, _)| op.kind == OpKind::ClassExtend && op.value == "libfoo-native")
    );
    // native_virtclass_handler filters DEPENDS: flagged, with a note, never guessed.
    let deps = ds.value("DEPENDS").unwrap();
    assert!(deps.unevaluated_python, "{deps:?}");
    assert!(
        ds.replay().var_notes["DEPENDS"]
            .iter()
            .any(|n| n.contains("native_virtclass_handler")),
        "{:?}",
        ds.replay().var_notes
    );
    assert_eq!(
        ds.replay().variant.as_ref().map(|v| v.word.as_str()),
        Some("native")
    );
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("the `native` variant of libfoo")),
        "{:?}",
        ds.replay().target_notes
    );
    // The base recipe is unchanged.
    let base = eval(&fx, "libfoo");
    assert_eq!(text(&base, "PN"), "libfoo");
    assert_eq!(text(&base, "FOO"), "target");
    assert!(!base.value("DEPENDS").unwrap().unevaluated_python);
    // The pointer form names the same variant.
    let p = eval(
        &fx,
        "virtual:native:meta-core:recipes-core/libfoo/libfoo_2.0.bb",
    );
    assert_eq!(text(&p, "PN"), "libfoo-native");
}

#[test]
fn nativesdk_and_multilib_variants_are_renamed_as_their_handlers_do() {
    let fx = modified(class_extension);
    let sdk = eval(&fx, "nativesdk-libfoo");
    assert_eq!(text(&sdk, "PN"), "nativesdk-libfoo");
    assert_eq!(text(&sdk, "MLPREFIX"), "nativesdk-");
    assert_eq!(text(&sdk, "FOO"), "sdk");
    assert!(
        sdk.history("PN")
            .iter()
            .any(|(op, _)| op.kind == OpKind::EventHandler)
    );

    let ml = eval(&fx, "lib32-libfoo");
    assert_eq!(text(&ml, "PN"), "lib32-libfoo");
    assert_eq!(text(&ml, "MLPREFIX"), "lib32-");
    assert_eq!(text(&ml, "BBEXTENDCURR"), "multilib");
    assert_eq!(text(&ml, "BBEXTENDVARIANT"), "lib32");
    let ov = text(&ml, "OVERRIDES");
    assert!(
        ov.contains("pn-lib32-libfoo:pn-libfoo") && ov.ends_with(":virtclass-multilib-lib32"),
        "{ov}"
    );
    // The RecipePostKeyExpansion handler's ClassExtender filters: flagged.
    assert!(ml.value("DEPENDS").unwrap().unevaluated_python);
    assert!(ml.value("PROVIDES").unwrap().unevaluated_python);
}

#[test]
fn a_variant_the_base_recipe_does_not_extend_is_not_found() {
    let fx = modified(class_extension);
    // hello has no BBCLASSEXTEND.
    let e = try_eval(&fx, "hello-native").unwrap_err().to_string();
    assert!(e.contains("not found"), "{e}");
    // A pointer to a variant whose word is not listed: BitBake makes no such variant.
    let e = try_eval(
        &fx,
        "virtual:native:meta-core:recipes-core/hello/hello_1.1.bb",
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("does not list `native`"), "{e}");
}

#[test]
fn a_variant_is_a_provider_of_its_name() {
    let fx = modified(|r| {
        class_extension(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "DEPENDS += \"libfoo-native\"\n",
        );
    });
    let p = Providers::new(&fx.idx, &fx.ws, None, None).unwrap();
    let set = p.providers("libfoo-native").unwrap();
    assert_eq!(set.reason, ProviderReason::Only);
    let sel = set.selected().unwrap();
    assert_eq!(sel.pn, "libfoo-native");
    assert_eq!(sel.selection.recipe.pn, "libfoo");
    assert_eq!(
        sel.selection.variant.as_ref().map(|v| v.word.as_str()),
        Some("native")
    );
    let ds = p.evaluate_selection(&sel.selection).unwrap();
    assert_eq!(text(&ds, "PN"), "libfoo-native");
    // best_version gives the same file, marked as the variant.
    let bv = p.best_version("libfoo-native", None).unwrap().unwrap();
    assert_eq!(bv.recipe.file.id, sel.selection.recipe.file.id);
    assert!(bv.variant.is_some());
}

#[tokio::test]
async fn the_tools_show_a_variant() {
    use sous_chef::mcp::tools::{deps, recipe, var};
    let fx = modified(|r| {
        class_extension(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "DEPENDS += \"libfoo-native\"\n",
        );
    });
    let ctx = ctx_for(&fx);
    let out = recipe::run(
        &ctx,
        recipe::Args {
            name: "libfoo-native".into(),
            version: None,
            show: recipe::Show::Summary,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        out.contains("libfoo-native (the `native` variant of libfoo, BBCLASSEXTEND `native`)"),
        "{out}"
    );
    assert!(
        out.contains("virtual:native:meta-core:recipes-core/libfoo/libfoo_2.0.bb"),
        "{out}"
    );
    let out = var::run(
        &ctx,
        var::Args {
            name: "FOO".into(),
            recipe: Some("libfoo-native".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("\"native\""), "{out}");
    let out = deps::run(
        &ctx,
        deps::Args {
            recipe: "app".into(),
            kind: deps::Kind::Build,
            depth: None,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        out.contains("libfoo-native (the `native` variant of libfoo 2.0)"),
        "{out}"
    );
}

/// `inherit_defer ${DEFCLASS}` in the recipe, with a `:class-native` variant of `DEFCLASS`.
fn deferred_pick(root: &Path) {
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
        "DEFCLASS = \"deftarget\"\nDEFCLASS:class-native = \"defnative\"\ninherit_defer ${DEFCLASS}\n",
    );
}

#[test]
fn the_variant_class_is_deferred_and_classoverride_is_set_before_the_deferred_inherits() {
    // Wrynose: the variant's class is deferred after the recipe's own deferred inherits
    // (BitBake 2.16+). Without OE-core's RecipePreDeferredInherits handler, CLASSOVERRIDE
    // is still class-target when `${DEFCLASS}` is expanded.
    let fx = modified(deferred_pick);
    assert_eq!(text(&eval(&fx, "libfoo-native"), "PICK"), "target");
    // With the handler (base_eventhandler on RecipePreDeferredInherits): class-native first.
    let fx = modified(|r| {
        deferred_pick(r);
        append_to(
            r,
            "meta-core/classes/base.bbclass",
            "base_eventhandler[eventmask] += \"bb.event.RecipePreDeferredInherits\"\n",
        );
    });
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(text(&ds, "PICK"), "native");
    assert!(
        ds.history("CLASSOVERRIDE")
            .iter()
            .any(|(op, _)| op.kind == OpKind::EventHandler && op.value == "class-native")
    );
    // Walnascar (BitBake 2.12): the class is inherited at once, so its CLASSOVERRIDE is
    // set before the deferred inherits anyway.
    let fx = modified_release("walnascar", deferred_pick);
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(text(&ds, "PICK"), "native");
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("inherits `native` at once")),
        "{:?}",
        ds.replay().target_notes
    );
    let _ = &fx.root;
}
