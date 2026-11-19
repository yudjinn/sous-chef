//! Anonymous python writes that cannot happen (`eval::anon`), and which values stay
//! flagged: docs/bitbake-semantics.md, "Anonymous python writes that cannot happen" and
//! "When a value is flagged".
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::bitbake::JobTable;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::var;
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

struct Fx {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace with `recipe` written as
/// `meta-mine/recipes-app/<name>/<name>_1.0.bb` (`@ROOT@` replaced by the copy's root),
/// after `edit(root)`.
fn with_recipe(name: &str, recipe: &str, edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let dir = root.join(format!("meta-mine/recipes-app/{name}"));
    fs::create_dir_all(&dir).unwrap();
    let text = recipe.replace("@ROOT@", &root.display().to_string());
    fs::write(dir.join(format!("{name}_1.0.bb")), text).unwrap();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _tmp: tmp,
        _cache: cache,
        root,
        ws,
        idx,
    }
}

fn target(recipe: &str) -> Target {
    Target {
        mc: None,
        recipe: Some(recipe.to_string()),
        version: None,
        machine: Some("qemux86".into()),
        distro: None,
    }
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &target(recipe)).unwrap()
}

fn flagged(ds: &DataStore, name: &str) -> bool {
    let v = ds.value(name).unwrap();
    v.unevaluated_python || !v.unresolved.is_empty()
}

/// Indents every non-empty line of `block` by `n` spaces.
fn indent(block: &str, n: usize) -> String {
    block
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                format!("{}{l}", " ".repeat(n))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- base.bbclass "multimachine" handling (OE-core Wrynose 33a6bd6c, lines 623-736) ----

const SRC_URI_LOOP: &str = r#"srcuri = d.getVar('SRC_URI')
for uri_string in srcuri.split():
    uri = bb.fetch.URI(uri_string)
    if uri.scheme in ("http", "https", "ftp"):
        d.appendVarFlag('do_fetch', 'depends', ' wget-native:do_populate_sysroot')
"#;

const MULTIMACHINE: &str = r#"# 'multimachine' handling
mach_arch = d.getVar('MACHINE_ARCH')
pkg_arch = d.getVar('PACKAGE_ARCH')

if (pkg_arch == mach_arch):
    # Already machine specific - nothing further to do
    return

#
# We always try to scan SRC_URI for urls with machine overrides
# unless the package sets SRC_URI_OVERRIDES_PACKAGE_ARCH=0
#
override = d.getVar('SRC_URI_OVERRIDES_PACKAGE_ARCH')
if override != '0':
    paths = []
    fpaths = (d.getVar('FILESPATH') or '').split(':')
    machine = d.getVar('MACHINE')
    for p in fpaths:
        if os.path.basename(p) == machine and os.path.isdir(p):
            paths.append(p)

    if paths:
        for s in srcuri.split():
            if not s.startswith("file://"):
                continue
            fetcher = bb.fetch2.Fetch([s], d)
            local = fetcher.localpath(s)
            for mp in paths:
                if local.startswith(mp):
                    #bb.note("overriding PACKAGE_ARCH from %s to %s for %s" % (pkg_arch, mach_arch, pn))
                    d.setVar('PACKAGE_ARCH', "${MACHINE_ARCH}")
                    return

packages = d.getVar('PACKAGES').split()
for pkg in packages:
    pkgarch = d.getVar("PACKAGE_ARCH_%s" % pkg)

    # We could look for != PACKAGE_ARCH here but how to choose
    # if multiple differences are present?
    # Look through PACKAGE_ARCHS for the priority order?
    if pkgarch and pkgarch == mach_arch:
        d.setVar('PACKAGE_ARCH', "${MACHINE_ARCH}")
        bb.warn("Recipe %s is marked as only being architecture specific but seems to have machine specific packages?! The recipe may as well mark itself as machine specific directly." % d.getVar("PN"))
"#;

fn mm_recipe(package_arch: &str, src_uri: &str, extra: &str, tail: &str) -> String {
    format!(
        "LICENSE = \"CLOSED\"\nMACHINE_ARCH = \"qemux86\"\nPACKAGE_ARCH = \"{package_arch}\"\n\
         SRC_URI = \"{src_uri}\"\nFILESPATH = \"@ROOT@/files/qemux86:@ROOT@/files\"\n\
         WORKDIR_X = \"/work/${{PACKAGE_ARCH}}/x\"\n{extra}\npython () {{\n{}\n\n{}\n}}\n",
        indent(SRC_URI_LOOP, 4),
        indent(tail, 4)
    )
}

#[test]
fn multimachine_package_arch_already_machine_arch() {
    let fx = with_recipe(
        "mm",
        &mm_recipe("qemux86", "file://a.patch", "", MULTIMACHINE),
        |_| {},
    );
    let ds = eval(&fx, "mm");
    assert!(!flagged(&ds, "PACKAGE_ARCH"));
    assert!(!flagged(&ds, "WORKDIR_X"));
    let w = &ds.replay().no_writes;
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].name, "PACKAGE_ARCH");
    assert!(
        w[0].why[0].1.contains("already equals MACHINE_ARCH"),
        "{w:?}"
    );
}

#[test]
fn multimachine_without_file_entries_or_machine_directories() {
    // No file:// entry: the FILESPATH branch never looks.
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "http://example.com/a.tar.gz", "", MULTIMACHINE),
        |_| {},
    );
    let ds = eval(&fx, "mm");
    assert!(!flagged(&ds, "WORKDIR_X"));
    assert!(
        ds.replay().no_writes[0].why[0]
            .1
            .contains("no file:// entry")
    );
    // A file:// entry, but no FILESPATH directory named after MACHINE exists.
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "file://a.patch", "", MULTIMACHINE),
        |_| {},
    );
    let ds = eval(&fx, "mm");
    assert!(!flagged(&ds, "WORKDIR_X"));
    assert!(
        ds.replay().no_writes[0].why[0]
            .1
            .contains("no FILESPATH directory")
    );
}

#[test]
fn multimachine_with_a_machine_directory_stays_flagged() {
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "file://a.patch", "", MULTIMACHINE),
        |root| {
            fs::create_dir_all(root.join("files/qemux86")).unwrap();
        },
    );
    let ds = eval(&fx, "mm");
    assert!(flagged(&ds, "PACKAGE_ARCH"));
    assert!(flagged(&ds, "WORKDIR_X"));
    assert!(ds.replay().no_writes.is_empty());
    // The cause is named, at the variable that carries it.
    let causes = ds.flag_causes("WORKDIR_X");
    assert_eq!(causes.len(), 1, "{causes:?}");
    assert!(
        causes[0].starts_with("anonymous python at meta-mine:recipes-app/mm/mm_1.0.bb:")
            && causes[0].ends_with("may write PACKAGE_ARCH"),
        "{causes:?}"
    );
    let _ = &fx.root;
}

#[test]
fn multimachine_package_arch_of_a_package() {
    // A PACKAGE_ARCH_<pkg> equal to MACHINE_ARCH: the write may happen.
    let extra = "PACKAGE_ARCH_${PN}-dev = \"qemux86\"";
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "http://x/a.tar.gz", extra, MULTIMACHINE),
        |_| {},
    );
    assert!(flagged(&eval(&fx, "mm"), "WORKDIR_X"));
    // Another value: it cannot.
    let extra = "PACKAGE_ARCH_${PN}-dev = \"armv5\"";
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "http://x/a.tar.gz", extra, MULTIMACHINE),
        |_| {},
    );
    let ds = eval(&fx, "mm");
    assert!(!flagged(&ds, "WORKDIR_X"));
    assert!(
        ds.replay().no_writes[0].why[0]
            .1
            .contains("no PACKAGE_ARCH_<pkg> equals")
    );
}

#[test]
fn multimachine_code_that_differs_is_not_recognised() {
    // One line changed (the parentheses of the first test): the code is not OE-core's.
    let changed = MULTIMACHINE.replace("if (pkg_arch == mach_arch):", "if pkg_arch == mach_arch:");
    let fx = with_recipe(
        "mm",
        &mm_recipe("qemux86", "http://x/a.tar.gz", "", &changed),
        |_| {},
    );
    assert!(flagged(&eval(&fx, "mm"), "WORKDIR_X"));
    // A dedented write: unconditional.
    let dedented = MULTIMACHINE.replace(
        "        d.setVar('PACKAGE_ARCH', \"${MACHINE_ARCH}\")\n        bb.warn",
        "d.setVar('PACKAGE_ARCH', \"${MACHINE_ARCH}\")\n        bb.warn",
    );
    assert_ne!(dedented, MULTIMACHINE);
    let fx = with_recipe(
        "mm",
        &mm_recipe("qemux86", "http://x/a.tar.gz", "", &dedented),
        |_| {},
    );
    assert!(flagged(&eval(&fx, "mm"), "WORKDIR_X"));
}

#[test]
fn another_writer_keeps_the_flag() {
    let extra = "python () {\n    d.setVar('PACKAGE_ARCH', 'other')\n}";
    let fx = with_recipe(
        "mm",
        &mm_recipe("qemux86", "http://x/a.tar.gz", extra, MULTIMACHINE),
        |_| {},
    );
    let ds = eval(&fx, "mm");
    assert!(flagged(&ds, "WORKDIR_X"));
    assert!(ds.replay().no_writes.is_empty());
}

// ---- base.bbclass PACKAGECONFIG handling (OE-core Wrynose 33a6bd6c, lines 466-549) ----

const PKGCONF: &str = r#"pkgconfigflags = d.getVarFlags("PACKAGECONFIG") or {}
if pkgconfigflags:
    pkgconfig = (d.getVar('PACKAGECONFIG') or "").split()
    pn = d.getVar("PN")

    def appendVar(varname, appends):
        if not appends:
            return
        varname = d.expand(varname)
        d.appendVar(varname, " " + " ".join(appends))

    extradeps = []
    extrardeps = []
    extrarrecs = []
    extraconf = []
    for flag, flagval in sorted(pkgconfigflags.items()):
        items = flagval.split(",")
        num = len(items)
        if num > 6:
            bb.error("%s: PACKAGECONFIG[%s] Only enable,disable,depend,rdepend,rrecommend,conflict_packageconfig can be specified!"
                % (d.getVar('PN'), flag))

        if flag in pkgconfig:
            if num >= 3 and items[2]:
                extradeps.append(items[2])
            if num >= 4 and items[3]:
                extrardeps.append(items[3])
            if num >= 5 and items[4]:
                extrarrecs.append(items[4])
            if num >= 1 and items[0]:
                extraconf.append(items[0])
        elif num >= 2 and items[1]:
                extraconf.append(items[1])

        if num >= 6 and items[5]:
            conflicts = set(items[5].split())
            invalid = conflicts.difference(set(pkgconfigflags.keys()))
            if invalid:
                bb.error("%s: PACKAGECONFIG[%s] Invalid conflict package config%s '%s' specified."
                    % (d.getVar('PN'), flag, 's' if len(invalid) > 1 else '', ' '.join(invalid)))

            if flag in pkgconfig:
                intersec = conflicts.intersection(set(pkgconfig))
                if intersec:
                    bb.fatal("%s: PACKAGECONFIG[%s] Conflict package config%s '%s' set in PACKAGECONFIG."
                        % (d.getVar('PN'), flag, 's' if len(intersec) > 1 else '', ' '.join(intersec)))

    appendVar('DEPENDS', extradeps)
    appendVar('RDEPENDS:${PN}', extrardeps)
    appendVar('RRECOMMENDS:${PN}', extrarrecs)
    appendVar('PACKAGECONFIG_CONFARGS', extraconf)
"#;

fn pc_recipe(extra: &str) -> String {
    format!(
        "LICENSE = \"CLOSED\"\nPACKAGECONFIG_CONFARGS ??= \"\"\n\
         EXTRA_OECONF = \"--base ${{PACKAGECONFIG_CONFARGS}}\"\nRDEPENDS:${{PN}} = \"r\"\n\
         {extra}\npython () {{\n{}\n}}\n",
        indent(PKGCONF, 4)
    )
}

#[test]
fn packageconfig_lists_that_stay_empty() {
    // No flag at all.
    let fx = with_recipe("pc", &pc_recipe(""), |_| {});
    let ds = eval(&fx, "pc");
    assert!(!flagged(&ds, "EXTRA_OECONF"));
    assert!(!flagged(&ds, "RDEPENDS:pc"));
    // A flag that is off and has no "disable" item, one that is on and gives only a
    // build dependency: no configure argument either way, but DEPENDS may change.
    let fx = with_recipe(
        "pc",
        &pc_recipe(
            "PACKAGECONFIG = \"y\"\nPACKAGECONFIG[x] = \",,xdep\"\nPACKAGECONFIG[y] = \",,ydep\"",
        ),
        |_| {},
    );
    let ds = eval(&fx, "pc");
    assert!(!flagged(&ds, "EXTRA_OECONF"));
    assert!(!flagged(&ds, "RDEPENDS:pc"));
    assert!(ds.replay().python_vars.contains("DEPENDS"));
}

#[test]
fn packageconfig_lists_that_get_an_item() {
    for extra in [
        // Off, with a "disable" argument.
        "PACKAGECONFIG = \"\"\nPACKAGECONFIG[x] = \"--with-x,--without-x\"",
        // On, with an "enable" argument.
        "PACKAGECONFIG = \"x\"\nPACKAGECONFIG[x] = \"--with-x,--without-x\"",
        // BitBake reads every flag, `doc` too: its text after a comma is a "disable"
        // argument (`getVarFlags` returns all of them).
        "PACKAGECONFIG[doc] = \"Enables a, b and c\"",
        // A flag python may set.
        "python () {\n    d.setVarFlag('PACKAGECONFIG', 'z', '--z')\n}",
    ] {
        let fx = with_recipe("pc", &pc_recipe(extra), |_| {});
        let ds = eval(&fx, "pc");
        assert!(flagged(&ds, "EXTRA_OECONF"), "{extra}");
        assert!(
            ds.flag_causes("EXTRA_OECONF")
                .iter()
                .any(|c| c.contains("may write PACKAGECONFIG_CONFARGS")),
            "{extra}: {:?}",
            ds.flag_causes("EXTRA_OECONF")
        );
    }
}

// ---- base.bbclass: EXTRANATIVEPATH for hg:// ----

const HG: &str = r#"srcuri = d.getVar('SRC_URI')
for uri_string in srcuri.split():
    uri = bb.fetch.URI(uri_string)
    if uri.scheme == "svn":
        d.appendVarFlag('do_fetch', 'depends', ' subversion-native:do_populate_sysroot')
    elif uri.scheme == "hg":
        d.appendVar("EXTRANATIVEPATH", ' python3-native ')
"#;

#[test]
fn hg_fetcher_dependency() {
    for (src, written) in [
        ("git://example.com/r;branch=main http://x/y.tgz", false),
        ("hg://example.com/repo;module=m", true),
        ("HG://example.com/repo;module=m", true),
    ] {
        let recipe = format!(
            "LICENSE = \"CLOSED\"\nSRC_URI = \"{src}\"\nEXTRANATIVEPATH = \"a\"\nPATH_X = \"${{EXTRANATIVEPATH}}\"\n\
             python () {{\n{}\n}}\n",
            indent(HG, 4)
        );
        let fx = with_recipe("hg", &recipe, |_| {});
        assert_eq!(flagged(&eval(&fx, "hg"), "PATH_X"), written, "{src}");
    }
}

// ---- early returns and writes under `if` ----

#[test]
fn a_function_that_returns_at_once_writes_nothing() {
    let block = "python () {\n    # Do nothing if multilib isn't being used\n    if not d.getVar(\"MULTI\"):\n        return\n    for entry in (d.getVar(\"MULTI_SCRIPTS\") or \"\").split():\n        pkg, script = entry.split(\":\", 1)\n        d.appendVar(\"FILES:\" + pkg, \" \" + script)\n    d.setVar('ER_X', '1')\n}\n";
    let base = "LICENSE = \"CLOSED\"\nER_X = \"0\"\nFILES:${PN} = \"/f\"\n";
    let fx = with_recipe("er", &format!("{base}{block}"), |_| {});
    let ds = eval(&fx, "er");
    assert!(!flagged(&ds, "ER_X"));
    assert!(!flagged(&ds, "FILES:er"));
    let a = ds.replay().anonymous.last().unwrap();
    assert_eq!(
        a.returns_early.as_deref(),
        Some("it returns at once: `not d.getVar(\"MULTI\")` is true")
    );
    // MULTI set: it runs on.
    let fx = with_recipe("er", &format!("{base}MULTI = \"lib32\"\n{block}"), |_| {});
    let ds = eval(&fx, "er");
    assert!(flagged(&ds, "ER_X"));
    assert!(flagged(&ds, "FILES:er"));
    assert!(
        ds.replay()
            .anonymous
            .last()
            .unwrap()
            .returns_early
            .is_none()
    );
    // MULTI written by anonymous python: not known.
    let other = "python () {\n    d.setVar('MULTI', 'x')\n}\n";
    let fx = with_recipe("er", &format!("{base}{other}{block}"), |_| {});
    assert!(flagged(&eval(&fx, "er"), "ER_X"));
}

#[test]
fn writes_under_a_false_if() {
    let base = "LICENSE = \"CLOSED\"\nS_X = \"s\"\nUSES_S_X = \"<${S_X}>\"\n";
    let cases = [
        // bb.utils.contains on a feature that is not there: the write never runs.
        (
            "    if bb.utils.contains('DISTRO_FEATURES', 'zzz', True, False, d):\n        d.appendVar('S_X', ' y')\n",
            false,
        ),
        // Nested: the outer condition is false.
        (
            "    if d.getVar('MACHINE') == 'boardx':\n        for x in range(3):\n            d.appendVar('S_X', ' y')\n",
            false,
        ),
        // elif: its own condition decides its body.
        (
            "    if d.getVar('MACHINE') == 'qemux86':\n        pass\n    elif d.getVar('MACHINE') == 'boardx':\n        d.appendVar('S_X', ' y')\n",
            false,
        ),
        // True: the write runs.
        (
            "    if bb.utils.contains('DISTRO_FEATURES', 'wayland', True, False, d):\n        d.appendVar('S_X', ' y')\n",
            true,
        ),
        // One write under a false if, one not under any.
        (
            "    if d.getVar('MACHINE') == 'boardx':\n        d.appendVar('S_X', ' y')\n    d.appendVar('S_X', ' z')\n",
            true,
        ),
        // else of a false if: not reasoned about.
        (
            "    if d.getVar('MACHINE') == 'qemux86':\n        pass\n    else:\n        d.appendVar('S_X', ' y')\n",
            true,
        ),
        // A condition on a local variable: not known.
        (
            "    m = d.getVar('MACHINE')\n    if m == 'boardx':\n        d.appendVar('S_X', ' y')\n",
            true,
        ),
        // `d` rebound: conditions would read another datastore.
        (
            "    d = bb.data.createCopy(d)\n    if d.getVar('MACHINE') == 'boardx':\n        d.appendVar('S_X', ' y')\n",
            true,
        ),
        // On one line with its `if`.
        (
            "    if d.getVar('MACHINE') == 'boardx': d.appendVar('S_X', ' y')\n",
            true,
        ),
        // Inside a nested function.
        (
            "    if d.getVar('MACHINE') == 'boardx':\n        def f():\n            d.appendVar('S_X', ' y')\n        f()\n",
            true,
        ),
        // A continued condition.
        (
            "    if d.getVar('MACHINE') == 'boardx' and \\\n            True:\n        d.appendVar('S_X', ' y')\n",
            true,
        ),
    ];
    for (body, still) in cases {
        let fx = with_recipe("site", &format!("{base}python () {{\n{body}}}\n"), |_| {});
        let ds = eval(&fx, "site");
        assert_eq!(flagged(&ds, "USES_S_X"), still, "{body}");
        assert_eq!(flagged(&ds, "S_X"), still, "{body}");
    }
}

// ---- what does not flag, and what does (existing behaviour, for the record) ----

#[test]
fn python_that_cannot_reach_the_value_does_not_flag_it() {
    let recipe = "LICENSE = \"CLOSED\"\n\
        SHADOWED = \"${@os.getcwd()}\"\nSHADOWED = \"plain\"\n\
        DEFAULTED = \"set\"\nDEFAULTED ?= \"${@os.getcwd()}\"\n\
        WEAK ??= \"${@os.getcwd()}\"\nWEAK = \"set\"\n\
        INACTIVE = \"a\"\nINACTIVE:nosuchoverride = \"${@os.getcwd()}\"\n\
        INACTIVE:append:nosuchoverride = \" ${@os.getcwd()}\"\n\
        FLAGGED_FLAG = \"v\"\nFLAGGED_FLAG[doc] = \"${@os.getcwd()}\"\n\
        REMOVED = \"a b\"\nREMOVED:remove = \"${@os.getcwd()}\"\n\
        OTHER = \"${@os.getcwd()}\"\nUSES_NOTHING = \"x\"\n";
    let fx = with_recipe("cases", recipe, |_| {});
    let ds = eval(&fx, "cases");
    for name in [
        "SHADOWED",
        "DEFAULTED",
        "WEAK",
        "INACTIVE",
        "FLAGGED_FLAG",
        "USES_NOTHING",
    ] {
        assert!(!flagged(&ds, name), "{name}: {:?}", ds.value(name));
        assert!(ds.flag_causes(name).is_empty(), "{name}");
    }
    // A :remove of an unknown value can remove any word: flagged.
    assert!(flagged(&ds, "REMOVED"));
    let c = ds.flag_causes("REMOVED");
    assert!(
        c.iter()
            .any(|c| c.starts_with("REMOVED holds python that is not evaluated: `os.getcwd()`")),
        "{c:?}"
    );
    assert!(flagged(&ds, "OTHER"));
}

// ---- the var tool says why ----

fn make_ctx(fx: &Fx) -> (tempfile::TempDir, Ctx) {
    let cache = tempfile::tempdir().unwrap();
    let config = ws_config(&fx.root);
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    (
        cache,
        Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
    )
}

fn vargs(name: &str, recipe: &str, format: Option<Format>) -> var::Args {
    var::Args {
        mc: None,
        name: name.into(),
        recipe: Some(recipe.into()),
        machine: Some("qemux86".into()),
        distro: None,
        authoritative: false,
        flags: false,
        budget_tokens: Some(4000),
        format,
    }
}

#[tokio::test]
async fn the_var_tool_names_the_cause() {
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "file://a.patch", "", MULTIMACHINE),
        |root| {
            fs::create_dir_all(root.join("files/qemux86")).unwrap();
        },
    );
    let (_c, ctx) = make_ctx(&fx);
    let text = var::run(&ctx, vargs("WORKDIR_X", "mm", None))
        .await
        .unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with("note: uncertain: "))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        line.starts_with(
            "note: uncertain: anonymous python at meta-mine:recipes-app/mm/mm_1.0.bb:"
        ) && line.ends_with(" may write PACKAGE_ARCH"),
        "{line}"
    );
    let json = var::run(&ctx, vargs("WORKDIR_X", "mm", Some(Format::Json)))
        .await
        .unwrap();
    assert!(
        json.contains("note: uncertain: anonymous python at "),
        "{json}"
    );
    // The variable itself: this function also hands `d` to other code (opaque), so the
    // replay's warning does not name PACKAGE_ARCH; the cause line does.
    let own = var::run(&ctx, vargs("PACKAGE_ARCH", "mm", None))
        .await
        .unwrap();
    assert!(
        own.contains("note: uncertain: anonymous python at "),
        "{own}"
    );
    assert!(
        own.contains("changes variables through computed names"),
        "{own}"
    );
    // A function that names it and is not opaque has its own warning: no duplicate line.
    let extra = "TOUCHED = \"t\"\npython () {\n    d.setVar('TOUCHED', 'u')\n}";
    let fx = with_recipe(
        "mm",
        &mm_recipe("armv7", "http://x/a.tar.gz", extra, MULTIMACHINE),
        |_| {},
    );
    let (_c, ctx) = make_ctx(&fx);
    let own = var::run(&ctx, vargs("TOUCHED", "mm", None)).await.unwrap();
    assert!(own.contains("changes TOUCHED"), "{own}");
    assert!(!own.contains("note: uncertain"), "{own}");
    // Proved not to happen: said so.
    let fx = with_recipe(
        "mm",
        &mm_recipe("qemux86", "file://a.patch", "", MULTIMACHINE),
        |_| {},
    );
    let (_c, ctx) = make_ctx(&fx);
    let text = var::run(&ctx, vargs("PACKAGE_ARCH", "mm", None))
        .await
        .unwrap();
    assert!(
        text.contains("does not write PACKAGE_ARCH here: PACKAGE_ARCH already equals MACHINE_ARCH"),
        "{text}"
    );
    assert!(!text.contains("note: uncertain"), "{text}");
}
