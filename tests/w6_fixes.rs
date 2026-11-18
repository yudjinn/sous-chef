//! Fixes for the defects left after the checks against a real Yocto workspace (wave 6 fix
//! round): each test runs on a temporary copy of the fixture workspace; the fixture itself
//! is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::config::Config;
use sous_chef::eval::{self, ConfigCache, DataStore, OpKind, OpStatus, Target};
use sous_chef::index::Index;
use sous_chef::index::query;
use sous_chef::lint::{self, Finding};
use sous_chef::workspace::Workspace;

struct Fx {
    cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let config = ws_config(&root);
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        cache,
        _tmp: tmp,
        root,
        config,
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

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

/// Lint with the given rules (ids), whole workspace with `--all`.
fn lint_all(fx: &Fx, rules: &[&str]) -> Vec<Finding> {
    let opts = lint::Options {
        rules: rules.iter().map(|r| r.to_string()).collect(),
        min_severity: None,
        all: true,
    };
    lint::run(&fx.ws, &fx.idx, &fx.config, &lint::Target::Workspace, &opts)
        .unwrap()
        .findings
}

fn ctx_for(fx: &Fx) -> sous_chef::mcp::Ctx {
    let config = fx.config.clone();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(fx.cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles =
        sous_chef::handles::HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    sous_chef::mcp::Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: sous_chef::bitbake::JobTable::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Item 1: the virtual-provider rewrite of DEPENDS is a rewrite, not an assignment
// ---------------------------------------------------------------------------------------

fn virtual_providers(root: &Path) {
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "\nBB_RECIPE_VIRTUAL_PROVIDERS = \"virtual/cross-cc\"\n\
         PREFERRED_PROVIDER_virtual/cross-cc = \"gcc-cross-${TARGET_ARCH}\"\n",
    );
    replace_in(
        root,
        "meta-mine/recipes-app/app/app_0.1.bb",
        "DEPENDS = \"libfoo\"",
        "DEPENDS = \"libfoo virtual/cross-cc\"",
    );
}

#[test]
fn virtual_provider_rewrite_keeps_the_recipe_assignment_applied() {
    let fx = modified(virtual_providers);
    let ds = eval(&fx, "app");
    assert_eq!(
        ds.value("DEPENDS").unwrap().text,
        "libfoo gcc-cross-i686",
        "the rewrite still happens"
    );
    let history = ds.history("DEPENDS");
    let own = history
        .iter()
        .find(|(op, _)| op.loc.path == Path::new("recipes-app/app/app_0.1.bb"))
        .unwrap();
    assert_eq!(own.1, OpStatus::Applied, "{history:?}");
    assert_eq!(own.0.kind, OpKind::Statement);
    let (last, st) = history.last().unwrap();
    assert_eq!(last.kind, OpKind::VirtualProviderRewrite);
    assert_eq!(*st, OpStatus::Applied);
    assert_eq!(last.loc.path, Path::new("conf/distro/testdistro.conf"));
}

#[test]
fn override_shadowed_ignores_the_virtual_provider_rewrite() {
    let fx = modified(virtual_providers);
    let found = lint_all(&fx, &["override/shadowed"]);
    assert!(
        !found.iter().any(|f| f.loc.path.ends_with("app_0.1.bb")),
        "{found:#?}"
    );
    // A real later `=` is still reported.
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "DEPENDS = \"libfoo virtual/cross-cc zlib\"\n",
        );
    });
    let found = lint_all(&fx, &["override/shadowed"]);
    assert!(
        found
            .iter()
            .any(|f| f.loc.path.ends_with("app_0.1.bb") && f.loc.lines.start == 4),
        "{found:#?}"
    );
    assert!(
        !found.iter().any(|f| f.message.contains("testdistro.conf")),
        "{found:#?}"
    );
}

#[tokio::test]
async fn var_shows_the_rewrite_as_a_rewrite() {
    use sous_chef::mcp::tools::var;
    let fx = modified(virtual_providers);
    let ctx = ctx_for(&fx);
    let out = var::run(
        &ctx,
        var::Args {
            name: "DEPENDS".into(),
            recipe: Some("app".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(8000),
            format: None,
        },
    )
    .await
    .unwrap();
    let own = out
        .lines()
        .find(|l| l.contains("meta-mine:recipes-app/app/app_0.1.bb:4"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(own.starts_with("applied\t=\t"), "{out}");
    let rewrite = out
        .lines()
        .find(|l| l.contains("testdistro.conf"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(rewrite.starts_with("applied\trewrite\t"), "{out}");
    assert!(rewrite.contains("rewritten after parsing"), "{out}");
    let _ = &fx.root;
}

// ---------------------------------------------------------------------------------------
// Item 2: files BitBake does not collect are told about once and otherwise left alone
// ---------------------------------------------------------------------------------------

/// A `.bb` with a legacy override (an `override/legacy-syntax` error) and a tab.
const BAD_BB: &str = "LICENSE = \"CLOSED\"\nRDEPENDS_${PN} = \"x\"\nFOO = \"\t1\"\n";

fn uncollected_files(root: &Path) {
    write(root, "meta-mine/lib/templates/example.bb", BAD_BB);
    write(root, "meta-mine/recipes-app/deep/sub/deep_1.0.bb", BAD_BB);
    write(root, "meta-mine/stray/x_1.0.bbappend", BAD_BB);
}

#[test]
fn uncollected_recipe_files_get_one_finding_and_no_other_rule() {
    let fx = modified(uncollected_files);
    let found = lint_all(&fx, &[]);
    let in_file = |p: &str| -> Vec<&Finding> {
        found
            .iter()
            .filter(|f| f.loc.layer == "meta-mine" && f.loc.path == Path::new(p))
            .collect()
    };
    let t = in_file("lib/templates/example.bb");
    assert_eq!(t.len(), 1, "{t:#?}");
    assert_eq!(t[0].rule, "org/recipe-path");
    assert_eq!(t[0].severity, lint::Severity::Info);
    assert!(
        t[0].message.contains("BitBake does not collect")
            && t[0].message.contains("${LAYERDIR}/recipes-*/*/*.bb"),
        "{}",
        t[0].message
    );
    let d = in_file("recipes-app/deep/sub/deep_1.0.bb");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].rule, "org/recipe-path");
    assert_eq!(d[0].severity, lint::Severity::Warning);
    // The bbappend: no file finding; org/layer-conf reports it once, at BBFILES.
    assert!(in_file("stray/x_1.0.bbappend").is_empty(), "{found:#?}");
    let conf: Vec<&Finding> = found
        .iter()
        .filter(|f| f.rule == "org/layer-conf" && f.message.contains("stray/x_1.0.bbappend"))
        .collect();
    assert_eq!(conf.len(), 1, "{found:#?}");
    assert!(
        !found
            .iter()
            .any(|f| f.rule == "org/recipe-path" && f.message.contains("x_1.0.bbappend")),
        "{found:#?}"
    );
    let _ = query::all_files(&fx.idx).unwrap();
}

#[test]
fn a_collected_recipe_keeps_every_check() {
    let fx = modified(|r| write(r, "meta-mine/recipes-app/bad/bad_1.0.bb", BAD_BB));
    let found = lint_all(&fx, &[]);
    let rules: Vec<&str> = found
        .iter()
        .filter(|f| f.loc.path == Path::new("recipes-app/bad/bad_1.0.bb"))
        .map(|f| f.rule)
        .collect();
    assert!(rules.contains(&"override/legacy-syntax"), "{rules:?}");
}

// ---------------------------------------------------------------------------------------
// Item 3: BBFILES_DYNAMIC in the index
// ---------------------------------------------------------------------------------------

fn dynamic_layers(root: &Path) {
    append_to(
        root,
        "meta-mine/conf/layer.conf",
        "BBFILES_DYNAMIC += \"core:${LAYERDIR}/dynamic-layers/core/*/*/*.bbappend \\\n    \
         tpm2:${LAYERDIR}/dynamic-layers/tpm2/*/*/*.bbappend \\\n    \
         !tpm2:${LAYERDIR}/dynamic-layers/no-tpm2/*/*/*.bbappend \\\n    \
         !core:${LAYERDIR}/dynamic-layers/no-core/*/*/*.bbappend\"\n",
    );
    for (dir, var) in [
        ("core", "DYN_CORE"),
        ("tpm2", "DYN_TPM2"),
        ("no-tpm2", "DYN_NO_TPM2"),
        ("no-core", "DYN_NO_CORE"),
    ] {
        write(
            root,
            &format!("meta-mine/dynamic-layers/{dir}/recipes-core/hello/hello_%.bbappend"),
            &format!("{var} = \"1\"\nRDEPENDS_${{PN}} = \"x\"\n"),
        );
    }
}

#[test]
fn bbfiles_dynamic_is_folded_into_the_collection() {
    let fx = modified(dynamic_layers);
    let ds = eval(&fx, "hello");
    // Present collection, and `!` of an absent one: collected, applied to the recipe.
    assert_eq!(ds.value("DYN_CORE").map(|v| v.text).as_deref(), Some("1"));
    assert_eq!(
        ds.value("DYN_NO_TPM2").map(|v| v.text).as_deref(),
        Some("1")
    );
    // Absent collection, and `!` of a present one: not collected.
    assert!(ds.value("DYN_TPM2").is_none());
    assert!(ds.value("DYN_NO_CORE").is_none());

    let hello = query::find_recipes(&fx.idx, "hello")
        .unwrap()
        .into_iter()
        .find(|r| r.file.layer == "meta-core")
        .unwrap();
    let appends: Vec<String> = query::appends_for(&fx.idx, hello.file.id)
        .unwrap()
        .iter()
        .map(|f| f.path.display().to_string())
        .collect();
    assert!(
        appends.contains(&"dynamic-layers/core/recipes-core/hello/hello_%.bbappend".to_string())
            && appends
                .contains(&"dynamic-layers/no-tpm2/recipes-core/hello/hello_%.bbappend".into()),
        "{appends:?}"
    );
    for dir in ["tpm2", "no-core"] {
        let f = query::file_by_path(
            &fx.idx,
            "meta-mine",
            Path::new(&format!(
                "dynamic-layers/{dir}/recipes-core/hello/hello_%.bbappend"
            )),
        )
        .unwrap()
        .unwrap();
        let why = query::not_collected(&fx.idx, f.id).unwrap().unwrap();
        assert!(why.contains("BBFILES_DYNAMIC"), "{dir}: {why}");
        assert!(!appends.iter().any(|a| a.contains(&format!("/{dir}/"))));
    }

    // Lint: the collected ones are linted (a legacy override each), the others are not.
    let found = lint_all(&fx, &["override/legacy-syntax"]);
    let dirs: Vec<String> = found
        .iter()
        .filter(|f| f.loc.path.starts_with("dynamic-layers"))
        .map(|f| f.loc.path.display().to_string())
        .collect();
    assert_eq!(
        dirs,
        [
            "dynamic-layers/core/recipes-core/hello/hello_%.bbappend",
            "dynamic-layers/no-tpm2/recipes-core/hello/hello_%.bbappend"
        ]
    );
}

#[tokio::test]
async fn layers_appends_reads_bbfiles_dynamic_from_the_index() {
    use sous_chef::mcp::tools::layers;
    let fx = modified(dynamic_layers);
    let ctx = ctx_for(&fx);
    let out = layers::run(
        &ctx,
        layers::Args {
            show: layers::Show::Appends,
            budget_tokens: Some(8000),
            format: None,
        },
    )
    .await
    .unwrap();
    for want in [
        "meta-mine:dynamic-layers/core/recipes-core/hello/hello_%.bbappend\t10\tmeta-core:recipes-core/hello/hello_1.0.bb\tapplies (BBFILES_DYNAMIC)",
        "meta-mine:dynamic-layers/no-tpm2/recipes-core/hello/hello_%.bbappend\t10\tmeta-core:recipes-core/hello/hello_1.0.bb\tapplies (BBFILES_DYNAMIC)",
        "meta-mine:dynamic-layers/tpm2/recipes-core/hello/hello_%.bbappend\t10\t-\tnot collected (dynamic layer `tpm2` not present)",
        "meta-mine:dynamic-layers/no-core/recipes-core/hello/hello_%.bbappend\t10\t-\tnot collected (BBFILES_DYNAMIC `!core`: layer `core` is present)",
    ] {
        assert!(out.contains(want), "{want:?} in\n{out}");
    }
}

// ---------------------------------------------------------------------------------------
// Item 4: data files under conf/
// ---------------------------------------------------------------------------------------

#[test]
fn an_unreferenced_unparsable_conf_under_conf_is_data() {
    const CCACHE: &str =
        "# ccache's own configuration\nsloppiness = include_file_ctime\nhash_dir = false\n";
    let fx = modified(|r| {
        write(r, "meta-mine/conf/ccache.conf", CCACHE);
        write(r, "meta-mine/conf/extra.conf", "EXTRA_SETTING = \"1\"\n");
        write(r, "meta-mine/conf/included.conf", CCACHE);
        write(r, "meta-mine/conf/machine/odd.conf", CCACHE);
        append_to(
            r,
            "meta-mine/conf/layer.conf",
            "include conf/included.conf\n",
        );
    });
    let reason = |p: &str| {
        let f = query::file_by_path(&fx.idx, "meta-mine", Path::new(p))
            .unwrap()
            .unwrap();
        query::not_collected(&fx.idx, f.id).unwrap()
    };
    assert!(
        reason("conf/ccache.conf").is_some_and(|r| r.contains("data file")),
        "{:?}",
        reason("conf/ccache.conf")
    );
    // Parses cleanly: may be included in a way the index cannot see.
    assert_eq!(reason("conf/extra.conf"), None);
    // Included: metadata, its errors are real.
    assert_eq!(reason("conf/included.conf"), None);
    // A machine conf is read by convention.
    assert_eq!(reason("conf/machine/odd.conf"), None);

    let found = lint_all(&fx, &["hygiene/parse-error"]);
    let files: std::collections::BTreeSet<String> = found
        .iter()
        .filter(|f| f.loc.layer == "meta-mine")
        .map(|f| f.loc.path.display().to_string())
        .collect();
    assert!(!files.contains("conf/ccache.conf"), "{found:#?}");
    assert!(files.contains("conf/included.conf"), "{found:#?}");
    assert!(files.contains("conf/machine/odd.conf"), "{found:#?}");
}

// ---------------------------------------------------------------------------------------
// Item 5: one tokeniser for values (parse::words)
// ---------------------------------------------------------------------------------------

#[test]
fn override_dead_does_not_take_python_code_for_override_names() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/machine/qemux86.conf",
            "MACHINEOVERRIDES =. \"${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}\"\n",
        );
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "EXTRA_X:armv7a = \"1\"\nEXTRA_X:utils = \"1\"\n",
        );
    });
    let found = lint_all(&fx, &["override/dead"]);
    let msgs: Vec<&str> = found
        .iter()
        .filter(|f| f.loc.path.ends_with("app_0.1.bb"))
        .map(|f| f.message.as_str())
        .collect();
    // `armv7a` is a value the python may add: known. `utils` is python code: not a name.
    assert!(!msgs.iter().any(|m| m.contains("`armv7a`")), "{msgs:#?}");
    assert!(msgs.iter().any(|m| m.contains("`utils`")), "{msgs:#?}");
}

/// Values with a `${@...}` expression (spaces, quotes, `file://` and `:` inside) in every
/// place a rule splits one: no finding may be about a fragment of the expression.
#[test]
fn rules_do_not_split_python_expressions() {
    const PY: &str = "${@' file://fix.patch' if oe.utils.read_file('x') else 'a:b'}";
    let fx = modified(|r| {
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            &format!("SRC_URI += \"{PY}\"\ninherit {PY}\nSRC_URI[sha256sum] = \"0\"\n"),
        );
        append_to(
            r,
            "meta-mine/recipes-core/hello/hello_1.1.bbappend",
            &format!("SRC_URI += \"{PY}\"\n"),
        );
        append_to(
            r,
            "meta-mine/conf/layer.conf",
            &format!("INHERIT += \"{PY}\"\nBBCLASSEXTEND += \"{PY}\"\n"),
        );
        append_to(
            r,
            "meta-core/conf/machine/qemux86.conf",
            &format!("MACHINEOVERRIDES =. \"{PY}:\"\n"),
        );
    });
    let found = lint_all(&fx, &[]);
    for f in &found {
        assert!(
            !f.message.contains("fix.patch'")
                && !f.message.contains("'a")
                && !f.message.contains("b'}")
                && !f.message.contains("file://fix.patch"),
            "{f:#?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Item 7: provider gaps, said in the output
// ---------------------------------------------------------------------------------------

fn class_extend(root: &Path) {
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "BBCLASSEXTEND = \"native nativesdk multilib:lib32\"\n",
    );
}

/// Since w10 a variant is evaluated (tests/eval_w10.rs); here, the names: every tool that
/// resolves a recipe name takes the variant as such, and lint's target resolution, which
/// works on files, names the base recipe.
#[tokio::test]
async fn class_extended_variants_are_named_as_such() {
    use sous_chef::mcp::tools::{deps, item, recipe, var};
    let fx = modified(class_extend);
    let ctx = ctx_for(&fx);
    let ok = |r: sous_chef::error::Result<String>| match r {
        Ok(out) => out,
        Err(e) => panic!("expected output, got {e}"),
    };
    let rec = |name: &str| recipe::Args {
        name: name.into(),
        version: None,
        show: recipe::Show::Summary,
        budget_tokens: Some(4000),
        format: None,
    };
    let out = ok(recipe::run(&ctx, rec("libfoo-native")).await);
    assert!(
        out.contains("the `native` variant of libfoo, BBCLASSEXTEND `native`"),
        "{out}"
    );
    let out = ok(recipe::run(&ctx, rec("nativesdk-libfoo")).await);
    assert!(out.contains("the `nativesdk` variant of libfoo"), "{out}");
    // No nativesdk class here renames PN as OE-core's handler does: said.
    assert!(
        out.contains("the evaluated PN is \"libfoo-nativesdk\""),
        "{out}"
    );
    let out = ok(recipe::run(&ctx, rec("lib32-libfoo")).await);
    assert!(
        out.contains("the `lib32` multilib variant of libfoo"),
        "{out}"
    );
    // A recipe whose BBCLASSEXTEND does not list the class: no claim.
    let e = recipe::run(&ctx, rec("hello-native"))
        .await
        .unwrap_err()
        .to_string();
    assert!(!e.contains("variant"), "{e}");

    let out = ok(var::run(
        &ctx,
        var::Args {
            name: "PN".into(),
            recipe: Some("libfoo-native".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await);
    assert!(out.contains("libfoo-native"), "{out}");
    let out = ok(deps::run(
        &ctx,
        deps::Args {
            recipe: "nativesdk-libfoo".into(),
            kind: deps::Kind::default(),
            depth: None,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await);
    assert!(out.contains("`nativesdk` variant of libfoo"), "{out}");
    // item looks in the variant's files, which are the base recipe's.
    let out = ok(item::run(
        &ctx,
        item::Args {
            loc: None,
            recipe: Some("libfoo-native".into()),
            file: None,
            name: Some("PROVIDES".into()),
            context: 0,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await);
    assert!(out.contains("libfoo_2.0.bb"), "{out}");
    // Lint's target resolution names the base recipe.
    let e = lint::resolve_target(&fx.ws, &fx.idx, Some("libfoo-native"), &fx.root)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("`native` variant of `libfoo`") && e.contains("give `libfoo`"),
        "{e}"
    );
}

#[test]
fn a_static_bbmask_marks_files_not_collected() {
    let fx = modified(|r| {
        append_to(
            r,
            "build/conf/local.conf",
            "BBMASK = \"meta-mine/recipes-app/app/ |meta-mine/recipes-core/hello/hello_1\\.1\\.bbappend\"\n",
        );
    });
    assert!(query::find_recipes(&fx.idx, "app").unwrap().is_empty());
    let app = query::file_by_path(
        &fx.idx,
        "meta-mine",
        Path::new("recipes-app/app/app_0.1.bb"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        query::not_collected(&fx.idx, app.id).unwrap().as_deref(),
        Some("masked by BBMASK")
    );
    let hello = query::find_recipes(&fx.idx, "hello")
        .unwrap()
        .into_iter()
        .find(|r| r.file.layer == "meta-core")
        .unwrap();
    let appends: Vec<String> = query::appends_for(&fx.idx, hello.file.id)
        .unwrap()
        .iter()
        .map(|f| f.loc(sous_chef::types::LineSpan::line(1)).to_string())
        .collect();
    assert!(
        !appends
            .iter()
            .any(|a| a.contains("meta-mine:recipes-core/hello/hello_1.1.bbappend")),
        "{appends:?}"
    );
    // Lint leaves the masked files alone.
    let found = lint_all(&fx, &["override/legacy-syntax", "org/recipe-path"]);
    assert!(
        !found.iter().any(|f| f.loc.path.ends_with("app_0.1.bb")),
        "{found:#?}"
    );
}

#[test]
fn a_bbmask_with_python_is_said_to_be_unapplied() {
    let fx = modified(|r| {
        append_to(
            r,
            "build/conf/local.conf",
            "BBMASK = \"${@'meta-mine/recipes-app/' if oe.utils.read_file('x') else ''}\"\n",
        );
    });
    // Not applied: the recipe is still found.
    assert!(!query::find_recipes(&fx.idx, "app").unwrap().is_empty());
    let ds = eval(&fx, "app");
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("BBMASK is set but sous-chef does not apply it")),
        "{:?}",
        ds.replay().target_notes
    );
}

fn skip_notes(fx: &Fx, recipe: &str) -> Vec<String> {
    eval(fx, recipe)
        .replay()
        .target_notes
        .iter()
        .filter(|n| n.contains("skip this recipe"))
        .cloned()
        .collect()
}

#[test]
fn skipped_recipes_are_noted() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "COMPATIBLE_MACHINE = \"boardx|qemuarm.*\"\n",
        );
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "SKIP_RECIPE[libfoo] = \"not for this distro\"\nHOST_SYS = \"i686-linux\"\n",
        );
        append_to(
            r,
            "meta-core/recipes-core/hello/hello_1.1.bb",
            "COMPATIBLE_HOST = \"(x86_64|i.86).*-linux\"\nCOMPATIBLE_MACHINE = \"qemux8\"\n",
        );
    });
    let app = skip_notes(&fx, "app");
    assert_eq!(app.len(), 1, "{app:?}");
    assert!(
        app[0].contains("incompatible with machine qemux86")
            && app[0].contains("COMPATIBLE_MACHINE = \"boardx|qemuarm.*\""),
        "{app:?}"
    );
    let libfoo = skip_notes(&fx, "libfoo");
    assert!(
        libfoo
            .iter()
            .any(|n| n.contains("SKIP_RECIPE[libfoo] = \"not for this distro\"")),
        "{libfoo:?}"
    );
    // re.match is anchored at the start only: `qemux8` matches `qemux86`; the host matches.
    assert!(skip_notes(&fx, "hello").is_empty());
    // Another machine: compatible.
    let t = Target {
        recipe: Some("app".into()),
        machine: Some("boardx".into()),
        ..Target::default()
    };
    let ds = eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap();
    assert!(
        !ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("would skip")),
        "{:?}",
        ds.replay().target_notes
    );
}

#[test]
fn an_undecidable_skip_check_says_bitbake_may_skip() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "COMPATIBLE_MACHINE = \"${@'boardx' if oe.utils.read_file('x') else 'qemux86'}\"\nCOMPATIBLE_HOST = \"arm.*\"\n",
        );
    });
    // Python in COMPATIBLE_MACHINE; HOST_SYS is not set in the fixture. (w8: said, not
    // silent; it was silent before.)
    let notes = skip_notes(&fx, "app");
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(
        notes[0].contains("BitBake may skip this recipe: COMPATIBLE_MACHINE is set but not known statically (it holds python)"),
        "{notes:?}"
    );
    assert!(
        notes[1].contains(
            "BitBake may skip this recipe: COMPATIBLE_HOST = \"arm.*\"; HOST_SYS is not set"
        ),
        "{notes:?}"
    );
}

#[tokio::test]
async fn recipe_summary_shows_the_skip_note() {
    use sous_chef::mcp::tools::recipe;
    let fx = modified(|r| {
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "COMPATIBLE_MACHINE = \"boardx\"\n",
        );
    });
    let ctx = ctx_for(&fx);
    let out = recipe::run(
        &ctx,
        recipe::Args {
            name: "app".into(),
            version: None,
            show: recipe::Show::Summary,
            budget_tokens: Some(8000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("BitBake would skip this recipe"), "{out}");
}
