//! `Index::refresh_file` (wave 4, for the edit hook T7.4): one file reindexed without a
//! walk; the result matches what a full refresh gives.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use sous_chef::index::{FileRefresh, Index, query};
use sous_chef::workspace::Workspace;

struct Fx {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    ws: Workspace,
    index: Index,
}

fn fx() -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Fx {
        _tmp: tmp,
        _cache: cache,
        root,
        ws,
        index,
    }
}

/// Every edge as `(src, kind, target_text, target, line)`, by pointer.
type EdgeKey = (String, String, String, Option<String>, Option<u32>);

fn edges(index: &Index) -> BTreeSet<EdgeKey> {
    let mut out = BTreeSet::new();
    for f in query::all_files(index).unwrap() {
        for e in query::edges_from(index, f.id).unwrap() {
            out.insert((
                format!("{}:{}", e.src.layer, e.src.path.display()),
                e.kind.clone(),
                e.target_text.clone(),
                e.target
                    .as_ref()
                    .map(|t| format!("{}:{}", t.layer, t.path.display())),
                e.line,
            ));
        }
    }
    out
}

/// Statement names and lines of one file.
fn statements(index: &Index, layer: &str, path: &str) -> Vec<(String, u32)> {
    let f = query::file_by_path(index, layer, Path::new(path))
        .unwrap()
        .unwrap();
    query::statements_in(index, f.id, &query::StatementFilter::default())
        .unwrap()
        .into_iter()
        .map(|s| (s.name.unwrap_or_default(), s.loc.lines.start))
        .collect()
}

/// A fresh index of the same tree, by full refresh.
fn full(ws: &Workspace) -> (tempfile::TempDir, Index) {
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), ws).unwrap();
    index.refresh(ws).unwrap();
    (cache, index)
}

fn appends(index: &Index, layer: &str, path: &str) -> Vec<String> {
    let f = query::file_by_path(index, layer, Path::new(path))
        .unwrap()
        .unwrap();
    query::appends_for(index, f.id)
        .unwrap()
        .iter()
        .map(|a| format!("{}:{}", a.layer, a.path.display()))
        .collect()
}

const HELLO: &str = "meta-core/recipes-core/hello/hello_1.1.bb";

#[test]
fn unchanged_file_is_unchanged() {
    let mut f = fx();
    let abs = f.root.join(HELLO);
    assert_eq!(
        f.index.refresh_file(&f.ws, &abs).unwrap(),
        FileRefresh::Unchanged
    );
    // Same bytes, new mtime: only the stat moves.
    let text = fs::read(&abs).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&abs, &text).unwrap();
    assert_eq!(
        f.index.refresh_file(&f.ws, &abs).unwrap(),
        FileRefresh::Unchanged
    );
    assert!(f.index.refresh(&f.ws).unwrap().reindexed.is_empty());
}

#[test]
fn an_edited_file_gets_new_rows_and_edges_and_keeps_the_ones_pointing_at_it() {
    let mut f = fx();
    let appends_before = appends(&f.index, "meta-core", "recipes-core/hello/hello_1.1.bb");
    assert_eq!(appends_before.len(), 2);
    let abs = f.root.join(HELLO);
    let mut text = fs::read_to_string(&abs).unwrap();
    text.push_str("\ninherit autotoolsish\nEXTRA_OECONF += \"--new\"\n");
    fs::write(&abs, &text).unwrap();

    let got = f.index.refresh_file(&f.ws, &abs).unwrap();
    let FileRefresh::Reindexed(loc) = got else {
        panic!("{got:?}")
    };
    assert_eq!(
        loc.to_string(),
        "meta-core:recipes-core/hello/hello_1.1.bb:1-12"
    );
    let stmts = statements(&f.index, "meta-core", "recipes-core/hello/hello_1.1.bb");
    assert!(stmts.contains(&("autotoolsish".into(), 11)), "{stmts:?}");
    assert!(stmts.contains(&("EXTRA_OECONF".into(), 12)), "{stmts:?}");
    // bbappends stay attached, in the same order.
    assert_eq!(
        appends(&f.index, "meta-core", "recipes-core/hello/hello_1.1.bb"),
        appends_before
    );
    // The result equals a full refresh of the same tree.
    let (_c, fresh) = full(&f.ws);
    assert_eq!(edges(&f.index), edges(&fresh));
    // And a full refresh afterwards has nothing to do.
    assert!(f.index.refresh(&f.ws).unwrap().reindexed.is_empty());
}

#[test]
fn an_edited_bbappend_keeps_its_place_in_the_append_order() {
    let mut f = fx();
    let before = appends(&f.index, "meta-core", "recipes-core/hello/hello_1.1.bb");
    let abs = f
        .root
        .join("meta-vendor/recipes-core/hello/hello_%.bbappend");
    let mut text = fs::read_to_string(&abs).unwrap();
    text.push_str("\nrequire hello-extra.inc\n");
    fs::write(&abs, &text).unwrap();
    assert!(matches!(
        f.index.refresh_file(&f.ws, &abs).unwrap(),
        FileRefresh::Reindexed(_)
    ));
    assert_eq!(
        appends(&f.index, "meta-core", "recipes-core/hello/hello_1.1.bb"),
        before
    );
    let (_c, fresh) = full(&f.ws);
    assert_eq!(edges(&f.index), edges(&fresh));
}

#[test]
fn a_new_file_resolves_references_and_a_removed_one_unresolves_them() {
    let mut f = fx();
    let recipe = f.root.join("meta-mine/recipes-app/new/new_1.0.bb");
    fs::create_dir_all(recipe.parent().unwrap()).unwrap();
    fs::write(&recipe, "require new.inc\nSUMMARY = \"new\"\n").unwrap();
    assert!(matches!(
        f.index.refresh_file(&f.ws, &recipe).unwrap(),
        FileRefresh::Reindexed(_)
    ));
    let unresolved = |index: &Index| {
        edges(index)
            .into_iter()
            .find(|e| e.0 == "meta-mine:recipes-app/new/new_1.0.bb" && e.1 == "require")
            .unwrap()
            .3
    };
    assert_eq!(unresolved(&f.index), None);

    let inc = f.root.join("meta-mine/recipes-app/new/new.inc");
    fs::write(&inc, "LICENSE = \"MIT\"\n").unwrap();
    assert!(matches!(
        f.index.refresh_file(&f.ws, &inc).unwrap(),
        FileRefresh::Reindexed(_)
    ));
    assert_eq!(
        unresolved(&f.index).as_deref(),
        Some("meta-mine:recipes-app/new/new.inc")
    );
    let (_c, fresh) = full(&f.ws);
    assert_eq!(edges(&f.index), edges(&fresh));

    fs::remove_file(&inc).unwrap();
    let got = f.index.refresh_file(&f.ws, &inc).unwrap();
    assert!(matches!(got, FileRefresh::Removed(_)), "{got:?}");
    assert_eq!(unresolved(&f.index), None);
    assert!(
        query::file_by_path(&f.index, "meta-mine", Path::new("recipes-app/new/new.inc"))
            .unwrap()
            .is_none()
    );
    let (_c, fresh) = full(&f.ws);
    assert_eq!(edges(&f.index), edges(&fresh));
}

#[test]
fn files_the_scan_skips_are_not_indexed() {
    let mut f = fx();
    for rel in [
        "meta-mine/.git/x.bb",
        "meta-mine/tmp/work/x.bb",
        "meta-mine/recipes-app/app/files/app.c",
        "build/tmp/x.conf",
    ] {
        let p = f.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "A = \"1\"\n").unwrap();
        assert_eq!(
            f.index.refresh_file(&f.ws, &p).unwrap(),
            FileRefresh::NotIndexed,
            "{rel}"
        );
    }
    let outside = tempfile::tempdir().unwrap();
    let p = outside.path().join("x.bb");
    fs::write(&p, "A = \"1\"\n").unwrap();
    assert_eq!(
        f.index.refresh_file(&f.ws, &p).unwrap(),
        FileRefresh::NotIndexed
    );
    // A file in the build directory's conf/ is indexed.
    let conf = f.root.join("build/conf/site.conf");
    fs::write(&conf, "SSTATE_DIR = \"/tmp/sstate\"\n").unwrap();
    assert!(matches!(
        f.index.refresh_file(&f.ws, &conf).unwrap(),
        FileRefresh::Reindexed(_)
    ));
}

#[test]
fn open_existing_never_creates() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("index.sqlite");
    assert!(Index::open_existing(&p, Duration::from_millis(10)).is_err());
    assert!(!p.exists());
    let f = fx();
    let idx = Index::open_existing(f.index.path(), Duration::from_millis(10)).unwrap();
    assert_eq!(idx.path(), f.index.path());
}
