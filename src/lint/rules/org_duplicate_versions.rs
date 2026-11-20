//! `org/duplicate-versions`: Two or more versions of a recipe whose bodies are mostly
//! identical.
//!
//! See docs/lint-rules.md and T4.3's notes. For every `PN` with `.bb` files of at least two
//! versions in the layer, each file's statements are normalised: the statement text with
//! every line trimmed, blank and comment lines dropped and runs of whitespace collapsed.
//! Left out: `require` / `include` lines (versions that already share an `.inc` should not
//! count it as duplication) and version data (`PV`, `PR`, `PE`, `SRCREV*`,
//! `LIC_FILES_CHKSUM`, checksum flags of `SRC_URI`). Two versions are duplicates when at
//! least 60 percent of the statements of the longer one are identical (as a multiset) and
//! at least three are. Reported once per `PN`, on the newest file of the duplicates (by
//! `vercmp`), listing the others as pointers. A `.bb` BitBake does not collect is left
//! out, and two files of the same version are never compared (alternatives, such as a
//! `dynamic-layers/` copy, not versions).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{FileKind, LineSpan};

pub struct OrgDuplicateVersions;

const PERCENT: usize = 60;
const MIN_SHARED: usize = 3;

/// The normalised statements of a file; `None` if it cannot be read.
fn statements(cx: &LintContext<'_>, f: &query::FileRow) -> Option<Vec<String>> {
    let (text, parsed) = helpers::read_indexed(cx, f)?;
    let mut out = Vec::new();
    for st in &parsed.statements {
        match &st.kind {
            StatementKind::Include { .. } => continue,
            StatementKind::Assign { var, .. }
                if helpers::is_version_data(&var.base, var.flag.as_deref()) =>
            {
                continue;
            }
            _ => {}
        }
        let n = helpers::normalise(text.get(st.bytes.clone()).unwrap_or(""));
        if !n.is_empty() {
            out.push(n);
        }
    }
    Some(out)
}

/// Number of statements the two lists share, as multisets.
fn shared(a: &[String], b: &[String]) -> usize {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in a {
        *counts.entry(s.as_str()).or_default() += 1;
    }
    let mut n = 0;
    for s in b {
        if let Some(c) = counts.get_mut(s.as_str())
            && *c > 0
        {
            *c -= 1;
            n += 1;
        }
    }
    n
}

impl Rule for OrgDuplicateVersions {
    fn id(&self) -> &'static str {
        "org/duplicate-versions"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(layer) = cx.layer else { return };
        let Ok(files) = query::files_in_layer(cx.index, layer) else {
            return;
        };
        // A `.bb` BitBake does not collect is no version of anything (meta-oe's
        // `dynamic-layers/selinux/` recipes without the selinux layer: lint-precision).
        let uncollected: HashSet<i64> = query::uncollected_recipe_files(cx.index)
            .map(|v| v.into_iter().map(|(f, _)| f.id).collect())
            .unwrap_or_default();
        let mut by_pn: BTreeMap<String, Vec<query::FileRow>> = BTreeMap::new();
        for f in files {
            if f.kind == FileKind::Recipe
                && !uncollected.contains(&f.id)
                && let (Some(pn), Some(_)) = (f.pn.clone(), f.pv.as_ref())
            {
                by_pn.entry(pn).or_default().push(f);
            }
        }
        for (pn, mut versions) in by_pn {
            if versions.len() < 2 {
                continue;
            }
            // Newest first.
            versions.sort_by(|a, b| {
                query::vercmp(b.pv.as_deref().unwrap_or(""), a.pv.as_deref().unwrap_or(""))
                    .then_with(|| a.path.cmp(&b.path))
            });
            let bodies: Vec<Option<Vec<String>>> =
                versions.iter().map(|f| statements(cx, f)).collect();
            let mut dup = vec![false; versions.len()];
            let mut best: Option<(usize, usize)> = None;
            for i in 0..versions.len() {
                for j in i + 1..versions.len() {
                    let (Some(a), Some(b)) = (&bodies[i], &bodies[j]) else {
                        continue;
                    };
                    // Two files of the same version are alternatives (a `dynamic-layers/`
                    // copy that replaces the recipe when its collection is present), not
                    // versions to share an `.inc`.
                    if versions[i].pv == versions[j].pv {
                        continue;
                    }
                    let n = shared(a, b);
                    let longer = a.len().max(b.len());
                    if n >= MIN_SHARED && n * 100 >= PERCENT * longer {
                        dup[i] = true;
                        dup[j] = true;
                        if best.is_none_or(|(bn, _)| n > bn) {
                            best = Some((n, longer));
                        }
                    }
                }
            }
            let Some(anchor) = dup.iter().position(|d| *d) else {
                continue;
            };
            let (n, longer) = best.unwrap_or((0, 0));
            let others: Vec<String> = versions
                .iter()
                .enumerate()
                .filter(|(k, _)| *k != anchor && dup[*k])
                .map(|(_, f)| f.loc(LineSpan::line(1)).to_string())
                .collect();
            let f = &versions[anchor];
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: f.loc(LineSpan::line(1)),
                message: format!(
                    "{name} repeats most of {} (up to {n} of {longer} statements identical, \
                     version data aside); move the shared statements into {pn}.inc and \
                     `require {pn}.inc` from each version",
                    others.join(", ")
                ),
            });
        }
    }
}
