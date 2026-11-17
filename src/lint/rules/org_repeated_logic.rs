//! `org/repeated-logic`: The same function body or block of assignments in three or more
//! recipes of a layer.
//!
//! See docs/lint-rules.md and T4.3's notes. Looks at the `.bb`, `.bbappend` and `.inc` files
//! of the layer (not classes: a class is already the shared form). A "recipe" is a `PN`
//! for `.bb` / `.bbappend` (so the versions of one recipe count once; that case is
//! `org/duplicate-versions`) and the file itself for an `.inc`.
//!
//! - **Function bodies**: shell or python functions (named or anonymous) and top-level
//!   `def`s whose body, normalised (lines trimmed, blank and comment lines dropped), has at
//!   least 5 lines, identical in 3 or more recipes. Names may differ.
//! - **Blocks of assignments**: 5 consecutive assignments (no other statement between them;
//!   version data `PV`, `PR`, `PE`, `SRCREV*`, `LIC_FILES_CHKSUM` and `SRC_URI` checksum
//!   flags ignored), normalised the same way, identical in 3 or more recipes. Overlapping
//!   windows of one longer block are reported as one finding.
//!
//! Reported once per repeated item, on the first file in path order, with the others as
//! pointers (at most five, then a count).

use std::collections::{BTreeMap, BTreeSet};

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{FileKind, LineSpan};

pub struct OrgRepeatedLogic;

const MIN_BODY_LINES: usize = 5;
const BLOCK: usize = 5;
const MIN_RECIPES: usize = 3;
const MAX_LISTED: usize = 5;

/// One occurrence: file index, lines, and (for a body) the function name.
#[derive(Debug, Clone)]
struct Hit {
    file: usize,
    span: LineSpan,
    name: String,
}

struct Unit {
    row: query::FileRow,
    /// `PN` for recipes and appends, the path for an `.inc`.
    recipe: String,
}

/// The text of lines `span` (1-based, inclusive) of `text`.
fn lines_of(text: &str, span: LineSpan) -> String {
    if span.end < span.start {
        return String::new();
    }
    text.lines()
        .skip(span.start.saturating_sub(1) as usize)
        .take((span.end - span.start + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

fn pointer(units: &[Unit], h: &Hit) -> String {
    units[h.file].row.loc(h.span).to_string()
}

fn listed(units: &[Unit], hits: &[&Hit]) -> String {
    let mut v: Vec<String> = hits
        .iter()
        .take(MAX_LISTED)
        .map(|h| pointer(units, h))
        .collect();
    if hits.len() > MAX_LISTED {
        v.push(format!("{} more", hits.len() - MAX_LISTED));
    }
    v.join(", ")
}

/// Hits in distinct recipes, one per recipe (the first), or `None` below the threshold.
fn spread<'h>(units: &[Unit], hits: &'h [Hit]) -> Option<Vec<&'h Hit>> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    for h in hits {
        if seen.insert(units[h.file].recipe.as_str()) {
            out.push(h);
        }
    }
    (out.len() >= MIN_RECIPES).then_some(out)
}

impl Rule for OrgRepeatedLogic {
    fn id(&self) -> &'static str {
        "org/repeated-logic"
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
        let units: Vec<Unit> = files
            .into_iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    FileKind::Recipe | FileKind::Append | FileKind::Include
                )
            })
            .map(|row| Unit {
                recipe: match (&row.kind, &row.pn) {
                    (FileKind::Recipe | FileKind::Append, Some(pn)) => format!("pn:{pn}"),
                    _ => format!("file:{}", row.path.display()),
                },
                row,
            })
            .collect();

        let mut bodies: BTreeMap<Vec<String>, Vec<Hit>> = BTreeMap::new();
        let mut blocks: BTreeMap<Vec<String>, Vec<Hit>> = BTreeMap::new();
        // Per file: the block windows, in order, as (key, span).
        let mut windows: Vec<Vec<(Vec<String>, LineSpan)>> = Vec::new();
        for (i, u) in units.iter().enumerate() {
            let mut mine = Vec::new();
            let Some((text, parsed)) = helpers::read_indexed(cx, &u.row) else {
                windows.push(mine);
                continue;
            };
            let mut run: Vec<(String, LineSpan)> = Vec::new();
            let mut runs: Vec<Vec<(String, LineSpan)>> = Vec::new();
            for st in &parsed.statements {
                let (body, name) = match &st.kind {
                    StatementKind::Function { name, body, .. } => (
                        Some(*body),
                        name.as_ref()
                            .map_or("anonymous python".to_string(), |v| v.base.clone()),
                    ),
                    StatementKind::PyDef { name, body } => (Some(*body), name.clone()),
                    _ => (None, String::new()),
                };
                if let Some(body) = body {
                    let key = helpers::normalise_lines(&lines_of(&text, body));
                    if key.len() >= MIN_BODY_LINES {
                        bodies.entry(key).or_default().push(Hit {
                            file: i,
                            span: st.span,
                            name,
                        });
                    }
                }
                match &st.kind {
                    StatementKind::Assign { var, .. } => {
                        if !helpers::is_version_data(&var.base, var.flag.as_deref()) {
                            let t =
                                helpers::normalise_lines(text.get(st.bytes.clone()).unwrap_or(""))
                                    .join(" ");
                            run.push((t, st.span));
                        }
                    }
                    _ => runs.push(std::mem::take(&mut run)),
                }
            }
            runs.push(run);
            for r in runs {
                if r.len() < BLOCK {
                    continue;
                }
                for w in r.windows(BLOCK) {
                    let key: Vec<String> = w.iter().map(|(t, _)| t.clone()).collect();
                    let span = LineSpan::new(w[0].1.start, w[BLOCK - 1].1.end);
                    blocks.entry(key.clone()).or_default().push(Hit {
                        file: i,
                        span,
                        name: String::new(),
                    });
                    mine.push((key, span));
                }
            }
            windows.push(mine);
        }

        for (key, hits) in &bodies {
            let Some(spread) = spread(&units, hits) else {
                continue;
            };
            let first = spread[0];
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: units[first.file].row.loc(first.span),
                message: format!(
                    "the same {}-line body of {} is in {} recipes of this layer (also {}); move \
                     it into a bbclass (or a shared .inc) and inherit it",
                    key.len(),
                    first.name,
                    spread.len(),
                    listed(&units, &spread[1..])
                ),
            });
        }

        // Blocks: windows that repeat widely enough, by the file they are reported in (the
        // first in path order); consecutive windows of that file become one finding.
        let mut wide: BTreeMap<usize, BTreeMap<usize, Vec<&Hit>>> = BTreeMap::new();
        for (key, hits) in &blocks {
            let Some(spread) = spread(&units, hits) else {
                continue;
            };
            let anchor = spread[0];
            if let Some(idx) = windows[anchor.file]
                .iter()
                .position(|(k, s)| k == key && *s == anchor.span)
            {
                wide.entry(anchor.file).or_default().insert(idx, spread);
            }
        }
        for (file, by_idx) in &wide {
            let mut groups: Vec<(usize, usize)> = Vec::new();
            for &idx in by_idx.keys() {
                match groups.last_mut() {
                    Some((_, last)) if *last + 1 == idx => *last = idx,
                    _ => groups.push((idx, idx)),
                }
            }
            for (first, last) in groups {
                let span = LineSpan::new(windows[*file][first].1.start, windows[*file][last].1.end);
                let spread = &by_idx[&first];
                out.push(Finding {
                    rule: self.id(),
                    severity: self.default_severity(),
                    loc: units[*file].row.loc(span),
                    message: format!(
                        "the same block of assignments is in {} recipes of this layer (also \
                         {}); move it into a .inc (or a bbclass) that each recipe requires",
                        spread.len(),
                        listed(&units, &spread[1..])
                    ),
                });
            }
        }
    }
}
