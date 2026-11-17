//! T6.3 `layer` and `class`: snapshots, discovery of a generated layer, `find_class` on a
//! generated class (also where the target layer claims Kirkstone), refusals.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod scaffold_gen;

use std::fs;
use std::path::Path;

use scaffold_gen::*;
use serde_json::json;
use sous_chef::scaffold::{FileAction, Kind};
use sous_chef::workspace::resolve::find_class;

fn set_compat(root: &Path, compat: &str) {
    let p = root.join("meta-mine/conf/layer.conf");
    let text = fs::read_to_string(&p).unwrap().replace(
        "LAYERSERIES_COMPAT_mine = \"kirkstone scarthgap wrynose\"",
        &format!("LAYERSERIES_COMPAT_mine = \"{compat}\""),
    );
    fs::write(&p, text).unwrap();
}

// ---------------------------------------------------------------------------------------
// layer

#[test]
fn layer_defaults_snapshot() {
    let mut env = Env::new();
    let req = request(Kind::Layer, "meta-new", None, json!({}));
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("layer_defaults", text);
    assert_eq!(o.plan.new_layer, Some(env.path("meta-new")));
    // Default compat is the core layer's (wrynose), depends core, priority 6.
    assert!(
        text.contains("LAYERSERIES_COMPAT_meta-new = \"wrynose\""),
        "{text}"
    );
    assert!(text.contains("LAYERDEPENDS_meta-new = \"core\""), "{text}");
    assert!(text.contains("BBFILE_PRIORITY_meta-new = \"6\""), "{text}");
    assert!(matches!(
        o.plan.files.last().unwrap().action,
        FileAction::Modify { .. }
    ));
    assert!(!text.contains("COPYING.MIT"), "{text}");
}

#[test]
fn layer_with_every_option_snapshot() {
    let mut env = Env::new();
    let req = request(
        Kind::Layer,
        "meta-board",
        None,
        json!({
            "path": ".",
            "priority": 12,
            "depends": "core vendor",
            "compat": ["scarthgap", "wrynose"],
            "copying": true,
            "example": true,
        }),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("layer_all_options", text);
    assert!(
        text.contains("--- create meta-board:COPYING.MIT\n"),
        "{text}"
    );
    assert!(
        text.contains("--- create meta-board:recipes-example/.gitkeep\n"),
        "{text}"
    );
    assert!(
        text.contains("LAYERDEPENDS_meta-board = \"core vendor\""),
        "{text}"
    );
}

#[test]
fn a_written_layer_is_discovered() {
    let mut env = Env::new();
    let req = request(Kind::Layer, "meta-new", None, json!({"priority": "8"}));
    // The bblayers.conf change is linted (it is in the build directory); nothing at
    // warning or above.
    let o = env.write_clean(&req);
    let bblayers = fs::read_to_string(env.path("build/conf/bblayers.conf")).unwrap();
    assert!(
        bblayers.contains("  ${TOPDIR}/../meta-mine \\\n  ${TOPDIR}/../meta-new \\\n  \"\n"),
        "{bblayers}"
    );

    env.rediscover();
    let l = env.ws.layer("meta-new").expect("discovered");
    assert_eq!(l.root, env.path("meta-new"));
    assert_eq!(l.collections, ["meta-new"]);
    assert_eq!(l.priority, 8);
    assert_eq!(l.compat, ["wrynose"]);
    assert_eq!(l.depends, ["core"]);
    assert!(
        l.bbfiles
            .iter()
            .any(|b| b.ends_with("recipes-*/*/*.bbappend")),
        "{:?}",
        l.bbfiles
    );
    // The other layers are unchanged and in order.
    let names: Vec<&str> = env.ws.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["meta-core", "meta-vendor", "meta-mine", "meta-new"]);
    for f in &o.plan.files {
        if f.path.extension().is_some_and(|e| e == "conf") {
            assert_parses(&f.path.display().to_string(), &f.content);
        }
    }

    // Once listed as writable, the new layer passes lint (its files are now in the
    // workspace), and recipes can be scaffolded into it.
    env.cfg.writable_layers.push("meta-new".into());
    env.rediscover();
    env.index.refresh(&env.ws).unwrap();
    let r = sous_chef::lint::run(
        &env.ws,
        &env.index,
        &env.cfg,
        &sous_chef::lint::Target::Layer("meta-new".into()),
        &sous_chef::lint::Options::default(),
    )
    .unwrap();
    assert!(r.files > 0, "{:?}", r.notes);
    let bad: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.severity >= sous_chef::lint::Severity::Warning)
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
    let mut r = request(
        Kind::Recipe,
        "thing",
        Some("1.0"),
        json!({"category": "example", "license": "CLOSED", "source": "file://t.c"}),
    );
    r.layer = Some("meta-new".into());
    let o = env.write_clean(&r);
    assert!(
        o.checked[0]
            .display
            .starts_with("meta-new:recipes-example/thing/")
    );
}

/// Writes a layer, lists it in `bblayers.conf` (the plan does) and discovers the workspace
/// again, without making it writable: its files are then in the workspace and linted.
fn written_layer(env: &mut Env, name: &str, opts: serde_json::Value) {
    env.write_clean(&request(Kind::Layer, name, None, opts));
    env.rediscover();
    assert!(env.ws.layer(name).is_some(), "{name} discovered");
}

#[test]
fn a_generated_layer_conf_passes_the_real_rules() {
    let mut env = Env::new();
    written_layer(&mut env, "meta-new", json!({}));
    let conf = "meta-new/conf/layer.conf";
    let text = fs::read_to_string(env.path(conf)).unwrap();
    // A collection name with a dash, as a `_<collection>` suffix.
    for line in [
        "BBFILE_PATTERN_meta-new = \"^${LAYERDIR}/\"",
        "BBFILE_PRIORITY_meta-new = \"6\"",
        "LAYERSERIES_COMPAT_meta-new = \"wrynose\"",
    ] {
        assert!(text.contains(line), "{text}");
    }
    let legacy = env.lint_rule("override/legacy-syntax", &[conf]);
    assert!(legacy.is_empty(), "{legacy:?}");
    let layer_conf = env.lint_rule("org/layer-conf", &[conf]);
    assert!(layer_conf.is_empty(), "{layer_conf:?}");
    // Every default rule on the whole layer, not writable (named as the target).
    let bad = env.lint_default(&sous_chef::lint::Target::Layer("meta-new".into()));
    assert!(bad.is_empty(), "{bad:#?}");

    // Controls: the same rules do report a broken layer.conf of the same layer.
    fs::write(
        env.path(conf),
        text.replace("LAYERSERIES_COMPAT_meta-new = \"wrynose\"\n", "")
            .replace(
                "BBFILE_PRIORITY_meta-new",
                "BBFILE_PRIORITY_meta-new_append",
            ),
    )
    .unwrap();
    env.rediscover();
    let legacy = env.lint_rule("override/legacy-syntax", &[conf]);
    assert_eq!(legacy.len(), 1, "{legacy:?}");
    let layer_conf = env.lint_rule("org/layer-conf", &[conf]);
    assert!(
        layer_conf
            .iter()
            .any(|f| f.message.contains("LAYERSERIES_COMPAT")),
        "{layer_conf:?}"
    );
}

#[test]
fn a_generated_layer_with_every_option_passes_the_real_rules() {
    let mut env = Env::new();
    written_layer(
        &mut env,
        "meta-board",
        json!({
            "priority": 12,
            "depends": "core vendor",
            "compat": ["scarthgap", "wrynose"],
            "copying": true,
            "example": true,
        }),
    );
    // Named as the target: checked against scarthgap and wrynose.
    assert_eq!(
        sous_chef::lint::releases_for(&env.ws, "meta-board"),
        [
            sous_chef::release::Release::Scarthgap,
            sous_chef::release::Release::Wrynose
        ]
    );
    let conf = "meta-board/conf/layer.conf";
    assert!(env.lint_rule("override/legacy-syntax", &[conf]).is_empty());
    let layer_conf = env.lint_rule("org/layer-conf", &[conf]);
    assert!(layer_conf.is_empty(), "{layer_conf:?}");
    let bad = env.lint_default(&sous_chef::lint::Target::Layer("meta-board".into()));
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn layer_refusals() {
    let mut env = Env::new();
    for (name, opts, want) in [
        ("new", json!({}), "meta-<name>"),
        ("meta-New", json!({}), "meta-<name>"),
        ("meta-mine", json!({}), "already uses"),
        ("meta-x", json!({"priority": "high"}), "whole number"),
        ("meta-x", json!({"copying": "yes"}), "true or false"),
        (
            "meta-x",
            json!({"path": "nowhere"}),
            "not an existing directory",
        ),
        (
            "meta-x",
            json!({"path": "build"}),
            "overlaps an existing layer or the build directory",
        ),
        (
            "meta-x",
            json!({"path": "meta-mine"}),
            "overlaps an existing layer",
        ),
        (
            "meta-x",
            json!({"path": "meta-mine/conf"}),
            "overlaps an existing layer",
        ),
        ("meta-x", json!({"name": "meta-y"}), "unknown option `name`"),
    ] {
        let e = env
            .run(&request(Kind::Layer, name, None, opts.clone()), false)
            .unwrap_err();
        assert!(e.to_string().contains(want), "{name} {opts}: {e}");
    }
    // Collection clash: meta-core provides "core".
    let e = env
        .run(&request(Kind::Layer, "meta-core", None, json!({})), false)
        .unwrap_err();
    assert!(e.to_string().contains("already uses"), "{e}");
}

#[test]
fn layer_notes_unknown_depends_and_compat() {
    let mut env = Env::new();
    let req = request(
        Kind::Layer,
        "meta-x",
        None,
        json!({"depends": "nosuch", "compat": "dunfell potato"}),
    );
    let text = env.show(&req);
    assert!(text.contains("LAYERDEPENDS names nosuch"), "{text}");
    assert!(text.contains("dunfell, which is older"), "{text}");
    assert!(
        text.contains("potato, which sous-chef does not know"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------------------
// class

fn class_req(name: &str, opts: serde_json::Value) -> sous_chef::scaffold::Request {
    request(Kind::Class, name, None, opts)
}

fn full_class_opts() -> serde_json::Value {
    json!({
        "description": "Builds and deploys the widget documentation.",
        "tasks": ["configure", "install", "deploy_docs after do_install before do_build", {"name": "do_check_docs", "after": ["do_compile"]}],
        "variables": {"WIDGET_DOCS_FORMAT": "html", "WIDGET_DOCS_DIR": "${datadir}/doc/${PN}"},
    })
}

#[test]
fn class_on_a_layer_claiming_kirkstone_goes_to_classes() {
    let mut env = Env::new();
    let req = class_req("widget_docs", full_class_opts());
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("class_kirkstone_layer", text);
    assert!(
        text.starts_with("--- create meta-mine:classes/widget_docs.bbclass\n"),
        "{text}"
    );
    assert!(text.contains("claims kirkstone"), "{text}");
    assert!(
        text.contains("EXPORT_FUNCTIONS do_configure do_install do_deploy_docs do_check_docs"),
        "{text}"
    );
    assert!(
        text.contains("addtask deploy_docs after do_install before do_build"),
        "{text}"
    );
    assert!(
        text.contains("addtask check_docs after do_compile before do_build"),
        "{text}"
    );
    assert!(!text.contains("addtask configure"), "{text}");
    assert!(text.contains("WIDGET_DOCS_FORMAT ?= \"html\""), "{text}");
    env.write_clean(&req);
    let found = find_class(&env.ws, "widget_docs", false);
    assert_eq!(
        found,
        Some(env.path("meta-mine/classes/widget_docs.bbclass"))
    );
    assert_eq!(
        find_class(&env.ws, "widget_docs", true),
        found,
        "classes/ serves both scopes"
    );
}

#[test]
fn class_on_a_split_layer_goes_to_classes_recipe_or_global() {
    let mut env = Env::edited(|root| set_compat(root, "scarthgap wrynose"));
    let req = class_req("widget_docs", full_class_opts());
    let o = env.plan(&req);
    let text = env.render(&o);
    insta::assert_snapshot!("class_split_layer", text);
    assert!(
        text.starts_with("--- create meta-mine:classes-recipe/widget_docs.bbclass\n"),
        "{text}"
    );
    env.write_clean(&req);
    assert_eq!(
        find_class(&env.ws, "widget_docs", false),
        Some(env.path("meta-mine/classes-recipe/widget_docs.bbclass"))
    );
    assert_eq!(find_class(&env.ws, "widget_docs", true), None);

    let req = class_req(
        "site_policy",
        json!({"scope": "global", "variables": ["SITE_POLICY_LEVEL=strict", "SITE_POLICY_EXTRA"]}),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("class_global", text);
    assert!(
        text.starts_with("--- create meta-mine:classes-global/site_policy.bbclass\n"),
        "{text}"
    );
    assert!(text.contains("SITE_POLICY_EXTRA ?= \"\""), "{text}");
    assert!(text.contains("INHERIT += \"site_policy\""), "{text}");
    env.write_clean(&req);
    assert_eq!(
        find_class(&env.ws, "site_policy", true),
        Some(env.path("meta-mine/classes-global/site_policy.bbclass"))
    );
}

#[test]
fn class_in_a_kirkstone_workspace_is_found() {
    // Workspace release kirkstone and a layer claiming only kirkstone: classes/, and the
    // Kirkstone search (classes/ only) finds it.
    let mut env = Env::release("kirkstone", |root| set_compat(root, "kirkstone"));
    let req = class_req("widget_docs", json!({"tasks": ["compile"]}));
    let o = env.plan(&req);
    assert!(
        env.render(&o)
            .starts_with("--- create meta-mine:classes/widget_docs.bbclass\n")
    );
    env.write_clean(&req);
    assert_eq!(
        find_class(&env.ws, "widget_docs", false),
        Some(env.path("meta-mine/classes/widget_docs.bbclass"))
    );
    // The index sees it too.
    env.index.refresh(&env.ws).unwrap();
    let rows =
        sous_chef::index::query::class_files(&env.index, &env.ws, "widget_docs", false).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rank, Some(0));
}

/// The generated class passes `org/class-export-functions` and `org/class-location`, and
/// both rules report a broken copy of it in the same layer (so they really ran).
fn class_rules_accept(env: &mut Env, rel: &str) {
    let ef = env.lint_rule("org/class-export-functions", &[rel]);
    assert!(ef.is_empty(), "{ef:?}");
    let cl = env.lint_rule("org/class-location", &[rel]);
    assert!(cl.is_empty(), "{cl:?}");

    // Control: without its EXPORT_FUNCTIONS line, in another class.
    let text = fs::read_to_string(env.path(rel)).unwrap();
    assert!(text.contains("EXPORT_FUNCTIONS "), "{text}");
    let stripped: String = text
        .replace("widget_docs", "other_docs")
        .lines()
        .filter(|l| !l.starts_with("EXPORT_FUNCTIONS "))
        .map(|l| format!("{l}\n"))
        .collect();
    let dir = Path::new(rel).parent().unwrap();
    let other = dir.join("other_docs.bbclass");
    let other = other.to_str().unwrap();
    fs::write(env.path(other), stripped).unwrap();
    let ef = env.lint_rule("org/class-export-functions", &[other]);
    assert!(!ef.is_empty(), "control: no finding for {other}");
}

#[test]
fn generated_classes_pass_the_class_rules_with_and_without_kirkstone() {
    // meta-mine claims kirkstone: classes/.
    let mut env = Env::new();
    env.write_clean(&class_req("widget_docs", full_class_opts()));
    class_rules_accept(&mut env, "meta-mine/classes/widget_docs.bbclass");

    // meta-mine claims scarthgap wrynose: classes-recipe/. The same class in classes/ is
    // reported there by org/class-location.
    let mut env = Env::edited(|root| set_compat(root, "scarthgap wrynose"));
    env.write_clean(&class_req("widget_docs", full_class_opts()));
    let rel = "meta-mine/classes-recipe/widget_docs.bbclass";
    class_rules_accept(&mut env, rel);
    let moved = "meta-mine/classes/moved_docs.bbclass";
    fs::create_dir_all(env.path("meta-mine/classes")).unwrap();
    fs::write(
        env.path(moved),
        fs::read_to_string(env.path(rel))
            .unwrap()
            .replace("widget_docs", "moved_docs"),
    )
    .unwrap();
    let cl = env.lint_rule("org/class-location", &[moved]);
    assert_eq!(cl.len(), 1, "control: {cl:?}");

    // A global class there: classes-global/.
    env.write_clean(&class_req(
        "site_policy",
        json!({"scope": "global", "variables": ["SITE_POLICY_LEVEL=strict"]}),
    ));
    let rel = "meta-mine/classes-global/site_policy.bbclass";
    assert!(env.lint_rule("org/class-location", &[rel]).is_empty());
    assert!(
        env.lint_rule("org/class-export-functions", &[rel])
            .is_empty()
    );
}

#[test]
fn class_minimal_and_existing_name() {
    let mut env = Env::new();
    let req = class_req("autotoolsish", json!({}));
    let text = env.show(&req);
    assert!(
        text.contains(
            "a class autotoolsish already exists at meta-core:classes-recipe/autotoolsish.bbclass"
        ),
        "{text}"
    );
    assert!(!text.contains("EXPORT_FUNCTIONS"), "{text}");
    let o = env.plan(&req);
    assert_plan_parses(&o);
}

#[test]
fn class_refusals() {
    let mut env = Env::new();
    for (name, opts, want) in [
        ("Widget", json!({}), "lower-case"),
        ("my-class", json!({"tasks": ["compile"]}), "contains `-`"),
        ("w", json!({"scope": "machine"}), "recipe or global"),
        ("w", json!({"tasks": ["Bad"]}), "lower-case letters"),
        ("w", json!({"tasks": ["build"]}), "do_build"),
        ("w", json!({"tasks": ["x", "do_x"]}), "listed twice"),
        ("w", json!({"tasks": ["x do_y"]}), "after do_x before do_y"),
        ("w", json!({"variables": {"lower": "1"}}), "upper-case"),
        ("w", json!({"variables": {"A": ["1"]}}), "must be a string"),
    ] {
        let e = env.run(&class_req(name, opts.clone()), false).unwrap_err();
        assert!(e.to_string().contains(want), "{name} {opts}: {e}");
    }
    // A class with `-` and no tasks is fine.
    env.plan(&class_req(
        "my-class",
        json!({"variables": {"MY_CLASS_X": "1"}}),
    ));
}

#[test]
fn a_layer_elsewhere_is_refused_and_the_generators_are_registered() {
    let mut env = Env::new();
    let away = tempfile::tempdir().unwrap();
    let req = request(
        Kind::Layer,
        "meta-x",
        None,
        json!({"path": away.path().display().to_string()}),
    );
    let e = env.run(&req, false).unwrap_err();
    assert!(
        e.to_string().contains("already holds a workspace layer"),
        "{e}"
    );

    for kind in [Kind::Recipe, Kind::Bbappend, Kind::Layer, Kind::Class] {
        let g = sous_chef::scaffold::generator(kind).expect("registered");
        assert_eq!(g.kind(), kind);
    }
}
