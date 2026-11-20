//! Anonymous python reproduced exactly (acc-anon-models, `src/eval/anonmodel.rs`): the
//! verified OE-core functions' writes, checked against the real functions.
//!
//! `tests/fixtures/anon_models/expected.json` was written by
//! `tests/fixtures/anon_models/oracle.py`, which runs each case of `cases.json` with the
//! release's own BitBake datastore (`bb.data_smart.DataSmart`) and OE-core `meta/lib`, the
//! function text taken from `src/eval/anonmodel/` (the texts sous-chef verifies), and reads
//! the outputs with `getVar` / `getVarFlag`:
//!
//! ```sh
//! cd tests/fixtures/anon_models
//! python3 oracle.py $SOUS_CHEF_LIVE wrynose cases.json expected.json      # host 3.14.9
//! python3 oracle.py $SOUS_CHEF_LIVE scarthgap cases.json expected.json
//! $SOUS_CHEF_LIVE/releases/kirkstone/python-bin/python3 \
//!     oracle.py $SOUS_CHEF_LIVE kirkstone cases.json expected.json        # 3.12.13
//! ```
//!
//! Here each case becomes a recipe of a copy of the fixture workspace that sets the same
//! variables and flags and inherits a class holding the same text; every output must be
//! certain (not flagged) and equal to the oracle's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value as Json;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    ws: Workspace,
    idx: Index,
}

fn model_text(name: &str) -> String {
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("src/eval/anonmodel/{name}.txt"));
    fs::read_to_string(p).unwrap()
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn build(edit: impl FnOnce(&Path)) -> Fx {
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
        ws,
        idx,
    }
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

/// The class holding a case's defs and function text.
fn class_text(case: &Json) -> String {
    let mut out = String::new();
    for d in case["defs"].as_array().unwrap() {
        out.push_str(&model_text(d.as_str().unwrap()));
        out.push('\n');
    }
    out.push_str(&model_text(case["text"].as_str().unwrap()));
    out
}

fn recipe_text(case: &Json, class: &str) -> String {
    let mut out = String::from("LICENSE = \"CLOSED\"\n");
    for (k, v) in case["vars"].as_object().unwrap() {
        out.push_str(&format!("{k} = \"{}\"\n", v.as_str().unwrap()));
    }
    for (var, flags) in case["flags"].as_object().unwrap() {
        for (f, v) in flags.as_object().unwrap() {
            out.push_str(&format!("{var}[{f}] = \"{}\"\n", v.as_str().unwrap()));
        }
    }
    out.push_str(&format!("inherit {class}\n"));
    out
}

fn cases() -> Vec<Json> {
    let text = fs::read_to_string(fixture_path("anon_models/cases.json")).unwrap();
    let v: Json = serde_json::from_str(&text).unwrap();
    v["cases"].as_array().unwrap().clone()
}

fn expected() -> BTreeMap<String, Json> {
    let text = fs::read_to_string(fixture_path("anon_models/expected.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn class_name(case: &Json) -> String {
    format!("am-{}", case["id"].as_str().unwrap())
}

fn output(ds: &DataStore, name: &str) -> Option<eval::Value> {
    match name.split_once('[') {
        Some((var, flag)) => ds.flag(var, flag.trim_end_matches(']')),
        None => ds.value(name),
    }
}

#[test]
fn every_case_equals_the_real_function() {
    let cases = cases();
    let expected = expected();
    let fx = build(|root| {
        for c in &cases {
            let class = class_name(c);
            write(
                root,
                &format!("meta-mine/classes/{class}.bbclass"),
                &class_text(c),
            );
            let pn = c["pn"].as_str().unwrap();
            write(
                root,
                &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
                &recipe_text(c, &class),
            );
            if c.get("manifest").is_some() {
                let m = fs::read_to_string(fixture_path(
                    "anon_models/python3-dir/python3/python3-manifest.json",
                ))
                .unwrap();
                write(
                    root,
                    &format!("meta-mine/recipes-app/{pn}/python3/python3-manifest.json"),
                    &m,
                );
            }
        }
    });
    let mut checked = 0;
    for c in &cases {
        let id = c["id"].as_str().unwrap();
        let ds = eval(&fx, c["pn"].as_str().unwrap());
        let want = &expected[id];
        for o in c["outputs"].as_array().unwrap() {
            let o = o.as_str().unwrap();
            let got = output(&ds, o);
            let want = want[o].as_str();
            match (got, want) {
                (Some(v), Some(w)) => {
                    assert!(
                        !v.unevaluated_python && v.unresolved.is_empty(),
                        "{id} {o} is flagged: {v:?}; causes {:?}",
                        ds.flag_causes(o)
                    );
                    assert_eq!(v.text, w, "{id} {o}");
                }
                (None, None) => {}
                (g, w) => panic!("{id} {o}: sous-chef {g:?}, BitBake {w:?}"),
            }
            checked += 1;
        }
    }
    assert!(checked > 60, "{checked}");
}

/// The base-w-plain case with its function changed by one byte, an uncertain input, or a
/// called `def` redefined: the writes are not reproduced and stay flagged.
#[test]
fn other_texts_and_uncertain_inputs_stay_flagged() {
    let cases = cases();
    let find = |id: &str| cases.iter().find(|c| c["id"] == id).unwrap().clone();
    let base = find("base-w-plain");
    let ua = find("ua-w");
    let fx = build(|root| {
        // One byte more in the function.
        let changed = class_text(&base).replacen("import string, re", "import string,  re", 1);
        write(root, "meta-mine/classes/am-changed.bbclass", &changed);
        write(
            root,
            "meta-mine/recipes-app/amx1/amx1_1.0.bb",
            &recipe_text(&base, "am-changed"),
        );
        // The verified text, but PACKAGECONFIG holds python outside the subset.
        write(
            root,
            "meta-mine/classes/am-base.bbclass",
            &class_text(&base),
        );
        let mut uncertain = base.clone();
        uncertain["vars"]["PACKAGECONFIG"] = Json::from("a ${@os.getpid()}");
        write(
            root,
            "meta-mine/recipes-app/amx2/amx2_1.0.bb",
            &recipe_text(&uncertain, "am-base"),
        );
        // update-alternatives with ua_extend_depends redefined by the recipe afterwards.
        write(root, "meta-mine/classes/am-ua.bbclass", &class_text(&ua));
        write(
            root,
            "meta-mine/recipes-app/amx3/amx3_1.0.bb",
            &format!(
                "{}def ua_extend_depends(d):\n    d.appendVar('DEPENDS', ' other')\n",
                recipe_text(&ua, "am-ua")
            ),
        );
        // A recipe's own anonymous python calling a def that appends: flagged, although
        // the anonymous function itself writes nothing by name.
        write(
            root,
            "meta-mine/recipes-app/amx4/amx4_1.0.bb",
            "LICENSE = \"CLOSED\"\nEXTRA_X = \"a\"\ndef add_x(d):\n    d.appendVar(\"EXTRA_X\", \" b\")\n\npython () {\n    add_x(d)\n}\n",
        );
    });
    for (pn, var) in [
        ("amx1", "DEPENDS"),
        ("amx1", "PACKAGECONFIG_CONFARGS"),
        ("amx2", "DEPENDS"),
        ("amx3", "DEPENDS"),
        ("amx4", "EXTRA_X"),
    ] {
        let ds = eval(&fx, pn);
        let v = ds.value(var).unwrap();
        assert!(v.unevaluated_python, "{pn} {var} not flagged: {v:?}");
    }
    // The refusal is named in the cause.
    let ds = eval(&fx, "amx3");
    let causes = ds.flag_causes("DEPENDS").join("\n");
    assert!(
        causes.contains("ua_extend_depends is defined at"),
        "{causes}"
    );
}
