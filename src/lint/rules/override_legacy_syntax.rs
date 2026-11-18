//! `override/legacy-syntax`: Underscore override syntax (`VAR_append`, `RDEPENDS_${PN}`).
//!
//! See docs/lint-rules.md and docs/bitbake-semantics.md ("Legacy syntax"). Three sources:
//!
//! - The parser's `VarRef.legacy_syntax`, set on assignment and function names when an
//!   `_`-separated element is `append` / `prepend` / `remove`, or when a package-scoped
//!   variable is followed by `_${PN}` or `_<lowercase package>` (`split_var`).
//! - (Since x7) the package variables of OE-core's `convert-overrides.py` the parser's list
//!   lacks ([`SCRIPT_PACKAGE_VARS`]), split the same way.
//! - The form the parser cannot see alone: `VAR_<o>` where the trailing `_`-separated
//!   elements are all machine or distro names of the workspace
//!   ([`KnownOverrides::is_machine_or_distro`]), or `class-<variant>` / `pn-<recipe>`
//!   overrides (`EXTRA_OECONF_class-native`). Names that legitimately carry a `_<name>`
//!   suffix are never flagged this way: the `layer.conf` variables keyed by collection
//!   (`BBFILE_PRIORITY_core`, `LAYERSERIES_COMPAT_core`, ...), `PREFERRED_PROVIDER_*`,
//!   `PREFERRED_VERSION_*`, `SRCREV_<name>`, and any suffix that is a collection name of a
//!   workspace layer.
//!
//! What BitBake does (bitbake-semantics.md): a name containing `_append`, `_prepend` or
//! `_remove` is fatal; the other forms are silently a different variable. Both are errors
//! here: the line does not do what it says.

use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers::{self, KnownOverrides};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{LineSpan, VarRef};

pub struct OverrideLegacySyntax;

/// Prefixes whose `_<suffix>` is part of the variable's name, never an override.
const NAMED_SUFFIX_VARS: &[&str] = &[
    "BBFILE_PATTERN",
    "BBFILE_PRIORITY",
    "LAYERDEPENDS",
    "LAYERRECOMMENDS",
    "LAYERSERIES_COMPAT",
    "LAYERVERSION",
    "PREFERRED_PROVIDER",
    "PREFERRED_RPROVIDER",
    "PREFERRED_VERSION",
    "SRCREV",
];

impl Rule for OverrideLegacySyntax {
    fn id(&self) -> &'static str {
        "override/legacy-syntax"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        out.extend(self.scan(cx, false).into_iter().map(|(f, _)| f));
    }

    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        self.scan(cx, true)
            .into_iter()
            .filter_map(|(finding, fix)| fix.map(|fix| FixedFinding { finding, fix }))
            .collect()
    }
}

impl OverrideLegacySyntax {
    /// The findings `check` reports, each with its fix when `want_fix` (see [`rename`]).
    fn scan(&self, cx: &LintContext<'_>, want_fix: bool) -> Vec<(Finding, Option<Fix>)> {
        let mut out = Vec::new();
        let Some(file) = cx.file else { return out };
        // Loaded lazily: most files have no candidate name at all.
        let mut known: Option<(KnownOverrides, Vec<String>)> = None;
        for s in &file.statements {
            let (var, is_fn) = match &s.kind {
                StatementKind::Assign { var, .. } => (var, false),
                StatementKind::Function {
                    name: Some(var), ..
                } => (var, true),
                _ => continue,
            };
            let line = LineSpan::line(s.span.start);
            // A package variable the parser's list lacks (convert-overrides.py's longer
            // list), read as the parser reads the others.
            let script_split = (!var.legacy_syntax)
                .then(|| script_package_split(var))
                .flatten();
            let var = script_split.as_ref().unwrap_or(var);
            if var.legacy_syntax {
                let written = written_name(helpers::statement_text(cx, s));
                // The suffix of the base (`VAR_qemux86_append`): the message says what the
                // fix writes, `VAR:qemux86:append`.
                let (k, collections) = known.get_or_insert_with(|| load_known(cx));
                let cut = override_suffix(&var.base, k, collections);
                let finding =
                    helpers::finding(self, cx, line, message(var, cut, is_fn, written, true));
                let fix = want_fix.then(|| rename(cx, s, var, cut, true)).flatten();
                out.push((finding, fix));
                continue;
            }
            if !var.base.contains('_') {
                continue;
            }
            let (k, collections) = known.get_or_insert_with(|| load_known(cx));
            if let Some(cut) = override_suffix(&var.base, k, collections) {
                let finding =
                    helpers::finding(self, cx, line, message(var, Some(cut), is_fn, None, false));
                let fix = want_fix
                    .then(|| rename(cx, s, var, Some(cut), false))
                    .flatten();
                out.push((finding, fix));
            }
        }
        out
    }
}

/// The fix of a reported name: the name as written replaced by the current syntax, every
/// `_` that separates an override becoming `:` and nothing else changing (the flag, the
/// operator, the value and a function body stay as they are).
///
/// - A name the parser split (`legacy`): its base and overrides joined with `:`. When the
///   base itself ends in `_<machine>`, `_<distro>`, `_class-*` or `_pn-*` elements (the
///   suffix check, `VAR_qemux86_append`: an append to `VAR_qemux86`, the `qemux86` variant
///   of `VAR`), they become overrides too (`VAR:qemux86:append`), which is what the name
///   meant to the BitBake that accepted it. Safe: the old spelling is fatal (`_append`,
///   `_prepend`, `_remove`) or a variable nothing reads (`RDEPENDS_${PN}`).
/// - A name only the suffix check reports (`KERNEL_DEVICETREE_qemux86`): safe when every
///   suffix element is a `class-*` or `pn-*` override; suggested when one is a machine or
///   distro name, since a current layer may read such a name as a variable of its own
///   (`${FOO_${MACHINE}}`).
fn rename(
    cx: &LintContext<'_>,
    s: &crate::parse::Statement,
    var: &VarRef,
    suffix_at: Option<usize>,
    legacy: bool,
) -> Option<Fix> {
    let source = cx.source?;
    let range = fix::statement_name(source, s)?;
    let old = source.get(range.clone())?.to_string();
    let (base, mut overrides): (String, Vec<String>) = match suffix_at {
        Some(cut) => (
            var.base[..cut].to_string(),
            var.base[cut + 1..].split('_').map(str::to_string).collect(),
        ),
        None => (var.base.clone(), Vec::new()),
    };
    let suffix = overrides.clone();
    overrides.extend(var.overrides.iter().cloned());
    let modern = helpers::joined(&base, &overrides, ':');
    if modern == old {
        return None;
    }
    let machine_like = suffix
        .iter()
        .any(|e| !e.starts_with("class-") && !e.starts_with("pn-"));
    let (safety, note) = if legacy || !machine_like {
        (Safety::Safe, String::new())
    } else {
        (
            Safety::Suggested,
            format!(
                " (`{}` is a machine or distro name; check that nothing reads `{old}` as a \
                 variable of its own)",
                suffix.join("_")
            ),
        )
    };
    Fix::in_file(
        cx,
        vec![Edit::replace(range, modern.clone())],
        format!("rename `{old}` to `{modern}`{note}"),
        safety,
    )
}

fn load_known(cx: &LintContext<'_>) -> (KnownOverrides, Vec<String>) {
    let known = KnownOverrides::from_index(cx.index).unwrap_or_default();
    let collections = cx
        .ws
        .layers
        .iter()
        .flat_map(|l| l.collections.iter().cloned())
        .collect();
    (known, collections)
}

/// For `VAR_qemux86` (and `VAR_a_b` where every trailing element is an override): the byte
/// index of the `_` that starts the override suffix.
fn override_suffix(base: &str, known: &KnownOverrides, collections: &[String]) -> Option<usize> {
    let elements = split_outside_braces(base);
    if elements.len() < 2 {
        return None;
    }
    let is_override = |e: &str| {
        !e.contains("${")
            && !collections.iter().any(|c| c == e)
            && (known.is_machine_or_distro(e)
                || (e.starts_with("class-") && known.contains(e))
                || (e.starts_with("pn-") && e.len() > 3))
    };
    // The first element from which every remaining one is an override.
    let mut first = elements.len();
    while first > 1 && is_override(elements[first - 1].1) {
        first -= 1;
    }
    if first == elements.len() {
        return None;
    }
    let cut = elements[first].0 - 1;
    let prefix = &base[..cut];
    if NAMED_SUFFIX_VARS.contains(&prefix) || !prefix.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(cut)
}

/// `(byte offset, element)` of the `_`-separated elements of `s`, ignoring `_` inside
/// `${...}`.
fn split_outside_braces(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'$' if b.get(i + 1) == Some(&b'{') => {
                depth += 1;
                i += 1;
            }
            b'}' if depth > 0 => depth -= 1,
            b'_' if depth == 0 => {
                out.push((start, &s[start..i]));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push((start, &s[start..]));
    out
}

/// The package variables of OE-core's `scripts/contrib/convert-overrides.py` (`packagevars`
/// and `imagevars`, wrynose) that the parser's [`crate::parse::parser::PACKAGE_VARS`] lacks. Each
/// is read with `:<package>` (or `:<image type>`) by OE-core on wrynose: `package.bbclass`
/// `PACKAGEVARS`, `systemd.bbclass`, `useradd.bbclass`, `update-alternatives.bbclass`
/// (`ALTERNATIVE:%s`), `image_types.bbclass`. Not here, though the class reads them with
/// `_<pkg>` on purpose: `ALTERNATIVE_TARGET_<pkg>` and `ALTERNATIVE_PRIORITY_<pkg>`
/// (`update-alternatives.bbclass` on wrynose: `getVarFlag('ALTERNATIVE_PRIORITY_%s' % pkg,
/// ...)`), which are current names, not legacy ones. The script's `shortvars` (`arc`,
/// `mips`, `mipsel`, `sh4`), which it matches only in a restricted way because they are
/// ambiguous, are overrides, not package variables, and play no part here.
pub const SCRIPT_PACKAGE_VARS: &[&str] = &[
    "INITSCRIPT_NAME",
    "INITSCRIPT_PARAMS",
    "DEBIAN_NOAUTONAME",
    "ALTERNATIVE",
    "PKGE",
    "PKGV",
    "PKGR",
    "USERADD_PARAM",
    "GROUPADD_PARAM",
    "SYSTEMD_SERVICE",
    "GROUPMEMS_PARAM",
    "SYSTEMD_AUTO_ENABLE",
    "SKIP_FILEDEPS",
    "PRIVATE_LIBS",
    "PACKAGE_ADD_METADATA",
    "DEBIANNAME",
    "SYSTEMD_SERVICE_ESCAPED",
    "IMAGE_CMD",
    "EXTRA_IMAGECMD",
    "IMAGE_TYPEDEP",
    "CONVERSION_CMD",
    "COMPRESS_CMD",
];

/// `VAR_<package>` for a variable of [`SCRIPT_PACKAGE_VARS`], split as the parser splits its
/// own package variables (`legacy_split`): the longest such prefix of at most three
/// `_`-separated elements followed by a `${...}` or a lowercase element (OE package and
/// image type names are lowercase; `SYSTEMD_SERVICE_ESCAPED` is not `SYSTEMD_SERVICE` of a
/// package `ESCAPED`). `None` when the name is not one of them.
fn script_package_split(var: &VarRef) -> Option<VarRef> {
    let el = split_outside_braces(&var.base);
    let package_like =
        |e: &str| e.starts_with("${") || e.starts_with(|c: char| c.is_ascii_lowercase());
    let known = |p: &str| {
        SCRIPT_PACKAGE_VARS.contains(&p) || crate::parse::parser::PACKAGE_VARS.contains(&p)
    };
    for i in (1..el.len().min(4)).rev() {
        let prefix = &var.base[..el[i].0 - 1];
        if SCRIPT_PACKAGE_VARS.contains(&prefix) && package_like(el[i].1) {
            // A longer known prefix wins (checked first, from the longest).
            if (i + 1..el.len().min(4)).any(|j| known(&var.base[..el[j].0 - 1])) {
                return None;
            }
            let mut overrides: Vec<String> = el[i..].iter().map(|(_, e)| e.to_string()).collect();
            overrides.extend(var.overrides.iter().cloned());
            return Some(VarRef {
                base: prefix.to_string(),
                overrides,
                flag: var.flag.clone(),
                legacy_syntax: true,
            });
        }
    }
    None
}

/// The message: the name as written and the colon form to write instead, which is what
/// [`rename`] writes. `legacy`: a name the parser (or [`script_package_split`]) split;
/// `suffix_at`: where the base's own override suffix starts (`VAR_qemux86`).
fn message(
    var: &VarRef,
    suffix_at: Option<usize>,
    is_fn: bool,
    written: Option<&str>,
    legacy: bool,
) -> String {
    let flag = var
        .flag
        .as_ref()
        .map(|f| format!("[{f}]"))
        .unwrap_or_default();
    let modern = match suffix_at {
        None => helpers::joined(&var.base, &var.overrides, ':'),
        Some(cut) => {
            let mut overrides: Vec<String> =
                var.base[cut + 1..].split('_').map(str::to_string).collect();
            overrides.extend(var.overrides.iter().cloned());
            helpers::joined(&var.base[..cut], &overrides, ':')
        }
    };
    let written = match written {
        Some(w) => w.to_string(),
        // The parser split the legacy name: base + overrides (legacy ones first, then any
        // written with `:`).
        None if legacy => helpers::joined(&var.base, &var.overrides, '_'),
        None => helpers::joined(&var.base, &var.overrides, ':'),
    };
    let parens = if is_fn { "()" } else { "" };
    format!(
        "legacy `_` override syntax in `{written}{flag}{parens}`, which current BitBake \
         rejects or treats as a different variable; write `{modern}{flag}{parens}`"
    )
}

/// The name at the start of a statement's text (after `export`, `python`, `fakeroot`),
/// without its flag; `None` if there is none.
fn written_name(text: &str) -> Option<&str> {
    let mut t = text.trim_start();
    loop {
        let before = t.len();
        for kw in ["export", "python", "fakeroot"] {
            if let Some(rest) = t.strip_prefix(kw)
                && rest.starts_with(char::is_whitespace)
            {
                t = rest.trim_start();
            }
        }
        if t.len() == before {
            break;
        }
    }
    let end = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || "-_+.${}/~:".contains(*c)))
        .map_or(t.len(), |(i, _)| i);
    let name = &t[..end];
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> KnownOverrides {
        let mut k = KnownOverrides::default();
        k.machines.insert("qemux86".into());
        k.distros.insert("poky".into());
        k
    }

    #[test]
    fn suffix_detection() {
        let k = known();
        let cols = vec!["core".to_string(), "poky".to_string()];
        let f = |b: &str| override_suffix(b, &k, &cols);
        assert_eq!(f("KERNEL_DEVICETREE_qemux86"), Some(17));
        assert_eq!(f("EXTRA_OECONF_class-native"), Some(12));
        assert_eq!(f("SRCREV_machine"), None);
        assert_eq!(f("SRCREV_qemux86"), None);
        assert_eq!(f("BBFILE_PRIORITY_qemux86"), None);
        assert_eq!(
            f("FOO_poky"),
            None,
            "a collection name is never an override"
        );
        assert_eq!(f("qemux86"), None);
        assert_eq!(f("_qemux86"), None);
        assert_eq!(f("FOO_${MACHINE}"), None);
        assert_eq!(f("PACKAGECONFIG_CONFARGS"), None);
    }

    #[test]
    fn written_forms() {
        let v = crate::parse::split_var("RDEPENDS_${PN}", None);
        assert_eq!(
            message(&v, None, false, None, true),
            "legacy `_` override syntax in `RDEPENDS_${PN}`, which current BitBake rejects \
             or treats as a different variable; write `RDEPENDS:${PN}`"
        );
        let v = crate::parse::split_var("FOO_qemux86", None);
        assert!(message(&v, Some(3), false, None, false).ends_with("write `FOO:qemux86`"));
        assert_eq!(
            written_name("export DEPENDS_append = \" x\""),
            Some("DEPENDS_append")
        );
        assert_eq!(
            written_name("fakeroot python pkg_postinst_${PN}() {"),
            Some("pkg_postinst_${PN}")
        );
        assert_eq!(
            written_name("SRC_URI_append[md5] = \"x\""),
            Some("SRC_URI_append")
        );
    }
}
