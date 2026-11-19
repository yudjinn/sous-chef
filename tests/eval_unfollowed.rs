//! References the replay cannot follow, or not exactly (acc-unfollowed; the rules are in
//! docs/bitbake-semantics.md, "References that cannot be followed exactly"): an include or
//! inherit BitBake follows and the replay does not may change any value set after it (and,
//! for `inherit_defer`, classes are read at finalisation), so the values it may change are
//! flagged, with a cause naming the reference. When the python in the argument has a known
//! set of exact results (`['', 'populate_sdk_ext'][...]`) or is the host's `os.uname()`
//! field in an include word, the words read whatever the python gives are followed and the
//! others are read on a copy of the store to learn what they would change.
//!
//! Each test runs on a temporary copy of the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn evaluate(edit: impl FnOnce(&Path), recipe: &str) -> DataStore {
    let (_tmp, root) = temp_ws();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &t).unwrap()
}

fn recipe(root: &Path, pn: &str, body: &str) {
    write(
        root,
        &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
        &format!("LICENSE = \"CLOSED\"\n{body}"),
    );
}

fn certain(ds: &DataStore, name: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(
        !v.unevaluated_python && v.unresolved.is_empty(),
        "{name} not certain: {v:?}; {:?}",
        ds.flag_causes(name)
    );
    v.text
}

fn flagged_by(ds: &DataStore, name: &str, cause_start: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(v.unevaluated_python, "{name} not flagged: {v:?}");
    let causes = ds.flag_causes(name);
    assert!(
        causes.iter().any(|c| c.starts_with(cause_start)),
        "{name}: no cause starting {cause_start:?} in {causes:?}"
    );
    v.text
}

/// An inherit whose python is outside the subset: the class BitBake reads may set any
/// variable, so a value set before it, which a class could change, is flagged. Before
/// acc-unfollowed the reference only gave a note and the value was certain.
#[test]
fn an_unfollowed_inherit_flags_the_values_it_may_change() {
    let ds = evaluate(
        |r| {
            write(
                r,
                "meta-mine/classes/extra.bbclass",
                "MYVAR:append = \" b\"\n",
            );
            recipe(
                r,
                "unf",
                "MYVAR = \"a\"\ninherit ${@oe.utils.read_file('which-class')}\n",
            );
        },
        "unf",
    );
    let v = flagged_by(
        &ds,
        "MYVAR",
        "an inherit at meta-mine:recipes-app/unf/unf_1.0.bb:3 could not be followed (python in its argument); the files it would read may change any value",
    );
    assert_eq!(v, "a");
}

/// OE-core's `image.bbclass`: `inherit_defer ${IMGCLASSES}`, one word of which is python
/// whose result depends on the host (`SDK_OS` is `os.uname()`). The other words are read
/// (their changes are certain: `DEPENDS` gets `wic-native` from the class standing in for
/// `image_types_wic`), the python's class is read on a copy only: the variables it changes
/// are flagged, an override variant whose override is not active is not, and nothing else.
/// Before acc-unfollowed the whole inherit was dropped and `DEPENDS` lacked `wic-native`,
/// unflagged.
#[test]
fn an_inherit_with_known_python_results_is_followed_for_its_certain_words() {
    let ds = evaluate(
        |r| {
            write(
                r,
                "meta-mine/classes/imgish.bbclass",
                "SDK_OS_X = \"${@os.uname()[0].lower()}\"\n\
                 IMGCLASSES = \"rootfs_x ${@['', 'sdkext']['linux' in d.getVar(\"SDK_OS_X\")]} imgtypes\"\n\
                 inherit_defer ${IMGCLASSES}\n",
            );
            write(
                r,
                "meta-mine/classes/rootfs_x.bbclass",
                "ROOTFS_X = \"1\"\n",
            );
            write(
                r,
                "meta-mine/classes/imgtypes.bbclass",
                "DEPENDS += \"wic-native\"\n",
            );
            write(
                r,
                "meta-mine/classes/sdkext.bbclass",
                "SDKVAR:append = \" ext\"\nDEPENDS:append:class-nativesdk = \" sdk-only\"\n",
            );
            recipe(r, "img", "SDKVAR = \"base\"\ninherit imgish\n");
        },
        "img",
    );
    assert_eq!(certain(&ds, "ROOTFS_X"), "1");
    assert!(
        certain(&ds, "DEPENDS")
            .split_whitespace()
            .any(|w| w == "wic-native"),
        "{:?}",
        ds.value("DEPENDS")
    );
    let v = flagged_by(
        &ds,
        "SDKVAR",
        "an inherit_defer at meta-mine:classes/imgish.bbclass:3 could not be followed exactly (python in IMGCLASSES); sdkext, which it may read and sous-chef did not, would change SDKVAR",
    );
    assert_eq!(v, "base");
    // Not part of the reference's references list: it was followed, partly.
    assert!(
        !ds.replay()
            .references
            .iter()
            .any(|r| r.text == "${IMGCLASSES}"),
        "{:?}",
        ds.replay().references
    );
}

/// `include conf/machine-sdk/${SDKMACHINE}.conf` with `SDKMACHINE` the host's
/// `os.uname()[4]`: one of the files of that shape is read, or none. Their plain
/// assignments are flagged; their override variants for an override that is not active
/// are not; values they do not touch are not.
#[test]
fn an_include_of_a_host_named_file_flags_what_those_files_change() {
    let ds = evaluate(
        |r| {
            write(
                r,
                "meta-mine/conf/sdkm/x86_64.conf",
                "SDKA = \"x86_64\"\nSDKB:class-nativesdk = \"1\"\n",
            );
            write(
                r,
                "meta-mine/conf/sdkm/aarch64.conf",
                "SDKA = \"aarch64\"\n",
            );
            recipe(
                r,
                "sdkm",
                "SDKA = \"none\"\nSDKB = \"0\"\nSDKM ??= \"${@os.uname()[4]}\"\n\
                 include conf/sdkm/${SDKM}.conf\nDERIVED := \"${SDKA}-x\"\n",
            );
        },
        "sdkm",
    );
    let v = flagged_by(
        &ds,
        "SDKA",
        "an include at meta-mine:recipes-app/sdkm/sdkm_1.0.bb:5 could not be followed exactly (python in SDKM); conf/sdkm/aarch64.conf, conf/sdkm/x86_64.conf, which it may read and sous-chef did not, would change SDKA",
    );
    assert_eq!(v, "none");
    assert_eq!(certain(&ds, "SDKB"), "0");
    // `:=` stored the value expanded at that point: it is flagged with the files.
    let v = ds.value("DERIVED").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
    assert_eq!(certain(&ds, "LICENSE"), "CLOSED");
}

/// An include whose python has known results, one of which names no file: an include of a
/// missing file does nothing (`ConfHandler.include_single_file`, no error for `include`),
/// so only the file that exists is read on a copy.
#[test]
fn an_include_whose_alternatives_name_no_file_changes_nothing() {
    let ds = evaluate(
        |r| {
            recipe(
                r,
                "noinc",
                "HOSTOS = \"${@os.uname()[0].lower()}\"\nKEEP = \"k\"\n\
                 include ${@['nosuch-a.inc', 'nosuch-b.inc']['linux' in d.getVar('HOSTOS')]}\n",
            );
        },
        "noinc",
    );
    assert_eq!(certain(&ds, "KEEP"), "k");
}

/// A variable whose name `expandKeys` cannot expand (`${SEL}` holds python) is renamed
/// by BitBake, possibly onto `MYV:append`: `MYV` may differ. Before acc-unfollowed only a
/// python read of an unset name took this into account.
#[test]
fn a_name_that_cannot_be_expanded_flags_the_variable_it_may_become() {
    let ds = evaluate(
        |r| {
            recipe(
                r,
                "keys",
                "MYV = \"a\"\nOTHER = \"o\"\nSEL = \"${@oe.utils.read_file('which')}\"\nMYV:${SEL} = \" b\"\n",
            );
        },
        "keys",
    );
    let v = flagged_by(&ds, "MYV", "the variable named `MYV:${SEL}`");
    assert_eq!(v, "a");
    assert_eq!(certain(&ds, "OTHER"), "o");
}

/// `eval::anon` takes a name out of `python_vars` when every literal write to it is proved
/// not to happen; a write through a computed name (`d.setVar(name, ...)`) in another
/// function that may run can still reach it, so it stays flagged. A computed name built
/// from a literal prefix (`"do_x_%s" % t`) cannot, and does not block the proof.
#[test]
fn a_computed_name_write_keeps_a_disproved_literal_write_possible() {
    const LITERAL: &str = "python () {\n    if d.getVar('GATE') != '1':\n        return\n    d.setVar('MYW', 'changed')\n}\n";
    let ds = evaluate(
        |r| {
            recipe(
                r,
                "cw",
                &format!(
                    "MYW = \"orig\"\nGATE = \"0\"\n{LITERAL}python () {{\n    for v in (d.getVar('NAMES') or '').split():\n        d.setVar(v, '1')\n}}\n"
                ),
            );
        },
        "cw",
    );
    let v = ds.value("MYW").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
    assert!(
        ds.replay().no_writes.is_empty(),
        "{:?}",
        ds.replay().no_writes
    );
}

#[test]
fn a_prefixed_computed_name_write_does_not_block_the_proof() {
    const BODY: &str = "MYW = \"orig\"\nGATE = \"0\"\npython () {\n    if d.getVar('GATE') != '1':\n        return\n    d.setVar('MYW', 'changed')\n}\npython () {\n    for t in (d.getVar('TYPES') or '').split():\n        task = \"do_x_%s\" % t\n        d.setVar(task, 'cmds')\n}\n";
    let ds = evaluate(|r| recipe(r, "cwp", BODY), "cwp");
    assert_eq!(certain(&ds, "MYW"), "orig");
}

/// A file BitBake reads that the index does not hold (here a `.cfg`, a kind the index does
/// not take): the replay has no statements for it, so it may change any value. Before
/// acc-unfollowed the `include` was a silent no-op.
#[test]
fn an_include_of_a_file_the_index_does_not_hold_flags_every_value() {
    let ds = evaluate(
        |r| {
            write(
                r,
                "meta-mine/recipes-app/cfg/extra.cfg",
                "MYVAR = \"from-cfg\"\n",
            );
            recipe(r, "cfg", "MYVAR = \"a\"\ninclude extra.cfg\n");
        },
        "cfg",
    );
    let v = flagged_by(
        &ds,
        "MYVAR",
        "an include at meta-mine:recipes-app/cfg/cfg_1.0.bb:3 could not be followed (",
    );
    assert_eq!(v, "a");
    assert!(
        ds.flag_causes("MYVAR")
            .iter()
            .any(|c| c.contains("extra.cfg is not in the index")),
        "{:?}",
        ds.flag_causes("MYVAR")
    );
}
