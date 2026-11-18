//! Evaluator gaps closed in round w8: virtual providers in task `depends` flags and
//! `DEPENDS` normalisation, kas defaults at `local.conf`'s place, honest skip notes
//! (`COMPATIBLE_MACHINE`, `COMPATIBLE_HOST`, `INCOMPATIBLE_LICENSE`), `BBMASK` per machine,
//! and the class context of an `inherit` in an `.inc` included from a `.conf`. Each test
//! runs on a temporary copy of the fixture workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, OpKind, RefProblem, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    #[allow(dead_code)]
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
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

fn target(recipe: &str) -> Target {
    Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    }
}

fn eval(fx: &Fx, t: &Target) -> DataStore {
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, t).unwrap()
}

// ---------------------------------------------------------------------------------------
// Item 2: handleVirtRecipeProviders on task `depends` flags; DEPENDS normalised / ""
// ---------------------------------------------------------------------------------------

fn virtual_providers(root: &Path) {
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "\nBB_RECIPE_VIRTUAL_PROVIDERS = \"virtual/cross-cc virtual/cross-binutils\"\n\
         PREFERRED_PROVIDER_virtual/cross-cc = \"gcc-cross-${TARGET_ARCH}\"\n",
    );
}

#[test]
fn task_depends_flags_are_rewritten_for_tasks_in_bbtasks() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\ndo_report[depends] = \"virtual/cross-cc:do_populate_sysroot  libfoo:do_build\"\n\
             do_notask[depends] = \"virtual/cross-cc:do_populate_sysroot\"\n\
             addtask gone\n\
             do_gone[depends] = \"virtual/cross-cc:do_populate_sysroot\"\n\
             deltask gone\n\
             addtask nobin\n\
             do_nobin[depends] = \"virtual/cross-binutils:do_populate_sysroot\"\n",
        );
    });
    let ds = eval(&fx, &target("app"));
    assert!(
        ds.tasks().iter().any(|t| t == "do_report"),
        "{:?}",
        ds.tasks()
    );
    assert!(
        !ds.tasks().iter().any(|t| t == "do_gone"),
        "{:?}",
        ds.tasks()
    );
    // In __BBTASKS: rewritten, entries joined with single spaces.
    assert_eq!(
        ds.flag("do_report", "depends").unwrap().text,
        "gcc-cross-i686:do_populate_sysroot libfoo:do_build"
    );
    let (op, _) = ds.flag_history("do_report", "depends").pop().unwrap();
    assert_eq!(op.kind, OpKind::VirtualProviderRewrite);
    assert_eq!(op.loc.path, Path::new("conf/distro/testdistro.conf"));
    let notes = &ds.replay().var_notes["do_report"];
    assert!(
        notes[0].contains("do_report[depends] rewritten")
            && notes[0].contains("virtual/cross-cc -> gcc-cross-i686"),
        "{notes:?}"
    );
    // Not a task (no addtask, or removed by deltask): BitBake does not look at it.
    assert_eq!(
        ds.flag("do_notask", "depends").unwrap().text,
        "virtual/cross-cc:do_populate_sysroot"
    );
    assert_eq!(
        ds.flag("do_gone", "depends").unwrap().text,
        "virtual/cross-cc:do_populate_sysroot"
    );
    // No PREFERRED_PROVIDER: BitBake writes the text None (no error for task flags).
    assert_eq!(
        ds.flag("do_nobin", "depends").unwrap().text,
        "None:do_populate_sysroot"
    );
}

#[test]
fn a_task_depends_entry_without_one_colon_is_a_parse_error() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\ndo_report[depends] = \"virtual/cross-cc:do_populate_sysroot bad\"\n",
        );
    });
    let ds = eval(&fx, &target("app"));
    assert!(
        ds.replay().references.iter().any(|r| matches!(
            &r.problem,
            RefProblem::Fatal(m) if m.contains("ValueError") && m.contains("\"bad\"")
        )),
        "{:?}",
        ds.replay().references
    );
    assert_eq!(
        ds.flag("do_report", "depends").unwrap().text,
        "virtual/cross-cc:do_populate_sysroot bad"
    );
}

#[test]
fn depends_is_normalised_and_unset_depends_becomes_empty() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\nDEPENDS += \"  zlib   \"\n",
        );
        write(
            r,
            "meta-mine/recipes-app/nodeps/nodeps_1.0.bb",
            "LICENSE = \"CLOSED\"\n",
        );
    });
    let ds = eval(&fx, &target("app"));
    assert_eq!(ds.value("DEPENDS").unwrap().text, "libfoo zlib");
    let (op, _) = ds.history("DEPENDS").pop().unwrap();
    assert_eq!(op.kind, OpKind::VirtualProviderRewrite);
    assert!(
        ds.replay().var_notes["DEPENDS"][0].contains("joined by single spaces"),
        "{:?}",
        ds.replay().var_notes
    );

    let ds = eval(&fx, &target("nodeps"));
    assert_eq!(ds.value("DEPENDS").unwrap().text, "");
    assert!(
        ds.replay().var_notes["DEPENDS"][0].contains("was unset"),
        "{:?}",
        ds.replay().var_notes
    );

    // Already normal: nothing recorded.
    let fx = modified(virtual_providers);
    let ds = eval(&fx, &target("app"));
    assert_eq!(ds.value("DEPENDS").unwrap().text, "libfoo");
    assert!(!ds.replay().var_notes.contains_key("DEPENDS"));
}

// ---------------------------------------------------------------------------------------
// Item 3: kas defaults where bitbake.conf includes conf/local.conf
// ---------------------------------------------------------------------------------------

/// The kas project before `kas checkout` (no build directory), with a `bitbake.conf` in
/// its own layer holding `text`.
fn kas_eval(bitbake_conf: &str) -> DataStore {
    let p = kas_support::Project::before();
    write(&p.root, "meta-proj/conf/bitbake.conf", bitbake_conf);
    let ws = p.ws();
    assert_eq!(ws.build_dir, None);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &Target::default()).unwrap()
}

#[test]
fn kas_defaults_apply_where_bitbake_conf_includes_local_conf() {
    // kas writes `MACHINE ??= "board1"` into local.conf: a `??=` before the include loses
    // to it, a `??=` after it wins, and nothing before the include sees the kas value.
    let ds = kas_eval(
        "MACHINE ??= \"fromconf\"\n\
         EARLY := \"m-${MACHINE}\"\n\
         DISTRO_EARLY := \"d-${DISTRO}\"\n\
         include conf/local.conf\n\
         LATE := \"m-${MACHINE}\"\n\
         DISTRO ??= \"laterdistro\"\n",
    );
    let text = |n: &str| ds.value(n).map(|v| v.text);
    assert_eq!(text("MACHINE").as_deref(), Some("board1"));
    assert_eq!(text("EARLY").as_deref(), Some("m-fromconf"));
    // `:=` before local.conf: DISTRO was unset there, the reference stays as written.
    assert_eq!(ds.raw("DISTRO_EARLY").as_deref(), Some("d-${DISTRO}"));
    assert_eq!(text("LATE").as_deref(), Some("m-board1"));
    assert_eq!(text("DISTRO").as_deref(), Some("laterdistro"));
    let notes = &ds.replay().target_notes;
    assert!(
        notes
            .iter()
            .any(|n| n.contains("MACHINE = \"board1\" from the kas file kas/board.yaml:5")),
        "{notes:?}"
    );
    // DISTRO's final value is not kas's: no note claims it is.
    assert!(
        !notes.iter().any(|n| n.starts_with("note: DISTRO =")),
        "{notes:?}"
    );

    // A `?=` in the configuration before local.conf beats kas's `??=`.
    let ds = kas_eval("MACHINE ?= \"strong\"\ninclude conf/local.conf\n");
    assert_eq!(ds.value("MACHINE").unwrap().text, "strong");

    // A bitbake.conf that never includes local.conf: kas's local.conf is not read.
    let ds = kas_eval("X = \"1\"\n");
    assert_eq!(ds.value("MACHINE"), None);
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("no MACHINE is set")),
        "{:?}",
        ds.replay().target_notes
    );
}

// ---------------------------------------------------------------------------------------
// Item 4: skip notes that can fire: COMPATIBLE_MACHINE against partly known
// MACHINEOVERRIDES, INCOMPATIBLE_LICENSE
// ---------------------------------------------------------------------------------------

fn skip_notes(fx: &Fx, recipe: &str) -> Vec<String> {
    eval(fx, &target(recipe))
        .replay()
        .target_notes
        .iter()
        .filter(|n| n.contains("skip this recipe") || n.contains("stop parsing"))
        .cloned()
        .collect()
}

fn recipe(root: &Path, pn: &str, body: &str) {
    write(
        root,
        &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
        body,
    );
}

#[test]
fn compatible_machine_against_partly_known_machine_overrides() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "MACHINEOVERRIDES = \"${@'soc-x' if oe.utils.read_file('y') else ''}:${MACHINE}\"\n",
        );
        recipe(
            r,
            "known",
            "LICENSE = \"CLOSED\"\nCOMPATIBLE_MACHINE = \"qemux86\"\n",
        );
        recipe(
            r,
            "unknown",
            "LICENSE = \"CLOSED\"\nCOMPATIBLE_MACHINE = \"boardx|soc-.*\"\n",
        );
    });
    // A match on a known entry is a match.
    assert!(skip_notes(&fx, "known").is_empty());
    // No known entry matches, and one part is python: BitBake may skip it.
    let n = skip_notes(&fx, "unknown");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].contains(
            "BitBake may skip this recipe: COMPATIBLE_MACHINE = \"boardx|soc-.*\" matches none of the known machine overrides (qemux86); 1 part of MACHINEOVERRIDES is not evaluated"
        ),
        "{n:?}"
    );
}

#[test]
fn incompatible_license_three_ways() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "INCOMPATIBLE_LICENSE = \"GPL-3.0*\"\n\
             INCOMPATIBLE_LICENSE_EXCEPTIONS = \"excepted:GPL-3.0-only excepted-dbg:GPL-3.0-only excepted-dev:GPL-3.0-only excepted-doc:GPL-3.0-only\"\n\
             SPDXLICENSEMAP[GPLv3] = \"GPL-3.0-only\"\n",
        );
        recipe(r, "gpl3", "LICENSE = \"GPL-3.0-only & MIT\"\n");
        recipe(r, "oldname", "LICENSE = \"GPLv3\"\n");
        recipe(r, "either", "LICENSE = \"GPL-3.0-only | MIT\"\n");
        recipe(r, "bothways", "LICENSE = \"MIT | GPL-3.0-or-later\"\n");
        recipe(
            r,
            "pkgonly",
            "LICENSE = \"GPL-3.0-only\"\nLICENSE:${PN} = \"MIT\"\n",
        );
        recipe(r, "excepted", "LICENSE = \"GPL-3.0-only\"\n");
        recipe(
            r,
            "pyl",
            "LICENSE = \"${@oe.utils.read_file('license')}\"\n",
        );
        recipe(r, "bad", "LICENSE = \"GPL-3.0-only, MIT\"\n");
        recipe(r, "empty", "LICENSE = \"GPL-3.0-only\"\nPACKAGES = \"\"\n");
    });
    // Every package has an incompatible license: would skip, the recipe's own and the
    // packages' licenses listed once.
    let n = skip_notes(&fx, "gpl3");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].contains(
            "BitBake would skip this recipe: it has incompatible license(s): GPL-3.0-only (INCOMPATIBLE_LICENSE = \"GPL-3.0*\")"
        ),
        "{n:?}"
    );
    // Through SPDXLICENSEMAP.
    let n = skip_notes(&fx, "oldname");
    assert!(
        n.len() == 1 && n[0].contains("incompatible license(s): GPL-3.0-only"),
        "{n:?}"
    );
    // An `|` with a compatible side, either way round; a package with its own compatible
    // LICENSE; exceptions for every package; no packages: not skipped.
    for pn in ["either", "bothways", "pkgonly", "excepted", "empty"] {
        assert!(
            skip_notes(&fx, pn).is_empty(),
            "{pn}: {:?}",
            skip_notes(&fx, pn)
        );
    }
    // Python in LICENSE, an invalid character: may skip.
    let n = skip_notes(&fx, "pyl");
    assert!(
        n.len() == 1
            && n[0].starts_with("note: BitBake may skip this recipe: INCOMPATIBLE_LICENSE = \"GPL-3.0*\": 0 of 4 packages")
            && n[0].contains("LICENSE: it holds python"),
        "{n:?}"
    );
    let n = skip_notes(&fx, "bad");
    assert!(
        n.len() == 1 && n[0].contains("invalid characters in license 'GPL-3.0-only,'"),
        "{n:?}"
    );
}

#[test]
fn incompatible_license_obsolete_name_and_native() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "INCOMPATIBLE_LICENSE = \"GPLv3\"\n",
        );
        recipe(r, "gpl3", "LICENSE = \"GPL-3.0-only\"\n");
        write(
            r,
            "meta-mine/recipes-app/tool/tool-native_1.0.bb",
            "LICENSE = \"GPL-3.0-only\"\n",
        );
    });
    let n = skip_notes(&fx, "gpl3");
    assert!(
        n.len() == 1
            && n[0].contains(
                "BitBake would stop parsing this recipe: Error, GPLv3 is an obsolete license"
            ),
        "{n:?}"
    );
    // -native recipes are not checked.
    assert!(skip_notes(&fx, "tool-native").is_empty());
}

// ---------------------------------------------------------------------------------------
// Item 5: BBMASK per machine
// ---------------------------------------------------------------------------------------

fn mask_notes(fx: &Fx, machine: Option<&str>) -> Vec<String> {
    let t = Target {
        recipe: Some("hello".into()),
        machine: machine.map(str::to_string),
        ..Target::default()
    };
    eval(fx, &t)
        .replay()
        .target_notes
        .iter()
        .filter(|n| n.contains("BBMASK"))
        .cloned()
        .collect()
}

#[test]
fn a_bbmask_that_depends_on_the_machine_is_said() {
    // Masked for boardx only: the index (default machine qemux86) masks nothing.
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "BBMASK:boardx = \"meta-mine/recipes-app/app/\"\n",
        );
    });
    assert!(mask_notes(&fx, None).is_empty());
    assert!(mask_notes(&fx, Some("qemux86")).is_empty());
    let n = mask_notes(&fx, Some("boardx"));
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].contains("BBMASK for this machine and distro differs")
            && n[0].contains("it would also mask 1 file: meta-mine:recipes-app/app/app_0.1.bb"),
        "{n:?}"
    );

    // Masked by default, not for boardx (whose machine conf clears the mask variable; an
    // override variant would make the configuration's BBMASK uncertain, because OVERRIDES
    // holds pn-${PN} there).
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "BOARD_MASK ?= \"meta-mine/recipes-app/app/\"\nBBMASK = \"${BOARD_MASK}\"\n",
        );
        append_to(
            r,
            "meta-core/conf/machine/boardx.conf",
            "\nBOARD_MASK = \"\"\n",
        );
    });
    let n = mask_notes(&fx, Some("qemux86"));
    assert!(n.is_empty(), "{n:?}");
    let n = mask_notes(&fx, Some("boardx"));
    assert!(
        n.len() == 1
            && n[0].contains(
                "it would not mask 1 file the index treats as masked: meta-mine:recipes-app/app/app_0.1.bb"
            ),
        "{n:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Item 6: `inherit` in an `.inc`: the class type of the parse at that point
// ---------------------------------------------------------------------------------------

fn not_found(ds: &DataStore) -> Vec<String> {
    ds.replay()
        .references
        .iter()
        .filter_map(|r| match &r.problem {
            RefProblem::NotFound(w) if r.kind == "inherit" => Some(w.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn inherit_in_an_inc_follows_the_class_type_of_the_parse() {
    let fx = modified(|r| {
        // Included from local.conf: parsed by BBHandler (an .inc) while __bbclasstype is
        // "global" (cookerdata.py), so classes-global/ then classes/.
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "\nrequire conf/distro/include/extra.inc\n",
        );
        write(
            r,
            "meta-core/conf/distro/include/extra.inc",
            "inherit sanityish\ninherit autotoolsish\n",
        );
        // Required from a recipe: "recipe", so classes-recipe/ then classes/.
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\nrequire app-extra.inc\n",
        );
        write(
            r,
            "meta-mine/recipes-app/app/app-extra.inc",
            "inherit sanityish\ninherit autotoolsish\n",
        );
    });
    let config = eval(&fx, &Target::default());
    assert_eq!(
        not_found(&config),
        ["autotoolsish"],
        "{:?}\n{:?}",
        config.files(),
        config.replay().references
    );
    assert!(
        config
            .files()
            .iter()
            .any(|v| v.loc.path.ends_with("classes-global/sanityish.bbclass")
                && v.via.iter().any(|l| l.path.ends_with("extra.inc"))),
        "{:?}",
        config.files()
    );
    let ds = eval(&fx, &target("app"));
    // The configuration's issue is carried; the recipe's .inc adds sanityish (not a
    // recipe class) and finds autotoolsish.
    assert_eq!(not_found(&ds), ["autotoolsish", "sanityish"]);
    assert!(ds.files().iter().any(
        |v| v.loc.path.ends_with("classes-recipe/autotoolsish.bbclass")
            && v.via.iter().any(|l| l.path.ends_with("app-extra.inc"))
    ));
}

#[test]
fn inherit_in_a_conf_is_an_unparsed_line() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "\ninherit autotoolsish\n",
        );
    });
    let config = eval(&fx, &Target::default());
    assert!(
        config
            .replay()
            .references
            .iter()
            .any(|r| r.kind == "inherit"
                && matches!(&r.problem, RefProblem::Fatal(m) if m.contains("unparsed line"))),
        "{:?}",
        config.replay().references
    );
    assert!(
        !config
            .files()
            .iter()
            .any(|v| v.loc.path.ends_with("autotoolsish.bbclass"))
    );
}

/// Live counts (ignored; starts no BitBake): every collected recipe of the bitbake-setup
/// build evaluated with the configuration's machine and distro, and the notes counted by
/// kind. `SOUS_CHEF_LIVE` is the live environment; the index goes to
/// `$SOUS_CHEF_W8_DIR/cache` (required, so that nothing is written elsewhere).
#[test]
#[ignore]
fn live_note_counts() {
    use sous_chef::config::Config;
    use sous_chef::index::query;
    use sous_chef::types::FileKind;
    use std::collections::BTreeMap;
    let live = PathBuf::from(std::env::var_os("SOUS_CHEF_LIVE").expect("SOUS_CHEF_LIVE"));
    let out = PathBuf::from(std::env::var_os("SOUS_CHEF_W8_DIR").expect("SOUS_CHEF_W8_DIR"));
    let build = live.join("bitbake-builds/kiss-wrynose-dogbonedark/build");
    let mut cfg = Config {
        build_dir: Some(build.clone()),
        work_dir: build,
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    let ws = Workspace::discover(&cfg).unwrap();
    let mut idx = Index::open_in(&out.join("cache"), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let machines: Vec<Option<String>> = match std::env::var("SOUS_CHEF_W8_MACHINES") {
        Ok(m) => m.split_whitespace().map(|s| Some(s.to_string())).collect(),
        Err(_) => vec![None],
    };
    let cache = ConfigCache::new();
    for machine in machines {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut examples: BTreeMap<String, String> = BTreeMap::new();
        let mut n = 0;
        for f in query::all_files_in(&idx, query::FileSet::COLLECTED).unwrap() {
            if f.kind != FileKind::Recipe {
                continue;
            }
            let t = Target {
                recipe: Some(format!("{}:{}", f.layer, f.path.display())),
                machine: machine.clone(),
                ..Target::default()
            };
            let Ok(ds) = eval::evaluate_with(&cache, &idx, &ws, &t) else {
                continue;
            };
            if n == 0
                && let Ok(vars) = std::env::var("SOUS_CHEF_W8_PRINT")
            {
                for v in vars.split_whitespace() {
                    println!("{v} = {:?}", ds.value(v));
                }
            }
            n += 1;
            for note in &ds.replay().target_notes {
                let kind = if note.contains("would skip") || note.contains("may skip") {
                    let what = [
                        "SKIP_RECIPE",
                        "COMPATIBLE_MACHINE",
                        "machine",
                        "COMPATIBLE_HOST",
                        "host",
                        "license",
                        "LICENSE",
                    ]
                    .iter()
                    .find(|w| note.contains(*w))
                    .copied()
                    .unwrap_or("other");
                    format!(
                        "{} {what}",
                        if note.contains("would skip") {
                            "would"
                        } else {
                            "may"
                        }
                    )
                } else if note.contains("BBMASK") {
                    "BBMASK".to_string()
                } else {
                    continue;
                };
                *counts.entry(kind.clone()).or_default() += 1;
                examples
                    .entry(kind)
                    .or_insert_with(|| format!("{}: {note}", f.path.display()));
            }
        }
        println!("machine {machine:?}: {n} recipes evaluated");
        for (k, c) in &counts {
            println!("  {k}: {c}\n    e.g. {}", examples[k]);
        }
    }
}
