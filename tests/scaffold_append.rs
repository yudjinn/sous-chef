//! T6.2 `bbappend`: each option, the existing-append case, refusals, and the parse / lint
//! rule on written output.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod scaffold_gen;

use std::fs;

use scaffold_gen::*;
use serde_json::json;
use sous_chef::scaffold::{FileAction, Kind};

fn append(name: &str, opts: serde_json::Value) -> sous_chef::scaffold::Request {
    request(Kind::Bbappend, name, None, opts)
}

#[test]
fn set_with_wildcard_default_mirrors_the_recipe_directory() {
    let mut env = Env::new();
    let req = append(
        "libfoo",
        json!({"set": {"EXTRA_OECONF:append": "--enable-bar", "PACKAGECONFIG:append": "ssl"}}),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("append_set_wildcard", text);
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/libfoo/libfoo_%.bbappend\n"),
        "{text}"
    );
    assert!(
        text.contains("EXTRA_OECONF:append = \" --enable-bar\""),
        "{text}"
    );
    env.write_clean(&req);
}

#[test]
fn set_as_lines_and_operators() {
    let mut env = Env::new();
    let req = append(
        "libfoo",
        json!({
            "version": "exact",
            "set": [
                "EXTRA_OECONF += \"--with-x\"",
                "RDEPENDS:${PN}:append = \"bash\"",
                "MYVAR ?= \"1\"",
                "do_install[depends] += \"foo:do_populate_sysroot\"",
            ],
        }),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("append_set_lines_exact", text);
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/libfoo/libfoo_2.0.bbappend\n"),
        "{text}"
    );
    assert!(text.contains("RDEPENDS:${PN}:append = \" bash\""), "{text}");
    env.write_clean(&req);
}

#[test]
fn files_add_filesextrapaths_and_src_uri_append() {
    let mut env = Env::new();
    let req = append(
        "libfoo",
        json!({
            "files": ["0001-fix-build.patch", {"name": "defaults.cfg", "content": "key=value\n"}],
        }),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("append_files", text);
    // A patch: exact version by default.
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/libfoo/libfoo_2.0.bbappend\n"),
        "{text}"
    );
    assert!(
        text.contains("FILESEXTRAPATHS:prepend := \"${THISDIR}/${PN}:\"\n"),
        "{text}"
    );
    assert!(
        text.contains("SRC_URI:append = \" file://0001-fix-build.patch file://defaults.cfg\""),
        "{text}"
    );
    assert!(
        text.contains("--- create meta-mine:recipes-core/libfoo/libfoo/defaults.cfg\nkey=value\n"),
        "{text}"
    );
    assert!(text.contains("note: copy 0001-fix-build.patch to meta-mine:recipes-core/libfoo/libfoo/0001-fix-build.patch"), "{text}");
    env.write_clean(&req);
    assert_eq!(
        fs::read_to_string(env.path("meta-mine/recipes-core/libfoo/libfoo/defaults.cfg")).unwrap(),
        "key=value\n"
    );
}

#[test]
fn machine_makes_every_change_conditional() {
    let mut env = Env::new();
    let req = append(
        "libfoo",
        json!({
            "version": "wildcard",
            "machine": "boardx",
            "files": ["board.cfg"],
            "set": ["EXTRA_OECONF += \"--board\"", "FOO = \"1\"", "BAR:append = \"x\"", "BAZ =+ \"y\""],
        }),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("append_machine", text);
    assert!(
        text.contains("SRC_URI:append:boardx = \" file://board.cfg\""),
        "{text}"
    );
    assert!(
        text.contains("EXTRA_OECONF:append:boardx = \" --board\""),
        "{text}"
    );
    assert!(text.contains("FOO:boardx = \"1\""), "{text}");
    assert!(text.contains("BAZ:prepend:boardx = \"y \""), "{text}");
    assert!(!text.contains(":boardx +="), "{text}");
    assert!(!text.contains("has no conf/machine"), "{text}");
    env.write_clean(&req);

    // An unknown machine is a note.
    let mut env = Env::new();
    let req = append("libfoo", json!({"machine": "nosuch", "set": {"FOO": "1"}}));
    assert!(
        env.show(&req)
            .contains("machine nosuch has no conf/machine/nosuch.conf")
    );
}

#[test]
fn version_prefix_and_explicit_recipe_version() {
    let mut env = Env::new();
    // hello has 1.0 and 1.1 in meta-core; prefix 1.% picks a match.
    let req = append("hello", json!({"version": "1.%", "set": {"FOO": "1"}}));
    let text = env.show(&req);
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/hello/hello_1.%.bbappend\n"),
        "{text}"
    );

    // version 1.0, exact: a new file (the existing one is for 1.1).
    let mut req = append("hello", json!({"version": "exact", "set": {"FOO": "1"}}));
    req.version = Some("1.0".into());
    let text = env.show(&req);
    assert!(
        text.starts_with("--- create meta-mine:recipes-core/hello/hello_1.0.bbappend\n"),
        "{text}"
    );
    env.write_clean(&req);

    let req = append("hello", json!({"version": "2.%", "set": {"FOO": "1"}}));
    let e = env.run(&req, false).unwrap_err();
    assert!(
        e.to_string().contains("versions: 1.1, 1.0") || e.to_string().contains("versions:"),
        "{e}"
    );
    let req = append("hello", json!({"version": "newest", "set": {"FOO": "1"}}));
    let e = env.run(&req, false).unwrap_err();
    assert!(e.to_string().contains("exact, wildcard or a prefix"), "{e}");
}

#[test]
fn an_existing_append_is_modified() {
    let mut env = Env::new();
    let file = env.path("meta-mine/recipes-core/hello/hello_1.1.bbappend");
    let before = fs::read_to_string(&file).unwrap();
    let req = append(
        "hello",
        json!({"files": ["mine.patch", "extra.patch"], "set": {"EXTRA_OECONF:append": "--more"}}),
    );
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("append_modify_existing", text);
    assert_eq!(o.plan.files.len(), 1);
    let FileAction::Modify { before_sha } = &o.plan.files[0].action else {
        panic!("expected a Modify: {text}")
    };
    assert_eq!(
        before_sha,
        &sous_chef::scaffold::file_sha256(&file).unwrap()
    );
    // Not repeated: FILESEXTRAPATHS and mine.patch are already there.
    assert_eq!(text.matches("FILESEXTRAPATHS").count(), 1, "{text}");
    assert!(
        text.contains("note: file://mine.patch is already in the bbappend"),
        "{text}"
    );
    assert!(
        text.contains("SRC_URI:append = \" file://extra.patch\""),
        "{text}"
    );
    let o = env.write_clean(&req);
    assert!(o.written);
    let after = fs::read_to_string(&file).unwrap();
    assert!(after.starts_with(&before), "{after}");

    // With `version` naming the same file, also a Modify.
    let mut env = Env::new();
    let req = append("hello", json!({"version": "exact", "set": {"X": "1"}}));
    let o = env.plan(&req);
    assert!(matches!(o.plan.files[0].action, FileAction::Modify { .. }));
}

#[test]
fn refusals() {
    let mut env = Env::new();
    for (opts, want) in [
        (json!({}), "nothing to append"),
        (json!({"set": {"RDEPENDS": "bash"}}), "package-scoped"),
        (
            json!({"set": ["RDEPENDS_${PN} += \"bash\""]}),
            "old `_` override syntax",
        ),
        (json!({"set": ["FOO:boardx += \"x\""]}), "use FOO:append"),
        (json!({"set": {"DISTRO_FEATURES:append": "x11"}}), "policy"),
        (
            json!({"set": {"PREFERRED_PROVIDER_virtual/kernel": "x"}}),
            "policy",
        ),
        (
            json!({"set": {"FILESEXTRAPATHS:prepend": "x:"}}),
            "added by `files`",
        ),
        (
            json!({"set": ["do_install() {"]}),
            "not one BitBake assignment",
        ),
        (
            json!({"set": ["A = \"1\"\nB = \"2\""]}),
            "not one BitBake assignment",
        ),
        (json!({"files": ["../escape.patch"]}), "relative path"),
        (json!({"files": ["/abs.patch"]}), "relative path"),
        (
            json!({"machine": "BoardX", "set": {"A": "1"}}),
            "lower-case",
        ),
        (json!({"machine": "boardx", "set": {"A[doc]": "1"}}), "flag"),
    ] {
        let e = env.run(&append("libfoo", opts.clone()), false).unwrap_err();
        assert!(e.to_string().contains(want), "{opts}: {e}");
    }
    let e = env
        .run(&append("nosuchrecipe", json!({"set": {"A": "1"}})), false)
        .unwrap_err();
    assert!(e.to_string().contains("recipe nosuchrecipe"), "{e}");
    let e = env
        .run(&append("hell", json!({"set": {"A": "1"}})), false)
        .unwrap_err();
    assert!(e.to_string().contains("similar: hello"), "{e}");
}

#[test]
fn appending_to_a_recipe_of_the_same_layer_is_noted() {
    let mut env = Env::new();
    let req = append("app", json!({"set": {"EXTRA_OEMAKE:append": "V=1"}}));
    let text = env.show(&req);
    assert!(
        text.starts_with("--- create meta-mine:recipes-app/app/app_%.bbappend\n"),
        "{text}"
    );
    assert!(text.contains("is in meta-mine itself"), "{text}");
}
