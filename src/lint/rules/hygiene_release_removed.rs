//! `hygiene/release-removed`: a variable, class, value or construct that a release the file
//! is checked against removed, renamed or deprecated.
//!
//! See docs/lint-rules.md and "Releases" in docs/bitbake-semantics.md. Data-driven: each
//! row of [`REMOVALS`] names what to look for, the `Feature` that says which releases still
//! have it (a `KeptUntil*` feature; `None`: no supported release has it, the rename is older
//! than the floor), what happens there and what to write instead. Every row comes from a
//! Yocto migration guide (4.1 to 6.0), checked against OE-core's or BitBake's source on the
//! release branches (the "Releases" table names each source). A finding names the releases
//! among [`LintContext::releases`] that lack the feature. No codename is compared here.
//!
//! The kinds of row:
//!
//! - [`What::RenamedVariable`] (error): a name in OE-core's `BB_RENAMED_VARIABLES`
//!   (`meta/conf/bitbake.conf`) or BitBake's `bitbake_renamed_vars` (`lib/bb/data_smart.py`).
//!   Setting it, a flag included, makes BitBake stop ("Variable X has been renamed to Y"):
//!   `DataSmart.setVar` / `setVarFlag` mark the parse failed. Any assignment to it is
//!   reported.
//! - [`What::RemovedVariable`] (warning): OE-core no longer sets or reads it. An assignment
//!   (no flag) has no effect; a `${NAME}` reference stays unexpanded unless the file itself,
//!   or a layer other than the core layer, assigns it.
//! - [`What::RemovedClass`] (error): `inherit NAME` (or `inherit_defer`), or NAME as a word of
//!   `INHERIT`, `USER_CLASSES`, `IMAGE_CLASSES`, `KERNEL_CLASSES` or `PACKAGE_CLASSES`:
//!   BitBake stops ("Could not inherit file classes/NAME.bbclass"). Not reported when a layer
//!   other than the core layer has its own class of that name.
//! - [`What::RemovedWord`]: NAME as a word of the value of the given variables (any
//!   override; `:remove` operations excepted).
//! - [`What::RenamedText`] (warning): NAME as text of an assignment or function, ending at a
//!   word boundary: the old virtual toolchain provider names.
//! - [`What::RemovedFetcher`] (error): a `SRC_URI` entry `NAME://...`.
//!
//! And four checks that are not a name in a table:
//!
//! - `S` below `${WORKDIR}` (`Feature::KeptUntilWalnascar`, error): OE-core's
//!   `do_qa_unpack` (`insane.bbclass`, `46480a5e66`, Whinlatter) stops on an `S` whose
//!   unexpanded value contains `${WORKDIR}`, and on `S = "${WORKDIR}/git"` or
//!   `S = "${UNPACKDIR}/git"`. `S = "${WORKDIR}"` itself is `hygiene/workdir-s`. Only an
//!   unconditional `S =` / `?=` / `??=` / `:=` outside `.conf` files (the default in
//!   `bitbake.conf` comes with the core layer of the release).
//! - `LAYERSERIES_COMPAT_<c> = "${LAYERSERIES_CORENAMES}"` (`Feature::KeptUntilLangdale`,
//!   error): BitBake 2.4 deletes `LAYERSERIES_CORENAMES` after the core layer's
//!   `layer.conf`, so the reference stays literal (4.2 guide, "LAYERSERIES_COMPAT for custom
//!   layers"). Not in the file that sets `LAYERSERIES_CORENAMES`.
//! - an obsolete license name in `LICENSE` (OE-core's `obsolete_license_list`, the same on
//!   every supported release): the `obsolete-license` QA check is a warning up to Scarthgap
//!   and an error (`ERROR_QA`) from Styhead (`Feature::KeptUntilScarthgap`).
//! - an assignment without whitespace around its operator (`FOO="bar"`,
//!   `Feature::AssignmentWhitespaceWarning`, warning): BitBake 2.16 `ConfHandler.feeder`
//!   warns "has a lack of whitespace around the assignment" (`24772dd2a`, 5.3 guide).

use crate::index::query;
use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{Statement, StatementKind, words};
use crate::release::{Feature, Release};
use crate::types::{AssignOp, FileKind, LineSpan};
use crate::workspace::resolve;

pub struct HygieneReleaseRemoved;

/// What a row of [`REMOVALS`] looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    RenamedVariable,
    RemovedVariable,
    RemovedClass,
    /// A word of the values of these variables.
    RemovedWord(&'static [&'static str]),
    RenamedText,
    RemovedFetcher,
}

/// One removed or renamed thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Removal {
    pub what: What,
    pub name: &'static str,
    /// The releases that still have it; `None`: no supported release does.
    pub feature: Option<Feature>,
    pub severity: Severity,
    /// What to write instead (one clause, no final period).
    pub instead: &'static str,
}

const fn row(
    what: What,
    name: &'static str,
    feature: Option<Feature>,
    severity: Severity,
    instead: &'static str,
) -> Removal {
    Removal {
        what,
        name,
        feature,
        severity,
        instead,
    }
}

use Severity::{Error as E, Warning as W};
use What::*;

const K0: Option<Feature> = None;
const K40: Option<Feature> = Some(Feature::KeptUntilKirkstone);
const K41: Option<Feature> = Some(Feature::KeptUntilLangdale);
const K42: Option<Feature> = Some(Feature::KeptUntilMickledore);
const K43: Option<Feature> = Some(Feature::KeptUntilNanbield);
const K50: Option<Feature> = Some(Feature::KeptUntilScarthgap);
const K51: Option<Feature> = Some(Feature::KeptUntilStyhead);
const K52: Option<Feature> = Some(Feature::KeptUntilWalnascar);
const K53: Option<Feature> = Some(Feature::KeptUntilWhinlatter);

const IMAGE_FEATURES: &[&str] = &["IMAGE_FEATURES", "EXTRA_IMAGE_FEATURES"];
const QA_LISTS: &[&str] = &["ERROR_QA", "WARN_QA", "INSANE_SKIP"];
const SIGNATURE_HANDLER: &[&str] = &["BB_SIGNATURE_HANDLER"];

/// The table. Sources per row group in docs/bitbake-semantics.md ("Releases").
#[rustfmt::skip]
pub const REMOVALS: &[Removal] = &[
    // BitBake `bitbake_renamed_vars` (identical on branches 2.0 to 2.18).
    row(RenamedVariable, "BB_ENV_WHITELIST", K0, E, "write `BB_ENV_PASSTHROUGH`"),
    row(RenamedVariable, "BB_ENV_EXTRAWHITE", K0, E, "write `BB_ENV_PASSTHROUGH_ADDITIONS`"),
    row(RenamedVariable, "BB_HASHBASE_WHITELIST", K0, E, "write `BB_BASEHASH_IGNORE_VARS`"),
    row(RenamedVariable, "BB_HASHCONFIG_WHITELIST", K0, E, "write `BB_HASHCONFIG_IGNORE_VARS`"),
    row(RenamedVariable, "BB_HASHTASK_WHITELIST", K0, E, "write `BB_TASKHASH_IGNORE_TASKS`"),
    row(RenamedVariable, "BB_SETSCENE_ENFORCE_WHITELIST", K0, E, "write `BB_SETSCENE_ENFORCE_IGNORE_TASKS`"),
    row(RenamedVariable, "MULTI_PROVIDER_WHITELIST", K0, E, "write `BB_MULTI_PROVIDER_ALLOWED`"),
    row(RenamedVariable, "BB_STAMP_WHITELIST", K0, E, "remove it (support has been removed)"),
    row(RenamedVariable, "BB_STAMP_POLICY", K0, E, "remove it (support has been removed)"),
    // OE-core `BB_RENAMED_VARIABLES` on the kirkstone branch (and every later one).
    row(RenamedVariable, "PNBLACKLIST", K0, E, "write `SKIP_RECIPE`"),
    row(RenamedVariable, "CVE_CHECK_PN_WHITELIST", K0, E, "write `CVE_CHECK_SKIP_RECIPE`"),
    row(RenamedVariable, "CVE_CHECK_WHITELIST", K0, E, "write `CVE_CHECK_IGNORE`"),
    row(RenamedVariable, "SDK_LOCAL_CONF_BLACKLIST", K0, E, "write `ESDK_LOCALCONF_REMOVE`"),
    row(RenamedVariable, "SDK_LOCAL_CONF_WHITELIST", K0, E, "write `ESDK_LOCALCONF_ALLOW`"),
    row(RenamedVariable, "SDK_INHERIT_BLACKLIST", K0, E, "write `ESDK_CLASS_INHERIT_DISABLE`"),
    row(RenamedVariable, "SSTATE_DUPWHITELIST", K0, E, "write `SSTATE_ALLOW_OVERLAP_FILES`"),
    row(RenamedVariable, "SYSROOT_DIRS_BLACKLIST", K0, E, "write `SYSROOT_DIRS_IGNORE`"),
    row(RenamedVariable, "UNKNOWN_CONFIGURE_WHITELIST", K0, E, "write `UNKNOWN_CONFIGURE_OPT_IGNORE`"),
    row(RenamedVariable, "ICECC_USER_CLASS_BL", K0, E, "write `ICECC_CLASS_DISABLE`"),
    row(RenamedVariable, "ICECC_SYSTEM_CLASS_BL", K0, E, "write `ICECC_CLASS_DISABLE`"),
    row(RenamedVariable, "ICECC_USER_PACKAGE_WL", K0, E, "write `ICECC_RECIPE_ENABLE`"),
    row(RenamedVariable, "ICECC_USER_PACKAGE_BL", K0, E, "write `ICECC_RECIPE_DISABLE`"),
    row(RenamedVariable, "ICECC_SYSTEM_PACKAGE_BL", K0, E, "write `ICECC_RECIPE_DISABLE`"),
    row(RenamedVariable, "INHERIT_BLACKLIST", K0, E, "remove it (no longer needed)"),
    row(RenamedVariable, "TUNEABI_WHITELIST", K0, E, "remove it (support has been removed)"),
    row(RenamedVariable, "LICENSE_FLAGS_WHITELIST", K0, E, "write `LICENSE_FLAGS_ACCEPTED`"),
    row(RenamedVariable, "WHITELIST_GPL-3.0-only", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS`"),
    row(RenamedVariable, "WHITELIST_GPL-3.0-or-later", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS`"),
    row(RenamedVariable, "WHITELIST_LGPL-3.0-only", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS`"),
    row(RenamedVariable, "WHITELIST_LGPL-3.0-or-later", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS`"),
    row(RenamedVariable, "WHITELIST_GPL-3.0", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS = \"<pkg>:GPL-3.0-only\"`"),
    row(RenamedVariable, "WHITELIST_GPL-3.0+", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS = \"<pkg>:GPL-3.0-or-later\"`"),
    row(RenamedVariable, "WHITELIST_LGPL-3.0", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS = \"<pkg>:LGPL-3.0-only\"`"),
    row(RenamedVariable, "WHITELIST_LGPL-3.0+", K0, E, "write `INCOMPATIBLE_LICENSE_EXCEPTIONS = \"<pkg>:LGPL-3.0-or-later\"`"),
    // 4.1 (Langdale), "Miscellaneous changes".
    row(RemovedVariable, "PEP517_BUILD_API", K40, W, "remove it (no longer supported)"),
    // 4.2 (Mickledore), "Removed variables" and "Removed classes".
    row(RemovedVariable, "SERIAL_CONSOLE", K41, W, "write `SERIAL_CONSOLES`"),
    row(RemovedVariable, "PACKAGEBUILDPKGD", K41, W, "use `PACKAGE_PREPROCESS_FUNCS` or `PACKAGESPLITFUNCS`"),
    row(RemovedClass, "rust-bin", K41, E, "build the Rust code with the `cargo` class"),
    row(RemovedClass, "package_tar", K41, E, "use `package_rpm`, `package_deb` or `package_ipk`"),
    row(RemovedWord(SIGNATURE_HANDLER), "OEBasic", K41, W, "remove the setting (the default `OEBasicHash` or `OEEquivHash` is used)"),
    // 4.3 (Nanbield), "Removed variables" and "Removed classes".
    row(RemovedVariable, "PERLARCH", K42, W, "remove it"),
    row(RemovedVariable, "PERLVERSION", K42, W, "remove it"),
    row(RemovedVariable, "QEMU_USE_SLIRP", K42, W, "add `slirp` to `TEST_RUNQEMUPARAMS` instead"),
    row(RemovedVariable, "SERIAL_CONSOLES_CHECK", K42, W, "use `SERIAL_CONSOLES` (every console there is checked)"),
    row(RemovedClass, "glide", K42, E, "use Go modules (Glide support was removed)"),
    // 5.0 (Scarthgap), "Removed variables".
    row(RemovedVariable, "DEPLOY_DIR_TAR", K43, W, "remove it (the package_tar class is gone)"),
    row(RemovedVariable, "oldincludedir", K43, W, "use `includedir`"),
    // 5.1 (Styhead), "Removed variables", "Removed classes", "Miscellaneous changes".
    row(RenamedVariable, "TCLIBCAPPEND", K50, E, "remove it (modify `TMPDIR` directly if required)"),
    row(RenamedVariable, "VOLATILE_LOG_DIR", K50, E, "remove it (`FILESYSTEM_PERMS_TABLES` is used instead)"),
    row(RenamedVariable, "VOLATILE_TMP_DIR", K50, E, "remove it (`FILESYSTEM_PERMS_TABLES` is used instead)"),
    row(RemovedClass, "siteconfig", K50, E, "remove the inherit"),
    row(RemovedWord(QA_LISTS), "license-incompatible", K50, W, "write `license-exception` (the check was renamed)"),
    // 5.2 (Walnascar), "Removed variables", "Removed classes", debug-tweaks, virtual providers.
    row(RenamedVariable, "BB_DANGLINGAPPENDS_WARNONLY", K51, E, "remove it (support was removed)"),
    row(RemovedVariable, "PACKAGE_SNAP_LIB_SYMLINKS", K51, W, "remove it"),
    row(RemovedVariable, "ACLOCALDIR", K51, W, "for m4 files autoreconf does not find, add `EXTRA_AUTORECONF += \"-I path/to/m4\"`"),
    row(RemovedVariable, "ACLOCALEXTRAPATH", K51, W, "for m4 files autoreconf does not find, add `EXTRA_AUTORECONF += \"-I path/to/m4\"`"),
    row(RemovedClass, "migrate_localcount", K51, E, "remove the inherit"),
    row(RemovedWord(IMAGE_FEATURES), "debug-tweaks", K51, E, "write the features it stood for: `allow-empty-password allow-root-login empty-root-password post-install-logging`"),
    row(RenamedText, "virtual/${TARGET_PREFIX}gcc", K51, W, "write `virtual/cross-cc`"),
    row(RenamedText, "virtual/${HOST_PREFIX}gcc", K51, W, "write `virtual/cross-cc`"),
    row(RenamedText, "virtual/${TARGET_PREFIX}g++", K51, W, "write `virtual/cross-c++`"),
    row(RenamedText, "virtual/${HOST_PREFIX}g++", K51, W, "write `virtual/cross-c++`"),
    row(RenamedText, "virtual/${TARGET_PREFIX}binutils", K51, W, "write `virtual/cross-binutils`"),
    row(RenamedText, "virtual/${HOST_PREFIX}binutils", K51, W, "write `virtual/cross-binutils`"),
    row(RenamedText, "virtual/${SDK_PREFIX}binutils", K51, W, "write `virtual/cross-binutils` (as a PREFERRED_PROVIDER name, `PREFERRED_PROVIDER_virtual/cross-binutils:class-nativesdk`)"),
    row(RenamedText, "virtual/${SDK_PREFIX}gcc", K51, W, "write `virtual/nativesdk-cross-cc`"),
    row(RenamedText, "virtual/${SDK_PREFIX}g++", K51, W, "write `virtual/nativesdk-cross-c++`"),
    row(RenamedText, "virtual/${TARGET_PREFIX}compilerlibs", K51, W, "write `virtual/compilerlibs`"),
    row(RenamedText, "virtual/${HOST_PREFIX}compilerlibs", K51, W, "write `virtual/compilerlibs`"),
    row(RenamedText, "virtual/nativesdk-${SDK_PREFIX}compilerlibs", K51, W, "write `virtual/nativesdk-compilerlibs`"),
    // 5.3 (Whinlatter), "Removed variables", "Removed classes".
    row(RemovedVariable, "GPE_MIRROR", K52, W, "remove it"),
    row(RemovedVariable, "BUILDHISTORY_RESET", K52, W, "remove it"),
    row(RemovedClass, "kernel-fitimage", K52, E, "use the `kernel-fit-image` class and a FIT image recipe (5.3 guide, \"Removed classes\")"),
    row(RemovedClass, "icecc", K52, E, "remove the inherit"),
    // 6.0 (Wrynose): feature backfill, cve-check, SPDX 2.2, oelint, fetchers.
    row(RenamedVariable, "DISTRO_FEATURES_DEFAULT", K53, E, "add the default features to `DISTRO_FEATURES_DEFAULTS`"),
    row(RenamedVariable, "DISTRO_FEATURES_BACKFILL", K53, E, "add the default features to `DISTRO_FEATURES_DEFAULTS`"),
    row(RenamedVariable, "DISTRO_FEATURES_BACKFILL_CONSIDERED", K53, E, "write `DISTRO_FEATURES_OPTED_OUT`"),
    row(RenamedVariable, "MACHINE_FEATURES_DEFAULT", K53, E, "add the default features to `MACHINE_FEATURES_DEFAULTS`"),
    row(RenamedVariable, "MACHINE_FEATURES_BACKFILL", K53, E, "add the default features to `MACHINE_FEATURES_DEFAULTS`"),
    row(RenamedVariable, "MACHINE_FEATURES_BACKFILL_CONSIDERED", K53, E, "write `MACHINE_FEATURES_OPTED_OUT`"),
    row(RemovedVariable, "CVE_CHECK_SHOW_WARNINGS", K53, W, "write `SBOM_CVE_CHECK_SHOW_WARNINGS`"),
    row(RemovedClass, "create-spdx-2.2", K53, E, "use the `create-spdx` class"),
    row(RemovedClass, "cve-check", K53, E, "replace `INHERIT += \"cve-check\"` with `OE_FRAGMENTS += \"core/yocto/sbom-cve-check\"`"),
    row(RemovedClass, "oelint", K53, E, "remove the inherit"),
    row(RemovedFetcher, "bzr", K53, E, "fetch the sources another way (a release tarball with a checksum, or git)"),
    row(RemovedFetcher, "osc", K53, E, "fetch the sources another way (a release tarball with a checksum, or git)"),
    row(RemovedFetcher, "cvs", K53, E, "fetch the sources another way (a release tarball with a checksum, or git)"),
    row(RemovedFetcher, "npm", K53, E, "fetch the sources another way (the npm fetchers were disabled for security reasons)"),
    row(RemovedFetcher, "npmsw", K53, E, "fetch the sources another way (the npm fetchers were disabled for security reasons)"),
];

/// OE-core's `obsolete_license_list()` (`meta/lib/oe/license.py`, identical on the kirkstone
/// and wrynose branches).
#[rustfmt::skip]
pub const OBSOLETE_LICENSES: &[&str] = &[
    "AGPL-3", "AGPL-3+", "AGPLv3", "AGPLv3+", "AGPLv3.0", "AGPLv3.0+", "AGPL-3.0", "AGPL-3.0+",
    "BSD-0-Clause", "GPL-1", "GPL-1+", "GPLv1", "GPLv1+", "GPLv1.0", "GPLv1.0+", "GPL-1.0",
    "GPL-1.0+", "GPL-2", "GPL-2+", "GPLv2", "GPLv2+", "GPLv2.0", "GPLv2.0+", "GPL-2.0",
    "GPL-2.0+", "GPL-3", "GPL-3+", "GPLv3", "GPLv3+", "GPLv3.0", "GPLv3.0+", "GPL-3.0",
    "GPL-3.0+", "LGPLv2", "LGPLv2+", "LGPLv2.0", "LGPLv2.0+", "LGPL-2.0", "LGPL-2.0+",
    "LGPL2.1", "LGPL2.1+", "LGPLv2.1", "LGPLv2.1+", "LGPL-2.1", "LGPL-2.1+", "LGPLv3",
    "LGPLv3+", "LGPL-3.0", "LGPL-3.0+", "MPL-1", "MPLv1", "MPLv1.1", "MPLv2", "MIT-X",
    "MIT-style", "openssl", "PSF", "PSFv2", "Python-2", "Apachev2", "Apache-2", "Artisticv1",
    "Artistic-1", "AFL-2", "AFL-1", "AFLv2", "AFLv1", "CDDLv1", "CDDL-1", "EPLv1.0",
    "FreeType", "Nauman", "tcl", "vim", "SGIv1",
];

/// The SPDX names OE-core's `SPDXLICENSEMAP` (`meta/conf/licenses.conf`) gives the common
/// obsolete names.
const SPDX_NAMES: &[(&str, &str)] = &[
    ("AGPL-3.0", "AGPL-3.0-only"),
    ("GPLv2", "GPL-2.0-only"),
    ("GPLv2+", "GPL-2.0-or-later"),
    ("GPL-2.0", "GPL-2.0-only"),
    ("GPL-2.0+", "GPL-2.0-or-later"),
    ("GPLv3", "GPL-3.0-only"),
    ("GPLv3+", "GPL-3.0-or-later"),
    ("GPL-3.0", "GPL-3.0-only"),
    ("GPL-3.0+", "GPL-3.0-or-later"),
    ("LGPLv2", "LGPL-2.0-only"),
    ("LGPLv2+", "LGPL-2.0-or-later"),
    ("LGPLv2.1", "LGPL-2.1-only"),
    ("LGPLv2.1+", "LGPL-2.1-or-later"),
    ("LGPL-2.1", "LGPL-2.1-only"),
    ("LGPL-2.1+", "LGPL-2.1-or-later"),
    ("LGPLv3", "LGPL-3.0-only"),
    ("LGPLv3+", "LGPL-3.0-or-later"),
    ("MPLv2", "MPL-2.0"),
    ("PSFv2", "PSF-2.0"),
    ("Apachev2", "Apache-2.0"),
    ("Apache-2", "Apache-2.0"),
];

/// The variables whose words OE-core inherits as classes.
const CLASS_LISTS: &[&str] = &[
    "INHERIT",
    "USER_CLASSES",
    "IMAGE_CLASSES",
    "KERNEL_CLASSES",
    "PACKAGE_CLASSES",
];

/// The releases of the context without `feature` (every release for `None`).
fn lacking(cx: &LintContext<'_>, feature: Option<Feature>) -> Vec<Release> {
    match feature {
        Some(f) => helpers::releases_lacking(cx, f),
        None => cx.releases.to_vec(),
    }
}

/// "from styhead on", or "in every supported release" for a rename older than the floor.
fn since(feature: Option<Feature>) -> String {
    match feature.and_then(Feature::removed_in) {
        Some(r) => format!("from {} on", r.codename()),
        None => "in every supported release".to_string(),
    }
}

/// The layer names of the workspace whose collections include `core`.
fn core_layers(cx: &LintContext<'_>) -> Vec<String> {
    cx.ws
        .layers
        .iter()
        .filter(|l| l.collections.iter().any(|c| c == "core"))
        .map(|l| l.name.clone())
        .collect()
}

/// Whether a layer other than the core layer assigns `name`. An index error counts as yes
/// (stay silent when it cannot tell).
fn assigned_outside_core(cx: &LintContext<'_>, name: &str) -> bool {
    let filter = query::StatementFilter {
        kinds: vec!["assign".into()],
        ..query::StatementFilter::default()
    };
    let core = core_layers(cx);
    match query::statements_by_base(cx.index, name, &filter) {
        Ok(rows) => rows.iter().any(|r| !core.contains(&r.loc.layer)),
        Err(_) => true,
    }
}

/// Whether a layer other than the core layer has its own class `name`.
fn class_outside_core(cx: &LintContext<'_>, name: &str) -> bool {
    let core = core_layers(cx);
    resolve::class_candidates(cx.ws, name, false)
        .into_iter()
        .chain(resolve::class_candidates(cx.ws, name, true))
        .filter(|p| p.is_file())
        .any(|p| {
            cx.ws
                .loc_for(&p, LineSpan::line(1))
                .is_none_or(|l| !core.contains(&l.layer))
        })
}

/// Whether `text` holds `needle` followed by a word boundary (not a name character).
fn contains_word(text: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = text[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        let next = text[end..].chars().next();
        if next.is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.'))) {
            return Some(start);
        }
        from = end;
    }
    None
}

/// The 1-based line of byte offset `at` of a statement's text.
fn line_at(stmt: &Statement, text: &str, at: usize) -> u32 {
    let n = text[..at.min(text.len())].matches('\n').count();
    stmt.span.start + u32::try_from(n).unwrap_or(0)
}

/// Whether the operator of an assignment line lacks whitespace on either side, as BitBake
/// 2.16's `__config_regexp__` reads it: `(export\s+)?`, a lazy variable name of
/// `[a-zA-Z0-9\-_+.${}/~:]`, an optional `[flag]`, `\s*`, the operator (`:=`, `??=`, `?=`,
/// `+=`, `=+`, `=.`, `.=`, `=`, tried in that order), `\s*`, then a quote.
pub fn lacks_assignment_whitespace(line: &str) -> bool {
    let mut s = line.trim_start();
    if let Some(rest) = s.strip_prefix("export")
        && rest.starts_with(char::is_whitespace)
    {
        s = rest.trim_start();
    }
    let name_char = |c: char| c.is_ascii_alphanumeric() || "-_+.${}/~:".contains(c);
    const OPS: [&str; 8] = [":=", "??=", "?=", "+=", "=+", "=.", ".=", "="];
    let bytes: Vec<(usize, char)> = s.char_indices().collect();
    for i in 0..=bytes.len() {
        if i > 0 && !name_char(bytes[i - 1].1) {
            break;
        }
        let at = bytes.get(i).map_or(s.len(), |(b, _)| *b);
        if at == 0 {
            continue;
        }
        let mut rest = &s[at..];
        if let Some(r) = rest.strip_prefix('[')
            && let Some(close) = r.find(']')
        {
            rest = &r[close + 1..];
        }
        let ws1 = rest.len() - rest.trim_start().len();
        rest = rest.trim_start();
        let Some(op) = OPS.iter().find(|o| rest.starts_with(**o)) else {
            continue;
        };
        rest = &rest[op.len()..];
        let ws2 = rest.len() - rest.trim_start().len();
        rest = rest.trim_start();
        if !rest.starts_with(['"', '\'']) {
            continue;
        }
        return ws1 == 0 || ws2 == 0;
    }
    false
}

/// The words of a `LICENSE` value (names between `&`, `|`, `(`, `)` and whitespace).
fn license_words(value: &str) -> Vec<&str> {
    value
        .split(|c: char| c.is_whitespace() || matches!(c, '&' | '|' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect()
}

fn is_set(op: &AssignOp) -> bool {
    matches!(
        op,
        AssignOp::Set | AssignOp::Default | AssignOp::WeakDefault | AssignOp::Immediate
    )
}

impl HygieneReleaseRemoved {
    fn push(
        &self,
        cx: &LintContext<'_>,
        out: &mut Sink,
        line: u32,
        severity: Severity,
        message: String,
    ) {
        let mut f = helpers::finding(self, cx, LineSpan::line(line), message);
        f.severity = severity;
        out.findings.push(f);
        if let Some((feature, what, instead)) = out.pending.take() {
            out.uses.push(RemovalUse {
                feature,
                line,
                what,
                instead,
            });
        }
    }

    fn table(&self, cx: &LintContext<'_>, out: &mut Sink) {
        let Some(file) = cx.file else { return };
        for r in REMOVALS {
            let lack = lacking(cx, r.feature);
            if lack.is_empty() {
                continue;
            }
            let when = since(r.feature);
            let releases = helpers::release_list(&lack);
            for s in &file.statements {
                let line = s.span.start;
                match (r.what, &s.kind) {
                    (RenamedVariable, StatementKind::Assign { var, .. }) if var.base == r.name => {
                        out.tag(r.feature, r.name, r.instead);
                        self.push(
                            cx,
                            out,
                            line,
                            r.severity,
                            format!(
                                "`{}` is a renamed variable {when}: setting it makes BitBake stop \
                                 (\"Variable {} has been renamed\"), and this layer is checked \
                                 against {releases}; {}",
                                r.name, r.name, r.instead
                            ),
                        );
                    }
                    (RemovedVariable, StatementKind::Assign { var, .. })
                        if var.base == r.name && var.flag.is_none() =>
                    {
                        out.tag(r.feature, r.name, r.instead);
                        self.push(
                            cx,
                            out,
                            line,
                            r.severity,
                            format!(
                                "`{}`: OE-core no longer reads it {when}, so setting it has \
                                 no effect there, and this layer is checked against {releases}; {}",
                                r.name, r.instead
                            ),
                        );
                    }
                    (
                        RemovedVariable,
                        StatementKind::Assign { .. }
                        | StatementKind::Function { .. }
                        | StatementKind::PyDef { .. },
                    ) => {
                        let text = helpers::statement_text(cx, s);
                        let needle = format!("${{{}}}", r.name);
                        let Some(at) = text.find(&needle) else {
                            continue;
                        };
                        let own = file.statements.iter().any(|o| {
                            matches!(&o.kind, StatementKind::Assign { var, .. } if var.base == r.name)
                        });
                        if own || assigned_outside_core(cx, r.name) {
                            continue;
                        }
                        out.tag(r.feature, r.name, r.instead);
                        self.push(
                            cx,
                            out,
                            line_at(s, text, at),
                            r.severity,
                            format!(
                                "`{needle}`: OE-core no longer sets `{}` {when}, so the reference \
                                 stays unexpanded there, and this layer is checked against \
                                 {releases}; {}",
                                r.name, r.instead
                            ),
                        );
                    }
                    (RemovedClass, StatementKind::Inherit { classes, .. }) => {
                        if words::words(classes).contains(&r.name)
                            && !class_outside_core(cx, r.name)
                        {
                            out.tag(r.feature, r.name, r.instead);
                            self.push(
                                cx,
                                out,
                                line,
                                r.severity,
                                class_message(r, &when, &releases),
                            );
                        }
                    }
                    (RemovedClass, StatementKind::Assign { var, value, .. })
                        if CLASS_LISTS.contains(&var.base.as_str())
                            && var.flag.is_none()
                            && !var.overrides.iter().any(|o| o == "remove") =>
                    {
                        if words::words(value).contains(&r.name) && !class_outside_core(cx, r.name)
                        {
                            out.tag(r.feature, r.name, r.instead);
                            self.push(
                                cx,
                                out,
                                line,
                                r.severity,
                                class_message(r, &when, &releases),
                            );
                        }
                    }
                    (RemovedWord(vars), StatementKind::Assign { var, value, .. })
                        if vars.contains(&var.base.as_str())
                            && var.flag.is_none()
                            && !var.overrides.iter().any(|o| o == "remove") =>
                    {
                        if words::words(value).contains(&r.name) {
                            let effect = if r.severity == Severity::Error {
                                "the recipe is skipped there (\"is not a valid image feature\")"
                            } else {
                                "it has no effect there"
                            };
                            out.tag(r.feature, r.name, r.instead);
                            self.push(
                                cx,
                                out,
                                line,
                                r.severity,
                                format!(
                                    "`{}` in {}: no longer a valid value {when}, so {effect}, and \
                                     this layer is checked against {releases}; {}",
                                    r.name, var.base, r.instead
                                ),
                            );
                        }
                    }
                    (
                        RenamedText,
                        StatementKind::Assign { .. }
                        | StatementKind::Function { .. }
                        | StatementKind::PyDef { .. },
                    ) => {
                        let text = helpers::statement_text(cx, s);
                        if let Some(at) = contains_word(text, r.name) {
                            out.tag(r.feature, r.name, r.instead);
                            self.push(
                                cx,
                                out,
                                line_at(s, text, at),
                                r.severity,
                                format!(
                                    "`{}`: OE-core no longer provides or asks for this toolchain \
                                     provider name {when}, and this layer is checked against \
                                     {releases}; {}",
                                    r.name, r.instead
                                ),
                            );
                        }
                    }
                    (RemovedFetcher, StatementKind::Assign { var, value, .. })
                        if var.base == "SRC_URI" && var.flag.is_none() =>
                    {
                        let prefix = format!("{}://", r.name);
                        if words::words(value).iter().any(|w| w.starts_with(&prefix)) {
                            out.tag(r.feature, r.name, r.instead);
                            self.push(
                                cx,
                                out,
                                line,
                                r.severity,
                                format!(
                                    "`{prefix}` in SRC_URI: BitBake has no usable {} fetcher \
                                     {when}, and this layer is checked against {releases}; {}",
                                    r.name, r.instead
                                ),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// `S` below `${WORKDIR}`, and `S = "${UNPACKDIR}/git"`.
    fn source_dir(&self, cx: &LintContext<'_>, out: &mut Sink) {
        let Some(file) = cx.file else { return };
        // `bitbake.conf`'s default `S` belongs to the core layer of the release: a migration
        // replaces it with the new release's.
        if cx.kind == Some(FileKind::Conf) {
            return;
        }
        let lack = helpers::releases_lacking(cx, Feature::KeptUntilWalnascar);
        if lack.is_empty() {
            return;
        }
        let when = since(Some(Feature::KeptUntilWalnascar));
        for s in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            if var.base != "S" || var.flag.is_some() || !var.overrides.is_empty() || !is_set(op) {
                continue;
            }
            let v = value.trim();
            let instead = if v == "${WORKDIR}/git" || v == "${UNPACKDIR}/git" {
                "remove the assignment: the git fetcher unpacks into `${UNPACKDIR}/${BP}` there, \
                 which the default S names"
                    .to_string()
            } else if v.contains("${WORKDIR}") && v != "${WORKDIR}" && v != "${WORKDIR}/" {
                format!(
                    "write `S = \"{}\"`",
                    v.replace("${WORKDIR}", "${UNPACKDIR}")
                )
            } else {
                continue;
            };
            out.tag(
                Some(Feature::KeptUntilWalnascar),
                "S below ${WORKDIR}",
                &instead,
            );
            self.push(
                cx,
                out,
                s.span.start,
                Severity::Error,
                format!(
                    "`S = \"{v}\"`: {when} OE-core's do_qa_unpack stops on it, and this layer is \
                     checked against {}; {instead}",
                    helpers::release_list(&lack)
                ),
            );
        }
    }

    /// `LAYERSERIES_COMPAT_<c> = "${LAYERSERIES_CORENAMES}"`.
    fn compat_corenames(&self, cx: &LintContext<'_>, out: &mut Sink) {
        let Some(file) = cx.file else { return };
        if cx.kind != Some(FileKind::Conf) {
            return;
        }
        let lack = helpers::releases_lacking(cx, Feature::KeptUntilLangdale);
        if lack.is_empty() {
            return;
        }
        let sets_corenames = file.statements.iter().any(|s| {
            matches!(&s.kind, StatementKind::Assign { var, .. } if var.base == "LAYERSERIES_CORENAMES")
        });
        if sets_corenames {
            return;
        }
        for s in &file.statements {
            let StatementKind::Assign { var, value, .. } = &s.kind else {
                continue;
            };
            if var.base.starts_with("LAYERSERIES_COMPAT_")
                && value.contains("${LAYERSERIES_CORENAMES}")
            {
                out.tag(
                    Some(Feature::KeptUntilLangdale),
                    "LAYERSERIES_COMPAT copying ${LAYERSERIES_CORENAMES}",
                    "write the release names the layer is tested with",
                );
                self.push(
                    cx,
                    out,
                    s.span.start,
                    Severity::Error,
                    format!(
                        "`{} = \"{}\"`: {} BitBake deletes LAYERSERIES_CORENAMES after the core \
                         layer's layer.conf, so the reference stays literal and the layer is \
                         refused, and this layer is checked against {}; write the release names \
                         the layer is tested with",
                        var.base,
                        value.trim(),
                        since(Some(Feature::KeptUntilLangdale)),
                        helpers::release_list(&lack)
                    ),
                );
            }
        }
    }

    /// Obsolete license names in `LICENSE`.
    fn licenses(&self, cx: &LintContext<'_>, out: &mut Sink) {
        let Some(file) = cx.file else { return };
        if matches!(cx.kind, Some(FileKind::Conf) | Some(FileKind::Class)) {
            return;
        }
        let strict = helpers::releases_lacking(cx, Feature::KeptUntilScarthgap);
        for s in &file.statements {
            let StatementKind::Assign { var, value, .. } = &s.kind else {
                continue;
            };
            if var.base != "LICENSE"
                || var.flag.is_some()
                || var.overrides.iter().any(|o| o == "remove")
            {
                continue;
            }
            let old: Vec<&str> = license_words(value)
                .into_iter()
                .filter(|w| OBSOLETE_LICENSES.contains(w))
                .collect();
            let Some(first) = old.first() else { continue };
            let spdx = SPDX_NAMES.iter().find(|(o, _)| o == first).map_or(
                "its SPDX identifier (OE-core's SPDXLICENSEMAP in meta/conf/licenses.conf)"
                    .to_string(),
                |(_, n)| format!("`{n}`"),
            );
            let names = old.iter().map(|o| format!("`{o}`")).collect::<Vec<_>>();
            let (severity, effect) = if strict.is_empty() {
                (
                    Severity::Warning,
                    format!(
                        "OE-core's obsolete-license QA check warns about it, and from {} on it is \
                         an error",
                        Feature::KeptUntilScarthgap
                            .removed_in()
                            .map_or("", |r| r.codename())
                    ),
                )
            } else {
                (
                    Severity::Error,
                    format!(
                        "OE-core's obsolete-license QA check is an error {} (ERROR_QA), and this \
                         layer is checked against {}",
                        since(Some(Feature::KeptUntilScarthgap)),
                        helpers::release_list(&strict)
                    ),
                )
            };
            out.tag(
                Some(Feature::KeptUntilScarthgap),
                "obsolete license names in LICENSE",
                &spdx,
            );
            self.push(
                cx,
                out,
                s.span.start,
                severity,
                format!(
                    "{} {} an obsolete license name: {effect}; write {spdx}",
                    helpers::and_list(&names),
                    if names.len() == 1 { "is" } else { "are" }
                ),
            );
        }
    }

    /// Assignments without whitespace around the operator.
    fn whitespace(&self, cx: &LintContext<'_>, out: &mut Sink) {
        let Some(file) = cx.file else { return };
        let warned = helpers::releases_having(cx, Feature::AssignmentWhitespaceWarning);
        if warned.is_empty() {
            return;
        }
        for s in &file.statements {
            if !matches!(s.kind, StatementKind::Assign { .. }) {
                continue;
            }
            let text = helpers::statement_text(cx, s);
            let first = text.lines().next().unwrap_or("");
            if !lacks_assignment_whitespace(first) {
                continue;
            }
            out.tag(
                Some(Feature::AssignmentWhitespaceWarning),
                "assignments without whitespace around the operator",
                "write `NAME = \"value\"`",
            );
            self.push(
                cx,
                out,
                s.span.start,
                Severity::Warning,
                format!(
                    "no whitespace around the assignment operator: from {} on BitBake warns \
                     (\"has a lack of whitespace around the assignment\"), and this layer is \
                     checked against {}; write `NAME = \"value\"` with a space on each side",
                    Feature::AssignmentWhitespaceWarning.first().codename(),
                    helpers::release_list(&warned)
                ),
            );
        }
    }
}

/// One use of something a release removed, for the migration report: the `Feature` that
/// still has it (`None`: no supported release), the line, what it is and what to write
/// instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalUse {
    pub feature: Option<Feature>,
    pub line: u32,
    pub what: String,
    pub instead: String,
}

/// Findings, and the use behind each.
#[derive(Default)]
pub struct Sink {
    findings: Vec<Finding>,
    uses: Vec<RemovalUse>,
    pending: Option<(Option<Feature>, String, String)>,
    /// The fix of the finding at that index of `findings`.
    fixes: Vec<(usize, Fix)>,
}

impl Sink {
    fn tag(&mut self, feature: Option<Feature>, what: &str, instead: &str) {
        self.pending = Some((feature, what.to_string(), instead.to_string()));
    }

    /// Attaches `fix` to the finding pushed last.
    fn fix_last(&mut self, fix: Option<Fix>) {
        if let (Some(fix), Some(i)) = (fix, self.findings.len().checked_sub(1)) {
            self.fixes.push((i, fix));
        }
    }
}

impl HygieneReleaseRemoved {
    fn run(&self, cx: &LintContext<'_>, out: &mut Sink) {
        self.table(cx, out);
        self.source_dir(cx, out);
        self.compat_corenames(cx, out);
        self.licenses(cx, out);
        self.whitespace(cx, out);
        self.srcpv(cx, out);
    }

    /// `${SRCPV}` in `PV` when no release the file is checked against needs it: from
    /// Nanbield on `bitbake.conf` sets `SRCPV = ""` and the source revision goes to `PKGV`
    /// (`Feature::KeptUntilMickledore`; the 4.3 guide, "Versioning changes": "you can remove
    /// it (leaving behind the "+")"). Info: it has no effect. Not reported when a release the
    /// file is checked against still needs it, or when a layer other than the core layer
    /// assigns `SRCPV` (then it may not be empty). The safe fix deletes each `${SRCPV}` from
    /// the value: in every release the file is checked against it expands to nothing.
    fn srcpv(&self, cx: &LintContext<'_>, out: &mut Sink) {
        let Some(file) = cx.file else { return };
        if cx.releases.is_empty()
            || !helpers::releases_having(cx, Feature::KeptUntilMickledore).is_empty()
        {
            return;
        }
        const NEEDLE: &str = "${SRCPV}";
        let mut checked_layers = None;
        for s in &file.statements {
            let StatementKind::Assign { var, value, .. } = &s.kind else {
                continue;
            };
            if var.base != "PV" || var.flag.is_some() || !value.contains(NEEDLE) {
                continue;
            }
            let own = file.statements.iter().any(
                |o| matches!(&o.kind, StatementKind::Assign { var, .. } if var.base == "SRCPV"),
            );
            if own || *checked_layers.get_or_insert_with(|| assigned_outside_core(cx, "SRCPV")) {
                continue;
            }
            let without = value.replace(NEEDLE, "");
            out.tag(
                Some(Feature::KeptUntilMickledore),
                "${SRCPV} in PV",
                "remove `${SRCPV}`, keeping the `+`",
            );
            self.push(
                cx,
                out,
                s.span.start,
                Severity::Info,
                format!(
                    "`${{SRCPV}}` in {}: it is empty from {} on (the source revision goes to \
                     PKGV), and this layer is checked against {} only; write `{} = \"{}\"`",
                    helpers::colon_name(var),
                    Feature::KeptUntilMickledore
                        .removed_in()
                        .map_or("", |r| r.codename()),
                    helpers::release_list(cx.releases),
                    helpers::colon_name(var),
                    without
                ),
            );
            let fix = (|| {
                let source = cx.source?;
                let p = fix::assign_parts(source, s)?;
                let raw = source.get(p.value())?;
                let edits: Vec<Edit> = raw
                    .match_indices(NEEDLE)
                    .map(|(at, _)| {
                        let start = p.open + 1 + at;
                        Edit::delete(start..start + NEEDLE.len())
                    })
                    .collect();
                Fix::in_file(cx, edits, "remove `${SRCPV}` from PV", Safety::Safe)
            })();
            out.fix_last(fix);
        }
    }

    /// What the rule finds in the context's file, as uses (one per finding, same order).
    pub fn uses(&self, cx: &LintContext<'_>) -> Vec<RemovalUse> {
        let mut sink = Sink::default();
        self.run(cx, &mut sink);
        sink.uses
    }
}

fn class_message(r: &Removal, when: &str, releases: &str) -> String {
    format!(
        "class `{}` is gone {when}, so inheriting it makes BitBake stop (\"Could not inherit \
         file classes/{}.bbclass\"), and this layer is checked against {releases}; {}",
        r.name, r.name, r.instead
    )
}

impl Rule for HygieneReleaseRemoved {
    fn id(&self) -> &'static str {
        "hygiene/release-removed"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let mut sink = Sink::default();
        self.run(cx, &mut sink);
        out.append(&mut sink.findings);
    }

    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        let mut sink = Sink::default();
        self.run(cx, &mut sink);
        sink.fixes
            .into_iter()
            .filter_map(|(i, fix)| {
                Some(FixedFinding {
                    finding: sink.findings.get(i)?.clone(),
                    fix,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_whitespace_as_bitbake_reads_it() {
        for bad in [
            "FOO=\"bar\"",
            "FOO= \"bar\"",
            "FOO =\"bar\"",
            "FOO+=\"bar\"",
            "export FOO=\"bar\"",
            "FOO[doc]=\"x\"",
            "FOO:append=\" x\"",
            "A ?=\"b\"",
        ] {
            assert!(lacks_assignment_whitespace(bad), "{bad}");
        }
        for good in [
            "FOO = \"bar\"",
            "FOO:append = \" x\"",
            "FOO[doc] = \"x\"",
            "export FOO = \"bar\"",
            "A ??= 'b'",
            "PREFERRED_PROVIDER_virtual/kernel = \"linux\"",
            "X := \"${@d.getVar('Y')}\"",
        ] {
            assert!(!lacks_assignment_whitespace(good), "{good}");
        }
    }

    #[test]
    fn words_end_at_a_boundary() {
        assert_eq!(
            contains_word(
                "DEPENDS = \"virtual/${TARGET_PREFIX}gcc\"",
                "virtual/${TARGET_PREFIX}gcc"
            ),
            Some(11)
        );
        assert_eq!(
            contains_word(
                "virtual/${TARGET_PREFIX}gcc-initial",
                "virtual/${TARGET_PREFIX}gcc"
            ),
            None
        );
        assert_eq!(
            contains_word(
                "a virtual/${TARGET_PREFIX}gcc",
                "virtual/${TARGET_PREFIX}gcc"
            ),
            Some(2)
        );
        assert_eq!(
            license_words("GPLv2 & (MIT | BSD-3-Clause)"),
            ["GPLv2", "MIT", "BSD-3-Clause"]
        );
    }

    #[test]
    fn every_row_names_something_once() {
        let mut seen = std::collections::HashSet::new();
        for r in REMOVALS {
            assert!(seen.insert((r.name, format!("{:?}", r.what))), "{}", r.name);
            assert!(!r.instead.ends_with('.'), "{}", r.name);
            if let Some(f) = r.feature {
                assert!(f.removed_in().is_some(), "{}", r.name);
            }
        }
    }
}
