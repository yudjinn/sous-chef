//! `BBFILES_DYNAMIC` as BitBake reads it: once every `layer.conf` has been parsed, with the
//! datastore discovery uses (so includes and overrides count), and a malformed word is
//! BitBake's fatal error (`cookerdata.py` `parseConfigurationFiles`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::bitbake::JobTable;
use sous_chef::handles::HandleStore;
use sous_chef::index::{Index, query};
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::layers;
use sous_chef::workspace::{Workspace, resolve};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append_to(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

/// A bbappend of `hello` under `meta-mine/dynamic-layers/<dir>/`.
fn dynamic_append(root: &Path, dir: &str) -> PathBuf {
    let rel = format!("meta-mine/dynamic-layers/{dir}/recipes-core/hello/hello_%.bbappend");
    write(
        root,
        &rel,
        &format!("DYN_{} = \"1\"\n", dir.replace('-', "_")),
    );
    fs::canonicalize(root.join(rel)).unwrap()
}

fn discover(root: &Path) -> sous_chef::Result<Workspace> {
    Workspace::discover(&common::ws_config(root))
}

#[test]
fn an_assignment_in_an_included_file_or_under_an_active_override_is_followed() {
    let (_tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let included = dynamic_append(&root, "included");
    let overridden = dynamic_append(&root, "overridden");
    let inactive = dynamic_append(&root, "inactive");
    append_to(
        &root,
        "meta-mine/conf/layer.conf",
        "include conf/dynamic.inc\n\
         OVERRIDES = \"mine\"\n\
         BBFILES_DYNAMIC:append:mine = \" core:${LAYERDIR}/dynamic-layers/overridden/*/*/*.bbappend\"\n\
         BBFILES_DYNAMIC:append:other = \" core:${LAYERDIR}/dynamic-layers/inactive/*/*/*.bbappend\"\n",
    );
    // Found along BBPATH (meta-mine's own conf/), `${LAYERDIR}` still meta-mine.
    write(
        &root,
        "meta-mine/conf/dynamic.inc",
        "BBFILES_DYNAMIC += \"core:${LAYERDIR}/dynamic-layers/included/*/*/*.bbappend\"\n",
    );
    let ws = discover(&root).unwrap();
    let c = resolve::collect(&ws);
    assert!(c.files.contains(&included), "{:?}", c.dynamic);
    assert!(c.files.contains(&overridden), "{:?}", c.dynamic);
    assert!(!c.files.contains(&inactive), "{:?}", c.dynamic);
    assert!(c.via_dynamic.contains(&included));
    assert!(c.unusable.is_empty(), "{:?}", c.unusable);
    let layers: Vec<&str> = c.dynamic.iter().map(|d| d.layer.as_str()).collect();
    assert_eq!(layers, ["meta-mine", "meta-mine"]);

    // The index collects them, so they are applied and linted.
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    for (dir, collected) in [
        ("included", true),
        ("overridden", true),
        ("inactive", false),
    ] {
        let f = query::file_by_path(
            &index,
            "meta-mine",
            Path::new(&format!(
                "dynamic-layers/{dir}/recipes-core/hello/hello_%.bbappend"
            )),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            query::not_collected(&index, f.id).unwrap().is_none(),
            collected,
            "{dir}"
        );
    }
}

#[test]
fn a_word_without_a_colon_is_bitbakes_fatal_error() {
    let (_tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    append_to(
        &root,
        "meta-mine/conf/layer.conf",
        "BBFILES_DYNAMIC += \"${LAYERDIR}/dynamic-layers/core/*/*/*.bbappend\"\n",
    );
    let e = discover(&root).unwrap_err().to_string();
    let word = format!(
        "{}/dynamic-layers/core/*/*/*.bbappend",
        root.join("meta-mine").display()
    );
    assert!(
        e.contains(&format!(
            "BBFILES_DYNAMIC entries must be of the form {{!}}<collection name>:<filename pattern>, not:\n    {word}"
        )),
        "{e}"
    );
    assert!(e.contains("meta-mine/conf/layer.conf"), "{e}");

    // An empty collection name: BitBake fails too (`l[0]` of an empty string).
    let (_tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    append_to(
        &root,
        "meta-mine/conf/layer.conf",
        "BBFILES_DYNAMIC += \":${LAYERDIR}/x/*.bbappend\"\n",
    );
    let e = discover(&root).unwrap_err().to_string();
    assert!(e.contains("empty collection name"), "{e}");
}

#[tokio::test]
async fn python_is_left_out_and_listed() {
    let (_tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    append_to(
        &root,
        "meta-mine/conf/layer.conf",
        "BBFILES_DYNAMIC += \"${@'core:' + d.getVar('LAYERDIR')}/x/*.bbappend \
         core:${UNSET_DIR}/x/*.bbappend\"\n",
    );
    let ws = discover(&root).unwrap();
    let c = resolve::collect(&ws);
    assert_eq!(c.unusable.len(), 2, "{:?}", c.unusable);
    assert!(c.unusable[0].starts_with("${@"), "{:?}", c.unusable);
    assert_eq!(c.unusable[1], "core:${UNSET_DIR}/x/*.bbappend");
    assert!(c.dynamic.is_empty(), "{:?}", c.dynamic);

    // Said by `layers show=appends` (and the audit's structure notes).
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config: common::ws_config(&root),
        workspace: ws,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    let out = layers::run(
        &ctx,
        layers::Args {
            show: layers::Show::Appends,
            budget_tokens: Some(8000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(
        out.contains("note: BBFILES_DYNAMIC words sous-chef cannot decide statically"),
        "{out}"
    );
    assert!(out.contains("core:${UNSET_DIR}/x/*.bbappend"), "{out}");
}
