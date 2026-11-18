//! T2.6: the `recipe` and `item` tools on the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;

use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::item::{self, CAP_BYTES};
use sous_chef::mcp::tools::recipe::{self, Show};
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

fn rargs(name: &str, show: Show) -> recipe::Args {
    recipe::Args {
        mc: None,
        name: name.into(),
        version: None,
        show,
        budget_tokens: None,
        format: None,
    }
}

fn iargs() -> item::Args {
    item::Args {
        loc: None,
        recipe: None,
        file: None,
        name: None,
        context: 0,
        budget_tokens: None,
        format: None,
    }
}

async fn recipe_ok(f: &Fixture, a: recipe::Args) -> String {
    recipe::run(&f.ctx, a).await.unwrap()
}

async fn item_ok(f: &Fixture, a: item::Args) -> String {
    item::run(&f.ctx, a).await.unwrap()
}

fn lines_of(path: &Path, a: usize, b: usize) -> String {
    let bytes = std::fs::read(path).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    text.split_inclusive('\n')
        .skip(a - 1)
        .take(b - a + 1)
        .collect()
}

// ---- recipe ----

#[tokio::test]
async fn recipe_summary_hello() {
    let f = fixture();
    let out = recipe_ok(&f, rargs("hello", Show::Summary)).await;
    insta::assert_snapshot!("recipe_summary_hello", out);
    assert!(
        out.contains("1.1 (preferred by layer priority and version)"),
        "{out}"
    );
    // Application order: vendor (6) before mine (10).
    let v = out
        .find("meta-vendor:recipes-core/hello/hello_%.bbappend")
        .unwrap();
    let m = out
        .find("meta-mine:recipes-core/hello/hello_1.1.bbappend")
        .unwrap();
    assert!(v < m, "{out}");
    // Four SRC_URI entries for qemux86: the boardx append is inactive (evaluated values).
    assert!(
        out.contains("SRC_URI\t4 entries from 4 applied operations; 1 inactive operation"),
        "{out}"
    );
    assert!(
        out.contains("key values are evaluated for MACHINE=qemux86"),
        "{out}"
    );
}

#[tokio::test]
async fn recipe_summary_by_version_pn_pv_and_pointer() {
    let f = fixture();
    let by_version = recipe_ok(
        &f,
        recipe::Args {
            version: Some("1.0".into()),
            ..rargs("hello", Show::Summary)
        },
    )
    .await;
    insta::assert_snapshot!("recipe_summary_hello_1_0", by_version);
    assert!(by_version.contains("1.0 (selected by version=1.0)"));
    // Only the vendor append applies to 1.0.
    assert!(!by_version.contains("hello_1.1.bbappend"));
    let by_pn_pv = recipe_ok(&f, rargs("hello_1.0", Show::Summary)).await;
    assert!(by_pn_pv.contains("recipe\thello 1.0\t"), "{by_pn_pv}");
    let by_ptr = recipe_ok(
        &f,
        rargs("meta-core:recipes-core/hello/hello_1.0.bb", Show::Summary),
    )
    .await;
    assert!(by_ptr.contains("recipe\thello 1.0\t"), "{by_ptr}");
    // A pointer to a bbappend means the recipe it applies to.
    let by_append = recipe_ok(
        &f,
        rargs(
            "meta-mine:recipes-core/hello/hello_1.1.bbappend",
            Show::Summary,
        ),
    )
    .await;
    assert!(by_append.contains("recipe\thello 1.1\t"), "{by_append}");
}

#[tokio::test]
async fn recipe_summary_app_python_note() {
    let f = fixture();
    let out = recipe_ok(&f, rargs("app", Show::Summary)).await;
    insta::assert_snapshot!("recipe_summary_app", out);
    assert!(out.contains("LICENSE\tCLOSED\tmeta-mine:recipes-app/app/app_0.1.bb:2"));
}

#[tokio::test]
async fn recipe_tree_hello() {
    let f = fixture();
    let out = recipe_ok(&f, rargs("hello", Show::Tree)).await;
    insta::assert_snapshot!("recipe_tree_hello", out);
    assert!(out.contains(
        "1\trequire\thello.inc\tmeta-core:recipes-core/hello/hello.inc\tmeta-core:recipes-core/hello/hello_1.1.bb:1"
    ));
}

#[tokio::test]
async fn recipe_tree_shows_unresolved_targets() {
    let (_tmp, root) = common::temp_ws();
    let bb = root.join("meta-mine/recipes-app/app/app_0.1.bb");
    let mut text = std::fs::read_to_string(&bb).unwrap();
    text.push_str("require missing.inc\ninherit nosuchclass\n");
    std::fs::write(&bb, text).unwrap();
    let f = fixture_with(common::ws_config(&root));
    let out = recipe_ok(&f, rargs("app", Show::Tree)).await;
    assert!(out.contains("1\trequire\tmissing.inc\t-\t"), "{out}");
    assert!(out.contains("1\tinherit\tnosuchclass\t-\t"), "{out}");
    assert!(out.contains("unresolved"), "{out}");
    assert!(out.contains("2 targets not resolved statically"), "{out}");
}

#[tokio::test]
async fn recipe_outline_hello_and_app() {
    let f = fixture();
    let out = recipe_ok(&f, rargs("hello", Show::Outline)).await;
    insta::assert_snapshot!("recipe_outline_hello", out);
    let out = recipe_ok(&f, rargs("app", Show::Outline)).await;
    insta::assert_snapshot!("recipe_outline_app", out);
    assert!(
        out.contains("do_install\tfakeroot shell\tbb\tmeta-mine:recipes-app/app/app_0.1.bb:25-29")
    );
    assert!(out.contains("__anonymous\tpython\tbb\tmeta-mine:recipes-app/app/app_0.1.bb:13-16"));
}

#[tokio::test]
async fn recipe_json_and_budget() {
    let f = fixture();
    let out = recipe_ok(
        &f,
        recipe::Args {
            format: Some(Format::Json),
            ..rargs("hello", Show::Tree)
        },
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["header"][0], "depth");
    assert!(v["rows"].as_array().unwrap().len() > 3);
    let small = recipe_ok(
        &f,
        recipe::Args {
            budget_tokens: Some(60),
            ..rargs("hello", Show::Tree)
        },
    )
    .await;
    assert!(small.contains("more over budget_tokens"), "{small}");
}

#[tokio::test]
async fn recipe_errors_list_candidates() {
    let f = fixture();
    let e = recipe::run(&f.ctx, rargs("libf", Show::Summary))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("ambiguous"), "{e}");
    assert!(e.contains("libfoo") && e.contains("libfoo-alt"), "{e}");
    // A bbappend that applies to several recipes is ambiguous.
    let e = recipe::run(
        &f.ctx,
        rargs(
            "meta-vendor:recipes-core/hello/hello_%.bbappend",
            Show::Summary,
        ),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("hello_1.0 (meta-core:recipes-core/hello/hello_1.0.bb)"),
        "{e}"
    );
    assert!(
        e.contains("hello_1.1 (meta-core:recipes-core/hello/hello_1.1.bb)"),
        "{e}"
    );
    let e = recipe::run(
        &f.ctx,
        recipe::Args {
            version: Some("9.9".into()),
            ..rargs("hello", Show::Summary)
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("versions: 1.1, 1.0") || e.contains("versions: 1.0, 1.1"),
        "{e}"
    );
    let e = recipe::run(&f.ctx, rargs("zzz", Show::Summary))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("not found"), "{e}");
}

// ---- item ----

#[tokio::test]
async fn item_by_loc_is_byte_exact() {
    let f = fixture();
    let out = item_ok(
        &f,
        item::Args {
            loc: Some("meta-mine:recipes-app/app/app_0.1.bb:25-29".into()),
            ..iargs()
        },
    )
    .await;
    insta::assert_snapshot!("item_loc_app_do_install", out);
    let path = common::fixture_path("ws/meta-mine/recipes-app/app/app_0.1.bb");
    let (header, body) = out.split_once('\n').unwrap();
    assert!(
        header.starts_with("meta-mine:recipes-app/app/app_0.1.bb:25-29 sha256="),
        "{header}"
    );
    assert_eq!(
        header.len(),
        "meta-mine:recipes-app/app/app_0.1.bb:25-29 sha256=".len() + 12
    );
    assert_eq!(body, lines_of(&path, 25, 29));
}

#[tokio::test]
async fn item_by_recipe_and_name_function_equals_source() {
    let f = fixture();
    let out = item_ok(
        &f,
        item::Args {
            recipe: Some("app".into()),
            name: Some("do_install".into()),
            ..iargs()
        },
    )
    .await;
    let path = common::fixture_path("ws/meta-mine/recipes-app/app/app_0.1.bb");
    let (header, body) = out.split_once('\n').unwrap();
    assert!(header.starts_with("meta-mine:recipes-app/app/app_0.1.bb:25-29 "));
    assert_eq!(body, lines_of(&path, 25, 29));
    // With context.
    let out = item_ok(
        &f,
        item::Args {
            recipe: Some("app".into()),
            name: Some("do_install".into()),
            context: 1,
            ..iargs()
        },
    )
    .await;
    let (header, body) = out.split_once('\n').unwrap();
    assert!(header.starts_with("meta-mine:recipes-app/app/app_0.1.bb:24-30 "));
    assert_eq!(body, lines_of(&path, 24, 30));
}

#[tokio::test]
async fn item_several_matches_are_listed() {
    let f = fixture();
    let out = item_ok(
        &f,
        item::Args {
            recipe: Some("hello".into()),
            name: Some("do_install:append".into()),
            ..iargs()
        },
    )
    .await;
    insta::assert_snapshot!("item_hello_do_install_append", out);
    assert!(out.contains("meta-core:recipes-core/hello/hello.inc:15-18"));
    assert!(out.contains("meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10"));
    // hello 1.0 has only the .inc one: returned as text.
    let out = item_ok(
        &f,
        item::Args {
            recipe: Some("hello_1.0".into()),
            name: Some("do_install".into()),
            ..iargs()
        },
    )
    .await;
    let path = common::fixture_path("ws/meta-core/recipes-core/hello/hello.inc");
    let (header, body) = out.split_once('\n').unwrap();
    assert!(
        header.starts_with("meta-core:recipes-core/hello/hello.inc:15-18 "),
        "{out}"
    );
    assert_eq!(body, lines_of(&path, 15, 18));
}

#[tokio::test]
async fn item_by_file_and_name_and_flag() {
    let f = fixture();
    let out = item_ok(
        &f,
        item::Args {
            file: Some("meta-core:recipes-core/hello/hello_1.1.bb".into()),
            name: Some("SRC_URI[sha256sum]".into()),
            ..iargs()
        },
    )
    .await;
    insta::assert_snapshot!("item_file_flag", out);
    let out = item_ok(
        &f,
        item::Args {
            file: Some("meta-core:recipes-core/hello/hello_1.1.bb".into()),
            name: Some("SRC_URI".into()),
            format: Some(Format::Json),
            ..iargs()
        },
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["loc"], "meta-core:recipes-core/hello/hello_1.1.bb:5");
    assert_eq!(v["text"], "SRC_URI += \"file://hello.patch\"\n");
    assert_eq!(v["sha256"].as_str().unwrap().len(), 12);
}

#[tokio::test]
async fn item_errors() {
    let f = fixture();
    let run_err = |a: item::Args| async { item::run(&f.ctx, a).await.unwrap_err().to_string() };
    let e = run_err(iargs()).await;
    assert!(e.contains("exactly one of"), "{e}");
    let e = run_err(item::Args {
        loc: Some("meta-mine:recipes-app/app/app_0.1.bb:200-210".into()),
        ..iargs()
    })
    .await;
    assert!(e.contains("has 34 lines"), "{e}");
    let e = run_err(item::Args {
        loc: Some("meta-mine:../../etc/passwd:1".into()),
        ..iargs()
    })
    .await;
    assert!(e.contains("not found"), "{e}");
    let e = run_err(item::Args {
        recipe: Some("app".into()),
        name: Some("do_nothing".into()),
        ..iargs()
    })
    .await;
    assert!(e.contains("not found: item do_nothing"), "{e}");
}

#[tokio::test]
async fn item_cap_and_marker() {
    let (_tmp, root) = common::temp_ws();
    let bb = root.join("meta-mine/recipes-app/big/big_1.0.bb");
    std::fs::create_dir_all(bb.parent().unwrap()).unwrap();
    let mut text = String::from("LICENSE = \"CLOSED\"\ndo_install() {\n");
    for i in 0..400 {
        text.push_str(&format!("\techo line {i:04} padding padding padding\n"));
    }
    text.push_str("}\n");
    std::fs::write(&bb, &text).unwrap();
    let f = fixture_with(common::ws_config(&root));
    let out = item_ok(
        &f,
        item::Args {
            recipe: Some("big".into()),
            name: Some("do_install".into()),
            ..iargs()
        },
    )
    .await;
    assert!(out.len() <= CAP_BYTES, "{}", out.len());
    let last = out.lines().last().unwrap();
    let (header, _) = out.split_once('\n').unwrap();
    // Header names the range returned; the marker the range cut.
    let shown: Vec<&str> = header.split(' ').next().unwrap().rsplit(':').collect();
    let range = shown[0];
    let (a, b) = range.split_once('-').unwrap();
    assert_eq!(a, "2");
    let b: u32 = b.parse().unwrap();
    assert_eq!(
        last,
        format!("+{} more lines (lines={}-403)", 403 - b, b + 1)
    );
    // The body is exactly the source lines 2..=b.
    let body: String = out
        .split_inclusive('\n')
        .skip(1)
        .take((b - 1) as usize)
        .collect();
    assert_eq!(body, lines_of(&bb, 2, b as usize));
    // A smaller budget lowers the cap.
    let small = item_ok(
        &f,
        item::Args {
            recipe: Some("big".into()),
            name: Some("do_install".into()),
            budget_tokens: Some(100),
            ..iargs()
        },
    )
    .await;
    assert!(small.len() <= 400, "{}", small.len());
    assert!(small.lines().last().unwrap().starts_with('+'), "{small}");
}

#[tokio::test]
async fn item_keeps_crlf() {
    let (_tmp, root) = common::temp_ws();
    let bb = root.join("meta-mine/recipes-app/crlf/crlf_1.0.bb");
    std::fs::create_dir_all(bb.parent().unwrap()).unwrap();
    std::fs::write(
        &bb,
        "LICENSE = \"CLOSED\"\r\ndo_compile() {\r\n\ttrue\r\n}\r\n",
    )
    .unwrap();
    let f = fixture_with(common::ws_config(&root));
    let out = item_ok(
        &f,
        item::Args {
            recipe: Some("crlf".into()),
            name: Some("do_compile".into()),
            ..iargs()
        },
    )
    .await;
    let (_, body) = out.split_once('\n').unwrap();
    assert_eq!(body, "do_compile() {\r\n\ttrue\r\n}\r\n");
}
