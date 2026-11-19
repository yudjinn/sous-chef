//! Python functions OE-core defines in classes with `def` that the inline python evaluator
//! ([`super::pyexpr`]) may run, and how it makes sure the definition in force is the one
//! that was verified.
//!
//! BitBake runs a top-level `def` of a `.bb`, `.bbclass` or `.inc` when it parses the
//! statement (`ast.PythonMethodNode.eval`: `bb.methodpool.insert_method` executes the text
//! into the global python context, and the text is also stored as the function's variable),
//! so `${@get_base_dep(d)}` calls whatever definition of that name was executed last. A
//! layer can redefine a class function (OE-core's `gcr` and `libgudev` recipes redefine
//! `gnome_verdir`), so a function is identified by **name and text**: the replay records,
//! for each allowlisted name, the last `def` of it the recipe's parse executed
//! ([`ClassDef`]), compared with the verified text ([`verify`]). Only when it is the same,
//! byte for byte (trailing blank lines aside), is the function evaluated; a different
//! definition, or none, leaves the expression unevaluated and flagged, as before.
//!
//! Each function below is pure: it reads the datastore through `d.getVar` only, calls
//! nothing but `str` methods, `os.path.join` and `bb.data.inherits_class` /
//! `oe.utils.inherits` (which read `__inherit_cache`), and has no side effect. Their
//! semantics are reproduced in `pyexpr.rs`; docs/bitbake-semantics.md ("Class functions
//! evaluated statically") has the table with sources and releases.

use std::path::Path;

use serde::Serialize;

use super::DataStore;
use super::replay::RefProblem;
use crate::types::{FileKind, Loc};

/// An allowlisted class function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ClassFn {
    /// `base_set_filespath(path, d)`, OE-core `classes-global/utils.bbclass`.
    BaseSetFilespath,
    /// `get_autotools_dep(d)`, OE-core `classes-recipe/autotools.bbclass`.
    GetAutotoolsDep,
    /// `get_base_dep(d)`, OE-core `classes-global/base.bbclass`.
    GetBaseDep,
    /// `gettext_dependencies(d)`, OE-core `classes-recipe/gettext.bbclass`.
    GettextDependencies,
    /// `gettext_oeconf(d)`, OE-core `classes-recipe/gettext.bbclass`.
    GettextOeconf,
    /// `gnome_verdir(v)`, OE-core `classes-recipe/gnomebase.bbclass`.
    GnomeVerdir,
    /// `map_kernel_arch(a, d)`, OE-core `classes-recipe/kernel-arch.bbclass`.
    MapKernelArch,
    /// `map_uboot_arch(a, d)`, OE-core `classes-recipe/kernel-arch.bbclass`.
    MapUbootArch,
}

/// Every allowlisted function.
pub const ALL: &[ClassFn] = &[
    ClassFn::BaseSetFilespath,
    ClassFn::GetAutotoolsDep,
    ClassFn::GetBaseDep,
    ClassFn::GettextDependencies,
    ClassFn::GettextOeconf,
    ClassFn::GnomeVerdir,
    ClassFn::MapKernelArch,
    ClassFn::MapUbootArch,
];

impl ClassFn {
    /// The function with this python name.
    pub fn from_name(name: &str) -> Option<ClassFn> {
        ALL.iter().copied().find(|f| f.name() == name)
    }

    /// The python name.
    pub fn name(self) -> &'static str {
        match self {
            ClassFn::BaseSetFilespath => "base_set_filespath",
            ClassFn::GetAutotoolsDep => "get_autotools_dep",
            ClassFn::GetBaseDep => "get_base_dep",
            ClassFn::GettextDependencies => "gettext_dependencies",
            ClassFn::GettextOeconf => "gettext_oeconf",
            ClassFn::GnomeVerdir => "gnome_verdir",
            ClassFn::MapKernelArch => "map_kernel_arch",
            ClassFn::MapUbootArch => "map_uboot_arch",
        }
    }

    /// The verified definitions (the `def` line and its body, as written). Each is the
    /// same on OE-core `kirkstone` (`meta/classes/`), `scarthgap` and Wrynose `33a6bd6cc8`
    /// (`meta/classes-global/` or `meta/classes-recipe/`), except `map_kernel_arch`, whose
    /// kirkstone text (no `loongarch` line) differs and is not verified.
    pub fn verified(self) -> &'static [&'static str] {
        match self {
            ClassFn::BaseSetFilespath => &[BASE_SET_FILESPATH],
            ClassFn::GetAutotoolsDep => &[GET_AUTOTOOLS_DEP],
            ClassFn::GetBaseDep => &[GET_BASE_DEP],
            ClassFn::GettextDependencies => &[GETTEXT_DEPENDENCIES],
            ClassFn::GettextOeconf => &[GETTEXT_OECONF],
            ClassFn::GnomeVerdir => &[GNOME_VERDIR],
            ClassFn::MapKernelArch => &[MAP_KERNEL_ARCH],
            ClassFn::MapUbootArch => &[MAP_UBOOT_ARCH],
        }
    }
}

/// The last `def` of an allowlisted name the parse executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassDef {
    /// The `def` statement.
    pub loc: Loc,
    /// The function, when the text is a verified definition of it; `None` when it is not
    /// (or could not be read).
    pub verified: Option<ClassFn>,
}

/// The text of a definition as compared: lines without a trailing `\r`, trailing blank
/// lines dropped.
fn normalize(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// The allowlisted function `text` (a `def name(...):` statement with its body) is a
/// verified definition of, if any.
pub fn verify(name: &str, text: &str) -> Option<ClassFn> {
    let f = ClassFn::from_name(name)?;
    let t = normalize(text);
    f.verified().iter().any(|v| *v == t).then_some(f)
}

impl DataStore {
    /// Whether `f` may be evaluated here: `Ok` when the last definition of its name the
    /// parse executed is a verified one, else why not.
    pub fn class_fn_defined(&self, f: ClassFn) -> Result<(), String> {
        match self.notes.class_defs.get(f.name()) {
            Some(ClassDef {
                verified: Some(v), ..
            }) if *v == f => Ok(()),
            Some(d) => Err(format!(
                "{} is defined at {} with a text other than the verified one",
                f.name(),
                d.loc
            )),
            None => Err(format!("{} is not defined in this parse", f.name())),
        }
    }

    /// `bb.data.inherits_class(klass, d)` (BitBake 2.18 `lib/bb/data.py`; the same in 2.0
    /// and 2.8): whether a path in `__inherit_cache` ends with `/<klass>.bbclass`. `None`
    /// when the answer is not certain: a reference that could have inherited a class was
    /// not followed (its argument holds python or undefined variables), or was followed
    /// with a value that is not certain. A reference in a `.conf` file is an `include` /
    /// `require` read by `ConfHandler`, which has no `inherit`, so it cannot.
    pub fn inherits_class_certain(&self, klass: &str) -> Option<bool> {
        let notes = &self.notes;
        let may_inherit = |kind: &str, loc: Option<&Loc>| match kind {
            "include" | "require" | "include_all" => {
                loc.is_none_or(|l| FileKind::from_path(&l.path) != Some(FileKind::Conf))
            }
            _ => true,
        };
        let unfollowed = notes.references.iter().any(|r| {
            matches!(r.problem, RefProblem::Python | RefProblem::Undefined(_))
                && may_inherit(&r.kind, r.loc.as_ref())
        });
        let uncertain = notes
            .uncertain_references
            .iter()
            .any(|r| may_inherit(&r.kind, r.loc.as_ref()));
        // A class an inherit followed only partly may add (`inherit_defer ${IMGCLASSES}`
        // with `populate_sdk_ext` behind python), or an include followed partly that may
        // read a file holding an inherit.
        let partial = notes.ref_effects.iter().any(|e| {
            e.vars.is_some()
                && match e.kind.as_str() {
                    "include" | "require" | "include_all" => {
                        may_inherit(&e.kind, e.loc.as_ref())
                            && e.not_read
                                .iter()
                                .any(|f| FileKind::from_path(Path::new(f)) != Some(FileKind::Conf))
                    }
                    _ => e.not_read.iter().any(|c| c == klass),
                }
        });
        if unfollowed || uncertain || partial {
            return None;
        }
        let needle = format!("/{klass}.bbclass");
        Some(
            notes
                .inherit_cache
                .iter()
                .any(|p| p.to_string_lossy().ends_with(&needle)),
        )
    }
}

// The verified definitions, copied from OE-core Wrynose `33a6bd6cc8` (and compared with
// `origin/kirkstone` and `origin/scarthgap`, where each is the same text).

/// `meta/classes-global/utils.bbclass` (`meta/classes/utils.bbclass` on kirkstone).
const BASE_SET_FILESPATH: &str = r##"def base_set_filespath(path, d):
    filespath = []
    extrapaths = (d.getVar("FILESEXTRAPATHS") or "")
    # Remove default flag which was used for checking
    extrapaths = extrapaths.replace("__default:", "")
    # Don't prepend empty strings to the path list
    if extrapaths != "":
        path = extrapaths.split(":") + path
    # The ":" ensures we have an 'empty' override
    overrides = (":" + (d.getVar("FILESOVERRIDES") or "")).split(":")
    overrides.reverse()
    for o in overrides:
        for p in path:
            if p != "":
                filespath.append(os.path.join(p, o))
    return ":".join(filespath)"##;

/// `meta/classes-recipe/autotools.bbclass` (`meta/classes/` on kirkstone).
const GET_AUTOTOOLS_DEP: &str = r##"def get_autotools_dep(d):
    if d.getVar('INHIBIT_AUTOTOOLS_DEPS'):
        return ''

    pn = d.getVar('PN')
    deps = ''

    if pn in ['autoconf-native', 'automake-native']:
        return deps
    deps += 'autoconf-native automake-native '

    if not pn in ['libtool', 'libtool-native'] and not pn.endswith("libtool-cross"):
        deps += 'libtool-native '
        if not bb.data.inherits_class('native', d) \
                        and not bb.data.inherits_class('nativesdk', d) \
                        and not bb.data.inherits_class('cross', d) \
                        and not d.getVar('INHIBIT_DEFAULT_DEPS'):
            deps += 'libtool-cross '

    return deps"##;

/// `meta/classes-global/base.bbclass` (`meta/classes/` on kirkstone).
const GET_BASE_DEP: &str = r##"def get_base_dep(d):
    if d.getVar('INHIBIT_DEFAULT_DEPS', False):
        return ""
    return "${BASE_DEFAULT_DEPS}""##;

/// `meta/classes-recipe/gettext.bbclass` (`meta/classes/` on kirkstone).
const GETTEXT_DEPENDENCIES: &str = r##"def gettext_dependencies(d):
    if d.getVar('INHIBIT_DEFAULT_DEPS') and not oe.utils.inherits(d, 'cross-canadian'):
        return ""
    if d.getVar('USE_NLS') == 'no':
        return "gettext-minimal-native"
    return "gettext-native""##;

/// `meta/classes-recipe/gettext.bbclass` (`meta/classes/` on kirkstone).
const GETTEXT_OECONF: &str = r##"def gettext_oeconf(d):
    if d.getVar('USE_NLS') == 'no':
        return '--disable-nls'
    # Remove the NLS bits if USE_NLS is no or INHIBIT_DEFAULT_DEPS is set
    if d.getVar('INHIBIT_DEFAULT_DEPS') and not oe.utils.inherits(d, 'cross-canadian'):
        return '--disable-nls'
    return "--enable-nls""##;

/// `meta/classes-recipe/gnomebase.bbclass` (`meta/classes/` on kirkstone).
const GNOME_VERDIR: &str = r##"def gnome_verdir(v):
    return ".".join(v.split(".")[:-1]) or v"##;

/// `meta/classes-recipe/kernel-arch.bbclass` (Wrynose and scarthgap; kirkstone's `map_kernel_arch` has no `loongarch` line and is not verified).
const MAP_KERNEL_ARCH: &str = r##"def map_kernel_arch(a, d):
    import re

    valid_archs = d.getVar('valid_archs').split()

    if   re.match('(i.86|athlon|x86.64)$', a):  return 'x86'
    elif re.match('arceb$', a):                 return 'arc'
    elif re.match('armeb$', a):                 return 'arm'
    elif re.match('aarch64$', a):               return 'arm64'
    elif re.match('aarch64_be$', a):            return 'arm64'
    elif re.match('aarch64_ilp32$', a):         return 'arm64'
    elif re.match('aarch64_be_ilp32$', a):      return 'arm64'
    elif re.match('loongarch(32|64|)$', a):     return 'loongarch'
    elif re.match('mips(isa|)(32|64|)(r6|)(el|)$', a):      return 'mips'
    elif re.match('mcf', a):                    return 'm68k'
    elif re.match('riscv(32|64|)(eb|)$', a):    return 'riscv'
    elif re.match('p(pc|owerpc)(|64)', a):      return 'powerpc'
    elif re.match('sh(3|4)$', a):               return 'sh'
    elif re.match('bfin', a):                   return 'blackfin'
    elif re.match('microblazee[bl]', a):        return 'microblaze'
    elif a in valid_archs:                      return a
    else:
        if not d.getVar("TARGET_OS").startswith("linux"):
            return a
        bb.error("cannot map '%s' to a linux kernel architecture" % a)"##;

/// `meta/classes-recipe/kernel-arch.bbclass` (Wrynose and scarthgap; kirkstone's `map_kernel_arch` has no `loongarch` line and is not verified).
const MAP_UBOOT_ARCH: &str = r##"def map_uboot_arch(a, d):
    import re

    if   re.match('p(pc|owerpc)(|64)', a): return 'ppc'
    elif re.match('i.86$', a): return 'x86'
    return a"##;

/// Whether `text` (python code read from a file) is `verified`, byte for byte apart from
/// `\r` line ends and trailing blank lines.
pub(crate) fn same_text(text: &str, verified: &str) -> bool {
    normalize(text) == normalize(verified)
}

// The class-extension python whose effects the replay reproduces only when the workspace's
// code is this text (`Replay::class_extend_handlers`, `Replay::anonymous`). Not on
// kirkstone or scarthgap, where the handlers set the mapped values with `d.setVar` instead
// of installing read filters.

/// OE-core Wrynose `33a6bd6cc8` `meta/classes-recipe/native.bbclass`.
pub(crate) const NATIVE_VIRTCLASS_HANDLER: &str = r##"python native_virtclass_handler () {
    import re
    pn = d.getVar("PN")
    if not pn.endswith("-native"):
        return
    bpn = d.getVar("BPN")

    oe.utils.set_class_filter("DISTRO_FEATURES", "DISTRO_FEATURES_NATIVE", "DISTRO_FEATURES_FILTER_NATIVE", d)

    classextend = d.getVar('BBCLASSEXTEND') or ""
    if "native" not in classextend:
        return

    def map_dependencies(varname, d, suffix, selfref=True, regex=False):
        varname = varname + ":" + suffix
        # Handle ${PN}-xxx -> ${BPN}-xxx-native
        if suffix != "${PN}" and "${PN}" in suffix:
            output_varname = varname.replace("${PN}", "${BPN}") + "-native"
            d.renameVar(varname, output_varname)

    d.setVarFilter("DEPENDS", "native_filter(val, '" + pn + "', '" + bpn + "', selfref=False)")

    for varname in ["RDEPENDS", "RRECOMMENDS", "RSUGGESTS", "RPROVIDES", "RREPLACES"]:
        d.setVarFilter(varname, "native_filter(val, '" + pn + "', '" + bpn + "')")

    # We need to handle things like ${@bb.utils.contains('PTEST_ENABLED', '1', '${PN}-ptest', '', d)}
    # and not pass ${PN}-test since in the native case it would be ignored. This does mean we ignore
    # anonymous python derived PACKAGES entries.
    for pkg in re.split(r"\${@(?:{.*?}|.)+?}|\s", d.getVar("PACKAGES", False)):
        if not pkg:
            continue
        map_dependencies("RDEPENDS", d, pkg)
        map_dependencies("RRECOMMENDS", d, pkg)
        map_dependencies("RSUGGESTS", d, pkg)
        map_dependencies("RPROVIDES", d, pkg)
        map_dependencies("RREPLACES", d, pkg)

    d.setVarFilter("PACKAGES", "native_filter(val, '" + pn + "', '" + bpn + "')")
    d.setVarFilter("PACKAGES_DYNAMIC", "native_filter(val, '" + pn + "', '" + bpn + "', regex=True)")

    d.setVarFilter("PROVIDES", "native_filter(val, '" + pn + "', '" + bpn + "')")
}"##;

/// OE-core Wrynose `33a6bd6cc8` `meta/classes-recipe/nativesdk.bbclass`.
pub(crate) const NATIVESDK_VIRTCLASS_HANDLER: &str = r##"python nativesdk_virtclass_handler () {
    pn = e.data.getVar("PN")
    if not (pn.endswith("-nativesdk") or pn.startswith("nativesdk-")):
        return

    oe.utils.set_class_filter("DISTRO_FEATURES", "DISTRO_FEATURES_NATIVESDK", "DISTRO_FEATURES_FILTER_NATIVESDK", d)

    e.data.setVar("MLPREFIX", "nativesdk-")
    e.data.setVar("PN", "nativesdk-" + e.data.getVar("PN").replace("-nativesdk", "").replace("nativesdk-", ""))
}"##;

/// OE-core Wrynose `33a6bd6cc8` `meta/classes-recipe/nativesdk.bbclass`.
pub(crate) const NATIVESDK_ANONYMOUS: &str = r##"python () {
    pn = d.getVar("PN")
    if not pn.startswith("nativesdk-"):
        return

    import oe.classextend

    clsextend = oe.classextend.ClassExtender("nativesdk", [], d)
    clsextend.rename_package_variables((d.getVar("PACKAGEVARS") or "").split())

    clsextend.set_filter("DEPENDS", deps=True)
    clsextend.set_filter("PACKAGE_WRITE_DEPS", deps=False)
    clsextend.set_filter("PROVIDES", deps=False)

    if "nativesdk" in (d.getVar("BBCLASSEXTEND") or ""):
        clsextend.map_packagevars()

    d.setVar("LIBCEXTENSION", "")
    d.setVar("ABIEXTENSION", "")
}"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for f in ALL {
            assert_eq!(ClassFn::from_name(f.name()), Some(*f));
            assert!(f.verified()[0].starts_with(&format!("def {}(", f.name())));
        }
        assert_eq!(ClassFn::from_name("meson_array"), None);
    }

    #[test]
    fn only_the_verified_text_is_accepted() {
        let text = format!("{GNOME_VERDIR}\n\n   \n");
        assert_eq!(verify("gnome_verdir", &text), Some(ClassFn::GnomeVerdir));
        assert_eq!(
            verify("gnome_verdir", &GNOME_VERDIR.replace('\n', "\r\n")),
            Some(ClassFn::GnomeVerdir)
        );
        // OE-core's gcr and libgudev recipes redefine it.
        assert_eq!(
            verify(
                "gnome_verdir",
                "def gnome_verdir(v):\n    return \".\".join(v.split(\".\")[:2]) or v"
            ),
            None
        );
        assert_eq!(
            verify("gnome_verdir", "def gnome_verdir(v):\n    return v"),
            None
        );
        // The right text under another name.
        assert_eq!(verify("get_base_dep", GNOME_VERDIR), None);
        assert_eq!(verify("meson_array", GNOME_VERDIR), None);
    }
}
