//! `--as-release`, the migration report (`--migrate-to`) and `hygiene/release-removed` on
//! temporary copies of the fixture workspace.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::index::Index;
use sous_chef::lint::{self, Finding, Options, Target};
use sous_chef::release::Release;
use sous_chef::workspace::Workspace;

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    index: Index,
}

/// A copy of the fixture workspace configured for `release`; `edit(root)` runs before
/// discovery.
fn env(release: &str, edit: impl FnOnce(&Path)) -> Env {
    let (tmp, root, config, ws) = temp_ws_release(release, edit);
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Env {
        _tmp: tmp,
        _cache: cache,
        root,
        config,
        ws,
        index,
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Sets meta-mine's `LAYERSERIES_COMPAT_mine`.
fn claim(root: &Path, names: &str) {
    let p = root.join("meta-mine/conf/layer.conf");
    let text = fs::read_to_string(&p).unwrap().replace(
        "LAYERSERIES_COMPAT_mine = \"kirkstone scarthgap wrynose\"",
        &format!("LAYERSERIES_COMPAT_mine = \"{names}\""),
    );
    fs::write(p, text).unwrap();
}

fn lint_layer(e: &Env, layer: &str, rules: &[&str], options: Options) -> Vec<Finding> {
    let options = Options {
        rules: rules.iter().map(|r| r.to_string()).collect(),
        ..options
    };
    lint::run(
        &e.ws,
        &e.index,
        &e.config,
        &Target::Layer(layer.into()),
        &options,
    )
    .unwrap()
    .findings
}

fn rows(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|f| {
            format!(
                "{} {} {}: {}",
                f.severity.as_str(),
                f.rule,
                f.loc,
                f.message
            )
        })
        .collect()
}

const WORKDIR_S: &str =
    "SUMMARY = \"x\"\nLICENSE = \"CLOSED\"\nSRC_URI = \"file://x.c\"\nS = \"${WORKDIR}\"\n";

#[test]
fn as_release_adds_a_release_to_a_layer_claiming_only_scarthgap() {
    let e = env("scarthgap", |root| {
        claim(root, "scarthgap");
        write(root, "meta-mine/recipes-app/old/old_1.0.bb", WORKDIR_S);
    });
    let mine = e.ws.layer("meta-mine").unwrap();
    assert_eq!(mine.lint_releases(&e.ws), [Release::Scarthgap]);

    // Without the option: Scarthgap accepts S = "${WORKDIR}".
    let plain = lint_layer(&e, "meta-mine", &["hygiene/workdir-s"], Options::default());
    assert!(plain.is_empty(), "{:?}", rows(&plain));

    // As Wrynose too: the error names wrynose.
    let as_wrynose = Options {
        as_release: Some(Release::Wrynose),
        ..Options::default()
    };
    let found = lint_layer(&e, "meta-mine", &["hygiene/workdir-s"], as_wrynose);
    let r = rows(&found);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        r[0].starts_with("error hygiene/workdir-s meta-mine:recipes-app/old/old_1.0.bb:4: "),
        "{r:?}"
    );
    assert!(r[0].contains("wrynose"), "{r:?}");
    assert!(!r[0].contains("scarthgap"), "{r:?}");

    // --only-release with a release that accepts it: nothing.
    let only_kirkstone = Options {
        as_release: Some(Release::Kirkstone),
        only_release: true,
        ..Options::default()
    };
    assert!(lint_layer(&e, "meta-mine", &["hygiene/workdir-s"], only_kirkstone).is_empty());
}

#[test]
fn releases_with_options_adds_or_replaces() {
    use Release::*;
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap], None, false),
        [Scarthgap]
    );
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap, Wrynose], Some(Kirkstone), false),
        [Kirkstone, Scarthgap, Wrynose]
    );
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap, Wrynose], Some(Wrynose), false),
        [Scarthgap, Wrynose]
    );
    assert_eq!(
        lint::releases_with_options(vec![Scarthgap, Wrynose], Some(Kirkstone), true),
        [Kirkstone]
    );
}

#[test]
fn as_release_on_the_cli_rejects_unknown_and_old_releases() {
    let e = env("wrynose", |_| {});
    let mut out = Vec::new();
    for (name, want) in [
        ("wrynoes", "unknown release"),
        ("dunfell", "older than kirkstone"),
    ] {
        let err = lint::cli_with(
            &e.ws,
            &e.index,
            &e.config,
            &lint::LintCliArgs {
                as_release: Some(name.into()),
                ..lint::LintCliArgs::default()
            },
            &mut out,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(want), "{err}");
        assert!(err.contains("kirkstone, langdale"), "{err}");
    }
    let _ = &e.root;
}

// ---------------------------------------------------------------------------------------
// hygiene/release-removed

use sous_chef::lint::rules::hygiene_release_removed::HygieneReleaseRemoved;
use sous_chef::types::{FileKind, LineSpan, Loc};

/// `line severity: message` of hygiene/release-removed on fixture
/// `lint/hygiene/release-removed/<name>`, linted as `as_path` of a layer checked against
/// `releases`.
fn removed(name: &str, as_path: &str, releases: &[Release]) -> Vec<String> {
    let cache = tempfile::tempdir().unwrap();
    let ws = fixture_ws();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let text = fs::read_to_string(fixture_path(&format!(
        "lint/hygiene/release-removed/{name}"
    )))
    .unwrap();
    lint::check_text(
        &HygieneReleaseRemoved,
        &ws,
        &index,
        Loc {
            layer: "fixture".into(),
            path: PathBuf::from(as_path),
            lines: LineSpan::line(1),
        },
        &text,
        FileKind::from_path(Path::new(as_path)),
        releases,
        None,
    )
    .into_iter()
    .map(|f| {
        assert_eq!(f.rule, "hygiene/release-removed");
        assert!(!f.message.contains('\n'));
        format!(
            "{} {}: {}",
            f.loc.lines.start,
            f.severity.as_str(),
            f.message
        )
    })
    .collect()
}

const MINE: [Release; 3] = [Release::Kirkstone, Release::Scarthgap, Release::Wrynose];

#[test]
fn release_removed_bad_recipe_for_kirkstone_to_wrynose() {
    let got = removed("bad.bb", "x_1.0.bb", &MINE);
    let head: Vec<String> = got
        .iter()
        .map(|r| r.split(": ").next().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        head,
        [
            "2 error",
            "3 error",
            "4 error",
            "5 warning",
            "6 warning",
            "7 error",
            "7 error",
            "8 error",
            "9 warning",
            "10 error",
            "11 warning",
            "13 warning",
        ],
        "{got:#?}"
    );
    let want = [
        "2 error: `GPLv2` is an obsolete license name: OE-core's obsolete-license QA check is an error from styhead on (ERROR_QA), and this layer is checked against wrynose; write `GPL-2.0-only`",
        "3 error: `bzr://` in SRC_URI: BitBake has no usable bzr fetcher from wrynose on, and this layer is checked against wrynose; fetch the sources another way (a release tarball with a checksum, or git)",
        "4 error: `S = \"${WORKDIR}/x-${PV}\"`: from whinlatter on OE-core's do_qa_unpack stops on it, and this layer is checked against wrynose; write `S = \"${UNPACKDIR}/x-${PV}\"`",
        "5 warning: `virtual/${TARGET_PREFIX}gcc`: OE-core no longer provides or asks for this toolchain provider name from walnascar on, and this layer is checked against wrynose; write `virtual/cross-cc`",
        "6 warning: `SERIAL_CONSOLE`: OE-core no longer reads it from mickledore on, so setting it has no effect there, and this layer is checked against scarthgap and wrynose; write `SERIAL_CONSOLES`",
        "7 error: class `cve-check` is gone from wrynose on, so inheriting it makes BitBake stop (\"Could not inherit file classes/cve-check.bbclass\"), and this layer is checked against wrynose; replace `INHERIT += \"cve-check\"` with `OE_FRAGMENTS += \"core/yocto/sbom-cve-check\"`",
        "7 error: class `siteconfig` is gone from styhead on, so inheriting it makes BitBake stop (\"Could not inherit file classes/siteconfig.bbclass\"), and this layer is checked against wrynose; remove the inherit",
        "8 error: `debug-tweaks` in IMAGE_FEATURES: no longer a valid value from walnascar on, so the recipe is skipped there (\"is not a valid image feature\"), and this layer is checked against wrynose; write the features it stood for: `allow-empty-password allow-root-login empty-root-password post-install-logging`",
        "9 warning: `license-incompatible` in WARN_QA: no longer a valid value from styhead on, so it has no effect there, and this layer is checked against wrynose; write `license-exception` (the check was renamed)",
        "10 error: `PNBLACKLIST` is a renamed variable in every supported release: setting it makes BitBake stop (\"Variable PNBLACKLIST has been renamed\"), and this layer is checked against kirkstone, scarthgap and wrynose; write `SKIP_RECIPE`",
        "11 warning: no whitespace around the assignment operator: from whinlatter on BitBake warns (\"has a lack of whitespace around the assignment\"), and this layer is checked against wrynose; write `NAME = \"value\"` with a space on each side",
        "13 warning: `${oldincludedir}`: OE-core no longer sets `oldincludedir` from scarthgap on, so the reference stays unexpanded there, and this layer is checked against scarthgap and wrynose; use `includedir`",
    ];
    assert_eq!(got, want);
}

#[test]
fn release_removed_bad_recipe_for_kirkstone_only() {
    // Kirkstone has everything later releases removed: only the renames older than the
    // floor (an error everywhere) and the obsolete license (a QA warning there) are left.
    let got = removed("bad.bb", "x_1.0.bb", &[Release::Kirkstone]);
    let head: Vec<&str> = got
        .iter()
        .map(|r| r.split(": ").next().unwrap_or(""))
        .collect();
    assert_eq!(head, ["2 warning", "10 error"], "{got:#?}");
    assert!(
        got[0].contains(
            "OE-core's obsolete-license QA check warns about it, and from styhead on it is an error"
        ),
        "{got:#?}"
    );
    assert!(got[1].contains("checked against kirkstone;"), "{got:#?}");
}

#[test]
fn release_removed_good_recipe_and_conf() {
    assert_eq!(removed("good.bb", "x_1.0.bb", &MINE), Vec::<String>::new());
    let got = removed("bad.conf", "conf/layer.conf", &MINE);
    assert_eq!(
        got,
        [
            "3 error: `LAYERSERIES_COMPAT_old = \"${LAYERSERIES_CORENAMES}\"`: from mickledore on BitBake deletes LAYERSERIES_CORENAMES after the core layer's layer.conf, so the reference stays literal and the layer is refused, and this layer is checked against scarthgap and wrynose; write the release names the layer is tested with",
            "4 error: class `icecc` is gone from whinlatter on, so inheriting it makes BitBake stop (\"Could not inherit file classes/icecc.bbclass\"), and this layer is checked against wrynose; remove the inherit",
            "5 error: `DISTRO_FEATURES_BACKFILL_CONSIDERED` is a renamed variable from wrynose on: setting it makes BitBake stop (\"Variable DISTRO_FEATURES_BACKFILL_CONSIDERED has been renamed\"), and this layer is checked against wrynose; write `DISTRO_FEATURES_OPTED_OUT`",
        ],
        "line 6 (`S` in a .conf, the core layer's default) is not reported"
    );
}

#[test]
fn release_removed_skips_a_class_or_variable_a_layer_provides() {
    // meta-mine brings its own `siteconfig` class and sets `oldincludedir`: neither is
    // reported.
    let e = env("wrynose", |root| {
        write(
            root,
            "meta-mine/classes/siteconfig.bbclass",
            "SITECONFIG = \"1\"\n",
        );
        write(
            root,
            "meta-mine/conf/distro/include/old.inc",
            "oldincludedir = \"${prefix}/include\"\n",
        );
        write(
            root,
            "meta-mine/recipes-app/x/x_1.0.bb",
            "SUMMARY = \"x\"\nLICENSE = \"MIT\"\ninherit siteconfig\ndo_install() {\n    install -d ${D}${oldincludedir}\n}\n",
        );
    });
    let found = lint_layer(
        &e,
        "meta-mine",
        &["hygiene/release-removed"],
        Options::default(),
    );
    // The assignment itself is still reported: OE-core no longer reads it.
    let r = rows(&found);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        r[0].starts_with("warning hygiene/release-removed meta-mine:conf/distro/include/old.inc:1: `oldincludedir`: OE-core no longer reads it"),
        "{r:?}"
    );
}
