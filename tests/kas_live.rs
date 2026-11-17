//! T9.3, live: discovery of the yocto-kiss kas project (`kas/kiss.yaml:kas/dogbonedark.yaml`)
//! against `bitbake-layers show-layers` run through kas. Ignored: it needs the environment
//! T9.1 creates, named by `SOUS_CHEF_LIVE` (the yocto-kiss checkout is
//! `$SOUS_CHEF_LIVE/yocto-kiss`, or `SOUS_CHEF_LIVE` itself when it holds `kas/kiss.yaml`;
//! a `kas` executable is looked for on `PATH` and in `$SOUS_CHEF_LIVE/venv/bin`). Run with
//! `SOUS_CHEF_LIVE=... cargo test --test kas_live -- --ignored`. It only parses (no build).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use sous_chef::bitbake::{BbEnv, HostEnv};
use sous_chef::config::Config;
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::KasEnv;

fn project() -> PathBuf {
    let live = PathBuf::from(
        std::env::var_os("SOUS_CHEF_LIVE").expect("set SOUS_CHEF_LIVE to the live environment"),
    );
    let nested = live.join("yocto-kiss");
    let root = if nested.join("kas/kiss.yaml").is_file() {
        nested
    } else {
        live
    };
    assert!(
        root.join("kas/kiss.yaml").is_file(),
        "no yocto-kiss checkout at {}",
        root.display()
    );
    root.canonicalize().unwrap()
}

/// Discovery alone (reads files, starts nothing).
#[test]
#[ignore = "needs the live kas environment (SOUS_CHEF_LIVE)"]
fn yocto_kiss_discovery() {
    let root = project();
    let cfg = Config {
        kas: vec![
            root.join("kas/kiss.yaml"),
            root.join("kas/dogbonedark.yaml"),
        ],
        work_dir: root.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let k = ws.kas.as_ref().unwrap();
    eprintln!(
        "layers: {:?}\nkas layers: {:?}\nproblems: {:?}\nnot_in_bblayers: {:?}\nnot_in_kas: {:?}\nlocks: {:?}",
        ws.layers
            .iter()
            .map(|l| (&l.name, l.writable))
            .collect::<Vec<_>>(),
        k.layers
            .iter()
            .map(|l| (&l.path, l.present, l.own))
            .collect::<Vec<_>>(),
        k.problems,
        k.not_in_bblayers,
        k.not_in_kas,
        k.lock_files
    );
    assert!(k.problems.is_empty(), "{:?}", k.problems);
    assert_eq!(k.machine_or_default(), "dogbonedark");
    assert_eq!(k.distro_or_default(), "kiss");
    assert!(k.not_in_bblayers.is_empty() && k.not_in_kas.is_empty());

    // The generated local.conf and bblayers.conf are all kas's own.
    let cache = tempfile::tempdir().unwrap();
    let index = sous_chef::index::Index::open_in(cache.path(), &ws).unwrap();
    for conf in ["conf/local.conf", "conf/bblayers.conf"] {
        let text = std::fs::read_to_string(root.join("build").join(conf)).unwrap();
        let found = sous_chef::lint::check_text(
            &sous_chef::lint::rules::org_generated_conf_edit::OrgGeneratedConfEdit,
            &ws,
            &index,
            sous_chef::types::Loc {
                layer: "build".into(),
                path: PathBuf::from(conf),
                lines: sous_chef::types::LineSpan::line(1),
            },
            &text,
            Some(sous_chef::types::FileKind::Conf),
            &[],
            None,
        );
        assert!(found.is_empty(), "{conf}: {found:?}");
    }

    // A bitbake-setup build next to it, when there is one.
    let live = PathBuf::from(std::env::var_os("SOUS_CHEF_LIVE").unwrap());
    if let Some(build) = std::fs::read_dir(live.join("bitbake-builds"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("build"))
        .find(|b| b.join("init-build-env").is_file())
    {
        let ws = Workspace {
            build_dir: Some(build.canonicalize().unwrap()),
            ..Workspace::default()
        };
        let env = BbEnv::probe(&Config::default(), &ws, &HostEnv::default()).unwrap();
        assert_eq!(env.mode(), "bitbake-setup", "{}", build.display());
    }
}

#[tokio::test]
#[ignore = "needs the live kas environment (SOUS_CHEF_LIVE)"]
async fn yocto_kiss_layers_match_show_layers() {
    let root = project();
    let cfg = Config {
        kas: vec![
            root.join("kas/kiss.yaml"),
            root.join("kas/dogbonedark.yaml"),
        ],
        work_dir: root.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let k = ws.kas.as_ref().unwrap();
    assert!(k.problems.is_empty(), "{:?}", k.problems);
    assert_eq!(k.machine_or_default(), "dogbonedark");
    assert_eq!(k.distro_or_default(), "kiss");
    assert_eq!(k.targets_or_default(), ["kiss-image"]);
    assert_eq!(ws.build_dir, Some(root.join("build")));
    assert!(k.not_in_bblayers.is_empty(), "{:?}", k.not_in_bblayers);
    assert!(k.not_in_kas.is_empty(), "{:?}", k.not_in_kas);
    let writable: BTreeSet<&str> = ws
        .layers
        .iter()
        .filter(|l| l.writable)
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(writable, BTreeSet::from(["meta-kiss", "meta-kiss-ti"]));

    let live = PathBuf::from(std::env::var_os("SOUS_CHEF_LIVE").unwrap());
    let mut dirs: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    dirs.push(live.join("venv/bin"));
    let host = HostEnv {
        path: Some(std::env::join_paths(dirs).unwrap()),
        ..HostEnv::default()
    };
    let env = BbEnv::probe(&cfg, &ws, &host).unwrap();
    assert_eq!(env.mode(), "kas");
    let out = env
        .output(
            "bitbake-layers",
            &["show-layers"],
            Some(Duration::from_secs(600)),
        )
        .await
        .unwrap();
    assert!(
        out.finish.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // `layer  path  priority` rows after the `====` rule.
    let shown: BTreeSet<PathBuf> = text
        .lines()
        .skip_while(|l| !l.starts_with("===="))
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(|p| {
            PathBuf::from(p)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(p))
        })
        .collect();
    let discovered: BTreeSet<PathBuf> = ws.layers.iter().map(|l| l.root.clone()).collect();
    assert_eq!(discovered, shown, "{text}");
    let from_kas: BTreeSet<PathBuf> = k
        .layers
        .iter()
        .map(|l| l.path.canonicalize().unwrap_or_else(|_| l.path.clone()))
        .collect();
    assert_eq!(from_kas, shown, "{text}");
}
