//! T8.3: `recipe show=sources`, `show=tasks`, `show=packages` on the fixture workspace and
//! on temporary copies for the cases it lacks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::recipe::{self, Show};
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

struct Fixture {
    _cache: tempfile::TempDir,
    _tmp: Option<tempfile::TempDir>,
    root: Option<String>,
    ctx: Ctx,
}

fn fixture_with(config: Config, tmp: Option<tempfile::TempDir>, root: Option<String>) -> Fixture {
    let cache = tempfile::tempdir().unwrap();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    Fixture {
        _cache: cache,
        _tmp: tmp,
        root,
        ctx,
    }
}

fn fixture() -> Fixture {
    fixture_with(common::fixture_config(), None, None)
}

/// A copy of the fixture after `edit(root)`.
fn edited(edit: impl FnOnce(&Path)) -> Fixture {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let cfg = common::ws_config(&root);
    fixture_with(cfg, Some(tmp), Some(root.display().to_string()))
}

async fn show(f: &Fixture, name: &str, show: Show, budget: usize) -> String {
    let out = recipe::run(
        &f.ctx,
        recipe::Args {
            name: name.into(),
            version: None,
            show,
            budget_tokens: Some(budget),
            format: None,
        },
    )
    .await
    .unwrap();
    match &f.root {
        Some(r) => out.replace(r.as_str(), "<ws>"),
        None => out,
    }
}

fn append(p: &Path, text: &str) {
    let mut s = fs::read_to_string(p).unwrap_or_default();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

// ---- sources ----

#[tokio::test]
async fn sources_hello() {
    let f = fixture();
    let out = show(&f, "hello", Show::Sources, 4000).await;
    insta::assert_snapshot!("recipe_sources_hello", out);
    assert!(out.contains("file://hello.patch\tfile (patch)\tactive\tmeta-core:recipes-core/hello/files/hello.patch\tUpstream-Status: Pending\tmeta-core:recipes-core/hello/hello_1.1.bb:5"), "{out}");
    assert!(out.contains("file://boardx.cfg\tfile\tinactive (override boardx not active)\tmeta-vendor:recipes-core/hello/hello/boardx.cfg"), "{out}");
    assert!(out.contains("reconstructed"), "{out}");
}

#[tokio::test]
async fn sources_json_and_budget() {
    let f = fixture();
    let out = recipe::run(
        &f.ctx,
        recipe::Args {
            name: "hello".into(),
            version: None,
            show: Show::Sources,
            budget_tokens: Some(4000),
            format: Some(Format::Json),
        },
    )
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["rows"].as_array().unwrap().len(), 5, "{out}");
    let small = show(&f, "hello", Show::Sources, 300).await;
    assert!(small.contains("more over budget_tokens"), "{small}");
}

#[tokio::test]
async fn sources_missing_file_and_patch_headers() {
    let f = edited(|root| {
        let dir = root.join("meta-mine/recipes-core/hello/hello");
        fs::write(
            dir.join("bad.patch"),
            "Subject: x\nupstream status: maybe\n---\n",
        )
        .unwrap();
        fs::write(dir.join("none.diff"), "--- a\n+++ b\n").unwrap();
        fs::create_dir_all(dir.join("cfgdir")).unwrap();
        append(
            &root.join("meta-mine/recipes-core/hello/hello_1.1.bbappend"),
            "SRC_URI += \"file://bad.patch file://none.diff file://cfgdir file://nowhere.cfg file://keep.patch;apply=no git://example.org/x.git;branch=main\"\nSRC_URI:remove = \"file://vendor.patch\"\n",
        );
    });
    let out = show(&f, "hello", Show::Sources, 8000).await;
    insta::assert_snapshot!("recipe_sources_missing", out);
    assert!(out.contains("malformed: upstream status: maybe"), "{out}");
    assert!(out.contains("missing Upstream-Status"), "{out}");
    assert!(
        out.contains("file://nowhere.cfg\tsearched\t#1\tmeta-mine:recipes-core/hello/hello/"),
        "{out}"
    );
    assert!(out.contains("removed by :remove"), "{out}");
    assert!(out.contains("(directory)"), "{out}");
}

#[tokio::test]
async fn sources_filesoverrides_order() {
    // FILESOVERRIDES as OE-Core sets it: override directories are tried first, last first.
    let f = edited(|root| {
        append(
            &root.join("meta-core/conf/bitbake.conf"),
            "FILESOVERRIDES = \"${TRANSLATED_TARGET_ARCH}:${MACHINEOVERRIDES}:${DISTROOVERRIDES}\"\n",
        );
        let d = root.join("meta-core/recipes-core/hello/files/qemux86");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("hello.patch"), "Upstream-Status: Backport [x]\n").unwrap();
        append(
            &root.join("meta-core/recipes-core/hello/hello_1.1.bb"),
            "SRC_URI += \"file://absent.cfg\"\n",
        );
    });
    let out = show(&f, "hello", Show::Sources, 8000).await;
    insta::assert_snapshot!("recipe_sources_overrides", out);
    assert!(
        out.contains(
            "meta-core:recipes-core/hello/files/qemux86/hello.patch\tUpstream-Status: Backport [x]"
        ),
        "{out}"
    );
}

#[tokio::test]
async fn sources_app_python_warning() {
    let f = fixture();
    let out = show(&f, "app", Show::Sources, 4000).await;
    insta::assert_snapshot!("recipe_sources_app", out);
    assert!(
        out.contains("meta-mine:recipes-app/app/files/app.c"),
        "{out}"
    );
}

// ---- tasks ----

#[tokio::test]
async fn tasks_hello() {
    let f = fixture();
    let out = show(&f, "hello", Show::Tasks, 4000).await;
    insta::assert_snapshot!("recipe_tasks_hello", out);
    assert!(out.contains("do_autoreconf\t-\tdo_patch\t"), "{out}");
}

#[tokio::test]
async fn tasks_app() {
    let f = fixture();
    let out = show(&f, "app", Show::Tasks, 4000).await;
    insta::assert_snapshot!("recipe_tasks_app", out);
    assert!(out.contains("do_report\t-\tdo_compile\t"), "{out}");
    assert!(out.contains("anonymous python"), "{out}");
}

#[tokio::test]
async fn tasks_deltask_and_flags() {
    let f = edited(|root| {
        append(
            &root.join("meta-mine/recipes-core/hello/hello_1.1.bbappend"),
            "deltask do_autoreconf\naddtask extra after do_install before do_build\ndo_extra[deps] = \"do_fetch\"\ndo_extra() {\n\t:\n}\n",
        );
    });
    let out = show(&f, "hello", Show::Tasks, 4000).await;
    insta::assert_snapshot!("recipe_tasks_deltask", out);
    assert!(out.contains("do_autoreconf\tdeleted\t"), "{out}");
    assert!(out.contains("do_configure\t-\tdo_patch\t"), "{out}");
    assert!(out.contains("do_extra[deps] is assigned at"), "{out}");
}

// ---- packages ----

#[tokio::test]
async fn packages_hello() {
    let f = fixture();
    let out = show(&f, "hello", Show::Packages, 4000).await;
    insta::assert_snapshot!("recipe_packages_hello", out);
    assert!(out.contains("hello\tFILES\t/usr/bin/* /usr/sbin/* /usr/lib/lib*.so.* /etc/hello.conf\tmeta-core:conf/bitbake.conf:"), "{out}");
}

#[tokio::test]
async fn packages_libfoo_and_dynamic() {
    let f = fixture();
    let out = show(&f, "libfoo", Show::Packages, 4000).await;
    insta::assert_snapshot!("recipe_packages_libfoo", out);
    assert!(out.contains("libfoo-dev\tRDEPENDS\tlibfoo\t"), "{out}");

    let f = edited(|root| {
        append(
            &root.join("meta-mine/recipes-app/app/app_0.1.bb"),
            "PACKAGES_DYNAMIC = \"^app-plugin-.*\"\nRPROVIDES:${PN} = \"app-bin\"\nPACKAGES =+ \"${@'app-extra' if True else ''}\"\n",
        );
    });
    let out = show(&f, "app", Show::Packages, 4000).await;
    insta::assert_snapshot!("recipe_packages_dynamic", out);
    assert!(
        out.contains("PACKAGES_DYNAMIC names packages created by python"),
        "{out}"
    );
    assert!(out.contains("app\tRPROVIDES\tapp-bin\t"), "{out}");
    // `${@'app-extra' if True else ''}` is in the evaluated subset: the package is known.
    assert!(!out.contains("depends on python, not evaluated"), "{out}");
    assert!(out.contains("app-extra\tFILES\t"), "{out}");
}
