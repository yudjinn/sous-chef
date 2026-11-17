//! `override/legacy-syntax`: Underscore override syntax (`VAR_append`, `RDEPENDS_${PN}`).
//!
//! See docs/lint-rules.md and docs/bitbake-semantics.md ("Legacy syntax"). Two sources:
//!
//! - The parser's `VarRef.legacy_syntax`, set on assignment and function names when an
//!   `_`-separated element is `append` / `prepend` / `remove`, or when a package-scoped
//!   variable is followed by `_${PN}` or `_<lowercase package>` (`split_var`).
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
        let Some(file) = cx.file else { return };
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
            if var.legacy_syntax {
                let written = written_name(helpers::statement_text(cx, s));
                out.push(helpers::finding(
                    self,
                    cx,
                    line,
                    message(var, None, is_fn, written),
                ));
                continue;
            }
            if !var.base.contains('_') {
                continue;
            }
            let (k, collections) = known.get_or_insert_with(|| load_known(cx));
            if let Some(cut) = override_suffix(&var.base, k, collections) {
                out.push(helpers::finding(
                    self,
                    cx,
                    line,
                    message(var, Some(cut), is_fn, None),
                ));
            }
        }
    }
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

/// The message: the name as written and the colon form to write instead.
fn message(var: &VarRef, suffix_at: Option<usize>, is_fn: bool, written: Option<&str>) -> String {
    let flag = var
        .flag
        .as_ref()
        .map(|f| format!("[{f}]"))
        .unwrap_or_default();
    let (written, modern) = match suffix_at {
        None => {
            // The parser split the legacy name: base + overrides (legacy ones first, then
            // any written with `:`).
            let written = written
                .map(str::to_string)
                .unwrap_or_else(|| joined(&var.base, &var.overrides, '_'));
            (written, joined(&var.base, &var.overrides, ':'))
        }
        Some(cut) => {
            let mut overrides: Vec<String> =
                var.base[cut + 1..].split('_').map(str::to_string).collect();
            overrides.extend(var.overrides.iter().cloned());
            (
                joined(&var.base, &var.overrides, ':'),
                joined(&var.base[..cut], &overrides, ':'),
            )
        }
    };
    let parens = if is_fn { "()" } else { "" };
    format!(
        "legacy `_` override syntax in `{written}{flag}{parens}`, which current BitBake \
         rejects or treats as a different variable; write `{modern}{flag}{parens}`"
    )
}

fn joined(base: &str, overrides: &[String], sep: char) -> String {
    let mut s = base.to_string();
    for o in overrides {
        s.push(sep);
        s.push_str(o);
    }
    s
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
            message(&v, None, false, None),
            "legacy `_` override syntax in `RDEPENDS_${PN}`, which current BitBake rejects \
             or treats as a different variable; write `RDEPENDS:${PN}`"
        );
        let v = crate::parse::split_var("FOO_qemux86", None);
        assert!(message(&v, Some(3), false, None).ends_with("write `FOO:qemux86`"));
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
