//! T6.2 `recipe`: snapshots per build system and source type, release variants, the `.inc`
//! case, refusals, and the parse / lint rule on written output.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod scaffold_gen;

use scaffold_gen::*;
use serde_json::json;
use sous_chef::scaffold::Kind;

const GIT: &str = "https://github.com/example/widget.git";
const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REV: &str = "89abcdef0123456789abcdef0123456789abcdef";
const CHK: &str = "file://LICENSE;md5=0835ade698e0bcf8506ecda2f7b4f302";

fn git_opts(build: &str) -> serde_json::Value {
    json!({
        "category": "app",
        "source": GIT,
        "branch": "main",
        "srcrev": REV,
        "build": build,
        "license": "MIT",
        "lic_files_chksum": CHK,
        "summary": "A widget",
    })
}

/// Plans, snapshots, writes into the copy and checks parse and lint.
fn check(name: &str, snap: &str, version: &str, opts: serde_json::Value) -> String {
    let mut env = Env::new();
    let req = request(Kind::Recipe, name, Some(version), opts);
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!(snap, text);
    env.write_clean(&req);
    text
}

#[test]
fn build_systems_with_a_git_source() {
    for build in [
        "autotools",
        "cmake",
        "meson",
        "cargo",
        "setuptools",
        "setuptools_build_meta",
        "flit",
        "poetry",
        "hatchling",
        "go",
        "kernel-module",
        "none",
    ] {
        let text = check(
            "widget",
            &format!("recipe_git_{build}"),
            "1.2",
            git_opts(build),
        );
        assert!(text.contains("SRCREV = \""), "{text}");
        assert!(!text.contains("S = \"${WORKDIR}"), "{text}");
    }
}

#[test]
fn tarball_source_with_checksum() {
    let text = check(
        "gizmo",
        "recipe_tarball",
        "2.0",
        json!({
            "category": "app",
            "source": "https://example.org/releases/gizmo-2.0.tar.gz",
            "sha256sum": SHA,
            "build": "autotools",
            "license": "GPL-2.0-only",
            "lic_files_chksum": "file://COPYING;md5=b234ee4d69f5fce4486a80fdaf4a4263",
            "summary": "Gizmo tool",
            "homepage": "https://example.org/gizmo",
            "depends": "zlib openssl",
            "rdepends": ["bash"],
        }),
    );
    assert!(
        text.contains("https://example.org/releases/${BP}.tar.gz"),
        "{text}"
    );
    assert!(text.contains("RDEPENDS:${PN} += \"bash\""), "{text}");
}

#[test]
fn local_files_on_a_layer_spanning_the_unpackdir_change() {
    // meta-mine claims kirkstone scarthgap wrynose: no single S works, subdir=${BP} does.
    let text = check(
        "tool",
        "recipe_local_mixed",
        "0.1",
        json!({
            "category": "app",
            "source": "file://tool.c file://Makefile",
            "license": "CLOSED",
            "summary": "Local tool",
        }),
    );
    assert!(text.contains("file://tool.c;subdir=${BP}"), "{text}");
    assert!(!text.contains("\nS = "), "{text}");
}

#[test]
fn a_local_files_recipe_passes_the_release_rules_on_kirkstone_scarthgap_wrynose() {
    let mut env = Env::new();
    let req = request(
        Kind::Recipe,
        "tool",
        Some("0.1"),
        json!({"category": "app", "source": "file://tool.c file://Makefile", "license": "CLOSED", "summary": "t"}),
    );
    env.write_clean(&req);
    let rel = "meta-mine/recipes-app/tool/tool_0.1.bb";
    let releases = sous_chef::lint::releases_for(&env.ws, "meta-mine");
    assert_eq!(releases.len(), 3, "{releases:?}");
    for id in ["hygiene/release-syntax", "hygiene/workdir-s"] {
        let f = env.lint_rule(id, &[rel]);
        assert!(f.is_empty(), "{id}: {f:?}");
    }

    // Controls in the same layer: each rule reports what one of the claimed releases lacks.
    let text = std::fs::read_to_string(env.path(rel)).unwrap();
    let bad = "meta-mine/recipes-app/tool/tool_0.2.bb";
    std::fs::write(
        env.path(bad),
        format!("{text}S = \"${{WORKDIR}}\"\ninherit_defer native\n"),
    )
    .unwrap();
    let ws = env.lint_rule("hygiene/workdir-s", &[bad]);
    assert!(
        ws.iter().any(|f| f.message.contains("wrynose")),
        "control: {ws:?}"
    );
    let rs = env.lint_rule("hygiene/release-syntax", &[bad]);
    assert!(
        rs.iter().any(|f| f.message.contains("kirkstone")),
        "control: {rs:?}"
    );
}

#[test]
fn local_files_follow_the_layer_releases() {
    // Only Styhead and later: S = "${UNPACKDIR}".
    let mut env = Env::edited(|root| set_compat(root, "scarthgap wrynose"));
    let req = request(
        Kind::Recipe,
        "tool",
        Some("0.1"),
        json!({"category": "app", "source": "tool.c", "license": "CLOSED", "summary": "t"}),
    );
    // scarthgap still lacks UNPACKDIR: mixed.
    let o = env.plan(&req);
    assert!(env.render(&o).contains(";subdir=${BP}"));

    let mut env = Env::edited(|root| set_compat(root, "wrynose"));
    let o = env.plan(&req);
    let text = env.render(&o);
    insta::assert_snapshot!("recipe_local_wrynose", text);
    assert_plan_parses(&o);
    env.write_clean(&req);

    // A Kirkstone workspace and layer: S = "${WORKDIR}", which Kirkstone accepts.
    let mut env = Env::release("kirkstone", |root| set_compat(root, "kirkstone"));
    let o = env.plan(&req);
    let text = env.render(&o);
    assert!(text.contains("S = \"${WORKDIR}\""), "{text}");
    assert_plan_parses(&o);
    env.write_clean(&req);
    let rel = "meta-mine/recipes-app/tool/tool_0.1.bb";
    for id in ["hygiene/release-syntax", "hygiene/workdir-s"] {
        let f = env.lint_rule(id, &[rel]);
        assert!(f.is_empty(), "{id}: {f:?}");
    }
}

#[test]
fn go_follows_the_workspace_release_when_the_layer_spans_unpackdir() {
    let mut env = Env::new();
    let req = request(Kind::Recipe, "widget", Some("1.2"), git_opts("go"));
    let text = env.show(&req);
    assert!(
        text.contains("destsuffix=${GO_SRCURI_DESTSUFFIX}"),
        "{text}"
    );
    assert!(
        text.contains("will not work on kirkstone scarthgap"),
        "{text}"
    );

    env.write_clean(&req);

    let mut env = Env::release("kirkstone", |root| set_compat(root, "kirkstone"));
    let text = env.show(&req);
    assert!(!text.contains("destsuffix"), "{text}");
    assert!(!text.contains("will not work"), "{text}");
    env.write_clean(&req);
}

#[test]
fn missing_facts_are_comments_and_notes() {
    let mut env = Env::new();
    let req = request(
        Kind::Recipe,
        "widget",
        Some("1.2"),
        json!({"category": "app", "source": format!("{GIT};branch=dev"), "build": "cmake", "license": "MIT"}),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("recipe_missing_facts", text);
    assert!(
        text.contains("# sous-chef: supply LIC_FILES_CHKSUM"),
        "{text}"
    );
    assert!(text.contains("# sous-chef: supply SRCREV"), "{text}");
    assert!(text.contains("note: supply SRCREV"), "{text}");
    assert!(text.contains("branch=dev"), "{text}");
    // Written as it is, the facts the notes ask for are what lint reports, at warning: the
    // generator does not invent them and the rules do not let the gap pass.
    let o = env.run(&req, true).unwrap();
    let mut warned: Vec<(&str, String)> = o
        .lint
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .iter()
        .filter(|f| f.severity >= sous_chef::lint::Severity::Warning)
        .map(|f| (f.rule, f.message.clone()))
        .collect();
    warned.sort();
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert!(warned[0].1.contains("no LIC_FILES_CHKSUM"), "{warned:?}");
    assert_eq!(warned[1].0, "hygiene/src-uri-checksum", "{warned:?}");
    assert!(warned[1].1.contains("set SRCREV"), "{warned:?}");

    let req = request(
        Kind::Recipe,
        "gizmo",
        Some("2.0"),
        json!({"category": "app", "source": "https://example.org/gizmo-2.0.tar.xz", "license": "MIT"}),
    );
    let text = env.show(&req);
    assert!(
        text.contains("# sous-chef: supply SRC_URI[sha256sum]"),
        "{text}"
    );
    assert!(text.contains("note: supply SRC_URI[sha256sum]"), "{text}");
}

#[test]
fn a_new_version_requires_the_shared_inc() {
    // hello 1.0 and 1.1 in meta-core require hello.inc; a new version in meta-mine requires
    // it through BBPATH and carries only version data.
    let mut env = Env::new();
    let req = request(
        Kind::Recipe,
        "hello",
        Some("1.2"),
        json!({"sha256sum": SHA, "lic_files_chksum": "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302", "build": "cmake"}),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("recipe_with_inc", text);
    assert!(text.contains("--- create meta-mine:recipes-core/hello/hello_1.2.bb\nrequire recipes-core/hello/hello.inc\n"), "{text}");
    assert!(text.contains("ignored because"), "{text}");
    env.write_clean(&req);

    // The require resolves through BBPATH from the new file.
    let new = env.path("meta-mine/recipes-core/hello/hello_1.2.bb");
    let found = sous_chef::workspace::resolve::find_in_bbpath(
        &env.ws,
        "recipes-core/hello/hello.inc",
        &new,
    );
    assert_eq!(
        found,
        Some(env.path("meta-core/recipes-core/hello/hello.inc"))
    );

    // In the layer that holds the .inc, the plain name is used.
    let mut env = Env::new();
    let mut req = req.clone();
    req.layer = Some("meta-core".into());
    let e = env.run(&req, false).unwrap_err();
    assert!(e.to_string().contains("not writable"), "{e}");
}

#[test]
fn the_inc_in_the_target_layer_is_required_by_name() {
    let mut env = Env::edited(|root| {
        let dir = root.join("meta-mine/recipes-app/thing");
        std::fs::create_dir_all(&dir).unwrap();
        // A complete .inc, so the post-write lint of the new version judges only what the
        // generator wrote.
        std::fs::write(dir.join("thing.inc"), "SUMMARY = \"t\"\nLICENSE = \"MIT\"\nLIC_FILES_CHKSUM = \"file://LICENSE;md5=0835ade698e0bcf8506ecda2f7b4f302\"\nSRC_URI = \"git://example.org/t.git;protocol=https;branch=main\"\n").unwrap();
        std::fs::write(
            dir.join("thing_1.0.bb"),
            "require thing.inc\nSRCREV = \"aaaa\"\n",
        )
        .unwrap();
    });
    let req = request(Kind::Recipe, "thing", Some("1.1"), json!({"srcrev": REV}));
    let o = env.plan(&req);
    let text = env.render(&o);
    assert!(
        text.starts_with(
            "--- create meta-mine:recipes-app/thing/thing_1.1.bb\nrequire thing.inc\n"
        ),
        "{text}"
    );
    assert!(text.contains(&format!("SRCREV = \"{REV}\"")), "{text}");
    assert!(!text.contains("sha256sum"), "{text}");
    assert_plan_parses(&o);
    env.write_clean(&req);
}

#[test]
fn category_defaults_and_refusals() {
    let mut env = Env::new();
    // meta-mine has recipes-app and recipes-core: ambiguous.
    let req = request(
        Kind::Recipe,
        "widget",
        Some("1.0"),
        json!({"license": "CLOSED", "source": "file://w.c"}),
    );
    let e = env.run(&req, false).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("category"), "{msg}");
    assert!(
        msg.contains("recipes-app") && msg.contains("recipes-core"),
        "{msg}"
    );

    // `recipes-` prefix accepted.
    let mut r = req.clone();
    r.options.insert("category".into(), "recipes-net".into());
    let text = env.show(&r);
    assert!(
        text.starts_with("--- create meta-mine:recipes-net/widget/widget_1.0.bb"),
        "{text}"
    );

    for (name, version, opts, want) in [
        (
            "Widget",
            "1.0",
            json!({"category": "app", "license": "MIT"}),
            "lower-case",
        ),
        (
            "wid_get",
            "1.0",
            json!({"category": "app", "license": "MIT"}),
            "no `_`",
        ),
        (
            "widget",
            "1_0",
            json!({"category": "app", "license": "MIT"}),
            "must not contain",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app"}),
            "needs `license`",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "build": "scons"}),
            "unknown build system",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "source": GIT}),
            "needs a branch",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "source": "https://a/x.tar.gz https://b/y.tar.gz"}),
            "more than one remote",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "source": "https://a/x.tar.gz", "sha256sum": "abc"}),
            "64 hexadecimal",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "build": "go", "source": "https://a/x.tar.gz"}),
            "go_import",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "summary": "a\"b'c"}),
            "both quote",
        ),
        (
            "widget",
            "1.0",
            json!({"category": "app", "license": "MIT", "colour": "red"}),
            "unknown option `colour`",
        ),
    ] {
        let e = env
            .run(&request(Kind::Recipe, name, Some(version), opts), false)
            .unwrap_err();
        assert!(e.to_string().contains(want), "{name} {version}: {e}");
    }
    let mut r = request(
        Kind::Recipe,
        "widget",
        None,
        json!({"category": "app", "license": "MIT"}),
    );
    let e = env.run(&r, false).unwrap_err();
    assert!(e.to_string().contains("needs `version`"), "{e}");
    r.version = Some("1.0".into());
    r.name = None;
    let e = env.run(&r, false).unwrap_err();
    assert!(e.to_string().contains("needs `name`"), "{e}");
}

#[test]
fn the_class_check_uses_the_workspace() {
    // The fixture has autotoolsish, not autotools: the note says so.
    let mut env = Env::new();
    let req = request(Kind::Recipe, "widget", Some("1.2"), git_opts("autotools"));
    let text = env.show(&req);
    assert!(
        text.contains("class autotools was not found in the indexed layers"),
        "{text}"
    );
}

#[test]
fn an_existing_recipe_file_is_not_overwritten() {
    let mut env = Env::new();
    let req = request(
        Kind::Recipe,
        "app",
        Some("0.1"),
        json!({"license": "CLOSED", "source": "file://app.c"}),
    );
    let e = env.run(&req, true).unwrap_err();
    assert!(e.to_string().contains("already exists"), "{e}");
}

#[test]
fn a_recipe_that_exists_in_another_layer_gets_a_shadowing_note() {
    // B17: T9.2 planned `curl` into meta-kiss (priority 9) while OE-core (priority 5) has
    // curl_8.19.0.bb. Here: libfoo 2.0 is in meta-core (priority 5).
    let req = request(
        Kind::Recipe,
        "libfoo",
        Some("2.0"),
        json!({"sha256sum": SHA, "source": "https://example.org/libfoo-2.0.tar.gz", "license": "MIT", "lic_files_chksum": CHK}),
    );
    let note = |text: &str| -> String {
        text.lines()
            .find(|l| l.starts_with("note: libfoo already exists in another layer"))
            .unwrap_or_else(|| panic!("no shadowing note in\n{text}"))
            .to_string()
    };
    // meta-mine (priority 10) is higher: the new file wins and shadows the existing one.
    let mut env = Env::new();
    let text = env.show(&req);
    let n = note(&text);
    for want in [
        "meta-core:recipes-core/libfoo/libfoo_2.0.bb (priority 5)",
        "this layer (meta-mine) has priority 10",
        "BitBake would then build the new libfoo_2.0.bb",
        "so the new file shadows the existing libfoo",
        "scaffold kind=bbappend, or kind=where",
    ] {
        assert!(n.contains(want), "{want:?} in {n}");
    }
    // A plan, not a refusal.
    assert!(
        text.contains("--- create meta-mine:recipes-core/libfoo/libfoo_2.0.bb"),
        "{text}"
    );

    // A lower-priority layer: the existing recipe stays the one built.
    let set_priority = |p: &'static str| {
        move |root: &std::path::Path| {
            let conf = root.join("meta-mine/conf/layer.conf");
            let t = std::fs::read_to_string(&conf).unwrap().replace(
                "BBFILE_PRIORITY_mine = \"10\"",
                &format!("BBFILE_PRIORITY_mine = \"{p}\""),
            );
            std::fs::write(conf, t).unwrap();
        }
    };
    let mut env = Env::edited(set_priority("1"));
    let n = note(&env.show(&req));
    assert!(
        n.contains("BitBake would then build meta-core:recipes-core/libfoo/libfoo_2.0.bb"),
        "{n}"
    );
    assert!(n.contains("so the new file is not built"), "{n}");
    // The same priority and version: BitBake takes the first in its file order.
    let mut env = Env::edited(set_priority("5"));
    let n = note(&env.show(&req));
    assert!(
        n.contains(
            "one of the priority-5 files of version 2.0 (equal: the first in BBFILES order)"
        ),
        "{n}"
    );

    // A name no other layer has: no note.
    let mut env = Env::new();
    let mut other = req.clone();
    other.name = Some("brandnew".into());
    other.options.insert("category".into(), json!("app"));
    let text = env.show(&other);
    assert!(!text.contains("already exists in another layer"), "{text}");
}

/// Replaces the `LAYERSERIES_COMPAT_mine` value of the copy's meta-mine.
pub fn set_compat(root: &std::path::Path, compat: &str) {
    let p = root.join("meta-mine/conf/layer.conf");
    let text = std::fs::read_to_string(&p).unwrap();
    let text = text.replace(
        "LAYERSERIES_COMPAT_mine = \"kirkstone scarthgap wrynose\"",
        &format!("LAYERSERIES_COMPAT_mine = \"{compat}\""),
    );
    std::fs::write(&p, text).unwrap();
}
