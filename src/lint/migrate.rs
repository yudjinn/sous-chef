//! The release migration report: what breaks if a layer moves to a release it does not
//! claim yet (`sous-chef lint --migrate-to <codename>`, the `lint` tool's `migrate_to`, and
//! a section of the audit).
//!
//! Built from what lint already does, run twice on the same target:
//!
//! 1. as the layers stand (their own releases, [`super::Options`] without `as_release`), and
//! 2. against the target release alone (`as_release` with `only_release`).
//!
//! A finding of run 2 whose rule and pointer run 1 does not have is **new with** the
//! release; one run 1 also has **already holds**. Then, per layer, every `Feature` whose
//! value differs between the newest release the layer is checked against now and the target
//! is looked up in the layer's files ([`scan_feature_uses`], and `hygiene/release-removed`'s
//! uses for the `KeptUntil*` features), grouped per feature with its instruction. Last, each
//! layer's own `LAYERSERIES_COMPAT` line and what to write, and what the report cannot
//! tell. Nothing here compares codenames: it asks [`Release::has`].

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use serde::Serialize;

use super::rules::hygiene_release_removed::HygieneReleaseRemoved;
use super::{Finding, LintContext, Options, ReleaseCache, Target};
use crate::config::Config;
use crate::error::Result;
use crate::index::{Index, query};
use crate::parse::{IncludeMode, ParsedFile, StatementKind, words};
use crate::release::{Feature, LintReach, Release};
use crate::render::Format;
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::resolve::{self, ClassDir};
use crate::workspace::{BUILD_LAYER, Workspace};

/// The `schema` field of the JSON report.
pub const SCHEMA: &str = "sous-chef-migration/1";

/// Uses listed per feature group in the text output; the rest are counted.
pub const MAX_POINTERS: usize = 20;

/// What the report cannot tell, in every report.
pub const CANNOT_TELL: [&str; 3] = [
    "anything that needs a build: QA checks that run in tasks (do_qa_unpack on the unpacked tree, buildpaths, patch fuzz), fetch and unpack results, task signatures, and recipes or classes that the target release's OE-core and the other upstream layers removed or renamed (a DEPENDS on a recipe that is gone)",
    "values computed by python (`${@...}`, anonymous python) are not evaluated, so a removed name built by python is not seen",
    "only the changes in sous-chef's release table (bitbake-semantics.md, \"Releases\"; lint-rules.md, \"Release removals\" lists the migration-guide changes it does not detect)",
];

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Migration {
    pub schema: String,
    /// The target release.
    pub to: Release,
    pub to_version: String,
    pub summary: String,
    /// Per layer of the target: its `LAYERSERIES_COMPAT` and what to write.
    pub layers: Vec<LayerCompat>,
    /// Findings that hold for the target release and not for the layers' releases now.
    pub new_findings: Vec<Finding>,
    /// Findings that hold for the target release and already hold now.
    pub existing_findings: Vec<Finding>,
    /// The features the target release changed that the layers use.
    pub features: Vec<FeatureChange>,
    pub cannot_tell: Vec<String>,
    pub notes: Vec<String>,
    /// Files checked.
    pub files: usize,
}

/// One layer's `LAYERSERIES_COMPAT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerCompat {
    pub layer: String,
    /// The variable, `LAYERSERIES_COMPAT_<collection>`; `None` without a collection.
    pub variable: Option<String>,
    /// Its statement, `None` when the layer does not set it.
    pub pointer: Option<String>,
    /// The names it claims now.
    pub compat: Vec<String>,
    /// The releases the layer is checked against now.
    pub releases: Vec<Release>,
    /// Whether it already claims the target.
    pub claims_target: bool,
    /// One line: what to change it to.
    pub advice: String,
}

/// A feature the target changed, with its uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeatureChange {
    /// The `Feature` (as in `src/release.rs`).
    pub feature: String,
    /// Whether the layers' newest release has it, and whether the target has it.
    pub now: bool,
    pub target: bool,
    /// One line: what changed, what to do (from the migration guides).
    pub instruction: String,
    pub groups: Vec<UseGroup>,
}

/// The uses of one thing under a feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UseGroup {
    pub what: String,
    /// What to write instead; empty when the feature's instruction says it.
    pub instead: String,
    pub pointers: Vec<String>,
}

/// One use found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureUse {
    pub feature: Feature,
    pub line: u32,
    pub what: String,
    pub instead: String,
}

/// The one-line instruction for a feature that `to` has (`target`) or lacks.
pub fn instruction(feature: Feature, target: bool) -> String {
    let rel = |r: Option<Release>| r.map_or(String::new(), |r| r.codename().to_string());
    let first = feature.first().codename();
    let gone = rel(feature.removed_in());
    match (feature, target) {
        (Feature::SplitClassDirs, true) => format!("from {first} on `inherit` searches classes-recipe/ and classes-global/ before classes/, and a class used in the wrong context is a parse error (4.1 guide, \"Classes now split by usage context\"); a class in classes/ keeps working"),
        (Feature::SplitClassDirs, false) => "BitBake there searches only classes/: move the classes out of classes-recipe/ and classes-global/".into(),
        (Feature::InheritDefer, true) => format!("`inherit_defer` exists from {first} on; `inherit ${{VAR}}` of a value a bbappend changes can use it"),
        (Feature::InheritDefer, false) => "`inherit_defer` does not parse there: use `inherit` with a value that is final at that point".into(),
        (Feature::IncludeAll, true) => format!("`include_all` exists from {first} on"),
        (Feature::IncludeAll, false) => "`include_all` does not parse there: use `include` or `require` of one path".into(),
        (Feature::UnpackDir, true) => format!("from {first} on `file://` sources are unpacked into ${{UNPACKDIR}}, not ${{WORKDIR}}: change `${{WORKDIR}}/<file>` to `${{UNPACKDIR}}/<file>` (5.1 guide, \"WORKDIR references in recipes\")"),
        (Feature::UnpackDir, false) => "OE-core sets no ${UNPACKDIR} there and sources are unpacked into ${WORKDIR}: use ${WORKDIR}".into(),
        (Feature::WorkdirAsSourceDir, true) => "`S = \"${WORKDIR}\"` is accepted there".into(),
        (Feature::WorkdirAsSourceDir, false) => format!("`S = \"${{WORKDIR}}\"` is an error from {gone} on: write `S = \"${{UNPACKDIR}}\"` (5.1 guide)"),
        (Feature::PrInWorkdir, _) => "the work directory ends in ${PV}-${PR} up to mickledore and in ${PV} after (4.3 guide, \"Versioning changes\"); only build paths change".into(),
        (Feature::AddFragments, true) => format!("`addfragments` exists from {first} on"),
        (Feature::AddFragments, false) => "`addfragments` does not parse there: set the fragment's variables directly".into(),
        (Feature::BuiltinFragments, true) => format!("from {first} on `addfragments` takes four arguments (the built-in fragments variable); the three-argument form is an unparsed line"),
        (Feature::BuiltinFragments, false) => "`addfragments` takes three arguments there; the four-argument form is an unparsed line".into(),
        (Feature::RecipeVirtualProviders, true) => format!("from {first} on DEPENDS words listed in BB_RECIPE_VIRTUAL_PROVIDERS are replaced with their PREFERRED_PROVIDER"),
        (Feature::RecipeVirtualProviders, false) => "BB_RECIPE_VIRTUAL_PROVIDERS is not applied there: DEPENDS keeps the virtual names".into(),
        (Feature::AddPyLib, true) => format!("`addpylib` exists from {first} on (conf/layer.conf only)"),
        (Feature::AddPyLib, false) => "`addpylib` does not parse there: add the layer's lib/ directory through BBPATH".into(),
        (Feature::ClassExtendDeferred, _) => "a BBCLASSEXTEND variant's class is inherited after the recipe's deferred inherits from whinlatter on, before them earlier (bitbake-semantics.md, \"Class-extended variants\"): check recipes that combine BBCLASSEXTEND with inherit_defer".into(),
        (Feature::DeferredInheritsClassOverride, _) => "from whinlatter on CLASSOVERRIDE is class-native (and the like) while deferred classes are parsed: check recipes that combine BBCLASSEXTEND with inherit_defer".into(),
        (Feature::AssignmentWhitespaceWarning, true) => format!("from {first} on BitBake warns about an assignment without whitespace around its operator (5.3 guide, \"Space around equal assignment\")"),
        (Feature::AssignmentWhitespaceWarning, false) => "no whitespace warning there".into(),
        (Feature::CveStatus, true) => format!("from {first} on the CVE check reads CVE_STATUS[<id>] (CVE_CHECK_IGNORE becomes CVE_STATUS with a reason)"),
        (Feature::CveStatus, false) => "the CVE check reads CVE_CHECK_IGNORE there, not CVE_STATUS".into(),
        (Feature::CveStatusCpe, true) => format!("from {first} on a CVE_STATUS value may name `cpe:vendor:product`"),
        (Feature::CveStatusCpe, false) => "a `cpe:` CVE_STATUS value is not understood there".into(),
        (Feature::CvePatchContentLines, true) => format!("from {first} on a patch declares CVEs on lines starting with `CVE:`"),
        (Feature::CvePatchContentLines, false) => "a patch's `CVE: CVE-...` tag is matched anywhere in its text there".into(),
        (_, true) => "still present there".into(),
        (kept, false) => format!(
            "removed or renamed in {gone} (the {} migration guide): change each use as shown",
            kept.removed_in().map_or("", |r| r.version())
        ),
    }
}

/// The `${WORKDIR}/<file>` reference of a `file://` entry (no `subdir=`) of `files`, first
/// in `text`.
fn workdir_local_ref<'a>(text: &str, files: &'a BTreeSet<String>) -> Option<&'a str> {
    let mut rest = text;
    while let Some(at) = rest.find("${WORKDIR}/") {
        let after = &rest[at + "${WORKDIR}/".len()..];
        let path: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | ';' | ')' | '`'))
            .collect();
        if let Some(f) = files.iter().find(|f| path == **f) {
            return Some(f.as_str());
        }
        rest = after;
    }
    None
}

/// Uses of the features [`instruction`] knows, other than the `KeptUntil*` ones (those come
/// from `hygiene/release-removed`), in one file, for a migration to `to`. `wanted` limits
/// the scan.
pub fn scan_feature_uses(
    ws: &Workspace,
    loc: &Loc,
    kind: Option<FileKind>,
    parsed: &ParsedFile,
    text: &str,
    wanted: &HashSet<Feature>,
    to: Release,
) -> Vec<FeatureUse> {
    let mut out = Vec::new();
    let mut add = |feature: Feature, line: u32, what: &str| {
        if wanted.contains(&feature) {
            out.push(FeatureUse {
                feature,
                line,
                what: what.to_string(),
                instead: String::new(),
            });
        }
    };
    if kind == Some(FileKind::Class) {
        let dir = loc
            .path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        match resolve::class_dir(ws, &loc.path) {
            ClassDir::Recipe | ClassDir::Global | ClassDir::Unsearched => {
                add(Feature::SplitClassDirs, 1, &format!("classes in {dir}/"));
            }
            // Only news when moving to a release that searches the split directories.
            _ if dir == "classes" && to.has(Feature::SplitClassDirs) => {
                add(Feature::SplitClassDirs, 1, "classes in classes/")
            }
            _ => {}
        }
    }
    let stmt_text = |s: &crate::parse::Statement| text.get(s.bytes.clone()).unwrap_or("");
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut extends = false;
    for s in &parsed.statements {
        if let StatementKind::Assign { var, value, .. } = &s.kind {
            if var.base == "SRC_URI" && var.flag.is_none() {
                for w in words::words(value) {
                    if let Some(f) = w.strip_prefix("file://")
                        && !w.split(';').skip(1).any(|p| p.starts_with("subdir="))
                    {
                        let f = f.split(';').next().unwrap_or(f);
                        if !f.is_empty() && !f.contains("${") && !f.contains('*') {
                            files.insert(f.to_string());
                        }
                    }
                }
            }
            if var.base == "BBCLASSEXTEND" {
                extends = true;
            }
        }
    }
    for s in &parsed.statements {
        let line = s.span.start;
        match &s.kind {
            StatementKind::Inherit { deferred: true, .. } => {
                add(Feature::InheritDefer, line, "inherit_defer");
                if extends {
                    add(
                        Feature::ClassExtendDeferred,
                        line,
                        "inherit_defer in a recipe with BBCLASSEXTEND",
                    );
                    add(
                        Feature::DeferredInheritsClassOverride,
                        line,
                        "inherit_defer in a recipe with BBCLASSEXTEND",
                    );
                }
            }
            StatementKind::Include {
                mode: IncludeMode::IncludeAll,
                ..
            } => add(Feature::IncludeAll, line, "include_all"),
            StatementKind::AddFragments { .. } => {
                add(Feature::AddFragments, line, "addfragments");
                add(Feature::BuiltinFragments, line, "addfragments");
            }
            StatementKind::AddPyLib { .. } => add(Feature::AddPyLib, line, "addpylib"),
            StatementKind::Assign { var, value, .. } => {
                let v = value.trim();
                if var.base == "S"
                    && var.flag.is_none()
                    && (v == "${WORKDIR}" || v == "${WORKDIR}/")
                {
                    add(Feature::WorkdirAsSourceDir, line, "S = \"${WORKDIR}\"");
                }
                if var.base == "BB_RECIPE_VIRTUAL_PROVIDERS" {
                    add(
                        Feature::RecipeVirtualProviders,
                        line,
                        "BB_RECIPE_VIRTUAL_PROVIDERS",
                    );
                }
            }
            _ => {}
        }
        if matches!(
            s.kind,
            StatementKind::Assign { .. }
                | StatementKind::Function { .. }
                | StatementKind::PyDef { .. }
        ) {
            let t = stmt_text(s);
            let is_unpackdir_def =
                matches!(&s.kind, StatementKind::Assign { var, .. } if var.base == "UNPACKDIR");
            if !is_unpackdir_def && let Some(i) = t.lines().position(|l| l.contains("${UNPACKDIR}"))
            {
                let at = line + u32::try_from(i).unwrap_or(0);
                add(Feature::UnpackDir, at, "${UNPACKDIR} references");
            }
            if let Some((i, ())) = t
                .lines()
                .enumerate()
                .find_map(|(i, l)| workdir_local_ref(l, &files).map(|f| (i, f)))
                .map(|(i, _)| (i, ()))
            {
                let at = line + u32::try_from(i).unwrap_or(0);
                add(
                    Feature::UnpackDir,
                    at,
                    "${WORKDIR}/<file> for a file:// source",
                );
            }
        }
    }
    out
}

/// The uses of one feature: `(what, instead)` to pointers.
type UsesByThing = BTreeMap<(String, String), Vec<Loc>>;

/// How lint reaches the layers of `target`.
fn reach_of(target: &Target) -> LintReach {
    match target {
        Target::Workspace => LintReach::Sweep,
        _ => LintReach::Target,
    }
}

/// `LAYERSERIES_COMPAT_<c>` of a layer, its statement and the advice.
fn layer_compat(
    ws: &Workspace,
    index: &Index,
    layer: &str,
    now: &[Release],
    to: Release,
) -> Option<LayerCompat> {
    let l = ws.layer(layer)?;
    let variable = l
        .collections
        .first()
        .map(|c| format!("LAYERSERIES_COMPAT_{c}"));
    let pointer = variable.as_ref().and_then(|var| {
        let f = query::file_by_path(index, layer, Path::new("conf/layer.conf"))
            .ok()
            .flatten()?;
        query::statements_in(index, f.id, &query::StatementFilter::default())
            .ok()?
            .into_iter()
            .find(|s| s.base.as_deref() == Some(var.as_str()))
            .map(|s| s.loc.to_string())
    });
    let claims_target = l.compat.iter().any(|c| c == to.codename());
    let mut names: Vec<String> = l.compat.clone();
    if !claims_target {
        names.push(to.codename().to_string());
    }
    // Known releases oldest first, then the rest as written.
    names.sort_by_key(|n| Release::from_codename(n).map_or(usize::MAX, |r| r as usize));
    names.dedup();
    let var = variable
        .clone()
        .unwrap_or_else(|| "LAYERSERIES_COMPAT_<collection>".into());
    let advice = if claims_target {
        format!("{var} already names {to}; nothing to change")
    } else if l.compat.is_empty() {
        format!("add {var} = \"{to}\" once the layer works with {to}")
    } else {
        format!(
            "write {var} = \"{}\" once the layer works with {to} (drop a release when you stop testing with it)",
            names.join(" ")
        )
    };
    Some(LayerCompat {
        layer: layer.to_string(),
        variable,
        pointer,
        compat: l.compat.clone(),
        releases: now.to_vec(),
        claims_target,
        advice,
    })
}

/// Builds the migration report of `target` to release `to`. `options` is used as given
/// (rules, severity, `all`), except `as_release` / `only_release`, which the report sets.
pub fn report(
    ws: &Workspace,
    index: &Index,
    config: &Config,
    target: &Target,
    options: &Options,
    to: Release,
) -> Result<Migration> {
    let base = Options {
        as_release: None,
        only_release: false,
        ..options.clone()
    };
    let now = super::run(ws, index, config, target, &base)?;
    let then = super::run(
        ws,
        index,
        config,
        target,
        &Options {
            as_release: Some(to),
            only_release: true,
            ..options.clone()
        },
    )?;
    let had: HashSet<(&str, String)> = now
        .findings
        .iter()
        .map(|f| (f.rule, f.loc.to_string()))
        .collect();
    let (existing, new): (Vec<Finding>, Vec<Finding>) = then
        .findings
        .iter()
        .cloned()
        .partition(|f| had.contains(&(f.rule, f.loc.to_string())));

    let mut notes = then.notes.clone();
    let mut plan_notes = Vec::new();
    let plan = super::plan(ws, index, target, &base, &mut plan_notes)?;
    let releases_now = ReleaseCache::new(ws, reach_of(target));

    // Per layer: the features whose value differs between its newest release now and `to`.
    let mut changed_of: BTreeMap<String, (Vec<Release>, HashSet<Feature>)> = BTreeMap::new();
    let mut layers = Vec::new();
    for l in &plan.layers_checked {
        let rel = releases_now.get(l);
        let newest = rel.iter().copied().max().unwrap_or(ws.release.release);
        let changed: HashSet<Feature> = Feature::ALL
            .into_iter()
            .filter(|f| newest.has(*f) != to.has(*f))
            .collect();
        if l != BUILD_LAYER
            && let Some(c) = layer_compat(ws, index, l, &rel, to)
        {
            layers.push(c);
        }
        changed_of.insert(l.clone(), (rel, changed));
    }

    // Uses, per feature, then per thing.
    let mut uses: BTreeMap<String, (Feature, UsesByThing)> = BTreeMap::new();
    let removed_rule = HygieneReleaseRemoved;
    let mut sources = super::SourceCache::default();
    for abs in &plan.files {
        let Some(loc0) = ws.loc_for(abs, LineSpan::line(1)) else {
            continue;
        };
        let Some((_, changed)) = changed_of.get(&loc0.layer) else {
            continue;
        };
        if changed.is_empty() {
            continue;
        }
        let key = (loc0.layer.clone(), loc0.path.clone());
        let entry = match sources.load(ws, &key, Some(abs)) {
            Ok(e) => e,
            Err(e) => {
                notes.push(format!("skipped {}:{}: {e}", key.0, key.1.display()));
                continue;
            }
        };
        let kind = FileKind::from_path(abs);
        let mut found = scan_feature_uses(ws, &loc0, kind, &entry.parsed, &entry.text, changed, to);
        let target_only = [to];
        let cx = LintContext {
            file: Some(&entry.parsed),
            source: Some(&entry.text),
            kind,
            data: None,
            layer: None,
            index,
            ws,
            loc: Loc {
                lines: LineSpan::new(1, entry.lines),
                ..loc0.clone()
            },
            releases: &target_only,
        };
        for u in removed_rule.uses(&cx) {
            // A rename older than the floor holds everywhere: it is a finding, not a change.
            let Some(feature) = u.feature else { continue };
            if changed.contains(&feature) {
                found.push(FeatureUse {
                    feature,
                    line: u.line,
                    what: u.what,
                    instead: u.instead,
                });
            }
        }
        for u in found {
            let name = format!("{:?}", u.feature);
            let slot = uses
                .entry(name)
                .or_insert_with(|| (u.feature, BTreeMap::new()));
            slot.1.entry((u.what, u.instead)).or_default().push(Loc {
                lines: LineSpan::line(u.line),
                ..loc0.clone()
            });
        }
    }
    // Newest release of the layers now, for the `now` column.
    let newest_now = changed_of
        .values()
        .flat_map(|(r, _)| r.iter().copied())
        .max()
        .unwrap_or(ws.release.release);
    let mut features: Vec<FeatureChange> = uses
        .into_iter()
        .map(|(name, (feature, groups))| FeatureChange {
            feature: name,
            now: newest_now.has(feature),
            target: to.has(feature),
            instruction: instruction(feature, to.has(feature)),
            groups: groups
                .into_iter()
                .map(|((what, instead), mut locs)| {
                    locs.sort_by(|a, b| {
                        (&a.layer, &a.path, a.lines.start).cmp(&(&b.layer, &b.path, b.lines.start))
                    });
                    locs.dedup();
                    UseGroup {
                        what,
                        instead,
                        pointers: locs.iter().map(|l| l.to_string()).collect(),
                    }
                })
                .collect(),
        })
        .collect();
    // Release table order.
    features.sort_by_key(|f| {
        Feature::ALL
            .iter()
            .position(|x| format!("{x:?}") == f.feature)
            .unwrap_or(usize::MAX)
    });
    for n in plan_notes {
        if !notes.contains(&n) {
            notes.push(n);
        }
    }
    let uses_n: usize = features
        .iter()
        .flat_map(|f| f.groups.iter())
        .map(|g| g.pointers.len())
        .sum();
    let summary = format!(
        "migration to {to} ({}): {} new with {to}, {} already {}; {} {} the layers use that {to} changed ({} {})",
        to.version(),
        count(new.len(), "finding", "findings"),
        existing.len(),
        if existing.len() == 1 { "holds" } else { "hold" },
        features.len(),
        if features.len() == 1 {
            "feature"
        } else {
            "features"
        },
        uses_n,
        if uses_n == 1 { "use" } else { "uses" },
    );
    Ok(Migration {
        schema: SCHEMA.into(),
        to,
        to_version: to.version().into(),
        summary,
        layers,
        new_findings: new,
        existing_findings: existing,
        features,
        cannot_tell: CANNOT_TELL.iter().map(|s| s.to_string()).collect(),
        notes,
        files: then.files,
    })
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Renders the report: text (sections, findings as `severity rule pointer message` rows)
/// or JSON (the [`Migration`] structure). The text keeps at most [`MAX_POINTERS`] pointers
/// per use group; JSON keeps all.
pub fn render(m: &Migration, format: Format) -> String {
    match format {
        Format::Json => serde_json::to_string_pretty(m).unwrap_or_else(|_| "{}".into()),
        Format::Text => render_text(m),
    }
}

fn finding_row(f: &Finding) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        f.severity.as_str(),
        f.rule,
        f.loc,
        crate::render::escape_cell(&f.message)
    )
}

fn render_text(m: &Migration) -> String {
    let mut out = String::new();
    let mut line = |s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    line(&m.summary);
    line("");
    line("LAYERSERIES_COMPAT:");
    if m.layers.is_empty() {
        line("  (no layer in the target)");
    }
    for l in &m.layers {
        let now = if l.compat.is_empty() {
            "not set".to_string()
        } else {
            format!("\"{}\"", l.compat.join(" "))
        };
        line(&format!(
            "  {}: {} = {now}{}; {}",
            l.layer,
            l.variable.as_deref().unwrap_or("LAYERSERIES_COMPAT"),
            l.pointer
                .as_deref()
                .map_or(String::new(), |p| format!(" ({p})")),
            l.advice
        ));
    }
    line("");
    line(&format!(
        "new with {} ({}):",
        m.to,
        count(m.new_findings.len(), "finding", "findings")
    ));
    if !m.new_findings.is_empty() {
        line("severity\trule\tpointer\tmessage");
    }
    for f in &m.new_findings {
        line(&finding_row(f));
    }
    line("");
    line(&format!(
        "already hold ({}):",
        count(m.existing_findings.len(), "finding", "findings")
    ));
    if !m.existing_findings.is_empty() {
        line("severity\trule\tpointer\tmessage");
    }
    for f in &m.existing_findings {
        line(&finding_row(f));
    }
    line("");
    line(&format!(
        "features {} changed that the layers use ({}):",
        m.to,
        m.features.len()
    ));
    for f in &m.features {
        line(&format!("- {}: {}", f.feature, f.instruction));
        for g in &f.groups {
            let instead = if g.instead.is_empty() {
                String::new()
            } else {
                format!("; {}", g.instead)
            };
            line(&format!(
                "  - {} ({}){instead}",
                g.what,
                count(g.pointers.len(), "use", "uses")
            ));
            for p in g.pointers.iter().take(MAX_POINTERS) {
                line(&format!("    {p}"));
            }
            if g.pointers.len() > MAX_POINTERS {
                line(&format!(
                    "    ... and {} more (--format json lists every one)",
                    g.pointers.len() - MAX_POINTERS
                ));
            }
        }
    }
    line("");
    line("not told by this report:");
    for c in &m.cannot_tell {
        line(&format!("- {c}"));
    }
    for n in &m.notes {
        line(&format!("note: {n}"));
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Exit status of `lint --migrate-to`: 1 if a new finding is at or above `fail_on`.
pub fn exit_code(m: &Migration, fail_on: super::Severity) -> i32 {
    super::exit_code(&m.new_findings, fail_on)
}

/// The writable layers' names (the default target of a migration report), for callers
/// that want to say which layers were covered.
pub fn default_layers(ws: &Workspace) -> Vec<String> {
    let any = ws.layers.iter().any(|l| l.writable);
    ws.layers
        .iter()
        .filter(|l| l.writable || !any)
        .map(|l| l.name.clone())
        .collect()
}
