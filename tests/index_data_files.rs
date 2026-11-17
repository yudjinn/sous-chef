//! Data files are not BitBake metadata (live verification B1) and recipe files no
//! `BBFILES` glob collects are said to be so (w5-fix-eval2 item 9). On a temporary copy of
//! the fixture workspace with one file of each real kind.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::index::query::{self, FileSet};
use sous_chef::lint;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{item, search};
use sous_chef::workspace::Workspace;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Not BitBake syntax: what the real data files look like.
const WPA: &str = "ctrl_interface=/var/run/wpa_supplicant\nctrl_interface_group=0\nupdate_config=1\n\nnetwork={\n\tkey_mgmt=NONE\n}\n";
const CCACHE: &str = "max_size = 0\ncache_dir_levels = 3\n";
const WKS: &str =
    "part /boot --source bootimg_pcbios --ondisk sda --label boot --active --align 1024\n";

fn data_ws(root: &Path) {
    // A recipe's <pn>/ directory (wpa-supplicant/wpa-supplicant/wpa_supplicant.conf).
    write(root, "meta-core/recipes-core/hello/hello/hello.conf", WPA);
    // A recipe's files/ directory (busybox/files/mdev.conf).
    write(
        root,
        "meta-core/recipes-core/hello/files/defaults.conf",
        WPA,
    );
    // A recipe's <pn>-<pv>/ directory (initscripts/initscripts-1.0/).
    write(
        root,
        "meta-core/recipes-core/hello/hello-1.1/logrotate.conf",
        WPA,
    );
    // files/wic/common.wks.inc at the layer root.
    write(root, "meta-core/files/wic/common.wks.inc", WKS);
    // A template .bb under lib/ (meta/lib/bblayers/templates/example.bb).
    write(
        root,
        "meta-core/lib/templates/example.bb",
        "SUMMARY = \"An example\"\nLICENSE = \"MIT\"\n",
    );
    // In the layer's conf/ tree: kept as metadata (conf/ccache.conf on OE-core).
    write(root, "meta-core/conf/ccache.conf", CCACHE);
    // An .inc in the recipe's <pn>/ directory that the recipe requires: metadata.
    write(
        root,
        "meta-core/recipes-core/hello/hello/shared.inc",
        "SHARED = \"1\"\n",
    );
    // One only a reference with a variable could name: metadata (could be its target).
    write(
        root,
        "meta-core/recipes-core/hello/hello/dyn-1.1.inc",
        "DYN = \"1\"\n",
    );
    // An unreferenced .inc next to the recipes, not in a data directory: kept.
    write(
        root,
        "meta-core/recipes-core/hello/orphan.inc",
        "ORPHAN = \"1\"\n",
    );
    let bb = root.join("meta-core/recipes-core/hello/hello_1.1.bb");
    let mut s = fs::read_to_string(&bb).unwrap();
    s.push_str("\nrequire hello/shared.inc\ninclude hello/dyn-${PV}.inc\n");
    fs::write(bb, s).unwrap();
}

struct Fx {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    cfg: Config,
    ws: Workspace,
    idx: Index,
}

fn fx() -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    data_ws(&root);
    let cfg = ws_config(&root);
    let ws = Workspace::discover(&cfg).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _tmp: tmp,
        _cache: cache,
        cfg,
        ws,
        idx,
    }
}

fn reason(fx: &Fx, rel: &str) -> Option<String> {
    let f = query::file_by_path(&fx.idx, "meta-core", Path::new(rel))
        .unwrap()
        .unwrap_or_else(|| panic!("{rel} not indexed"));
    query::not_collected(&fx.idx, f.id).unwrap()
}

#[test]
fn data_files_are_marked_and_metadata_is_kept() {
    let fx = fx();
    for data in [
        "recipes-core/hello/hello/hello.conf",
        "recipes-core/hello/files/defaults.conf",
        "recipes-core/hello/hello-1.1/logrotate.conf",
        "files/wic/common.wks.inc",
    ] {
        let r = reason(&fx, data).unwrap_or_else(|| panic!("{data} not marked"));
        assert!(r.starts_with("not BitBake metadata"), "{data}: {r}");
    }
    assert!(
        reason(&fx, "lib/templates/example.bb")
            .unwrap()
            .contains("BBFILES")
    );
    for meta in [
        "conf/ccache.conf",
        "recipes-core/hello/hello/shared.inc",
        "recipes-core/hello/hello/dyn-1.1.inc",
        "recipes-core/hello/orphan.inc",
        "conf/layer.conf",
        "recipes-core/hello/hello.inc",
    ] {
        assert_eq!(reason(&fx, meta), None, "{meta}");
    }
    // Listings leave data files out unless asked.
    let listed: Vec<String> = query::files_in_layer(&fx.idx, "meta-core")
        .unwrap()
        .into_iter()
        .map(|f| f.path.display().to_string())
        .collect();
    assert!(
        !listed.iter().any(|p| p.ends_with("hello.conf")),
        "{listed:?}"
    );
    assert!(listed.iter().any(|p| p == "lib/templates/example.bb"));
    let all = query::files_in_layer_in(&fx.idx, "meta-core", FileSet::ALL).unwrap();
    assert!(all.iter().any(|f| f.path.ends_with("hello/hello.conf")));
    let collected = query::files_in_layer_in(&fx.idx, "meta-core", FileSet::COLLECTED).unwrap();
    assert!(!collected.iter().any(|f| f.path.ends_with("example.bb")));
    assert!(
        !query::all_files(&fx.idx)
            .unwrap()
            .iter()
            .any(|f| f.path.ends_with("common.wks.inc"))
    );
    assert!(
        query::all_files_in(&fx.idx, FileSet::ALL)
            .unwrap()
            .iter()
            .any(|f| f.path.ends_with("common.wks.inc"))
    );
}

#[test]
fn a_data_file_becomes_metadata_when_something_requires_it() {
    let mut fx = fx();
    let root = fx.ws.layer("meta-core").unwrap().root.clone();
    let inc = root.join("recipes-core/hello/hello.inc");
    let mut s = fs::read_to_string(&inc).unwrap();
    s.push_str("\ninclude hello/hello.conf\n");
    fs::write(&inc, s).unwrap();
    fx.idx.refresh_file(&fx.ws, &inc).unwrap();
    assert_eq!(reason(&fx, "recipes-core/hello/hello/hello.conf"), None);
    // And back when the reference goes.
    let s = fs::read_to_string(&inc)
        .unwrap()
        .replace("\ninclude hello/hello.conf\n", "\n");
    fs::write(&inc, s).unwrap();
    fx.idx.refresh(&fx.ws).unwrap();
    assert!(reason(&fx, "recipes-core/hello/hello/hello.conf").is_some());
}

#[test]
fn lint_all_reports_no_parse_error_in_data_files() {
    let fx = fx();
    let report = lint::run(
        &fx.ws,
        &fx.idx,
        &fx.cfg,
        &lint::Target::Workspace,
        &lint::Options {
            rules: vec!["hygiene/parse-error".into()],
            min_severity: None,
            all: true,
        },
    )
    .unwrap();
    let in_data: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.loc.to_string())
        .filter(|l| {
            l.contains("hello/hello.conf")
                || l.contains("files/defaults.conf")
                || l.contains("logrotate.conf")
                || l.contains("common.wks.inc")
        })
        .collect();
    assert!(in_data.is_empty(), "{in_data:?}");
    // conf/ccache.conf stays metadata, so the rule still reports it (a known residual:
    // the rule of where metadata lives keeps every file in conf/).
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.loc.to_string().contains("conf/ccache.conf")),
        "{:?}",
        report.findings
    );
}

fn ctx(fx: &Fx) -> (Ctx, tempfile::TempDir) {
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &fx.ws).unwrap();
    index.refresh(&fx.ws).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    (
        Ctx {
            config: fx.cfg.clone(),
            workspace: fx.ws.clone(),
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
        cache,
    )
}

fn sargs(query: &str, mode: search::Mode) -> search::Args {
    search::Args {
        query: query.into(),
        mode,
        kind: None,
        layer: None,
        file_kind: None,
        recipe: None,
        limit: None,
        budget_tokens: Some(4000),
        format: None,
    }
}

#[tokio::test]
async fn search_skips_data_files_and_notes_uncollected_recipes() {
    let fx = fx();
    let (ctx, _c) = ctx(&fx);
    for mode in [search::Mode::Name, search::Mode::Regex] {
        let out = search::run(&ctx, sargs("ctrl_interface", mode))
            .await
            .unwrap();
        assert!(!out.contains("hello.conf"), "{mode:?}: {out}");
    }
    // A statement in the template .bb that BBFILES does not collect: found, with the note.
    let out = search::run(&ctx, sargs("SUMMARY", search::Mode::Name))
        .await
        .unwrap();
    assert!(
        out.contains("meta-core:lib/templates/example.bb:1"),
        "{out}"
    );
    assert!(
        out.contains(
            "note: meta-core:lib/templates/example.bb is not matched by any layer's BBFILES"
        ),
        "{out}"
    );
    let out = search::run(&ctx, sargs("An example", search::Mode::Regex))
        .await
        .unwrap();
    assert!(
        out.contains("is not matched by any layer's BBFILES"),
        "{out}"
    );
}

#[tokio::test]
async fn item_notes_uncollected_and_data_files() {
    let fx = fx();
    let (ctx, _c) = ctx(&fx);
    let run = |loc: &str| item::Args {
        loc: Some(loc.into()),
        recipe: None,
        file: None,
        name: None,
        context: 0,
        budget_tokens: None,
        format: None,
    };
    let out = item::run(&ctx, run("meta-core:lib/templates/example.bb:1"))
        .await
        .unwrap();
    assert!(
        out.contains("is not matched by any layer's BBFILES: BitBake does not collect it"),
        "{out}"
    );
    let out = item::run(
        &ctx,
        run("meta-core:recipes-core/hello/hello/hello.conf:1-2"),
    )
    .await
    .unwrap();
    assert!(out.contains("is not BitBake metadata"), "{out}");
    let out = item::run(&ctx, run("meta-core:recipes-core/hello/hello.inc:1"))
        .await
        .unwrap();
    assert!(!out.contains("note:"), "{out}");
}
