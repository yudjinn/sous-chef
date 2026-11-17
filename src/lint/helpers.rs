//! Shared helpers for rules: variable classification (list-like, package-scoped), the
//! known-override set from the index, release wording, and small conveniences for building
//! findings.
//!
//! Owned by T4.1. Rule tasks ask for additions in their hand-off, or keep a private helper in
//! their own file. Wave 4 follow-up: the helpers the rule streams had duplicated (names,
//! message values, recipe-pass files, version data, reading an indexed file, here-documents,
//! image classes) moved here.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use crate::error::Result;
use crate::eval::DataStore;
use crate::index::{Index, query};
use crate::lint::{Finding, LintContext, Rule};
use crate::parse::words;
use crate::parse::{ParsedFile, Statement};
use crate::release::{Feature, Release};
use crate::types::{FileKind, LineSpan, Loc, VarRef};

// ---------------------------------------------------------------------------------------
// Findings

/// A pointer into the context's file (or layer conf) at `lines`.
pub fn loc_at(cx: &LintContext<'_>, lines: LineSpan) -> Loc {
    Loc {
        lines,
        ..cx.loc.clone()
    }
}

/// A finding of `rule` at its default severity, at `lines` of the context's file.
pub fn finding(
    rule: &dyn Rule,
    cx: &LintContext<'_>,
    lines: LineSpan,
    message: impl Into<String>,
) -> Finding {
    Finding {
        rule: rule.id(),
        severity: rule.default_severity(),
        loc: loc_at(cx, lines),
        message: message.into(),
    }
}

/// The source text of a statement (whole, continuation lines and body included); empty when
/// the context has no source.
pub fn statement_text<'a>(cx: &LintContext<'a>, stmt: &Statement) -> &'a str {
    cx.source
        .and_then(|s| s.get(stmt.bytes.clone()))
        .unwrap_or("")
}

/// `a`, `a and b`, `a, b and c`.
pub fn and_list<S: AsRef<str>>(items: &[S]) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_string(),
        [init @ .., last] => {
            let init: Vec<&str> = init.iter().map(AsRef::as_ref).collect();
            format!("{} and {}", init.join(", "), last.as_ref())
        }
    }
}

/// The longest value a message quotes in full, in characters.
pub const MESSAGE_VALUE_CHARS: usize = 40;

/// `text` as a message quotes it: unchanged up to [`MESSAGE_VALUE_CHARS`] characters, else
/// its first 37 and `...`.
pub fn clip(text: &str) -> String {
    if text.chars().count() > MESSAGE_VALUE_CHARS {
        text.chars()
            .take(MESSAGE_VALUE_CHARS - 3)
            .collect::<String>()
            + "..."
    } else {
        text.to_string()
    }
}

/// A value as a message quotes it: whitespace runs collapsed to one space, then at most
/// [`MESSAGE_VALUE_CHARS`] characters. A longer value keeps its start (`keep_end` false) or
/// its end, with `...` for the cut.
pub fn short_value(value: &str, keep_end: bool) -> String {
    let s = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = s.chars().count();
    if n <= MESSAGE_VALUE_CHARS {
        return s;
    }
    if keep_end {
        "...".to_string()
            + &s.chars()
                .skip(n - (MESSAGE_VALUE_CHARS - 3))
                .collect::<String>()
    } else {
        s.chars().take(MESSAGE_VALUE_CHARS - 3).collect::<String>() + "..."
    }
}

// ---------------------------------------------------------------------------------------
// Names

/// `base` followed by each override, joined with `sep` (`:` for the current syntax, `_` for
/// the legacy one).
pub fn joined(base: &str, overrides: &[String], sep: char) -> String {
    let mut s = base.to_string();
    for o in overrides {
        s.push(sep);
        s.push_str(o);
    }
    s
}

/// A variable reference back to text in the current syntax, without its flag:
/// `RDEPENDS:${PN}:append`.
pub fn colon_name(var: &VarRef) -> String {
    joined(&var.base, &var.overrides, ':')
}

// ---------------------------------------------------------------------------------------
// Files

/// Files as `(layer, path)`.
pub type FileSet = HashSet<(String, PathBuf)>;

/// `(tree, all)` from a recipe pass's visits ([`DataStore::files`]), from the visit of the
/// context's own `.bb` on: `tree` holds the recipe's own files (`.bb`, `.inc`, bbappends),
/// `all` every file visited (classes included). Both empty when the recipe was not visited.
pub fn recipe_pass_files(cx: &LintContext<'_>, data: &DataStore) -> (FileSet, FileSet) {
    let visits = data.files();
    let Some(start) = visits.iter().position(|v| {
        v.kind == FileKind::Recipe && v.loc.layer == cx.loc.layer && v.loc.path == cx.loc.path
    }) else {
        return (HashSet::new(), HashSet::new());
    };
    let mut tree = HashSet::new();
    let mut all = HashSet::new();
    for v in &visits[start..] {
        let key = (v.loc.layer.clone(), v.loc.path.clone());
        if matches!(
            v.kind,
            FileKind::Recipe | FileKind::Append | FileKind::Include
        ) {
            tree.insert(key.clone());
        }
        all.insert(key);
    }
    (tree, all)
}

/// Reads an indexed file through [`crate::workspace::Workspace::confine`], decodes and
/// parses it; `None` when it cannot be read.
pub fn read_indexed(cx: &LintContext<'_>, f: &query::FileRow) -> Option<(String, ParsedFile)> {
    let abs = cx
        .ws
        .confine(&cx.ws.abs_path(&f.loc(LineSpan::line(1)))?)
        .ok()?;
    let bytes = std::fs::read(abs).ok()?;
    let text = crate::parse::lexer::decode(&bytes).into_owned();
    let parsed = crate::parse::parse(&text);
    Some((text, parsed))
}

/// Who includes a file, followed up through `require` / `include` / `include_all` edges
/// ([`including_recipes`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Includers {
    /// The `.bb` files that reach the file, directly or through other `.inc` files, by id.
    pub recipes: Vec<query::FileRow>,
    /// Whether a class, a `.conf`, a bbappend or another non-`.inc` file also reaches it.
    pub other: bool,
}

/// The recipes that reach `file` through include edges (the file itself when it is a
/// `.bb`, and every `.bb` that requires it), and whether anything else does. `None` when
/// the index cannot answer. Only resolved edges count: an include whose argument needs
/// evaluation is not seen.
pub fn including_recipes(index: &Index, file: &query::FileRow) -> Option<Includers> {
    let mut out = Includers::default();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut todo = vec![file.clone()];
    while let Some(f) = todo.pop() {
        if !seen.insert(f.id) {
            continue;
        }
        let id = f.id;
        match f.kind {
            // A `.bb` may itself be required by another `.bb`
            // (`buildtools-docs-tarball.bb` requires `buildtools-tarball.bb`).
            FileKind::Recipe => out.recipes.push(f),
            FileKind::Include => {}
            _ => {
                out.other = true;
                continue;
            }
        }
        for e in query::edges_to(index, id).ok()? {
            if matches!(e.kind.as_str(), "require" | "include" | "include_all") {
                todo.push(e.src);
            }
        }
    }
    out.recipes.sort_by_key(|r| r.id);
    Some(out)
}

/// Text normalised for comparison, one entry per line: lines trimmed, blank and comment
/// lines dropped, whitespace runs collapsed.
pub fn normalise_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// [`normalise_lines`] joined with `\n`.
pub fn normalise(text: &str) -> String {
    normalise_lines(text).join("\n")
}

/// The here-document delimiter a shell line opens (`cat > x <<EOF`, `<<-'EOF'`), if any; not
/// a `<<<` here-string.
pub fn heredoc_delimiter(line: &str) -> Option<String> {
    let at = line.find("<<")?;
    let rest = &line[at + 2..];
    if rest.starts_with('<') {
        return None;
    }
    let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    let word: String = rest
        .trim_start_matches(['\'', '"', '\\'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then_some(word)
}

// ---------------------------------------------------------------------------------------
// Values that begin or end with a reference

/// Whether `name` is the name of a plain `${NAME}` reference (BitBake's
/// `[a-zA-Z0-9\-_+./~:]+`).
fn is_ref_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+./~:".contains(c))
}

/// How a value's first (or last) character is known statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueEdge<'a> {
    /// Literal text: the character is known.
    Literal,
    /// A `${NAME}` reference: the character comes from `NAME`'s value.
    Ref(&'a str),
    /// Python (`${@...}`), a nested reference, or anything else that cannot be told.
    Unknown,
}

/// What `value` starts with: literal text, a `${NAME}` reference, or something unknown.
pub fn leading_edge(value: &str) -> ValueEdge<'_> {
    let Some(rest) = value.strip_prefix("${") else {
        return ValueEdge::Literal;
    };
    match rest.find('}') {
        Some(end) if is_ref_name(&rest[..end]) => ValueEdge::Ref(&rest[..end]),
        _ => ValueEdge::Unknown,
    }
}

/// What `value` ends with: literal text, a `${NAME}` reference, or something unknown (a
/// `}` that closes python or a nested reference).
pub fn trailing_edge(value: &str) -> ValueEdge<'_> {
    let Some(head) = value.strip_suffix('}') else {
        return ValueEdge::Literal;
    };
    match head.rfind("${") {
        Some(at) if is_ref_name(&head[at + 2..]) => ValueEdge::Ref(&head[at + 2..]),
        _ => ValueEdge::Unknown,
    }
}

/// The values `file` gives `base` by a plain assignment (`=`, `:=`, `?=`, `??=`; any
/// conditional override, but no `:append` / `:prepend` / `:remove` and no flag), with the
/// line of each. These decide how the variable's value starts and ends.
pub fn plain_values<'a>(file: &'a ParsedFile, base: &str) -> Vec<(u32, &'a str)> {
    use crate::types::AssignOp;
    file.statements
        .iter()
        .filter_map(|s| match &s.kind {
            crate::parse::StatementKind::Assign { var, op, value, .. }
                if var.base == base
                    && var.flag.is_none()
                    && !var.legacy_syntax
                    && !var
                        .overrides
                        .iter()
                        .any(|o| matches!(o.as_str(), "append" | "prepend" | "remove"))
                    && matches!(
                        op,
                        AssignOp::Set
                            | AssignOp::Immediate
                            | AssignOp::Default
                            | AssignOp::WeakDefault
                    ) =>
            {
                Some((s.span.start, value.as_str()))
            }
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Releases

/// `kirkstone`, `kirkstone and scarthgap`, `kirkstone, langdale and scarthgap`.
pub fn release_list(releases: &[Release]) -> String {
    let names: Vec<&str> = releases.iter().map(|r| r.codename()).collect();
    and_list(&names)
}

/// The releases of the context that do not have `feature`, oldest first.
pub fn releases_lacking(cx: &LintContext<'_>, feature: Feature) -> Vec<Release> {
    cx.releases
        .iter()
        .copied()
        .filter(|r| !r.has(feature))
        .collect()
}

/// The releases of the context that have `feature`, oldest first.
pub fn releases_having(cx: &LintContext<'_>, feature: Feature) -> Vec<Release> {
    cx.releases
        .iter()
        .copied()
        .filter(|r| r.has(feature))
        .collect()
}

/// The release range of a feature, for [`Rule::releases`]: `first()..=last()` (the newest
/// release when it is still present).
pub fn feature_range(feature: Feature) -> std::ops::RangeInclusive<Release> {
    feature.first()..=feature.last().unwrap_or(Release::NEWEST)
}

// ---------------------------------------------------------------------------------------
// Variable classification

/// How a variable's value is structured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    /// Space-separated words (`DEPENDS`, `SRC_URI`, `IMAGE_INSTALL`): `:append` needs a
    /// leading space.
    Space,
    /// Colon-separated (`FILESEXTRAPATHS`, `OVERRIDES`, `BBPATH`).
    Colon,
    /// A single value (`S`, `PV`, `SUMMARY`, a flag such as `SRC_URI[sha256sum]`).
    Scalar,
    /// Not known; rules should not report on it.
    Unknown,
}

/// Well-known space-separated list variables (OE-core usage).
const SPACE_LISTS: &[&str] = &[
    "ASSUME_PROVIDED",
    "BBCLASSEXTEND",
    "BBFILES",
    "BBFILES_DYNAMIC",
    "BBLAYERS",
    "BBMASK",
    "CORE_IMAGE_EXTRA_INSTALL",
    "CVE_CHECK_IGNORE",
    "CVE_PRODUCT",
    "CVE_STATUS_GROUPS",
    "DEPENDS",
    "DISTRO_FEATURES",
    "DISTRO_FEATURES_BACKFILL_CONSIDERED",
    "ERROR_QA",
    "EXTRA_IMAGE_FEATURES",
    "EXTRA_IMAGEDEPENDS",
    "EXTRA_OECMAKE",
    "EXTRA_OECONF",
    "EXTRA_OEMAKE",
    "EXTRA_OEMESON",
    "EXTRA_OESCONS",
    "EXTRA_USERS_PARAMS",
    "FILES",
    "IMAGE_CLASSES",
    "IMAGE_FEATURES",
    "IMAGE_FSTYPES",
    "IMAGE_INSTALL",
    "IMAGE_LINGUAS",
    "INHERIT",
    "INHIBIT_DEFAULT_DEPS",
    "INITSCRIPT_PACKAGES",
    "INSANE_SKIP",
    "KERNEL_FEATURES",
    "KERNEL_MODULE_AUTOLOAD",
    "LIC_FILES_CHKSUM",
    "MACHINE_ESSENTIAL_EXTRA_RDEPENDS",
    "MACHINE_ESSENTIAL_EXTRA_RRECOMMENDS",
    "MACHINE_EXTRA_RDEPENDS",
    "MACHINE_EXTRA_RRECOMMENDS",
    "MACHINE_FEATURES",
    "MACHINE_FEATURES_BACKFILL_CONSIDERED",
    "PACKAGECONFIG",
    "PACKAGES",
    "PACKAGES_DYNAMIC",
    "PACKAGE_ARCHS",
    "PACKAGE_CLASSES",
    "PACKAGE_EXCLUDE",
    "PROVIDES",
    "RCONFLICTS",
    "RDEPENDS",
    "RPROVIDES",
    "RRECOMMENDS",
    "RREPLACES",
    "RSUGGESTS",
    "SDKIMAGE_FEATURES",
    "SRC_URI",
    "SYSTEMD_PACKAGES",
    "SYSTEMD_SERVICE",
    "TOOLCHAIN_HOST_TASK",
    "TOOLCHAIN_TARGET_TASK",
    "USERADD_PACKAGES",
    "WARN_QA",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "TARGET_CFLAGS",
    "TARGET_CXXFLAGS",
    "TARGET_LDFLAGS",
    "BUILD_CFLAGS",
    "BUILD_LDFLAGS",
];

/// Well-known colon-separated variables.
const COLON_LISTS: &[&str] = &[
    "BBPATH",
    "DISTROOVERRIDES",
    "FILESEXTRAPATHS",
    "FILESOVERRIDES",
    "FILESPATH",
    "MACHINEOVERRIDES",
    "OVERRIDES",
    "PATH",
];

/// Well-known single-value variables.
const SCALARS: &[&str] = &[
    "AUTHOR",
    "B",
    "BPN",
    "BP",
    "COMPATIBLE_HOST",
    "COMPATIBLE_MACHINE",
    "D",
    "DESCRIPTION",
    "DISTRO",
    "DISTRO_NAME",
    "DISTRO_VERSION",
    "HOMEPAGE",
    "LICENSE",
    "MACHINE",
    "MAINTAINER",
    "PE",
    "PN",
    "PR",
    "PREFERRED_VERSION",
    "PV",
    "S",
    "SECTION",
    "SRCREV",
    "SUMMARY",
    "UNPACKDIR",
    "WORKDIR",
];

/// Classifies a variable by its base name: the tables above, then a suffix heuristic
/// (`*_FEATURES`, `*DEPENDS`, `*_INSTALL`, `*FLAGS`, `*_PACKAGES`, `EXTRA_OE*` are space
/// lists; `*OVERRIDES` and `*PATH` / `*PATHS` colon lists; `*DIR`, `*_VERSION`, `*_NAME`
/// scalars). A flag (`VAR[flag]`) is a scalar.
pub fn list_kind(var: &VarRef) -> ListKind {
    if var.flag.is_some() {
        return ListKind::Scalar;
    }
    list_kind_of(&var.base)
}

/// [`list_kind`] for a plain base name.
pub fn list_kind_of(base: &str) -> ListKind {
    if SPACE_LISTS.contains(&base) {
        return ListKind::Space;
    }
    if COLON_LISTS.contains(&base) {
        return ListKind::Colon;
    }
    if SCALARS.contains(&base) {
        return ListKind::Scalar;
    }
    let ends = |s: &[&str]| s.iter().any(|x| base.ends_with(x));
    if ends(&["OVERRIDES", "PATH", "PATHS"]) {
        return ListKind::Colon;
    }
    if ends(&[
        "_FEATURES",
        "DEPENDS",
        "_INSTALL",
        "FLAGS",
        "_PACKAGES",
        "RRECOMMENDS",
    ]) || base.starts_with("EXTRA_OE")
    {
        return ListKind::Space;
    }
    if ends(&["DIR", "_VERSION", "_NAME"]) {
        return ListKind::Scalar;
    }
    ListKind::Unknown
}

/// Whether a variable is known to be a space-separated list (`list_kind == Space`).
pub fn is_space_list(var: &VarRef) -> bool {
    list_kind(var) == ListKind::Space
}

/// Variables that only mean something per package: OE-core's `pkgvarcheck` QA check
/// (`meta/classes-global/insane.bbclass`) reports each of these when set without a package
/// override ("Variable %s is set as not being package specific"). Checked against OE-core
/// master, 2026-11.
pub const PACKAGE_ONLY_VARS: &[&str] = &[
    "RDEPENDS",
    "RRECOMMENDS",
    "RSUGGESTS",
    "RCONFLICTS",
    "RPROVIDES",
    "RREPLACES",
    "FILES",
    "pkg_preinst",
    "pkg_postinst",
    "pkg_prerm",
    "pkg_postrm",
    "ALLOW_EMPTY",
];

/// Variables that may carry a package override: `PACKAGEVARS` in
/// `meta/classes-global/package.bbclass` (OE-core master 2026-11), plus `INSANE_SKIP` and
/// `QA_EMPTY_DIRS`, which OE-core reads per package too. Many of them (`SUMMARY`, `LICENSE`,
/// `SECTION`) also have a recipe-wide meaning, so being in this list does not make a bare
/// assignment wrong; see [`PACKAGE_ONLY_VARS`] for that.
pub const PACKAGE_VARS: &[&str] = &[
    "FILES",
    "RDEPENDS",
    "RRECOMMENDS",
    "SUMMARY",
    "DESCRIPTION",
    "RSUGGESTS",
    "RPROVIDES",
    "RCONFLICTS",
    "PKG",
    "ALLOW_EMPTY",
    "pkg_postinst",
    "pkg_postrm",
    "pkg_postinst_ontarget",
    "INITSCRIPT_NAME",
    "INITSCRIPT_PARAMS",
    "DEBIAN_NOAUTONAME",
    "ALTERNATIVE",
    "PKGE",
    "PKGV",
    "PKGR",
    "USERADD_PARAM",
    "GROUPADD_PARAM",
    "USERMOD_PARAM",
    "CONFFILES",
    "SYSTEMD_SERVICE",
    "LICENSE",
    "SECTION",
    "pkg_preinst",
    "pkg_prerm",
    "RREPLACES",
    "SYSTEMD_AUTO_ENABLE",
    "SKIP_FILEDEPS",
    "PRIVATE_LIBS",
    "PACKAGE_ADD_METADATA",
    "INSANE_SKIP",
    "QA_EMPTY_DIRS",
];

/// Whether an assignment (`base`, `flag`) is version data: `PV`, `PR`, `PE`,
/// `LIC_FILES_CHKSUM`, `SRCREV` and `SRCREV_<name>`, and the checksum flags of `SRC_URI`.
pub fn is_version_data(base: &str, flag: Option<&str>) -> bool {
    matches!(base, "PV" | "PR" | "PE" | "LIC_FILES_CHKSUM")
        || base == "SRCREV"
        || base.starts_with("SRCREV_")
        || (base == "SRC_URI" && flag.is_some_and(|f| f.ends_with("sum")))
}

/// Whether an inherited class name makes a recipe an image: `image`, `core-image`, or a
/// name ending in `-image`.
pub fn image_class(name: &str) -> bool {
    name == "image" || name == "core-image" || name.ends_with("-image")
}

/// Whether `base` only means something with a package override ([`PACKAGE_ONLY_VARS`]).
pub fn is_package_only(base: &str) -> bool {
    PACKAGE_ONLY_VARS.contains(&base)
}

/// The names an `*OVERRIDES`-like value may put in `OVERRIDES`, for `override/dead`'s known
/// set (and the `overrides` tool's dead view, which uses the same definition): runs of
/// `[a-z0-9_+.-]` in the text [`words::visible_text`] keeps, that is outside `${...}` and,
/// inside a `${@...}` expression, only in its quoted strings (the values python may add:
/// OE-core's tune files add `armv7a` through `bb.utils.contains('TUNE_FEATURES', 'armv7a',
/// 'armv7a:', '', d)`), never the python code itself (`bb`, `utils`, `d`).
pub fn override_words(value: &str) -> Vec<String> {
    words::visible_text(value)
        .into_iter()
        .flat_map(|t| {
            t.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "-_+.".contains(c)))
        })
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `base` may take a package override ([`PACKAGE_VARS`]).
pub fn is_package_var(base: &str) -> bool {
    PACKAGE_VARS.contains(&base)
}

// ---------------------------------------------------------------------------------------
// Known overrides

/// Override names that are always possible or are operators: they never make an override
/// unknown. From OE-core's `OVERRIDES` (`meta/conf/bitbake.conf`, master 2026-11:
/// `${TARGET_OS}:${TRANSLATED_TARGET_ARCH}:pn-${PN}:layer-${FILE_LAYERNAME}:
/// ${MACHINEOVERRIDES}:${DISTROOVERRIDES}:${CLASSOVERRIDE}:${TCOVERRIDE}${LIBCOVERRIDE}:
/// forcevariable`): the target OS and the common architectures; machine and distro
/// overrides come from the index.
const BUILTIN_OVERRIDES: &[&str] = &[
    "aarch64",
    "aarch64-be",
    "arm",
    "armeb",
    "i586",
    "i686",
    "mips",
    "mips64",
    "mipsel",
    "powerpc",
    "powerpc64",
    "powerpc64le",
    "riscv32",
    "riscv64",
    "x86",
    "x86-64",
    "append",
    "prepend",
    "remove",
    "forcevariable",
    "class-target",
    "class-native",
    "class-nativesdk",
    "class-cross",
    "class-crosssdk",
    "class-cross-canadian",
    "linux",
    "linux-gnu",
    "linux-musl",
    "libc-glibc",
    "libc-musl",
];

/// Prefixes of overrides that are generated per recipe, layer, task, toolchain or variant
/// (`pn-<pn>`, `layer-<layer>`, `task-<task>`, `class-<variant>`, `toolchain-gcc`,
/// `virtclass-multilib-<lib>`, `libc-<libc>`).
const DYNAMIC_PREFIXES: &[&str] = &[
    "pn-",
    "layer-",
    "task-",
    "class-",
    "toolchain-",
    "virtclass-",
    "libc-",
];

/// The overrides the workspace can produce, as far as the index shows them statically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownOverrides {
    /// `conf/machine/<name>.conf` stems.
    pub machines: BTreeSet<String>,
    /// `conf/distro/<name>.conf` stems.
    pub distros: BTreeSet<String>,
    /// `<name>.bbclass` stems (an inherited class is not itself an override; rules decide
    /// whether a class name counts).
    pub classes: BTreeSet<String>,
    /// Static words of `OVERRIDES`, `MACHINEOVERRIDES` and `DISTROOVERRIDES` assignments
    /// (split on `:`, words containing `${` dropped), and `class-<variant>` for every
    /// `BBCLASSEXTEND` word.
    pub declared: BTreeSet<String>,
}

impl KnownOverrides {
    /// Collects the set from the index.
    pub fn from_index(index: &Index) -> Result<KnownOverrides> {
        let mut out = KnownOverrides::default();
        for f in query::files_by_kind(index, &[FileKind::Conf, FileKind::Class])? {
            let kind = f.kind;
            let p = f.path.as_path();
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let parent = p
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default();
            if kind == FileKind::Class {
                out.classes.insert(stem.to_string());
            } else if parent.ends_with("conf/machine") {
                out.machines.insert(stem.to_string());
            } else if parent.ends_with("conf/distro") {
                out.distros.insert(stem.to_string());
            }
        }
        let scope = query::StatementFilter {
            kinds: vec!["assign".into()],
            ..query::StatementFilter::default()
        };
        for base in ["OVERRIDES", "MACHINEOVERRIDES", "DISTROOVERRIDES"] {
            for s in query::statements_by_base(index, base, &scope)? {
                let value = s.value.unwrap_or_default();
                for w in words::colon_parts(&value).into_iter().map(str::trim) {
                    if !w.is_empty() && !w.contains("${") && !w.contains(char::is_whitespace) {
                        out.declared.insert(w.to_string());
                    }
                }
            }
        }
        for s in query::statements_by_base(index, "BBCLASSEXTEND", &scope)? {
            for w in words::words(&s.value.unwrap_or_default()) {
                if !w.contains("${") {
                    let variant = w.split(':').next().unwrap_or(w);
                    out.declared.insert(format!("class-{variant}"));
                }
            }
        }
        Ok(out)
    }

    /// Whether `o` is a machine, a distro, a declared override, a built-in one, a generated
    /// one (`pn-*`, `task-*`, ...), or contains `${` (cannot be known statically).
    pub fn contains(&self, o: &str) -> bool {
        o.contains("${")
            || self.machines.contains(o)
            || self.distros.contains(o)
            || self.declared.contains(o)
            || BUILTIN_OVERRIDES.contains(&o)
            || DYNAMIC_PREFIXES.iter().any(|p| o.starts_with(p))
    }

    /// Whether `o` is a machine or distro name: the legacy-syntax test for `VAR_<o>`.
    pub fn is_machine_or_distro(&self, o: &str) -> bool {
        self.machines.contains(o) || self.distros.contains(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(base: &str, flag: Option<&str>) -> VarRef {
        VarRef {
            base: base.into(),
            overrides: vec![],
            flag: flag.map(str::to_string),
            legacy_syntax: false,
        }
    }

    #[test]
    fn list_kinds() {
        assert_eq!(list_kind(&var("DEPENDS", None)), ListKind::Space);
        assert_eq!(
            list_kind(&var("SRC_URI", Some("sha256sum"))),
            ListKind::Scalar
        );
        assert_eq!(list_kind(&var("FILESEXTRAPATHS", None)), ListKind::Colon);
        assert_eq!(list_kind(&var("S", None)), ListKind::Scalar);
        assert_eq!(list_kind(&var("MY_FEATURES", None)), ListKind::Space);
        assert_eq!(list_kind(&var("EXTRA_OEQMAKE", None)), ListKind::Space);
        assert_eq!(list_kind(&var("FOO_PATH", None)), ListKind::Colon);
        assert_eq!(list_kind(&var("FOO_DIR", None)), ListKind::Scalar);
        assert_eq!(list_kind(&var("SOMETHING", None)), ListKind::Unknown);
        assert!(is_space_list(&var("IMAGE_INSTALL", None)));
    }

    #[test]
    fn value_edges() {
        assert_eq!(leading_edge(" x"), ValueEdge::Literal);
        assert_eq!(leading_edge("${A}"), ValueEdge::Ref("A"));
        assert_eq!(leading_edge("${A} b"), ValueEdge::Ref("A"));
        assert_eq!(leading_edge("${@d.getVar('A')}"), ValueEdge::Unknown);
        assert_eq!(leading_edge("${${A}}"), ValueEdge::Unknown);
        assert_eq!(trailing_edge("a:${B_C}"), ValueEdge::Ref("B_C"));
        assert_eq!(trailing_edge("a:"), ValueEdge::Literal);
        assert_eq!(trailing_edge("${@'x' if y else ''}"), ValueEdge::Unknown);
        let f = crate::parse::parse(
            "A = \"x\"\nA:append = \"y\"\nA[doc] = \"z\"\nA:qemux86 ?= \" w\"\nB = \"v\"\n",
        );
        assert_eq!(plain_values(&f, "A"), [(1, "x"), (4, " w")]);
    }

    #[test]
    fn package_vars() {
        assert!(is_package_only("RDEPENDS"));
        assert!(!is_package_only("SUMMARY"));
        assert!(is_package_var("SUMMARY"));
        assert!(PACKAGE_ONLY_VARS.iter().all(|v| is_package_var(v)));
    }

    #[test]
    fn release_wording() {
        assert_eq!(release_list(&[]), "");
        assert_eq!(release_list(&[Release::Kirkstone]), "kirkstone");
        assert_eq!(
            release_list(&[Release::Kirkstone, Release::Scarthgap, Release::Wrynose]),
            "kirkstone, scarthgap and wrynose"
        );
        assert_eq!(
            feature_range(Feature::WorkdirAsSourceDir),
            Release::Kirkstone..=Release::Scarthgap
        );
        assert_eq!(
            feature_range(Feature::InheritDefer),
            Release::Scarthgap..=Release::NEWEST
        );
    }

    #[test]
    fn known_override_contains() {
        let mut k = KnownOverrides::default();
        k.machines.insert("qemux86".into());
        assert!(k.contains("qemux86"));
        assert!(k.contains("pn-foo"));
        assert!(k.contains("class-native"));
        assert!(k.contains("${PN}"));
        assert!(!k.contains("nosuchmachine"));
        assert!(k.is_machine_or_distro("qemux86"));
    }
}
