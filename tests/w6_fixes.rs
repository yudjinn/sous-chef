//! Fixes for the defects left after the checks against a real Yocto workspace (wave 6 fix
//! round): each test runs on a temporary copy of the fixture workspace; the fixture itself
//! is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::eval::{self, ConfigCache, DataStore, OpKind, OpStatus, Target};
use sous_chef::index::Index;
use sous_chef::index::query;
use sous_chef::lint::{self, Finding};
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

fn replace_in(root: &Path, rel: &str, from: &str, to: &str) {
    let p = root.join(rel);
    let s = fs::read_to_string(&p).unwrap();
    assert!(s.contains(from), "{rel} has no {from:?}");
    fs::write(p, s.replace(from, to)).unwrap();
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

/// Lint with the given rules (ids), whole workspace with `--all`.
fn lint_all(fx: &Fx, rules: &[&str]) -> Vec<Finding> {
    let opts = lint::Options {
        rules: rules.iter().map(|r| r.to_string()).collect(),
        min_severity: None,
        all: true,
    };
    lint::run(&fx.ws, &fx.idx, &fx.config, &lint::Target::Workspace, &opts)
        .unwrap()
        .findings
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
// Item 1: the virtual-provider rewrite of DEPENDS is a rewrite, not an assignment
// ---------------------------------------------------------------------------------------

fn virtual_providers(root: &Path) {
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "\nBB_RECIPE_VIRTUAL_PROVIDERS = \"virtual/cross-cc\"\n\
         PREFERRED_PROVIDER_virtual/cross-cc = \"gcc-cross-${TARGET_ARCH}\"\n",
    );
    replace_in(
        root,
        "meta-mine/recipes-app/app/app_0.1.bb",
        "DEPENDS = \"libfoo\"",
        "DEPENDS = \"libfoo virtual/cross-cc\"",
    );
}

#[test]
fn virtual_provider_rewrite_keeps_the_recipe_assignment_applied() {
    let fx = modified(virtual_providers);
    let ds = eval(&fx, "app");
    assert_eq!(
        ds.value("DEPENDS").unwrap().text,
        "libfoo gcc-cross-i686",
        "the rewrite still happens"
    );
    let history = ds.history("DEPENDS");
    let own = history
        .iter()
        .find(|(op, _)| op.loc.path == Path::new("recipes-app/app/app_0.1.bb"))
        .unwrap();
    assert_eq!(own.1, OpStatus::Applied, "{history:?}");
    assert_eq!(own.0.kind, OpKind::Statement);
    let (last, st) = history.last().unwrap();
    assert_eq!(last.kind, OpKind::VirtualProviderRewrite);
    assert_eq!(*st, OpStatus::Applied);
    assert_eq!(last.loc.path, Path::new("conf/distro/testdistro.conf"));
}

#[test]
fn override_shadowed_ignores_the_virtual_provider_rewrite() {
    let fx = modified(virtual_providers);
    let found = lint_all(&fx, &["override/shadowed"]);
    assert!(
        !found.iter().any(|f| f.loc.path.ends_with("app_0.1.bb")),
        "{found:#?}"
    );
    // A real later `=` is still reported.
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "DEPENDS = \"libfoo virtual/cross-cc zlib\"\n",
        );
    });
    let found = lint_all(&fx, &["override/shadowed"]);
    assert!(
        found
            .iter()
            .any(|f| f.loc.path.ends_with("app_0.1.bb") && f.loc.lines.start == 4),
        "{found:#?}"
    );
    assert!(
        !found.iter().any(|f| f.message.contains("testdistro.conf")),
        "{found:#?}"
    );
}

#[tokio::test]
async fn var_shows_the_rewrite_as_a_rewrite() {
    use sous_chef::mcp::tools::var;
    let fx = modified(virtual_providers);
    let ctx = ctx_for(&fx);
    let out = var::run(
        &ctx,
        var::Args {
            name: "DEPENDS".into(),
            recipe: Some("app".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(8000),
            format: None,
        },
    )
    .await
    .unwrap();
    let own = out
        .lines()
        .find(|l| l.contains("meta-mine:recipes-app/app/app_0.1.bb:4"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(own.starts_with("applied\t=\t"), "{out}");
    let rewrite = out
        .lines()
        .find(|l| l.contains("testdistro.conf"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(rewrite.starts_with("applied\trewrite\t"), "{out}");
    assert!(rewrite.contains("rewritten after parsing"), "{out}");
    let _ = &fx.root;
    let _ = query::all_files(&fx.idx).unwrap();
    let _ = write;
}
