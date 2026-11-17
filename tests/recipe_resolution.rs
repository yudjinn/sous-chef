//! A bare recipe name resolves to the same file in `recipe`, `item`, `search` and the
//! `bbappend` generator: provider selection (`PREFERRED_VERSION` here), not layer priority
//! and version alone.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod scaffold_gen;

use std::fs;

use scaffold_gen::{Env, request};
use serde_json::json;
use sous_chef::bitbake::JobTable;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{item, recipe, search};
use sous_chef::scaffold::Kind;

/// The fixture workspace with `PREFERRED_VERSION_hello = "1.0"` in local.conf: the older
/// of hello's two versions, which layer priority and version alone would not pick.
fn env() -> Env {
    Env::edited(|root| {
        let conf = root.join("build/conf/local.conf");
        let mut text = fs::read_to_string(&conf).unwrap();
        text.push_str("PREFERRED_VERSION_hello = \"1.0\"\n");
        fs::write(&conf, text).unwrap();
    })
}

fn ctx(env: &Env) -> (tempfile::TempDir, Ctx) {
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &env.ws).unwrap();
    index.refresh(&env.ws).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config: env.cfg.clone(),
        workspace: env.ws.clone(),
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    (cache, ctx)
}

#[tokio::test]
async fn preferred_version_is_honoured_everywhere() {
    let mut env = env();
    let (_cache, ctx) = ctx(&env);

    let summary = recipe::run(
        &ctx,
        recipe::Args {
            name: "hello".into(),
            version: None,
            show: recipe::Show::Summary,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        summary.contains("recipe\thello 1.0\tmeta-core:recipes-core/hello/hello_1.0.bb\n"),
        "{summary}"
    );
    assert!(
        summary.contains("1.0 (PREFERRED_VERSION_hello = \"1.0\")"),
        "{summary}"
    );

    // item: LIC_FILES_CHKSUM is in both .bb files; the tree is hello_1.0.bb's.
    let text = item::run(
        &ctx,
        item::Args {
            loc: None,
            recipe: Some("hello".into()),
            file: None,
            name: Some("LIC_FILES_CHKSUM".into()),
            context: 0,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        text.starts_with("meta-core:recipes-core/hello/hello_1.0.bb:3 sha256="),
        "{text}"
    );

    // search: the recipe filter is hello_1.0.bb's tree (meta-mine's hello_1.1.bbappend does
    // not apply to it).
    let hits = search::run(
        &ctx,
        search::Args {
            query: "LIC_FILES_CHKSUM".into(),
            mode: search::Mode::Name,
            kind: None,
            layer: None,
            file_kind: None,
            recipe: Some("hello".into()),
            limit: None,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        hits.contains("meta-core:recipes-core/hello/hello_1.0.bb:3\t"),
        "{hits}"
    );
    assert!(!hits.contains("hello_1.1.bb"), "{hits}");
    let hits = search::run(
        &ctx,
        search::Args {
            query: "EXTRA_OECONF".into(),
            mode: search::Mode::Name,
            kind: None,
            layer: Some("meta-mine".into()),
            file_kind: None,
            recipe: Some("hello".into()),
            limit: None,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(hits.starts_with("loc\tkind\titem\tsnippet\nno hits"), "{hits}");

    // The bbappend generator: an exact-version append names hello_1.0, and meta-mine's
    // existing hello_1.1.bbappend is not the one modified.
    let req = request(
        Kind::Bbappend,
        "hello",
        None,
        json!({"version": "exact", "set": {"EXTRA_OECONF:append": "--with-x"}}),
    );
    let text = env.show(&req);
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/hello/hello_1.0.bbappend\n"),
        "{text}"
    );
    // With no version option the default (wildcard) is created, since meta-mine has no
    // bbappend that applies to hello 1.0; with hello 1.1 it would modify hello_1.1.bbappend.
    let req = request(
        Kind::Bbappend,
        "hello",
        None,
        json!({"set": {"EXTRA_OECONF:append": "--with-x"}}),
    );
    let text = env.show(&req);
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/hello/hello_%.bbappend\n"),
        "{text}"
    );
}

/// Without the setting, all four agree on hello 1.1 (the snapshot tests cover the output).
#[tokio::test]
async fn default_is_layer_priority_and_version() {
    let mut env = Env::new();
    let (_cache, ctx) = ctx(&env);
    let text = item::run(
        &ctx,
        item::Args {
            loc: None,
            recipe: Some("hello".into()),
            file: None,
            name: Some("LIC_FILES_CHKSUM".into()),
            context: 0,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        text.starts_with("meta-core:recipes-core/hello/hello_1.1.bb:3 sha256="),
        "{text}"
    );
    let req = request(
        Kind::Bbappend,
        "hello",
        None,
        json!({"set": {"EXTRA_OECONF:append": "--with-x"}}),
    );
    let text = env.show(&req);
    assert!(
        text.starts_with("--- modify meta-mine:recipes-core/hello/hello_1.1.bbappend\n"),
        "{text}"
    );
}
