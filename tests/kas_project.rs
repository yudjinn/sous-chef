//! T9.3: reading a kas project and discovering its workspace, on the kas fixture
//! (`tests/fixtures/kas/`: the project repository with two own layers and three kas files,
//! one of them included and one a lock file; and what `kas checkout` adds). No kas is run.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]

mod common;
mod kas_support;

use std::fs;
use std::path::PathBuf;

use kas_support::{Project, names, rel};
use sous_chef::config::Config;
use sous_chef::release::ReleaseSource;
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::{self, KasEnv, KasSource};

#[test]
fn reads_the_project_with_includes_lock_file_and_defaults() {
    let p = Project::before();
    let ws = p.ws();
    let k = ws.kas.as_ref().unwrap();
    assert_eq!(k.source, KasSource::Config);
    assert_eq!(k.root, p.root);
    assert_eq!(k.work_dir, p.root);
    assert_eq!(k.build_dir, p.path("build"));
    let read: Vec<String> = k.read.iter().map(|f| rel(&p.root, f)).collect();
    assert_eq!(
        read,
        [
            "kas/base.lock.yaml",
            "kas/common.yaml",
            "kas/base.yaml",
            "kas/board.yaml"
        ]
    );
    assert_eq!(k.lock_files, [p.path("kas/base.lock.yaml")]);
    assert!(k.problems.is_empty(), "{:?}", k.problems);

    let m = k.machine.as_ref().unwrap();
    assert_eq!(
        (m.value.as_str(), m.loc.display_in(&p.root)),
        ("board1", "kas/board.yaml:5".to_string())
    );
    assert_eq!(k.distro_or_default(), "projdistro");
    assert_eq!(k.targets_or_default(), ["proj-image"]);
    assert_eq!(k.target[0].loc.display_in(&p.root), "kas/base.yaml:8");
    assert_eq!(k.task, None);

    let repos: Vec<(&str, Option<&str>, Option<&str>, bool, String)> = k
        .repos
        .iter()
        .map(|r| {
            (
                r.id.as_str(),
                r.branch.as_deref(),
                r.commit.as_deref(),
                r.own,
                rel(&p.root, &r.path),
            )
        })
        .collect();
    assert_eq!(
        repos,
        [
            ("bitbake", Some("2.18"), None, false, "bitbake".into()),
            (
                "openembedded-core",
                Some("wrynose"),
                // Pinned by the lock file's overrides.
                Some("89e6c98d92887913cadf06b2adb97f26cde4849b"),
                false,
                "openembedded-core".into()
            ),
            (
                "meta-vendor",
                None,
                Some("0123456789abcdef0123456789abcdef01234567"),
                false,
                "meta-vendor".into()
            ),
            ("proj", None, None, true, "".into()),
            ("proj-bsp", None, None, true, "".into()),
        ]
    );

    // BBLAYERS order as kas writes it; the bitbake repo's only layer is disabled.
    let layers: Vec<(String, bool, bool)> = k
        .layers
        .iter()
        .map(|l| (rel(&p.root, &l.path), l.own, l.present))
        .collect();
    assert_eq!(
        layers,
        [
            ("meta-vendor".into(), false, false),
            ("openembedded-core/meta".into(), false, false),
            ("meta-proj".into(), true, true),
            ("meta-proj-bsp".into(), true, true),
        ]
    );

    let headers: Vec<(&str, String)> = k
        .local_conf_header
        .iter()
        .map(|h| (h.id.as_str(), h.loc.display_in(&p.root)))
        .collect();
    assert_eq!(
        headers,
        [
            ("common", "kas/common.yaml:7".into()),
            ("proj", "kas/base.yaml:33".into())
        ]
    );
    assert_eq!(
        k.local_conf_header[0].text,
        "PATCHRESOLVE = \"noop\"\nCONF_VERSION = \"2\"\n"
    );
    assert_eq!(k.bblayers_conf_header.len(), 1);
    assert_eq!(
        k.files_arg(&p.root),
        "kas/base.yaml:kas/board.yaml".to_string()
    );
}

#[test]
fn discovery_before_checkout_uses_the_present_kas_layers() {
    let p = Project::before();
    let ws = p.ws();
    assert_eq!(ws.build_dir, None);
    assert_eq!(names(&ws), ["meta-proj", "meta-proj-bsp"]);
    // Own layers are writable by default.
    assert!(ws.layers.iter().all(|l| l.writable));
    let k = ws.kas.as_ref().unwrap();
    let missing: Vec<String> = k
        .layers
        .iter()
        .filter(|l| !l.present)
        .map(|l| rel(&p.root, &l.path))
        .collect();
    assert_eq!(missing, ["meta-vendor", "openembedded-core/meta"]);
    assert!(k.not_in_bblayers.is_empty() && k.not_in_kas.is_empty());
    // No core layer yet: the release comes from the own layers' compat.
    assert_eq!(ws.release.source, ReleaseSource::LayerCompat);
}

#[test]
fn discovery_after_checkout_follows_bblayers_and_marks_own_layers() {
    let p = Project::after();
    let ws = p.ws();
    assert_eq!(ws.build_dir, Some(p.path("build")));
    assert_eq!(
        names(&ws),
        ["meta-vendor", "meta", "meta-proj", "meta-proj-bsp"]
    );
    let writable: Vec<&str> = ws
        .layers
        .iter()
        .filter(|l| l.writable)
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(writable, ["meta-proj", "meta-proj-bsp"]);
    assert_eq!(ws.release.source, ReleaseSource::CoreNames);
    let k = ws.kas.as_ref().unwrap();
    assert!(k.layers.iter().all(|l| l.present));
    assert!(k.not_in_bblayers.is_empty() && k.not_in_kas.is_empty());
    assert!(k.generates(&p.path("build")));
}

#[test]
fn writable_layers_in_the_config_win_over_own_layers() {
    let p = Project::after();
    let cfg = Config {
        writable_layers: vec!["meta-vendor".into()],
        ..p.config()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let writable: Vec<&str> = ws
        .layers
        .iter()
        .filter(|l| l.writable)
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(writable, ["meta-vendor"]);
}

/// The kas files changed since the last checkout: bblayers.conf stays the authority and
/// the differences are listed.
#[test]
fn kas_files_and_bblayers_disagree() {
    let p = Project::after();
    // A layer added to the kas file after the checkout (on disk, not in bblayers.conf)...
    common::copy_dir(&p.path("meta-proj-bsp"), &p.path("meta-proj-extra"));
    let conf = p.path("meta-proj-extra/conf/layer.conf");
    let text = fs::read_to_string(&conf)
        .unwrap()
        .replace("proj-bsp", "proj-extra");
    fs::write(&conf, text).unwrap();
    let board = p.path("kas/board.yaml");
    let text = fs::read_to_string(&board).unwrap() + "      meta-proj-extra:\n";
    fs::write(&board, text).unwrap();
    // ...and meta-vendor removed from the kas file, still in bblayers.conf.
    let base = p.path("kas/base.yaml");
    let text = fs::read_to_string(&base).unwrap().replace(
        "    commit: 0123456789abcdef0123456789abcdef01234567\n",
        "    commit: 0123456789abcdef0123456789abcdef01234567\n    layers:\n      .: disabled\n",
    );
    fs::write(&base, text).unwrap();

    let ws = p.ws();
    assert_eq!(
        names(&ws),
        ["meta-vendor", "meta", "meta-proj", "meta-proj-bsp"]
    );
    let k = ws.kas.as_ref().unwrap();
    assert_eq!(k.not_in_bblayers, [p.path("meta-proj-extra")]);
    assert_eq!(k.not_in_kas, ["meta-vendor"]);
}

#[test]
fn an_include_outside_the_project_is_reported_not_read() {
    let p = Project::before();
    fs::write(
        p._tmp.path().join("outside.yaml"),
        "header:\n  version: 11\nmachine: evil\n",
    )
    .unwrap();
    fs::write(
        p.path("kas/evil.yaml"),
        "header:\n  version: 11\n  includes:\n    - ../outside.yaml\n    - kas/../../outside.yaml\n",
    )
    .unwrap();
    let cfg = Config {
        kas: vec![p.path("kas/base.yaml"), p.path("kas/evil.yaml")],
        ..p.config()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let k = ws.kas.as_ref().unwrap();
    assert_eq!(k.machine, None);
    assert_eq!(k.problems.len(), 2, "{:?}", k.problems);
    assert!(
        k.problems
            .iter()
            .all(|m| m.contains("outside") && m.contains("not followed")),
        "{:?}",
        k.problems
    );
    assert!(!k.read.iter().any(|f| f.ends_with("outside.yaml")));
}

#[test]
fn a_repo_include_is_listed_until_the_repo_is_checked_out() {
    let mk = |p: &Project| {
        fs::write(
            p.path("kas/vendor.yaml"),
            "header:\n  version: 11\n  includes:\n    - repo: meta-vendor\n      file: kas/vendor-extra.yaml\n",
        )
        .unwrap();
        Config {
            kas: vec![p.path("kas/base.yaml"), p.path("kas/vendor.yaml")],
            ..p.config()
        }
    };
    let p = Project::before();
    let ws = Workspace::discover_in(&mk(&p), &KasEnv::default()).unwrap();
    let k = ws.kas.unwrap();
    assert_eq!(
        k.problems,
        [
            "includes from repo meta-vendor were not read: it is not checked out yet (run kas checkout)"
        ]
    );

    let p = Project::after();
    fs::create_dir_all(p.path("meta-vendor/kas")).unwrap();
    fs::write(
        p.path("meta-vendor/kas/vendor-extra.yaml"),
        "header:\n  version: 11\nmachine: vendorboard\n",
    )
    .unwrap();
    let ws = Workspace::discover_in(&mk(&p), &KasEnv::default()).unwrap();
    let k = ws.kas.unwrap();
    assert!(k.problems.is_empty(), "{:?}", k.problems);
    assert_eq!(k.machine.unwrap().value, "vendorboard");
}

#[test]
fn detection_without_configuration() {
    // A kas build directory (CACHEDIR.TAG) with two candidate files: nothing is guessed.
    let p = Project::after();
    let cfg = Config {
        work_dir: p.root.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let k = ws.kas.as_ref().unwrap();
    let KasSource::Ambiguous { candidates } = &k.source else {
        panic!("{:?}", k.source)
    };
    let c: Vec<String> = candidates.iter().map(|c| rel(&p.root, c)).collect();
    // common.yaml is included by base.yaml, the lock file is not a candidate.
    assert_eq!(c, ["kas/base.yaml", "kas/board.yaml"]);
    assert!(k.files.is_empty());
    assert!(k.source.describe().contains("set kas = [...]"));
    // Nothing is writable without a decision.
    assert!(ws.layers.iter().all(|l| !l.writable));

    // One candidate: used, and status says how it was found.
    fs::remove_file(p.path("kas/board.yaml")).unwrap();
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let k = ws.kas.as_ref().unwrap();
    assert_eq!(k.source, KasSource::Found);
    assert_eq!(k.files, [p.path("kas/base.yaml")]);

    // .config.yaml written by kas menu in the work directory wins over that.
    fs::write(
        p.path(".config.yaml"),
        "header:\n  version: 11\n  includes:\n    - kas/base.yaml\nmachine: menuboard\n",
    )
    .unwrap();
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let k = ws.kas.as_ref().unwrap();
    assert_eq!(k.source, KasSource::MenuConfig);
    assert_eq!(k.machine_or_default(), "menuboard");
}

#[test]
fn the_kas_environment_counts_only_for_a_related_directory() {
    let p = Project::after();
    fs::write(p.path(".config.yaml"), "header:\n  version: 11\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    common::copy_dir(&common::fixture_ws_root(), &elsewhere.path().join("ws"));
    let cfg = common::ws_config(&elsewhere.path().join("ws"));
    let env = KasEnv {
        work_dir: Some(p.root.clone()),
        build_dir: Some(p.path("build")),
    };
    let ws = Workspace::discover_in(&cfg, &env).unwrap();
    assert_eq!(ws.kas, None);

    // Started below KAS_WORK_DIR: it is used.
    let cfg = Config {
        work_dir: p.path("meta-proj"),
        ..Config::default()
    };
    let ws = Workspace::discover_in(&cfg, &env).unwrap();
    assert_eq!(ws.kas.as_ref().unwrap().source, KasSource::MenuConfig);
}

#[test]
fn a_workspace_without_kas_is_unchanged() {
    let ws = common::fixture_ws();
    assert_eq!(ws.kas, None);
    assert_eq!(names(&ws), ["meta-core", "meta-vendor", "meta-mine"]);
}

#[test]
fn a_missing_configured_kas_file_is_a_config_error() {
    let p = Project::before();
    let cfg = Config {
        kas: vec![p.path("kas/nope.yaml")],
        ..p.config()
    };
    let e = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap_err();
    assert!(e.to_string().contains("nope.yaml"), "{e}");
    assert!(cfg.validate(&Workspace::default()).is_err());
}

#[test]
fn config_file_kas_key_is_relative_to_the_file() {
    let cfg = Config::from_toml(
        "kas = [\"kas/kiss.yaml\", \"kas/dogbonedark.yaml\"]\n",
        &PathBuf::from("/p/sous-chef.toml"),
    )
    .unwrap();
    assert_eq!(
        cfg.kas,
        [
            PathBuf::from("/p/kas/kiss.yaml"),
            PathBuf::from("/p/kas/dogbonedark.yaml")
        ]
    );
    assert!(Config::from_toml("kas = \"x\"\n", &PathBuf::from("/p/sous-chef.toml")).is_err());
}

#[test]
fn repo_root_follows_kas() {
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().join("top");
    fs::create_dir_all(top.join(".git")).unwrap();
    fs::create_dir_all(top.join("sub/kas")).unwrap();
    // A submodule's .git file points under modules/: the superproject is the root.
    fs::write(top.join("sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
    assert_eq!(kas::repo_root(&top.join("sub/kas")), Some(top.clone()));
    // A worktree's .git file is a root of its own.
    fs::write(top.join("sub/.git"), "gitdir: /x/.git/worktrees/sub\n").unwrap();
    assert_eq!(kas::repo_root(&top.join("sub/kas")), Some(top.join("sub")));
}
