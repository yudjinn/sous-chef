//! `lint --fix` (src/lint/fix.rs): every fix on its fixtures, evaluation checks for the fixes
//! that claim to preserve meaning, and the engine (dry run, write, rounds, idempotence,
//! writable layers, refusals) on temporary copies of the fixture workspace.
//!
//! Per rule with a fix: `tests/fixtures/lint/<group>/<rule>/bad.*` (or `fixable.*` when
//! `bad.*` also holds cases without a fix) is fixed in memory, in rounds as `--write` does,
//! into exactly `fixed.*` beside it (byte for byte); the fixed text has no finding of that
//! rule and no finding of any file-scope rule that the bad text did not have; fixing it
//! again changes nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::eval::{self, Overlay};
use sous_chef::index::Index;
use sous_chef::lint::fix::{self, FixOptions, Safety};
use sous_chef::lint::rules::{
    hygiene_continuation::HygieneContinuation, hygiene_release_removed::HygieneReleaseRemoved,
    hygiene_release_syntax::HygieneReleaseSyntax, hygiene_workdir_s::HygieneWorkdirS,
    org_class_export_functions::OrgClassExportFunctions,
    org_include_must_exist::OrgIncludeMustExist, org_inherit_defer::OrgInheritDefer,
    org_layer_conf::OrgLayerConf, override_append_no_space::OverrideAppendNoSpace,
    override_append_vs_plus_equals::OverrideAppendVsPlusEquals,
    override_filesextrapaths::OverrideFilesextrapaths,
    override_legacy_syntax::OverrideLegacySyntax, override_missing_package::OverrideMissingPackage,
    override_plus_equals_on_default::OverridePlusEqualsOnDefault,
    override_plus_equals_with_override::OverridePlusEqualsWithOverride,
    override_weak_after_default::OverrideWeakAfterDefault,
};
use sous_chef::lint::{self, Finding, LintCliArgs, Options, Rule, Scope, Target};
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
    /// A copy of the fixture workspace with `files` (`(path under ws, content)`) added, and
    /// `writable` as the writable layers.
    fn with(files: &[(&str, &str)], writable: &[&str]) -> Env {
        Env::with_layers(files, writable, &[])
    }

    /// [`Env::with`], with `extra` (directories of the copy) added to the copy's `BBLAYERS`.
    fn with_layers(files: &[(&str, &str)], writable: &[&str], extra: &[&str]) -> Env {
        let (tmp, root) = temp_ws();
        let root = fs::canonicalize(root).unwrap();
        for (rel, content) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        if !extra.is_empty() {
            let bblayers = root.join("build/conf/bblayers.conf");
            let text = fs::read_to_string(&bblayers).unwrap();
            let more: String = extra
                .iter()
                .map(|l| format!("  ${{TOPDIR}}/../{l} \\\n"))
                .collect();
            let anchor = "  ${TOPDIR}/../meta-mine \\\n";
            assert!(text.contains(anchor));
            fs::write(&bblayers, text.replace(anchor, &format!("{anchor}{more}"))).unwrap();
        }
        let cache = tempfile::tempdir().unwrap();
        let mut config = ws_config(&root);
        config.writable_layers = writable.iter().map(|s| s.to_string()).collect();
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
        Env::with(&[], &["meta-mine"])
    }

    fn fix(&mut self, target: &Target, suggested: bool, write: bool) -> fix::FixReport {
        fix::run(
            &self.ws,
            &mut self.index,
            &self.config,
            target,
            &Options::default(),
            &FixOptions { suggested, write },
        )
        .unwrap()
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }
}

fn fixture(rel: &str) -> String {
    fs::read_to_string(fixture_path(&format!("lint/{rel}"))).unwrap()
}

fn loc_of(path: &str) -> Loc {
    Loc {
        layer: "fixture".into(),
        path: PathBuf::from(path),
        lines: LineSpan::line(1),
    }
}

fn kind(path: &str) -> Option<FileKind> {
    FileKind::from_path(Path::new(path))
}

/// The findings of every file-scope rule on `text` as a file at `path`.
fn all_findings(env: &Env, text: &str, path: &str) -> Vec<Finding> {
    lint::all_rules()
        .iter()
        .filter(|r| r.scope() == Scope::File && r.default_enabled())
        .flat_map(|r| {
            lint::check_text(
                *r,
                &env.ws,
                &env.index,
                loc_of(path),
                text,
                kind(path),
                &[],
                None,
            )
        })
        .collect()
}

/// A finding without its line, and with the line numbers a message quotes taken out.
fn key(f: &Finding) -> (String, String) {
    let mut out = String::new();
    let mut rest = f.message.as_str();
    while let Some(at) = rest.find("line ") {
        out.push_str(&rest[..at + 5]);
        rest = rest[at + 5..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    (f.rule.to_string(), out)
}

fn fix_text(env: &Env, rules: &[&dyn Rule], text: &str, path: &str, suggested: bool) -> String {
    fix::fix_text(
        rules,
        &env.ws,
        &env.index,
        &loc_of(path),
        text,
        kind(path),
        &[],
        suggested,
    )
}

/// Fixes `bad` (in rounds, suggested fixes included) and checks it against `fixed`: equal
/// byte for byte, no finding of the rule left except `left` (lines), no new finding of any
/// file-scope rule, and a second fix changes nothing.
fn check_fixture(rule: &dyn Rule, dir: &str, bad: &str, fixed: &str, left: &[u32]) {
    check_fixture_revealing(rule, dir, bad, fixed, left, &[]);
}

/// [`check_fixture`], where the findings `revealed` (`(rule, line)`) are expected on the
/// fixed text: findings on a line whose legacy name was converted, which the other rules
/// could not see before (they skip legacy names).
fn check_fixture_revealing(
    rule: &dyn Rule,
    dir: &str,
    bad: &str,
    fixed: &str,
    left: &[u32],
    revealed: &[(&str, u32)],
) {
    let env = Env::new();
    let path = bad; // the extension decides the file kind
    let bad_text = fixture(&format!("{dir}/{bad}"));
    let want = fixture(&format!("{dir}/{fixed}"));
    let got = fix_text(&env, &[rule], &bad_text, path, true);
    assert_eq!(got, want, "{dir}/{bad} fixed");
    let remaining: Vec<u32> = lint::check_text(
        rule,
        &env.ws,
        &env.index,
        loc_of(path),
        &got,
        kind(path),
        &[],
        None,
    )
    .iter()
    .map(|f| f.loc.lines.start)
    .collect();
    assert_eq!(remaining, left, "{} left on {dir}/{fixed}", rule.id());
    let mut before: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in all_findings(&env, &bad_text, path) {
        *before.entry(key(&f)).or_default() += 1;
    }
    for f in all_findings(&env, &got, path) {
        if revealed.contains(&(f.rule, f.loc.lines.start)) {
            continue;
        }
        let n = before.entry(key(&f)).or_default();
        assert!(*n > 0, "new finding on {dir}/{fixed}: {f:?}");
        *n -= 1;
    }
    assert_eq!(
        fix_text(&env, &[rule], &got, path, true),
        got,
        "{dir}/{fixed}: a second fix changes nothing"
    );
}

/// The safety classes of a rule's fixes on a fixture, in file order.
fn safeties(rule: &dyn Rule, dir: &str, name: &str) -> Vec<(u32, Safety)> {
    let env = Env::new();
    let text = fixture(&format!("{dir}/{name}"));
    fix::fixes_in_text(
        rule,
        &env.ws,
        &env.index,
        loc_of(name),
        &text,
        kind(name),
        &[],
    )
    .iter()
    .map(|f| (f.finding.loc.lines.start, f.fix.safety))
    .collect()
}

const S: Safety = Safety::Safe;
const G: Safety = Safety::Suggested;

// ---------------------------------------------------------------------------------------
// Fixtures, per rule

#[test]
fn legacy_syntax_fixes() {
    let r = &OverrideLegacySyntax;
    let dir = "override/legacy-syntax";
    // `EXTRA_OECONF_remove` in a recipe is now `override/remove-in-recipe`'s business.
    check_fixture_revealing(
        r,
        dir,
        "bad.bb",
        "fixed.bb",
        &[],
        &[("override/remove-in-recipe", 4)],
    );
    check_fixture(r, dir, "bad.conf", "fixed.conf", &[]);
    check_fixture(r, dir, "bad-more.bb", "fixed-more.bb", &[]);
    // x7: convert-overrides.py's package variables; all safe (the old names are variables
    // nothing reads).
    check_fixture(r, dir, "package-vars.bb", "package-vars-fixed.bb", &[]);
    assert!(
        safeties(r, dir, "package-vars.bb")
            .iter()
            .all(|(_, s)| *s == S)
    );
    // A machine or distro suffix alone is only suggested.
    assert_eq!(
        safeties(r, dir, "bad.bb"),
        [
            (2, S),
            (3, S),
            (4, S),
            (5, S),
            (6, S),
            (7, S),
            (8, G),
            (9, G),
            (10, S),
            (11, S),
            (14, S),
            (17, S)
        ]
    );
    let env = Env::new();
    let bad = fixture(&format!("{dir}/bad.bb"));
    let safe_only = fix_text(&env, &[r], &bad, "bad.bb", false);
    let want = fixture(&format!("{dir}/fixed.bb"))
        .replace("KERNEL_DEVICETREE:qemux86", "KERNEL_DEVICETREE_qemux86")
        .replace("EXTRA_OECONF:testdistro", "EXTRA_OECONF_testdistro");
    assert_eq!(safe_only, want);
}

#[test]
fn legacy_syntax_fix_keeps_crlf_and_everything_outside_the_names() {
    let env = Env::new();
    let r = &OverrideLegacySyntax;
    let crlf = |s: &str| s.replace('\n', "\r\n");
    let bad = crlf(&fixture("override/legacy-syntax/bad.conf"));
    let want = crlf(&fixture("override/legacy-syntax/fixed.conf"));
    assert_eq!(fix_text(&env, &[r], &bad, "bad.conf", false), want);
    // Trailing whitespace, a comment and a value mentioning a legacy name are kept.
    let text = "# DEPENDS_append in a comment\nDEPENDS_append = \" a DEPENDS_append\"   \n";
    assert_eq!(
        fix_text(&env, &[r], text, "x.bb", false),
        "# DEPENDS_append in a comment\nDEPENDS:append = \" a DEPENDS_append\"   \n"
    );
}

#[test]
fn append_no_space_fixes() {
    let r = &OverrideAppendNoSpace;
    let dir = "override/append-no-space";
    check_fixture(r, dir, "bad.bb", "fixed.bb", &[]);
    check_fixture(r, dir, "bad-ref.bb", "fixed-ref.bb", &[]);
    assert!(safeties(r, dir, "bad.bb").iter().all(|(_, s)| *s == S));
}

#[test]
fn plus_equals_with_override_fixes() {
    let r = &OverridePlusEqualsWithOverride;
    let dir = "override/plus-equals-with-override";
    check_fixture(r, dir, "bad.bb", "fixed.bb", &[]);
    assert!(safeties(r, dir, "bad.bb").iter().all(|(_, s)| *s == G));
    // Suggested only: a safe-only run leaves the file alone.
    let env = Env::new();
    let bad = fixture(&format!("{dir}/bad.bb"));
    assert_eq!(fix_text(&env, &[r], &bad, "bad.bb", false), bad);
}

#[test]
fn missing_package_fixes() {
    let r = &OverrideMissingPackage;
    let dir = "override/missing-package";
    check_fixture(r, dir, "bad.bb", "fixed.bb", &[]);
    assert!(safeties(r, dir, "bad.bb").iter().all(|(_, s)| *s == G));
}

#[test]
fn weak_after_default_fixes() {
    let r = &OverrideWeakAfterDefault;
    let dir = "override/weak-after-default";
    check_fixture(r, dir, "bad.bb", "fixed.bb", &[]);
    assert!(safeties(r, dir, "bad.bb").iter().all(|(_, s)| *s == S));
    // `export NAME ??=` also sets the export flag: no fix.
    let env = Env::new();
    let text = "A = \"x\"\nexport A ??= \"y\"\n";
    assert_eq!(fix_text(&env, &[r], text, "x.bb", true), text);
}

#[test]
fn append_vs_plus_equals_fixes() {
    let r = &OverrideAppendVsPlusEquals;
    let dir = "override/append-vs-plus-equals";
    check_fixture(r, dir, "bad.bb", "fixed.bb", &[]);
    assert!(safeties(r, dir, "bad.bb").iter().all(|(_, s)| *s == G));
}

#[test]
fn filesextrapaths_fixes() {
    let r = &OverrideFilesextrapaths;
    let dir = "override/filesextrapaths";
    check_fixture(r, dir, "fixable.bbappend", "fixed.bbappend", &[]);
    assert_eq!(
        safeties(r, dir, "fixable.bbappend"),
        [(2, S), (3, G), (4, S), (5, S), (6, G)]
    );
    // bad-dot.bb: the conditional `=.` (line 4) has no fix and stays.
    check_fixture(r, dir, "bad-dot.bb", "fixed-dot.bb", &[4]);
    // bad.bbappend: `+=` (line 4) has no fix.
    assert_eq!(safeties(r, dir, "bad.bbappend"), [(2, S), (3, G), (5, S)]);
    // A blank between the quote and the path: no colon fix (the blank would stay in the
    // entry).
    let env = Env::new();
    let text = "FILESEXTRAPATHS:prepend := \"${THISDIR}/files \"\n";
    assert_eq!(fix_text(&env, &[r], text, "x.bbappend", true), text);
}

#[test]
fn continuation_fixes() {
    let r = &HygieneContinuation;
    let dir = "hygiene/continuation";
    check_fixture(r, dir, "fixable.bb", "fixed.bb", &[]);
    // bad.bb: the whitespace (line 2) is fixed, the blank line ending a statement is not.
    assert_eq!(safeties(r, dir, "bad.bb"), [(2, S)]);
}

#[test]
fn release_removed_fixes() {
    let r = &HygieneReleaseRemoved;
    let dir = "hygiene/release-removed";
    // Line 8: `WHITELIST_GPL-3.0-only` takes another value form under its new name: no fix.
    check_fixture(r, dir, "fixable.bb", "fixed.bb", &[8]);
    assert_eq!(
        {
            let mut s = safeties(r, dir, "fixable.bb");
            s.sort();
            s
        },
        [(3, S), (4, S), (5, S), (6, S), (7, S)]
    );
    // The old bad fixture: only PNBLACKLIST[foo] (line 10) has a fix.
    assert_eq!(safeties(r, dir, "bad.bb"), [(10, S)]);
}

/// The fixes of `rule` on fixture `dir/name` for a layer checked against `releases`.
fn safeties_for(
    rule: &dyn Rule,
    dir: &str,
    name: &str,
    releases: &[sous_chef::release::Release],
) -> Vec<(u32, Safety)> {
    let env = Env::new();
    let text = fixture(&format!("{dir}/{name}"));
    fix::fixes_in_text(
        rule,
        &env.ws,
        &env.index,
        loc_of(name),
        &text,
        kind(name),
        releases,
    )
    .iter()
    .map(|f| (f.finding.loc.lines.start, f.fix.safety))
    .collect()
}

#[test]
fn a_rename_is_fixed_only_where_no_release_still_has_the_old_name() {
    use sous_chef::release::Release;
    // DISTRO_FEATURES_BACKFILL_CONSIDERED is renamed from Wrynose on: a layer that also
    // claims Whinlatter still needs the old name there.
    let env = Env::new();
    let text = "DISTRO_FEATURES_BACKFILL_CONSIDERED = \"pulseaudio\"\n";
    let fixes = |releases: &[Release]| {
        fix::fixes_in_text(
            &HygieneReleaseRemoved,
            &env.ws,
            &env.index,
            loc_of("conf/distro/x.conf"),
            text,
            kind("conf/distro/x.conf"),
            releases,
        )
        .into_iter()
        .map(|f| f.fix.description)
        .collect::<Vec<_>>()
    };
    assert_eq!(
        fixes(&[Release::Wrynose]),
        ["rename `DISTRO_FEATURES_BACKFILL_CONSIDERED` to `DISTRO_FEATURES_OPTED_OUT`"]
    );
    assert!(fixes(&[Release::Whinlatter, Release::Wrynose]).is_empty());
}

#[test]
fn workdir_s_fixes_only_when_every_release_has_unpackdir() {
    use sous_chef::release::Release;
    let r = &HygieneWorkdirS;
    let dir = "hygiene/workdir-s";
    check_fixture(r, dir, "bad.bb", "fixed.bb", &[]);
    assert_eq!(safeties(r, dir, "bad.bb"), [(3, S), (5, S)]);
    // A layer that also claims scarthgap (no UNPACKDIR there): reported, no fix, and the
    // message says to do it by hand.
    let mixed = [Release::Scarthgap, Release::Wrynose];
    assert_eq!(safeties_for(r, dir, "bad.bb", &mixed), []);
    let env = Env::new();
    let text = fixture(&format!("{dir}/bad.bb"));
    let found = lint::check_text(
        r,
        &env.ws,
        &env.index,
        loc_of("bad.bb"),
        &text,
        kind("bad.bb"),
        &mixed,
        None,
    );
    assert_eq!(found.len(), 2);
    assert!(
        found[0].message.ends_with(
            "; no automatic fix while the layer also claims scarthgap, which has no UNPACKDIR"
        ),
        "{found:?}"
    );
}

#[test]
fn release_syntax_workdir_file_fixes_only_when_every_release_has_unpackdir() {
    use sous_chef::release::Release;
    let r = &HygieneReleaseSyntax;
    let dir = "hygiene/release-syntax";
    check_fixture(r, dir, "fixable.bb", "fixed.bb", &[]);
    // One finding (and fix) per statement; every `${WORKDIR}/<local file>` of it changes.
    assert_eq!(safeties(r, dir, "fixable.bb"), [(5, S), (10, S)]);
    let mixed = [Release::Kirkstone, Release::Wrynose];
    assert_eq!(safeties_for(r, dir, "fixable.bb", &mixed), []);
    // The bad fixture's other cases (inherit_defer, include_all) have none.
    assert_eq!(safeties(r, dir, "bad.bb"), [(7, S)]);
}

#[test]
fn class_export_functions_leaves_helpers_and_non_tasks_alone() {
    // x7: on OE-core the rule reported 23 helpers (do_qa_sysroot as a postfunc,
    // package_do_filedeps in PACKAGEFUNCS, do_package_deb called with exec_func, ...).
    // In the fixture workspace base.bbclass adds the standard tasks.
    let env = Env::new();
    let text = fixture("org/class-export-functions/helpers.bbclass");
    let path = "classes/mytool.bbclass";
    let found = lint::check_text(
        &OrgClassExportFunctions,
        &env.ws,
        &env.index,
        loc_of(path),
        &text,
        kind(path),
        &[],
        None,
    );
    let lines: Vec<String> = found.iter().map(|f| f.loc.lines.to_string()).collect();
    // Only `do_install()` defined directly: a standard task, used by nothing else.
    assert_eq!(lines, ["38-40"], "{found:?}");
}

#[test]
fn class_export_functions_fixes() {
    // The rule takes the class name from the file name: lint the fixture as mytool.bbclass.
    let env = Env::new();
    let r = &OrgClassExportFunctions;
    let bad = fixture("org/class-export-functions/bad.bbclass");
    let want = fixture("org/class-export-functions/fixed.bbclass");
    let path = "classes/mytool.bbclass";
    let got = fix_text(&env, &[r], &bad, path, true);
    assert_eq!(got, want);
    let left = lint::check_text(
        r,
        &env.ws,
        &env.index,
        loc_of(path),
        &got,
        kind(path),
        &[],
        None,
    );
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(fix_text(&env, &[r], &got, path, true), got);
    assert_eq!(
        fix_text(&env, &[r], &bad, path, false),
        bad,
        "suggested only"
    );
}

#[test]
fn rules_without_a_fix_offer_none() {
    // Every rule's fixes on its own bad fixtures: only the rules listed in lint-rules.md
    // ("Fixes") return any.
    let with_fix = [
        "override/legacy-syntax",
        "override/append-no-space",
        "override/plus-equals-with-override",
        "override/missing-package",
        "override/weak-after-default",
        "override/append-vs-plus-equals",
        "override/filesextrapaths",
        "hygiene/continuation",
        "org/class-export-functions",
        "org/layer-conf",
        "org/include-must-exist",
        "org/inherit-defer",
        "override/plus-equals-on-default",
        "hygiene/release-removed",
        "hygiene/workdir-s",
        "hygiene/release-syntax",
    ];
    let env = Env::new();
    for r in lint::all_rules() {
        if r.scope() != Scope::File || with_fix.contains(&r.id()) {
            continue;
        }
        let dir = fixture_path(&format!("lint/{}", r.id()));
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("bad") || !e.path().is_file() {
                continue;
            }
            let text = fs::read_to_string(e.path()).unwrap();
            let fixes = fix::fixes_in_text(
                *r,
                &env.ws,
                &env.index,
                loc_of(&name),
                &text,
                kind(&name),
                &[],
            );
            assert!(fixes.is_empty(), "{} offers a fix on {name}", r.id());
        }
    }
}

// ---------------------------------------------------------------------------------------
// Evaluation: the safe fixes that claim to preserve meaning

const RECIPE: &str = "meta-mine/recipes-test/fixme/fixme_1.0.bb";

/// Every name of an evaluation with its final value.
fn values(ds: &eval::DataStore) -> BTreeMap<String, Option<String>> {
    ds.names()
        .into_iter()
        .map(|n| {
            let v = ds.value(&n).map(|v| v.text);
            (n, v)
        })
        .collect()
}

/// `before` written as a recipe of meta-mine in a fresh copy of the fixture workspace, and
/// `after` in memory over it: the evaluations of both for the default machine (qemux86) and
/// distro (testdistro).
fn evaluate_pair(
    before: &str,
    after: &str,
) -> (
    BTreeMap<String, Option<String>>,
    BTreeMap<String, Option<String>>,
) {
    evaluate_pair_with(before, after, &[])
}

/// [`evaluate_pair`] in a copy with `extra` files (`(path under ws, content)`) written too.
fn evaluate_pair_with(
    before: &str,
    after: &str,
    extra: &[(&str, &str)],
) -> (
    BTreeMap<String, Option<String>>,
    BTreeMap<String, Option<String>>,
) {
    let mut files = vec![(RECIPE, before)];
    files.extend_from_slice(extra);
    let env = Env::with(&files, &["meta-mine"]);
    let target = eval::Target {
        recipe: Some(format!(
            "meta-mine:{}",
            RECIPE.trim_start_matches("meta-mine/")
        )),
        ..eval::Target::default()
    };
    let a = eval::evaluate(&env.index, &env.ws, &target).unwrap();
    let mut o = Overlay::new();
    o.insert(&env.ws, &env.root.join(RECIPE), after).unwrap();
    let b = eval::evaluate_with_overlay(&env.index, &env.ws, &target, &o).unwrap();
    (values(&a), values(&b))
}

fn fixed_in_memory(rule: &dyn Rule, text: &str) -> String {
    let env = Env::new();
    fix_text(&env, &[rule], text, "fixme_1.0.bb", true)
}

#[test]
fn weak_after_default_fix_keeps_every_value() {
    let bad = fixture("override/weak-after-default/bad.bb");
    let fixed = fixed_in_memory(&OverrideWeakAfterDefault, &bad);
    assert_ne!(bad, fixed);
    let (a, b) = evaluate_pair(&bad, &fixed);
    assert_eq!(a, b);
    assert_eq!(a.get("MY_A"), Some(&Some("x".to_string())));
    assert_eq!(a.get("MY_D:qemux86"), Some(&Some("x".to_string())));
}

#[test]
fn release_removed_srcpv_fix_keeps_every_value_where_srcpv_is_empty() {
    // From Nanbield OE-core's bitbake.conf sets `SRCPV = ""` (the fixture's sets nothing):
    // a layer.conf of the copy sets it as OE-core does, and removing `${SRCPV}` changes no
    // value.
    let bad = fixture("hygiene/release-removed/fixable.bb");
    let fixed = fixed_in_memory(&HygieneReleaseRemoved, &bad);
    assert_eq!(fixed, fixture("hygiene/release-removed/fixed.bb"));
    let layer_conf = format!(
        "{}SRCPV = \"\"\n",
        fs::read_to_string(fixture_path("ws/meta-mine/conf/layer.conf")).unwrap()
    );
    let (a, b) = evaluate_pair_with(&bad, &fixed, &[("meta-mine/conf/layer.conf", &layer_conf)]);
    // The renames: the new name holds the old one's value; nothing else changes.
    assert_eq!(
        changed(&a, &b),
        [
            "BB_ENV_EXTRAWHITE",
            "BB_ENV_PASSTHROUGH_ADDITIONS",
            "LICENSE_FLAGS_ACCEPTED",
            "LICENSE_FLAGS_WHITELIST"
        ]
    );
    assert_eq!(
        a.get("BB_ENV_EXTRAWHITE"),
        b.get("BB_ENV_PASSTHROUGH_ADDITIONS")
    );
    assert_eq!(
        a.get("LICENSE_FLAGS_WHITELIST"),
        b.get("LICENSE_FLAGS_ACCEPTED")
    );
    assert_eq!(a.get("PV"), Some(&Some("1.0+git".to_string())));
    assert_eq!(a.get("PV"), b.get("PV"));
}

/// The fixture's meta-mine layer.conf with `extra` appended.
fn layer_conf_with(extra: &str) -> String {
    format!(
        "{}{extra}",
        fs::read_to_string(fixture_path("ws/meta-mine/conf/layer.conf")).unwrap()
    )
}

/// The names whose values differ between two evaluations.
fn changed(
    a: &BTreeMap<String, Option<String>>,
    b: &BTreeMap<String, Option<String>>,
) -> Vec<String> {
    a.keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn workdir_s_fix_changes_only_s_to_the_unpack_directory() {
    // As OE-core from Styhead (`UNPACKDIR ??= "${WORKDIR}/sources-unpack"`).
    let conf = layer_conf_with("UNPACKDIR ??= \"${WORKDIR}/sources-unpack\"\n");
    let bad = fixture("hygiene/workdir-s/bad.bb");
    let fixed = fixed_in_memory(&HygieneWorkdirS, &bad);
    assert_eq!(fixed, fixture("hygiene/workdir-s/fixed.bb"));
    let (a, b) = evaluate_pair_with(&bad, &fixed, &[("meta-mine/conf/layer.conf", &conf)]);
    // B is `${S}` in the fixture's bitbake.conf, so it follows.
    assert_eq!(changed(&a, &b), ["B", "S", "S:class-native"]);
    let s = b.get("S").cloned().flatten().unwrap();
    assert!(s.ends_with("/sources-unpack"), "{s}");
    assert_eq!(Some(&Some(s)), b.get("UNPACKDIR"));
}

#[test]
fn release_syntax_fix_changes_only_the_functions_that_name_local_files() {
    let conf = layer_conf_with("UNPACKDIR ??= \"${WORKDIR}/sources-unpack\"\n");
    let bad = fixture("hygiene/release-syntax/fixable.bb");
    let fixed = fixed_in_memory(&HygieneReleaseSyntax, &bad);
    let (a, b) = evaluate_pair_with(&bad, &fixed, &[("meta-mine/conf/layer.conf", &conf)]);
    // Only function bodies change, and the evaluator gives a function's value as its
    // location, which the fix keeps: every value is equal.
    assert_eq!(a, b);
    // In the text: the local files go through UNPACKDIR, the sed line keeps WORKDIR (the
    // guide: "References to WORKDIR in sed commands are usually left as they are"), and
    // `main.c.orig`, no SRC_URI entry, keeps WORKDIR.
    assert!(fixed.contains("s:@WORK@:${WORKDIR}:"), "{fixed}");
    assert!(fixed.contains("cp ${WORKDIR}/main.c.orig"), "{fixed}");
    assert!(!fixed.contains("${WORKDIR}/app.conf"), "{fixed}");
    assert!(!fixed.contains("${WORKDIR}/main.c "), "{fixed}");
}

#[test]
fn continuation_fix_keeps_every_value() {
    let bad = fixture("hygiene/continuation/fixable.bb");
    let fixed = fixed_in_memory(&HygieneContinuation, &bad);
    assert_ne!(bad, fixed);
    let (a, b) = evaluate_pair(&bad, &fixed);
    assert_eq!(a, b);
    // (The words joined by the virtual-provider rewrite of DEPENDS.)
    assert_eq!(
        a.get("DEPENDS"),
        Some(&Some("zlib openssl bzip2".to_string()))
    );
}

/// The legacy-syntax fix cannot be checked by equality with the legacy file: to current
/// BitBake (and to sous-chef, which follows it) `RDEPENDS_${PN}` is another variable and a
/// name with `_append` is rejected, so the two evaluate differently by design. What the fix
/// must give is what the legacy file meant to the BitBake that accepted it (before 3.4
/// honister), where `_` was the override separator with exactly the rules `:` has now
/// (BitBake's 2021 switch changed the separator character only). So the test has three
/// parts:
///
/// 1. `MEANT` is that meaning written by hand, line by line from the legacy file, in the
///    current syntax. The fixed file must evaluate to exactly the same value for every
///    name (all of them, not only those the file sets), so the fix renamed what it had to
///    and touched nothing else.
/// 2. The values the legacy file meant, spelled out for the default machine (qemux86) and
///    distro (testdistro): the appends, prepends, removes and variants applied.
/// 3. The legacy file itself evaluates differently: equality there is expected to fail,
///    which is the reason for the fix.
const LEGACY: &str = "\
SUMMARY = \"legacy\"
DEPENDS = \"base\"
DEPENDS_append = \" zlib\"
SRC_URI = \"file://a.patch\"
SRC_URI_prepend = \"file://first.patch \"
EXTRA_OECONF = \"--enable-foo --enable-bar\"
EXTRA_OECONF_remove = \"--enable-foo\"
EXTRA_OECONF_append_qemux86 = \" --enable-qemu\"
EXTRA_OECONF_append_boardx = \" --enable-boardx\"
RDEPENDS_${PN} = \"bash\"
FILES_${PN}-dev += \"${includedir}/x\"
KERNEL_DEVICETREE = \"generic.dtb\"
KERNEL_DEVICETREE_qemux86 = \"x.dtb\"
EXTRA_OEMAKE = \"A=1\"
EXTRA_OEMAKE_testdistro = \"A=2\"
EXTRA_OEMAKE_class-native = \"A=3\"
CFLAGS_append = \" -g\"
CFLAGS_qemux86_append = \" -DQEMU\"
do_install() {
\techo base
}
do_install_append() {
\techo appended
}
pkg_postinst_${PN}() {
\techo postinst
}
";

const MEANT: &str = "\
SUMMARY = \"legacy\"
DEPENDS = \"base\"
DEPENDS:append = \" zlib\"
SRC_URI = \"file://a.patch\"
SRC_URI:prepend = \"file://first.patch \"
EXTRA_OECONF = \"--enable-foo --enable-bar\"
EXTRA_OECONF:remove = \"--enable-foo\"
EXTRA_OECONF:append:qemux86 = \" --enable-qemu\"
EXTRA_OECONF:append:boardx = \" --enable-boardx\"
RDEPENDS:${PN} = \"bash\"
FILES:${PN}-dev += \"${includedir}/x\"
KERNEL_DEVICETREE = \"generic.dtb\"
KERNEL_DEVICETREE:qemux86 = \"x.dtb\"
EXTRA_OEMAKE = \"A=1\"
EXTRA_OEMAKE:testdistro = \"A=2\"
EXTRA_OEMAKE:class-native = \"A=3\"
CFLAGS:append = \" -g\"
CFLAGS:qemux86:append = \" -DQEMU\"
do_install() {
\techo base
}
do_install:append() {
\techo appended
}
pkg_postinst:${PN}() {
\techo postinst
}
";

#[test]
fn legacy_syntax_fix_gives_what_the_legacy_file_meant() {
    let fixed = fixed_in_memory(&OverrideLegacySyntax, LEGACY);
    // 1. The fixed file means what MEANT says, for every name.
    let (meant, got) = evaluate_pair(MEANT, &fixed);
    assert_eq!(got, meant);
    // 2. ... which is what the legacy file meant.
    let v = |k: &str| got.get(k).cloned().flatten();
    assert_eq!(v("DEPENDS").as_deref(), Some("base zlib"));
    assert_eq!(
        v("SRC_URI").as_deref(),
        Some("file://first.patch file://a.patch")
    );
    assert_eq!(
        v("EXTRA_OECONF").as_deref(),
        Some(" --enable-bar --enable-qemu"),
        "removed, then the qemux86 append (boardx is not the machine)"
    );
    assert_eq!(v("RDEPENDS:fixme").as_deref(), Some("bash"));
    assert_eq!(v("KERNEL_DEVICETREE").as_deref(), Some("x.dtb"));
    assert_eq!(
        v("EXTRA_OEMAKE").as_deref(),
        Some("A=2"),
        "the distro variant"
    );
    // `CFLAGS_qemux86_append` appended to the variant CFLAGS_qemux86, which then replaced
    // CFLAGS for qemux86 although it had no value of its own (the same rule BitBake applies
    // to `CFLAGS:qemux86:append` now): the bitbake.conf default is gone.
    assert_eq!(v("CFLAGS").as_deref(), Some(" -DQEMU -g"));
    // A function's value is shown as pointers to its bodies: the definition, then the append.
    assert_eq!(
        v("do_install").as_deref(),
        Some(
            "meta-mine:recipes-test/fixme/fixme_1.0.bb:19-21\n\
             meta-mine:recipes-test/fixme/fixme_1.0.bb:22-24\n"
        )
    );
    // 3. The legacy file itself does not mean that to current BitBake.
    let (legacy, _) = evaluate_pair(LEGACY, &fixed);
    assert_ne!(legacy, meant);
    let l = |k: &str| legacy.get(k).cloned().flatten();
    assert_eq!(l("KERNEL_DEVICETREE").as_deref(), Some("generic.dtb"));
    assert_eq!(l("RDEPENDS:fixme"), None);
    assert_eq!(l("DEPENDS").as_deref(), Some("base"));
}

#[test]
fn append_no_space_fix_separates_the_word() {
    let bad = "DEPENDS = \"base\"\nDEPENDS:append = \"zlib\"\n";
    let fixed = fixed_in_memory(&OverrideAppendNoSpace, bad);
    let (a, b) = evaluate_pair(bad, &fixed);
    assert_eq!(a.get("DEPENDS"), Some(&Some("basezlib".to_string())));
    assert_eq!(b.get("DEPENDS"), Some(&Some("base zlib".to_string())));
}

// ---------------------------------------------------------------------------------------
// The engine

const MINE: &str = "meta-mine/recipes-app/old/old_1.0.bb";
const OLD: &str = "\
SUMMARY = \"old\"
LICENSE = \"MIT\"
DEPENDS_append = \" zlib\"
RDEPENDS_${PN} += \"bash\"
EXTRA_OECONF:qemux86 += \"--x\"
";
const OLD_FIXED_SAFE: &str = "\
SUMMARY = \"old\"
LICENSE = \"MIT\"
DEPENDS:append = \" zlib\"
RDEPENDS:${PN} += \"bash\"
EXTRA_OECONF:qemux86 += \"--x\"
";

fn files(env: &Env, rels: &[&str]) -> Target {
    Target::Files {
        files: rels.iter().map(|r| env.root.join(r)).collect(),
        lines: None,
    }
}

#[test]
fn a_dry_run_writes_nothing_and_shows_the_diff() {
    let mut env = Env::with(&[(MINE, OLD)], &["meta-mine"]);
    let t = files(&env, &[MINE]);
    let r = env.fix(&t, false, false);
    assert_eq!(env.read(MINE), OLD, "dry run");
    assert_eq!(r.rounds.len(), 1);
    let change = &r.rounds[0].changes[0];
    assert_eq!(change.file, "meta-mine:recipes-app/old/old_1.0.bb");
    assert_eq!(
        change.diff,
        "--- meta-mine:recipes-app/old/old_1.0.bb\n+++ meta-mine:recipes-app/old/old_1.0.bb\n\
         @@ -1,5 +1,5 @@\n SUMMARY = \"old\"\n LICENSE = \"MIT\"\n-DEPENDS_append = \" zlib\"\n\
         -RDEPENDS_${PN} += \"bash\"\n+DEPENDS:append = \" zlib\"\n+RDEPENDS:${PN} += \"bash\"\n \
         EXTRA_OECONF:qemux86 += \"--x\"\n"
    );
    assert_eq!(change.fixes.len(), 2);
    // The suggested fix is shown with its own diff, not applied.
    assert_eq!(r.suggested_skipped.len(), 1);
    assert_eq!(
        r.suggested_skipped[0].fixed.finding.rule,
        "override/plus-equals-with-override"
    );
    assert!(
        r.suggested_skipped[0]
            .diff
            .contains("+EXTRA_OECONF:append:qemux86 = \" --x\""),
        "{}",
        r.suggested_skipped[0].diff
    );
    let text = fix::render_text(&r);
    assert!(text.starts_with("lint --fix (dry run"), "{text}");
    assert!(
        text.contains(
            "fixes available: 3 for 3 findings (2 safe, 1 suggested); by rule: \
             override/legacy-syntax 2 safe; override/plus-equals-with-override 1 suggested"
        ),
        "{text}"
    );
    assert!(text.contains("would apply 2 fixes in 1 file"), "{text}");
    assert!(r.after.is_none());
}

#[test]
fn write_applies_relints_and_a_second_run_changes_nothing() {
    let mut env = Env::with(&[(MINE, OLD)], &["meta-mine"]);
    let t = files(&env, &[MINE]);
    let r = env.fix(&t, false, true);
    assert_eq!(env.read(MINE), OLD_FIXED_SAFE);
    let after = r.after.as_ref().unwrap();
    assert_eq!(after.files, ["meta-mine:recipes-app/old/old_1.0.bb"]);
    assert_eq!(after.fixed.len(), 2);
    assert_eq!(after.remaining.len(), 1, "the suggested one");
    assert!(after.new.is_empty(), "{:?}", after.new);
    assert!(after.problems.is_empty(), "{:?}", after.problems);
    // The index was refreshed: the second run sees the new text and changes nothing.
    let again = env.fix(&t, false, true);
    assert!(again.rounds[0].changes.is_empty());
    assert!(again.after.is_none());
    assert_eq!(env.read(MINE), OLD_FIXED_SAFE);
    // With suggested fixes the last one goes too, and then nothing is left.
    let r = env.fix(&t, true, true);
    assert_eq!(
        env.read(MINE),
        OLD_FIXED_SAFE.replace(
            "EXTRA_OECONF:qemux86 += \"--x\"",
            "EXTRA_OECONF:append:qemux86 = \" --x\""
        )
    );
    assert!(r.after.unwrap().new.is_empty());
    assert!(env.fix(&t, true, true).rounds[0].changes.is_empty());
}

#[test]
fn a_fix_that_reveals_a_finding_is_followed_by_another_round() {
    // `DEPENDS_append = "zlib"` glued the word under the old syntax too; once renamed,
    // override/append-no-space sees it and its fix runs in round 2.
    let text =
        "SUMMARY = \"x\"\nLICENSE = \"MIT\"\nDEPENDS = \"base\"\nDEPENDS_append = \"zlib\"\n";
    let mut env = Env::with(&[(MINE, text)], &["meta-mine"]);
    let t = files(&env, &[MINE]);
    let r = env.fix(&t, false, true);
    assert_eq!(
        env.read(MINE),
        "SUMMARY = \"x\"\nLICENSE = \"MIT\"\nDEPENDS = \"base\"\nDEPENDS:append = \" zlib\"\n"
    );
    assert_eq!(r.rounds.len(), 2);
    assert_eq!(
        r.rounds[1].changes[0].fixes[0].finding.rule,
        "override/append-no-space"
    );
    let after = r.after.unwrap();
    assert!(
        after.new.is_empty() && after.remaining.is_empty(),
        "{after:?}"
    );
    // The info-level style suggestion on the converted line was hidden by the old name.
    let revealed: Vec<(&str, u32)> = after
        .revealed
        .iter()
        .map(|f| (f.rule, f.loc.lines.start))
        .collect();
    assert_eq!(revealed, [("override/append-vs-plus-equals", 4)]);
    assert!(env.fix(&t, false, true).rounds[0].changes.is_empty());
}

#[test]
fn overlapping_fixes_wait_for_the_next_round() {
    // Line 2: weak-after-default removes the line, append-no-space would add a space in
    // it. The removal comes first; the other is deferred, and gone after it.
    let head = "SUMMARY = \"x\"\nLICENSE = \"MIT\"\n";
    let text = format!("{head}DEPENDS:append = \" a\"\nDEPENDS:append ??= \"b\"\n");
    let mut env = Env::with(&[(MINE, &text)], &["meta-mine"]);
    let t = files(&env, &[MINE]);
    let dry = env.fix(&t, false, false);
    assert_eq!(dry.rounds[0].deferred.len(), 1);
    assert_eq!(
        dry.rounds[0].deferred[0].finding.rule,
        "override/append-no-space"
    );
    let dry_text = fix::render_text(&dry);
    assert!(dry_text.contains("re-run after applying"));
    // x7: the dry run says that a write may take more rounds than the one it shows.
    assert!(
        dry_text.contains(
            "note: this is the first round only; --write re-lints the changed files and may \
             apply further fixes they make possible (up to 5 rounds)"
        ),
        "{dry_text}"
    );
    let r = env.fix(&t, false, true);
    assert_eq!(env.read(MINE), format!("{head}DEPENDS:append = \" a\"\n"));
    let after = r.after.unwrap();
    assert_eq!(after.fixed.len(), 2);
    assert!(
        after.new.is_empty() && after.remaining.is_empty(),
        "{after:?}"
    );
}

#[test]
fn files_in_layers_that_are_not_writable_are_never_touched() {
    // meta-legacy holds legacy syntax and is not writable (added as an extra layer: it is
    // not in BBLAYERS).
    let mut env = Env::with_layers(&[], &["meta-mine"], &["meta-legacy"]);
    let before = env.read("meta-legacy/recipes-legacy/oldapp/oldapp_1.0.bb");
    let r = env.fix(&Target::Layer("meta-legacy".into()), true, true);
    assert!(!r.not_writable.is_empty());
    assert!(r.not_writable.iter().all(|f| f.fix.layer == "meta-legacy"));
    assert!(r.rounds[0].changes.is_empty());
    assert!(r.after.is_none());
    assert_eq!(
        env.read("meta-legacy/recipes-legacy/oldapp/oldapp_1.0.bb"),
        before
    );
    let text = fix::render_text(&r);
    assert!(
        text.contains("in layers that are not writable, never applied (meta-legacy "),
        "{text}"
    );
}

#[test]
fn the_meta_legacy_layer_is_fixed_on_a_writable_copy() {
    // The same layer made writable (a temporary copy; tests/fixtures/ws is not changed).
    let mut env = Env::with_layers(&[], &["meta-mine", "meta-legacy"], &["meta-legacy"]);
    let t = Target::Layer("meta-legacy".into());
    let r = env.fix(&t, false, true);
    let after = r.after.as_ref().unwrap();
    assert!(after.new.is_empty(), "{:?}", after.new);
    assert!(
        !r.report
            .findings
            .iter()
            .filter(|f| f.rule == "override/legacy-syntax")
            .any(|f| after.remaining.contains(f)),
        "every legacy name of meta-legacy has a safe fix: {:?}",
        after.remaining
    );
    let again = env.fix(&t, false, true);
    assert!(again.rounds[0].changes.is_empty());
}

#[test]
fn a_file_changed_since_it_was_indexed_is_refused() {
    let mut env = Env::with(&[(MINE, OLD)], &["meta-mine"]);
    let t = files(&env, &[MINE]);
    // Changed on disk after the index was refreshed (the index is not refreshed again).
    let changed = format!("{OLD}# edited\n");
    fs::write(env.root.join(MINE), &changed).unwrap();
    let r = fix::run(
        &env.ws,
        &mut env.index,
        &env.config,
        &t,
        &Options::default(),
        &FixOptions {
            suggested: false,
            write: true,
        },
    )
    .unwrap();
    assert!(r.rounds[0].changes.is_empty());
    assert!(
        r.rounds[0]
            .refused
            .iter()
            .any(|l| l.contains("changed since it was indexed")),
        "{:?}",
        r.rounds[0].refused
    );
    assert_eq!(env.read(MINE), changed);
}

#[test]
fn layer_conf_fix_adds_bbpath() {
    let conf = "meta-mine/conf/layer.conf";
    let mut env = Env::new();
    let text = env.read(conf);
    let without = text.replace("BBPATH .= \":${LAYERDIR}\"\n\n", "\n");
    fs::write(env.root.join(conf), &without).unwrap();
    env.index.refresh(&env.ws).unwrap();
    let t = Target::Layer("meta-mine".into());
    let rules: [&dyn Rule; 1] = [&OrgLayerConf];
    let run = |env: &mut Env, suggested: bool, write: bool| {
        fix::run_with(
            &env.ws,
            &mut env.index,
            &env.config,
            &t,
            &Options::default(),
            &FixOptions { suggested, write },
            &rules,
            &lint::Replay,
        )
        .unwrap()
    };
    let dry = run(&mut env, false, false);
    assert_eq!(dry.suggested_skipped.len(), 1, "suggested only");
    let r = run(&mut env, true, true);
    // Before the first statement (the blank line left at the top stays where it was).
    assert_eq!(
        env.read(conf),
        without.replacen("BBFILES", "BBPATH .= \":${LAYERDIR}\"\nBBFILES", 1)
    );
    let after = r.after.unwrap();
    assert_eq!(after.fixed.len(), 1);
    assert!(
        after.new.is_empty() && after.remaining.is_empty(),
        "{after:?}"
    );
}

#[test]
fn the_cli_runs_fix_mode() {
    let mut env = Env::with(&[(MINE, OLD)], &["meta-mine"]);
    let mut out = Vec::new();
    let args = LintCliArgs {
        target: Some(format!(
            "meta-mine:{}",
            MINE.trim_start_matches("meta-mine/")
        )),
        fix: true,
        ..LintCliArgs::default()
    };
    let code = lint::cli_fix_with(&env.ws, &mut env.index, &env.config, &args, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("+DEPENDS:append = \" zlib\""), "{text}");
    assert_eq!(
        code, 1,
        "the legacy names are errors and still there (dry run)"
    );
    assert_eq!(env.read(MINE), OLD);
    let mut out = Vec::new();
    let args = LintCliArgs {
        write: true,
        ..args
    };
    let code = lint::cli_fix_with(&env.ws, &mut env.index, &env.config, &args, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("after: 1 file written; 2 findings fixed"),
        "{text}"
    );
    assert_eq!(code, 0, "only a warning is left");
    assert_eq!(env.read(MINE), OLD_FIXED_SAFE);
    let bad = LintCliArgs {
        write: true,
        ..LintCliArgs::default()
    };
    assert!(
        lint::cli_fix_with(&env.ws, &mut env.index, &env.config, &bad, &mut Vec::new()).is_err()
    );
}

/// Runs one rule's fixes on `target` (`suggested`, write), returns the report.
fn fix_one(env: &mut Env, rule: &dyn Rule, target: &Target, suggested: bool) -> fix::FixReport {
    let rules: [&dyn Rule; 1] = [rule];
    fix::run_with(
        &env.ws,
        &mut env.index,
        &env.config,
        target,
        &Options::default(),
        &FixOptions {
            suggested,
            write: true,
        },
        &rules,
        &lint::Replay,
    )
    .unwrap()
}

#[test]
fn include_must_exist_fix_makes_it_require() {
    let bb = "meta-mine/recipes-app/inc/inc_1.0.bb";
    let mut env = Env::with(
        &[
            (bb, "include inc-common.inc\nrequire inc-missing.inc\n"),
            (
                "meta-mine/recipes-app/inc/inc-common.inc",
                "LICENSE = \"MIT\"\n",
            ),
        ],
        &["meta-mine"],
    );
    let t = files(&env, &[bb]);
    let r = fix_one(&mut env, &OrgIncludeMustExist, &t, false);
    assert_eq!(r.available.len(), 1, "the missing require has no fix");
    assert_eq!(r.available[0].fix.safety, Safety::Safe);
    assert_eq!(
        env.read(bb),
        "require inc-common.inc\nrequire inc-missing.inc\n"
    );
    let after = r.after.unwrap();
    assert_eq!((after.fixed.len(), after.remaining.len()), (1, 1));
    assert!(after.new.is_empty());
}

#[test]
fn inherit_defer_fix_is_suggested() {
    let bb = "meta-mine/recipes-app/d/d_1.0.bb";
    let conf = fixture_path("ws/meta-mine/conf/layer.conf");
    let conf = fs::read_to_string(conf)
        .unwrap()
        .replace("kirkstone scarthgap wrynose", "scarthgap wrynose");
    let text = "DCLASS = \"autotoolsish\"\ninherit ${DCLASS}\nDCLASS = \"base\"\n";
    let mut env = Env::with(
        &[(bb, text), ("meta-mine/conf/layer.conf", &conf)],
        &["meta-mine"],
    );
    let t = files(&env, &[bb]);
    let r = fix_one(&mut env, &OrgInheritDefer, &t, false);
    assert_eq!(r.suggested_skipped.len(), 1);
    assert_eq!(env.read(bb), text, "suggested only");
    let r = fix_one(&mut env, &OrgInheritDefer, &t, true);
    assert_eq!(env.read(bb), text.replace("inherit ", "inherit_defer "));
    let after = r.after.unwrap();
    assert!(
        after.new.is_empty() && after.remaining.is_empty(),
        "{after:?}"
    );
}

#[test]
fn plus_equals_on_default_fix_is_shown_for_the_build_directory() {
    // The fixture's finding is in build/conf/local.conf, which is never written.
    let mut env = Env::new();
    let before = env.read("build/conf/local.conf");
    let r = fix_one(
        &mut env,
        &OverridePlusEqualsOnDefault,
        &Target::Layer("build".into()),
        true,
    );
    assert_eq!(env.read("build/conf/local.conf"), before);
    assert_eq!(r.not_writable.len(), 1);
    let f = &r.not_writable[0];
    assert_eq!(f.fix.safety, Safety::Suggested);
    assert_eq!(
        f.fix.apply_to(&before).unwrap(),
        before.replace(
            "DISTRO_FEATURES += \"wayland\"",
            "DISTRO_FEATURES:append = \" wayland\""
        )
    );
}
