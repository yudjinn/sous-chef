//! T3.5 to T3.7: the `var`, `overrides` and `deps` tools on the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::error::Error;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{deps, overrides, var};
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

struct Fixture {
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn fixture_with(config: Config) -> Fixture {
    let cache = tempfile::tempdir().unwrap();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    // Refresh once so tool output carries no freshness line.
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    Fixture { _cache: cache, ctx }
}

fn fixture() -> Fixture {
    fixture_with(common::fixture_config())
}

fn vargs(name: &str, recipe: Option<&str>, machine: Option<&str>) -> var::Args {
    var::Args {
        mc: None,
        name: name.into(),
        recipe: recipe.map(str::to_string),
        machine: machine.map(str::to_string),
        distro: None,
        authoritative: false,
        flags: false,
        budget_tokens: Some(4000),
        format: None,
    }
}

fn oargs(recipe: Option<&str>, machine: Option<&str>, show: overrides::Show) -> overrides::Args {
    overrides::Args {
        mc: None,
        recipe: recipe.map(str::to_string),
        machine: machine.map(str::to_string),
        distro: None,
        show,
        budget_tokens: Some(4000),
        format: None,
    }
}

fn dargs(recipe: &str, kind: deps::Kind) -> deps::Args {
    deps::Args {
        mc: None,
        recipe: recipe.into(),
        kind,
        depth: None,
        budget_tokens: Some(4000),
        format: None,
    }
}

async fn var_ok(f: &Fixture, a: var::Args) -> String {
    var::run(&f.ctx, a).await.unwrap()
}

// ---- var ----

#[tokio::test]
async fn var_src_uri_three_layers() {
    let f = fixture();
    let out = var_ok(&f, vargs("SRC_URI", Some("hello"), None)).await;
    insta::assert_snapshot!("var_src_uri_hello_qemux86", out);
}

#[tokio::test]
async fn var_machine_conditional_two_machines() {
    let f = fixture();
    let q = var_ok(&f, vargs("EXTRA_OECONF", Some("hello"), Some("qemux86"))).await;
    let b = var_ok(&f, vargs("EXTRA_OECONF", Some("hello"), Some("boardx"))).await;
    insta::assert_snapshot!("var_extra_oeconf_hello_qemux86", q);
    insta::assert_snapshot!("var_extra_oeconf_hello_boardx", b);
}

#[tokio::test]
async fn var_flag_query() {
    let f = fixture();
    let one = var_ok(&f, vargs("PACKAGECONFIG[zlib]", Some("libfoo"), None)).await;
    insta::assert_snapshot!("var_packageconfig_zlib_libfoo", one);
    let mut a = vargs("SRC_URI", Some("hello_1.1"), None);
    a.flags = true;
    let all = var_ok(&f, a).await;
    insta::assert_snapshot!("var_src_uri_flags_hello", all);
}

#[tokio::test]
async fn var_python_dependent() {
    let f = fixture();
    let out = var_ok(&f, vargs("EXTRA_OEMAKE", Some("app"), None)).await;
    insta::assert_snapshot!("var_extra_oemake_app", out);
}

#[tokio::test]
async fn var_global_and_names() {
    let f = fixture();
    let g = var_ok(&f, vargs("DISTRO_FEATURES", None, None)).await;
    insta::assert_snapshot!("var_distro_features_global", g);
    let files = var_ok(&f, vargs("FILES:${PN}", Some("hello"), None)).await;
    insta::assert_snapshot!("var_files_pn_hello", files);
    let func = var_ok(&f, vargs("do_install", Some("hello"), None)).await;
    insta::assert_snapshot!("var_do_install_hello", func);
}

#[tokio::test]
async fn var_json_and_budget() {
    let f = fixture();
    let mut a = vargs("SRC_URI", Some("hello"), None);
    a.format = Some(Format::Json);
    let j: serde_json::Value = serde_json::from_str(&var_ok(&f, a).await).unwrap();
    assert_eq!(j["header"][0], "status");
    assert!(j["notes"][0].as_str().unwrap().starts_with("final: "));
    let mut a = vargs("SRC_URI", Some("hello"), None);
    a.budget_tokens = Some(60);
    let out = var_ok(&f, a).await;
    assert!(out.contains("more over budget_tokens"), "{out}");
    assert!(out.contains("final: "), "{out}");
}

#[tokio::test]
async fn var_long_value_goes_to_a_handle() {
    let (_tmp, root) = common::temp_ws();
    let long: String = (0..120).map(|i| format!(" file://p{i}.patch")).collect();
    let app = root.join("meta-mine/recipes-app/app/app_0.1.bb");
    let mut text = std::fs::read_to_string(&app).unwrap();
    text.push_str(&format!("SRC_URI += \"{long}\"\n"));
    std::fs::write(&app, text).unwrap();
    let f = fixture_with(common::ws_config(&root));
    let out = var_ok(&f, vargs("SRC_URI", Some("app"), None)).await;
    let line = out.lines().find(|l| l.starts_with("final: ")).unwrap();
    assert!(line.contains("slice handle=h:"), "{line}");
    let h = line.split("handle=").nth(1).unwrap().trim_end_matches(']');
    let blob = f
        .ctx
        .handles
        .get(&sous_chef::handles::Handle(h.to_string()))
        .unwrap();
    assert!(
        String::from_utf8(blob)
            .unwrap()
            .ends_with("file://p119.patch")
    );
}

#[tokio::test]
async fn var_authoritative_is_unavailable() {
    let f = fixture();
    let mut a = vargs("SRC_URI", Some("hello"), None);
    a.authoritative = true;
    let e = var::run(&f.ctx, a).await.unwrap_err();
    assert!(matches!(e, Error::BitbakeUnavailable), "{e}");
}

#[tokio::test]
async fn var_unknown_recipe_lists_candidates() {
    let f = fixture();
    let e = var::run(&f.ctx, vargs("SRC_URI", Some("hel"), None))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("hello"), "{e}");
}

// ---- deps ----

async fn deps_ok(f: &Fixture, a: deps::Args) -> String {
    deps::run(&f.ctx, a).await.unwrap()
}

#[tokio::test]
async fn deps_providers_virtual_foo() {
    let f = fixture();
    let out = deps_ok(&f, dargs("virtual/foo", deps::Kind::Providers)).await;
    insta::assert_snapshot!("deps_providers_virtual_foo", out);
    let first = out.lines().nth(1).unwrap();
    assert!(first.starts_with("libfoo-alt\t"), "{out}");
    let pp = out
        .lines()
        .find(|l| l.starts_with("PREFERRED_PROVIDER_virtual/foo\t"))
        .unwrap();
    assert!(
        pp.ends_with("meta-core:conf/distro/testdistro.conf:8"),
        "{pp}"
    );
}

#[tokio::test]
async fn deps_packageconfig_libfoo() {
    let f = fixture();
    let out = deps_ok(&f, dargs("libfoo", deps::Kind::Packageconfig)).await;
    insta::assert_snapshot!("deps_packageconfig_libfoo", out);
}

#[tokio::test]
async fn deps_reverse_libfoo() {
    let f = fixture();
    let out = deps_ok(&f, dargs("libfoo", deps::Kind::Reverse)).await;
    insta::assert_snapshot!("deps_reverse_libfoo", out);
}

#[tokio::test]
async fn deps_build_and_runtime() {
    let f = fixture();
    let b = deps_ok(&f, dargs("hello", deps::Kind::Build)).await;
    insta::assert_snapshot!("deps_build_hello", b);
    let mut a = dargs("app", deps::Kind::Build);
    a.depth = Some(2);
    let b2 = deps_ok(&f, a).await;
    insta::assert_snapshot!("deps_build_app_depth2", b2);
    let r = deps_ok(&f, dargs("libfoo", deps::Kind::Runtime)).await;
    insta::assert_snapshot!("deps_runtime_libfoo", r);
}

// ---- provider resolution ----

fn append(path: &std::path::Path, text: &str) {
    let mut s = std::fs::read_to_string(path).unwrap();
    s.push_str(text);
    std::fs::write(path, s).unwrap();
}

#[tokio::test]
async fn providers_default_preference_and_preferred_version() {
    use sous_chef::eval::providers::{Providers, VersionReason};
    let (_tmp, root) = common::temp_ws();
    let dir = root.join("meta-core/recipes-core/hello");
    // A newer version that lowers itself: the latest stays 1.1.
    std::fs::write(
        dir.join("hello_1.2.bb"),
        "require hello.inc\nDEFAULT_PREFERENCE = \"-1\"\n",
    )
    .unwrap();
    let f = fixture_with(common::ws_config(&root));
    {
        let index = f.ctx.index.lock().await;
        let p = Providers::new(&index, &f.ctx.workspace, None, None).unwrap();
        let s = p.best_version("hello", None).unwrap().unwrap();
        assert_eq!(s.recipe.pv, "1.1");
        assert_eq!(s.reason, VersionReason::Latest);
        // A version pattern takes the first match in sortPriorities order, where a lower
        // DEFAULT_PREFERENCE comes first within a priority (BitBake's own quirk).
        let s = p.best_version("hello", Some("1.%")).unwrap().unwrap();
        assert_eq!(s.recipe.pv, "1.2", "first match in sortPriorities order");
        let s = p.best_version("hello", Some("1.2")).unwrap().unwrap();
        assert_eq!(s.recipe.pv, "1.2");
    }
    // PREFERRED_VERSION picks 1.0, also for evaluate() by name and for the recipe tool.
    append(
        &root.join("build/conf/local.conf"),
        "PREFERRED_VERSION_hello = \"1.0\"\n",
    );
    let out = var_ok(&f, vargs("SRC_URI", Some("hello"), None)).await;
    assert!(out.contains("hello-1.0.tar.gz"), "{out}");
    assert!(!out.contains("mine.patch"), "{out}");
    let summary = sous_chef::mcp::tools::recipe::run(
        &f.ctx,
        sous_chef::mcp::tools::recipe::Args {
            mc: None,
            name: "hello".into(),
            version: None,
            show: sous_chef::mcp::tools::recipe::Show::Summary,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        summary.contains("1.0 (PREFERRED_VERSION_hello = \"1.0\")"),
        "{summary}"
    );
    // A preferred version that matches nothing falls back to the latest, with a note.
    append(
        &root.join("build/conf/local.conf"),
        "PREFERRED_VERSION_hello = \"9.%\"\n",
    );
    {
        let mut index = f.ctx.index.lock().await;
        index.refresh(&f.ctx.workspace).unwrap();
        let p = Providers::new(&index, &f.ctx.workspace, None, None).unwrap();
        let s = p.best_version("hello", None).unwrap().unwrap();
        assert_eq!(s.recipe.pv, "1.1");
        assert!(s.notes[0].contains("matches no version"), "{:?}", s.notes);
    }
}

#[tokio::test]
async fn providers_without_preferred_provider() {
    use sous_chef::eval::providers::{ProviderReason, Providers};
    let (_tmp, root) = common::temp_ws();
    let conf = root.join("meta-core/conf/distro/testdistro.conf");
    let text = std::fs::read_to_string(&conf)
        .unwrap()
        .replace("PREFERRED_PROVIDER_virtual/foo ?= \"libfoo-alt\"\n", "");
    std::fs::write(&conf, text).unwrap();
    let f = fixture_with(common::ws_config(&root));
    let index = f.ctx.index.lock().await;
    let p = Providers::new(&index, &f.ctx.workspace, None, None).unwrap();
    let set = p.providers("virtual/foo").unwrap();
    // libfoo-alt has a matching PREFERRED_VERSION, so it is listed first (filterProviders).
    assert_eq!(set.reason, ProviderReason::FirstOfSeveral);
    let pns: Vec<&str> = set.providers.iter().map(|p| p.pn.as_str()).collect();
    assert_eq!(pns, ["libfoo-alt", "libfoo"]);
    // A recipe's own name: the PN goes first.
    let set = p.providers("libfoo").unwrap();
    assert_eq!(set.reason, ProviderReason::Only);
    assert_eq!(set.providers[0].pn, "libfoo");
}

// ---- overrides ----

async fn ovr_ok(f: &Fixture, a: overrides::Args) -> String {
    overrides::run(&f.ctx, a).await.unwrap()
}

#[tokio::test]
async fn overrides_chain_two_machines() {
    let f = fixture();
    let q = ovr_ok(&f, oargs(Some("hello"), None, overrides::Show::Chain)).await;
    insta::assert_snapshot!("overrides_chain_hello_qemux86", q);
    let b = ovr_ok(
        &f,
        oargs(Some("hello"), Some("boardx"), overrides::Show::Chain),
    )
    .await;
    insta::assert_snapshot!("overrides_chain_hello_boardx", b);
}

#[tokio::test]
async fn overrides_conditional_hello() {
    let f = fixture();
    let out = ovr_ok(&f, oargs(Some("hello"), None, overrides::Show::Conditional)).await;
    insta::assert_snapshot!("overrides_conditional_hello", out);
}

#[tokio::test]
async fn overrides_dead_reports_typo_with_suggestion() {
    let f = fixture();
    let out = ovr_ok(&f, oargs(Some("hello"), None, overrides::Show::Dead)).await;
    insta::assert_snapshot!("overrides_dead_hello", out);
    let row = out.lines().find(|l| l.starts_with("boradx\t")).unwrap();
    assert!(row.contains("boardx (distance 2)"), "{row}");
    assert!(
        row.contains("meta-vendor:recipes-core/hello/hello_%.bbappend:9"),
        "{row}"
    );
    // boardx is a known machine even when evaluating for qemux86.
    assert!(!out.lines().any(|l| l.starts_with("boardx\t")), "{out}");
}
