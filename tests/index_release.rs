//! The release the index was built for (`meta.release`, schema version 2): changing only
//! `release` in `sous-chef.toml` recomputes the edges, through `Index::refresh` and through
//! `Index::refresh_file`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use sous_chef::config::{CliOverrides, Config};
use sous_chef::index::{FileRefresh, Index, query, schema};
use sous_chef::release::Release;
use sous_chef::workspace::Workspace;

const LIBFOO: &str = "recipes-core/libfoo/libfoo_2.0.bb";
/// `meta-core` has `autotoolsish` only under `classes-recipe/`.
const CLASS: &str = "classes-recipe/autotoolsish.bbclass";

/// Rewrites the first line of the copy's `sous-chef.toml` (`release = "..."`, put there by
/// `temp_ws_release`) and discovers the workspace again.
fn switch_release(root: &Path, codename: &str) -> Workspace {
    let toml = root.join("sous-chef.toml");
    let text = fs::read_to_string(&toml).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.starts_with("release = "), "{first}");
    fs::write(&toml, format!("release = \"{codename}\"\n{rest}")).unwrap();
    let cfg = Config::load(&CliOverrides {
        config: Some(toml),
        work_dir: root.to_path_buf(),
        ..Default::default()
    })
    .unwrap();
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.release.release.codename(), codename);
    ws
}

/// The file `autotoolsish` resolves to from `libfoo_2.0.bb`, if any.
fn autotoolsish_target(index: &Index) -> Option<PathBuf> {
    let f = query::file_by_path(index, "meta-core", Path::new(LIBFOO))
        .unwrap()
        .unwrap();
    let edges = query::edges_from(index, f.id).unwrap();
    let e = edges
        .iter()
        .find(|e| e.kind == "inherit" && e.target_text == "autotoolsish")
        .expect("libfoo inherits autotoolsish");
    e.target.as_ref().map(|t| t.path.clone())
}

/// Whether the class under `classes-recipe/` is the target of any edge.
fn class_is_inherit_target(index: &Index) -> bool {
    let class = query::file_by_path(index, "meta-core", Path::new(CLASS))
        .unwrap()
        .expect("the class is indexed on every release");
    query::edges_to(index, class.id)
        .unwrap()
        .iter()
        .any(|e| e.kind == "inherit")
}

#[test]
fn a_release_change_alone_recomputes_the_edges_on_refresh() {
    let (_tmp, root, _cfg, ws) = temp_ws_release("wrynose", |_| {});
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    assert_eq!(
        query::indexed_release(&index).unwrap(),
        Some(Release::Wrynose)
    );
    assert_eq!(autotoolsish_target(&index), Some(PathBuf::from(CLASS)));
    assert!(class_is_inherit_target(&index));
    let path = index.path().to_path_buf();
    drop(index);

    // Same layer roots, so the same cache key and database; no file changed.
    let ws = switch_release(&root, "kirkstone");
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    assert_eq!(index.path(), path);
    let report = index.refresh(&ws).unwrap();
    assert!(report.reindexed.is_empty(), "{report:?}");
    assert_eq!(
        query::indexed_release(&index).unwrap(),
        Some(Release::Kirkstone)
    );
    assert_eq!(autotoolsish_target(&index), None);
    assert!(!class_is_inherit_target(&index));

    // And back.
    let ws = switch_release(&root, "wrynose");
    index.refresh(&ws).unwrap();
    assert_eq!(
        query::indexed_release(&index).unwrap(),
        Some(Release::Wrynose)
    );
    assert_eq!(autotoolsish_target(&index), Some(PathBuf::from(CLASS)));
    assert!(class_is_inherit_target(&index));
}

#[test]
fn an_index_built_for_kirkstone_gains_the_class_edges_on_wrynose() {
    let (_tmp, root, _cfg, ws) = temp_ws_release("kirkstone", |_| {});
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    assert!(!class_is_inherit_target(&index));
    let ws = switch_release(&root, "wrynose");
    index.refresh(&ws).unwrap();
    assert!(class_is_inherit_target(&index));
}

#[test]
fn refresh_file_recomputes_every_edge_when_the_release_differs() {
    let (_tmp, root, _cfg, ws) = temp_ws_release("wrynose", |_| {});
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let path = index.path().to_path_buf();
    drop(index);
    let ws = switch_release(&root, "kirkstone");

    // The edit hook's path: an existing database, one unrelated file that did not change.
    let mut index = Index::open_existing(&path, Duration::from_secs(1)).unwrap();
    let other = root.join("meta-mine/conf/layer.conf");
    assert_eq!(
        index.refresh_file(&ws, &other).unwrap(),
        FileRefresh::Unchanged
    );
    assert_eq!(
        query::indexed_release(&index).unwrap(),
        Some(Release::Kirkstone)
    );
    assert!(!class_is_inherit_target(&index));

    // An edited file in a stale index: its rows are replaced and every edge follows the
    // workspace's release, as a full refresh would give.
    let ws = switch_release(&root, "wrynose");
    let libfoo = root.join("meta-core").join(LIBFOO);
    let mut text = fs::read_to_string(&libfoo).unwrap();
    text.push_str("\nEXTRA_X = \"1\"\n");
    fs::write(&libfoo, text).unwrap();
    assert!(matches!(
        index.refresh_file(&ws, &libfoo).unwrap(),
        FileRefresh::Reindexed(_)
    ));
    assert_eq!(
        query::indexed_release(&index).unwrap(),
        Some(Release::Wrynose)
    );
    assert!(class_is_inherit_target(&index));
    assert_eq!(autotoolsish_target(&index), Some(PathBuf::from(CLASS)));
}

#[test]
fn a_version_1_index_is_rebuilt_and_refused_read_only() {
    let (_tmp, _root, _cfg, ws) = temp_ws_release("wrynose", |_| {});
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let path = index.path().to_path_buf();
    index.conn().pragma_update(None, "user_version", 1).unwrap();
    drop(index);
    assert!(Index::open_read_only(&path, Duration::from_secs(1)).is_err());
    let index = Index::open_at(&path).unwrap();
    assert_eq!(
        schema::user_version(index.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
    // Rebuilt from scratch: nothing recorded until the first refresh.
    assert_eq!(query::indexed_release(&index).unwrap(), None);
}
