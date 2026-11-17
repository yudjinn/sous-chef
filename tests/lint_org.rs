//! T4.3: the seventeen rules of the `org` lint group, on their fixtures in
//! `tests/fixtures/lint/org/<rule>/`.
//!
//! Layer-scope rules, and file-scope rules that need the index, run through the real engine
//! (`lint::run_with`) on a temporary workspace made of the fixture layers (no build
//! directory; the layers are listed in the configuration). File-scope rules that need only
//! the text run through `lint::check_text`. Every test asserts the exact findings
//! (severity, rule, pointer) on `bad` and none on `good`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use common::*;
use sous_chef::config::Config;
use sous_chef::error::Result;
use sous_chef::eval::{self, DataStore};
use sous_chef::index::Index;
use sous_chef::lint::rules::{
    org_append_missing_filesextrapaths::OrgAppendMissingFilesextrapaths,
    org_append_no_match::OrgAppendNoMatch, org_append_wildcard_broad::OrgAppendWildcardBroad,
    org_class_export_functions::OrgClassExportFunctions, org_class_location::OrgClassLocation,
    org_class_recipe_specific::OrgClassRecipeSpecific,
    org_duplicate_versions::OrgDuplicateVersions, org_inc_has_version_data::OrgIncHasVersionData,
    org_inc_unused::OrgIncUnused, org_include_must_exist::OrgIncludeMustExist,
    org_inherit_defer::OrgInheritDefer, org_layer_conf::OrgLayerConf,
    org_policy_in_recipe::OrgPolicyInRecipe,
    org_recipe_change_in_local_conf::OrgRecipeChangeInLocalConf, org_recipe_path::OrgRecipePath,
    org_repeated_logic::OrgRepeatedLogic, org_upstream_edit::OrgUpstreamEdit,
};
use sous_chef::lint::{self, Evaluate, Finding, Options, Rule, Target};
use sous_chef::types::{FileKind, LineSpan, Loc};
use sous_chef::workspace::Workspace;

// ---------------------------------------------------------------------------------------
// Harness

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    config: Config,
    ws: Workspace,
    index: Index,
}

/// No evaluator: the org rules are file and layer scope.
struct NoEval;

impl Evaluate for NoEval {
    fn evaluate(&self, _: &Index, _: &Workspace, _: &eval::Target) -> Result<Option<DataStore>> {
        Ok(None)
    }
}

/// A workspace of the fixture layers `tests/fixtures/lint/org/<rule>/<layer>` (in this
/// order), every one writable unless named in `upstream`. `release`: `release` in the
/// configuration (`None`: detected from the layers).
fn env_with(
    rule: &str,
    layers: &[&str],
    upstream: &[&str],
    release: Option<&str>,
    prepare: impl FnOnce(&Path),
) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    for l in layers {
        copy_dir(
            &fixture_path(&format!("lint/org/{rule}/{l}")),
            &root.join(l),
        );
    }
    let root = fs::canonicalize(root).unwrap();
    prepare(&root);
    let mut config = Config {
        layers: layers.iter().map(PathBuf::from).collect(),
        writable_layers: layers
            .iter()
            .filter(|l| !upstream.contains(l))
            .map(|l| l.to_string())
            .collect(),
        release: release.map(str::to_string),
        work_dir: root.clone(),
        ..Config::default()
    };
    config.bitbake.enabled = false;
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Env {
        _tmp: tmp,
        _cache: cache,
        config,
        ws,
        index,
    }
}

fn env(rule: &str, layers: &[&str]) -> Env {
    env_with(rule, layers, &[], Some("wrynose"), |_| {})
}

impl Env {
    /// The findings of `rule` when linting layer `layer`.
    fn lint(&self, rule: &dyn Rule, layer: &str) -> Vec<Finding> {
        let report = lint::run_with(
            &self.ws,
            &self.index,
            &self.config,
            &Target::Layer(layer.into()),
            &Options {
                rules: vec![rule.id().to_string()],
                ..Options::default()
            },
            &[rule],
            &NoEval,
        )
        .unwrap();
        for f in &report.findings {
            assert_eq!(f.rule, rule.id());
            assert!(!f.message.is_empty() && !f.message.contains('\n'), "{f:?}");
        }
        report.findings
    }
}

/// `severity pointer` per finding.
fn rows(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|f| format!("{} {}", f.severity.as_str(), f.loc))
        .collect()
}

fn messages(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| format!("{} {}", f.loc, f.message))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bad layer's findings as rows; the good layer has none.
fn bad_and_good(rule: &dyn Rule, dir: &str, support: &[&str]) -> Vec<Finding> {
    let mut good: Vec<&str> = support.to_vec();
    good.push("good-layer");
    let g = env(dir, &good);
    let found = g.lint(rule, "good-layer");
    assert!(found.is_empty(), "good-layer:\n{}", messages(&found));
    let mut bad: Vec<&str> = support.to_vec();
    bad.push("bad-layer");
    env(dir, &bad).lint(rule, "bad-layer")
}

/// A file-scope rule on `lint/org/<dir>/<name>` as the file `path` of a layer `fixture`.
fn check_file(rule: &dyn Rule, dir: &str, name: &str, path: &str) -> Vec<Finding> {
    let env = env("recipe-path", &["good-layer"]);
    let text = fs::read_to_string(fixture_path(&format!("lint/org/{dir}/{name}"))).unwrap();
    let loc = Loc {
        layer: "fixture".into(),
        path: PathBuf::from(path),
        lines: LineSpan::line(1),
    };
    let found = lint::check_text(
        rule,
        &env.ws,
        &env.index,
        loc,
        &text,
        FileKind::from_path(Path::new(path)),
        &[],
        None,
    );
    for f in &found {
        assert!(!f.message.contains('\n'), "{f:?}");
    }
    found
}

// ---------------------------------------------------------------------------------------
// Layer-scope rules

#[test]
fn recipe_path() {
    let found = bad_and_good(&OrgRecipePath, "recipe-path", &[]);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:recipes-app/Hello/Hello_1.0.bb:1",
            "warning bad-layer:recipes-app/foo_1.0.bb:1",
            "warning bad-layer:recipes-app/my/my_tool_1.0.bb:1",
            "info bad-layer:recipes-app/noversion/noversion.bb:1",
            "info bad-layer:recipes-app/tools/mytool_1.0.bb:1",
        ],
        "{}",
        messages(&found)
    );
    let m = messages(&found);
    assert!(m.contains("rename the file to hello_1.0.bb"), "{m}");
    assert!(
        m.contains("move it to recipes-<category>/foo/foo_1.0.bb"),
        "{m}"
    );
    assert!(m.contains("use `-` inside the name: my-tool_1.0.bb"), "{m}");
    assert!(m.contains("name it noversion_<version>.bb"), "{m}");
    assert!(m.contains("recipes-app/mytool/"), "{m}");
}

#[test]
fn duplicate_versions() {
    let found = bad_and_good(&OrgDuplicateVersions, "duplicate-versions", &[]);
    assert_eq!(
        rows(&found),
        ["info bad-layer:recipes-app/dup/dup_2.0.bb:1"],
        "{}",
        messages(&found)
    );
    assert!(
        found[0]
            .message
            .contains("repeats most of bad-layer:recipes-app/dup/dup_1.0.bb:1"),
        "{}",
        found[0].message
    );
    assert!(found[0].message.contains("into dup.inc"));
}

#[test]
fn inc_unused() {
    let found = bad_and_good(&OrgIncUnused, "inc-unused", &[]);
    assert_eq!(
        rows(&found),
        ["info bad-layer:recipes-app/u/u.inc:1"],
        "{}",
        messages(&found)
    );
}

#[test]
fn repeated_logic() {
    let found = bad_and_good(&OrgRepeatedLogic, "repeated-logic", &[]);
    assert_eq!(
        rows(&found),
        [
            "info bad-layer:recipes-app/a/a_1.0.bb:2-8",
            "info bad-layer:recipes-app/a/a_1.0.bb:10-17",
        ],
        "{}",
        messages(&found)
    );
    let m = messages(&found);
    assert!(
        m.contains("the same 5-line body of do_install is in 3 recipes"),
        "{m}"
    );
    assert!(m.contains("bad-layer:recipes-app/c/c_1.0.bb:10-17"), "{m}");
    assert!(
        m.contains("the same block of assignments is in 3 recipes"),
        "{m}"
    );
}

#[test]
fn class_location() {
    let found = bad_and_good(&OrgClassLocation, "class-location", &[]);
    assert_eq!(
        rows(&found),
        [
            "info bad-layer:classes/myglobal.bbclass:1",
            "info bad-layer:classes/myrecipeclass.bbclass:1",
            "info bad-layer:classes/unused.bbclass:1",
        ],
        "{}",
        messages(&found)
    );
    let m = messages(&found);
    assert!(m.contains("(scarthgap and wrynose)"), "{m}");
    assert!(m.contains("myglobal is in classes/"), "{m}");
    assert!(
        m.contains("move it to classes-global/ (it is used through INHERIT)"),
        "{m}"
    );
    assert!(
        m.contains("move it to classes-recipe/ (recipes inherit it)"),
        "{m}"
    );

    // A layer that also claims kirkstone keeps classes/: kirkstone searches nothing else.
    let k = env("class-location", &["kirkstone-layer"]);
    assert_eq!(
        rows(&k.lint(&OrgClassLocation, "kirkstone-layer")),
        Vec::<String>::new()
    );
}

#[test]
fn append_no_match() {
    let found = bad_and_good(&OrgAppendNoMatch, "append-no-match", &[]);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:recipes-app/x/x_2.%.bbappend:1",
            "warning bad-layer:recipes-app/y/y_%.bbappend:1",
        ],
        "{}",
        messages(&found)
    );
    assert!(
        found[0].message.contains("x exists as x_1.0.bb"),
        "{}",
        found[0].message
    );
    assert!(
        found[1].message.contains("there is no y recipe"),
        "{}",
        found[1].message
    );
}

#[test]
fn append_wildcard_broad() {
    let found = bad_and_good(&OrgAppendWildcardBroad, "append-wildcard-broad", &[]);
    assert_eq!(
        rows(&found),
        ["info bad-layer:recipes-app/w/w_%.bbappend:2"],
        "{}",
        messages(&found)
    );
    assert!(
        found[0]
            .message
            .contains("applies to w 1.0 and 2.0 but pins a patch"),
        "{}",
        found[0].message
    );
    assert!(found[0].message.contains("(w_2.0.bbappend)"));
}

#[test]
fn layer_conf() {
    let layers = [
        "core-layer",
        "bad-layer",
        "incompat-layer",
        "copy-layer",
        "good-layer",
    ];
    let e = env_with("layer-conf", &layers, &[], None, |_| {});
    assert_eq!(e.ws.release.release.codename(), "wrynose");
    let good = e.lint(&OrgLayerConf, "good-layer");
    assert!(good.is_empty(), "{}", messages(&good));
    let core = e.lint(&OrgLayerConf, "core-layer");
    assert!(core.is_empty(), "{}", messages(&core));

    let bad = e.lint(&OrgLayerConf, "bad-layer");
    let m = messages(&bad);
    assert_eq!(
        rows(&bad),
        [
            "warning bad-layer:conf/layer.conf:1",
            "error bad-layer:conf/layer.conf:1",
            "warning bad-layer:conf/layer.conf:2",
            "error bad-layer:conf/layer.conf:8",
            "warning bad-layer:conf/layer.conf:8",
        ],
        "{m}"
    );
    assert!(m.contains("conf/layer.conf:1 conf/layer.conf does not add ${LAYERDIR} to BBPATH, so BitBake cannot find this layer's classes"), "{m}");
    assert!(
        m.contains("conf/layer.conf:1 LAYERSERIES_COMPAT_bad is not set"),
        "{m}"
    );
    assert!(
        m.contains("does not match recipes-app/b/b_%.bbappend"),
        "{m}"
    );
    assert!(
        m.contains("LAYERDEPENDS_bad names `other`, which no layer"),
        "{m}"
    );
    assert!(
        m.contains("bad-layer:recipes-app/b/b_1.0.bb:3 inherits a class from core-layer (collection `core`), but LAYERDEPENDS_bad does not name `core`"),
        "{m}"
    );

    // Two layers adding one collection (which also share its per-collection variables, as
    // in BitBake's single datastore): both are reported.
    let c = env_with(
        "layer-conf",
        &["core-layer", "good-layer", "clash-layer"],
        &[],
        None,
        |_| {},
    );
    let clash = c.lint(&OrgLayerConf, "clash-layer");
    assert_eq!(
        rows(&clash),
        ["error clash-layer:conf/layer.conf:6"],
        "{}",
        messages(&clash)
    );
    assert!(
        clash[0]
            .message
            .contains("collection `good` is also added by good-layer")
    );
    let good = c.lint(&OrgLayerConf, "good-layer");
    assert_eq!(
        rows(&good),
        ["error good-layer:conf/layer.conf:6"],
        "{}",
        messages(&good)
    );

    let incompat = e.lint(&OrgLayerConf, "incompat-layer");
    let m = messages(&incompat);
    assert_eq!(
        rows(&incompat),
        [
            "error incompat-layer:conf/layer.conf:11",
            "info incompat-layer:conf/layer.conf:11",
            "info incompat-layer:conf/layer.conf:11",
        ],
        "{m}"
    );
    assert!(m.contains("names none of the core layer's releases"), "{m}");
    assert!(
        m.contains("names dunfell, which is older than kirkstone"),
        "{m}"
    );
    assert!(
        m.contains("names scarthgapp, which is not a Yocto release name"),
        "{m}"
    );

    let copy = e.lint(&OrgLayerConf, "copy-layer");
    assert_eq!(
        rows(&copy),
        ["warning copy-layer:conf/layer.conf:11"],
        "{}",
        messages(&copy)
    );
    assert!(
        copy[0]
            .message
            .contains("copies another layer's LAYERSERIES_COMPAT")
    );

    // No core layer: the configured release is the reference.
    let nocore = env_with(
        "layer-conf",
        &["nocore-layer"],
        &[],
        Some("wrynose"),
        |_| {},
    );
    let found = nocore.lint(&OrgLayerConf, "nocore-layer");
    assert_eq!(
        rows(&found),
        ["warning nocore-layer:conf/layer.conf:10"],
        "{}",
        messages(&found)
    );
    assert!(found[0].message.contains(
        "does not name wrynose, the release this workspace targets (from sous-chef.toml)"
    ));
}

/// `git` with a fixed identity and no user configuration, in `dir`.
fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn commit_all(dir: &Path) -> bool {
    git(dir, &["init", "-q"]) && git(dir, &["add", "-A"]) && git(dir, &["commit", "-qm", "init"])
}

#[test]
fn upstream_edit() {
    if !Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("git is not available; skipping");
        return;
    }
    // bad-layer: an upstream checkout with a modified recipe, a deleted .inc, an untracked
    // recipe and an untracked editor backup (not reported).
    let bad = env_with(
        "upstream-edit",
        &["bad-layer"],
        &["bad-layer"],
        Some("wrynose"),
        |root| {
            let l = root.join("bad-layer");
            assert!(commit_all(&l));
            fs::write(l.join("recipes-app/u/u_1.0.bb"), "SUMMARY = \"edited\"\n").unwrap();
            fs::remove_file(l.join("recipes-app/u/u.inc")).unwrap();
            fs::write(l.join("recipes-app/u/u_2.0.bb"), "SUMMARY = \"new\"\n").unwrap();
            fs::write(l.join("recipes-app/u/u_1.0.bb~"), "backup\n").unwrap();
        },
    );
    let found = bad.lint(&OrgUpstreamEdit, "bad-layer");
    let m = messages(&found);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:recipes-app/u/u.inc:1",
            "warning bad-layer:recipes-app/u/u_1.0.bb:1",
            "warning bad-layer:recipes-app/u/u_2.0.bb:1",
        ],
        "{m}"
    );
    assert!(m.contains("u.inc is deleted in bad-layer"), "{m}");
    assert!(m.contains("u_1.0.bb is modified in bad-layer"), "{m}");
    assert!(m.contains("u_2.0.bb is untracked in bad-layer"), "{m}");

    // The same changes in a writable layer are the user's own work.
    let mine = env_with(
        "upstream-edit",
        &["bad-layer"],
        &[],
        Some("wrynose"),
        |root| {
            let l = root.join("bad-layer");
            assert!(commit_all(&l));
            fs::write(l.join("recipes-app/u/u_1.0.bb"), "SUMMARY = \"edited\"\n").unwrap();
        },
    );
    assert!(mine.lint(&OrgUpstreamEdit, "bad-layer").is_empty());

    // A clean checkout, and a layer that is not a git checkout at all: nothing.
    let good = env_with(
        "upstream-edit",
        &["good-layer"],
        &["good-layer"],
        Some("wrynose"),
        |root| {
            assert!(commit_all(&root.join("good-layer")));
        },
    );
    assert!(good.lint(&OrgUpstreamEdit, "good-layer").is_empty());
    let plain = env_with(
        "upstream-edit",
        &["bad-layer"],
        &["bad-layer"],
        Some("wrynose"),
        |_| {},
    );
    assert!(plain.lint(&OrgUpstreamEdit, "bad-layer").is_empty());

    // A layer that sits untracked inside another checkout is not an upstream checkout.
    let nested = env_with(
        "upstream-edit",
        &["bad-layer"],
        &["bad-layer"],
        Some("wrynose"),
        |root| {
            assert!(git(root, &["init", "-q"]));
        },
    );
    assert!(nested.lint(&OrgUpstreamEdit, "bad-layer").is_empty());
}

// ---------------------------------------------------------------------------------------
// File-scope rules on fixture layers (they need the index)

#[test]
fn inc_has_version_data() {
    let found = bad_and_good(&OrgIncHasVersionData, "inc-has-version-data", &[]);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:recipes-app/v/v.inc:3",
            "warning bad-layer:recipes-app/v/v.inc:4",
            "warning bad-layer:recipes-app/v/v.inc:5",
            "info bad-layer:recipes-app/v/v.inc:6",
        ],
        "{}",
        messages(&found)
    );
    let m = messages(&found);
    assert!(
        m.contains("`PV` sets the version in an .inc shared by v 1.0 and 2.0"),
        "{m}"
    );
    assert!(
        m.contains("`SRC_URI[sha256sum]` sets a source checksum"),
        "{m}"
    );
    assert!(m.contains("move it to each v_<version>.bb"), "{m}");
}

#[test]
fn include_must_exist() {
    let found = bad_and_good(&OrgIncludeMustExist, "include-must-exist", &[]);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:recipes-app/i/i_1.0.bb:1",
            "warning bad-layer:recipes-app/i/i_1.0.bb:2",
        ],
        "{}",
        messages(&found)
    );
    assert!(
        found[0]
            .message
            .contains("(it sets LICENSE at bad-layer:recipes-app/i/i-common.inc:2)"),
        "{}",
        found[0].message
    );
    assert!(found[0].message.contains("write `require i-common.inc`"));
    assert!(
        found[1]
            .message
            .starts_with("`require i-missing.inc` does not resolve")
    );
}

#[test]
fn class_recipe_specific() {
    let found = bad_and_good(&OrgClassRecipeSpecific, "class-recipe-specific", &[]);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:classes/fetchy.bbclass:1",
            "warning bad-layer:classes/fetchy.bbclass:2",
            "warning bad-layer:classes/fetchy.bbclass:3",
        ],
        "{}",
        messages(&found)
    );
    assert!(
        found[0]
            .message
            .contains("class fetchy sets SRC_URI with `=`")
    );
    assert!(
        found[2]
            .message
            .contains("replaces the value bad-layer:recipes-app/r/r_1.0.bb:1 sets before `inherit fetchy`; write `FETCHY_MODE ?=`"),
        "{}",
        found[2].message
    );
}

#[test]
fn inherit_defer() {
    let found = bad_and_good(&OrgInheritDefer, "inherit-defer", &[]);
    assert_eq!(
        rows(&found),
        [
            "info bad-layer:recipes-app/d/d_1.0.bb:2",
            "info bad-layer:recipes-app/e/e_1.0.bb:1",
            "info bad-layer:recipes-app/f/f_1.0.bb:1",
        ],
        "{}",
        messages(&found)
    );
    let m = messages(&found);
    assert!(
        m.contains("but bad-layer:recipes-app/d/d_1.0.bb:3 changes DCLASS later"),
        "{m}"
    );
    assert!(
        m.contains("bad-layer:recipes-app/e/e_%.bbappend:1 changes ECLASS"),
        "{m}"
    );
    assert!(
        m.contains("bad-layer:recipes-app/f/f_1.0.bb:3 changes FCLASS"),
        "{m}"
    );
    assert!(m.contains("write `inherit_defer ${DCLASS}`"), "{m}");

    // A layer that also claims kirkstone, which has no inherit_defer: no suggestion.
    let k = env("inherit-defer", &["kirkstone-layer"]);
    assert!(k.lint(&OrgInheritDefer, "kirkstone-layer").is_empty());
}

#[test]
fn append_missing_filesextrapaths() {
    let found = bad_and_good(
        &OrgAppendMissingFilesextrapaths,
        "append-missing-filesextrapaths",
        &["base-layer"],
    );
    assert_eq!(
        rows(&found),
        [
            "error bad-layer:recipes-app/f/f_%.bbappend:1",
            "warning bad-layer:recipes-app/f/f_%.bbappend:3",
        ],
        "{}",
        messages(&found)
    );
    assert!(
        found[0]
            .message
            .contains("add `FILESEXTRAPATHS:prepend := \"${THISDIR}/f:\"`"),
        "{}",
        found[0].message
    );
    assert!(found[1].message.contains("file://from-recipe.cfg"));
}

#[test]
fn policy_in_recipe() {
    let found = bad_and_good(&OrgPolicyInRecipe, "policy-in-recipe", &[]);
    assert_eq!(
        rows(&found),
        [
            "warning bad-layer:recipes-app/p/p_%.bbappend:1",
            "warning bad-layer:recipes-app/p/p_1.0.bb:3",
            "warning bad-layer:recipes-app/p/p_1.0.bb:4",
            "warning bad-layer:recipes-app/p/p_1.0.bb:5",
            "warning bad-layer:recipes-app/p/p_1.0.bb:6",
        ],
        "{}",
        messages(&found)
    );
    let m = messages(&found);
    assert!(m.contains("`DISTRO_FEATURES:append` in a recipe"), "{m}");
    assert!(
        m.contains("`PREFERRED_PROVIDER_virtual/kernel` in a recipe"),
        "{m}"
    );
    assert!(
        m.contains("`IMAGE_INSTALL:append` in a recipe that is not an image"),
        "{m}"
    );
}

// ---------------------------------------------------------------------------------------
// File-scope rules on text

#[test]
fn class_export_functions() {
    let found = check_file(
        &OrgClassExportFunctions,
        "class-export-functions",
        "bad.bbclass",
        "classes/mytool.bbclass",
    );
    let lines: Vec<String> = found.iter().map(|f| f.loc.lines.to_string()).collect();
    assert_eq!(lines, ["6-8", "11-13"], "{}", messages(&found));
    assert!(
        found[0]
            .message
            .contains("no `EXPORT_FUNCTIONS do_install`")
    );
    assert!(
        found[1]
            .message
            .contains("name it mytool_do_configure and add `EXPORT_FUNCTIONS do_configure`")
    );
    let good = check_file(
        &OrgClassExportFunctions,
        "class-export-functions",
        "good.bbclass",
        "classes/mytool.bbclass",
    );
    assert!(good.is_empty(), "{}", messages(&good));
    // Not a class: nothing.
    assert!(
        check_file(
            &OrgClassExportFunctions,
            "class-export-functions",
            "bad.bbclass",
            "recipes-x/x/x.inc",
        )
        .is_empty()
    );
}

#[test]
fn recipe_change_in_local_conf() {
    let found = check_file(
        &OrgRecipeChangeInLocalConf,
        "recipe-change-in-local-conf",
        "bad.conf",
        "conf/local.conf",
    );
    let lines: Vec<String> = found.iter().map(|f| f.loc.lines.to_string()).collect();
    assert_eq!(lines, ["2", "3"], "{}", messages(&found));
    assert!(
        found[0]
            .message
            .contains("move it to a hello_%.bbappend in your layer as `EXTRA_OECONF:append`")
    );
    assert!(found[1].message.contains("as `PACKAGECONFIG`"));
    assert!(
        check_file(
            &OrgRecipeChangeInLocalConf,
            "recipe-change-in-local-conf",
            "good.conf",
            "conf/local.conf",
        )
        .is_empty()
    );
    // Only local.conf.
    assert!(
        check_file(
            &OrgRecipeChangeInLocalConf,
            "recipe-change-in-local-conf",
            "bad.conf",
            "conf/distro/mine.conf",
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------------------------
// The fixture workspace

fn org_rules() -> Vec<&'static dyn Rule> {
    lint::all_rules()
        .iter()
        .copied()
        .filter(|r| lint::group_of(r.id()) == "org")
        .collect()
}

/// Every org finding on a copy of the fixture workspace, all layers included.
#[test]
fn fixture_workspace_findings() {
    let (_tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let config = ws_config(&root);
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let rules = org_rules();
    assert_eq!(rules.len(), 17);
    let start = Instant::now();
    let report = lint::run_with(
        &ws,
        &index,
        &config,
        &Target::Workspace,
        &Options {
            all: true,
            ..Options::default()
        },
        &rules,
        &NoEval,
    )
    .unwrap();
    eprintln!("org rules on the fixture workspace: {:?}", start.elapsed());
    let got: Vec<String> = report
        .findings
        .iter()
        .map(|f| format!("{} {} {}", f.severity.as_str(), f.rule, f.loc))
        .collect();
    assert_eq!(
        got,
        [
            "info org/recipe-change-in-local-conf build:conf/local.conf:9",
            "info org/class-location meta-core:classes/base.bbclass:1",
            "warning org/append-no-match meta-mine:recipes-app/orphan/orphan_9.%.bbappend:1",
            "info org/append-wildcard-broad meta-vendor:recipes-core/hello/hello_%.bbappend:4",
        ],
        "{}",
        messages(&report.findings)
    );
}
