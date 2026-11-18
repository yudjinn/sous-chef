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
            mc: None,
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
            mc: None,
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
    // The variant pointer the recipe view shows is accepted as `recipe` too.
    const PTR: &str = "virtual:native:meta-core:recipes-core/libfoo/libfoo_2.0.bb";
    let out = var::run(
        &ctx,
        var::Args {
            mc: None,
            name: "PN".into(),
            recipe: Some(PTR.into()),
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
    assert!(out.contains("libfoo-native"), "{out}");
    let out = recipe::run(
        &ctx,
        recipe::Args {
            mc: None,
            name: PTR.into(),
            version: None,
            show: recipe::Show::Summary,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("the `native` variant of libfoo"), "{out}");
    let out = deps::run(
        &ctx,
        deps::Args {
            mc: None,
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
    // Walnascar (BitBake 2.12, `multi_finalize` on branch 2.12, corrected in w11): the
    // recipe's deferred inherits run once on the base recipe, before the variant is made
    // and before its class is inherited at once, so `${DEFCLASS}` is expanded as for the
    // target. The order is known there: no note.
    let fx = modified_release("walnascar", deferred_pick);
    let ds = eval(&fx, "libfoo-native");
    assert_eq!(text(&ds, "PICK"), "target");
    assert!(
        !ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("deferred inherits")),
        "{:?}",
        ds.replay().target_notes
    );
    let _ = &fx.root;
}

// ---------------------------------------------------------------------------------------
// Item 2: PREFERRED_PROVIDERS
// ---------------------------------------------------------------------------------------

/// testdistro.conf without its `PREFERRED_PROVIDER_virtual/foo`, plus `extra` in local.conf.
fn preferred_providers(extra: &'static str, keep_pp: bool) -> impl FnOnce(&Path) {
    move |root: &Path| {
        if !keep_pp {
            let conf = root.join("meta-core/conf/distro/testdistro.conf");
            let text = fs::read_to_string(&conf)
                .unwrap()
                .replace("PREFERRED_PROVIDER_virtual/foo ?= \"libfoo-alt\"\n", "");
            fs::write(&conf, text).unwrap();
        }
        append_to(root, "build/conf/local.conf", extra);
    }
}

fn providers_of(fx: &Fx, name: &str) -> sous_chef::eval::providers::ProviderSet {
    Providers::new(&fx.idx, &fx.ws, None, None)
        .unwrap()
        .providers(name)
        .unwrap()
}

#[test]
fn preferred_providers_picks_a_provider() {
    // Without a preference libfoo-alt is first (its PREFERRED_VERSION matches); the
    // PREFERRED_PROVIDERS entry moves libfoo to the front.
    let fx = modified(preferred_providers(
        "PREFERRED_PROVIDERS = \"virtual/bar:x virtual/foo:libfoo\"\n",
        false,
    ));
    let set = providers_of(&fx, "virtual/foo");
    assert_eq!(set.reason, ProviderReason::PreferredProviders);
    assert_eq!(set.providers[0].pn, "libfoo");
    let pp = set.preferred_provider.unwrap();
    assert_eq!(
        (pp.name.as_str(), pp.value.as_str()),
        ("PREFERRED_PROVIDERS", "libfoo")
    );
    assert!(pp.loc.unwrap().path.ends_with("conf/local.conf"));
}

#[test]
fn preferred_provider_replaces_a_preferred_providers_entry() {
    let fx = modified(preferred_providers(
        "PREFERRED_PROVIDERS = \"virtual/foo:libfoo\"\n",
        true,
    ));
    let set = providers_of(&fx, "virtual/foo");
    assert_eq!(set.reason, ProviderReason::PreferredProvider);
    assert_eq!(set.providers[0].pn, "libfoo-alt");
    assert!(
        set.notes
            .iter()
            .any(|n| n.contains("PREFERRED_PROVIDERS names libfoo") && n.contains("replaces it")),
        "{:?}",
        set.notes
    );
}

#[test]
fn preferred_providers_conflicts_and_malformed_words_are_said() {
    let fx = modified(preferred_providers(
        "PREFERRED_PROVIDERS = \"virtual/foo:libfoo virtual/foo:a:b virtual/foo:libfoo-alt\"\n",
        false,
    ));
    let set = providers_of(&fx, "virtual/foo");
    // The last entry wins; the malformed word is ignored.
    assert_eq!(set.reason, ProviderReason::PreferredProviders);
    assert_eq!(set.providers[0].pn, "libfoo-alt");
    assert!(
        set.notes
            .iter()
            .any(|n| n.contains("conflicting preferences")),
        "{:?}",
        set.notes
    );
    assert!(
        set.notes
            .iter()
            .any(|n| n.contains("malformed word \"virtual/foo:a:b\"")),
        "{:?}",
        set.notes
    );
    // An entry naming no provider of the item: said, BitBake's order kept.
    let fx = modified(preferred_providers(
        "PREFERRED_PROVIDERS = \"virtual/foo:nosuch\"\n",
        false,
    ));
    let set = providers_of(&fx, "virtual/foo");
    assert_eq!(set.reason, ProviderReason::FirstOfSeveral);
    assert!(
        set.notes
            .iter()
            .any(|n| n.contains("PREFERRED_PROVIDERS entry virtual/foo:nosuch")),
        "{:?}",
        set.notes
    );
}

// ---------------------------------------------------------------------------------------
// Item 3: BBMASK recomputed after an edit to any file the configuration pass read
// ---------------------------------------------------------------------------------------

/// `BBMASK` set in an `.inc` the distro conf requires, and a global class.
fn mask_in_inc(root: &Path) {
    write(
        root,
        "meta-core/conf/distro/include/mask.inc",
        "BBMASK = \"\"\n",
    );
    write(
        root,
        "meta-core/classes-global/maskclass.bbclass",
        "MASKCLASS = \"1\"\n",
    );
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "require conf/distro/include/mask.inc\n",
    );
    append_to(root, "build/conf/local.conf", "INHERIT += \"maskclass\"\n");
}

fn app_is_recipe(fx: &Fx) -> bool {
    !sous_chef::index::query::find_recipes(&fx.idx, "app")
        .unwrap()
        .is_empty()
}

#[test]
fn bbmask_is_recomputed_after_an_edit_to_an_inc_the_configuration_includes() {
    let mut fx = modified(mask_in_inc);
    assert!(app_is_recipe(&fx));
    let inputs = sous_chef::index::query::bbmask_inputs(&fx.idx)
        .unwrap()
        .unwrap();
    assert!(
        inputs
            .iter()
            .any(|(_, p)| p.ends_with("conf/distro/include/mask.inc")),
        "{inputs:?}"
    );
    // An edit to the .inc: refresh_file alone recomputes the marks.
    let inc = fx.root.join("meta-core/conf/distro/include/mask.inc");
    fs::write(&inc, "BBMASK = \"meta-mine/recipes-app/app/\"\n").unwrap();
    fx.idx.refresh_file(&fx.ws, &inc).unwrap();
    assert!(
        !app_is_recipe(&fx),
        "app is still collected after the .inc edit"
    );
    // And back.
    fs::write(&inc, "BBMASK = \"\"\n").unwrap();
    fx.idx.refresh_file(&fx.ws, &inc).unwrap();
    assert!(app_is_recipe(&fx));
    // A global class the configuration inherits.
    let class = fx.root.join("meta-core/classes-global/maskclass.bbclass");
    fs::write(
        &class,
        "MASKCLASS = \"1\"\nBBMASK += \"meta-mine/recipes-app/\"\n",
    )
    .unwrap();
    fx.idx.refresh_file(&fx.ws, &class).unwrap();
    assert!(
        !app_is_recipe(&fx),
        "app is still collected after the class edit"
    );
}

#[test]
fn the_per_machine_bbmask_note_fires_for_recipe_and_variant_evaluations() {
    // Masked for boardx only (eval_w8 checks the configuration-only evaluation).
    let fx = modified(|r| {
        class_extension(r);
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "BBMASK:boardx = \"meta-mine/recipes-app/app/\"\n",
        );
    });
    for recipe in ["hello", "libfoo-native"] {
        let t = Target {
            recipe: Some(recipe.into()),
            machine: Some("boardx".into()),
            ..Target::default()
        };
        let ds = eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap();
        assert!(
            ds.replay()
                .target_notes
                .iter()
                .any(|n| n.contains("BBMASK for this machine and distro differs")
                    && n.contains("meta-mine:recipes-app/app/app_0.1.bb")),
            "{recipe}: {:?}",
            ds.replay().target_notes
        );
    }
    // The index keeps the default machine's mask: app stays a recipe.
    assert!(app_is_recipe(&fx));
}

// ---------------------------------------------------------------------------------------
// Item 4: the skip checks, callable with given values
// ---------------------------------------------------------------------------------------

#[test]
fn compatible_machine_and_host_match_as_base_bbclass_does() {
    // re.match: anchored at the start only, any entry of MACHINEOVERRIDES.split(":").
    let mo = "armv7a:ti-soc:ti33x:dogbonedark";
    assert_eq!(
        eval::compatible_machine("ti33x|beagle", mo.split(':')),
        Some(true)
    );
    assert_eq!(eval::compatible_machine("ti", mo.split(':')), Some(true));
    assert_eq!(eval::compatible_machine("33x", mo.split(':')), Some(false));
    assert_eq!(
        eval::compatible_machine("^qemu.*$", mo.split(':')),
        Some(false)
    );
    assert_eq!(
        eval::compatible_machine("invalid", "".split(':')),
        Some(false)
    );
    assert_eq!(eval::compatible_machine("(?<=a)b", mo.split(':')), None);
    let host = "arm-oe-linux-gnueabi";
    assert_eq!(
        eval::compatible_host("(arm|aarch64).*-linux", host),
        Some(true)
    );
    assert_eq!(eval::compatible_host("aarch64.*-linux", host), Some(false));
    assert_eq!(eval::compatible_host(".*-musl.*", host), Some(false));
    assert_eq!(eval::compatible_host("^(?!riscv32).*", host), None);
}

#[test]
fn the_skip_notes_use_the_same_checks() {
    // A static MACHINEOVERRIDES (the fixture's is `${MACHINE}`) and HOST_SYS: the notes say
    // "would skip" exactly when the checks with those values fail.
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/bitbake.conf",
            "HOST_SYS = \"arm-oe-linux-gnueabi\"\n",
        );
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "COMPATIBLE_MACHINE = \"qemu\"\nCOMPATIBLE_HOST = \"aarch64.*-linux\"\n",
        );
    });
    let ds = eval(&fx, "app");
    let mo = text(&ds, "MACHINEOVERRIDES");
    assert_eq!(eval::compatible_machine("qemu", mo.split(':')), Some(true));
    assert_eq!(
        eval::compatible_host("aarch64.*-linux", &text(&ds, "HOST_SYS")),
        Some(false)
    );
    let notes = &ds.replay().target_notes;
    assert!(
        !notes
            .iter()
            .any(|n| n.contains("incompatible with machine")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.starts_with(
            "note: BitBake would skip this recipe: incompatible with host arm-oe-linux-gnueabi"
        )),
        "{notes:?}"
    );
}
