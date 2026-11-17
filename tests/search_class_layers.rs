//! T2.7: the `search`, `class` and `layers` tools on the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::class;
use sous_chef::mcp::tools::layers;
use sous_chef::mcp::tools::search::{self, Mode};
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

fn sargs(query: &str, mode: Mode) -> search::Args {
    search::Args {
        query: query.into(),
        mode,
        kind: None,
        layer: None,
        file_kind: None,
        recipe: None,
        limit: None,
        budget_tokens: None,
        format: None,
    }
}

async fn search_ok(f: &Fixture, a: search::Args) -> String {
    search::run(&f.ctx, a).await.unwrap()
}

async fn class_ok(f: &Fixture, name: &str, show: class::Show) -> String {
    class::run(
        &f.ctx,
        class::Args {
            name: name.into(),
            show,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap()
}

async fn layers_ok(f: &Fixture, show: layers::Show) -> String {
    layers::run(
        &f.ctx,
        layers::Args {
            show,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap()
}

// ---- search ----

#[tokio::test]
async fn search_name_mode() {
    let f = fixture();
    let out = search_ok(&f, sargs("do_install", Mode::Name)).await;
    insta::assert_snapshot!("search_name_do_install", out);
    let out = search_ok(
        &f,
        search::Args {
            kind: Some(vec!["function".into()]),
            layer: Some("meta-mine".into()),
            ..sargs("do_", Mode::Name)
        },
    )
    .await;
    insta::assert_snapshot!("search_name_filtered", out);
    assert!(
        out.lines().skip(1).all(|l| l.starts_with("meta-mine:")),
        "{out}"
    );
}

#[tokio::test]
async fn search_name_task_alias_and_file_kind() {
    let f = fixture();
    let out = search_ok(
        &f,
        search::Args {
            kind: Some(vec!["task".into()]),
            file_kind: Some("bbclass".into()),
            ..sargs("autoreconf", Mode::Name)
        },
    )
    .await;
    assert_eq!(
        out,
        "loc\tkind\titem\tsnippet\n\
         meta-core:classes-recipe/autotoolsish.bbclass:16\taddtask\t[autoreconf 16]\taddtask autoreconf after do_patch before do_configure\n"
    );
}

#[tokio::test]
async fn search_fts_mode() {
    let f = fixture();
    let out = search_ok(&f, sargs("sysconfdir", Mode::Fts)).await;
    insta::assert_snapshot!("search_fts_sysconfdir", out);
    let e = search::run(&f.ctx, sargs("\"unterminated", Mode::Fts))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("full-text"), "{e}");
}

#[tokio::test]
async fn search_regex_hit_inside_do_install_append() {
    let f = fixture();
    let out = search_ok(&f, sargs(r"hello\.readme", Mode::Regex)).await;
    insta::assert_snapshot!("search_regex_readme", out);
    assert!(
        out.contains(
            "meta-mine:recipes-core/hello/hello_1.1.bbappend:9\tfunction\t[do_install:append 8-10]\t"
        ),
        "{out}"
    );
}

#[tokio::test]
async fn search_regex_filters_and_limit() {
    let f = fixture();
    // Restricted to hello 1.0's tree: hello.inc and the vendor append, not the mine append.
    let out = search_ok(
        &f,
        search::Args {
            recipe: Some("hello_1.0".into()),
            ..sargs(r"sysconfdir", Mode::Regex)
        },
    )
    .await;
    insta::assert_snapshot!("search_regex_recipe_hello_1_0", out);
    assert!(!out.contains("meta-mine"), "{out}");
    // kind filters on the enclosing statement.
    let out = search_ok(
        &f,
        search::Args {
            kind: Some(vec!["function".into()]),
            ..sargs(r"install", Mode::Regex)
        },
    )
    .await;
    assert!(
        out.lines()
            .skip(1)
            .all(|l| l.split('\t').nth(1) == Some("function")),
        "{out}"
    );
    let out = search_ok(
        &f,
        search::Args {
            limit: Some(2),
            ..sargs(r"=", Mode::Regex)
        },
    )
    .await;
    assert_eq!(out.lines().count(), 4, "{out}");
    assert!(
        out.lines().last().unwrap().ends_with("more (limit=2)"),
        "{out}"
    );
    let small = search_ok(
        &f,
        search::Args {
            budget_tokens: Some(50),
            ..sargs(r"=", Mode::Regex)
        },
    )
    .await;
    assert!(small.contains("more over budget_tokens"), "{small}");
    let json = search_ok(
        &f,
        search::Args {
            format: Some(Format::Json),
            limit: Some(3),
            ..sargs(r"=", Mode::Regex)
        },
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["limit"], 3);
    assert_eq!(v["rows"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn search_errors() {
    let f = fixture();
    let err = |a| async { search::run(&f.ctx, a).await.unwrap_err().to_string() };
    assert!(err(sargs("(", Mode::Regex)).await.contains("bad regex"));
    assert!(
        err(search::Args {
            kind: Some(vec!["bogus".into()]),
            ..sargs("x", Mode::Name)
        })
        .await
        .contains("unknown kind")
    );
    assert!(
        err(search::Args {
            layer: Some("meta-nope".into()),
            ..sargs("x", Mode::Name)
        })
        .await
        .contains("meta-core")
    );
    assert!(err(sargs("  ", Mode::Name)).await.contains("empty"));
}

// ---- class ----

#[tokio::test]
async fn class_summary() {
    let f = fixture();
    let out = class_ok(&f, "autotoolsish", class::Show::Summary).await;
    insta::assert_snapshot!("class_summary_autotoolsish", out);
    assert!(out.contains("interface\tEXTRA_OECONF ?= \"\""), "{out}");
    let out = class_ok(&f, "sanityish", class::Show::Summary).await;
    insta::assert_snapshot!("class_summary_sanityish", out);
    assert!(out.contains("wins (INHERIT)"), "{out}");
}

#[tokio::test]
async fn class_summary_shows_shadowing() {
    let (_tmp, root) = common::temp_ws();
    let src = root.join("meta-core/classes-recipe/autotoolsish.bbclass");
    let dst = root.join("meta-mine/classes-recipe/autotoolsish.bbclass");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dst).unwrap();
    let f = fixture_with(common::ws_config(&root));
    let out = class_ok(&f, "autotoolsish", class::Show::Summary).await;
    // BBPATH order: meta-core comes before meta-mine, so meta-core's copy wins.
    let lines: Vec<&str> = out.lines().filter(|l| l.starts_with("file\t")).collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("wins (inherit), meta-core"), "{out}");
    assert!(lines[1].contains("shadowed, meta-mine"), "{out}");
}

#[tokio::test]
async fn class_users_and_outline() {
    let f = fixture();
    let out = class_ok(&f, "autotoolsish", class::Show::Users).await;
    insta::assert_snapshot!("class_users_autotoolsish", out);
    assert!(
        out.contains("1\tinc\tmeta-core:recipes-core/hello/hello.inc\t"),
        "{out}"
    );
    assert!(out.contains("hello_1.1.bb"), "{out}");
    let out = class_ok(&f, "sanityish", class::Show::Users).await;
    assert!(
        out.contains("1\tINHERIT\tmeta-core:conf/distro/testdistro.conf\t"),
        "{out}"
    );
    let out = class_ok(&f, "autotoolsish", class::Show::Outline).await;
    insta::assert_snapshot!("class_outline_autotoolsish", out);
    let out = class_ok(&f, "base", class::Show::Outline).await;
    insta::assert_snapshot!("class_outline_base", out);
}

#[tokio::test]
async fn class_errors() {
    let f = fixture();
    let e = class::run(
        &f.ctx,
        class::Args {
            name: "autotools".into(),
            show: class::Show::Summary,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("candidates: autotoolsish"), "{e}");
}

// ---- layers ----

#[tokio::test]
async fn layers_list() {
    let f = fixture();
    let out = layers_ok(&f, layers::Show::List).await;
    insta::assert_snapshot!("layers_list", out);
}

#[tokio::test]
async fn layers_appends() {
    let f = fixture();
    let out = layers_ok(&f, layers::Show::Appends).await;
    insta::assert_snapshot!("layers_appends", out);
    assert!(
        out.contains("meta-mine:recipes-app/orphan/orphan_9.%.bbappend\t10\t-\tno-match"),
        "{out}"
    );
}

#[tokio::test]
async fn layers_shadowed() {
    let f = fixture();
    let out = layers_ok(&f, layers::Show::Shadowed).await;
    assert!(out.contains("no recipe or class file"), "{out}");

    let (_tmp, root) = common::temp_ws();
    let copy = |from: &str, to: &str| {
        let dst = root.join(to);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(root.join(from), dst).unwrap();
    };
    copy(
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "meta-vendor/recipes-core/libfoo/libfoo_2.0.bb",
    );
    copy(
        "meta-core/classes-recipe/autotoolsish.bbclass",
        "meta-mine/classes-recipe/autotoolsish.bbclass",
    );
    let f = fixture_with(common::ws_config(&root));
    let out = layers_ok(&f, layers::Show::Shadowed).await;
    insta::assert_snapshot!("layers_shadowed", out);
}
