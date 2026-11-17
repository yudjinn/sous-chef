//! T1.6: release detection through `Workspace::discover`, and `Layer::lint_releases`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::config::Config;
use sous_chef::error::Error;
use sous_chef::release::{LintReach, Release, ReleaseInfo, ReleaseSource};
use sous_chef::workspace::{Workspace, layers};

use Release::{Kirkstone, Scarthgap, Wrynose};

/// A layer root with a minimal `conf/layer.conf` for collection `c`; `extra` is appended.
fn write_layer(root: &Path, c: &str, extra: &str) {
    fs::create_dir_all(root.join("conf")).unwrap();
    fs::write(
        root.join("conf/layer.conf"),
        format!(
            "BBPATH .= \":${{LAYERDIR}}\"\nBBFILE_COLLECTIONS += \"{c}\"\n\
             BBFILE_PRIORITY_{c} = \"6\"\n{extra}"
        ),
    )
    .unwrap();
}

/// Discovery over explicit layer roots (no build directory).
fn layers_cfg(work_dir: &Path, roots: &[&Path], writable: &[&str]) -> Config {
    Config {
        layers: roots.iter().map(|r| r.to_path_buf()).collect(),
        writable_layers: writable.iter().map(|s| s.to_string()).collect(),
        work_dir: work_dir.to_path_buf(),
        ..Config::default()
    }
}

#[test]
fn fixture_is_wrynose_from_corenames() {
    let ws = fixture_ws();
    assert_eq!(
        ws.release,
        ReleaseInfo {
            release: Wrynose,
            source: ReleaseSource::CoreNames,
            note: None,
        }
    );
    assert_eq!(ws.release.describe(), "wrynose (LAYERSERIES_CORENAMES)");
}

#[test]
fn fixture_lint_releases() {
    let ws = fixture_ws();
    let mine = ws.layer("meta-mine").unwrap();
    assert!(mine.writable);
    assert_eq!(mine.lint_releases(&ws), [Kirkstone, Scarthgap, Wrynose]);
    assert_eq!(
        mine.lint_releases_as(&ws, LintReach::Sweep),
        [Kirkstone, Scarthgap, Wrynose]
    );
    // Not writable: every claim as the target; the workspace release alone when swept in
    // by a whole-workspace run (there is a writable layer).
    let vendor = ws.layer("meta-vendor").unwrap();
    assert_eq!(vendor.compat, ["scarthgap", "wrynose"]);
    assert_eq!(vendor.lint_releases(&ws), [Scarthgap, Wrynose]);
    assert_eq!(vendor.lint_releases_as(&ws, LintReach::Sweep), [Wrynose]);
    assert_eq!(ws.layer("meta-core").unwrap().lint_releases(&ws), [Wrynose]);
}

#[test]
fn writable_layer_claiming_kirkstone_scarthgap_in_a_wrynose_workspace() {
    let ws = fixture_ws();
    let mut layer = ws.layer("meta-mine").unwrap().clone();
    layer.compat = vec!["kirkstone".into(), "scarthgap".into()];
    assert_eq!(layer.lint_releases(&ws), [Kirkstone, Scarthgap, Wrynose]);
    layer.writable = false;
    assert_eq!(layer.lint_releases(&ws), [Kirkstone, Scarthgap, Wrynose]);
    assert_eq!(layer.lint_releases_as(&ws, LintReach::Sweep), [Wrynose]);
}

#[test]
fn fixture_without_build_dir_still_finds_corenames() {
    // Every layer.conf under ws/, meta-legacy (dunfell) included.
    let cfg = Config {
        writable_layers: vec!["meta-mine".into()],
        work_dir: fixture_ws_root(),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert!(ws.build_dir.is_none());
    assert_eq!(ws.layers.len(), 4);
    assert_eq!(ws.release.release, Wrynose);
    assert_eq!(ws.release.source, ReleaseSource::CoreNames);
    let legacy = ws.layer("meta-legacy").unwrap();
    assert_eq!(legacy.lint_releases(&ws), [Wrynose]);
}

#[test]
fn read_corenames_matches_discovery() {
    let ws = fixture_ws();
    let roots: Vec<_> = ws.layers.iter().map(|l| l.root.clone()).collect();
    assert_eq!(
        layers::read_corenames(ws.build_dir.as_deref(), &roots).unwrap(),
        ["wrynose"]
    );
    // Without the core layer nobody sets it.
    assert!(
        layers::read_corenames(ws.build_dir.as_deref(), &roots[1..])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn config_release_overrides_corenames() {
    let (_tmp, root) = temp_ws();
    fs::write(
        root.join("sous-chef.toml"),
        "build_dir = \"build\"\nwritable_layers = [\"meta-mine\"]\nrelease = \"scarthgap\"\n\n\
         [bitbake]\nenabled = false\n",
    )
    .unwrap();
    let cfg = Config::load(&sous_chef::config::CliOverrides {
        work_dir: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.release.release, Scarthgap);
    assert_eq!(ws.release.source, ReleaseSource::Config);
    assert_eq!(ws.release.describe(), "scarthgap (sous-chef.toml)");
    // meta-mine still claims kirkstone and wrynose.
    let mine = ws.layer("meta-mine").unwrap();
    assert_eq!(mine.lint_releases(&ws), [Kirkstone, Scarthgap, Wrynose]);
    let vendor = ws.layer("meta-vendor").unwrap();
    assert_eq!(vendor.lint_releases(&ws), [Scarthgap, Wrynose]);
    assert_eq!(vendor.lint_releases_as(&ws, LintReach::Sweep), [Scarthgap]);
}

#[test]
fn config_release_typo_fails_discovery() {
    let mut cfg = fixture_config();
    cfg.release = Some("wrynoze".into());
    match Workspace::discover(&cfg) {
        Err(e @ Error::Config { .. }) => {
            let msg = e.to_string();
            assert!(
                msg.contains("wrynoze") && msg.contains("kirkstone"),
                "{msg}"
            );
            assert!(!msg.contains('\n'), "{msg}");
        }
        other => panic!("expected a configuration error, got {other:?}"),
    }
}

#[test]
fn config_release_older_than_the_floor_is_the_floor_with_a_note() {
    let mut cfg = fixture_config();
    cfg.release = Some("dunfell".into());
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.release.release, Kirkstone);
    assert_eq!(ws.release.source, ReleaseSource::Config);
    let d = ws.release.describe();
    assert!(
        d.starts_with("kirkstone (sous-chef.toml); ") && d.contains("dunfell"),
        "{d}"
    );
}

#[test]
fn no_core_layer_uses_writable_layer_compat() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("meta-a");
    let b = tmp.path().join("meta-b");
    write_layer(&a, "a", "LAYERSERIES_COMPAT_a = \"kirkstone scarthgap\"\n");
    write_layer(&b, "b", "LAYERSERIES_COMPAT_b = \"wrynose\"\n");

    // meta-a writable: its newest claim, scarthgap; meta-b's wrynose is ignored.
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&a, &b], &["meta-a"])).unwrap();
    assert_eq!(ws.release.release, Scarthgap);
    assert_eq!(ws.release.source, ReleaseSource::LayerCompat);
    assert_eq!(ws.release.describe(), "scarthgap (LAYERSERIES_COMPAT)");
    assert_eq!(
        ws.layer("meta-a").unwrap().lint_releases(&ws),
        [Kirkstone, Scarthgap]
    );

    // Nothing writable: every layer counts.
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&a, &b], &[])).unwrap();
    assert_eq!(ws.release.release, Wrynose);
    assert_eq!(ws.release.source, ReleaseSource::LayerCompat);
}

#[test]
fn nothing_to_go_on_is_assumed_wrynose() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("meta-a");
    write_layer(&a, "a", "");
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&a], &["meta-a"])).unwrap();
    assert_eq!(ws.release, ReleaseInfo::default());
    assert_eq!(ws.release.describe(), "wrynose (assumed)");
    assert_eq!(ws.layer("meta-a").unwrap().lint_releases(&ws), [Wrynose]);

    // Only names older than the floor: still assumed.
    write_layer(&a, "a", "LAYERSERIES_COMPAT_a = \"dunfell\"\n");
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&a], &["meta-a"])).unwrap();
    assert_eq!(ws.release.source, ReleaseSource::Assumed);
}

#[test]
fn corenames_newer_than_the_table_and_older_than_the_floor() {
    let tmp = tempfile::tempdir().unwrap();
    let core = tmp.path().join("meta");
    let mine = tmp.path().join("meta-mine");
    write_layer(&mine, "mine", "LAYERSERIES_COMPAT_mine = \"scarthgap\"\n");

    write_layer(&core, "core", "LAYERSERIES_CORENAMES = \"zzyzx\"\n");
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&core, &mine], &["meta-mine"])).unwrap();
    assert_eq!(ws.release.release, Release::NEWEST);
    assert_eq!(ws.release.source, ReleaseSource::CoreNames);
    let note = ws.release.note.clone().unwrap();
    assert!(note.contains("zzyzx") && note.contains("wrynose"), "{note}");

    write_layer(&core, "core", "LAYERSERIES_CORENAMES = \"dunfell\"\n");
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&core, &mine], &["meta-mine"])).unwrap();
    assert_eq!(ws.release.release, Kirkstone);
    assert_eq!(ws.release.source, ReleaseSource::CoreNames);
    assert!(ws.release.note.unwrap().contains("older"));

    // Corenames come before the writable layer's compat.
    write_layer(&core, "core", "LAYERSERIES_CORENAMES = \"nanbield\"\n");
    let ws = Workspace::discover(&layers_cfg(tmp.path(), &[&core, &mine], &["meta-mine"])).unwrap();
    assert_eq!(ws.release.release, Release::Nanbield);
    assert_eq!(ws.release.source, ReleaseSource::CoreNames);
    assert_eq!(
        ws.layer("meta-mine").unwrap().lint_releases(&ws),
        [Release::Nanbield, Scarthgap]
    );
}
