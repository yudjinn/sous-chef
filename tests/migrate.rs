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
    // (x7: the scarthgap claim now appears in the no-fix clause, not as a rejecting release.)
    assert!(!r[0].contains("error in scarthgap"), "{r:?}");
    assert!(
        r[0].ends_with(
            "no automatic fix while the layer also claims scarthgap, which has no UNPACKDIR"
        ),
        "{r:?}"
    );

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

// ---------------------------------------------------------------------------------------
// The migration report

const OLD_RECIPE: &str = "\
SUMMARY = \"An old recipe\"
LICENSE = \"CLOSED\"
SRC_URI = \"file://old.c\"
S = \"${WORKDIR}\"
SERIAL_CONSOLE = \"115200 ttyS0\"
OLD_FLAGS=\"-O2\"
do_compile() {
    ${CC} ${WORKDIR}/old.c -o old
}
";

/// A Scarthgap workspace whose meta-mine claims only scarthgap, with a recipe that Wrynose
/// breaks.
fn scarthgap_env() -> Env {
    env("scarthgap", |root| {
        claim(root, "scarthgap");
        let core = root.join("meta-core/conf/layer.conf");
        let t = fs::read_to_string(&core).unwrap().replace(
            "LAYERSERIES_CORENAMES = \"wrynose\"",
            "LAYERSERIES_CORENAMES = \"scarthgap wrynose\"",
        );
        fs::write(&core, t).unwrap();
        write(root, "meta-mine/recipes-app/old/old_1.0.bb", OLD_RECIPE);
        write(
            root,
            "meta-mine/recipes-app/old/files/old.c",
            "int main(void) { return 0; }\n",
        );
    })
}

fn migrate_cli(e: &Env, args: lint::LintCliArgs) -> (i32, String) {
    let mut out = Vec::new();
    let code = lint::cli_with(&e.ws, &e.index, &e.config, &args, &mut out).unwrap();
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn migrate_to_wrynose_text_and_json() {
    let e = scarthgap_env();
    let (code, text) = migrate_cli(
        &e,
        lint::LintCliArgs {
            target: Some("meta-mine".into()),
            migrate_to: Some("wrynose".into()),
            ..lint::LintCliArgs::default()
        },
    );
    // S = "${WORKDIR}" is a new error: exit status 1.
    assert_eq!(code, 1, "{text}");
    insta::assert_snapshot!("migrate_meta_mine_to_wrynose_text", text);
    let (_, json) = migrate_cli(
        &e,
        lint::LintCliArgs {
            target: Some("meta-mine".into()),
            migrate_to: Some("wrynose".into()),
            format: Some(sous_chef::render::Format::Json),
            ..lint::LintCliArgs::default()
        },
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["schema"], "sous-chef-migration/1");
    assert_eq!(v["to"], "wrynose");
    insta::assert_snapshot!("migrate_meta_mine_to_wrynose_json", json);
    // Without the option, the same layer is clean of these.
    let (_, plain) = migrate_cli(
        &e,
        lint::LintCliArgs {
            target: Some("meta-mine".into()),
            rules: vec!["hygiene/workdir-s".into()],
            ..lint::LintCliArgs::default()
        },
    );
    assert!(plain.starts_with("no findings"), "{plain}");
}

#[test]
fn migrate_report_groups_feature_uses() {
    let e = scarthgap_env();
    let m = lint::migrate::report(
        &e.ws,
        &e.index,
        &e.config,
        &Target::Layer("meta-mine".into()),
        &Options::default(),
        Release::Wrynose,
    )
    .unwrap();
    let features: Vec<&str> = m.features.iter().map(|f| f.feature.as_str()).collect();
    assert!(features.contains(&"UnpackDir"), "{features:?}");
    assert!(features.contains(&"WorkdirAsSourceDir"), "{features:?}");
    // SERIAL_CONSOLE went in mickledore: it already fails on scarthgap, so it is a finding
    // that already holds, not a change wrynose makes.
    assert!(!features.contains(&"KeptUntilLangdale"), "{features:?}");
    assert!(
        m.existing_findings
            .iter()
            .any(|f| f.message.starts_with("`SERIAL_CONSOLE`")),
        "{:?}",
        m.existing_findings
    );
    assert!(
        features.contains(&"AssignmentWhitespaceWarning"),
        "{features:?}"
    );
    let unpack = m
        .features
        .iter()
        .find(|f| f.feature == "UnpackDir")
        .unwrap();
    let ptrs: Vec<&str> = unpack
        .groups
        .iter()
        .flat_map(|g| g.pointers.iter().map(String::as_str))
        .collect();
    assert!(
        ptrs.contains(&"meta-mine:recipes-app/old/old_1.0.bb:8"),
        "{ptrs:?}"
    );
    assert!(
        ptrs.contains(&"meta-mine:recipes-app/app/app_0.1.bb:22"),
        "{ptrs:?}"
    );
    let compat = &m.layers[0];
    assert_eq!(compat.layer, "meta-mine");
    assert_eq!(
        compat.pointer.as_deref(),
        Some("meta-mine:conf/layer.conf:11")
    );
    assert!(
        compat
            .advice
            .starts_with("write LAYERSERIES_COMPAT_mine = \"scarthgap wrynose\""),
        "{}",
        compat.advice
    );
    // The same report backwards, to kirkstone: nothing Kirkstone lacks is used except
    // the unpack directory convention (the recipe already uses WORKDIR).
    let back = lint::migrate::report(
        &e.ws,
        &e.index,
        &e.config,
        &Target::Layer("meta-mine".into()),
        &Options::default(),
        Release::Kirkstone,
    )
    .unwrap();
    assert!(
        back.new_findings
            .iter()
            .all(|f| f.rule != "hygiene/workdir-s")
    );
}

#[test]
fn migrate_to_rejects_bad_names_and_as_release() {
    let e = scarthgap_env();
    let mut out = Vec::new();
    let err = lint::cli_with(
        &e.ws,
        &e.index,
        &e.config,
        &lint::LintCliArgs {
            migrate_to: Some("thud".into()),
            ..lint::LintCliArgs::default()
        },
        &mut out,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("older than kirkstone"), "{err}");
}

#[test]
fn audit_with_migrate_to_has_section_7() {
    let e = scarthgap_env();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &e.ws).unwrap();
    index.refresh(&e.ws).unwrap();
    let handles =
        sous_chef::handles::HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = sous_chef::mcp::Ctx {
        config: e.config.clone(),
        workspace: e.ws.clone(),
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: sous_chef::bitbake::JobTable::new(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = fs::canonicalize(&e.root).unwrap();
    let opts = sous_chef::audit::Options {
        generated: Some("2026-11-18T00:00:00Z".into()),
        migrate_to: Some(Release::Wrynose),
        ..sous_chef::audit::Options::new(&root)
    };
    let a = rt.block_on(sous_chef::audit::build(&ctx, &opts)).unwrap();
    let md = sous_chef::audit::render_markdown(&a);
    let at = md
        .find("## 7. Migration to wrynose (6.0)")
        .expect("section 7");
    let section = &md[at..];
    assert!(section.contains("### New with wrynose"), "{section}");
    assert!(section.contains("hygiene/workdir-s"), "{section}");
    assert!(section.contains("`UnpackDir`"), "{section}");
    assert!(section.contains("### Not told by this report"), "{section}");
    let json = sous_chef::audit::render_json(&a);
    assert!(json.contains("\"migration\""), "{json}");
    // Without the option there is no section 7 and no `migration` field.
    let plain = rt
        .block_on(sous_chef::audit::build(
            &ctx,
            &sous_chef::audit::Options::new(&root),
        ))
        .unwrap();
    assert!(!sous_chef::audit::render_markdown(&plain).contains("## 7."));
    assert!(!sous_chef::audit::render_json(&plain).contains("\"migration\""));
}

// ---------------------------------------------------------------------------------------
// hygiene/release-syntax, made release-aware where the migration report showed a gap

fn syntax(text: &str, as_path: &str, releases: &[Release]) -> Vec<String> {
    use sous_chef::lint::rules::hygiene_release_syntax::HygieneReleaseSyntax;
    let cache = tempfile::tempdir().unwrap();
    let ws = fixture_ws();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    lint::check_text(
        &HygieneReleaseSyntax,
        &ws,
        &index,
        Loc {
            layer: "fixture".into(),
            path: PathBuf::from(as_path),
            lines: LineSpan::line(1),
        },
        text,
        FileKind::from_path(Path::new(as_path)),
        releases,
        None,
    )
    .into_iter()
    .map(|f| {
        format!(
            "{} {}: {}",
            f.loc.lines.start,
            f.severity.as_str(),
            f.message
        )
    })
    .collect()
}

#[test]
fn release_syntax_addfragments_by_release() {
    use Release::*;
    let three = "addfragments conf/fragments OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS\n";
    let four = "addfragments conf/fragments OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS OE_FRAGMENTS_BUILTIN\n";
    let got = syntax(three, "conf/x.conf", &[Scarthgap, Wrynose]);
    assert_eq!(got.len(), 2, "{got:#?}");
    assert!(
        got[0].starts_with("1 error: `addfragments` is not available in scarthgap"),
        "{got:#?}"
    );
    assert!(
        got[1].starts_with("1 error: `addfragments` with three arguments: from whinlatter on"),
        "{got:#?}"
    );
    assert!(got[1].contains("checked against wrynose;"), "{got:#?}");
    assert_eq!(
        syntax(four, "conf/x.conf", &[Wrynose]),
        Vec::<String>::new()
    );
    assert_eq!(
        syntax(three, "conf/x.conf", &[Walnascar]),
        Vec::<String>::new()
    );
}

#[test]
fn release_syntax_unpackdir_from_the_core_layer_does_not_hide_an_old_release() {
    // meta-core (collection core) sets UNPACKDIR, as OE-core does from Styhead; meta-mine
    // uses it and is linted as kirkstone too: reported. A definition in meta-mine itself
    // would keep it quiet.
    let e = env("wrynose", |root| {
        write(
            root,
            "meta-core/conf/unpack.conf",
            "UNPACKDIR ??= \"${WORKDIR}/sources-unpack\"\n",
        );
        write(
            root,
            "meta-mine/recipes-app/u/u_1.0.bb",
            "SUMMARY = \"u\"\nLICENSE = \"MIT\"\ndo_install() {\n    install -d ${D} ${UNPACKDIR}\n}\n",
        );
    });
    let opts = Options {
        as_release: Some(Release::Kirkstone),
        only_release: true,
        ..Options::default()
    };
    let found = lint_layer(&e, "meta-mine", &["hygiene/release-syntax"], opts.clone());
    let r = rows(&found);
    assert!(
        r.iter().any(|x| x.starts_with("error hygiene/release-syntax meta-mine:recipes-app/u/u_1.0.bb:4: `${UNPACKDIR}` is not available in kirkstone")),
        "{r:?}"
    );
    let e = env("wrynose", |root| {
        write(
            root,
            "meta-mine/conf/unpack.inc",
            "UNPACKDIR ??= \"${WORKDIR}/sources-unpack\"\n",
        );
        write(
            root,
            "meta-mine/recipes-app/u/u_1.0.bb",
            "SUMMARY = \"u\"\nLICENSE = \"MIT\"\ndo_install() {\n    install -d ${D} ${UNPACKDIR}\n}\n",
        );
    });
    let found = lint_layer(&e, "meta-mine", &["hygiene/release-syntax"], opts);
    assert!(
        rows(&found).iter().all(|x| !x.contains("u_1.0.bb:4")),
        "{:?}",
        rows(&found)
    );
}

#[test]
fn lint_tool_migrate_to_and_as_release() {
    let e = scarthgap_env();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &e.ws).unwrap();
    index.refresh(&e.ws).unwrap();
    let handles =
        sous_chef::handles::HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = sous_chef::mcp::Ctx {
        config: e.config.clone(),
        workspace: e.ws.clone(),
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: sous_chef::bitbake::JobTable::new(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    use sous_chef::mcp::tools::lint as tool;
    let out = rt
        .block_on(tool::run(
            &ctx,
            tool::Args {
                target: Some("meta-mine".into()),
                migrate_to: Some("wrynose".into()),
                budget_tokens: Some(150),
                ..tool::Args::default()
            },
        ))
        .unwrap();
    assert!(
        out.starts_with("migration to wrynose (6.0): 4 findings new with wrynose"),
        "{out}"
    );
    assert!(out.ends_with("format json has the whole report"), "{out}");
    assert!(sous_chef::render::estimate_tokens(&out) <= 150, "{out}");
    let out = rt
        .block_on(tool::run(
            &ctx,
            tool::Args {
                target: Some("meta-mine".into()),
                rules: Some(vec!["hygiene/workdir-s".into()]),
                as_release: Some("wrynose".into()),
                ..tool::Args::default()
            },
        ))
        .unwrap();
    assert!(out.starts_with("1 finding in"), "{out}");
    let err = rt
        .block_on(tool::run(
            &ctx,
            tool::Args {
                only_release: Some(true),
                ..tool::Args::default()
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(err.contains("only_release needs as_release"), "{err}");
}

#[test]
fn release_removed_srcpv_only_when_no_release_needs_it() {
    // Nanbield and later: `SRCPV = ""`, an info finding per `${SRCPV}` in PV.
    let got = removed(
        "fixable.bb",
        "x_git.bb",
        &[Release::Scarthgap, Release::Wrynose],
    )
    .into_iter()
    .filter(|r| r.contains("SRCPV"))
    .collect::<Vec<_>>();
    assert_eq!(
        got,
        [
            "3 info: `${SRCPV}` in PV: it is empty from nanbield on (the source revision goes to PKGV), and this layer is checked against scarthgap and wrynose only; write `PV = \"1.0+git\"`",
            "4 info: `${SRCPV}` in PV:append:x: it is empty from nanbield on (the source revision goes to PKGV), and this layer is checked against scarthgap and wrynose only; write `PV:append:x = \"\"`",
        ]
    );
    // A layer that also claims kirkstone needs it there: nothing.
    assert_eq!(
        removed("fixable.bb", "x_git.bb", &MINE)
            .iter()
            .filter(|r| r.contains("SRCPV"))
            .count(),
        0
    );
}
