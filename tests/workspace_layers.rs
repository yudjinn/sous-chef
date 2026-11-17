//! T1.3: layer discovery, pointers and confinement over `tests/fixtures/ws`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::error::Error;
use sous_chef::types::{LineSpan, Loc};
use sous_chef::workspace::{Workspace, layers};

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap()
}

fn names(ws: &Workspace) -> Vec<&str> {
    ws.layers.iter().map(|l| l.name.as_str()).collect()
}

#[test]
fn layer_order_follows_bblayers() {
    let ws = fixture_ws();
    assert_eq!(names(&ws), ["meta-core", "meta-vendor", "meta-mine"]);
    assert_eq!(ws.build_dir, Some(canon(&fixture_path("ws/build"))));
    for l in &ws.layers {
        assert_eq!(l.root, canon(&fixture_path(&format!("ws/{}", l.name))));
    }
}

#[test]
fn priorities_collections_compat_depends() {
    let ws = fixture_ws();
    let row = |n: &str| {
        let l = ws.layer(n).unwrap();
        (
            l.collections.clone(),
            l.priority,
            l.depends.clone(),
            l.compat.clone(),
            l.writable,
        )
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        row("meta-core"),
        (s(&["core"]), 5, s(&[]), s(&["scarthgap", "styhead"]), false)
    );
    assert_eq!(
        row("meta-vendor"),
        (s(&["vendor"]), 6, s(&["core"]), s(&["scarthgap"]), false)
    );
    assert_eq!(
        row("meta-mine"),
        (s(&["mine"]), 10, s(&["core"]), s(&["scarthgap"]), true)
    );
}

#[test]
fn bbfiles_have_layerdir_substituted() {
    let ws = fixture_ws();
    for l in &ws.layers {
        let root = l.root.display().to_string();
        assert_eq!(
            l.bbfiles,
            vec![
                format!("{root}/recipes-*/*/*.bb"),
                format!("{root}/recipes-*/*/*.bbappend"),
            ],
            "{}",
            l.name
        );
    }
}

#[test]
fn build_dir_found_by_walking_up() {
    let root = fixture_ws_root();
    let cfg = Config {
        work_dir: root.join("build/conf"),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.build_dir, Some(canon(&root.join("build"))));
    assert_eq!(names(&ws), ["meta-core", "meta-vendor", "meta-mine"]);
    assert_eq!(layers::find_build_dir(&root.join("meta-core/conf")), None);
}

#[test]
fn build_dir_without_bblayers_is_an_error() {
    let root = fixture_ws_root();
    let cfg = Config {
        build_dir: Some(root.join("meta-core")),
        work_dir: root,
        ..Config::default()
    };
    let err = Workspace::discover(&cfg).unwrap_err();
    assert!(matches!(err, Error::Config { .. }), "{err}");
    assert!(err.to_string().contains("bblayers.conf"), "{err}");
}

#[test]
fn no_build_dir_scans_for_layer_confs() {
    // meta-core/conf/layer.conf is found from ws/, which has no conf/bblayers.conf and
    // whose ancestors (the source tree) have none either.
    let root = fixture_ws_root();
    let cfg = Config {
        work_dir: root.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.build_dir, None);
    assert_eq!(
        names(&ws),
        ["meta-core", "meta-legacy", "meta-mine", "meta-vendor"]
    );
    let legacy = ws.layer("meta-legacy").unwrap();
    assert_eq!(legacy.collections, ["legacy"]);
    assert_eq!(legacy.priority, 7);
    assert_eq!(legacy.depends, ["core"]);
    assert_eq!(legacy.compat, ["dunfell"]);
}

#[test]
fn no_build_dir_uses_configured_layers() {
    let root = fixture_ws_root();
    let cfg = Config {
        work_dir: root.clone(),
        layers: vec![root.join("meta-mine"), PathBuf::from("meta-legacy")],
        writable_layers: vec!["meta-legacy".into()],
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(names(&ws), ["meta-mine", "meta-legacy"]);
    assert!(ws.layer("meta-legacy").unwrap().writable);
    assert!(!ws.layer("meta-mine").unwrap().writable);
}

#[test]
fn scan_respects_depth_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let mk = |rel: &str| {
        let d = tmp.path().join(rel).join("conf");
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("layer.conf"),
            "BBFILE_COLLECTIONS += \"x\"\nBBFILE_PRIORITY_x = \"1\"\n",
        )
        .unwrap();
    };
    mk("a/b/c/meta-deep4");
    mk("a/b/c/d/meta-deep5");
    mk(".hidden/meta-hidden");
    let found = layers::scan_for_layers(tmp.path(), layers::SCAN_DEPTH);
    assert_eq!(found, vec![tmp.path().join("a/b/c/meta-deep4")]);
}

#[test]
fn duplicate_names_are_disambiguated() {
    let tmp = tempfile::tempdir().unwrap();
    for (dir, c) in [("poky/meta", "core"), ("other/meta", "other")] {
        let d = tmp.path().join(dir).join("conf");
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("layer.conf"),
            format!("BBFILE_COLLECTIONS += \"{c}\"\nBBFILE_PRIORITY_{c} = \"5\"\n"),
        )
        .unwrap();
    }
    let cfg = Config {
        work_dir: tmp.path().to_path_buf(),
        layers: vec![tmp.path().join("poky/meta"), tmp.path().join("other/meta")],
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(names(&ws), ["poky/meta", "other/meta"]);
}

#[test]
fn bblayers_variables_and_python_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let base = canon(tmp.path());
    for (dir, c) in [("src/meta-a", "a"), ("src/meta-b", "b")] {
        let d = base.join(dir).join("conf");
        fs::create_dir_all(&d).unwrap();
        // meta-b leaves its priority unset: BitBake computes it from its dependencies.
        let prio = if c == "a" {
            "BBFILE_PRIORITY_a = \"4\"\n"
        } else {
            ""
        };
        fs::write(
            d.join("layer.conf"),
            format!(
                "BBFILE_COLLECTIONS += \"{c}\"\n{prio}LAYERDEPENDS_{c} = \"{}\"\n",
                if c == "b" { "a (>= 2)" } else { "" }
            ),
        )
        .unwrap();
    }
    let build = base.join("build");
    fs::create_dir_all(build.join("conf")).unwrap();
    fs::write(
        build.join("conf/bblayers.conf"),
        "BSPDIR := \"${TOPDIR}/../src\"\n\
         OEROOT := \"${@os.path.abspath(os.path.dirname(d.getVar('FILE')) + '/../..')}\"\n\
         BBLAYERS ?= \" \\\n  ${BSPDIR}/meta-a/ \\\n  ${OEROOT}/meta \\\n  ${UNSET}/meta-x \\\n  \"\n\
         BBLAYERS += \"${TOPDIR}/../src/meta-b\"\n",
    )
    .unwrap();

    let bb = layers::read_bblayers(&build).unwrap();
    assert_eq!(
        bb.layers,
        vec![
            PathBuf::from(format!("{}/../src/meta-a", build.display())),
            PathBuf::from(format!("{}/../src/meta-b", build.display())),
        ]
    );
    assert_eq!(bb.unresolved.len(), 2, "{:?}", bb.unresolved);
    assert!(bb.unresolved[0].starts_with("${@os.path.abspath("));
    assert_eq!(bb.unresolved[1], "${UNSET}/meta-x");

    let cfg = Config {
        build_dir: Some(build.clone()),
        work_dir: base.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(names(&ws), ["meta-a", "meta-b"]);
    assert_eq!(ws.layer("meta-b").unwrap().depends, ["a"]);
    assert_eq!(ws.layer("meta-b").unwrap().priority, 5);
}

#[test]
fn missing_layer_in_bblayers_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let build = tmp.path().join("build");
    fs::create_dir_all(build.join("conf")).unwrap();
    fs::write(
        build.join("conf/bblayers.conf"),
        "BBLAYERS = \"${TOPDIR}/../nope\"\n",
    )
    .unwrap();
    let cfg = Config {
        build_dir: Some(build),
        work_dir: tmp.path().to_path_buf(),
        ..Config::default()
    };
    let err = Workspace::discover(&cfg).unwrap_err();
    assert!(err.to_string().contains("not a directory"), "{err}");
}

#[test]
fn loc_for_and_abs_path_round_trip() {
    let ws = fixture_ws();
    let core = &ws.layer("meta-core").unwrap().root;
    let file = core.join("recipes-core/hello/hello_1.0.bb");
    let loc = ws.loc_for(&file, LineSpan::new(2, 4)).unwrap();
    assert_eq!(
        loc.to_string(),
        "meta-core:recipes-core/hello/hello_1.0.bb:2-4"
    );
    assert_eq!(ws.abs_path(&loc), Some(file.clone()));

    // A non-normalised path is accepted.
    let dotted = core.join("recipes-core/../recipes-core/hello/hello_1.0.bb");
    assert_eq!(
        ws.loc_for(&dotted, LineSpan::line(1)).unwrap().layer,
        "meta-core"
    );

    // The build directory is the pseudo-layer `build`.
    let local = ws.build_dir.as_ref().unwrap().join("conf/local.conf");
    let loc = ws.loc_for(&local, LineSpan::line(2)).unwrap();
    assert_eq!(loc.to_string(), "build:conf/local.conf:2");
    assert_eq!(ws.abs_path(&loc), Some(local));

    // Outside every layer.
    assert_eq!(
        ws.loc_for(Path::new("/etc/passwd"), LineSpan::line(1)),
        None
    );
    let legacy = fixture_path("ws/meta-legacy/conf/layer.conf");
    assert_eq!(ws.loc_for(&legacy, LineSpan::line(1)), None);

    // abs_path refuses unknown layers and escaping paths.
    let bad = |layer: &str, path: &str| Loc {
        layer: layer.into(),
        path: path.into(),
        lines: LineSpan::line(1),
    };
    assert_eq!(ws.abs_path(&bad("meta-nope", "conf/layer.conf")), None);
    assert_eq!(
        ws.abs_path(&bad("meta-core", "../meta-legacy/conf/layer.conf")),
        None
    );
    assert_eq!(ws.abs_path(&bad("meta-core", "/etc/passwd")), None);
}

#[test]
fn confine_accepts_inside_paths() {
    let ws = fixture_ws();
    let core = ws.layer("meta-core").unwrap().root.clone();
    let f = core.join("conf/layer.conf");
    assert_eq!(ws.confine(&f).unwrap(), f);
    // `..` that stays inside is fine.
    let via = core.join("recipes-core/../conf/layer.conf");
    assert_eq!(ws.confine(&via).unwrap(), f);
    // A file that does not exist yet (scaffold) inside a layer.
    let new = core.join("recipes-new/new/new_1.0.bb");
    assert_eq!(ws.confine(&new).unwrap(), new);
    // The build directory.
    let local = ws.build_dir.clone().unwrap().join("conf/local.conf");
    assert_eq!(ws.confine(&local).unwrap(), local);
}

#[test]
fn confine_rejects_dotdot_escapes() {
    let ws = fixture_ws();
    let core = ws.layer("meta-core").unwrap().root.clone();
    for p in [
        core.join("../meta-legacy/conf/layer.conf"),
        core.join("conf/../../../../../../etc/passwd"),
        core.join("../meta-core-not-really/x"),
        PathBuf::from("/etc/passwd"),
    ] {
        let err = ws.confine(&p).unwrap_err();
        assert!(
            matches!(err, Error::Confined { .. }),
            "{}: {err}",
            p.display()
        );
    }
}

#[test]
fn confine_rejects_symlinks_pointing_outside() {
    let (_tmp, root) = temp_ws();
    let outside = root.parent().unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), "x").unwrap();
    let core = root.join("meta-core");
    std::os::unix::fs::symlink(outside.join("secret"), core.join("link-file")).unwrap();
    std::os::unix::fs::symlink(&outside, core.join("link-dir")).unwrap();
    std::os::unix::fs::symlink(root.join("meta-legacy"), core.join("link-legacy")).unwrap();
    std::os::unix::fs::symlink(core.join("conf"), core.join("link-inside")).unwrap();
    std::os::unix::fs::symlink(outside.join("missing"), core.join("dangling")).unwrap();

    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let core = ws.layer("meta-core").unwrap().root.clone();
    for name in [
        "link-file",
        "link-dir/secret",
        "link-dir/new-file",
        "link-legacy/conf/layer.conf",
        "dangling",
        "link-inside/../../meta-legacy/conf/layer.conf",
    ] {
        let err = ws.confine(&core.join(name)).unwrap_err();
        assert!(matches!(err, Error::Confined { .. }), "{name}: {err}");
    }
    // A symlink that stays inside resolves to its target.
    assert_eq!(
        ws.confine(&core.join("link-inside/layer.conf")).unwrap(),
        core.join("conf/layer.conf")
    );
}
