//! `--as-release`, the migration report (`--migrate-to`) and `hygiene/release-removed` on
//! temporary copies of the fixture workspace.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::index::Index;
use sous_chef::lint::{self, Finding, Options, Target};
use sous_chef::release::Release;
use sous_chef::workspace::Workspace;

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    index: Index,
}

/// A copy of the fixture workspace configured for `release`; `edit(root)` runs before
/// discovery.
fn env(release: &str, edit: impl FnOnce(&Path)) -> Env {
    let (tmp, root, config, ws) = temp_ws_release(release, edit);
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Env {
        _tmp: tmp,
        _cache: cache,
        root,
        config,
        ws,
        index,
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Sets meta-mine's `LAYERSERIES_COMPAT_mine`.
fn claim(root: &Path, names: &str) {
    let p = root.join("meta-mine/conf/layer.conf");
    let text = fs::read_to_string(&p).unwrap().replace(
        "LAYERSERIES_COMPAT_mine = \"kirkstone scarthgap wrynose\"",
        &format!("LAYERSERIES_COMPAT_mine = \"{names}\""),
    );
    fs::write(p, text).unwrap();
}

fn lint_layer(e: &Env, layer: &str, rules: &[&str], options: Options) -> Vec<Finding> {
    let options = Options {
        rules: rules.iter().map(|r| r.to_string()).collect(),
        ..options
    };
    lint::run(
        &e.ws,
        &e.index,
        &e.config,
        &Target::Layer(layer.into()),
        &options,
    )
    .unwrap()
    .findings
}

fn rows(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|f| {
            format!(
                "{} {} {}: {}",
                f.severity.as_str(),
                f.rule,
                f.loc,
                f.message
            )
        })
        .collect()
}

const WORKDIR_S: &str =
    "SUMMARY = \"x\"\nLICENSE = \"CLOSED\"\nSRC_URI = \"file://x.c\"\nS = \"${WORKDIR}\"\n";

#[test]
fn as_release_adds_a_release_to_a_layer_claiming_only_scarthgap() {
    let e = env("scarthgap", |root| {
        claim(root, "scarthgap");
        write(root, "meta-mine/recipes-app/old/old_1.0.bb", WORKDIR_S);
    });
    let mine = e.ws.layer("meta-mine").unwrap();
    assert_eq!(mine.lint_releases(&e.ws), [Release::Scarthgap]);

    // Without the option: Scarthgap accepts S = "${WORKDIR}".
    let plain = lint_layer(&e, "meta-mine", &["hygiene/workdir-s"], Options::default());
    assert!(plain.is_empty(), "{:?}", rows(&plain));

    // As Wrynose too: the error names wrynose.
    let as_wrynose = Options {
        as_release: Some(Release::Wrynose),
        ..Options::default()
    };
    let found = lint_layer(&e, "meta-mine", &["hygiene/workdir-s"], as_wrynose);
    let r = rows(&found);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        r[0].starts_with("error hygiene/workdir-s meta-mine:recipes-app/old/old_1.0.bb:4: "),
        "{r:?}"
    );
    assert!(r[0].contains("wrynose"), "{r:?}");
    assert!(!r[0].contains("scarthgap"), "{r:?}");

    // --only-release with a release that accepts it: nothing.
    let only_kirkstone = Options {
        as_release: Some(Release::Kirkstone),
        only_release: true,
        ..Options::default()
    };
    assert!(lint_layer(&e, "meta-mine", &["hygiene/workdir-s"], only_kirkstone).is_empty());
}

#[test]
fn releases_with_options_adds_or_replaces() {
    use Release::*;
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap], None, false),
        [Scarthgap]
    );
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap, Wrynose], Some(Kirkstone), false),
        [Kirkstone, Scarthgap, Wrynose]
    );
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap, Wrynose], Some(Wrynose), false),
        [Scarthgap, Wrynose]
    );
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap, Wrynose], Some(Kirkstone), true),
        [Kirkstone]
    );
}

#[test]
fn as_release_on_the_cli_rejects_unknown_and_old_releases() {
    let e = env("wrynose", |_| {});
    let mut out = Vec::new();
    for (name, want) in [
        ("wrynoes", "unknown release"),
        ("dunfell", "older than kirkstone"),
    ] {
        let err = lint::cli_with(
            &e.ws,
            &e.index,
            &e.config,
            &lint::LintCliArgs {
                as_release: Some(name.into()),
                ..lint::LintCliArgs::default()
            },
            &mut out,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(want), "{err}");
        assert!(err.contains("kirkstone, langdale"), "{err}");
    }
    let _ = &e.root;
}
