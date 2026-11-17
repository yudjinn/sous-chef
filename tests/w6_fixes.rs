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
}

// ---------------------------------------------------------------------------------------
// Item 2: files BitBake does not collect are told about once and otherwise left alone
// ---------------------------------------------------------------------------------------

/// A `.bb` with a legacy override (an `override/legacy-syntax` error) and a tab.
const BAD_BB: &str = "LICENSE = \"CLOSED\"\nRDEPENDS_${PN} = \"x\"\nFOO = \"\t1\"\n";

fn uncollected_files(root: &Path) {
    write(root, "meta-mine/lib/templates/example.bb", BAD_BB);
    write(root, "meta-mine/recipes-app/deep/sub/deep_1.0.bb", BAD_BB);
    write(root, "meta-mine/stray/x_1.0.bbappend", BAD_BB);
}

#[test]
fn uncollected_recipe_files_get_one_finding_and_no_other_rule() {
    let fx = modified(uncollected_files);
    let found = lint_all(&fx, &[]);
    let in_file = |p: &str| -> Vec<&Finding> {
        found
            .iter()
            .filter(|f| f.loc.layer == "meta-mine" && f.loc.path == Path::new(p))
            .collect()
    };
    let t = in_file("lib/templates/example.bb");
    assert_eq!(t.len(), 1, "{t:#?}");
    assert_eq!(t[0].rule, "org/recipe-path");
    assert_eq!(t[0].severity, lint::Severity::Info);
    assert!(
        t[0].message.contains("BitBake does not collect")
            && t[0].message.contains("${LAYERDIR}/recipes-*/*/*.bb"),
        "{}",
        t[0].message
    );
    let d = in_file("recipes-app/deep/sub/deep_1.0.bb");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].rule, "org/recipe-path");
    assert_eq!(d[0].severity, lint::Severity::Warning);
    // The bbappend: no file finding; org/layer-conf reports it once, at BBFILES.
    assert!(in_file("stray/x_1.0.bbappend").is_empty(), "{found:#?}");
    let conf: Vec<&Finding> = found
        .iter()
        .filter(|f| f.rule == "org/layer-conf" && f.message.contains("stray/x_1.0.bbappend"))
        .collect();
    assert_eq!(conf.len(), 1, "{found:#?}");
    assert!(
        !found
            .iter()
            .any(|f| f.rule == "org/recipe-path" && f.message.contains("x_1.0.bbappend")),
        "{found:#?}"
    );
    let _ = query::all_files(&fx.idx).unwrap();
}

#[test]
fn a_collected_recipe_keeps_every_check() {
    let fx = modified(|r| write(r, "meta-mine/recipes-app/bad/bad_1.0.bb", BAD_BB));
    let found = lint_all(&fx, &[]);
    let rules: Vec<&str> = found
        .iter()
        .filter(|f| f.loc.path == Path::new("recipes-app/bad/bad_1.0.bb"))
        .map(|f| f.rule)
        .collect();
    assert!(rules.contains(&"override/legacy-syntax"), "{rules:?}");
}

// ---------------------------------------------------------------------------------------
// Item 3: BBFILES_DYNAMIC in the index
// ---------------------------------------------------------------------------------------

fn dynamic_layers(root: &Path) {
    append_to(
        root,
        "meta-mine/conf/layer.conf",
        "BBFILES_DYNAMIC += \"core:${LAYERDIR}/dynamic-layers/core/*/*/*.bbappend \\\n    \
         tpm2:${LAYERDIR}/dynamic-layers/tpm2/*/*/*.bbappend \\\n    \
         !tpm2:${LAYERDIR}/dynamic-layers/no-tpm2/*/*/*.bbappend \\\n    \
         !core:${LAYERDIR}/dynamic-layers/no-core/*/*/*.bbappend\"\n",
    );
    for (dir, var) in [
        ("core", "DYN_CORE"),
        ("tpm2", "DYN_TPM2"),
        ("no-tpm2", "DYN_NO_TPM2"),
        ("no-core", "DYN_NO_CORE"),
    ] {
        write(
            root,
            &format!("meta-mine/dynamic-layers/{dir}/recipes-core/hello/hello_%.bbappend"),
            &format!("{var} = \"1\"\nRDEPENDS_${{PN}} = \"x\"\n"),
        );
    }
}

#[test]
fn bbfiles_dynamic_is_folded_into_the_collection() {
    let fx = modified(dynamic_layers);
    let ds = eval(&fx, "hello");
    // Present collection, and `!` of an absent one: collected, applied to the recipe.
    assert_eq!(ds.value("DYN_CORE").map(|v| v.text).as_deref(), Some("1"));
    assert_eq!(
        ds.value("DYN_NO_TPM2").map(|v| v.text).as_deref(),
        Some("1")
    );
    // Absent collection, and `!` of a present one: not collected.
    assert!(ds.value("DYN_TPM2").is_none());
    assert!(ds.value("DYN_NO_CORE").is_none());

    let hello = query::find_recipes(&fx.idx, "hello")
        .unwrap()
        .into_iter()
        .find(|r| r.file.layer == "meta-core")
        .unwrap();
    let appends: Vec<String> = query::appends_for(&fx.idx, hello.file.id)
        .unwrap()
        .iter()
        .map(|f| f.path.display().to_string())
        .collect();
    assert!(
        appends.contains(&"dynamic-layers/core/recipes-core/hello/hello_%.bbappend".to_string())
            && appends
                .contains(&"dynamic-layers/no-tpm2/recipes-core/hello/hello_%.bbappend".into()),
        "{appends:?}"
    );
    for dir in ["tpm2", "no-core"] {
        let f = query::file_by_path(
            &fx.idx,
            "meta-mine",
            Path::new(&format!(
                "dynamic-layers/{dir}/recipes-core/hello/hello_%.bbappend"
            )),
        )
        .unwrap()
        .unwrap();
        let why = query::not_collected(&fx.idx, f.id).unwrap().unwrap();
        assert!(why.contains("BBFILES_DYNAMIC"), "{dir}: {why}");
        assert!(!appends.iter().any(|a| a.contains(&format!("/{dir}/"))));
    }

    // Lint: the collected ones are linted (a legacy override each), the others are not.
    let found = lint_all(&fx, &["override/legacy-syntax"]);
    let dirs: Vec<String> = found
        .iter()
        .filter(|f| f.loc.path.starts_with("dynamic-layers"))
        .map(|f| f.loc.path.display().to_string())
        .collect();
    assert_eq!(
        dirs,
        [
            "dynamic-layers/core/recipes-core/hello/hello_%.bbappend",
            "dynamic-layers/no-tpm2/recipes-core/hello/hello_%.bbappend"
        ]
    );
}

#[tokio::test]
async fn layers_appends_reads_bbfiles_dynamic_from_the_index() {
    use sous_chef::mcp::tools::layers;
    let fx = modified(dynamic_layers);
    let ctx = ctx_for(&fx);
    let out = layers::run(
        &ctx,
        layers::Args {
            show: layers::Show::Appends,
            budget_tokens: Some(8000),
            format: None,
        },
    )
    .await
    .unwrap();
    for want in [
        "meta-mine:dynamic-layers/core/recipes-core/hello/hello_%.bbappend\t10\tmeta-core:recipes-core/hello/hello_1.0.bb\tapplies (BBFILES_DYNAMIC)",
        "meta-mine:dynamic-layers/no-tpm2/recipes-core/hello/hello_%.bbappend\t10\tmeta-core:recipes-core/hello/hello_1.0.bb\tapplies (BBFILES_DYNAMIC)",
        "meta-mine:dynamic-layers/tpm2/recipes-core/hello/hello_%.bbappend\t10\t-\tnot collected (dynamic layer `tpm2` not present)",
        "meta-mine:dynamic-layers/no-core/recipes-core/hello/hello_%.bbappend\t10\t-\tnot collected (BBFILES_DYNAMIC `!core`: layer `core` is present)",
    ] {
        assert!(out.contains(want), "{want:?} in\n{out}");
    }
}
