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
//!   - copies another layer's value (`${LAYERSERIES_COMPAT_...}`, error): BitBake 2.4
//!     (Mickledore) and later delete each layer's `LAYERSERIES_COMPAT_<c>` right after
//!     reading its `layer.conf` and set them again only after the last one (`cookerdata.py`,
//!     commit 6709aedcc "cookerdata: Ensure layers use LAYERSERIES_COMPAT fairly", 2022-12),
//!     so the reference is undefined when the copy is expanded, is left as written
//!     (`data_smart.py` `var_sub`), names no release, and BitBake stops ("Layer <c> is not
//!     compatible with the core layer", or "No core layer found" without one). BitBake 2.0
//!     (Kirkstone) and 2.2 (Langdale) delete nothing and expand the copy at the check, so
//!     the layer is accepted there: a warning when the workspace release is one of those;
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
//!
//! The checks are public through [`layer_problems`], which the rule turns into findings and
//! `layers show=compat` (`mcp::tools::layers`) into rows, so the two never disagree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::index::{Index, query};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::release::{Release, ReleaseSource};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::{Layer, Workspace, layers, resolve};

pub struct OrgLayerConf;

/// The first release whose BitBake (2.4) deletes each layer's `LAYERSERIES_COMPAT_<c>`
/// after its `layer.conf` (BitBake commit 6709aedcc; not in its `2.0` or `2.2` branches).
pub const COMPAT_DELETED_FROM: Release = Release::Mickledore;

/// Which check of `org/layer-conf` found a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerCheck {
    /// `conf/layer.conf` adds no `BBFILE_COLLECTIONS`.
    NoCollections,
    /// Another layer adds the same collection.
    DuplicateCollection,
    /// `LAYERSERIES_COMPAT_<c>` copies another layer's value.
    CompatCopied,
    /// `LAYERSERIES_COMPAT_<c>` is not set or empty.
    CompatMissing,
    /// No name in common with the core layer's `LAYERSERIES_CORENAMES`.
    CompatNotCore,
    /// No core layer, and the workspace release is not named.
    CompatNotWorkspace,
    /// A name older than the supported floor.
    CompatOlderThanFloor,
    /// A name that is not a known release.
    CompatUnknownName,
    /// `LAYERDEPENDS` names a collection no layer adds.
    DependsNotEnabled,
    /// A file inherits a class from a layer `LAYERDEPENDS` does not reach.
    DependsMissing,
    /// `BBFILES` does not collect a `.bbappend` of the layer.
    BbfilesMissesAppend,
    /// `BBPATH` is not extended with `LAYERDIR`.
    BbpathMissing,
}

impl LayerCheck {
    /// `collections`, `compat`, `depends`, `bbfiles` or `bbpath`.
    pub fn group(self) -> &'static str {
        match self {
            LayerCheck::NoCollections | LayerCheck::DuplicateCollection => "collections",
            LayerCheck::CompatCopied
            | LayerCheck::CompatMissing
            | LayerCheck::CompatNotCore
            | LayerCheck::CompatNotWorkspace
            | LayerCheck::CompatOlderThanFloor
            | LayerCheck::CompatUnknownName => "compat",
            LayerCheck::DependsNotEnabled | LayerCheck::DependsMissing => "depends",
            LayerCheck::BbfilesMissesAppend => "bbfiles",
            LayerCheck::BbpathMissing => "bbpath",
        }
    }
}

/// One problem found in a layer's configuration: the finding `org/layer-conf` reports,
/// before rule id, severity configuration and suppression are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerProblem {
    pub check: LayerCheck,
    pub severity: Severity,
    /// The relevant statement of `conf/layer.conf` (line 1 when there is none).
    pub loc: Loc,
    /// One line: what is wrong, then what to write instead.
    pub message: String,
}

/// Every problem `org/layer-conf` finds in layer `name` of the workspace, in check order
/// (collections, compat, depends, bbfiles, bbpath). Empty for a layer the workspace does
/// not have. The index must be refreshed.
pub fn layer_problems(ws: &Workspace, index: &Index, name: &str) -> Vec<LayerProblem> {
    let Some(layer) = ws.layer(name) else {
        return Vec::new();
    };
    let stmts = query::file_by_path(index, name, Path::new("conf/layer.conf"))
        .ok()
        .flatten()
        .and_then(|f| query::statements_in(index, f.id, &query::StatementFilter::default()).ok())
        .unwrap_or_default();
    let conf = Conf {
        ws,
        index,
        layer,
        stmts,
    };
    let mut out = Vec::new();
    conf.collections(&mut out);
    conf.compat(&mut out);
    conf.depends(&mut out);
    conf.bbfiles(&mut out);
    conf.bbpath(&mut out);
    out
}

struct Conf<'a> {
    ws: &'a Workspace,
    index: &'a Index,
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

    fn push(
        &self,
        out: &mut Vec<LayerProblem>,
        check: LayerCheck,
        severity: Severity,
        line: u32,
        message: String,
    ) {
        out.push(LayerProblem {
            check,
            severity,
            loc: self.conf_loc(line),
            message,
        });
    }

    fn collections(&self, out: &mut Vec<LayerProblem>) {
        let layer = self.layer;
        if layer.collections.is_empty() {
            self.push(
                out,
                LayerCheck::NoCollections,
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
            let others: Vec<&str> = self
                .ws
                .layers
                .iter()
                .filter(|l| l.name != layer.name && l.collections.contains(c))
                .map(|l| l.name.as_str())
                .collect();
            if !others.is_empty() {
                self.push(
                    out,
                    LayerCheck::DuplicateCollection,
                    Severity::Error,
                    self.line_of("BBFILE_COLLECTIONS"),
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

    fn compat(&self, out: &mut Vec<LayerProblem>) {
        let layer = self.layer;
        let Some(c) = layer.collections.first() else {
            return;
        };
        let var = format!("LAYERSERIES_COMPAT_{c}");
        let stmt = self.stmt(&var);
        let line = stmt.map_or(1, |s| s.loc.lines.start);
        let ws_release = self.ws.release.release.codename();
        if let Some(raw) = stmt
            .and_then(|s| s.value.as_deref())
            .filter(|v| v.contains("${LAYERSERIES_COMPAT_"))
        {
            let names = self.copied_names(raw).join(" ");
            // cookerdata.py: with no LAYERSERIES_CORENAMES the fatal error is "No core layer
            // found", else "not compatible with the core layer".
            let stop = if core_names(self.ws).is_empty() {
                format!("\"No core layer found to work with layer '{c}'\"")
            } else {
                format!("\"Layer {c} is not compatible with the core layer\"")
            };
            let (severity, effect) = if self.ws.release.release >= COMPAT_DELETED_FROM {
                (
                    Severity::Error,
                    format!(
                        "BitBake deletes every layer's LAYERSERIES_COMPAT_<collection> right \
                         after reading its layer.conf and sets them again only after the last \
                         one, so the reference is undefined when this value is expanded, stays \
                         literal and names no release, and BitBake stops with {stop}"
                    ),
                )
            } else {
                (
                    Severity::Warning,
                    format!(
                        "the BitBake of {ws_release} still expands it to that layer's value, but \
                         BitBake 2.4 ({}) and later delete every layer's \
                         LAYERSERIES_COMPAT_<collection> after reading its layer.conf, so there \
                         the reference stays literal, names no release, and BitBake stops with \
                         {stop}",
                        COMPAT_DELETED_FROM.codename()
                    ),
                )
            };
            self.push(
                out,
                LayerCheck::CompatCopied,
                severity,
                line,
                format!(
                    "{var} = \"{raw}\" copies another layer's LAYERSERIES_COMPAT; {effect}; \
                     write the release names themselves: {var} = \"{names}\" (the releases \
                     this layer is tested with)"
                ),
            );
            return;
        }
        if layer.compat.is_empty() {
            self.push(
                out,
                LayerCheck::CompatMissing,
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
        let corenames = core_names(self.ws);
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
                self.push(
                    out,
                    LayerCheck::CompatNotCore,
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
        } else if self.ws.release.source != ReleaseSource::Assumed && !words.contains(&ws_release) {
            self.push(
                out,
                LayerCheck::CompatNotWorkspace,
                Severity::Warning,
                line,
                format!(
                    "{var} = \"{claimed}\" does not name {ws_release}, the release this \
                     workspace targets (from {}), so BitBake would refuse the layer with that \
                     core; add {ws_release} once the layer works with it",
                    self.ws.release.source.as_str()
                ),
            );
        }
        for w in words {
            if Release::from_codename(w).is_some() {
                continue;
            }
            let (check, message) = if Release::is_older_than_floor(w) {
                (
                    LayerCheck::CompatOlderThanFloor,
                    format!(
                        "{var} names {w}, which is older than {}, the oldest release sous-chef \
                         checks: lint does not check the layer against it; drop {w} if the layer no \
                         longer supports it",
                        Release::FLOOR.codename()
                    ),
                )
            } else {
                (
                    LayerCheck::CompatUnknownName,
                    format!(
                        "{var} names {w}, which is not a Yocto release name sous-chef knows ({} to \
                         {}, or an older one); check the spelling",
                        Release::FLOOR.codename(),
                        Release::NEWEST.codename()
                    ),
                )
            };
            self.push(out, check, Severity::Info, line, message);
        }
    }

    /// The release names to suggest in place of a copied `LAYERSERIES_COMPAT`: the
    /// referenced collections' own names when the workspace knows them, else the core
    /// layer's `LAYERSERIES_CORENAMES`, else the workspace release.
    fn copied_names(&self, raw: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for part in raw.split("${LAYERSERIES_COMPAT_").skip(1) {
            let Some((referenced, _)) = part.split_once('}') else {
                continue;
            };
            let Some(l) = self
                .ws
                .layers
                .iter()
                .find(|l| l.collections.first().is_some_and(|c| c == referenced))
            else {
                continue;
            };
            for w in &l.compat {
                if !w.contains("${") && !names.contains(w) {
                    names.push(w.clone());
                }
            }
        }
        if names.is_empty() {
            names = core_names(self.ws);
        }
        if names.is_empty() {
            names.push(self.ws.release.release.codename().to_string());
        }
        names
    }

    fn depends(&self, out: &mut Vec<LayerProblem>) {
        let layer = self.layer;
        let Some(c) = layer.collections.first() else {
            return;
        };
        let Ok(files) = query::files_in_layer(self.index, &layer.name) else {
            return;
        };
        let var = format!("LAYERDEPENDS_{c}");
        for d in &layer.depends {
            if !self.ws.layers.iter().any(|l| l.collections.contains(d)) {
                self.push(
                    out,
                    LayerCheck::DependsNotEnabled,
                    Severity::Error,
                    self.line_of(&var),
                    format!(
                        "{var} names `{d}`, which no layer of the workspace adds, so BitBake \
                         stops with \"Layer '{c}' depends on layer '{d}', but this layer is not \
                         enabled\"; add that layer to BBLAYERS or drop `{d}`"
                    ),
                );
            }
        }
        let have = reachable(self.ws, &layer.depends);
        // Missing collection -> (other layer, the first inherit that needs it).
        let mut missing: Vec<(String, String, Loc)> = Vec::new();
        for f in &files {
            let Ok(edges) = query::edges_from(self.index, f.id) else {
                continue;
            };
            for e in edges {
                let Some(t) = &e.target else { continue };
                if e.kind != "inherit" || t.layer == layer.name {
                    continue;
                }
                let Some(other) = self.ws.layer(&t.layer) else {
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
            self.push(
                out,
                LayerCheck::DependsMissing,
                Severity::Warning,
                self.line_of(&var),
                format!(
                    "{via} inherits a class from {other} (collection `{oc}`), but {var} does not \
                     name `{oc}`; add it: {var} += \"{oc}\""
                ),
            );
        }
    }

    fn bbfiles(&self, out: &mut Vec<LayerProblem>) {
        let layer = self.layer;
        let Ok(files) = query::files_in_layer(self.index, &layer.name) else {
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
        let collected: BTreeSet<PathBuf> = resolve::recipe_files(self.ws).into_iter().collect();
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
        self.push(
            out,
            LayerCheck::BbfilesMissesAppend,
            Severity::Warning,
            self.line_of("BBFILES"),
            format!(
                "BBFILES of this layer does not match {}{more}, so BitBake never applies it; \
                 add ${{LAYERDIR}}/recipes-*/*/*.bbappend (or a glob that matches it) to BBFILES",
                first.path.display()
            ),
        );
    }

    fn bbpath(&self, out: &mut Vec<LayerProblem>) {
        let layer = self.layer;
        let extends = self.stmts.iter().any(|s| {
            s.base.as_deref() == Some("BBPATH")
                && s.value.as_deref().is_some_and(|v| v.contains("LAYERDIR"))
        });
        if extends || resolve::bbpath(self.ws).contains(&layer.root) {
            return;
        }
        let needs = query::files_in_layer(self.index, &layer.name)
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
        self.push(
            out,
            LayerCheck::BbpathMissing,
            severity,
            self.line_of("BBPATH"),
            format!(
                "conf/layer.conf does not add ${{LAYERDIR}} to BBPATH, {why}; add \
                 BBPATH .= \":${{LAYERDIR}}\""
            ),
        );
    }
}

/// `LAYERSERIES_CORENAMES` of the workspace's core layer (empty when no layer sets it).
pub fn core_names(ws: &Workspace) -> Vec<String> {
    let roots: Vec<PathBuf> = ws.layers.iter().map(|l| l.root.clone()).collect();
    layers::read_corenames(ws.build_dir.as_deref(), &roots).unwrap_or_default()
}

/// Collections reachable through `LAYERDEPENDS` from `start`, transitively.
fn reachable(ws: &Workspace, start: &[String]) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = start.to_vec();
    while let Some(c) = stack.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        for l in ws.layers.iter().filter(|l| l.collections.contains(&c)) {
            stack.extend(l.depends.iter().cloned());
        }
    }
    seen
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
        for p in layer_problems(cx.ws, cx.index, name) {
            out.push(Finding {
                rule: self.id(),
                severity: p.severity,
                loc: p.loc,
                message: p.message,
            });
        }
    }
}
