//! T6.4 `inc_split`: a clean split of two versions (variants of the fixture `hello_1.0` /
//! `hello_1.1` written out in full in the writable layer), the before/after evaluation
//! equality check, refusals where order or `FILE` matters, and the parse / lint rule on
//! written output.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod scaffold_gen;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use scaffold_gen::*;
use serde_json::json;
use sous_chef::eval::{self, DataStore, Target};
use sous_chef::lint::Target as LintTarget;
use sous_chef::parse::{StatementKind, parse};
use sous_chef::scaffold::{FileAction, Kind};

const DIR: &str = "meta-mine/recipes-app/greet";

/// `hello.inc` plus `hello_1.0.bb`, as one file (no `.inc`), with comments.
const GREET_10: &str = r#"# Copyright example.org
# SPDX-License-Identifier: MIT

SUMMARY = "Greeting tool"
DESCRIPTION = "Prints a friendly greeting."
HOMEPAGE = "https://example.org/greet"
SECTION = "examples"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

DEPENDS = "virtual/foo"

SRC_URI = "https://example.org/releases/greet-${PV}.tar.gz"
SRC_URI[sha256sum] = "1111111111111111111111111111111111111111111111111111111111111111"

inherit autotoolsish

# Native language support is not wanted on the target.
EXTRA_OECONF = "--disable-nls"

do_install:append() {
	install -d ${D}${sysconfdir}

	echo "greeting=hello" > ${D}${sysconfdir}/greet.conf
}

FILES:${PN} += "${sysconfdir}/greet.conf"
"#;

/// `hello.inc` plus `hello_1.1.bb`: a version-specific patch and `PACKAGECONFIG`.
const GREET_11: &str = r#"# Copyright example.org
# SPDX-License-Identifier: MIT

SUMMARY = "Greeting tool"
DESCRIPTION = "Prints a friendly greeting."
HOMEPAGE = "https://example.org/greet"
SECTION = "examples"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

DEPENDS = "virtual/foo"

SRC_URI = "https://example.org/releases/greet-${PV}.tar.gz"
SRC_URI += "file://fix-greeting.patch"
SRC_URI[sha256sum] = "2222222222222222222222222222222222222222222222222222222222222222"

inherit autotoolsish

# Native language support is not wanted on the target.
EXTRA_OECONF = "--disable-nls"

do_install:append() {
	install -d ${D}${sysconfdir}

	echo "greeting=hello" > ${D}${sysconfdir}/greet.conf
}

FILES:${PN} += "${sysconfdir}/greet.conf"

PACKAGECONFIG ??= "greeting"
PACKAGECONFIG[greeting] = "--enable-greeting,--disable-greeting"
"#;

const PATCH: &str = "Fix the greeting.\n\nUpstream-Status: Pending\n\n--- a/x\n+++ b/x\n";

fn greet_env(v10: &str, v11: &str) -> Env {
    let (a, b) = (v10.to_string(), v11.to_string());
    Env::edited(move |root| {
        let dir = root.join(DIR);
        fs::create_dir_all(dir.join("files")).unwrap();
        fs::write(dir.join("greet_1.0.bb"), a).unwrap();
        fs::write(dir.join("greet_1.1.bb"), b).unwrap();
        fs::write(dir.join("files/fix-greeting.patch"), PATCH).unwrap();
    })
}

fn split(opts: serde_json::Value) -> sous_chef::scaffold::Request {
    request(Kind::IncSplit, "greet", None, opts)
}

fn evaluate(env: &Env, file: &str) -> DataStore {
    let target = Target {
        recipe: Some(format!("meta-mine:recipes-app/greet/{file}")),
        ..Target::default()
    };
    eval::evaluate(&env.index, &env.ws, &target).unwrap()
}

/// Text of the lines a pointer covers.
fn loc_text(env: &Env, loc: &sous_chef::types::Loc) -> String {
    let path = env.ws.abs_path(loc).unwrap();
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .skip(loc.lines.start as usize - 1)
        .take((loc.lines.end - loc.lines.start + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every variable's final value and flags, functions by the text of their pieces, and the
/// `addtask` / `deltask` statements of every file visited, in parse order.
fn snapshot(env: &Env, ds: &DataStore) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in ds.names() {
        let value = if ds.is_function(&name) {
            ds.function(&name)
                .unwrap_or_default()
                .iter()
                .map(|p| format!("{} -> {:?}", loc_text(env, &p.loc), p.calls))
                .collect::<Vec<_>>()
                .join("\n---\n")
        } else {
            ds.value(&name).map(|v| v.text).unwrap_or_default()
        };
        out.insert(name.clone(), value);
        for f in ds.flag_names(&name) {
            let v = ds.flag(&name, &f).map(|v| v.text).unwrap_or_default();
            out.insert(format!("{name}[{f}]"), v);
        }
    }
    let mut tasks = Vec::new();
    for visit in ds.files() {
        let text = fs::read_to_string(env.ws.abs_path(&visit.loc).unwrap()).unwrap();
        for s in parse(&text).statements {
            if matches!(
                s.kind,
                StatementKind::AddTask { .. } | StatementKind::DelTask { .. }
            ) {
                tasks.push(text[s.bytes].trim().to_string());
            }
        }
    }
    out.insert("<tasks>".into(), tasks.join("\n"));
    out
}

#[test]
fn clean_split_of_two_versions() {
    let mut env = greet_env(GREET_10, GREET_11);
    let req = split(json!({}));
    let o = env.plan(&req);
    assert_plan_parses(&o);
    let text = env.render(&o);
    insta::assert_snapshot!("inc_split_clean", text);

    assert_eq!(o.plan.files.len(), 3, "{text}");
    assert_eq!(o.plan.files[0].action, FileAction::Create);
    assert!(o.plan.files[0].path.ends_with("greet/greet.inc"));
    for f in &o.plan.files[1..] {
        assert!(matches!(f.action, FileAction::Modify { .. }), "{f:?}");
    }
    let inc = &o.plan.files[0].content;
    // Comments attached to moved statements move with them; version data does not move.
    assert!(
        inc.contains("# Native language support is not wanted on the target.\nEXTRA_OECONF"),
        "{inc}"
    );
    for absent in [
        "LIC_FILES_CHKSUM",
        "sha256sum",
        "fix-greeting",
        "PACKAGECONFIG",
        "Copyright",
    ] {
        assert!(!inc.contains(absent), "{absent} in the .inc:\n{inc}");
    }
    let bb11 = &o.plan.files[1].content;
    assert!(bb11.contains("greet_1.1") || o.plan.files[1].path.ends_with("greet_1.1.bb"));
    assert!(bb11.starts_with("# Copyright example.org\n"), "{bb11}");
    assert!(bb11.contains("require greet.inc\n"), "{bb11}");
    assert!(
        bb11.contains("SRC_URI += \"file://fix-greeting.patch\""),
        "{bb11}"
    );
    assert!(!bb11.contains("Native language"), "{bb11}");
}

#[test]
fn the_split_keeps_every_value_and_the_task_list() {
    let mut env = greet_env(GREET_10, GREET_11);
    env.index.refresh(&env.ws).unwrap();
    let before: Vec<BTreeMap<String, String>> = ["greet_1.0.bb", "greet_1.1.bb"]
        .iter()
        .map(|f| snapshot(&env, &evaluate(&env, f)))
        .collect();
    assert_eq!(before[0]["SUMMARY"], "Greeting tool");
    assert!(before[1]["SRC_URI"].contains("file://fix-greeting.patch"));

    env.write_clean(&split(json!({})));
    assert!(env.path(&format!("{DIR}/greet.inc")).exists());
    env.index.refresh(&env.ws).unwrap();
    let after: Vec<BTreeMap<String, String>> = ["greet_1.0.bb", "greet_1.1.bb"]
        .iter()
        .map(|f| snapshot(&env, &evaluate(&env, f)))
        .collect();
    for (b, a) in before.iter().zip(&after) {
        let mut diff = Vec::new();
        for k in b.keys().chain(a.keys()) {
            if b.get(k) != a.get(k) {
                diff.push(format!("{k}: {:?} -> {:?}", b.get(k), a.get(k)));
            }
        }
        diff.dedup();
        assert!(diff.is_empty(), "values changed:\n{}", diff.join("\n"));
    }
    // The split really happened: the `.inc` is in each recipe's tree.
    let ds = evaluate(&env, "greet_1.0.bb");
    assert!(
        ds.files().iter().any(|v| v.loc.path.ends_with("greet.inc")),
        "{:?}",
        ds.files()
    );
}

#[test]
fn duplicate_versions_fires_before_and_not_after() {
    let mut env = greet_env(GREET_10, GREET_11);
    let files = [
        "meta-mine/recipes-app/greet/greet_1.0.bb",
        "meta-mine/recipes-app/greet/greet_1.1.bb",
    ];
    let control = env.lint_rule("org/duplicate-versions", &files);
    assert_eq!(control.len(), 1, "control: {control:?}");
    assert!(control[0].message.contains("greet.inc"), "{control:?}");

    let o = env.write_clean(&split(json!({})));
    assert_eq!(o.checked.len(), 3);
    let after = env.lint_rule("org/duplicate-versions", &files);
    assert!(after.is_empty(), "{after:?}");
    let inc = "meta-mine/recipes-app/greet/greet.inc";
    for id in ["org/inc-has-version-data", "org/include-must-exist"] {
        let f = env.lint_rule(id, &[inc, files[0], files[1]]);
        assert!(f.is_empty(), "{id}: {f:?}");
    }
    let f = env.lint_rule("org/inc-unused", &[inc]);
    assert!(f.is_empty(), "{f:?}");
    // The whole layer, default rules: nothing at warning or above in the three files.
    let bad: Vec<String> = env
        .lint_default(&LintTarget::Layer("meta-mine".into()))
        .into_iter()
        .filter(|f| f.contains("recipes-app/greet/"))
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn a_version_specific_assignment_before_a_shared_append_is_refused() {
    // GREET_FLAGS is set per version, then `+=` the same way in both: moving the `+=` into
    // the `.inc` would run it before the `=` and lose it.
    let v10 = GREET_10.replace(
        "inherit autotoolsish\n",
        "inherit autotoolsish\n\nGREET_FLAGS = \"--old\"\nGREET_FLAGS += \"--shared\"\n",
    );
    let v11 = GREET_11.replace(
        "inherit autotoolsish\n",
        "inherit autotoolsish\n\nGREET_FLAGS = \"--new\"\nGREET_FLAGS += \"--shared\"\n",
    );
    let mut env = greet_env(&v10, &v11);
    let o = env.plan(&split(json!({})));
    let text = env.render(&o);
    insta::assert_snapshot!("inc_split_refused_order", text);
    assert!(o.plan.files.is_empty(), "{text}");
    assert!(text.contains("nothing planned"), "{text}");
    assert!(text.contains("both set GREET_FLAGS"), "{text}");
    // A write of an empty plan writes nothing.
    let w = env.run(&split(json!({})), true).unwrap();
    assert!(!w.written);
    assert!(!env.path(&format!("{DIR}/greet.inc")).exists());

    // Control for the equality check: the naive split (the `+=` moved into the .inc ahead
    // of the version's `=`) does change the value, and the evaluation sees it.
    env.index.refresh(&env.ws).unwrap();
    let before = snapshot(&env, &evaluate(&env, "greet_1.0.bb"));
    assert_eq!(before["GREET_FLAGS"], "--old --shared");
    let dir = env.path(DIR);
    fs::write(
        dir.join("greet.inc"),
        "inherit autotoolsish\nGREET_FLAGS += \"--shared\"\n",
    )
    .unwrap();
    let naive = v10
        .replace("inherit autotoolsish\n", "require greet.inc\n")
        .replace("GREET_FLAGS += \"--shared\"\n", "");
    fs::write(dir.join("greet_1.0.bb"), naive).unwrap();
    env.index.refresh(&env.ws).unwrap();
    let after = snapshot(&env, &evaluate(&env, "greet_1.0.bb"));
    assert_eq!(after["GREET_FLAGS"], "--old");
}

#[test]
fn an_immediate_expansion_of_pn_is_not_moved_into_the_inc() {
    // Inside an .inc, PN is defaultpkgname while it is parsed: `:=` of ${PN} must stay.
    let line = "FILESEXTRAPATHS:prepend := \"${THISDIR}/${PN}:\"\n";
    let v10 = GREET_10.replace("DEPENDS =", &format!("{line}DEPENDS ="));
    let v11 = GREET_11.replace("DEPENDS =", &format!("{line}DEPENDS ="));
    let mut env = greet_env(&v10, &v11);
    let o = env.plan(&split(json!({})));
    let text = env.render(&o);
    assert!(o.plan.files.is_empty(), "{text}");
    assert!(text.contains("expands PN immediately"), "{text}");

    // `${THISDIR}` alone is the same in the .inc: that one moves.
    let line = "FILESEXTRAPATHS:prepend := \"${THISDIR}/files:\"\n";
    let v10 = GREET_10.replace("DEPENDS =", &format!("{line}DEPENDS ="));
    let v11 = GREET_11.replace("DEPENDS =", &format!("{line}DEPENDS ="));
    let mut env = greet_env(&v10, &v11);
    let o = env.plan(&split(json!({})));
    assert_eq!(o.plan.files.len(), 3, "{}", env.render(&o));
    assert!(o.plan.files[0].content.contains("${THISDIR}/files:"));
    env.write_clean(&split(json!({})));
}

#[test]
fn a_differing_comment_stays_in_its_bb() {
    let v10 = GREET_10.replace(
        "# Native language support is not wanted on the target.",
        "# No NLS for 1.0.",
    );
    let mut env = greet_env(&v10, GREET_11);
    let o = env.plan(&split(json!({})));
    let text = env.render(&o);
    assert_eq!(o.plan.files.len(), 3, "{text}");
    let bb10 = o
        .plan
        .files
        .iter()
        .find(|f| f.path.ends_with("greet_1.0.bb"))
        .unwrap();
    assert!(
        bb10.content.contains("# No NLS for 1.0.\n"),
        "{}",
        bb10.content
    );
    assert!(text.contains("this comment differs"), "{text}");
    env.write_clean(&split(json!({})));
}

#[test]
fn refusals_and_errors() {
    // Nothing in common.
    let mut env = greet_env(
        "LICENSE = \"MIT\"\nLIC_FILES_CHKSUM = \"file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302\"\n",
        "LICENSE = \"CLOSED\"\n",
    );
    let o = env.plan(&split(json!({})));
    assert!(o.plan.files.is_empty());
    assert!(env.render(&o).contains("share no statement"));

    // An existing greet.inc.
    let mut env = greet_env(GREET_10, GREET_11);
    fs::write(env.path(&format!("{DIR}/greet.inc")), "SECTION = \"x\"\n").unwrap();
    let o = env.plan(&split(json!({})));
    assert!(o.plan.files.is_empty());
    assert!(env.render(&o).contains("greet.inc already exists"));

    // One version only.
    let mut env = greet_env(GREET_10, GREET_11);
    let e = env
        .run(&split(json!({"versions": ["1.0"]})), false)
        .unwrap_err();
    assert!(e.to_string().contains("two or more versions"), "{e}");
    let e = env
        .run(&split(json!({"versions": "1.0 9.9"})), false)
        .unwrap_err();
    assert!(e.to_string().contains("no version 9.9"), "{e}");

    // A recipe only in a layer that is not writable.
    let e = env
        .run(&split(json!({"recipe": "hello"})), false)
        .unwrap_err();
    assert!(e.to_string().contains("no version in meta-mine"), "{e}");

    // Unknown option.
    let e = env.run(&split(json!({"inc": "x"})), false).unwrap_err();
    assert!(
        e.to_string().contains("valid options: recipe, versions"),
        "{e}"
    );
}

#[test]
fn versions_in_different_directories_are_refused() {
    let mut env = greet_env(GREET_10, GREET_11);
    let other = env.path("meta-mine/recipes-app/greet-old");
    fs::create_dir_all(&other).unwrap();
    fs::rename(
        env.path(&format!("{DIR}/greet_1.0.bb")),
        other.join("greet_1.0.bb"),
    )
    .unwrap();
    let o = env.plan(&split(json!({})));
    assert!(o.plan.files.is_empty());
    assert!(
        env.render(&o).contains("different directories"),
        "{}",
        env.render(&o)
    );
    assert!(Path::new(&other).exists());
}
