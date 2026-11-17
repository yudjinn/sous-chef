//! T9.3: `scaffold kind=layer` in a kas project adds the layer to a kas file (a `Modify`
//! that inserts one line and keeps comments, order and formatting) instead of changing
//! `bblayers.conf`; when the file cannot be edited safely the lines are in the notes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;

use kas_support::Project;
use serde_json::json;
use sous_chef::index::Index;
use sous_chef::scaffold::layer::LayerGen;
use sous_chef::scaffold::{FileAction, GenContext, Generator, Kind, Plan, Request};
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::KasEnv;

fn plan(p: &Project) -> Plan {
    let cfg = p.config();
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let request = Request {
        kind: Kind::Layer,
        layer: None,
        name: Some("meta-proj-new".into()),
        version: None,
        options: json!({}).as_object().cloned().unwrap(),
    };
    LayerGen
        .plan(&GenContext {
            ws: &ws,
            config: &cfg,
            index: &index,
            request: &request,
        })
        .unwrap()
}

#[test]
fn the_layer_goes_into_the_kas_file_after_checkout() {
    let p = Project::after();
    let before = fs::read_to_string(p.path("kas/base.yaml")).unwrap();
    let plan = plan(&p);
    assert_eq!(plan.new_layer, Some(p.path("meta-proj-new")));
    let paths: Vec<String> = plan
        .files
        .iter()
        .map(|f| kas_support::rel(&p.root, &f.path))
        .collect();
    assert_eq!(
        paths,
        [
            "meta-proj-new/conf/layer.conf",
            "meta-proj-new/README.md",
            "kas/base.yaml"
        ]
    );
    let edit = &plan.files[2];
    let FileAction::Modify { before_sha } = &edit.action else {
        panic!("{:?}", edit.action)
    };
    assert_eq!(
        *before_sha,
        sous_chef::scaffold::sha256_hex(before.as_bytes())
    );
    // Exactly one line added, after the last layer of the own repo; everything else as it
    // was, comments included.
    let want = before.replace(
        "  proj:\n    layers:\n      meta-proj:\n",
        "  proj:\n    layers:\n      meta-proj:\n      meta-proj-new:\n",
    );
    assert_eq!(edit.content, want);
    assert!(
        !plan.files.iter().any(|f| f.path.ends_with("bblayers.conf")),
        "bblayers.conf must not change in a kas project"
    );
    assert!(
        plan.notes.iter().any(|n| n.contains(
            "the layer is added to kas/base.yaml (a separate change in this plan), not to bblayers.conf, which kas generates; run kas checkout kas/base.yaml:kas/board.yaml"
        )),
        "{:#?}",
        plan.notes
    );

    // The edited file is read by kas detection as the same project plus the layer.
    fs::write(p.path("kas/base.yaml"), &edit.content).unwrap();
    common::copy_dir(&p.path("meta-proj"), &p.path("meta-proj-new"));
    let ws = p.ws();
    let k = ws.kas.unwrap();
    assert!(k.problems.is_empty(), "{:?}", k.problems);
    assert!(k.own_layer(&p.path("meta-proj-new")).is_some());
    assert_eq!(k.not_in_bblayers, [p.path("meta-proj-new")]);
}

#[test]
fn before_checkout_the_kas_file_is_edited_too() {
    let p = Project::before();
    let plan = plan(&p);
    assert!(plan.files.iter().any(
        |f| f.path == p.path("kas/base.yaml") && matches!(f.action, FileAction::Modify { .. })
    ));
}

#[test]
fn a_file_shape_it_cannot_edit_safely_gives_the_lines_instead() {
    let p = Project::after();
    let base = p.path("kas/base.yaml");
    let text = fs::read_to_string(&base).unwrap().replace(
        "  proj:\n    layers:\n      meta-proj:\n",
        "  proj:\n    layers: {meta-proj: }\n",
    );
    fs::write(&base, &text).unwrap();
    let plan = plan(&p);
    assert!(!plan.files.iter().any(|f| f.path.ends_with("base.yaml")));
    assert!(!plan.files.iter().any(|f| f.path.ends_with("bblayers.conf")));
    let note = plan
        .notes
        .iter()
        .find(|n| n.contains("no kas file was changed"))
        .unwrap_or_else(|| panic!("{:#?}", plan.notes));
    assert!(
        note.contains("is not a block mapping sous-chef can extend safely"),
        "{note}"
    );
    assert!(
        note.ends_with("repos:\n  proj:\n    layers:\n      meta-proj-new:\n"),
        "{note}"
    );
    assert_eq!(fs::read_to_string(&base).unwrap(), text);
}

/// What the engine does with the plan today: `check_plan` refuses the kas file, because it
/// lies outside every layer and the build directory (`scaffold::confine_writable`, which
/// this task does not own). Pinned so the gap is visible; see the T9.3 hand-off.
#[test]
fn the_engine_refuses_the_kas_file_for_now() {
    let p = Project::after();
    let ws = p.ws();
    let plan = plan(&p);
    let e = sous_chef::scaffold::check_plan(&ws, &plan).unwrap_err();
    assert!(e.to_string().contains("base.yaml"), "{e}");
}

#[test]
fn without_kas_bblayers_conf_is_modified_as_before() {
    let (_tmp, root) = common::temp_ws();
    let cfg = common::ws_config(&root.canonicalize().unwrap());
    let ws = Workspace::discover_in(&cfg, &KasEnv::default()).unwrap();
    assert!(ws.kas.is_none());
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let request = Request {
        kind: Kind::Layer,
        layer: None,
        name: Some("meta-new".into()),
        version: None,
        options: serde_json::Map::new(),
    };
    let plan = LayerGen
        .plan(&GenContext {
            ws: &ws,
            config: &cfg,
            index: &index,
            request: &request,
        })
        .unwrap();
    assert!(plan.files.iter().any(|f| f.path.ends_with("bblayers.conf")));
}
