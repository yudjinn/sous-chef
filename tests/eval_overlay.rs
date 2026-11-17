//! `eval::evaluate_with_overlay`: evaluating with files replaced in memory (planned edits,
//! files not created yet), without writing anything and without changing the configuration
//! cache or the index.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use sous_chef::eval::{self, ConfigCache, Overlay, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

const HELLO: &str = "meta-core:recipes-core/hello/hello_1.0.bb";

fn target() -> Target {
    Target {
        recipe: Some(HELLO.into()),
        ..Target::default()
    }
}

fn value(ds: &eval::DataStore, name: &str) -> Option<String> {
    ds.value(name).map(|v| v.text)
}

#[test]
fn an_overlay_replaces_and_adds_files_for_one_evaluation_only() {
    let (_tmp, root) = temp_ws();
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache_dir.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let cache = ConfigCache::new();

    let plain = eval::evaluate_with(&cache, &idx, &ws, &target()).unwrap();
    assert_eq!(cache.passes(), 1);
    assert_eq!(
        value(&plain, "SUMMARY").as_deref(),
        Some("GNU Hello stand-in")
    );
    assert_eq!(value(&plain, "EXTRA_SET"), None);

    // The recipe now requires a new extra.inc (not on disk), and hello.inc is replaced.
    let dir = root.join("meta-core/recipes-core/hello");
    let bb = std::fs::read_to_string(dir.join("hello_1.0.bb")).unwrap();
    let mut o = Overlay::new();
    o.insert(
        &ws,
        &dir.join("hello_1.0.bb"),
        &format!("{bb}require extra.inc\n"),
    )
    .unwrap();
    o.insert(
        &ws,
        &dir.join("extra.inc"),
        "EXTRA_SET = \"from ${PN}\"\npython () {\n    d.setVar('ANON_SET', '1')\n}\n",
    )
    .unwrap();
    let inc = std::fs::read_to_string(dir.join("hello.inc")).unwrap();
    o.insert(
        &ws,
        &dir.join("hello.inc"),
        &inc.replace("GNU Hello stand-in", "Overlaid"),
    )
    .unwrap();
    let ds = eval::evaluate_overlay_with(&cache, &idx, &ws, &target(), &o).unwrap();
    assert_eq!(value(&ds, "SUMMARY").as_deref(), Some("Overlaid"));
    assert_eq!(value(&ds, "EXTRA_SET").as_deref(), Some("from hello"));
    assert!(
        ds.files()
            .iter()
            .any(|v| v.loc.path.ends_with("hello/extra.inc")),
        "{:?}",
        ds.files()
    );
    // The anonymous python body is read from the overlay, not from disk.
    assert!(ds.replay().python_vars.contains("ANON_SET"));
    // Nothing written; the cached configuration was used and is unchanged.
    assert!(!dir.join("extra.inc").exists());
    assert_eq!(cache.passes(), 1);
    let again = eval::evaluate_with(&cache, &idx, &ws, &target()).unwrap();
    assert_eq!(cache.passes(), 1);
    assert_eq!(
        value(&again, "SUMMARY").as_deref(),
        Some("GNU Hello stand-in")
    );
    assert_eq!(value(&again, "EXTRA_SET"), None);
}

#[test]
fn an_overlay_conf_replays_the_configuration_without_caching_it() {
    let (_tmp, root) = temp_ws();
    // local.conf includes an .inc that does not exist yet.
    let local = root.join("build/conf/local.conf");
    let text = std::fs::read_to_string(&local).unwrap();
    std::fs::write(&local, format!("{text}include conf/later.inc\n")).unwrap();
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache_dir.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let cache = ConfigCache::new();
    eval::evaluate_with(&cache, &idx, &ws, &target()).unwrap();
    assert_eq!(cache.passes(), 1);

    // conf/site.conf does not exist; bitbake.conf includes it. In the overlay it does.
    let mut o = Overlay::new();
    o.insert(&ws, &root.join("build/conf/site.conf"), "SITE_X = \"1\"\n")
        .unwrap();
    let ds = eval::evaluate_overlay_with(&cache, &idx, &ws, &target(), &o).unwrap();
    assert_eq!(value(&ds, "SITE_X").as_deref(), Some("1"));
    let ds = eval::evaluate_overlay_with(&cache, &idx, &ws, &Target::default(), &o).unwrap();
    assert_eq!(value(&ds, "SITE_X").as_deref(), Some("1"));
    // Not cached: the next plain evaluation has no SITE_X and runs no new pass.
    let plain = eval::evaluate_with(&cache, &idx, &ws, &target()).unwrap();
    assert_eq!(value(&plain, "SITE_X"), None);
    assert_eq!(cache.passes(), 1);
    assert!(!root.join("build/conf/site.conf").exists());

    // An .inc (not a .conf) the cached pass looked for and did not find: the configuration
    // is replayed with it too.
    let mut o = Overlay::new();
    o.insert(&ws, &root.join("build/conf/later.inc"), "LATER_X = \"1\"\n")
        .unwrap();
    let ds = eval::evaluate_overlay_with(&cache, &idx, &ws, &target(), &o).unwrap();
    assert_eq!(value(&ds, "LATER_X").as_deref(), Some("1"));
    assert_eq!(cache.passes(), 1);
}

#[test]
fn an_overlay_path_outside_the_layers_is_refused() {
    let (_tmp, root) = temp_ws();
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let mut o = Overlay::new();
    assert!(
        o.insert(&ws, std::path::Path::new("/tmp/x.inc"), "")
            .is_err()
    );
    assert!(o.insert(&ws, &root.join("meta-core/README"), "").is_err());
    assert!(o.is_empty());
}
