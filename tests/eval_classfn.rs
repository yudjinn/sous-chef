//! Class functions evaluated statically (acc-helpers, `src/eval/classfn.rs`): a function a
//! class defines with `def` is run only when the last definition of its name the parse
//! executed is the verified text (`tests/fixtures/classfn/realdefs.bbclass`, copied from
//! OE-core); a different definition (the fixture workspace's own stand-in
//! `base_set_filespath`, a recipe's `gnome_verdir`) or none leaves it unevaluated, and
//! `bb.data.inherits_class` is answered only when the inherits are certain.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let real = fs::read_to_string(fixture_path("classfn/realdefs.bbclass")).unwrap();
    write(&root, "meta-mine/classes/realdefs.bbclass", &real);
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

fn flagged(ds: &DataStore, name: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(v.unevaluated_python, "{name} not flagged: {v:?}");
    v.text
}

const USES: &str = "BASE_DEFAULT_DEPS = \"virtual/libc\"\n\
    AUTO = \"${@get_autotools_dep(d)}\"\n\
    BASEDEP = \"${@get_base_dep(d)}\"\n\
    GD = \"${@gettext_dependencies(d)}\"\n\
    GO = \"${@gettext_oeconf(d)}\"\n\
    GV = \"${@gnome_verdir(\"${PV}.5\")}\"\n\
    FILESOVERRIDES = \"arm:kiss\"\n\
    FILESEXTRAPATHS:prepend = \"/extra:\"\n\
    FP = \"${@base_set_filespath([\"/r/${BPN}\", \"/r/files\"], d)}\"\n\
    valid_archs = \"x86 arm\"\n\
    KARCH = \"${@map_kernel_arch(d.getVar('TARGET_ARCH'), d)}\"\n\
    UARCH = \"${@map_uboot_arch(d.getVar('KARCH'), d)}\"\n";

#[test]
fn verified_definitions_are_evaluated() {
    let fx = modified(|r| recipe(r, "cf", &format!("inherit realdefs\n{USES}")));
    let ds = eval(&fx, "cf");
    assert_eq!(
        certain(&ds, "AUTO"),
        "autoconf-native automake-native libtool-native libtool-cross "
    );
    // The returned text is expanded again, as BitBake's expansion loop does.
    assert_eq!(certain(&ds, "BASEDEP"), "virtual/libc");
    assert_eq!(certain(&ds, "GD"), "gettext-native");
    assert_eq!(certain(&ds, "GO"), "--enable-nls");
    assert_eq!(certain(&ds, "GV"), "1.0");
    // The fixture's TARGET_ARCH is i686.
    assert_eq!(certain(&ds, "KARCH"), "x86");
    assert_eq!(certain(&ds, "UARCH"), "x86");
    // FILESEXTRAPATHS is `__default:` in the fixture's bitbake.conf, or unset.
    let fp = certain(&ds, "FP");
    assert!(
        fp.starts_with("/extra/kiss:/r/cf/kiss:/r/files/kiss:/extra/arm:")
            && fp.ends_with(":/r/cf/:/r/files/"),
        "{fp}"
    );
    let log = ds.python_log("AUTO");
    assert!(
        log.own
            .iter()
            .any(|e| e.evaluated && e.code == "get_autotools_dep(d)"),
        "{log:?}"
    );
}

#[test]
fn other_definitions_are_not_evaluated() {
    let fx = modified(|r| {
        // A recipe's own gnome_verdir (OE-core's gcr does this) after the class's.
        recipe(
            r,
            "redef",
            &format!(
                "inherit realdefs\n{USES}def gnome_verdir(v):\n    return \".\".join(v.split(\".\")[:2]) or v\n"
            ),
        );
        // No realdefs: base_set_filespath is the fixture's stand-in (meta-core
        // base.bbclass), the others are not defined at all.
        recipe(r, "nodefs", USES);
    });
    let ds = eval(&fx, "redef");
    assert!(flagged(&ds, "GV").contains("${@gnome_verdir("));
    assert_eq!(certain(&ds, "BASEDEP"), "virtual/libc");
    let log = ds.python_log("GV");
    assert!(
        log.own.iter().any(|e| !e.evaluated
            && e.why
                .as_deref()
                .is_some_and(|w| w.contains("other than the verified one"))),
        "{log:?}"
    );
    let ds = eval(&fx, "nodefs");
    for v in ["AUTO", "BASEDEP", "GD", "GO", "GV", "FP"] {
        flagged(&ds, v);
    }
    let log = ds.python_log("FP");
    assert!(
        log.own.iter().any(|e| e
            .why
            .as_deref()
            .is_some_and(|w| w.contains("base_set_filespath is defined at"))),
        "{log:?}"
    );
}

#[test]
fn inherits_class_is_read_from_the_inherit_cache() {
    let fx = modified(|r| {
        write(r, "meta-mine/classes/native.bbclass", "# a stand-in\n");
        recipe(r, "nat", &format!("inherit realdefs native\n{USES}"));
        // An inherit whose argument names an undefined variable is not followed: whether
        // the recipe inherits native is then not known.
        recipe(
            r,
            "unsure",
            &format!("inherit realdefs\ninherit ${{MAYBE_NATIVE}}\n{USES}"),
        );
        // An inherit followed only partly (acc-unfollowed): the python gives '' or
        // 'native' depending on the host, so native may be inherited; the stand-in class
        // changes no variable, so nothing else is flagged.
        let partly = "HOSTOS = \"${@os.uname()[0].lower()}\"\n\
                      inherit ${@['', 'native']['linux' in d.getVar('HOSTOS')]}\n";
        recipe(r, "partly", &format!("inherit realdefs\n{partly}{USES}"));
        // INHIBIT_AUTOTOOLS_DEPS decides first: no inherits_class call.
        recipe(
            r,
            "inhibit",
            &format!("inherit realdefs\n{partly}INHIBIT_AUTOTOOLS_DEPS = \"1\"\n{USES}"),
        );
    });
    let ds = eval(&fx, "nat");
    assert_eq!(
        certain(&ds, "AUTO"),
        "autoconf-native automake-native libtool-native "
    );
    let ds = eval(&fx, "unsure");
    flagged(&ds, "AUTO");
    // gettext_dependencies asks only when INHIBIT_DEFAULT_DEPS is set, so its value is
    // computed; but the inherit that was not followed may read any class (BitBake reads
    // one: `MAYBE_NATIVE` must be set where sous-chef cannot see it, or the parse fails),
    // which may change any value (acc-unfollowed).
    assert_eq!(flagged(&ds, "GD"), "gettext-native");
    assert!(
        ds.flag_causes("GD")
            .iter()
            .any(|c| c.starts_with("an inherit at meta-mine:recipes-app/unsure/unsure_1.0.bb:3 could not be followed (undefined variable MAYBE_NATIVE)")),
        "{:?}",
        ds.flag_causes("GD")
    );
    let ds = eval(&fx, "partly");
    flagged(&ds, "AUTO");
    assert_eq!(certain(&ds, "GD"), "gettext-native");
    let ds = eval(&fx, "inhibit");
    assert_eq!(certain(&ds, "AUTO"), "");
    let _ = &fx.root;
}

/// `handleVirtRecipeProviders` stores DEPENDS with its words joined by single spaces: a
/// `DEPENDS:prepend = "${BASEDEPENDS} "` whose python gives "" ends as "" (OE-core's
/// base-files and packagegroups, `INHIBIT_DEFAULT_DEPS = "1"`).
#[test]
fn depends_of_only_spaces_is_stored_empty() {
    let fx = modified(|r| {
        recipe(
            r,
            "inh",
            "inherit realdefs\nINHIBIT_DEFAULT_DEPS = \"1\"\n\
             BASEDEPENDS = \"${@get_base_dep(d)}\"\nDEPENDS:prepend = \"${BASEDEPENDS} \"\n",
        )
    });
    let ds = eval(&fx, "inh");
    assert_eq!(certain(&ds, "DEPENDS"), "");
}

// ---------------------------------------------------------------------------------------
// Class extension: the read filters of OE-core Wrynose's native.bbclass and
// nativesdk.bbclass, reproduced only for OE-core's own text
// (`tests/fixtures/classfn/native-handler.bbclass`, `nativesdk-handler.bbclass`).

fn class_extension(r: &Path, real: bool) {
    let native = if real {
        fs::read_to_string(fixture_path("classfn/native-handler.bbclass")).unwrap()
    } else {
        // A handler of the same name with other code: nothing of it is reproduced.
        "python native_virtclass_handler () {\n    pn = d.getVar(\"PN\")\n}\n\
         addhandler native_virtclass_handler\n\
         native_virtclass_handler[eventmask] = \"bb.event.RecipePreFinalise\"\n"
            .to_string()
    };
    write(
        r,
        "meta-core/classes-recipe/native.bbclass",
        &format!("CLASSOVERRIDE = \"class-native\"\nMACHINEOVERRIDES = \"\"\n{native}"),
    );
    let sdk = fs::read_to_string(fixture_path("classfn/nativesdk-handler.bbclass")).unwrap();
    write(
        r,
        "meta-core/classes-recipe/nativesdk.bbclass",
        &format!("CLASSOVERRIDE = \"class-nativesdk\"\nMACHINEOVERRIDES = \"\"\n{sdk}"),
    );
    // OE-core bitbake.conf's values for the feature filters.
    let p = r.join("meta-core/conf/bitbake.conf");
    let mut conf = fs::read_to_string(&p).unwrap();
    conf.push_str(
        "DISTRO_FEATURES_NATIVE ?= \"acl x11 ipv6 xattr\"\n\
         DISTRO_FEATURES_NATIVESDK ?= \"acl x11 ipv6 xattr\"\n\
         DISTRO_FEATURES_FILTER_NATIVE ?= \"api-documentation debuginfod opencl opengl wayland\"\n\
         DISTRO_FEATURES_FILTER_NATIVESDK ?= \"api-documentation debuginfod opencl opengl wayland\"\n\
         SPECIAL_PKGSUFFIX = \"-native -cross\"\n\
         BPN = \"${@oe.utils.prune_suffix(d.getVar('PN'), d.getVar('SPECIAL_PKGSUFFIX').split(), d)}\"\n",
    );
    fs::write(p, conf).unwrap();
    write(
        r,
        "meta-mine/recipes-app/tool/tool_1.0.bb",
        "LICENSE = \"CLOSED\"\n\
         DEPENDS = \"zlib pkgconf-native binutils-cross-arm tool-native virtual/libc (>= 2)\"\n\
         PACKAGES = \"${PN}-src ${PN} libtool-x\"\n\
         PACKAGES_DYNAMIC = \"^${PN}-locale-.*$\"\n\
         PACKAGE_WRITE_DEPS = \"ptest-runner\"\n\
         RDEPENDS:${PN}-src = \"bash\"\n\
         BBCLASSEXTEND = \"native nativesdk\"\n",
    );
}

#[test]
fn native_read_filters_for_oe_cores_handler() {
    let fx = modified(|r| class_extension(r, true));
    let ds = eval(&fx, "tool-native");
    // native_filter(val, 'tool-native', 'tool', selfref=False): the recipe itself goes, the
    // version constraint too (explode_deps). handleVirtRecipeProviders stores the filtered
    // words and the next read filters them again, as BitBake does: binutils-cross-arm, then
    // binutils-native-arm, then binutils-native-arm-native.
    assert_eq!(
        certain(&ds, "DEPENDS"),
        "zlib-native pkgconf-native binutils-native-arm-native virtual/libc-native"
    );
    assert_eq!(
        certain(&ds, "PACKAGES"),
        "tool-src-native tool-native libtool-x-native"
    );
    assert_eq!(certain(&ds, "PACKAGES_DYNAMIC"), "^tool-locale-.*-native$");
    // A reference reads the filtered value too.
    let e = ds.expand("${DEPENDS}");
    assert!(e.text.starts_with("zlib-native "), "{e:?}");
    // The runtime variables are renamed per package too, which is not reproduced.
    flagged(&ds, "RDEPENDS:tool-native-src");
    // set_class_filter: acl x11 ipv6 xattr, plus wayland (local.conf's DISTRO_FEATURES +=
    // "wayland", kept by DISTRO_FEATURES_FILTER_NATIVE).
    assert_eq!(
        certain(&ds, "DISTRO_FEATURES"),
        "acl ipv6 wayland x11 xattr"
    );
    assert!(
        ds.replay().var_notes["DEPENDS"]
            .iter()
            .any(|n| n.contains("native_filter(val, 'tool-native', 'tool', selfref=False)")),
        "{:?}",
        ds.replay().var_notes
    );
    // The base recipe is not filtered.
    let base = eval(&fx, "tool");
    assert!(certain(&base, "DEPENDS").starts_with("zlib pkgconf-native"));
}

#[test]
fn a_handler_with_other_code_stays_flagged() {
    let fx = modified(|r| class_extension(r, false));
    let ds = eval(&fx, "tool-native");
    for v in ["DEPENDS", "PACKAGES", "DISTRO_FEATURES"] {
        flagged(&ds, v);
    }
}

#[test]
fn nativesdk_filters_for_oe_cores_anonymous_python() {
    let fx = modified(|r| class_extension(r, true));
    let ds = eval(&fx, "nativesdk-tool");
    // suffix_filter_deps(val, 'nativesdk', []): a -native or -cross- word stays; sorted (the
    // words handleVirtRecipeProviders stores are filtered again on the next read), the
    // version constraint kept.
    assert_eq!(
        certain(&ds, "DEPENDS"),
        "binutils-cross-arm nativesdk-zlib pkgconf-native tool-native virtual/nativesdk-libc (>= 2)"
    );
    assert_eq!(certain(&ds, "PACKAGE_WRITE_DEPS"), "nativesdk-ptest-runner");
    assert_eq!(
        certain(&ds, "DISTRO_FEATURES"),
        "acl ipv6 wayland x11 xattr"
    );
    // rename_package_variables (PACKAGES and the per-package names) is not reproduced.
    flagged(&ds, "PACKAGES");
}
