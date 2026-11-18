//! Multiconfig (`eval::multiconfig`): a copy of the fixture workspace with two multiconfigs,
//! modelled on OE-core's selftest `meta/lib/oeqa/selftest/cases/multiconfig.py`
//! (`test_multiconfig`: `BBMULTICONFIG = "tiny musl"`, each `conf/multiconfig/<name>.conf`
//! setting `MACHINE`, `TMPDIR` and the C library), and a kas project whose targets name
//! multiconfigs. The rules are in docs/bitbake-semantics.md, "Multiconfig".
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::error::Error;
use sous_chef::eval::multiconfig::{self, Source};
use sous_chef::eval::providers::Providers;
use sous_chef::eval::{self, ConfigCache, DataStore, RefProblem, Target};
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{deps, overrides, recipe, status, var};
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::KasEnv;

/// OE-core Wrynose `conf/bitbake.conf:824`, word for word.
const OE_REQUIRE: &str =
    r#"require ${@"conf/multiconfig/${BB_CURRENT_MC}.conf" if "${BB_CURRENT_MC}" != "" else ""}"#;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

/// The multiconfig workspace: OE-core's `require` after `conf/local.conf` (and before the
/// machine conf, as in OE-core), `BBMULTICONFIG = "tiny musl ghost"` in `local.conf`
/// (`ghost` has no file), two multiconfig files in the build directory, an unlisted one in
/// a layer, and a recipe with machine overrides and an `mcdepends`.
fn mc_workspace(root: &Path, require: &str) {
    let bb = root.join("meta-core/conf/bitbake.conf");
    let text = fs::read_to_string(&bb).unwrap();
    let text = text.replace(
        "include conf/local.conf\n",
        &format!("include conf/local.conf\n{require}\n"),
    );
    // A weak default after every include: the multiconfig file comes before it.
    let text = format!("{text}\nMCLATE ?= \"bitbake.conf\"\n");
    fs::write(&bb, text).unwrap();
    append(
        root,
        "build/conf/local.conf",
        "\nBBMULTICONFIG = \"tiny musl ghost\"\nMCLOCAL ?= \"local.conf\"\n",
    );
    for m in ["qemux86", "boardx"] {
        append(
            root,
            &format!("meta-core/conf/machine/{m}.conf"),
            "MCWEAK ?= \"machine\"\n",
        );
    }
    write(
        root,
        "build/conf/multiconfig/musl.conf",
        "MACHINE = \"boardx\"\nTCLIBC = \"musl\"\nTMPDIR = \"${TOPDIR}/tmp-mc-musl\"\n\
         PREFERRED_PROVIDER_virtual/foo = \"libfoo\"\n\
         MCWEAK ?= \"musl\"\nMCLATE ?= \"musl\"\nMCLOCAL ?= \"musl\"\n",
    );
    write(
        root,
        "build/conf/multiconfig/tiny.conf",
        "MACHINE = \"qemux86\"\nTMPDIR = \"${TOPDIR}/tmp-mc-tiny\"\n",
    );
    write(
        root,
        "meta-mine/conf/multiconfig/spare.conf",
        "MACHINE = \"boardx\"\n",
    );
    write(
        root,
        "meta-mine/recipes-app/mcapp/mcapp_1.0.bb",
        "SUMMARY = \"multiconfig test recipe\"\nLICENSE = \"MIT\"\n\
         GREETING = \"plain\"\nGREETING:boardx = \"boardx\"\nGREETING:qemux86 = \"qemu\"\n\
         DEPENDS = \"virtual/foo\"\n\
         do_install () {\n    :\n}\naddtask install\n\
         do_install[mcdepends] = \"mc:musl:tiny:hello:do_populate_sysroot mc::musl:libfoo:do_build\"\n",
    );
}

struct Fx {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: std::path::PathBuf,
    ws: Workspace,
    idx: Index,
}

fn fixture_with(require: &str) -> Fx {
    let (tmp, root) = temp_ws();
    mc_workspace(&root, require);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _tmp: tmp,
        _cache: cache,
        root,
        ws,
        idx,
    }
}

fn fixture() -> Fx {
    fixture_with(OE_REQUIRE)
}

fn target(recipe: Option<&str>, mc: Option<&str>) -> Target {
    Target {
        recipe: recipe.map(str::to_string),
        mc: mc.map(str::to_string),
        ..Target::default()
    }
}

fn eval_in(fx: &Fx, cache: &ConfigCache, recipe: Option<&str>, mc: Option<&str>) -> DataStore {
    eval::evaluate_with(cache, &fx.idx, &fx.ws, &target(recipe, mc)).unwrap()
}

fn text(ds: &DataStore, name: &str) -> String {
    ds.value(name).map(|v| v.text).unwrap_or_default()
}

fn ctx_for(fx: &Fx) -> (Ctx, tempfile::TempDir) {
    let config = ws_config(&fx.root);
    let workspace = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    (
        Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
        cache,
    )
}

// ---------------------------------------------------------------------------------------
// 1. Knowing the configurations
// ---------------------------------------------------------------------------------------

#[test]
fn lists_bbmulticonfig_with_files_and_problems() {
    let fx = fixture();
    let all = multiconfig::list(&fx.idx, &fx.ws).unwrap();
    assert_eq!(all.source, Source::Configuration);
    assert_eq!(all.value.as_deref(), Some("tiny musl ghost"));
    assert_eq!(
        all.setting.as_ref().map(|l| l.layer.as_str()),
        Some("build")
    );
    assert!(all.uncertain.is_none());
    let names: Vec<(&str, Option<String>)> = all
        .enabled
        .iter()
        .map(|m| (m.name.as_str(), m.file.as_ref().map(|l| l.to_string())))
        .collect();
    assert_eq!(
        names,
        [
            (
                "tiny",
                Some("build:conf/multiconfig/tiny.conf:1".to_string())
            ),
            (
                "musl",
                Some("build:conf/multiconfig/musl.conf:1".to_string())
            ),
            ("ghost", None),
        ]
    );
    // A listed name with no file is what BitBake stops on; reported, not an error.
    assert_eq!(all.problems.len(), 1, "{:?}", all.problems);
    assert!(all.problems[0].contains("\"ghost\""));
    assert!(all.problems[0].contains("Could not require file conf/multiconfig/ghost.conf"));
    // A file along BBPATH that BBMULTICONFIG does not list.
    let present: Vec<&str> = all.present.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(present, ["spare"]);
}

#[test]
fn a_python_bbmulticonfig_is_uncertain_and_lists_the_files_present() {
    let (tmp, root) = temp_ws();
    mc_workspace(&root, OE_REQUIRE);
    append(
        &root,
        "build/conf/local.conf",
        "BBMULTICONFIG = \"${@'musl' if True else ''}\"\n",
    );
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let all = multiconfig::list(&idx, &ws).unwrap();
    assert!(all.uncertain.is_some());
    assert!(all.enabled.is_empty());
    let mut present: Vec<&str> = all.present.iter().map(|m| m.name.as_str()).collect();
    present.sort();
    assert_eq!(present, ["musl", "spare", "tiny"]);
    // Such a name is accepted, with a note that it is not confirmed.
    let ds =
        eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &target(None, Some("musl"))).unwrap();
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("present but not confirmed enabled")),
        "{:?}",
        ds.replay().target_notes
    );
    drop(tmp);
}

#[test]
fn a_workspace_without_multiconfig_lists_nothing() {
    let ws = fixture_ws();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let all = multiconfig::list(&idx, &ws).unwrap();
    assert!(all.is_empty(), "{all:?}");
    assert!(all.value.is_none());
}

// ---------------------------------------------------------------------------------------
// 2. Evaluating in a configuration
// ---------------------------------------------------------------------------------------

#[test]
fn the_multiconfig_file_sets_the_machine_and_its_values() {
    let fx = fixture();
    let cache = ConfigCache::new();
    let default = eval_in(&fx, &cache, None, None);
    let musl = eval_in(&fx, &cache, None, Some("musl"));
    let tiny = eval_in(&fx, &cache, None, Some("tiny"));
    assert_eq!(text(&default, "MACHINE"), "qemux86");
    assert_eq!(text(&musl, "MACHINE"), "boardx");
    assert_eq!(text(&tiny, "MACHINE"), "qemux86");
    assert_eq!(text(&musl, "BB_CURRENT_MC"), "musl");
    assert_eq!(text(&tiny, "BB_CURRENT_MC"), "tiny");
    // The default configuration does not set it (unchanged from before multiconfig).
    assert!(default.value("BB_CURRENT_MC").is_none());
    assert!(
        text(&musl, "TMPDIR").ends_with("/tmp-mc-musl"),
        "{}",
        text(&musl, "TMPDIR")
    );
    assert!(text(&tiny, "TMPDIR").ends_with("/tmp-mc-tiny"));
    assert!(text(&default, "TMPDIR").ends_with("/tmp"));
    // The machine conf the multiconfig's MACHINE selects is the one read.
    assert_eq!(text(&musl, "KERNEL_IMAGETYPE"), "zImage");
    assert_eq!(text(&default, "KERNEL_IMAGETYPE"), "bzImage");
    assert_eq!(text(&musl, "TCLIBC"), "musl");
    assert_eq!(text(&default, "TCLIBC"), "glibc");
    // The note says which configuration and which file.
    assert!(
        musl.replay().target_notes.iter().any(|n| n.starts_with(
            "note: multiconfig musl: the configuration parsed again with BB_CURRENT_MC = \"musl\", conf/multiconfig/musl.conf (build:conf/multiconfig/musl.conf)"
        )),
        "{:?}",
        musl.replay().target_notes
    );
    assert!(
        !default
            .replay()
            .target_notes
            .iter()
            .any(|n| n.contains("multiconfig"))
    );
}

/// `?=` decides by order, and the order is BitBake's: the multiconfig file is read where
/// OE-core's `bitbake.conf` requires it, after `local.conf` and before the machine conf and
/// the rest of `bitbake.conf`. Appending it after the finished base configuration would give
/// `machine` and `bitbake.conf` for the first two.
#[test]
fn weak_defaults_follow_the_require_position() {
    let fx = fixture();
    let cache = ConfigCache::new();
    let musl = eval_in(&fx, &cache, None, Some("musl"));
    assert_eq!(text(&musl, "MCWEAK"), "musl");
    assert_eq!(text(&musl, "MCLATE"), "musl");
    // local.conf comes before the multiconfig file: its `?=` was first.
    assert_eq!(text(&musl, "MCLOCAL"), "local.conf");
    let default = eval_in(&fx, &cache, None, None);
    assert_eq!(text(&default, "MCWEAK"), "machine");
    assert_eq!(text(&default, "MCLATE"), "bitbake.conf");
    // The visit order shows it: local.conf, the multiconfig file, then the machine conf.
    let order: Vec<String> = musl
        .files()
        .iter()
        .map(|v| v.loc.path.display().to_string())
        .filter(|p| {
            p == "conf/local.conf"
                || p == "conf/multiconfig/musl.conf"
                || p.starts_with("conf/machine/")
        })
        .collect();
    assert_eq!(
        order,
        [
            "conf/local.conf",
            "conf/multiconfig/musl.conf",
            "conf/machine/boardx.conf"
        ]
    );
}

/// The Kirkstone / Scarthgap form of OE-core's line expands with `BB_CURRENT_MC` alone.
#[test]
fn the_older_require_form_works_too() {
    let fx = fixture_with("require conf/multiconfig/${BB_CURRENT_MC}.conf");
    let cache = ConfigCache::new();
    let musl = eval_in(&fx, &cache, None, Some("musl"));
    assert_eq!(text(&musl, "MACHINE"), "boardx");
    assert_eq!(text(&musl, "MCWEAK"), "musl");
    // The default configuration is replayed as before: the reference is left unexpanded.
    let default = eval_in(&fx, &cache, None, None);
    assert!(default.replay().references.iter().any(|r| r.text
        == "conf/multiconfig/${BB_CURRENT_MC}.conf"
        && matches!(&r.problem, RefProblem::Undefined(v) if v == &["BB_CURRENT_MC".to_string()])));
}

#[test]
fn the_default_pass_keeps_the_python_require_unfollowed() {
    let fx = fixture();
    let default = eval_in(&fx, &ConfigCache::new(), None, None);
    assert!(
        default
            .replay()
            .references
            .iter()
            .any(|r| r.kind == "require" && r.problem == RefProblem::Python)
    );
    let musl = eval_in(&fx, &ConfigCache::new(), None, Some("musl"));
    assert!(
        !musl
            .replay()
            .references
            .iter()
            .any(|r| r.kind == "require" && r.text.contains("multiconfig"))
    );
}

#[test]
fn a_bitbake_conf_without_the_require_reads_no_multiconfig_file() {
    let (tmp, root) = temp_ws();
    mc_workspace(&root, "");
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let ds =
        eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &target(None, Some("musl"))).unwrap();
    assert_eq!(text(&ds, "MACHINE"), "qemux86");
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("never read conf/multiconfig/musl.conf")),
        "{:?}",
        ds.replay().target_notes
    );
    drop(tmp);
}

#[test]
fn the_cache_is_per_configuration() {
    let fx = fixture();
    let cache = ConfigCache::new();
    eval_in(&fx, &cache, None, None);
    assert_eq!(cache.passes(), 1);
    eval_in(&fx, &cache, None, Some("musl"));
    assert_eq!(cache.passes(), 2);
    eval_in(&fx, &cache, None, Some("tiny"));
    assert_eq!(cache.passes(), 3);
    eval_in(&fx, &cache, Some("mcapp"), Some("musl"));
    eval_in(&fx, &cache, Some("mcapp"), None);
    eval_in(&fx, &cache, None, Some("tiny"));
    assert_eq!(cache.passes(), 3);
    // An empty name is the default configuration, with the default's cache entry.
    eval_in(&fx, &cache, None, Some(""));
    assert_eq!(cache.passes(), 3);
}

#[test]
fn recipe_values_differ_per_configuration() {
    let fx = fixture();
    let cache = ConfigCache::new();
    let d = eval_in(&fx, &cache, Some("mcapp"), None);
    let m = eval_in(&fx, &cache, Some("mcapp"), Some("musl"));
    let prefixed = eval_in(&fx, &cache, Some("mc:musl:mcapp"), None);
    assert_eq!(text(&d, "GREETING"), "qemu");
    assert_eq!(text(&m, "GREETING"), "boardx");
    assert_eq!(text(&prefixed, "GREETING"), "boardx");
    // `multiconfig:` is the old spelling BitBake still accepts on the command line.
    let old = eval_in(&fx, &cache, Some("multiconfig:musl:mcapp"), None);
    assert_eq!(text(&old, "GREETING"), "boardx");
    // `machine` still overrides, on top of the multiconfig.
    let t = Target {
        recipe: Some("mcapp".into()),
        machine: Some("qemux86".into()),
        mc: Some("musl".into()),
        ..Target::default()
    };
    let over = eval::evaluate_with(&cache, &fx.idx, &fx.ws, &t).unwrap();
    assert_eq!(text(&over, "MACHINE"), "qemux86");
    assert_eq!(text(&over, "GREETING"), "qemu");
    assert_eq!(text(&over, "TCLIBC"), "musl");
}

#[test]
fn unknown_and_fileless_names_are_errors() {
    let fx = fixture();
    let cache = ConfigCache::new();
    let err =
        eval::evaluate_with(&cache, &fx.idx, &fx.ws, &target(None, Some("nope"))).unwrap_err();
    match err {
        Error::NotFound { what } => {
            assert!(
                what.contains("multiconfig nope; known: tiny, musl, ghost"),
                "{what}"
            )
        }
        e => panic!("{e:?}"),
    }
    let err =
        eval::evaluate_with(&cache, &fx.idx, &fx.ws, &target(None, Some("spare"))).unwrap_err();
    assert!(
        err.to_string().contains(
            "meta-mine:conf/multiconfig/spare.conf:1 exists but BBMULTICONFIG does not list spare"
        ),
        "{err}"
    );
    let err =
        eval::evaluate_with(&cache, &fx.idx, &fx.ws, &target(None, Some("ghost"))).unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    let err = eval::evaluate_with(
        &cache,
        &fx.idx,
        &fx.ws,
        &Target {
            recipe: Some("mc:musl:mcapp".into()),
            mc: Some("tiny".into()),
            ..Target::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------------------
// 4. Providers and dependencies
// ---------------------------------------------------------------------------------------

#[test]
fn providers_are_per_configuration() {
    let fx = fixture();
    let d = Providers::new(&fx.idx, &fx.ws, None, None).unwrap();
    let m = Providers::new_in(&fx.idx, &fx.ws, None, None, Some("musl")).unwrap();
    let pick = |p: &Providers<'_>| {
        p.providers("virtual/foo")
            .unwrap()
            .selected()
            .map(|s| s.pn.clone())
            .unwrap()
    };
    assert_eq!(pick(&d), "libfoo-alt");
    assert_eq!(pick(&m), "libfoo");
}

#[test]
fn mcdepends_are_read_with_pointers() {
    let fx = fixture();
    let cache = ConfigCache::new();
    let m = eval_in(&fx, &cache, Some("mcapp"), Some("musl"));
    let deps = multiconfig::mcdepends(&m);
    assert_eq!(deps.len(), 2, "{deps:?}");
    assert_eq!(deps[0].task, "do_install");
    assert_eq!(
        (
            deps[0].from.as_deref(),
            deps[0].to.as_deref(),
            deps[0].recipe.as_deref(),
            deps[0].dep_task.as_deref()
        ),
        (
            Some("musl"),
            Some("tiny"),
            Some("hello"),
            Some("do_populate_sysroot")
        )
    );
    assert!(deps[0].applies);
    assert!(!deps[1].applies);
    assert_eq!(
        deps[0].locs[0].path,
        Path::new("recipes-app/mcapp/mcapp_1.0.bb")
    );
    // In the default configuration the second one applies (from = "").
    let d = eval_in(&fx, &cache, Some("mcapp"), None);
    let deps = multiconfig::mcdepends(&d);
    assert!(!deps[0].applies);
    assert!(deps[1].applies);
}

// ---------------------------------------------------------------------------------------
// 3. Addressing, through the tools
// ---------------------------------------------------------------------------------------

fn vargs(name: &str, recipe: Option<&str>, mc: Option<&str>) -> var::Args {
    var::Args {
        name: name.into(),
        recipe: recipe.map(str::to_string),
        machine: None,
        distro: None,
        mc: mc.map(str::to_string),
        authoritative: false,
        flags: false,
        budget_tokens: Some(4000),
        format: None,
    }
}

#[tokio::test]
async fn var_answers_per_configuration_with_a_note() {
    let fx = fixture();
    let (ctx, _c) = ctx_for(&fx);
    let d = var::run(&ctx, vargs("GREETING", Some("mcapp"), None))
        .await
        .unwrap();
    assert!(d.contains("final: \"qemu\""), "{d}");
    assert!(!d.contains("note: multiconfig"), "{d}");
    let m = var::run(&ctx, vargs("GREETING", Some("mcapp"), Some("musl")))
        .await
        .unwrap();
    assert!(m.contains("final: \"boardx\""), "{m}");
    assert_eq!(m.matches("note: multiconfig musl").count(), 1, "{m}");
    let p = var::run(&ctx, vargs("GREETING", Some("mc:tiny:mcapp"), None))
        .await
        .unwrap();
    assert!(p.contains("final: \"qemu\""), "{p}");
    assert!(p.contains("note: multiconfig tiny"), "{p}");
    let err = var::run(&ctx, vargs("GREETING", Some("mcapp"), Some("nope")))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("known: tiny, musl, ghost"),
        "{err}"
    );
    let mut a = vargs("GREETING", Some("mcapp"), Some("musl"));
    a.authoritative = true;
    assert!(var::run(&ctx, a).await.is_err());
}

#[tokio::test]
async fn overrides_deps_and_recipe_take_mc() {
    let fx = fixture();
    let (ctx, _c) = ctx_for(&fx);
    let o = overrides::run(
        &ctx,
        overrides::Args {
            recipe: Some("mcapp".into()),
            machine: None,
            distro: None,
            mc: Some("musl".into()),
            show: overrides::Show::Chain,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(o.contains("boardx"), "{o}");
    assert!(o.contains("note: multiconfig musl"), "{o}");

    let dargs = |recipe: &str, kind, mc: Option<&str>| deps::Args {
        recipe: recipe.into(),
        kind,
        mc: mc.map(str::to_string),
        depth: None,
        budget_tokens: Some(4000),
        format: None,
    };
    let b = deps::run(&ctx, dargs("mc:musl:mcapp", deps::Kind::Build, None))
        .await
        .unwrap();
    assert!(
        b.contains("libfoo 2.0 (PREFERRED_PROVIDER_virtual/foo)"),
        "{b}"
    );
    assert!(b.contains("2 cross-configuration task dependencies"), "{b}");
    assert!(b.contains("note: multiconfig musl"), "{b}");
    let b = deps::run(&ctx, dargs("mcapp", deps::Kind::Build, None))
        .await
        .unwrap();
    assert!(
        b.contains("libfoo-alt 1.0 (PREFERRED_PROVIDER_virtual/foo)"),
        "{b}"
    );
    let p = deps::run(
        &ctx,
        dargs("virtual/foo", deps::Kind::Providers, Some("musl")),
    )
    .await
    .unwrap();
    assert!(
        p.lines().nth(1).unwrap_or("").starts_with("libfoo\t"),
        "{p}"
    );
    assert!(p.contains("note: multiconfig musl"), "{p}");
    let x = deps::run(&ctx, dargs("mcapp", deps::Kind::Mcdepends, Some("musl")))
        .await
        .unwrap();
    assert!(x.contains("do_install\tmusl\ttiny\thello\tdo_populate_sysroot\tapplies here\tmeta-mine:recipes-app/mcapp/mcapp_1.0.bb:"), "{x}");
    assert!(
        x.contains("(default)\tmusl\tlibfoo\tdo_build\tnot in this configuration"),
        "{x}"
    );
    assert!(x.contains("listed, not followed"), "{x}");

    let r = recipe::run(
        &ctx,
        recipe::Args {
            name: "mc:musl:mcapp".into(),
            version: None,
            show: recipe::Show::Summary,
            mc: None,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(r.contains("MACHINE=boardx"), "{r}");
    assert!(r.contains("note: multiconfig musl"), "{r}");
}

#[tokio::test]
async fn status_lists_the_configurations() {
    let fx = fixture();
    let (ctx, _c) = ctx_for(&fx);
    let s = status::run(
        &ctx,
        status::Args {
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        s.contains("multiconfig\ttiny build:conf/multiconfig/tiny.conf:1"),
        "{s}"
    );
    assert!(
        s.contains("multiconfig\tmusl build:conf/multiconfig/musl.conf:1"),
        "{s}"
    );
    assert!(s.contains("multiconfig\tghost (no file)"), "{s}");
    assert!(
        s.contains("multiconfig_problem\tBBMULTICONFIG lists \"ghost\""),
        "{s}"
    );
    assert!(
        s.contains("multiconfig_present\tspare meta-mine:conf/multiconfig/spare.conf:1 (not in BBMULTICONFIG)"),
        "{s}"
    );
}

// ---------------------------------------------------------------------------------------
// kas
// ---------------------------------------------------------------------------------------

/// `tests/fixtures/kas-mc/project`: a kas project whose targets name two multiconfigs
/// (one with the old `multiconfig:` spelling), its layer shipping their files.
fn kas_project() -> (tempfile::TempDir, std::path::PathBuf, Workspace) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    copy_dir(&fixture_path("kas-mc/project"), &root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let root = root.canonicalize().unwrap();
    let cfg = Config {
        kas: vec![root.join("kas/mc.yaml")],
        work_dir: root.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    (tmp, root, ws)
}

#[test]
fn kas_targets_give_the_multiconfigs() {
    let (_tmp, _root, ws) = kas_project();
    let k = ws.kas.as_ref().unwrap();
    assert_eq!(k.multiconfigs(), ["musl", "tiny"]);
    assert!(k.multiconfig_headers().is_empty());
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let all = multiconfig::list(&idx, &ws).unwrap();
    assert_eq!(all.source, Source::Kas);
    assert!(all.uncertain.is_none(), "{all:?}");
    let names: Vec<(&str, Option<String>)> = all
        .enabled
        .iter()
        .map(|m| (m.name.as_str(), m.file.as_ref().map(|l| l.to_string())))
        .collect();
    assert_eq!(
        names,
        [
            (
                "musl",
                Some("meta-mc:conf/multiconfig/musl.conf:1".to_string())
            ),
            ("tiny", None)
        ]
    );
    assert_eq!(all.problems.len(), 1, "{:?}", all.problems);
    // Without OE-core checked out there is no bitbake.conf: the pass says nothing is read.
    let ds =
        eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &target(None, Some("musl"))).unwrap();
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("never read conf/multiconfig/musl.conf")),
        "{:?}",
        ds.replay().target_notes
    );
}

#[test]
fn a_kas_header_setting_bbmulticonfig_makes_the_list_uncertain() {
    let (_tmp, root, _) = kas_project();
    append(
        &root,
        "kas/mc.yaml",
        "local_conf_header:\n  mc: |\n    BBMULTICONFIG = \"musl\"\n",
    );
    let cfg = Config {
        kas: vec![root.join("kas/mc.yaml")],
        work_dir: root.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    assert_eq!(ws.kas.as_ref().unwrap().multiconfig_headers(), ["mc"]);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let all = multiconfig::list(&idx, &ws).unwrap();
    assert!(
        all.uncertain.as_deref().unwrap_or("").contains("(mc)"),
        "{all:?}"
    );
}
