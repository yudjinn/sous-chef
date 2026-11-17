//! T1.4: path resolution (`workspace::resolve`) over `tests/fixtures/ws`, plus the
//! `bblayers.conf` / `layer.conf` handling that moved to `parse()`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::parse::IncludeMode;
use sous_chef::workspace::resolve::{self, RefKind, Resolver, Target};
use sous_chef::workspace::{Workspace, layers};

fn root(ws: &Workspace, layer: &str) -> PathBuf {
    ws.layer(layer).unwrap().root.clone()
}

#[test]
fn bbpath_is_build_dir_then_layers() {
    let ws = fixture_ws();
    assert_eq!(
        resolve::bbpath(&ws),
        vec![
            ws.build_dir.clone().unwrap(),
            root(&ws, "meta-core"),
            root(&ws, "meta-vendor"),
            root(&ws, "meta-mine"),
        ]
    );
}

#[test]
fn include_found_next_to_the_file() {
    let ws = fixture_ws();
    let dir = root(&ws, "meta-core").join("recipes-core/hello");
    let from = dir.join("hello_1.0.bb");
    assert_eq!(
        resolve::find_in_bbpath(&ws, "hello.inc", &from),
        Some(dir.join("hello.inc"))
    );
}

#[test]
fn include_found_via_bbpath() {
    let ws = fixture_ws();
    let from = root(&ws, "meta-core").join("conf/bitbake.conf");
    let build = ws.build_dir.clone().unwrap();
    // bitbake.conf line 50: `include conf/local.conf` is in the build directory, the first
    // BBPATH entry.
    assert_eq!(
        resolve::find_in_bbpath(&ws, "conf/local.conf", &from),
        Some(build.join("conf/local.conf"))
    );
    assert_eq!(
        resolve::find_in_bbpath(&ws, "conf/machine/qemux86.conf", &from),
        Some(root(&ws, "meta-core").join("conf/machine/qemux86.conf"))
    );
    // From a recipe in another layer, the BBPATH still finds it.
    let other = root(&ws, "meta-mine").join("recipes-app/app/app_0.1.bb");
    assert_eq!(
        resolve::find_in_bbpath(&ws, "conf/distro/testdistro.conf", &other),
        Some(root(&ws, "meta-core").join("conf/distro/testdistro.conf"))
    );
    // Absolute paths are used as is.
    let abs = root(&ws, "meta-core").join("recipes-core/hello/hello.inc");
    assert_eq!(
        resolve::find_in_bbpath(&ws, abs.to_str().unwrap(), &other),
        Some(abs)
    );
}

#[test]
fn include_not_found() {
    let ws = fixture_ws();
    let from = root(&ws, "meta-core").join("conf/bitbake.conf");
    assert_eq!(resolve::find_in_bbpath(&ws, "conf/site.conf", &from), None);
    assert_eq!(resolve::find_in_bbpath(&ws, "conf/auto.conf", &from), None);
    // A file outside the workspace is never found.
    assert_eq!(resolve::find_in_bbpath(&ws, "/etc/passwd", &from), None);
    assert_eq!(
        resolve::find_in_bbpath(&ws, "../../../../../../../../etc/passwd", &from),
        None
    );
}

#[test]
fn directory_next_to_the_file_comes_before_bbpath() {
    let (_tmp, ws_root) = temp_ws();
    let ws = Workspace::discover(&ws_config(&ws_root)).unwrap();
    let mine = root(&ws, "meta-mine");
    fs::create_dir_all(mine.join("recipes-app/app/conf")).unwrap();
    fs::write(mine.join("recipes-app/app/conf/local.conf"), "X = \"1\"\n").unwrap();
    let from = mine.join("recipes-app/app/app_0.1.bb");
    assert_eq!(
        resolve::find_in_bbpath(&ws, "conf/local.conf", &from),
        Some(mine.join("recipes-app/app/conf/local.conf"))
    );
}

#[test]
fn class_in_each_directory() {
    let ws = fixture_ws();
    let core = root(&ws, "meta-core");
    // classes-recipe/ for a recipe inherit.
    assert_eq!(
        resolve::find_class(&ws, "autotoolsish", false),
        Some(core.join("classes-recipe/autotoolsish.bbclass"))
    );
    // classes-global/ for INHERIT; a recipe inherit does not search it.
    assert_eq!(
        resolve::find_class(&ws, "sanityish", true),
        Some(core.join("classes-global/sanityish.bbclass"))
    );
    assert_eq!(resolve::find_class(&ws, "sanityish", false), None);
    // classes/ is the fallback for both.
    assert_eq!(
        resolve::find_class(&ws, "base", true),
        Some(core.join("classes/base.bbclass"))
    );
    assert_eq!(
        resolve::find_class(&ws, "base", false),
        Some(core.join("classes/base.bbclass"))
    );
    // meta-legacy is not in BBLAYERS.
    assert_eq!(resolve::find_class(&ws, "oldclass", false), None);
    // A name ending in .bbclass is a path on BBPATH.
    assert_eq!(
        resolve::find_class(&ws, "classes/base.bbclass", false),
        Some(core.join("classes/base.bbclass"))
    );
    assert_eq!(
        resolve::class_candidates(&ws, "autotoolsish", false),
        vec![core.join("classes-recipe/autotoolsish.bbclass")]
    );
}

#[test]
fn class_shadowed_across_layers() {
    let (_tmp, ws_root) = temp_ws();
    let ws = Workspace::discover(&ws_config(&ws_root)).unwrap();
    let core = root(&ws, "meta-core");
    let mine = root(&ws, "meta-mine");
    let vendor = root(&ws, "meta-vendor");
    // Same directory in a later layer: shadowed by meta-core (earlier on BBPATH).
    fs::create_dir_all(mine.join("classes-recipe")).unwrap();
    fs::write(
        mine.join("classes-recipe/autotoolsish.bbclass"),
        "X = \"mine\"\n",
    )
    .unwrap();
    // classes-recipe/ anywhere on BBPATH beats classes/ anywhere: base in meta-core's
    // classes/ is shadowed by a later layer's classes-recipe/base.bbclass for a recipe
    // inherit, but not for the global one.
    fs::create_dir_all(vendor.join("classes-recipe")).unwrap();
    fs::write(
        vendor.join("classes-recipe/base.bbclass"),
        "X = \"vendor\"\n",
    )
    .unwrap();

    assert_eq!(
        resolve::class_candidates(&ws, "autotoolsish", false),
        vec![
            core.join("classes-recipe/autotoolsish.bbclass"),
            mine.join("classes-recipe/autotoolsish.bbclass"),
        ]
    );
    assert_eq!(
        resolve::find_class(&ws, "base", false),
        Some(vendor.join("classes-recipe/base.bbclass"))
    );
    assert_eq!(
        resolve::class_candidates(&ws, "base", false),
        vec![
            vendor.join("classes-recipe/base.bbclass"),
            core.join("classes/base.bbclass"),
        ]
    );
    assert_eq!(
        resolve::find_class(&ws, "base", true),
        Some(core.join("classes/base.bbclass"))
    );
}

#[test]
fn recipe_files_expand_bbfiles() {
    let ws = fixture_ws();
    let rel: Vec<String> = resolve::recipe_files(&ws)
        .iter()
        .map(|p| ws.loc_for(p, sous_chef::types::LineSpan::line(1)).unwrap())
        .map(|l| format!("{}:{}", l.layer, l.path.display()))
        .collect();
    // Lowest priority layer first; per layer the .bb glob, then the .bbappend glob.
    assert_eq!(
        rel,
        [
            "meta-core:recipes-core/hello/hello_1.0.bb",
            "meta-core:recipes-core/hello/hello_1.1.bb",
            "meta-core:recipes-core/libfoo/libfoo_2.0.bb",
            "meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb",
            "meta-vendor:recipes-core/hello/hello_%.bbappend",
            "meta-mine:recipes-app/app/app_0.1.bb",
            "meta-mine:recipes-app/orphan/orphan_9.%.bbappend",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend",
        ]
    );
}

#[test]
fn bbappend_matching_on_the_fixture() {
    let ws = fixture_ws();
    let files = resolve::recipe_files(&ws);
    let (recipes, appends): (Vec<&PathBuf>, Vec<&PathBuf>) = files
        .iter()
        .partition(|p| p.extension().is_some_and(|e| e == "bb"));
    let matches = |append: &str| -> Vec<String> {
        let a = appends
            .iter()
            .find(|p| p.ends_with(append))
            .unwrap_or_else(|| panic!("{append} not collected"));
        recipes
            .iter()
            .filter(|r| resolve::append_matches(a, r))
            .map(|r| r.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        matches("recipes-core/hello/hello_%.bbappend"),
        ["hello_1.0.bb", "hello_1.1.bb"]
    );
    assert_eq!(
        matches("recipes-core/hello/hello_1.1.bbappend"),
        ["hello_1.1.bb"]
    );
    assert!(matches("recipes-app/orphan/orphan_9.%.bbappend").is_empty());
}

#[test]
fn resolve_with_static_vars() {
    let ws = fixture_ws();
    let core = root(&ws, "meta-core");
    let from = core.join("recipes-core/hello/hello_1.0.bb");
    let r = Resolver::new(&ws);
    let vars = |n: &str| (n == "BPN").then(|| "hello".to_string());
    let inc = RefKind::Include(IncludeMode::Require);
    assert_eq!(
        r.resolve_with(&ws, inc, "${BPN}.inc", &from, &vars),
        Some(vec![Target {
            text: "hello.inc".into(),
            file: Some(core.join("recipes-core/hello/hello.inc")),
        }])
    );
    // Several words: one target each; unknown variables need the evaluator.
    assert_eq!(
        r.resolve_with(&ws, inc, "hello.inc nope.inc", &from, &vars)
            .unwrap()
            .iter()
            .map(|t| t.file.is_some())
            .collect::<Vec<_>>(),
        [true, false]
    );
    assert_eq!(r.resolve_with(&ws, inc, "${PV}.inc", &from, &vars), None);
    assert_eq!(r.resolve_with(&ws, inc, "${@'x'}.inc", &from, &vars), None);
    let inh = RefKind::Inherit { global: false };
    assert_eq!(
        r.resolve_with(&ws, inh, "autotoolsish nosuch", &from, &vars),
        Some(vec![
            Target {
                text: "autotoolsish".into(),
                file: Some(core.join("classes-recipe/autotoolsish.bbclass")),
            },
            Target {
                text: "nosuch".into(),
                file: None,
            },
        ])
    );
}

#[test]
fn include_all_searches_every_bbpath_entry() {
    let (_tmp, ws_root) = temp_ws();
    let ws = Workspace::discover(&ws_config(&ws_root)).unwrap();
    for l in ["meta-core", "meta-mine"] {
        let d = root(&ws, l).join("conf/extra");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("all.conf"), "X = \"1\"\n").unwrap();
    }
    let r = Resolver::new(&ws);
    assert_eq!(
        r.include_all(&ws, "conf/extra/all.conf"),
        vec![
            root(&ws, "meta-core").join("conf/extra/all.conf"),
            root(&ws, "meta-mine").join("conf/extra/all.conf"),
        ]
    );
}

#[test]
fn known_files_resolver_needs_no_disk() {
    let ws = fixture_ws();
    let core = root(&ws, "meta-core");
    let inc = core.join("recipes-core/hello/hello.inc");
    let r = Resolver::new(&ws).with_known_files([inc.clone()].into_iter().collect());
    let from = core.join("recipes-core/hello/hello_1.0.bb");
    assert_eq!(r.find_in_bbpath(&ws, "hello.inc", &from), Some(inc));
    // Exists on disk, but not in the set.
    let conf = core.join("conf/bitbake.conf");
    assert_eq!(r.find_in_bbpath(&ws, "conf/local.conf", &conf), None);
}

// ---- layer configuration read with parse() ----

fn write(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn layer_conf(c: &str, extra: &str) -> String {
    format!(
        "BBPATH .= \":${{LAYERDIR}}\"\nBBFILE_COLLECTIONS += \"{c}\"\n\
         BBFILE_PRIORITY_{c} = \"5\"\nBBFILES += \"${{LAYERDIR}}/recipes/*.bb\"\n{extra}"
    )
}

#[test]
fn bblayers_append_and_include() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    for c in ["a", "b", "c", "d"] {
        write(
            &base.join(format!("meta-{c}/conf/layer.conf")),
            &layer_conf(c, ""),
        );
    }
    let build = base.join("build");
    write(
        &build.join("conf/bblayers.conf"),
        "BBPATH = \"${TOPDIR}\"\nBBLAYERS ?= \"${TOPDIR}/../meta-a ${TOPDIR}/../meta-d\"\n\
         BBLAYERS:append = \" ${TOPDIR}/../meta-b\"\n\
         BBLAYERS:remove = \"${TOPDIR}/../meta-d\"\n\
         include conf/extra-layers.conf\n\
         include conf/missing.conf\n\
         require ${@'python'}.conf\n",
    );
    write(
        &build.join("conf/extra-layers.conf"),
        "BBLAYERS += \"${TOPDIR}/../meta-c\"\n",
    );
    let bb = layers::read_bblayers(&build).unwrap();
    assert_eq!(
        bb.layers,
        vec![
            PathBuf::from(format!("{}/../meta-a", build.display())),
            PathBuf::from(format!("{}/../meta-c", build.display())),
            PathBuf::from(format!("{}/../meta-b", build.display())),
        ]
    );
    let cfg = Config {
        build_dir: Some(build.clone()),
        work_dir: base.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    let names: Vec<_> = ws.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["meta-a", "meta-c", "meta-b"]);
}

#[test]
fn bblayers_require_missing_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let build = fs::canonicalize(tmp.path()).unwrap().join("build");
    write(
        &build.join("conf/bblayers.conf"),
        "BBLAYERS = \"\"\nrequire conf/missing.conf\n",
    );
    let err = layers::read_bblayers(&build).unwrap_err();
    assert!(err.to_string().contains("missing.conf"), "{err}");
}

#[test]
fn layer_conf_append_include_and_layerdir() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let a = base.join("meta-a");
    let b = base.join("meta-b");
    // meta-a includes a file next to it; the included file refers to ${LAYERDIR}, which is
    // still meta-a while the include is read.
    write(
        &a.join("conf/layer.conf"),
        &layer_conf(
            "a",
            "include layer-extra.inc\nBBFILES:append = \" ${LAYERDIR}/other/*.bb\"\n",
        ),
    );
    write(
        &a.join("conf/layer-extra.inc"),
        "LAYERDEPENDS_a = \"core\"\nBBPATH .= \":${LAYERDIR}/sub\"\n",
    );
    // meta-b sets its priority through an override-qualified name that is not active, and
    // unsets a variable.
    write(
        &b.join("conf/layer.conf"),
        &layer_conf(
            "b",
            "BBFILE_PRIORITY_b:inactive = \"99\"\nunset LAYERDEPENDS_b\n",
        ),
    );
    let build = base.join("build");
    write(
        &build.join("conf/bblayers.conf"),
        "BBPATH = \"${TOPDIR}\"\nBBLAYERS = \"${TOPDIR}/../meta-a ${TOPDIR}/../meta-b\"\n",
    );
    let cfg = Config {
        build_dir: Some(build.clone()),
        work_dir: base.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    let la = ws.layer("meta-a").unwrap();
    assert_eq!(la.depends, ["core"]);
    assert_eq!(
        la.bbfiles,
        [
            format!("{}/recipes/*.bb", a.display()),
            format!("{}/other/*.bb", a.display()),
        ]
    );
    assert_eq!(ws.layer("meta-b").unwrap().priority, 5);
    assert_eq!(
        layers::read_bbpath(Some(&build), &[a.clone(), b.clone()]).unwrap(),
        [
            build.display().to_string(),
            a.display().to_string(),
            format!("{}/sub", a.display()),
            b.display().to_string(),
        ]
    );
}

#[test]
fn layer_conf_include_outside_workspace_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let outside = base.join("outside.inc");
    write(&outside, "BBFILE_COLLECTIONS += \"evil\"\n");
    let a = base.join("meta-a");
    write(
        &a.join("conf/layer.conf"),
        &layer_conf("a", &format!("include {}\n", outside.display())),
    );
    let cfg = Config {
        layers: vec![a.clone()],
        work_dir: base.clone(),
        ..Config::default()
    };
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.layer("meta-a").unwrap().collections, ["a"]);
}
