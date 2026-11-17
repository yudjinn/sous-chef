//! `org/layer-conf`: Problems in `conf/layer.conf` (severity per check).
//!
//! See docs/lint-rules.md. Several checks under one id; the message names the problem. The
//! pointer is the relevant statement of `conf/layer.conf` (line 1 when there is none).
//! BitBake behaviour checked in `lib/bb/cookerdata.py` (`parseConfigurationFiles`) and
//! `lib/bb/cooker.py` (`handleCollections`) on master (2026-11) and branches `2.0`
//! (Kirkstone) and `2.8` (Scarthgap), which agree on everything used here.
//!
//! - **No `BBFILE_COLLECTIONS`** (warning): the layer's recipes get no priority and none of
//!   the per-collection checks below run in BitBake either.
//! - **Collection name clash** (error): another layer of the workspace adds the same
//!   collection; BitBake stops ("Found duplicated BBFILE_COLLECTIONS").
//! - **`LAYERSERIES_COMPAT_<c>`** (first collection only, as `Layer::compat`):
//!   - not set, or empty (warning): BitBake warns on every run ("should set
//!     LAYERSERIES_COMPAT_<c>") and accepts the layer;
//!   - copies another layer's value (`${LAYERSERIES_COMPAT_...}`, warning): BitBake deletes
//!     each layer's value after its `layer.conf`, so the copy expands to nothing;
//!   - shares no name with `LAYERSERIES_CORENAMES` of the core layer (error): BitBake stops
//!     ("Layer <c> is not compatible with the core layer");
//!   - with no core layer in the workspace (no `LAYERSERIES_CORENAMES`): when it does not
//!     name the workspace's release and that release was configured or detected (not
//!     assumed), a warning, since BitBake would refuse the layer against that core;
//!   - a name older than the supported floor (Kirkstone), e.g. `dunfell` (info): lint does not
//!     check the layer against it; harmless to BitBake when another name matches;
//!   - a name that is not a known release, older or supported (info): probably a typo, or a
//!     release newer than sous-chef's table; BitBake only compares names.
//! - **`LAYERDEPENDS` names a collection no layer adds** (error): BitBake stops ("depends on
//!   layer '<d>', but this layer is not enabled in your configuration").
//! - **Missing `LAYERDEPENDS`** (warning): a file of the layer inherits (resolved index
//!   edge) a class from another layer whose collections are not among the layer's
//!   `LAYERDEPENDS`, followed transitively through the other layers' `LAYERDEPENDS`. One
//!   finding per missing collection.
//! - **`BBFILES` misses `.bbappend`** (warning): a `.bbappend` of the layer (outside
//!   `dynamic-layers/`, which `BBFILES_DYNAMIC` handles) is not collected by any `BBFILES`
//!   glob (`workspace::resolve::recipe_files`, BitBake's `collect_bbfiles`), so BitBake
//!   never applies it.
//! - **`BBPATH` not extended with `LAYERDIR`** (error when the layer has classes or `.conf`
//!   files other than `conf/layer.conf`, which BitBake then cannot find; warning otherwise):
//!   no `BBPATH` statement of `conf/layer.conf` mentions `LAYERDIR` and the layer root is not
//!   in the workspace's configured `BBPATH`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::index::query;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::release::{Release, ReleaseSource};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::{Layer, layers, resolve};

pub struct OrgLayerConf;

struct Conf<'a> {
    rule: &'a OrgLayerConf,
    layer: &'a Layer,
    stmts: Vec<query::StatementRow>,
}

impl Conf<'_> {
    fn conf_loc(&self, line: u32) -> Loc {
        Loc {
            layer: self.layer.name.clone(),
            path: PathBuf::from("conf/layer.conf"),
            lines: LineSpan::line(line),
        }
    }

    /// The first statement whose name (as written) or base is `name`.
    fn stmt(&self, name: &str) -> Option<&query::StatementRow> {
        self.stmts.iter().find(|s| {
            s.name.as_deref().map(|n| n.split(':').next().unwrap_or(n)) == Some(name)
                || (s.base.as_deref() == Some(name) && s.overrides.is_empty())
        })
    }

    fn line_of(&self, name: &str) -> u32 {
        self.stmt(name).map_or(1, |s| s.loc.lines.start)
    }

    fn push(&self, out: &mut Vec<Finding>, severity: Severity, line: u32, message: String) {
        out.push(Finding {
            rule: self.rule.id(),
            severity,
            loc: self.conf_loc(line),
            message,
        });
    }
}

/// Collections reachable through `LAYERDEPENDS` from `start`, transitively.
fn reachable(cx: &LintContext<'_>, start: &[String]) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = start.to_vec();
    while let Some(c) = stack.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        for l in cx.ws.layers.iter().filter(|l| l.collections.contains(&c)) {
            stack.extend(l.depends.iter().cloned());
        }
    }
    seen
}

impl OrgLayerConf {
    fn collections(&self, conf: &Conf<'_>, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let layer = conf.layer;
        if layer.collections.is_empty() {
            conf.push(
                out,
                Severity::Warning,
                1,
                "conf/layer.conf adds no BBFILE_COLLECTIONS, so the layer has no collection, \
                 priority or compatibility check; add BBFILE_COLLECTIONS += \"<name>\" with \
                 BBFILE_PATTERN_<name> and BBFILE_PRIORITY_<name>"
                    .to_string(),
            );
            return;
        }
        for c in &layer.collections {
            let others: Vec<&str> = cx
                .ws
                .layers
                .iter()
                .filter(|l| l.name != layer.name && l.collections.contains(c))
                .map(|l| l.name.as_str())
                .collect();
            if !others.is_empty() {
                conf.push(
                    out,
                    Severity::Error,
                    conf.line_of("BBFILE_COLLECTIONS"),
                    format!(
                        "collection `{c}` is also added by {}; BitBake stops with \"Found \
                         duplicated BBFILE_COLLECTIONS\"; give this layer a collection name of \
                         its own",
                        others.join(", ")
                    ),
                );
            }
        }
    }

    fn compat(&self, conf: &Conf<'_>, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let layer = conf.layer;
        let Some(c) = layer.collections.first() else {
            return;
        };
        let var = format!("LAYERSERIES_COMPAT_{c}");
        let stmt = conf.stmt(&var);
        let line = stmt.map_or(1, |s| s.loc.lines.start);
        let ws_release = cx.ws.release.release.codename();
        if stmt
            .and_then(|s| s.value.as_deref())
            .is_some_and(|v| v.contains("${LAYERSERIES_COMPAT_"))
        {
            conf.push(
                out,
                Severity::Warning,
                line,
                format!(
                    "{var} copies another layer's LAYERSERIES_COMPAT, which BitBake deletes once \
                     that layer.conf is read, so it expands to nothing; list the release names \
                     this layer supports, e.g. {var} = \"{ws_release}\""
                ),
            );
            return;
        }
        if layer.compat.is_empty() {
            conf.push(
                out,
                Severity::Warning,
                line,
                format!(
                    "{var} is not set, so BitBake warns on every run and cannot tell which \
                     releases the layer supports; add {var} = \"{ws_release}\" (the releases \
                     you test it with)"
                ),
            );
            return;
        }
        let words: Vec<&str> = layer
            .compat
            .iter()
            .map(String::as_str)
            .filter(|w| !w.contains("${"))
            .collect();
        let roots: Vec<PathBuf> = cx.ws.layers.iter().map(|l| l.root.clone()).collect();
        let corenames =
            layers::read_corenames(cx.ws.build_dir.as_deref(), &roots).unwrap_or_default();
        let claimed = layer.compat.join(" ");
        if !corenames.is_empty() {
            if !words.iter().any(|w| corenames.iter().any(|c| c == w)) {
                let newest = corenames
                    .iter()
                    .filter_map(|n| Release::from_codename(n))
                    .max()
                    .map_or_else(
                        || corenames.last().cloned().unwrap_or_default(),
                        |r| r.codename().to_string(),
                    );
                conf.push(
                    out,
                    Severity::Error,
                    line,
                    format!(
                        "{var} = \"{claimed}\" names none of the core layer's releases \
                         (LAYERSERIES_CORENAMES \"{}\"), so BitBake stops with \"Layer {c} is \
                         not compatible with the core layer\"; add {newest} once the layer \
                         works with it",
                        corenames.join(" ")
                    ),
                );
            }
        } else if cx.ws.release.source != ReleaseSource::Assumed && !words.contains(&ws_release) {
            conf.push(
                out,
                Severity::Warning,
                line,
                format!(
                    "{var} = \"{claimed}\" does not name {ws_release}, the release this \
                     workspace targets (from {}), so BitBake would refuse the layer with that \
                     core; add {ws_release} once the layer works with it",
                    cx.ws.release.source.as_str()
                ),
            );
        }
        for w in words {
            if Release::from_codename(w).is_some() {
                continue;
            }
            let message = if Release::is_older_than_floor(w) {
                format!(
                    "{var} names {w}, which is older than {}, the oldest release sous-chef \
                     checks: lint does not check the layer against it; drop {w} if the layer no \
                     longer supports it",
                    Release::FLOOR.codename()
                )
            } else {
                format!(
                    "{var} names {w}, which is not a Yocto release name sous-chef knows ({} to \
                     {}, or an older one); check the spelling",
                    Release::FLOOR.codename(),
                    Release::NEWEST.codename()
                )
            };
            conf.push(out, Severity::Info, line, message);
        }
    }

    fn depends(&self, conf: &Conf<'_>, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let layer = conf.layer;
        let Some(c) = layer.collections.first() else {
            return;
        };
        let Ok(files) = query::files_in_layer(cx.index, &layer.name) else {
            return;
        };
        let var = format!("LAYERDEPENDS_{c}");
        for d in &layer.depends {
            if !cx.ws.layers.iter().any(|l| l.collections.contains(d)) {
                conf.push(
                    out,
                    Severity::Error,
                    conf.line_of(&var),
                    format!(
                        "{var} names `{d}`, which no layer of the workspace adds, so BitBake \
                         stops with \"Layer '{c}' depends on layer '{d}', but this layer is not \
                         enabled\"; add that layer to BBLAYERS or drop `{d}`"
                    ),
                );
            }
        }
        let have = reachable(cx, &layer.depends);
        // Missing collection -> (other layer, the first inherit that needs it).
        let mut missing: Vec<(String, String, Loc)> = Vec::new();
        for f in &files {
            let Ok(edges) = query::edges_from(cx.index, f.id) else {
                continue;
            };
            for e in edges {
                let Some(t) = &e.target else { continue };
                if e.kind != "inherit" || t.layer == layer.name {
                    continue;
                }
                let Some(other) = cx.ws.layer(&t.layer) else {
                    continue;
                };
                if other.collections.is_empty()
                    || other.collections.iter().any(|oc| have.contains(oc))
                {
                    continue;
                }
                let oc = other.collections[0].clone();
                if !missing.iter().any(|(m, _, _)| *m == oc) {
                    missing.push((oc, other.name.clone(), e.via()));
                }
            }
        }
        for (oc, other, via) in missing {
            conf.push(
                out,
                Severity::Warning,
                conf.line_of(&var),
                format!(
                    "{via} inherits a class from {other} (collection `{oc}`), but {var} does not \
                     name `{oc}`; add it: {var} += \"{oc}\""
                ),
            );
        }
    }

    fn bbfiles(&self, conf: &Conf<'_>, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let layer = conf.layer;
        let Ok(files) = query::files_in_layer(cx.index, &layer.name) else {
            return;
        };
        let appends: Vec<&query::FileRow> = files
            .iter()
            .filter(|f| f.kind == FileKind::Append)
            .filter(|f| !f.path.starts_with("dynamic-layers"))
            .collect();
        if appends.is_empty() {
            return;
        }
        let collected: BTreeSet<PathBuf> = resolve::recipe_files(cx.ws).into_iter().collect();
        let missed: Vec<&&query::FileRow> = appends
            .iter()
            .filter(|f| !collected.contains(&layer.root.join(&f.path)))
            .collect();
        let Some(first) = missed.first() else { return };
        let more = if missed.len() > 1 {
            format!(" and {} more", missed.len() - 1)
        } else {
            String::new()
        };
        conf.push(
            out,
            Severity::Warning,
            conf.line_of("BBFILES"),
            format!(
                "BBFILES of this layer does not match {}{more}, so BitBake never applies it; \
                 add ${{LAYERDIR}}/recipes-*/*/*.bbappend (or a glob that matches it) to BBFILES",
                first.path.display()
            ),
        );
    }

    fn bbpath(&self, conf: &Conf<'_>, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let layer = conf.layer;
        let extends = conf.stmts.iter().any(|s| {
            s.base.as_deref() == Some("BBPATH")
                && s.value.as_deref().is_some_and(|v| v.contains("LAYERDIR"))
        });
        if extends || resolve::bbpath(cx.ws).contains(&layer.root) {
            return;
        }
        let needs = query::files_in_layer(cx.index, &layer.name)
            .unwrap_or_default()
            .iter()
            .any(|f| {
                f.kind == FileKind::Class
                    || (f.kind == FileKind::Conf && f.path != Path::new("conf/layer.conf"))
            });
        let (severity, why) = if needs {
            (
                Severity::Error,
                "so BitBake cannot find this layer's classes and conf files",
            )
        } else {
            (
                Severity::Warning,
                "so classes or conf files added to it later will not be found",
            )
        };
        conf.push(
            out,
            severity,
            conf.line_of("BBPATH"),
            format!(
                "conf/layer.conf does not add ${{LAYERDIR}} to BBPATH, {why}; add \
                 BBPATH .= \":${{LAYERDIR}}\""
            ),
        );
    }
}

impl Rule for OrgLayerConf {
    fn id(&self) -> &'static str {
        "org/layer-conf"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(name) = cx.layer else { return };
        let Some(layer) = cx.ws.layer(name) else {
            return;
        };
        let stmts = query::file_by_path(cx.index, name, Path::new("conf/layer.conf"))
            .ok()
            .flatten()
            .and_then(|f| {
                query::statements_in(cx.index, f.id, &query::StatementFilter::default()).ok()
            })
            .unwrap_or_default();
        let conf = Conf {
            rule: self,
            layer,
            stmts,
        };
        self.collections(&conf, cx, out);
        self.compat(&conf, cx, out);
        self.depends(&conf, cx, out);
        self.bbfiles(&conf, cx, out);
        self.bbpath(&conf, cx, out);
    }
}
