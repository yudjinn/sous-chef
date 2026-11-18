//! Yocto releases (T1.6): which one a workspace targets, and what differs between them.
//!
//! Wrynose is the primary target, Scarthgap is fully supported and Kirkstone is the floor.
//! Nothing outside this file compares codenames or version numbers: code asks
//! [`Release::has`] for a [`Feature`]. Each feature has a row in the "Releases" table of
//! `docs/bitbake-semantics.md` naming the source that was checked.

use std::fmt;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::workspace::Layer;

/// A supported Yocto release. Ordered oldest to newest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Release {
    Kirkstone,
    Langdale,
    Mickledore,
    Nanbield,
    Scarthgap,
    Styhead,
    Walnascar,
    Whinlatter,
    Wrynose,
}

/// Codenames older than the floor. They are recognised so that a layer naming one is
/// treated as "older than supported" and not as "newer than this table".
const OLDER_THAN_FLOOR: &[&str] = &[
    "honister",
    "hardknott",
    "gatesgarth",
    "dunfell",
    "zeus",
    "warrior",
    "thud",
    "sumo",
    "rocko",
    "pyro",
    "morty",
    "krogoth",
    "jethro",
    "fido",
    "dizzy",
    "daisy",
    "dora",
    "dylan",
    "danny",
    "denzil",
];

impl Release {
    /// Every supported release, oldest first.
    pub const ALL: [Release; 9] = [
        Release::Kirkstone,
        Release::Langdale,
        Release::Mickledore,
        Release::Nanbield,
        Release::Scarthgap,
        Release::Styhead,
        Release::Walnascar,
        Release::Whinlatter,
        Release::Wrynose,
    ];
    /// The oldest supported release.
    pub const FLOOR: Release = Release::Kirkstone;
    /// The newest release this table knows.
    pub const NEWEST: Release = Release::Wrynose;
    /// Assumed when nothing can be detected: the most recent LTS.
    pub const DEFAULT: Release = Release::Wrynose;

    pub fn from_codename(s: &str) -> Option<Release> {
        let s = s.trim();
        Release::ALL
            .into_iter()
            .find(|r| r.codename().eq_ignore_ascii_case(s))
    }

    pub fn codename(self) -> &'static str {
        match self {
            Release::Kirkstone => "kirkstone",
            Release::Langdale => "langdale",
            Release::Mickledore => "mickledore",
            Release::Nanbield => "nanbield",
            Release::Scarthgap => "scarthgap",
            Release::Styhead => "styhead",
            Release::Walnascar => "walnascar",
            Release::Whinlatter => "whinlatter",
            Release::Wrynose => "wrynose",
        }
    }

    /// The Yocto Project version number.
    pub fn version(self) -> &'static str {
        match self {
            Release::Kirkstone => "4.0",
            Release::Langdale => "4.1",
            Release::Mickledore => "4.2",
            Release::Nanbield => "4.3",
            Release::Scarthgap => "5.0",
            Release::Styhead => "5.1",
            Release::Walnascar => "5.2",
            Release::Whinlatter => "5.3",
            Release::Wrynose => "6.0",
        }
    }

    pub fn is_lts(self) -> bool {
        matches!(
            self,
            Release::Kirkstone | Release::Scarthgap | Release::Wrynose
        )
    }

    /// Whether this release has `f`.
    pub fn has(self, f: Feature) -> bool {
        self >= f.first() && f.last().is_none_or(|last| self <= last)
    }

    /// The release after this one; `None` for the newest.
    pub fn next(self) -> Option<Release> {
        let i = Release::ALL.iter().position(|r| *r == self)?;
        Release::ALL.get(i + 1).copied()
    }

    /// A release named on the command line or in a tool argument (`--as-release`,
    /// `--migrate-to`): a supported codename (any case). A codename older than the floor is
    /// refused, and anything else is an error listing the supported names.
    pub fn parse_arg(s: &str, what: &str) -> Result<Release> {
        if let Some(r) = Release::from_codename(s) {
            return Ok(r);
        }
        let known: Vec<&str> = Release::ALL.iter().map(|r| r.codename()).collect();
        let msg = if Release::is_older_than_floor(s) {
            format!(
                "{what}: release {:?} is older than {} ({}), the oldest release sous-chef supports; supported releases: {}",
                s.trim(),
                Release::FLOOR,
                Release::FLOOR.version(),
                known.join(", ")
            )
        } else {
            format!(
                "{what}: unknown release {:?}; supported releases: {}",
                s.trim(),
                known.join(", ")
            )
        };
        Err(Error::Invalid { msg })
    }

    /// A codename that names a release older than the floor.
    pub fn is_older_than_floor(codename: &str) -> bool {
        let c = codename.trim();
        OLDER_THAN_FLOOR.iter().any(|o| o.eq_ignore_ascii_case(c))
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.codename())
    }
}

/// A behaviour that differs between supported releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Feature {
    /// `classes-recipe/` and `classes-global/` are searched before `classes/`.
    SplitClassDirs,
    /// The `inherit_defer` directive.
    InheritDefer,
    /// The `include_all` directive.
    IncludeAll,
    /// `UNPACKDIR`: sources are unpacked there and not into `WORKDIR`.
    UnpackDir,
    /// `S = "${WORKDIR}"` is accepted. Later releases reject it with an error.
    WorkdirAsSourceDir,
    /// `WORKDIR` ends in `${PV}-${PR}`; later releases end it in `${PV}`.
    PrInWorkdir,
    /// The `addfragments` directive (OE configuration fragments), three-argument form.
    AddFragments,
    /// `addfragments` takes a fourth argument, the built-in fragments variable
    /// (`OE_FRAGMENTS_BUILTIN`: `machine/<name>` sets `MACHINE`); the three-argument form is
    /// an unparsed line from here on.
    BuiltinFragments,
    /// `DEPENDS` words listed in `BB_RECIPE_VIRTUAL_PROVIDERS` are replaced with their
    /// `PREFERRED_PROVIDER_<word>` when a recipe is finalised
    /// (`ast.handleVirtRecipeProviders`).
    RecipeVirtualProviders,
    /// The `addpylib` directive (base configuration only).
    AddPyLib,
    /// A `BBCLASSEXTEND` variant's class is added to the deferred inherits
    /// (`ast.multi_finalize` `extendfunc` calls `BBHandler.inherit_defer`), so it is parsed
    /// after the recipe's own deferred inherits; before, it is inherited at once.
    ClassExtendDeferred,
    /// OE-core's `base_eventhandler` sets `CLASSOVERRIDE = "class-<c>"` on
    /// `RecipePreDeferredInherits` when `native`, `nativesdk`, `crosssdk` or `cross` is among
    /// the deferred inherits, so their override applies while the deferred classes are parsed.
    DeferredInheritsClassOverride,
    /// The metadata the 4.1 (Langdale) migration guide lists as removed is still there
    /// (`PEP517_BUILD_API`). One of the `KeptUntil*` features behind `hygiene/release-removed`
    /// (`src/lint/rules/hygiene_release_removed.rs` lists what each one covers).
    KeptUntilKirkstone,
    /// What the 4.2 (Mickledore) guide removes is still there: `SERIAL_CONSOLE`,
    /// `PACKAGEBUILDPKGD`, the classes `rust-bin` and `package_tar`, the `OEBasic` signature
    /// handler; and BitBake (before 2.4)
    /// keeps each layer's `LAYERSERIES_COMPAT_<c>` and `LAYERSERIES_CORENAMES` while the
    /// other `layer.conf` files are read, so copying them works.
    KeptUntilLangdale,
    /// What the 4.3 (Nanbield) guide removes is still there: `PERLARCH`, `PERLVERSION`,
    /// `QEMU_USE_SLIRP`, `SERIAL_CONSOLES_CHECK`, the class `glide`.
    KeptUntilMickledore,
    /// What the 5.0 (Scarthgap) guide removes is still there: `DEPLOY_DIR_TAR`,
    /// `oldincludedir`.
    KeptUntilNanbield,
    /// What the 5.1 (Styhead) guide removes is still there: `TCLIBCAPPEND`,
    /// `VOLATILE_LOG_DIR` and `VOLATILE_TMP_DIR` (renamed variables from Styhead on), the class
    /// `siteconfig`, the QA check name `license-incompatible`; and `obsolete-license` is a
    /// warning (`WARN_QA`), not an error.
    KeptUntilScarthgap,
    /// What the 5.2 (Walnascar) guide removes is still there: `BB_DANGLINGAPPENDS_WARNONLY`,
    /// `PACKAGE_SNAP_LIB_SYMLINKS`, `ACLOCALDIR`, `ACLOCALEXTRAPATH`, the class
    /// `migrate_localcount`, the image feature
    /// `debug-tweaks`, the `virtual/${TARGET_PREFIX}gcc`-style toolchain providers; and a git
    /// URL without `branch=` (BitBake before 2.12) is a warning, not an error.
    KeptUntilStyhead,
    /// What the 5.3 (Whinlatter) guide removes is still there: `GPE_MIRROR`,
    /// `BUILDHISTORY_RESET`, the classes `kernel-fitimage` and `icecc`; and `S` may name a
    /// directory below `${WORKDIR}` (and `S = "${UNPACKDIR}/git"` is accepted).
    KeptUntilWalnascar,
    /// What the 6.0 (Wrynose) guide removes is still there: `DISTRO_FEATURES_BACKFILL` and the
    /// other feature backfill variables (renamed variables from Wrynose on),
    /// `CVE_CHECK_SHOW_WARNINGS`, the classes `create-spdx-2.2`, `cve-check` and `oelint`, the
    /// `bzr`, `osc` and `cvs` fetchers, and the `npm` / `npmsw` fetchers are enabled.
    KeptUntilWhinlatter,
    /// BitBake warns about an assignment without whitespace around the operator
    /// (`FOO="bar"`: "has a lack of whitespace around the assignment").
    AssignmentWhitespaceWarning,
}

impl Feature {
    pub const ALL: [Feature; 21] = [
        Feature::SplitClassDirs,
        Feature::InheritDefer,
        Feature::IncludeAll,
        Feature::UnpackDir,
        Feature::WorkdirAsSourceDir,
        Feature::PrInWorkdir,
        Feature::AddFragments,
        Feature::BuiltinFragments,
        Feature::RecipeVirtualProviders,
        Feature::AddPyLib,
        Feature::ClassExtendDeferred,
        Feature::DeferredInheritsClassOverride,
        Feature::KeptUntilKirkstone,
        Feature::KeptUntilLangdale,
        Feature::KeptUntilMickledore,
        Feature::KeptUntilNanbield,
        Feature::KeptUntilScarthgap,
        Feature::KeptUntilStyhead,
        Feature::KeptUntilWalnascar,
        Feature::KeptUntilWhinlatter,
        Feature::AssignmentWhitespaceWarning,
    ];

    /// The first supported release that has the feature.
    pub fn first(self) -> Release {
        match self {
            Feature::SplitClassDirs => Release::Langdale,
            Feature::AddPyLib => Release::Mickledore,
            Feature::InheritDefer => Release::Scarthgap,
            Feature::IncludeAll | Feature::AddFragments | Feature::RecipeVirtualProviders => {
                Release::Walnascar
            }
            Feature::BuiltinFragments
            | Feature::ClassExtendDeferred
            | Feature::DeferredInheritsClassOverride
            | Feature::AssignmentWhitespaceWarning => Release::Whinlatter,
            Feature::UnpackDir => Release::Styhead,
            Feature::WorkdirAsSourceDir
            | Feature::PrInWorkdir
            | Feature::KeptUntilKirkstone
            | Feature::KeptUntilLangdale
            | Feature::KeptUntilMickledore
            | Feature::KeptUntilNanbield
            | Feature::KeptUntilScarthgap
            | Feature::KeptUntilStyhead
            | Feature::KeptUntilWalnascar
            | Feature::KeptUntilWhinlatter => Release::FLOOR,
        }
    }

    /// The last release that has the feature; `None` when it is still present.
    pub fn last(self) -> Option<Release> {
        match self {
            Feature::WorkdirAsSourceDir => Some(Release::Scarthgap),
            Feature::PrInWorkdir => Some(Release::Mickledore),
            Feature::KeptUntilKirkstone => Some(Release::Kirkstone),
            Feature::KeptUntilLangdale => Some(Release::Langdale),
            Feature::KeptUntilMickledore => Some(Release::Mickledore),
            Feature::KeptUntilNanbield => Some(Release::Nanbield),
            Feature::KeptUntilScarthgap => Some(Release::Scarthgap),
            Feature::KeptUntilStyhead => Some(Release::Styhead),
            Feature::KeptUntilWalnascar => Some(Release::Walnascar),
            Feature::KeptUntilWhinlatter => Some(Release::Whinlatter),
            _ => None,
        }
    }

    /// Whether both ends of the range were checked against a source. A feature that is not
    /// verified must not be used to report an error. Every current feature is verified (see
    /// the "Releases" table in bitbake-semantics.md); a new one whose range is a guess is
    /// added here as `false` until it is checked.
    pub fn verified(self) -> bool {
        match self {
            Feature::SplitClassDirs
            | Feature::InheritDefer
            | Feature::IncludeAll
            | Feature::UnpackDir
            | Feature::WorkdirAsSourceDir
            | Feature::PrInWorkdir
            | Feature::AddFragments
            | Feature::BuiltinFragments
            | Feature::RecipeVirtualProviders
            | Feature::AddPyLib
            | Feature::ClassExtendDeferred
            | Feature::DeferredInheritsClassOverride
            | Feature::KeptUntilKirkstone
            | Feature::KeptUntilLangdale
            | Feature::KeptUntilMickledore
            | Feature::KeptUntilNanbield
            | Feature::KeptUntilScarthgap
            | Feature::KeptUntilStyhead
            | Feature::KeptUntilWalnascar
            | Feature::KeptUntilWhinlatter
            | Feature::AssignmentWhitespaceWarning => true,
        }
    }

    /// The first release without the feature, for a feature that ends (`last()` is set and
    /// is not the newest release).
    pub fn removed_in(self) -> Option<Release> {
        self.last().and_then(Release::next)
    }
}

/// Where the workspace's release came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseSource {
    /// `release` in `sous-chef.toml`.
    Config,
    /// `LAYERSERIES_CORENAMES` of the core layer.
    CoreNames,
    /// `LAYERSERIES_COMPAT` of the writable layers (all layers if none is writable).
    LayerCompat,
    /// Nothing to go on: [`Release::DEFAULT`].
    Assumed,
}

impl ReleaseSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseSource::Config => "sous-chef.toml",
            ReleaseSource::CoreNames => "LAYERSERIES_CORENAMES",
            ReleaseSource::LayerCompat => "LAYERSERIES_COMPAT",
            ReleaseSource::Assumed => "assumed",
        }
    }
}

/// The release a workspace targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseInfo {
    pub release: Release,
    pub source: ReleaseSource,
    /// Set when the detected name had to be mapped onto a supported release.
    pub note: Option<String>,
}

impl Default for ReleaseInfo {
    fn default() -> ReleaseInfo {
        ReleaseInfo {
            release: Release::DEFAULT,
            source: ReleaseSource::Assumed,
            note: None,
        }
    }
}

impl ReleaseInfo {
    /// One line for `status`: `wrynose (LAYERSERIES_CORENAMES)`, plus the note if any.
    pub fn describe(&self) -> String {
        let mut s = format!("{} ({})", self.release, self.source.as_str());
        if let Some(n) = &self.note {
            s.push_str("; ");
            s.push_str(n);
        }
        s
    }
}

/// What a list of codenames says: the newest supported release in it, or a mapping of
/// names outside the table onto the nearest end of it.
fn from_names(names: &[String]) -> Option<(Release, Option<String>)> {
    if let Some(r) = names.iter().filter_map(|n| Release::from_codename(n)).max() {
        return Some((r, None));
    }
    // No supported name. A name that is not a known old release is taken to be newer than
    // this table.
    if let Some(n) = names.iter().find(|n| !Release::is_older_than_floor(n)) {
        return Some((
            Release::NEWEST,
            Some(format!(
                "release {n} is not known to this version of sous-chef; treated as {}",
                Release::NEWEST
            )),
        ));
    }
    names.first().map(|n| {
        (
            Release::FLOOR,
            Some(format!(
                "release {n} is older than the oldest supported release; treated as {}",
                Release::FLOOR
            )),
        )
    })
}

/// Detects the workspace release, in order: the configured name, `LAYERSERIES_CORENAMES`,
/// the `LAYERSERIES_COMPAT` of the writable layers (all layers if none is writable), then
/// [`Release::DEFAULT`].
///
/// A configured name that is neither a supported release nor a known older one is an
/// error (most likely a typo); the message lists the supported names.
pub fn detect(
    configured: Option<&str>,
    corenames: &[String],
    layers: &[Layer],
) -> Result<ReleaseInfo> {
    if let Some(name) = configured {
        let names = [name.trim().to_string()];
        if Release::from_codename(name).is_none() && !Release::is_older_than_floor(name) {
            let known: Vec<&str> = Release::ALL.iter().map(|r| r.codename()).collect();
            return Err(Error::Config {
                msg: format!(
                    "unknown release {name:?} in sous-chef.toml; supported releases: {}",
                    known.join(", ")
                ),
            });
        }
        if let Some((release, note)) = from_names(&names) {
            return Ok(ReleaseInfo {
                release,
                source: ReleaseSource::Config,
                note,
            });
        }
    }
    if let Some((release, note)) = from_names(corenames) {
        return Ok(ReleaseInfo {
            release,
            source: ReleaseSource::CoreNames,
            note,
        });
    }
    let any_writable = layers.iter().any(|l| l.writable);
    let claimed = layers
        .iter()
        .filter(|l| l.writable || !any_writable)
        .flat_map(|l| l.compat.iter())
        .filter_map(|n| Release::from_codename(n))
        .max();
    if let Some(release) = claimed {
        return Ok(ReleaseInfo {
            release,
            source: ReleaseSource::LayerCompat,
            note: None,
        });
    }
    Ok(ReleaseInfo::default())
}

/// How a layer came to be linted, for [`lint_releases`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LintReach {
    /// The layer is the lint target or contains it: a layer named as the target, a file of
    /// a file target, a file of a recipe target. Also the answer for code outside lint that
    /// asks what a layer must be valid for (scaffold, which writes into a writable layer).
    Target,
    /// The layer is part of a whole-workspace run.
    Sweep,
}

/// The releases a layer is linted against, oldest first, de-duplicated (decided
/// 2026-11-17, overview.md "Lint and multi-release layers"):
///
/// - every supported release the layer claims in `LAYERSERIES_COMPAT`, plus the workspace's
///   release, when the layer is writable, when it is reached as the target
///   ([`LintReach::Target`]), or when the workspace has no writable layer
///   (`any_writable == false`);
/// - the workspace's release alone for a layer that is not writable, swept in by a
///   whole-workspace run ([`LintReach::Sweep`], which only reaches such a layer with `--all`)
///   in a workspace with at least one writable layer.
///
/// Names older than the floor or newer than the table are ignored.
pub fn lint_releases(
    layer: &Layer,
    workspace: Release,
    any_writable: bool,
    reach: LintReach,
) -> Vec<Release> {
    let mut out = vec![workspace];
    let workspace_only = !layer.writable && any_writable && reach == LintReach::Sweep;
    if !workspace_only {
        out.extend(
            layer
                .compat
                .iter()
                .filter_map(|n| Release::from_codename(n)),
        );
    }
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------------------
// The workspace's BitBake: when a class-extended variant's class is inherited

/// How BitBake inherits a variant's class (`BBCLASSEXTEND`), relative to the recipe's
/// deferred inherits (`inherit_defer`). bitbake-semantics.md, "Class-extended variants".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassExtendOrder {
    /// BitBake before `139f61fe9` (2.8.0 up to Scarthgap 5.0.10, Styhead 2.10, Walnascar
    /// 2.12): `multi_finalize` runs the recipe's deferred inherits once, on the base
    /// recipe after its bbappends, before any variant is made; each variant then inherits
    /// its class at once.
    BaseDeferredThenClass,
    /// `139f61fe9` "ast: Change deferred inherits to happen per recipe" (Scarthgap 5.0.11
    /// on, BitBake 2.16 on): the class is appended to the variant's deferred inherits, which
    /// `finalize` runs per variant, so the class comes after the recipe's deferred inherits
    /// and those are evaluated in the variant.
    PerVariantDeferred,
}

/// What the workspace's BitBake source says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitbakeSource {
    /// The BitBake directory (`.../bitbake`).
    pub dir: std::path::PathBuf,
    /// `lib/bb/__init__.py` `__version__`.
    pub version: Option<String>,
    /// From `lib/bb/parse/ast.py` `multi_finalize`: whether `extendfunc` appends the class
    /// to `__BBDEFINHERITS` or inherits it at once; `None` when neither line is there.
    pub order: Option<ClassExtendOrder>,
}

/// `__version__ = "x.y.z"` from the text of `lib/bb/__init__.py`.
pub fn bitbake_version(init_py: &str) -> Option<String> {
    init_py.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("__version__")?.trim_start();
        let v = rest.strip_prefix('=')?.trim().trim_matches(['"', '\'']);
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// The order `ast.py`'s `multi_finalize` uses, from its text: `139f61fe9` on branch `2.8`
/// appends `(extendedmap[name], fn, 0)` to `__BBDEFINHERITS`; branches `2.16` and `2.18` call
/// `BBHandler.inherit_defer(extendedmap[name], fn, 0, d)`; before, `extendfunc` calls
/// `bb.parse.BBHandler.inherit(extendedmap[name], fn, 0, d)`.
pub fn class_extend_order_in(ast_py: &str) -> Option<ClassExtendOrder> {
    if ast_py.contains("inherits.append((extendedmap[name], fn, 0))")
        || ast_py.contains("BBHandler.inherit_defer(extendedmap[name], fn, 0, d)")
    {
        Some(ClassExtendOrder::PerVariantDeferred)
    } else if ast_py.contains("bb.parse.BBHandler.inherit(extendedmap[name], fn, 0, d)") {
        Some(ClassExtendOrder::BaseDeferredThenClass)
    } else {
        None
    }
}

/// The BitBake checkout next to the OE-core layer (collection `core`), found as OE-core's
/// `scripts/oe-buildenv-internal` does: `$OEROOT/bitbake`, else `$OEROOT/../bitbake`, where
/// `OEROOT` is the directory above the layer (the one holding `oe-init-build-env`). Read
/// once per directory per process.
pub fn workspace_bitbake(layers: &[Layer]) -> Option<std::sync::Arc<BitbakeSource>> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<BitbakeSource>>>>> = OnceLock::new();
    let core = layers
        .iter()
        .find(|l| l.collections.iter().any(|c| c == "core"))?;
    let oeroot = core.root.parent()?.to_path_buf();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(c) = cache.lock()
        && let Some(hit) = c.get(&oeroot)
    {
        return hit.clone();
    }
    let found = [oeroot.join("bitbake"), oeroot.join("../bitbake")]
        .into_iter()
        .find(|d| d.join("lib/bb/__init__.py").is_file())
        .map(|dir| {
            let version = std::fs::read_to_string(dir.join("lib/bb/__init__.py"))
                .ok()
                .and_then(|t| bitbake_version(&t));
            let order = std::fs::read_to_string(dir.join("lib/bb/parse/ast.py"))
                .ok()
                .and_then(|t| class_extend_order_in(&t));
            Arc::new(BitbakeSource {
                dir,
                version,
                order,
            })
        });
    if let Ok(mut c) = cache.lock() {
        c.insert(oeroot, found.clone());
    }
    found
}

/// The order for `release`, with the workspace's BitBake (`bb`) when found, and a note when
/// it is not known. Whinlatter (BitBake 2.16) on: per variant. Styhead (2.10) and Walnascar
/// (2.12): at once, after the base's deferred inherits (their branches never got
/// `139f61fe9`). Scarthgap: BitBake 2.8.0 is the version of Yocto 5.0 to 5.0.12, and
/// `139f61fe9` came in with 5.0.11; `__version__` became 2.8.1 in 5.0.13 (`1c9ec1ffd`). So
/// `ast.py` decides, then a version from 2.8.1 on; without either the order of 5.0 to
/// 5.0.10 is used and the note says so.
pub fn class_extend_order(
    release: Release,
    bb: Option<&BitbakeSource>,
) -> (ClassExtendOrder, Option<String>) {
    if release.has(Feature::ClassExtendDeferred) {
        return (ClassExtendOrder::PerVariantDeferred, None);
    }
    if release != Release::Scarthgap {
        return (ClassExtendOrder::BaseDeferredThenClass, None);
    }
    if let Some(o) = bb.and_then(|b| b.order) {
        return (o, None);
    }
    let v = bb.and_then(|b| b.version.clone());
    let parts: Vec<u32> = v
        .as_deref()
        .unwrap_or("")
        .split('.')
        .map_while(|p| p.parse().ok())
        .collect();
    if parts.len() >= 3 && (parts[0], parts[1], parts[2]) >= (2, 8, 1) {
        return (ClassExtendOrder::PerVariantDeferred, None);
    }
    let seen = match (bb, v) {
        (Some(b), Some(v)) => format!(
            "the workspace's BitBake ({}) is {v}, which Scarthgap 5.0 to 5.0.12 all report, and its ast.py was not recognised",
            b.dir.display()
        ),
        (Some(b), None) => format!(
            "the workspace's BitBake ({}) has no readable version or ast.py",
            b.dir.display()
        ),
        _ => "no BitBake checkout was found next to the OE-core layer".to_string(),
    };
    (
        ClassExtendOrder::BaseDeferredThenClass,
        Some(format!(
            "this evaluation follows Scarthgap 5.0 to 5.0.10 (BitBake 2.8.0): the recipe's deferred inherits run on the base recipe before the variant is made, then the class is inherited at once; Scarthgap 5.0.11 and later (BitBake 2.8 with 139f61fe9) append the class to the variant's own deferred inherits instead, after them; {seen}"
        )),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn layer(name: &str, compat: &[&str], writable: bool) -> Layer {
        Layer {
            name: name.to_string(),
            root: PathBuf::from("/x").join(name),
            collections: vec![name.to_string()],
            priority: 5,
            compat: compat.iter().map(|s| s.to_string()).collect(),
            depends: Vec::new(),
            bbfiles: Vec::new(),
            writable,
        }
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn releases_are_ordered_and_named() {
        assert!(Release::Kirkstone < Release::Scarthgap);
        assert!(Release::Scarthgap < Release::Wrynose);
        assert!(Release::ALL.windows(2).all(|w| w[0] < w[1]));
        for r in Release::ALL {
            assert_eq!(Release::from_codename(r.codename()), Some(r));
        }
        assert_eq!(
            Release::from_codename(" Scarthgap "),
            Some(Release::Scarthgap)
        );
        assert_eq!(Release::from_codename("dunfell"), None);
        assert_eq!(Release::Wrynose.version(), "6.0");
        let lts: Vec<Release> = Release::ALL.into_iter().filter(|r| r.is_lts()).collect();
        assert_eq!(
            lts,
            [Release::Kirkstone, Release::Scarthgap, Release::Wrynose]
        );
    }

    #[test]
    fn features_at_the_three_lts_releases() {
        use Feature::*;
        // (feature, kirkstone, scarthgap, wrynose)
        let table = [
            (SplitClassDirs, false, true, true),
            (InheritDefer, false, true, true),
            (IncludeAll, false, false, true),
            (UnpackDir, false, false, true),
            (WorkdirAsSourceDir, true, true, false),
            (PrInWorkdir, true, false, false),
            (AddFragments, false, false, true),
            (BuiltinFragments, false, false, true),
            (RecipeVirtualProviders, false, false, true),
            (AddPyLib, false, true, true),
            (ClassExtendDeferred, false, false, true),
            (DeferredInheritsClassOverride, false, false, true),
            (KeptUntilKirkstone, true, false, false),
            (KeptUntilLangdale, true, false, false),
            (KeptUntilMickledore, true, false, false),
            (KeptUntilNanbield, true, false, false),
            (KeptUntilScarthgap, true, true, false),
            (KeptUntilStyhead, true, true, false),
            (KeptUntilWalnascar, true, true, false),
            (KeptUntilWhinlatter, true, true, false),
            (AssignmentWhitespaceWarning, false, false, true),
        ];
        assert_eq!(table.len(), Feature::ALL.len());
        for (f, k, s, w) in table {
            assert_eq!(Release::Kirkstone.has(f), k, "{f:?} at kirkstone");
            assert_eq!(Release::Scarthgap.has(f), s, "{f:?} at scarthgap");
            assert_eq!(Release::Wrynose.has(f), w, "{f:?} at wrynose");
        }
        // The releases next to each boundary.
        assert!(Release::Langdale.has(SplitClassDirs));
        assert!(!Release::Nanbield.has(InheritDefer));
        assert!(!Release::Styhead.has(IncludeAll));
        assert!(Release::Walnascar.has(IncludeAll));
        assert!(Release::Styhead.has(UnpackDir));
        assert!(!Release::Styhead.has(WorkdirAsSourceDir));
        assert!(Release::Mickledore.has(PrInWorkdir));
        assert!(!Release::Nanbield.has(PrInWorkdir));
        assert!(!Release::Styhead.has(AddFragments));
        assert!(Release::Walnascar.has(AddFragments));
        assert!(!Release::Walnascar.has(BuiltinFragments));
        assert!(Release::Whinlatter.has(BuiltinFragments));
        assert!(!Release::Styhead.has(RecipeVirtualProviders));
        assert!(Release::Walnascar.has(RecipeVirtualProviders));
        assert!(!Release::Langdale.has(AddPyLib));
        assert!(Release::Mickledore.has(AddPyLib));
        assert!(!Release::Walnascar.has(ClassExtendDeferred));
        assert!(Release::Whinlatter.has(ClassExtendDeferred));
        assert!(!Release::Walnascar.has(DeferredInheritsClassOverride));
        assert!(Release::Whinlatter.has(DeferredInheritsClassOverride));
        assert!(Feature::ALL.iter().all(|f| f.verified()));
        assert!(Release::Walnascar.has(KeptUntilWalnascar));
        assert!(!Release::Whinlatter.has(KeptUntilWalnascar));
        assert!(!Release::Walnascar.has(AssignmentWhitespaceWarning));
        assert_eq!(KeptUntilStyhead.removed_in(), Some(Release::Walnascar));
        assert_eq!(KeptUntilWhinlatter.removed_in(), Some(Release::Wrynose));
        assert_eq!(InheritDefer.removed_in(), None);
        assert_eq!(Release::Wrynose.next(), None);
        assert_eq!(Release::Kirkstone.next(), Some(Release::Langdale));
    }

    #[test]
    fn release_arguments() {
        assert_eq!(
            Release::parse_arg("Wrynose", "--as-release").unwrap(),
            Release::Wrynose
        );
        let e = Release::parse_arg("wrynoes", "--as-release")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("unknown release") && e.contains("kirkstone, langdale"),
            "{e}"
        );
        let e = Release::parse_arg("dunfell", "--migrate-to")
            .unwrap_err()
            .to_string();
        assert!(e.contains("older than kirkstone"), "{e}");
    }

    #[test]
    fn config_wins_over_everything() {
        let layers = [layer("mine", &["wrynose"], true)];
        let info = detect(Some("scarthgap"), &names(&["wrynose"]), &layers).unwrap();
        assert_eq!(info.release, Release::Scarthgap);
        assert_eq!(info.source, ReleaseSource::Config);
        assert_eq!(info.note, None);
    }

    #[test]
    fn config_typo_is_an_error_listing_the_releases() {
        let e = detect(Some("scarthgab"), &[], &[]).unwrap_err().to_string();
        assert!(e.contains("scarthgab"), "{e}");
        assert!(e.contains("kirkstone") && e.contains("wrynose"), "{e}");
    }

    #[test]
    fn corenames_come_before_layer_compat() {
        let layers = [layer("mine", &["kirkstone"], true)];
        let info = detect(None, &names(&["scarthgap", "styhead"]), &layers).unwrap();
        assert_eq!(info.release, Release::Styhead);
        assert_eq!(info.source, ReleaseSource::CoreNames);
    }

    #[test]
    fn layer_compat_uses_writable_layers_when_there_are_any() {
        let layers = [
            layer("upstream", &["wrynose"], false),
            layer("mine", &["kirkstone", "scarthgap"], true),
        ];
        let info = detect(None, &[], &layers).unwrap();
        assert_eq!(info.release, Release::Scarthgap);
        assert_eq!(info.source, ReleaseSource::LayerCompat);

        let none_writable = [
            layer("a", &["kirkstone"], false),
            layer("b", &["nanbield"], false),
        ];
        let info = detect(None, &[], &none_writable).unwrap();
        assert_eq!(info.release, Release::Nanbield);
    }

    #[test]
    fn nothing_to_go_on_assumes_the_default() {
        let info = detect(None, &[], &[layer("mine", &[], true)]).unwrap();
        assert_eq!(info, ReleaseInfo::default());
        assert_eq!(info.release, Release::Wrynose);
        assert_eq!(info.source, ReleaseSource::Assumed);
        assert_eq!(info.describe(), "wrynose (assumed)");
    }

    #[test]
    fn a_newer_codename_is_treated_as_the_newest_known() {
        let info = detect(None, &names(&["zzyzx"]), &[]).unwrap();
        assert_eq!(info.release, Release::NEWEST);
        assert_eq!(info.source, ReleaseSource::CoreNames);
        let note = info.note.unwrap();
        assert!(note.contains("zzyzx") && note.contains("wrynose"), "{note}");
    }

    #[test]
    fn an_older_codename_is_treated_as_the_floor() {
        let info = detect(None, &names(&["dunfell"]), &[]).unwrap();
        assert_eq!(info.release, Release::Kirkstone);
        assert!(info.note.unwrap().contains("older"));

        let info = detect(Some("dunfell"), &[], &[]).unwrap();
        assert_eq!(info.release, Release::Kirkstone);
        assert_eq!(info.source, ReleaseSource::Config);
        assert!(info.note.is_some());

        // An old name in a layer's compat is not a supported claim.
        let info = detect(None, &[], &[layer("legacy", &["dunfell"], true)]).unwrap();
        assert_eq!(info.source, ReleaseSource::Assumed);
    }

    #[test]
    fn lint_releases_for_writable_and_upstream_layers() {
        use LintReach::{Sweep, Target};
        let mine = layer("mine", &["kirkstone", "scarthgap", "dunfell"], true);
        let claimed = [Release::Kirkstone, Release::Scarthgap, Release::Wrynose];
        for reach in [Target, Sweep] {
            for any in [true, false] {
                assert_eq!(lint_releases(&mine, Release::Wrynose, any, reach), claimed);
            }
        }
        let upstream = layer("upstream", &["kirkstone", "scarthgap"], false);
        // Named as the target, or nothing writable: every claim.
        assert_eq!(
            lint_releases(&upstream, Release::Wrynose, true, Target),
            claimed
        );
        assert_eq!(
            lint_releases(&upstream, Release::Wrynose, false, Sweep),
            claimed
        );
        // Swept in by --all next to a writable layer: the workspace release alone.
        assert_eq!(
            lint_releases(&upstream, Release::Wrynose, true, Sweep),
            [Release::Wrynose]
        );
        let same = layer("mine", &["wrynose"], true);
        assert_eq!(
            lint_releases(&same, Release::Wrynose, true, Target),
            [Release::Wrynose]
        );
    }

    #[test]
    fn class_extend_order_by_release_and_bitbake() {
        use ClassExtendOrder::*;
        assert_eq!(
            bitbake_version("import os\n__version__ = \"2.8.1\"\n").as_deref(),
            Some("2.8.1")
        );
        assert_eq!(bitbake_version("x = 1\n"), None);
        let bb = |version: &str, order: Option<ClassExtendOrder>| BitbakeSource {
            dir: PathBuf::from("/bb"),
            version: Some(version.into()),
            order,
        };
        assert_eq!(
            class_extend_order(Release::Wrynose, None),
            (PerVariantDeferred, None)
        );
        assert_eq!(
            class_extend_order(Release::Whinlatter, None).0,
            PerVariantDeferred
        );
        for r in [Release::Styhead, Release::Walnascar] {
            assert_eq!(class_extend_order(r, None), (BaseDeferredThenClass, None));
        }
        let s = Release::Scarthgap;
        assert_eq!(
            class_extend_order(s, Some(&bb("2.8.1", None))),
            (PerVariantDeferred, None)
        );
        assert_eq!(
            class_extend_order(s, Some(&bb("2.8.0", Some(PerVariantDeferred)))),
            (PerVariantDeferred, None)
        );
        assert_eq!(
            class_extend_order(s, Some(&bb("2.8.0", Some(BaseDeferredThenClass)))),
            (BaseDeferredThenClass, None)
        );
        let (o, note) = class_extend_order(s, Some(&bb("2.8.0", None)));
        assert_eq!(o, BaseDeferredThenClass);
        assert!(note.is_some_and(|n| n.contains("5.0 to 5.0.12")));
        assert!(class_extend_order(s, None).1.is_some());
        assert_eq!(
            class_extend_order_in("    inherits.append((extendedmap[name], fn, 0))"),
            Some(PerVariantDeferred)
        );
        assert_eq!(
            class_extend_order_in("bb.parse.BBHandler.inherit_defer(extendedmap[name], fn, 0, d)"),
            Some(PerVariantDeferred)
        );
        assert_eq!(
            class_extend_order_in("bb.parse.BBHandler.inherit(extendedmap[name], fn, 0, d)"),
            Some(BaseDeferredThenClass)
        );
        assert_eq!(class_extend_order_in(""), None);
    }
}
