//! `hygiene/parse-error` by place (lint-dynamic-data): a true syntax error in a recipe, a
//! bbappend, a class, an included `.inc`, a `layer.conf` and a recipe only an inactive
//! `BBFILES_DYNAMIC` entry collects is reported; the data files beside a recipe of an
//! absent `BBFILES_DYNAMIC` collection (meta-oe's `dynamic-layers/`) and files no reference
//! can reach that do not parse are not. On the fixture layer
//! `tests/fixtures/lint/hygiene/parse-error/places-layer/`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::index::{Index, query};
use sous_chef::lint;
use sous_chef::workspace::Workspace;

const LAYER: &str = "places-layer";

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    config: Config,
    ws: Workspace,
    index: Index,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir(
        &fixture_path(&format!("lint/hygiene/parse-error/{LAYER}")),
        &root.join(LAYER),
    );
    let root = fs::canonicalize(root).unwrap();
    let mut config = Config {
        layers: vec![PathBuf::from(LAYER)],
        writable_layers: vec![LAYER.to_string()],
        release: Some("wrynose".into()),
        work_dir: root.clone(),
        ..Config::default()
    };
    config.bitbake.enabled = false;
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Env {
        _tmp: tmp,
        _cache: cache,
        config,
        ws,
        index,
    }
}

fn reason(env: &Env, rel: &str) -> Option<String> {
    let f = query::file_by_path(&env.index, LAYER, Path::new(rel))
        .unwrap()
        .unwrap_or_else(|| panic!("{rel} not indexed"));
    query::not_collected(&env.index, f.id).unwrap()
}

/// `pointer` of every `hygiene/parse-error` finding of `lint --all`.
fn parse_errors(env: &Env) -> Vec<String> {
    let report = lint::run(
        &env.ws,
        &env.index,
        &env.config,
        &lint::Target::Workspace,
        &lint::Options {
            rules: vec!["hygiene/parse-error".into()],
            all: true,
            ..lint::Options::default()
        },
    )
    .unwrap();
    report
        .findings
        .iter()
        .map(|f| {
            assert_eq!(f.rule, "hygiene/parse-error");
            f.loc.to_string()
        })
        .collect()
}

#[test]
fn true_parse_errors_are_reported_and_data_files_are_not() {
    let env = env();
    let got = parse_errors(&env);
    assert_eq!(
        got,
        [
            "places-layer:classes/broken.bbclass:2",
            "places-layer:conf/layer.conf:17",
            "places-layer:dynamic-layers/absent/recipes-x/oops/oops_1.0.bb:3",
            "places-layer:recipes-app/broken/broken.inc:2",
            "places-layer:recipes-app/broken/broken_%.bbappend:2",
            "places-layer:recipes-app/broken/broken_1.0.bb:5",
            // Reached by `require ${@'dyn-extra.inc'}` from the recipe next to it.
            "places-layer:recipes-app/dyn/dyn-extra.inc:2",
        ],
        "{got:#?}"
    );
}

#[test]
fn files_beside_a_recipe_of_an_absent_dynamic_collection_are_data() {
    let env = env();
    // The recipe is not collected: only an inactive BBFILES_DYNAMIC entry matches it.
    assert!(
        reason(&env, "dynamic-layers/absent/recipes-x/lirc/lirc_0.10.bb")
            .unwrap()
            .contains("BBFILES_DYNAMIC"),
    );
    // Its <pn>/, files/ and <pn>-<pv>/ directories (FILESPATH) hold data.
    for data in [
        "dynamic-layers/absent/recipes-x/lirc/lirc/lircd.conf",
        "dynamic-layers/absent/recipes-x/lirc/files/defaults.conf",
        "dynamic-layers/absent/recipes-x/lirc/lirc-0.10/old.conf",
    ] {
        let r = reason(&env, data).unwrap_or_else(|| panic!("{data} not marked"));
        assert!(r.starts_with("not BitBake metadata"), "{data}: {r}");
    }
}

#[test]
fn an_unreachable_file_that_does_not_parse_is_data() {
    let env = env();
    for data in ["tools/stray.conf", "tools/stray.inc"] {
        let r = reason(&env, data).unwrap_or_else(|| panic!("{data} not marked"));
        assert!(r.starts_with("not BitBake metadata"), "{data}: {r}");
    }
    // Metadata: included, a class in classes/, beside a python-expression require, and
    // the layer's conf/layer.conf (which BitBake reads by convention).
    for meta in [
        "recipes-app/broken/broken.inc",
        "classes/broken.bbclass",
        "recipes-app/dyn/dyn-extra.inc",
        "conf/layer.conf",
    ] {
        assert_eq!(reason(&env, meta), None, "{meta}");
    }
}
