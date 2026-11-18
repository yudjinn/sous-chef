//! Fixes for the bugs the live verification found in the evaluator and its tools
//! (docs/live-verification.md, B4, B5, B6, B11, B15, B18). Each test runs on a temporary
//! copy of the fixture workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::bitbake::JobTable;
use sous_chef::eval::{self, ConfigCache, DataStore, RefProblem, Target};
use sous_chef::handles::HandleStore;
use sous_chef::index::{Index, query};
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{deps, overrides, var};
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    #[allow(dead_code)]
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace on `release` after `edit(ws root)`, indexed.
fn modified(release: &str, edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root, _cfg, ws) = temp_ws_release(release, edit);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _cache: cache,
        _tmp: tmp,
        root,
        ws,
        idx,
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append_to(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

fn replace_in(root: &Path, rel: &str, from: &str, to: &str) {
    let p = root.join(rel);
    let s = fs::read_to_string(&p).unwrap();
    assert!(s.contains(from), "{rel} has no {from:?}");
    fs::write(p, s.replace(from, to)).unwrap();
}

fn eval(fx: &Fx, recipe: Option<&str>) -> DataStore {
    let t = Target {
        recipe: recipe.map(str::to_string),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

// ---------------------------------------------------------------------------------------
// B5: `inherit_defer toolchain/${TOOLCHAIN}` with python in OVERRIDES
// ---------------------------------------------------------------------------------------

/// OE-core Wrynose's shape: `OVERRIDES` holds python (the tune's `MACHINEOVERRIDES`) and
/// `${TCOVERRIDE}`, which only `toolchain/<toolchain>.bbclass` sets; `base.bbclass` picks
/// the toolchain with `TOOLCHAIN ??= "${PREFERRED_TOOLCHAIN}"` (which has `class-*`
/// variants) and inherits it deferred.
fn toolchain_ws(root: &Path) {
    replace_in(
        root,
        "meta-core/conf/bitbake.conf",
        "MACHINEOVERRIDES ?= \"${MACHINE}\"",
        "MACHINEOVERRIDES ?= \"${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}${MACHINE}\"",
    );
    replace_in(
        root,
        "meta-core/conf/bitbake.conf",
        "${CLASSOVERRIDE}${LIBCOVERRIDE}",
        "${CLASSOVERRIDE}:${TCOVERRIDE}${LIBCOVERRIDE}",
    );
    append_to(
        root,
        "meta-core/classes/base.bbclass",
        "\nPREFERRED_TOOLCHAIN_TARGET ??= \"gcc\"\n\
         PREFERRED_TOOLCHAIN_NATIVE ??= \"gcc\"\n\
         PREFERRED_TOOLCHAIN = \"${PREFERRED_TOOLCHAIN_TARGET}\"\n\
         PREFERRED_TOOLCHAIN:class-native = \"${PREFERRED_TOOLCHAIN_NATIVE}\"\n\
         TOOLCHAIN ??= \"${PREFERRED_TOOLCHAIN}\"\n\
         inherit_defer toolchain/${TOOLCHAIN}\n",
    );
    write(
        root,
        "meta-core/classes/toolchain/gcc.bbclass",
        "TCOVERRIDE = \"toolchain-gcc\"\nCC = \"gcc\"\n",
    );
    write(
        root,
        "meta-core/classes/toolchain/clang.bbclass",
        "TCOVERRIDE = \"toolchain-clang\"\nCC = \"clang\"\n",
    );
}

#[test]
fn deferred_toolchain_inherit_is_followed_when_overrides_hold_python() {
    let fx = modified("wrynose", toolchain_ws);
    let ds = eval(&fx, Some("hello"));
    let notes = ds.replay();
    // Not refused as python: the argument expands to complete text (`toolchain/gcc`).
    assert!(
        !notes
            .references
            .iter()
            .any(|r| r.kind == "inherit_defer" && r.problem == RefProblem::Python),
        "{:?}",
        notes.references
    );
    // The subdirectory class resolves along the class search path.
    assert!(
        ds.files()
            .iter()
            .any(|f| f.loc.path.ends_with("classes/toolchain/gcc.bbclass")),
        "toolchain/gcc.bbclass not read"
    );
    let tc = ds.value("TCOVERRIDE").unwrap();
    assert_eq!(tc.text, "toolchain-gcc");
    // ... but which class was chosen depended on unknown override entries: flagged.
    assert!(tc.unevaluated_python);
    assert!(ds.value("CC").unwrap().unevaluated_python);
    assert_eq!(notes.uncertain_references.len(), 1, "{notes:?}");
    let u = &notes.uncertain_references[0];
    assert_eq!(u.kind, "inherit_defer");
    assert_eq!(u.text, "toolchain/${TOOLCHAIN}");
    assert_eq!(u.expanded, "toolchain/gcc");
    assert!(u.python);
    // TCOVERRIDE is no longer an unresolved name of OVERRIDES, and its entry is there.
    let ov = ds.overrides_value().unwrap();
    assert!(!ov.unresolved.contains(&"TCOVERRIDE".to_string()), "{ov:?}");
    assert!(ds.overrides().contains(&"toolchain-gcc".to_string()));
    // A value that consults no override is not touched.
    let s = ds.value("TMPDIR").unwrap();
    assert!(!s.unevaluated_python && s.unresolved.is_empty(), "{s:?}");
}

#[test]
fn deferred_inherit_with_python_text_is_still_refused() {
    let fx = modified("wrynose", |root| {
        toolchain_ws(root);
        append_to(
            root,
            "meta-core/classes/base.bbclass",
            "OTHER_CLASSES = \"${@'toolchain/clang'}\"\ninherit_defer ${OTHER_CLASSES}\n",
        );
    });
    let ds = eval(&fx, Some("hello"));
    assert!(
        ds.replay()
            .references
            .iter()
            .any(|r| r.kind == "inherit_defer"
                && r.text == "${OTHER_CLASSES}"
                && r.problem == RefProblem::Python),
        "{:?}",
        ds.replay().references
    );
    assert_eq!(ds.value("CC").unwrap().text, "gcc");
}

#[test]
fn deferred_inherit_without_uncertainty_is_exact() {
    // No python in OVERRIDES: the class is followed and nothing is flagged.
    let fx = modified("wrynose", |root| {
        toolchain_ws(root);
        replace_in(
            root,
            "meta-core/conf/bitbake.conf",
            "${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}",
            "",
        );
    });
    let ds = eval(&fx, Some("hello"));
    let tc = ds.value("TCOVERRIDE").unwrap();
    assert_eq!(tc.text, "toolchain-gcc");
    assert!(!tc.unevaluated_python, "{tc:?}");
    assert!(ds.replay().uncertain_references.is_empty());
}

// ---------------------------------------------------------------------------------------
// Tools on a modified workspace
// ---------------------------------------------------------------------------------------

struct ToolFx {
    fx: Fx,
    ctx: Ctx,
}

fn tools(release: &str, edit: impl FnOnce(&Path)) -> ToolFx {
    let (tmp, root, cfg, ws) = temp_ws_release(release, edit);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let mut tool_idx = Index::open_in(cache.path(), &ws).unwrap();
    tool_idx.refresh(&ws).unwrap();
    let handles = HandleStore::open(&tool_idx.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config: cfg,
        workspace: ws.clone(),
        index: tokio::sync::Mutex::new(tool_idx),
        handles,
        jobs: JobTable::new(),
    };
    ToolFx {
        fx: Fx {
            _cache: cache,
            _tmp: tmp,
            root,
            ws,
            idx,
        },
        ctx,
    }
}

/// Table rows (header and note lines left out), split on tabs.
fn rows(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .skip(1)
        .filter(|l| l.contains('\t') && !l.starts_with("note:") && !l.starts_with("warning:"))
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

async fn overrides_out(f: &ToolFx, recipe: Option<&str>, show: overrides::Show) -> String {
    overrides::run(
        &f.ctx,
        overrides::Args {
            mc: None,
            recipe: recipe.map(str::to_string),
            machine: None,
            distro: None,
            show,
            budget_tokens: Some(20000),
            format: None,
        },
    )
    .await
    .unwrap()
}

async fn deps_out(f: &ToolFx, recipe: &str, kind: deps::Kind) -> String {
    deps::run(
        &f.ctx,
        deps::Args {
            mc: None,
            recipe: recipe.into(),
            kind,
            depth: None,
            budget_tokens: Some(20000),
            format: None,
        },
    )
    .await
    .unwrap()
}

/// The python shapes the live verification met on OE-core (B4, B6, B15): python in
/// `MACHINEOVERRIDES` with `:` inside it (the ti33x machine include), a `DEPENDS` /
/// `PACKAGES` / `PACKAGECONFIG` word that is python with spaces and commas (curl_8.19.0.bb),
/// a `PROVIDES` that is python only (libglvnd), and BitBake's own flags on `PACKAGECONFIG`
/// (bitbake.conf:942, documentation.conf:311).
fn python_lists(root: &Path) {
    replace_in(
        root,
        "meta-core/conf/bitbake.conf",
        "MACHINEOVERRIDES ?= \"${MACHINE}\"",
        "MACHINEOVERRIDES ?= \"${@['', 'ti-soc:ti33x:'][d.getVar('SOC_FAMILY') != '']}${MACHINE}\"",
    );
    append_to(
        root,
        "meta-core/conf/bitbake.conf",
        "\nPACKAGECONFIG[vardepvalue] = \"${PACKAGECONFIG}\"\n\
         PACKAGECONFIG[doc] = \"This variable provides a means of enabling or disabling features of a recipe on a per-recipe basis.\"\n",
    );
    append_to(
        root,
        "meta-core/recipes-core/hello/hello_1.1.bb",
        "\nDEPENDS = \"zlib\"\n\
         DEPENDS:append = \" ${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '', d)}\"\n\
         PACKAGES =+ \"${@bb.utils.contains('PTEST_ENABLED', '1', '${PN}-ptest', '', d)}\"\n\
         RDEPENDS:${PN}-ptest = \"bash\"\n\
         RDEPENDS:hello-ptest:append = \" sed\"\n\
         PACKAGECONFIG:append = \" ${@bb.utils.filter('DISTRO_FEATURES', 'ipv6', d)}\"\n",
    );
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "\nPROVIDES += \"${@bb.utils.contains('PACKAGECONFIG', 'egl', 'virtual/egl', '', d)}\"\n",
    );
}

#[tokio::test]
async fn overrides_chain_keeps_python_entries_whole() {
    let f = tools("wrynose", python_lists);
    let ds = eval(&f.fx, Some("hello"));
    // Not override names: the fragments and the accidental `ti33x` of a naive split.
    assert!(
        !ds.overrides()
            .iter()
            .any(|o| o.contains('\'') || o == "ti33x"),
        "{:?}",
        ds.overrides()
    );
    let out = overrides_out(&f, Some("hello"), overrides::Show::Chain).await;
    let table = rows(&out);
    assert!(
        table
            .iter()
            .all(|r| r[1].matches("${").count() == r[1].matches('}').count()),
        "{out}"
    );
    let py: Vec<&Vec<String>> = table.iter().filter(|r| r[1].contains("${@")).collect();
    assert_eq!(py.len(), 1, "{out}");
    assert_eq!(py[0][1], "${@...}qemux86");
    assert!(py[0][2].contains("python, not evaluated"), "{out}");
}

#[tokio::test]
async fn deps_build_shows_python_as_one_unknown_word() {
    let f = tools("wrynose", python_lists);
    let out = deps_out(&f, "hello", deps::Kind::Build).await;
    let table = rows(&out);
    assert!(
        !table
            .iter()
            .any(|r| r[1].contains('\'') || r[1].contains(',')),
        "python fragments as dependencies: {out}"
    );
    let py: Vec<&Vec<String>> = table.iter().filter(|r| r[1].contains("${@")).collect();
    assert_eq!(py.len(), 1, "{out}");
    assert_eq!(py[0][1], "${@bb.utils.contains(...)}");
    assert!(py[0][2].starts_with("unknown"), "{out}");
    // The index has no fragment as a provided name either (libglvnd's `'',`).
    for frag in ["'',", "'virtual/egl',", "'egl',"] {
        assert!(
            query::files_providing(&f.fx.idx, frag).unwrap().is_empty(),
            "{frag}"
        );
    }
}

#[tokio::test]
async fn packageconfig_lists_options_only() {
    let f = tools("wrynose", python_lists);
    let out = deps_out(&f, "hello", deps::Kind::Packageconfig).await;
    let opts: Vec<String> = rows(&out).into_iter().map(|r| r[0].clone()).collect();
    assert!(opts.contains(&"greeting".to_string()), "{out}");
    assert!(
        !opts.iter().any(|o| o == "doc" || o == "vardepvalue"),
        "{out}"
    );
    assert!(
        !opts.iter().any(|o| o.contains('\'') || o.ends_with(',')),
        "{out}"
    );
    assert!(
        opts.iter().any(|o| o == "${@bb.utils.filter(...)}"),
        "{out}"
    );
    assert!(
        out.contains("not options: PACKAGECONFIG[doc], [vardepvalue]"),
        "{out}"
    );
}

#[tokio::test]
async fn package_added_by_python_is_not_a_dead_override() {
    let f = tools("wrynose", python_lists);
    let out = overrides_out(&f, Some("hello"), overrides::Show::Dead).await;
    assert!(!rows(&out).iter().any(|r| r[0].contains("ptest")), "{out}");
}

// ---------------------------------------------------------------------------------------
// B11: `overrides show=dead`, variables read by name and inactive overrides
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn dead_leaves_out_variables_read_by_name_and_inactive_overrides() {
    let f = tools("wrynose", |root| {
        // OE-core's bitbake.conf:162 and a tune include of another architecture.
        append_to(
            root,
            "meta-core/conf/bitbake.conf",
            "\nDEFAULTTUNE ??= \"core2-32\"\n\
             TUNE_FEATURES ??= \"${TUNE_FEATURES:tune-${DEFAULTTUNE}}\"\n\
             include conf/machine/include/tune-cortexa8.inc\n",
        );
        write(
            root,
            "meta-core/conf/machine/include/tune-cortexa8.inc",
            "TUNE_FEATURES:tune-cortexa8 = \"arm vfp cortexa8\"\n\
             EXTRA_OECONF:append:boardx = \" --board\"\n\
             EXTRA_OECONF:append:nosuchboard = \" --typo\"\n",
        );
    });
    let out = overrides_out(&f, None, overrides::Show::Dead).await;
    let dead: Vec<String> = rows(&out).into_iter().map(|r| r[0].clone()).collect();
    // A typo is dead; a variable read by name is not; a machine override that is only
    // inactive for this machine is not.
    assert!(dead.contains(&"nosuchboard".to_string()), "{out}");
    assert!(!dead.iter().any(|d| d.starts_with("tune-")), "{out}");
    assert!(!dead.contains(&"boardx".to_string()), "{out}");
    assert!(out.contains("variables read by name"), "{out}");
    assert!(
        out.contains("inactive for this machine and distro, not dead"),
        "{out}"
    );
    // For a recipe, an `.inc` the configuration includes is not one of the recipe's files.
    let out = overrides_out(&f, Some("hello"), overrides::Show::Conditional).await;
    assert!(!out.contains("tune-cortexa8.inc"), "{out}");
}

// ---------------------------------------------------------------------------------------
// B18: the OVERRIDES uncertainty note only where an override decision is involved
// ---------------------------------------------------------------------------------------

async fn var_final(f: &ToolFx, name: &str) -> String {
    let out = var::run(
        &f.ctx,
        var::Args {
            mc: None,
            name: name.into(),
            recipe: Some("hello".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(20000),
            format: None,
        },
    )
    .await
    .unwrap();
    out.lines()
        .find(|l| l.starts_with("final:"))
        .unwrap_or_else(|| panic!("no final line: {out}"))
        .to_string()
}

#[tokio::test]
async fn overrides_uncertainty_is_noted_only_where_it_matters() {
    let f = tools("wrynose", |root| {
        // Part of OVERRIDES is genuinely unknown: a variable nothing sets.
        replace_in(
            root,
            "meta-core/conf/bitbake.conf",
            "${CLASSOVERRIDE}${LIBCOVERRIDE}",
            "${CLASSOVERRIDE}:${NOT_SET_ANYWHERE}${LIBCOVERRIDE}",
        );
        append_to(
            root,
            "meta-core/recipes-core/hello/hello_1.1.bb",
            "\nPLAIN_VALUE = \"a b\"\n\
             COND_VALUE = \"a\"\n\
             COND_VALUE:append:nosuchoverride = \" b\"\n\
             USES_COND = \"x ${COND_VALUE}\"\n\
             LEFT = \"${NOT_SET_EITHER} y\"\n",
        );
    });
    // No conditional operation anywhere in its history: certain, no note.
    assert_eq!(var_final(&f, "PLAIN_VALUE").await, "final: \"a b\"");
    // An inactive `:append:nosuchoverride` could be active if the unknown entry were
    // `nosuchoverride`: flagged, and said how (the text is complete).
    for name in ["COND_VALUE", "USES_COND"] {
        let l = var_final(&f, name).await;
        assert!(
            l.contains(
                "may differ: an override decision it depends on is uncertain because OVERRIDES has unresolved NOT_SET_ANYWHERE"
            ),
            "{l}"
        );
        assert!(!l.contains("left as written"), "{l}");
    }
    // A reference really left in the text.
    let l = var_final(&f, "LEFT").await;
    assert!(
        l.ends_with("(unresolved, left as written: NOT_SET_EITHER)"),
        "{l}"
    );
}
