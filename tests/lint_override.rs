//! T4.2: the eleven rules of the `override` group, on their fixtures in
//! `tests/fixtures/lint/override/<rule>/` (file-scope rules through `lint::check_text`,
//! recipe-scope rules through the real engine and evaluator on a temporary copy of the
//! fixture workspace), and the group over the whole fixture workspace against
//! `tests/fixtures/lint/override/workspace.expected`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::index::Index;
use sous_chef::lint::rules::{
    override_append_no_space::OverrideAppendNoSpace,
    override_append_vs_plus_equals::OverrideAppendVsPlusEquals, override_dead::OverrideDead,
    override_filesextrapaths::OverrideFilesextrapaths,
    override_legacy_syntax::OverrideLegacySyntax, override_missing_package::OverrideMissingPackage,
    override_plus_equals_on_default::OverridePlusEqualsOnDefault,
    override_plus_equals_with_override::OverridePlusEqualsWithOverride,
    override_remove_in_recipe::OverrideRemoveInRecipe, override_shadowed::OverrideShadowed,
    override_weak_after_default::OverrideWeakAfterDefault,
};
use sous_chef::lint::{self, Finding, Options, Replay, Rule, Severity, Target};
use sous_chef::types::{FileKind, LineSpan, Loc};
use sous_chef::workspace::Workspace;

// ---------------------------------------------------------------------------------------
// Harness

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    index: Index,
}

impl Env {
    /// A copy of the fixture workspace; `files` (`(path under ws, content)`) are added
    /// before the index is built.
    fn with(files: &[(&str, String)]) -> Env {
        let (tmp, root) = temp_ws();
        let root = fs::canonicalize(root).unwrap();
        for (rel, content) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let cache = tempfile::tempdir().unwrap();
        let config = ws_config(&root);
        let ws = Workspace::discover(&config).unwrap();
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

    fn new() -> Env {
        Env::with(&[])
    }

    fn run(&self, target: &Target, all: bool, rules: &[&dyn Rule]) -> lint::Report {
        let options = Options {
            all,
            ..Options::default()
        };
        lint::run_with(
            &self.ws,
            &self.index,
            &self.config,
            target,
            &options,
            rules,
            &Replay,
        )
        .unwrap()
    }
}

fn fixture(rule: &str, name: &str) -> String {
    fs::read_to_string(fixture_path(&format!("lint/override/{rule}/{name}"))).unwrap()
}

/// Runs a file-scope rule on a fixture as a file `path` of a layer outside the workspace.
fn check_at(env: &Env, rule: &dyn Rule, text: &str, path: &str) -> Vec<Finding> {
    let loc = Loc {
        layer: "fixture".into(),
        path: PathBuf::from(path),
        lines: LineSpan::line(1),
    };
    let out = lint::check_text(
        rule,
        &env.ws,
        &env.index,
        loc,
        text,
        FileKind::from_path(Path::new(path)),
        &[],
        None,
    );
    for f in &out {
        assert_eq!(f.rule, rule.id());
        assert!(!f.message.contains('\n'), "{}", f.message);
        assert!(
            f.message.contains("; "),
            "what, then what to write: {}",
            f.message
        );
    }
    out
}

/// `(line, severity)` of a rule's findings on `lint/override/<id>/<name>`.
fn lines(env: &Env, rule: &dyn Rule, name: &str) -> Vec<(u32, Severity)> {
    let dir = rule.id().trim_start_matches("override/");
    check_at(env, rule, &fixture(dir, name), name)
        .iter()
        .map(|f| (f.loc.lines.start, f.severity))
        .collect()
}

fn messages(env: &Env, rule: &dyn Rule, name: &str) -> Vec<String> {
    let dir = rule.id().trim_start_matches("override/");
    check_at(env, rule, &fixture(dir, name), name)
        .into_iter()
        .map(|f| f.message)
        .collect()
}

const E: Severity = Severity::Error;
const W: Severity = Severity::Warning;
const I: Severity = Severity::Info;

// ---------------------------------------------------------------------------------------
// File-scope rules

#[test]
fn legacy_syntax() {
    let env = Env::new();
    let r = &OverrideLegacySyntax;
    assert_eq!(
        lines(&env, r, "bad.bb"),
        [
            (2, E),
            (3, E),
            (4, E),
            (5, E),
            (6, E),
            (7, E),
            (8, E),
            (9, E),
            (10, E),
            (11, E),
            (14, E),
            (17, E)
        ]
    );
    let m = messages(&env, r, "bad.bb");
    assert!(m[0].contains("`DEPENDS_append`") && m[0].ends_with("write `DEPENDS:append`"));
    assert!(m[3].ends_with("write `RDEPENDS:${PN}`"), "{}", m[3]);
    assert!(m[4].ends_with("write `FILES:${PN}-dev`"), "{}", m[4]);
    assert!(
        m[5].ends_with("write `EXTRA_OECONF:append:qemux86`"),
        "{}",
        m[5]
    );
    assert!(
        m[6].ends_with("write `KERNEL_DEVICETREE:qemux86`"),
        "{}",
        m[6]
    );
    assert!(
        m[7].ends_with("write `EXTRA_OECONF:testdistro`"),
        "{}",
        m[7]
    );
    assert!(
        m[8].ends_with("write `EXTRA_OECONF:class-native`"),
        "{}",
        m[8]
    );
    assert!(m[9].ends_with("write `do_install:append()`"), "{}", m[9]);
    assert!(m[10].ends_with("write `pkg_postinst:${PN}()`"), "{}", m[10]);
    assert!(m[11].ends_with("write `CFLAGS:append`"), "{}", m[11]);
    assert_eq!(lines(&env, r, "bad.conf"), [(2, E), (3, E)]);
    assert_eq!(lines(&env, r, "good.bb"), []);
    assert_eq!(lines(&env, r, "good.conf"), []);
}

#[test]
fn legacy_syntax_on_meta_legacy() {
    // meta-legacy is not in BBLAYERS; its files are checked one by one (fixture README,
    // "meta-legacy": true positives, and SRCREV_machine, do_install(), the layer.conf
    // collection variables not flagged).
    let env = Env::new();
    let at = |rel: &str| -> Vec<u32> {
        let text = fs::read_to_string(fixture_path(&format!("ws/meta-legacy/{rel}"))).unwrap();
        check_at(&env, &OverrideLegacySyntax, &text, rel)
            .iter()
            .map(|f| f.loc.lines.start)
            .collect()
    };
    assert_eq!(
        at("recipes-legacy/oldapp/oldapp_1.0.bb"),
        [14, 16, 17, 19, 20, 21, 27, 31]
    );
    assert_eq!(at("recipes-legacy/oldapp/oldapp_%.bbappend"), [1]);
    assert_eq!(at("classes/oldclass.bbclass"), [4, 8]);
    assert_eq!(at("conf/layer.conf"), [] as [u32; 0]);
}

#[test]
fn legacy_syntax_matches_the_edit_hook_case() {
    // The edit hook's acceptance test (T7.4) relies on this: an edit introducing
    // `VAR_append` yields the rule.
    let env = Env::new();
    let got = check_at(
        &env,
        &OverrideLegacySyntax,
        "SUMMARY = \"x\"\nVAR_append = \" y\"\n",
        "recipes-x/x/x_1.0.bb",
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].loc.lines.start, 2);
    assert!(got[0].message.ends_with("write `VAR:append`"));
}

#[test]
fn append_no_space() {
    let env = Env::new();
    let r = &OverrideAppendNoSpace;
    assert_eq!(
        lines(&env, r, "bad.bb"),
        [(2, W), (3, W), (4, W), (5, W), (6, W)]
    );
    let m = messages(&env, r, "bad.bb");
    assert!(
        m[0].ends_with("write `DEPENDS:append = \" zlib\"`"),
        "{}",
        m[0]
    );
    assert!(
        m[1].ends_with("write `SRC_URI:prepend = \"file://first.patch \"`"),
        "{}",
        m[1]
    );
    assert!(
        m[4].ends_with("write `EXTRA_OECONF:append = \" --enable-foo --enable-bar\"`"),
        "{}",
        m[4]
    );
    assert_eq!(lines(&env, r, "good.bb"), []);
}

#[test]
fn plus_equals_with_override() {
    let env = Env::new();
    let r = &OverridePlusEqualsWithOverride;
    assert_eq!(
        lines(&env, r, "bad.bb"),
        [(2, W), (3, W), (4, W), (5, W), (6, W)]
    );
    let m = messages(&env, r, "bad.bb");
    assert!(
        m[0].ends_with("write `EXTRA_OECONF:append:qemux86 = \" --enable-qemu\"`"),
        "{}",
        m[0]
    );
    assert!(
        m[1].ends_with("write `DEPENDS:prepend:class-native = \"zlib-native \"`"),
        "{}",
        m[1]
    );
    assert!(
        m[2].ends_with("write `CFLAGS:append:testdistro = \" -g\"`"),
        "{}",
        m[2]
    );
    assert!(
        m[3].ends_with("write `RDEPENDS:${PN}:append:qemux86 = \" bash\"`"),
        "{}",
        m[3]
    );
    assert!(
        m[4].ends_with("write `EXTRA_OEMAKE:append:mymachine = \" X=1\"`"),
        "{}",
        m[4]
    );
    assert_eq!(lines(&env, r, "good.bb"), []);
}

#[test]
fn plus_equals_with_override_ignores_the_fixture_workspace_idiom() {
    // hello.inc uses `FILES:${PN} +=` (decided in wave 0: idiomatic, not a finding).
    let env = Env::new();
    let text =
        fs::read_to_string(fixture_path("ws/meta-core/recipes-core/hello/hello.inc")).unwrap();
    assert_eq!(
        check_at(&env, &OverridePlusEqualsWithOverride, &text, "hello.inc"),
        []
    );
}

#[test]
fn remove_in_recipe() {
    let env = Env::new();
    let r = &OverrideRemoveInRecipe;
    assert_eq!(lines(&env, r, "bad.bb"), [(3, W), (4, W)]);
    assert_eq!(lines(&env, r, "bad.bbappend"), [(2, I)]);
    assert_eq!(lines(&env, r, "good.bb"), []);
    // A class is like a recipe; configuration (a .conf, or an .inc under conf/) is like a
    // bbappend.
    let text = fixture("remove-in-recipe", "bad.bb");
    let sev = |path: &str| -> Vec<Severity> {
        check_at(&env, r, &text, path)
            .iter()
            .map(|f| f.severity)
            .collect()
    };
    assert_eq!(sev("classes-recipe/x.bbclass"), [W, W]);
    assert_eq!(sev("recipes-x/x/x.inc"), [W, W]);
    assert_eq!(sev("conf/machine/include/x.inc"), [I, I]);
    assert_eq!(sev("conf/distro/x.conf"), [I, I]);
}

#[test]
fn weak_after_default() {
    let env = Env::new();
    let r = &OverrideWeakAfterDefault;
    assert_eq!(lines(&env, r, "bad.bb"), [(3, I), (5, I), (7, I), (9, I)]);
    let m = messages(&env, r, "bad.bb");
    assert!(m[0].contains("line 2"), "{}", m[0]);
    assert!(m[2].contains("`MY_C[doc] ??=`"), "{}", m[2]);
    assert_eq!(lines(&env, r, "good.bb"), []);
}

#[test]
fn missing_package() {
    let env = Env::new();
    let r = &OverrideMissingPackage;
    assert_eq!(
        lines(&env, r, "bad.bb"),
        [(2, E), (3, E), (4, E), (5, E), (8, E), (9, E)]
    );
    let m = messages(&env, r, "bad.bb");
    assert!(m[0].ends_with("write `RDEPENDS:${PN}` (or the package it is for)"));
    assert!(m[2].contains("`RRECOMMENDS:${PN}:append`"), "{}", m[2]);
    assert!(m[3].contains("`pkg_postinst:${PN}()`"), "{}", m[3]);
    assert!(
        m[5].contains("`RPROVIDES:${PN}:append:qemux86`"),
        "{}",
        m[5]
    );
    assert_eq!(lines(&env, r, "good.bb"), []);
}

/// The fixture workspace plus `lint/override/missing-package/no-packages`: its recipes in
/// `meta-mine/recipes-images/imgs/`, and (with `classes`) its stand-in classes in
/// `meta-core/classes-recipe/`.
fn no_packages_env(classes: bool) -> Env {
    let dir = fixture_path("lint/override/missing-package/no-packages");
    let mut files: Vec<(String, String)> = Vec::new();
    for (sub, dest) in [
        ("recipes", "meta-mine/recipes-images/imgs"),
        ("classes", "meta-core/classes-recipe"),
    ] {
        if sub == "classes" && !classes {
            continue;
        }
        for f in files_under(&dir.join(sub)) {
            let name = f.file_name().unwrap().to_str().unwrap().to_string();
            files.push((format!("{dest}/{name}"), fs::read_to_string(&f).unwrap()));
        }
    }
    let refs: Vec<(&str, String)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    Env::with(&refs)
}

/// `path:line` of the missing-package findings in `meta-mine/recipes-images/imgs/`.
fn missing_package_in_images(env: &Env) -> Vec<String> {
    let dir = env.root.join("meta-mine/recipes-images/imgs");
    let files: Vec<PathBuf> = files_under(&dir);
    let report = env.run(
        &Target::Files { files, lines: None },
        false,
        &[&OverrideMissingPackage],
    );
    report
        .findings
        .iter()
        .map(|f| {
            format!(
                "{}:{}",
                f.loc.path.file_name().unwrap().to_str().unwrap(),
                f.loc.lines.start
            )
        })
        .collect()
}

#[test]
fn missing_package_is_not_reported_for_recipes_without_packages() {
    // OE-core's pkgvarcheck runs only when PACKAGES is not empty (insane.bbclass).
    let want = [
        "bad-packagegroup_1.0.bb:3",
        "bad-refilled_1.0.bb:4",
        "shared.inc:2",
    ];
    let env = no_packages_env(true);
    assert_eq!(missing_package_in_images(&env), want);
    // Without the classes indexed, `inherit core-image` still counts by name.
    let env = no_packages_env(false);
    let got = missing_package_in_images(&env);
    assert!(!got.iter().any(|g| g.starts_with("good-")), "{got:?}");
    assert!(got.contains(&"shared.inc:2".to_string()), "{got:?}");
}

#[test]
fn append_vs_plus_equals() {
    let env = Env::new();
    let r = &OverrideAppendVsPlusEquals;
    assert_eq!(lines(&env, r, "bad.bb"), [(3, I), (5, I)]);
    let m = messages(&env, r, "bad.bb");
    assert!(m[0].ends_with("write `DEPENDS += \"openssl\"`"), "{}", m[0]);
    assert!(m[1].ends_with("write `EXTRA_OEMAKE .= \"B\"`"), "{}", m[1]);
    assert_eq!(lines(&env, r, "good.bb"), []);
    assert_eq!(lines(&env, r, "good.bbappend"), []);
}

#[test]
fn plus_equals_on_default() {
    let env = Env::new();
    let r = &OverridePlusEqualsOnDefault;
    // EXTRA_AUTORECONF and AUTOTOOLS_SCRIPT_PATH get a `?=` in the recipe class
    // autotoolsish (parsed after every conf file); MY_LOCAL_LIST later in the same file.
    assert_eq!(lines(&env, r, "bad.conf"), [(2, W), (3, W), (5, W)]);
    let m = messages(&env, r, "bad.conf");
    assert!(
        m[0].contains("meta-core:classes-recipe/autotoolsish.bbclass:9"),
        "{}",
        m[0]
    );
    assert!(
        m[0].ends_with("write `EXTRA_AUTORECONF:append = \" --verbose\"`"),
        "{}",
        m[0]
    );
    assert!(m[1].contains("fixture:bad.conf:4"), "{}", m[1]);
    assert!(
        m[2].ends_with("write `AUTOTOOLS_SCRIPT_PATH:append = \"/sub\"`"),
        "{}",
        m[2]
    );
    assert_eq!(lines(&env, r, "good.conf"), []);
    // Only conf files.
    assert_eq!(
        check_at(
            &env,
            r,
            &fixture("plus-equals-on-default", "bad.conf"),
            "x.bb"
        ),
        []
    );
}

#[test]
fn plus_equals_on_default_uses_the_configuration_order() {
    // local.conf:13 `DISTRO_FEATURES += "wayland"` is parsed before testdistro.conf:5
    // `DISTRO_FEATURES ?= ...`; meta-mine's `BBFILES +=` comes after bblayers.conf's
    // `BBFILES ?= ""` and is not reported.
    let env = Env::new();
    let report = env.run(&Target::Workspace, true, &[&OverridePlusEqualsOnDefault]);
    let rows: Vec<String> = report
        .findings
        .iter()
        .map(|f| format!("{} {}", f.rule, f.loc))
        .collect();
    assert_eq!(
        rows,
        ["override/plus-equals-on-default build:conf/local.conf:13"]
    );
    assert!(
        report.findings[0]
            .message
            .contains("meta-core:conf/distro/testdistro.conf:5"),
        "{}",
        report.findings[0].message
    );
}

#[test]
fn filesextrapaths() {
    let env = Env::new();
    let r = &OverrideFilesextrapaths;
    assert_eq!(
        lines(&env, r, "bad.bbappend"),
        [(2, E), (3, E), (4, E), (5, E)]
    );
    let m = messages(&env, r, "bad.bbappend");
    assert!(m[0].contains("without the trailing `:`"), "{}", m[0]);
    assert!(
        m[0].ends_with("write `FILESEXTRAPATHS:prepend := \"${THISDIR}/files:\"`"),
        "{}",
        m[0]
    );
    assert!(m[1].contains("without `:=`"), "{}", m[1]);
    assert!(m[2].contains("without `:prepend`"), "{}", m[2]);
    assert!(m[3].contains("without the leading `:`"), "{}", m[3]);
    assert_eq!(lines(&env, r, "good.bbappend"), []);
    assert_eq!(lines(&env, r, "good.conf"), []);
    assert_eq!(lines(&env, r, "good.bb"), []);
}

// ---------------------------------------------------------------------------------------
// Recipe-scope rules, on the real datastore

/// A copy of the fixture workspace with `<rule>/<name>.bb` (and `.inc`, `.bbappend` of the
/// same name when they exist) as recipe `<pn>` in meta-mine.
fn recipe_env(rule: &str, name: &str, pn: &str) -> Env {
    let dir = format!("meta-mine/recipes-app/{pn}");
    let mut files = vec![(
        format!("{dir}/{pn}_1.0.bb"),
        fixture(rule, &format!("{name}.bb")),
    )];
    let inc = fixture_path(&format!("lint/override/{rule}/{name}.inc"));
    if inc.exists() {
        files.push((
            format!("{dir}/{name}.inc"),
            fs::read_to_string(inc).unwrap(),
        ));
    }
    let app = fixture_path(&format!("lint/override/{rule}/{name}.bbappend"));
    if app.exists() {
        files.push((
            format!("{dir}/{pn}_%.bbappend"),
            fs::read_to_string(app).unwrap(),
        ));
    }
    let refs: Vec<(&str, String)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    Env::with(&refs)
}

fn recipe_findings(env: &Env, pn: &str, rule: &dyn Rule) -> Vec<Finding> {
    let report = env.run(
        &Target::Recipe {
            name: pn.into(),
            version: None,
        },
        false,
        &[rule],
    );
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    for f in &report.findings {
        assert_eq!(f.rule, rule.id());
        assert!(!f.message.contains('\n'), "{}", f.message);
    }
    report.findings
}

fn rows(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|f| format!("{} {}", f.severity.as_str(), f.loc))
        .collect()
}

#[test]
fn dead_on_the_real_datastore() {
    let env = recipe_env("dead", "bad", "deadbad");
    let got = recipe_findings(&env, "deadbad", &OverrideDead);
    assert_eq!(
        rows(&got),
        [
            "warning meta-mine:recipes-app/deadbad/deadbad_1.0.bb:3",
            "warning meta-mine:recipes-app/deadbad/deadbad_1.0.bb:4",
            "warning meta-mine:recipes-app/deadbad/deadbad_1.0.bb:5",
            "warning meta-mine:recipes-app/deadbad/deadbad_1.0.bb:8",
        ]
    );
    assert!(got[0].message.contains("`boradx`"), "{}", got[0].message);
    assert!(got[0].message.contains("(`boardx`?)"), "{}", got[0].message);
    assert!(got[1].message.contains("`qemuarm`"), "{}", got[1].message);
    assert!(got[2].message.contains("`do_configure:nosuchdistro`"));
    assert!(got[3].message.contains("`nosuch`"), "{}", got[3].message);

    let env = recipe_env("dead", "good", "deadgood");
    assert_eq!(
        rows(&recipe_findings(&env, "deadgood", &OverrideDead)),
        [] as [&str; 0]
    );
}

#[test]
fn shadowed_on_the_real_datastore() {
    let env = recipe_env("shadowed", "bad", "shadowbad");
    let got = recipe_findings(&env, "shadowbad", &OverrideShadowed);
    assert_eq!(
        rows(&got),
        [
            "info meta-mine:recipes-app/shadowbad/bad.inc:1",
            "info meta-mine:recipes-app/shadowbad/shadowbad_1.0.bb:5",
        ]
    );
    assert!(
        got[0]
            .message
            .contains("meta-mine:recipes-app/shadowbad/shadowbad_1.0.bb:8"),
        "{}",
        got[0].message
    );
    assert!(
        got[1]
            .message
            .contains("meta-mine:recipes-app/shadowbad/shadowbad_1.0.bb:7"),
        "{}",
        got[1].message
    );

    let env = recipe_env("shadowed", "good", "shadowgood");
    assert_eq!(
        rows(&recipe_findings(&env, "shadowgood", &OverrideShadowed)),
        [] as [&str; 0]
    );
}

// ---------------------------------------------------------------------------------------
// The whole group over the fixture workspace

fn group() -> Vec<&'static dyn Rule> {
    lint::all_rules()
        .iter()
        .copied()
        .filter(|r| r.id().starts_with("override/"))
        .collect()
}

#[test]
fn the_group_has_eleven_rules() {
    assert_eq!(group().len(), 11);
}

#[test]
fn the_group_over_the_fixture_workspace_matches_the_expected_file() {
    let env = Env::new();
    let report = env.run(&Target::Workspace, true, &group());
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    let got: Vec<String> = report
        .findings
        .iter()
        .map(|f| format!("{}\t{}\t{}", f.severity.as_str(), f.rule, f.loc))
        .collect();
    let expected_path = fixture_path("lint/override/workspace.expected");
    let expected: Vec<String> = fs::read_to_string(&expected_path)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(got, expected, "{}", got.join("\n"));
    let _ = &env.root;
}
