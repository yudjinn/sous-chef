//! Inline python of the pure helpers evaluated statically (x1): propagation through
//! references, `:=` against the datastore of the moment, cycles, an expression reading a
//! value that is not certain, `OVERRIDES` built from an evaluated expression, an `inherit`
//! argument from one, and the skip checks on `DISTRO_FEATURES` computed by a helper.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, RefProblem, Target};
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

fn recipe(root: &Path, pn: &str, body: &str) {
    write(
        root,
        &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
        &format!("LICENSE = \"CLOSED\"\n{body}"),
    );
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

fn certain(ds: &DataStore, name: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(
        !v.unevaluated_python && v.unresolved.is_empty(),
        "{name} not certain: {v:?}"
    );
    v.text
}

#[test]
fn helpers_propagate_through_references_and_overrides() {
    let fx = modified(|r| {
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "DISTRO_FEATURES:append = \" pam\"\n",
        );
        recipe(
            r,
            "py",
            "FEAT = \"${@bb.utils.contains('DISTRO_FEATURES', 'pam', 'with-pam', 'no-pam', d)}\"\n\
             OPT = \"--${FEAT} ${@bb.utils.filter('DISTRO_FEATURES', 'wayland ipv6 x11', d)}\"\n\
             ARCHY = \"${@d.getVar('TARGET_ARCH').replace('_', '-')}\"\n",
        );
    });
    let ds = eval(&fx, "py");
    // local.conf: `DISTRO_FEATURES += "wayland"` before testdistro.conf's `?=`, which then
    // does nothing; the append adds pam.
    assert_eq!(certain(&ds, "FEAT"), "with-pam");
    assert_eq!(certain(&ds, "OPT"), "--with-pam wayland");
    assert_eq!(certain(&ds, "ARCHY"), "i686");
    // The history says a helper was evaluated statically.
    let log = ds.python_log("OPT");
    assert!(
        log.own
            .iter()
            .any(|e| e.evaluated && e.code.starts_with("bb.utils.contains('DISTRO_FEATURES'")),
        "{log:?}"
    );
}

#[test]
fn immediate_assignment_reads_the_datastore_of_that_moment() {
    let fx = modified(|r| {
        recipe(
            r,
            "imm",
            "X = \"one\"\n\
             EARLY := \"${@d.getVar('X') + '-' + str(1) if False else d.getVar('X')}\"\n\
             LAZY = \"${@d.getVar('X')}\"\n\
             BEFORE := \"${@d.getVar('Y') or 'unset'}\"\n\
             Y = \"y\"\n\
             X = \"two\"\n",
        );
    });
    let ds = eval(&fx, "imm");
    // `str(1)` is outside the subset: the whole expression stays (it is never partly run).
    assert!(ds.value("EARLY").unwrap().unevaluated_python);
    assert_eq!(certain(&ds, "LAZY"), "two");
    assert_eq!(certain(&ds, "BEFORE"), "unset");
}

#[test]
fn immediate_assignment_of_a_helper() {
    let fx = modified(|r| {
        recipe(
            r,
            "imm2",
            "F = \"a b\"\n\
             HAS := \"${@bb.utils.contains('F', 'b', 'yes', 'no', d)}\"\n\
             F = \"a\"\n\
             LATER = \"${@bb.utils.contains('F', 'b', 'yes', 'no', d)}\"\n",
        );
    });
    let ds = eval(&fx, "imm2");
    assert_eq!(certain(&ds, "HAS"), "yes");
    assert_eq!(certain(&ds, "LATER"), "no");
}

#[test]
fn cycles_are_left_unevaluated() {
    let fx = modified(|r| {
        recipe(
            r,
            "cyc",
            "SELF = \"${@d.getVar('SELF') or 'x'}\"\n\
             A = \"${@d.getVar('B') or 'a'}\"\n\
             B = \"${@d.getVar('A') or 'b'}\"\n",
        );
    });
    let ds = eval(&fx, "cyc");
    for n in ["SELF", "A", "B"] {
        let v = ds.value(n).unwrap();
        assert!(v.unevaluated_python, "{n}: {v:?}");
        assert!(v.text.starts_with("${@"), "{n}: {v:?}");
    }
    let log = ds.python_log("SELF");
    assert!(
        log.own.iter().any(|e| !e.evaluated
            && e.why.as_deref() == Some("SELF refers back to the value being expanded")),
        "{log:?}"
    );
}

#[test]
fn reading_a_value_that_is_not_certain_is_not_evaluated_and_names_it() {
    let fx = modified(|r| {
        recipe(
            r,
            "unc",
            "HOSTY = \"${@os.uname()[4]}\"\n\
             USES = \"${@bb.utils.contains('HOSTY', 'x86_64', 'a', 'b', d)}\"\n\
             BYPY = \"${@bb.utils.contains('SETBYANON', 'x', 'a', 'b', d)}\"\n\
             python () {\n    d.setVar('SETBYANON', 'x')\n}\n",
        );
    });
    let ds = eval(&fx, "unc");
    let v = ds.value("USES").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
    let log = ds.python_log("USES");
    let e = log
        .own
        .iter()
        .find(|e| e.code.starts_with("bb.utils.contains"))
        .unwrap();
    assert!(!e.evaluated);
    assert_eq!(
        e.why.as_deref(),
        Some("HOSTY holds python that is not evaluated")
    );
    // Unset, but anonymous python sets it: not None.
    let v = ds.value("BYPY").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
    let log = ds.python_log("BYPY");
    assert!(
        log.own.iter().any(|e| e.why.as_deref().is_some_and(
            |w| w.starts_with("SETBYANON is unset here, but anonymous python may set it")
        )),
        "{log:?}"
    );
}

#[test]
fn overrides_built_from_an_evaluated_expression() {
    let fx = modified(|r| {
        // OE-core's soc-family.inc and arch-armv7a.inc shapes.
        append_to(
            r,
            "meta-core/conf/bitbake.conf",
            "\nSOC_FAMILY = \"ti33x\"\nTUNE_FEATURES = \"arm armv7a neon\"\n\
             MACHINEOVERRIDES =. \"${@['', '${SOC_FAMILY}:']['${SOC_FAMILY}' != '']}\"\n\
             MACHINEOVERRIDES =. \"${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}\"\n",
        );
        recipe(
            r,
            "ovr",
            "V = \"plain\"\nV:armv7a = \"arm\"\nV:ti33x = \"soc\"\nW = \"w\"\nW:append:armv7a = \" +neon\"\n",
        );
    });
    let ds = eval(&fx, "ovr");
    let o = ds.overrides();
    for e in ["armv7a", "ti33x", "qemux86", "pn-ovr"] {
        assert!(o.iter().any(|i| i == e), "{e} not in {o:?}");
    }
    assert!(
        ds.override_entries()
            .iter()
            .all(|(_, unevaluated)| !unevaluated)
    );
    // ti33x is to the right of armv7a: it wins.
    assert_eq!(certain(&ds, "V"), "soc");
    assert_eq!(certain(&ds, "W"), "w +neon");
    assert_eq!(certain(&ds, "MACHINEOVERRIDES"), "armv7a:ti33x:qemux86");
}

#[test]
fn inherit_and_include_arguments_from_an_evaluated_expression() {
    let fx = modified(|r| {
        write(
            r,
            "meta-mine/classes/pamsupport.bbclass",
            "PAMCLASS = \"yes\"\n",
        );
        write(
            r,
            "meta-mine/recipes-app/inh/extra.inc",
            "FROMINC = \"1\"\n",
        );
        recipe(
            r,
            "inh",
            "inherit ${@bb.utils.contains('DISTRO_FEATURES', 'wayland', 'pamsupport', '', d)}\n\
             require ${@oe.utils.conditional('DISTRO', 'testdistro', 'extra.inc', 'nosuch.inc', d)}\n",
        );
        recipe(
            r,
            "inh2",
            "MINE = \"x\"\ninherit ${@oe.utils.read_file('classes')}\n",
        );
    });
    let ds = eval(&fx, "inh");
    assert_eq!(certain(&ds, "PAMCLASS"), "yes");
    assert_eq!(certain(&ds, "FROMINC"), "1");
    // Python outside the subset (it reads a file) is still refused, and the class it would
    // read may change any value (acc-unfollowed).
    let ds = eval(&fx, "inh2");
    let v = ds.value("MINE").unwrap();
    assert!(v.unevaluated_python, "{v:?}");
    let refs = &ds.replay().references;
    assert_eq!(
        refs.iter()
            .filter(|r| r.kind == "inherit" && r.problem == RefProblem::Python)
            .count(),
        1,
        "{refs:?}"
    );
}

#[test]
fn features_check_decides_on_features_a_helper_computes() {
    let fx = modified(|r| {
        write(
            r,
            "meta-core/classes-recipe/features_check.bbclass",
            FEATURES_CHECK,
        );
        // OE-core Wrynose bitbake.conf: the defaults minus the opted-out ones.
        append_to(
            r,
            "meta-core/conf/distro/testdistro.conf",
            "DISTRO_FEATURES_DEFAULTS = \"acl x11 pulseaudio\"\nDISTRO_FEATURES_OPTED_OUT = \"pulseaudio\"\n\
             DISTRO_FEATURES:append = \" ${@oe.utils.filter_default_features('DISTRO_FEATURES', d)}\"\n",
        );
        recipe(
            r,
            "needx11",
            "inherit features_check\nREQUIRED_DISTRO_FEATURES = \"x11\"\n",
        );
        recipe(
            r,
            "needpulse",
            "inherit features_check\nREQUIRED_DISTRO_FEATURES = \"pulseaudio\"\n",
        );
    });
    let ds = eval(&fx, "needx11");
    let f = certain(&ds, "DISTRO_FEATURES");
    assert!(f.split_whitespace().any(|w| w == "x11"), "{f}");
    let skips = |ds: &DataStore| -> Vec<String> {
        ds.replay()
            .target_notes
            .iter()
            .filter(|n| n.contains("skip this recipe"))
            .cloned()
            .collect()
    };
    assert!(skips(&ds).is_empty(), "{:?}", skips(&ds));
    let n = skips(&eval(&fx, "needpulse"));
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(
        n[0].starts_with("note: BitBake would skip this recipe"),
        "{n:?}"
    );
}

/// OE-core Wrynose `classes-recipe/features_check.bbclass`, the part the skip check reads.
const FEATURES_CHECK: &str = r#"python () {
    if bb.utils.to_boolean(d.getVar('PARSE_ALL_RECIPES', False)):
        return

    unused = True

    for kind in ['DISTRO', 'MACHINE', 'COMBINED', 'IMAGE', 'TUNE']:
        if d.getVar('ANY_OF_' + kind + '_FEATURES') is None and not d.hasOverrides('ANY_OF_' + kind + '_FEATURES') and \
           d.getVar('REQUIRED_' + kind + '_FEATURES') is None and not d.hasOverrides('REQUIRED_' + kind + '_FEATURES') and \
           d.getVar('CONFLICT_' + kind + '_FEATURES') is None and not d.hasOverrides('CONFLICT_' + kind + '_FEATURES'):
            continue

        unused = False

        # Assume at least one var is set.
        features = set((d.getVar(kind + '_FEATURES') or '').split())

        any_of_features = set((d.getVar('ANY_OF_' + kind + '_FEATURES') or '').split())
        if any_of_features:
            if set.isdisjoint(any_of_features, features):
                raise bb.parse.SkipRecipe("one of '%s' needs to be in %s_FEATURES"
                    % (' '.join(any_of_features), kind))

        required_features = set((d.getVar('REQUIRED_' + kind + '_FEATURES') or '').split())
        if required_features:
            missing = set.difference(required_features, features)
            if missing:
                if kind == 'DISTRO':
                    raise bb.parse.SkipRecipe("using DISTRO '%s', which is missing required DISTRO_FEATURES: '%s'"
                        % (d.getVar('DISTRO'), ' '.join(missing)))
                else:
                    raise bb.parse.SkipRecipe("missing required %s feature%s '%s' (not in %s_FEATURES)"
                        % (kind.lower(), 's' if len(missing) > 1 else '', ' '.join(missing), kind))

        conflict_features = set((d.getVar('CONFLICT_' + kind + '_FEATURES') or '').split())
        if conflict_features:
            conflicts = set.intersection(conflict_features, features)
            if conflicts:
                raise bb.parse.SkipRecipe("conflicting %s feature%s '%s' (in %s_FEATURES)"
                    % (kind.lower(), 's' if len(conflicts) > 1 else '', ' '.join(conflicts), kind))

    if unused:
        bb.warn("Recipe inherits features_check but doesn't use it")
}
"#;
