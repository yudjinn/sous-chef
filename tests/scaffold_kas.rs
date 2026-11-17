//! T9.3: `scaffold kind=layer` in a kas project adds the layer to a kas file (a `Modify`
//! that inserts one line and keeps comments, order and formatting) instead of changing
//! `bblayers.conf`; when the file cannot be edited safely the lines are in the notes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;

use kas_support::Project;
use serde_json::json;
use sous_chef::error::Error;
use sous_chef::index::Index;
use sous_chef::scaffold::layer::LayerGen;
use sous_chef::scaffold::{
    FileAction, GenContext, Generator, Kind, Plan, Request, apply_plan, check_plan,
};
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

/// The engine accepts the kas file edit of a plan that creates a layer, shows it relative
/// to the kas project root, and applies it: the kas file is the old file plus the one
/// inserted entry, byte for byte.
#[test]
fn the_engine_accepts_and_applies_the_kas_file_edit() {
    let p = Project::after();
    let ws = p.ws();
    let before = fs::read(p.path("kas/base.yaml")).unwrap();
    let plan = plan(&p);
    let checked = check_plan(&ws, &plan).unwrap();
    let shown: Vec<&str> = checked.iter().map(|c| c.display.as_str()).collect();
    assert_eq!(
        shown,
        [
            "meta-proj-new:conf/layer.conf",
            "meta-proj-new:README.md",
            "kas/base.yaml"
        ]
    );
    apply_plan(&plan, &checked).unwrap();
    let after = fs::read(p.path("kas/base.yaml")).unwrap();
    let entry = b"      meta-proj-new:\n";
    let anchor = b"  proj:\n    layers:\n      meta-proj:\n";
    let at = before
        .windows(anchor.len())
        .position(|w| w == anchor)
        .unwrap()
        + anchor.len();
    let mut want = before[..at].to_vec();
    want.extend_from_slice(entry);
    want.extend_from_slice(&before[at..]);
    assert_eq!(after, want);
    assert!(p.path("meta-proj-new/conf/layer.conf").is_file());
    // bblayers.conf, which kas generates, is untouched.
    assert_eq!(
        fs::read_to_string(p.path("build/conf/bblayers.conf")).unwrap(),
        fs::read_to_string(common::fixture_path(
            "kas/checkout/build/conf/bblayers.conf"
        ))
        .unwrap()
    );
}

/// `plan` with its kas file edit replaced by a `Modify` of `path` (the current sha, so only
/// confinement can refuse it).
fn modify_instead(plan: &Plan, path: std::path::PathBuf) -> Plan {
    let mut plan = plan.clone();
    let sha = sous_chef::scaffold::sha256_hex(&fs::read(&path).unwrap());
    let edit = plan
        .files
        .iter_mut()
        .find(|f| f.path.ends_with("kas/base.yaml"))
        .unwrap();
    edit.path = path;
    edit.action = FileAction::Modify { before_sha: sha };
    plan
}

fn assert_confined(ws: &Workspace, plan: &Plan, what: &str) {
    match check_plan(ws, plan) {
        Err(Error::Confined { .. }) => {}
        other => panic!("{what}: expected a confinement refusal, got {other:?}"),
    }
}

#[test]
fn the_engine_still_refuses_everything_else_in_a_kas_project() {
    let p = Project::after();
    let ws = p.ws();
    let plan = plan(&p);

    // A file in the project that kas did not read.
    fs::write(p.path("kas/other.yaml"), "header:\n  version: 11\n").unwrap();
    assert_confined(
        &ws,
        &modify_instead(&plan, p.path("kas/other.yaml")),
        "unread",
    );
    fs::write(p.path("notes.txt"), "x\n").unwrap();
    assert_confined(
        &ws,
        &modify_instead(&plan, p.path("notes.txt")),
        "other file",
    );

    // A lock file, although kas read it.
    let lock = p.path("kas/base.lock.yaml");
    assert!(ws.kas.as_ref().unwrap().lock_files.contains(&lock));
    assert_confined(&ws, &modify_instead(&plan, lock), "lock file");

    // A file kas read that lies outside the project root.
    let outside = p._tmp.path().join("elsewhere.yaml");
    fs::write(&outside, "header:\n  version: 11\n").unwrap();
    let mut ws_out = p.ws();
    ws_out.kas.as_mut().unwrap().read.push(outside.clone());
    assert_confined(&ws_out, &modify_instead(&plan, outside), "outside the root");

    // A Create of a kas file, even one the project read.
    let mut create = plan.clone();
    for f in &mut create.files {
        if f.path.ends_with("kas/base.yaml") {
            f.action = FileAction::Create;
        }
    }
    assert_confined(&ws, &create, "create");

    // The same kas file edit in a plan that creates no layer.
    let mut no_layer = plan.clone();
    no_layer.new_layer = None;
    no_layer.files.retain(|f| f.path.ends_with("kas/base.yaml"));
    assert_confined(&ws, &no_layer, "no new_layer");

    // The before_sha check applies as for any Modify.
    let mut stale = plan.clone();
    for f in &mut stale.files {
        if let FileAction::Modify { before_sha } = &mut f.action {
            *before_sha = "0".repeat(64);
        }
    }
    let e = check_plan(&ws, &stale).unwrap_err();
    assert!(e.to_string().contains("kas/base.yaml changed since"), "{e}");
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
