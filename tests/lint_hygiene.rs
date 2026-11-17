//! T4.4: the rules of the `hygiene` group (except `parse-error`, tested in lint_engine.rs).
//!
//! File-scope rules run on their fixtures through `lint::check_text`; recipe-scope rules
//! (`missing-metadata`, `src-uri-checksum`) through the real engine and evaluator, with the
//! fixture copied into a temporary copy of the fixture workspace. Release-dependent rules
//! are checked for a layer that claims an affected release and for one that does not.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::index::Index;
use sous_chef::lint::rules::{
    hygiene_autorev::HygieneAutorev, hygiene_continuation::HygieneContinuation,
    hygiene_host_path::HygieneHostPath, hygiene_release_syntax::HygieneReleaseSyntax,
    hygiene_tabs::HygieneTabs, hygiene_variable_order::HygieneVariableOrder,
    hygiene_workdir_s::HygieneWorkdirS,
};
use sous_chef::lint::{self, Options, Rule, Severity, Target};
use sous_chef::release::Release;
use sous_chef::types::{FileKind, LineSpan, Loc};
use sous_chef::workspace::Workspace;

const MINE: [Release; 3] = [Release::Kirkstone, Release::Scarthgap, Release::Wrynose];
const OLD: [Release; 2] = [Release::Kirkstone, Release::Scarthgap];
const NEW: [Release; 1] = [Release::Wrynose];

/// The fixture workspace and an index of it in a temporary cache.
struct Fx {
    _cache: tempfile::TempDir,
    ws: Workspace,
    index: Index,
}

fn fx() -> Fx {
    let cache = tempfile::tempdir().unwrap();
    let ws = fixture_ws();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Fx {
        _cache: cache,
        ws,
        index,
    }
}

/// `(line, severity, message)` of `rule` on fixture `lint/hygiene/<dir>/<name>`, linted as
/// the file `as_path` of a layer checked against `releases`.
fn check(
    rule: &dyn Rule,
    dir: &str,
    name: &str,
    as_path: &str,
    releases: &[Release],
) -> Vec<(u32, Severity, String)> {
    let f = fx();
    let text = fs::read_to_string(fixture_path(&format!("lint/hygiene/{dir}/{name}"))).unwrap();
    let loc = Loc {
        layer: "fixture".into(),
        path: PathBuf::from(as_path),
        lines: LineSpan::line(1),
    };
    lint::check_text(
        rule,
        &f.ws,
        &f.index,
        loc,
        &text,
        FileKind::from_path(Path::new(as_path)),
        releases,
        None,
    )
    .into_iter()
    .map(|x| {
        assert_eq!(x.rule, rule.id());
        assert!(!x.message.contains('\n'), "{}", x.message);
        (x.loc.lines.start, x.severity, x.message)
    })
    .collect()
}

fn lines(found: &[(u32, Severity, String)]) -> Vec<u32> {
    found.iter().map(|f| f.0).collect()
}

// ---------------------------------------------------------------------------------------
// hygiene/autorev

#[test]
fn autorev_bad_and_good() {
    let bad = check(
        &HygieneAutorev,
        "autorev",
        "bad.bb",
        "recipes-x/x/x_1.0.bb",
        &[],
    );
    assert_eq!(lines(&bad), [3, 4, 5], "{bad:#?}");
    assert!(bad.iter().all(|f| f.1 == Severity::Warning));
    assert!(
        bad[1]
            .2
            .starts_with("`SRCREV_meta` follows the branch head"),
        "{bad:#?}"
    );
    assert!(bad[0].2.contains("pin a commit hash"));
    assert_eq!(
        check(
            &HygieneAutorev,
            "autorev",
            "good.bb",
            "recipes-x/x/x_1.0.bb",
            &[]
        ),
        []
    );
    assert_eq!(
        check(
            &HygieneAutorev,
            "autorev",
            "local.conf",
            "conf/local.conf",
            &[]
        ),
        []
    );
    // The same text anywhere else is reported.
    assert_eq!(
        lines(&check(
            &HygieneAutorev,
            "autorev",
            "local.conf",
            "conf/distro/d.conf",
            &[]
        )),
        [2]
    );
}

// ---------------------------------------------------------------------------------------
// hygiene/continuation

#[test]
fn continuation_bad_and_good() {
    let bad = check(
        &HygieneContinuation,
        "continuation",
        "bad.bb",
        "x_1.0.bb",
        &[],
    );
    assert_eq!(lines(&bad), [2, 5], "{bad:#?}");
    assert!(
        bad[0].2.starts_with("whitespace after the trailing `\\`"),
        "{bad:#?}"
    );
    assert!(
        bad[1].2.starts_with("blank line after a trailing `\\`"),
        "{bad:#?}"
    );
    assert!(bad.iter().all(|f| f.1 == Severity::Warning));
    assert_eq!(
        check(
            &HygieneContinuation,
            "continuation",
            "good.bb",
            "x_1.0.bb",
            &[]
        ),
        []
    );
}

// ---------------------------------------------------------------------------------------
// hygiene/host-path

#[test]
fn host_path_bad_and_good() {
    assert_eq!(
        check(
            &HygieneHostPath,
            "host-path",
            "good.bbclass",
            "classes/mytool.bbclass",
            &[]
        ),
        []
    );
    let bad = check(&HygieneHostPath, "host-path", "bad.bb", "x_1.0.bb", &[]);
    assert_eq!(lines(&bad), [4, 5, 6, 7, 11], "{bad:#?}");
    assert!(
        bad[0].2.starts_with("do_install never references ${D}"),
        "{bad:#?}"
    );
    assert!(
        bad[1]
            .2
            .starts_with("absolute host path `/usr/bin` in do_install")
    );
    assert!(bad[1].2.contains("`${D}${bindir}`"));
    assert!(
        bad[3]
            .2
            .starts_with("absolute host path `/etc` in do_install")
    );
    assert!(
        bad[4]
            .2
            .starts_with("absolute host path `/home` in do_compile")
    );
    assert_eq!(
        check(&HygieneHostPath, "host-path", "good.bb", "x_1.0.bb", &[]),
        []
    );
    // B13: grep patterns, echo / printf text, sed expressions, shebangs and symlink targets.
    assert_eq!(
        check(
            &HygieneHostPath,
            "host-path",
            "good-data.bb",
            "x_1.0.bb",
            &[]
        ),
        []
    );
}

// ---------------------------------------------------------------------------------------
// hygiene/tabs

#[test]
fn tabs_bad_and_good() {
    let bad = check(&HygieneTabs, "tabs", "bad.bb", "x_1.0.bb", &[]);
    assert_eq!(lines(&bad), [3, 6, 9], "{bad:#?}");
    assert!(
        bad[0]
            .2
            .starts_with("shell body indented with tabs above and spaces here")
    );
    assert!(bad[1].2.starts_with("python body indented with 2 spaces"));
    assert!(bad[2].2.starts_with("tab in python indentation"));
    assert!(bad.iter().all(|f| f.1 == Severity::Info));
    assert_eq!(check(&HygieneTabs, "tabs", "good.bb", "x_1.0.bb", &[]), []);
}

// ---------------------------------------------------------------------------------------
// hygiene/workdir-s

#[test]
fn workdir_s_is_an_error_for_a_layer_checked_against_a_release_that_rejects_it() {
    let bad = check(&HygieneWorkdirS, "workdir-s", "bad.bb", "x_1.0.bb", &MINE);
    assert_eq!(lines(&bad), [3, 5], "{bad:#?}");
    assert!(bad.iter().all(|f| f.1 == Severity::Error));
    assert!(
        bad[0]
            .2
            .starts_with("`S = \"${WORKDIR}\"` is rejected with an error in wrynose, which"),
        "{bad:#?}"
    );
    assert!(
        bad[0]
            .2
            .contains("`S = \"${UNPACKDIR}\"` (the variable exists from styhead on)")
    );
    // Every release from styhead on is named.
    let all_new = [Release::Styhead, Release::Walnascar, Release::Wrynose];
    let named = check(
        &HygieneWorkdirS,
        "workdir-s",
        "bad.bb",
        "x_1.0.bb",
        &all_new,
    );
    assert!(
        named[0].2.contains("in styhead, walnascar and wrynose,"),
        "{named:#?}"
    );
    assert_eq!(
        check(&HygieneWorkdirS, "workdir-s", "good.bb", "x_1.0.bb", &MINE),
        []
    );
}

#[test]
fn workdir_s_is_accepted_by_a_layer_that_claims_only_releases_that_accept_it() {
    assert_eq!(
        check(&HygieneWorkdirS, "workdir-s", "bad.bb", "x_1.0.bb", &OLD),
        []
    );
}

// ---------------------------------------------------------------------------------------
// hygiene/variable-order

#[test]
fn variable_order_bad_and_good() {
    let bad = check(
        &HygieneVariableOrder,
        "variable-order",
        "bad.bb",
        "x_1.0.bb",
        &[],
    );
    assert_eq!(lines(&bad), [7], "{bad:#?}");
    assert!(
        bad[0]
            .2
            .starts_with("`SRC_URI` (SRC_URI) comes after `inherit` (inherit, line 5)"),
        "{bad:#?}"
    );
    assert_eq!(
        check(
            &HygieneVariableOrder,
            "variable-order",
            "good.bb",
            "x_1.0.bb",
            &[]
        ),
        []
    );
    // Only recipes.
    assert_eq!(
        check(
            &HygieneVariableOrder,
            "variable-order",
            "bad.bb",
            "x.inc",
            &[]
        ),
        []
    );
}

#[test]
fn variable_order_is_off_by_default_and_selected_by_id_alias_and_alias_group() {
    assert!(!HygieneVariableOrder.default_enabled());
    let (_tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let rel = "meta-mine/recipes-test/order/order_1.0.bb";
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::copy(fixture_path("lint/hygiene/variable-order/bad.bb"), &p).unwrap();
    let config = ws_config(&root);
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let target = Target::Files {
        files: vec![p.clone()],
        lines: None,
    };
    let found = |rules: &[&str]| -> bool {
        let options = Options {
            rules: rules.iter().map(|s| s.to_string()).collect(),
            ..Options::default()
        };
        lint::run(&ws, &index, &config, &target, &options)
            .unwrap()
            .findings
            .iter()
            .any(|f| f.rule == "hygiene/variable-order")
    };
    assert!(!found(&[]));
    assert!(!found(&["hygiene"]));
    assert!(found(&["hygiene/variable-order"]));
    assert!(found(&["style/variable-order"]));
    assert!(found(&["style"]));
    assert!(found(&["style/"]));
}

// ---------------------------------------------------------------------------------------
// hygiene/release-syntax

#[test]
fn release_syntax_for_a_layer_that_claims_kirkstone_to_wrynose() {
    let bad = check(
        &HygieneReleaseSyntax,
        "release-syntax",
        "bad.bb",
        "x_1.0.bb",
        &MINE,
    );
    let got: Vec<(u32, Severity)> = bad.iter().map(|f| (f.0, f.1)).collect();
    assert_eq!(
        got,
        [
            (3, Severity::Error),
            (4, Severity::Error),
            (7, Severity::Warning),
            (8, Severity::Error),
        ],
        "{bad:#?}"
    );
    assert!(
        bad[0].2.starts_with(
            "`inherit_defer` is not available in kirkstone, which this layer is checked against;"
        ),
        "{bad:#?}"
    );
    assert!(
        bad[1]
            .2
            .starts_with("`include_all` is not available in kirkstone and scarthgap, which"),
        "{bad:#?}"
    );
    assert!(
        bad[2].2.starts_with(
            "`${WORKDIR}/main.c`: from styhead on, `file://` sources are unpacked into \
             ${UNPACKDIR}, not ${WORKDIR}, and this layer is checked against wrynose;"
        ),
        "{bad:#?}"
    );
    assert!(
        bad[3]
            .2
            .starts_with("`${UNPACKDIR}` is not available in kirkstone and scarthgap, which"),
        "{bad:#?}"
    );
    assert_eq!(
        check(
            &HygieneReleaseSyntax,
            "release-syntax",
            "good.bb",
            "x_1.0.bb",
            &MINE
        ),
        []
    );
}

#[test]
fn release_syntax_for_layers_that_claim_only_new_or_only_old_releases() {
    // Wrynose only: every directive exists; only the WORKDIR reference is left.
    let new = check(
        &HygieneReleaseSyntax,
        "release-syntax",
        "bad.bb",
        "x_1.0.bb",
        &NEW,
    );
    assert_eq!(lines(&new), [7], "{new:#?}");
    // Kirkstone and Scarthgap: no UNPACKDIR there, so `${WORKDIR}/main.c` is right.
    let old = check(
        &HygieneReleaseSyntax,
        "release-syntax",
        "bad.bb",
        "x_1.0.bb",
        &OLD,
    );
    assert_eq!(lines(&old), [3, 4, 8], "{old:#?}");
    // Scarthgap alone has inherit_defer.
    let sg = check(
        &HygieneReleaseSyntax,
        "release-syntax",
        "bad.bb",
        "x_1.0.bb",
        &[Release::Scarthgap],
    );
    assert_eq!(lines(&sg), [4, 8], "{sg:#?}");
    assert_eq!(
        check(
            &HygieneReleaseSyntax,
            "release-syntax",
            "good.bb",
            "x_1.0.bb",
            &OLD
        ),
        []
    );
}

#[test]
fn release_syntax_class_directories() {
    let path = "classes-recipe/bad.bbclass";
    let bad = check(
        &HygieneReleaseSyntax,
        "release-syntax",
        "bad.bbclass",
        path,
        &MINE,
    );
    assert_eq!(lines(&bad), [1], "{bad:#?}");
    assert!(
        bad[0].2.starts_with(
            "the `classes-recipe/` class directory is not available in kirkstone, which"
        ),
        "{bad:#?}"
    );
    let global = "classes-global/bad.bbclass";
    let bad = check(
        &HygieneReleaseSyntax,
        "release-syntax",
        "bad.bbclass",
        global,
        &OLD,
    );
    assert_eq!(lines(&bad), [1], "{bad:#?}");
    // A layer that does not claim kirkstone.
    assert_eq!(
        check(
            &HygieneReleaseSyntax,
            "release-syntax",
            "bad.bbclass",
            path,
            &NEW
        ),
        []
    );
    let good = "classes/good.bbclass";
    assert_eq!(
        check(
            &HygieneReleaseSyntax,
            "release-syntax",
            "good.bbclass",
            good,
            &MINE
        ),
        []
    );
}

#[test]
fn release_syntax_unpackdir_defined_by_the_file_is_not_reported() {
    let f = fx();
    let text = "UNPACKDIR ??= \"${WORKDIR}\"\nS = \"${UNPACKDIR}\"\n";
    let loc = Loc {
        layer: "fixture".into(),
        path: PathBuf::from("x_1.0.bb"),
        lines: LineSpan::line(1),
    };
    let got = lint::check_text(
        &HygieneReleaseSyntax,
        &f.ws,
        &f.index,
        loc,
        text,
        Some(FileKind::Recipe),
        &MINE,
        None,
    );
    assert_eq!(got, []);
}

// ---------------------------------------------------------------------------------------
// Recipe scope: through the engine and the real evaluator

struct Ws {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    index: Index,
}

/// A copy of the fixture workspace with `files` (`(fixture path, path in the copy)`) added;
/// `edit(root)` runs before discovery.
fn ws_with(files: &[(&str, &str)], edit: impl FnOnce(&Path)) -> Ws {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    for (from, to) in files {
        let p = root.join(to);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::copy(fixture_path(from), &p).unwrap();
    }
    edit(&root);
    let config = ws_config(&root);
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Ws {
        _tmp: tmp,
        _cache: cache,
        root,
        config,
        ws,
        index,
    }
}

impl Ws {
    /// `severity rule pointer` and the message of every finding of `rule` on `rel`.
    fn lint(&self, rule: &str, rel: &str) -> Vec<(String, String)> {
        let report = lint::run(
            &self.ws,
            &self.index,
            &self.config,
            &Target::Files {
                files: vec![self.root.join(rel)],
                lines: None,
            },
            &Options {
                rules: vec![rule.into()],
                ..Options::default()
            },
        )
        .unwrap();
        assert!(
            report
                .notes
                .iter()
                .all(|n| !n.contains("evaluation failed")),
            "{:?}",
            report.notes
        );
        report
            .findings
            .iter()
            .map(|f| {
                assert_eq!(f.rule, rule);
                assert!(!f.message.contains('\n'));
                (
                    format!("{} {} {}", f.severity.as_str(), f.rule, f.loc),
                    f.message.clone(),
                )
            })
            .collect()
    }
}

const MD: &str = "lint/hygiene/missing-metadata";

#[test]
fn missing_metadata_bad() {
    let w = ws_with(
        &[(
            &format!("{MD}/bad.bb"),
            "meta-mine/recipes-test/bad/bad_1.0.bb",
        )],
        |_| {},
    );
    let got = w.lint(
        "hygiene/missing-metadata",
        "meta-mine/recipes-test/bad/bad_1.0.bb",
    );
    let rows: Vec<&str> = got.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(
        rows,
        [
            "info hygiene/missing-metadata meta-mine:recipes-test/bad/bad_1.0.bb:1",
            "warning hygiene/missing-metadata meta-mine:recipes-test/bad/bad_1.0.bb:1",
        ],
        "{got:#?}"
    );
    assert_eq!(
        got[1].1,
        "recipe has no SUMMARY, LICENSE, LIC_FILES_CHKSUM; set them in the recipe (or its \
         .inc), or LICENSE = \"CLOSED\" for closed source"
    );
    assert!(got[0].1.starts_with("recipe has no HOMEPAGE"));
}

#[test]
fn missing_metadata_good_and_closed() {
    let w = ws_with(
        &[
            (
                &format!("{MD}/good.bb"),
                "meta-mine/recipes-test/good/good_1.0.bb",
            ),
            (
                &format!("{MD}/good.inc"),
                "meta-mine/recipes-test/good/good.inc",
            ),
            (
                &format!("{MD}/closed.bb"),
                "meta-mine/recipes-test/closed/closed_1.0.bb",
            ),
        ],
        |_| {},
    );
    assert_eq!(
        w.lint(
            "hygiene/missing-metadata",
            "meta-mine/recipes-test/good/good_1.0.bb"
        ),
        []
    );
    assert_eq!(
        w.lint(
            "hygiene/missing-metadata",
            "meta-mine/recipes-test/closed/closed_1.0.bb"
        ),
        []
    );
}

#[test]
fn missing_metadata_ignores_configuration_defaults() {
    // OE-core's bitbake.conf gives SUMMARY and LICENSE defaults; they do not count.
    let w = ws_with(
        &[(
            &format!("{MD}/bad.bb"),
            "meta-mine/recipes-test/bad/bad_1.0.bb",
        )],
        |root| {
            let conf = root.join("meta-core/conf/bitbake.conf");
            let mut text = fs::read_to_string(&conf).unwrap();
            text.push_str(
                "SUMMARY ?= \"${PN} version ${PV}\"\nLICENSE ??= \"INVALID\"\nHOMEPAGE = \"\"\n",
            );
            fs::write(&conf, text).unwrap();
        },
    );
    let got = w.lint(
        "hygiene/missing-metadata",
        "meta-mine/recipes-test/bad/bad_1.0.bb",
    );
    assert_eq!(got.len(), 2, "{got:#?}");
    assert!(
        got[1]
            .1
            .starts_with("recipe has no SUMMARY, LICENSE, LIC_FILES_CHKSUM;")
    );
}

#[test]
fn missing_metadata_on_the_fixture_recipe() {
    // app_0.1.bb: LICENSE = "CLOSED", SUMMARY set, no HOMEPAGE.
    let w = ws_with(&[], |_| {});
    let got = w.lint(
        "hygiene/missing-metadata",
        "meta-mine/recipes-app/app/app_0.1.bb",
    );
    let rows: Vec<&str> = got.iter().map(|(r, _)| r.as_str()).collect();
    // B12: its SRC_URI is only `file://app.c`, so it has no upstream and no HOMEPAGE is
    // asked for (it was reported before).
    assert_eq!(rows, [] as [&str; 0]);
}

#[test]
fn missing_metadata_does_not_ask_images_packagegroups_or_local_recipes_for_a_homepage() {
    // B12. Stand-ins for OE-core's image.bbclass, core-image.bbclass and
    // packagegroup.bbclass (each `LICENSE ?= "MIT"`; core-image inherits image).
    let files = [
        (
            format!("{MD}/good-image.bb"),
            "meta-mine/recipes-core/images/kiss-image.bb",
        ),
        (
            format!("{MD}/good-packagegroup.bb"),
            "meta-mine/recipes-core/packagegroups/packagegroup-mine.bb",
        ),
        (
            format!("{MD}/good-local.bb"),
            "meta-mine/recipes-test/local/local-conf.bb",
        ),
        (
            format!("{MD}/no-src.bb"),
            "meta-mine/recipes-test/nosrc/nosrc_1.0.bb",
        ),
    ];
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    let w = ws_with(&refs, |root| {
        let dir = root.join("meta-core/classes-recipe");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("image.bbclass"),
            "LICENSE ?= \"MIT\"\nPACKAGES = \"\"\n",
        )
        .unwrap();
        fs::write(dir.join("core-image.bbclass"), "inherit image\n").unwrap();
        fs::write(
            dir.join("packagegroup.bbclass"),
            "LICENSE ?= \"MIT\"\nPACKAGES = \"${PN}\"\n",
        )
        .unwrap();
    });
    for (_, rel) in &files[..3] {
        assert_eq!(w.lint("hygiene/missing-metadata", rel), [], "{rel}");
    }
    // Without a SRC_URI only SUMMARY and LICENSE are asked for.
    let got = w.lint("hygiene/missing-metadata", files[3].1);
    let rows: Vec<&str> = got.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(
        rows,
        ["warning hygiene/missing-metadata meta-mine:recipes-test/nosrc/nosrc_1.0.bb:1"]
    );
    assert!(
        got[0]
            .1
            .starts_with("recipe has no SUMMARY, LICENSE; set them"),
        "{}",
        got[0].1
    );
}

const SC: &str = "lint/hygiene/src-uri-checksum";

#[test]
fn src_uri_checksum_bad() {
    let rel = "meta-mine/recipes-test/bad/bad_1.0.bb";
    let w = ws_with(&[(&format!("{SC}/bad.bb"), rel)], |_| {});
    let got = w.lint("hygiene/src-uri-checksum", rel);
    let rows: Vec<&str> = got.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(
        rows,
        [
            "warning hygiene/src-uri-checksum meta-mine:recipes-test/bad/bad_1.0.bb:2-4",
            "warning hygiene/src-uri-checksum meta-mine:recipes-test/bad/bad_1.0.bb:2-4",
            "warning hygiene/src-uri-checksum meta-mine:recipes-test/bad/bad_1.0.bb:7",
        ],
        "{got:#?}"
    );
    let msgs: Vec<&str> = got.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(
        msgs,
        [
            "`git://example.org/lib.git`: git URL without a revision or `branch=`; add \
             `;branch=<branch>` and set SRCREV_lib = \"<commit hash>\"",
            "`https://example.org/releases/bad-1.0.tar.gz`: remote download without a \
             checksum; add SRC_URI[sha256sum] = \"<sha256 of the file>\"",
            "`git://example.org/tools.git`: git URL without `branch=`; add `;branch=<branch>` \
             (or `;nobranch=1` for a commit on no branch)",
        ]
    );
}

#[test]
fn src_uri_checksum_good() {
    let rel = "meta-mine/recipes-test/good/good_1.0.bb";
    let w = ws_with(&[(&format!("{SC}/good.bb"), rel)], |_| {});
    assert_eq!(w.lint("hygiene/src-uri-checksum", rel), []);
}

#[test]
fn src_uri_checksum_on_the_fixture_workspace_recipes() {
    // Every fixture recipe pins its downloads (meta-core is linted when named).
    let w = ws_with(&[], |_| {});
    for rel in [
        "meta-mine/recipes-app/app/app_0.1.bb",
        "meta-core/recipes-core/hello/hello_1.0.bb",
        "meta-core/recipes-core/hello/hello_1.1.bb",
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
    ] {
        assert_eq!(w.lint("hygiene/src-uri-checksum", rel), [], "{rel}");
    }
}
