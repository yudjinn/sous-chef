//! Scaffold generator: `recipe` (T6.2).
//!
//! Writes `recipes-<category>/<pn>/<pn>_<pv>.bb` in the target layer. The tool's `name` and
//! `version` arguments are `PN` and `PV`; the options are listed in [`OPTIONS`].
//!
//! The file follows the OE style-guide order (description block, license, dependencies,
//! `SRC_URI` and its revision or checksum, `S`, `inherit`, packaging) and uses colon
//! override syntax with `:${PN}` on package-scoped variables. Facts that cannot be defaulted
//! (`LIC_FILES_CHKSUM`, `SRCREV`, `SRC_URI[sha256sum]`) are left out, marked with a
//! `# sous-chef:` comment in the file and listed in the plan notes.
//!
//! Releases (see "Releases" in `docs/bitbake-semantics.md`, "Scaffold: sources and `S`"):
//! the target layer's releases are [`Layer::lint_releases`]. A git source gets
//! `destsuffix=${BP}` and no `S`, which works from Kirkstone to Wrynose (the default `S` is
//! `${WORKDIR}/${BP}` before Whinlatter and `${UNPACKDIR}/${BP}` from it). Local `file://`
//! sources get `S = "${UNPACKDIR}"` when every release has [`Feature::UnpackDir`],
//! `S = "${WORKDIR}"` when every release has [`Feature::WorkdirAsSourceDir`], and otherwise
//! `subdir=${BP}` on each entry with the default `S`. A go recipe follows the workspace
//! release when the layer's releases disagree, with a note.
//!
//! When another version of the recipe requires or includes a `.inc`, the new `.bb` requires
//! it and sets only version data.
//!
//! The shared helpers (`opt_words`, `quote`, `check_parses`...) are used by the other
//! generators of this directory too.

use std::path::{Path, PathBuf};

use super::{FileAction, GenContext, Generator, Kind, Plan, PlannedFile};
use crate::error::{Error, Result};
use crate::index::query::{self, RecipeRow};
use crate::parse::{StatementKind, parse};
use crate::release::{Feature, Release};
use crate::types::FileKind;
use crate::workspace::resolve::find_class;
use crate::workspace::{Layer, Workspace};

/// Every option `recipe` accepts.
pub const OPTIONS: &[&str] = &[
    "category",
    "source",
    "branch",
    "srcrev",
    "sha256sum",
    "build",
    "license",
    "lic_files_chksum",
    "depends",
    "rdepends",
    "summary",
    "homepage",
    "go_import",
];

pub struct RecipeGen;

impl Generator for RecipeGen {
    fn kind(&self) -> Kind {
        Kind::Recipe
    }

    fn options(&self) -> &'static [&'static str] {
        OPTIONS
    }

    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        plan(cx)
    }
}

// ---------------------------------------------------------------------------------------
// Shared helpers (also used by append.rs, layer.rs and class.rs)

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid { msg: msg.into() }
}

/// A list option: a string of whitespace-separated words, or an array of strings.
pub(crate) fn opt_words(cx: &GenContext<'_>, key: &str) -> Result<Vec<String>> {
    match cx.request.options.get(key) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::String(s)) => {
            Ok(s.split_whitespace().map(str::to_string).collect())
        }
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
                _ => Err(invalid(format!(
                    "option `{key}` must be a string or an array of non-empty strings"
                ))),
            })
            .collect(),
        Some(_) => Err(invalid(format!(
            "option `{key}` must be a string or an array of strings"
        ))),
    }
}

/// A non-empty, trimmed string option.
pub(crate) fn opt_text<'a>(cx: &GenContext<'a>, key: &str) -> Result<Option<&'a str>> {
    Ok(cx.opt_str(key)?.map(str::trim).filter(|s| !s.is_empty()))
}

/// `"value"`, or `'value'` when the value holds a double quote. A value holding both quote
/// characters or a newline cannot be written on one line and is refused.
pub(crate) fn quote(value: &str) -> Result<String> {
    if value.contains('\n') || value.contains('\r') {
        return Err(invalid(format!("value {value:?} spans several lines")));
    }
    if !value.contains('"') {
        Ok(format!("\"{value}\""))
    } else if !value.contains('\'') {
        Ok(format!("'{value}'"))
    } else {
        Err(invalid(format!(
            "value {value:?} holds both quote characters; BitBake cannot read it on one line"
        )))
    }
}

/// Refuses generated text that does not parse cleanly: every statement must be known and
/// the parser must report nothing. A generator calls this on each file it plans, so bad
/// input becomes an error instead of a broken file.
pub(crate) fn check_parses(what: &str, content: &str) -> Result<()> {
    let parsed = parse(content);
    if let Some(s) = parsed.statements.iter().find_map(|s| match &s.kind {
        StatementKind::Unknown { text } => Some(text.clone()),
        _ => None,
    }) {
        return Err(invalid(format!(
            "the generated {what} would not parse (unrecognised line {s:?}); check the options"
        )));
    }
    if let Some(d) = parsed.diagnostics.first() {
        return Err(invalid(format!(
            "the generated {what} would not parse ({}); check the options",
            d.message
        )));
    }
    Ok(())
}

/// A recipe name BitBake and `org/recipe-path` accept: lower-case letters, digits, `-`,
/// `+`, `.`; no `_` (it separates `PN` from `PV` in the file name).
pub(crate) fn check_pn(pn: &str) -> Result<()> {
    let ok = !pn.is_empty()
        && pn
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && pn
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-+.".contains(c));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "recipe name {pn:?} must be lower-case letters, digits, `-`, `+` or `.` (no `_`, which separates PN from PV)"
        )))
    }
}

/// The releases of `releases` that lack `f`, as codenames.
pub(crate) fn lacking(releases: &[Release], f: Feature) -> Vec<&'static str> {
    releases
        .iter()
        .filter(|r| !r.has(f))
        .map(|r| r.codename())
        .collect()
}

/// `layer:path` of an absolute path inside `layer`.
pub(crate) fn show(layer: &Layer, path: &Path) -> String {
    match path.strip_prefix(&layer.root) {
        Ok(rel) => format!("{}:{}", layer.name, rel.display()),
        Err(_) => path.display().to_string(),
    }
}

/// `SRC_URI = "a \ b"` as one or several lines (continuation aligned under the first word).
pub(crate) fn assign_list(lhs: &str, words: &[String]) -> String {
    match words {
        [] => format!("{lhs} = \"\"\n"),
        [one] => format!("{lhs} = \"{one}\"\n"),
        many => {
            let pad = " ".repeat(lhs.len() + 4);
            let mut out = format!("{lhs} = \"{} \\\n", many[0]);
            for w in &many[1..] {
                out.push_str(&format!("{pad}{w} \\\n"));
            }
            out.push_str(&format!("{pad}\"\n"));
            out
        }
    }
}

// ---------------------------------------------------------------------------------------
// Sources and build systems

#[derive(Debug, Clone, PartialEq, Eq)]
enum Remote {
    Git { url: String },
    Tarball { url: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Source {
    remote: Option<Remote>,
    /// `file://` entries (patches and local files), as written.
    local: Vec<String>,
}

fn is_git(word: &str) -> bool {
    let base = word.split(';').next().unwrap_or(word);
    word.starts_with("git://")
        || word.starts_with("gitsm://")
        || base.ends_with(".git")
        || word.contains(";protocol=") && !word.starts_with("file://")
}

fn parse_source(text: Option<&str>) -> Result<Source> {
    let mut src = Source::default();
    for word in text.unwrap_or("").split_whitespace() {
        if word.starts_with("file://") {
            src.local.push(word.to_string());
        } else if !word.contains("://") {
            src.local.push(format!("file://{word}"));
        } else {
            if src.remote.is_some() {
                return Err(invalid(
                    "option `source` names more than one remote source; give one git or tarball URL plus any local files",
                ));
            }
            src.remote = Some(if is_git(word) {
                Remote::Git {
                    url: word.to_string(),
                }
            } else {
                Remote::Tarball {
                    url: word.to_string(),
                }
            });
        }
    }
    Ok(src)
}

/// URL and its `;key=value` parameters.
fn split_params(url: &str) -> (String, Vec<(String, String)>) {
    let mut parts = url.split(';');
    let base = parts.next().unwrap_or("").to_string();
    let params = parts
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (p.to_string(), String::new()),
        })
        .collect();
    (base, params)
}

fn join_params(base: &str, params: &[(String, String)]) -> String {
    let mut out = base.to_string();
    for (k, v) in params {
        out.push(';');
        out.push_str(k);
        if !v.is_empty() {
            out.push('=');
            out.push_str(v);
        }
    }
    out
}

fn set_param(params: &mut Vec<(String, String)>, key: &str, value: &str) {
    if !params.iter().any(|(k, _)| k == key) {
        params.push((key.to_string(), value.to_string()));
    }
}

/// A build system and the class it inherits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Build {
    Autotools,
    Cmake,
    Meson,
    Cargo,
    Setuptools,
    SetuptoolsBuildMeta,
    Flit,
    Poetry,
    Hatchling,
    Go,
    KernelModule,
    None,
}

const BUILDS: &[(&str, Build)] = &[
    ("autotools", Build::Autotools),
    ("cmake", Build::Cmake),
    ("meson", Build::Meson),
    ("cargo", Build::Cargo),
    ("setuptools", Build::Setuptools),
    ("setuptools_build_meta", Build::SetuptoolsBuildMeta),
    ("flit", Build::Flit),
    ("poetry", Build::Poetry),
    ("hatchling", Build::Hatchling),
    ("go", Build::Go),
    ("kernel-module", Build::KernelModule),
    ("none", Build::None),
];

impl Build {
    fn parse(text: Option<&str>) -> Result<Build> {
        let Some(t) = text else {
            return Ok(Build::None);
        };
        BUILDS
            .iter()
            .find(|(n, _)| *n == t)
            .map(|(_, b)| *b)
            .ok_or_else(|| {
                invalid(format!(
                    "unknown build system {t:?}; one of: {}",
                    BUILDS
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }

    /// The OE-core class (checked on the kirkstone, scarthgap and wrynose branches; see
    /// "Scaffold: build-system classes" in bitbake-semantics.md).
    fn class(self) -> Option<&'static str> {
        Some(match self {
            Build::Autotools => "autotools",
            Build::Cmake => "cmake",
            Build::Meson => "meson",
            Build::Cargo => "cargo",
            Build::Setuptools => "setuptools3",
            Build::SetuptoolsBuildMeta => "python_setuptools_build_meta",
            Build::Flit => "python_flit_core",
            Build::Poetry => "python_poetry_core",
            Build::Hatchling => "python_hatchling",
            Build::Go => "go",
            Build::KernelModule => "module",
            Build::None => return None,
        })
    }
}

// ---------------------------------------------------------------------------------------
// The plan

/// Where the `.bb` goes and what it shares with other versions.
struct Placement {
    /// Directory relative to the layer root (`recipes-core/hello`).
    dir: PathBuf,
    /// `require` argument of a shared `.inc`, if other versions use one.
    inc: Option<Inc>,
}

struct Inc {
    require: String,
    /// Display pointer of the `.inc`.
    shown: String,
    /// What its `SRC_URI` fetches, to know which revision data the `.bb` must carry.
    remote: Option<Remote>,
}

fn plan(cx: &GenContext<'_>) -> Result<Plan> {
    let layer = cx.target_layer()?;
    let pn = cx
        .request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("recipe needs `name` (the PN)"))?;
    check_pn(pn)?;
    let pv = cx
        .request
        .version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("recipe needs `version` (the PV), e.g. 1.0"))?;
    if pv.contains(['_', '/', ' ', '\t', '"', '\'']) {
        return Err(invalid(format!(
            "version {pv:?} must not contain `_`, `/`, quotes or spaces"
        )));
    }
    let releases = layer.lint_releases(cx.ws);
    let others = query::find_recipes(cx.index, pn)?;
    let placement = place(cx, layer, pn, &others)?;
    let rel = placement.dir.join(format!("{pn}_{pv}.bb"));
    let path = layer.root.join(&rel);
    let mut notes = Vec::new();

    let content = match &placement.inc {
        Some(inc) => with_inc(cx, inc, &mut notes)?,
        None => full(cx, layer, &placement.dir, pn, pv, &releases, &mut notes)?,
    };
    check_parses(&rel.display().to_string(), &content)?;
    if others.iter().any(|r| r.file.layer == layer.name) {
        notes.push(format!(
            "other versions of {pn} are in this layer: {}",
            others
                .iter()
                .filter(|r| r.file.layer == layer.name)
                .map(|r| r.pv.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Plan {
        files: vec![PlannedFile {
            path,
            content,
            action: FileAction::Create,
        }],
        notes,
        new_layer: None,
    })
}

/// The `recipes-<category>` directory name for an option value (`core` or `recipes-core`).
fn category_dir(cat: &str) -> Result<String> {
    let c = cat.trim().trim_start_matches("recipes-");
    if c.is_empty()
        || !c
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "-+.".contains(ch))
    {
        return Err(invalid(format!(
            "category {cat:?} must be lower-case letters, digits, `-`, `+` or `.`"
        )));
    }
    Ok(format!("recipes-{c}"))
}

/// The `recipes-*` directories at the top of a layer, sorted.
fn layer_categories(layer: &Layer) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(&layer.root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("recipes-"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn place(cx: &GenContext<'_>, layer: &Layer, pn: &str, others: &[RecipeRow]) -> Result<Placement> {
    let inc = find_inc(cx, layer, others)?;
    // A version in this layer fixes the directory.
    let same_layer = others.iter().find(|r| r.file.layer == layer.name);
    let category = match opt_text(cx, "category")? {
        Some(c) => Some(category_dir(c)?),
        None => None,
    };
    let dir = if let (Some(r), None) = (same_layer, &category) {
        r.file
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    } else {
        let cat = match category {
            Some(c) => c,
            None => default_category(layer, others)?,
        };
        PathBuf::from(cat).join(pn)
    };
    // An `.inc` in this layer is required by its plain name only from its own directory.
    let inc = inc.map(|(row, remote)| {
        let in_dir = row.layer == layer.name && row.path.parent() == Some(dir.as_path());
        let require = if in_dir {
            row.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            row.path.display().to_string()
        };
        Inc {
            require,
            shown: format!("{}:{}", row.layer, row.path.display()),
            remote,
        }
    });
    Ok(Placement { dir, inc })
}

fn default_category(layer: &Layer, others: &[RecipeRow]) -> Result<String> {
    // The category the recipe already has in another layer.
    if let Some(cat) = others.iter().find_map(|r| {
        let first = r.file.path.components().next()?;
        let s = first.as_os_str().to_string_lossy().into_owned();
        s.starts_with("recipes-").then_some(s)
    }) {
        return Ok(cat);
    }
    let cats = layer_categories(layer);
    match cats.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(invalid(format!(
            "layer {} has no recipes-* directory yet; pass `category` (e.g. \"app\" for recipes-app)",
            layer.name
        ))),
        many => Err(Error::Ambiguous {
            what: format!(
                "category in layer {}; pass `category` (an existing one or a new name)",
                layer.name
            ),
            candidates: many.to_vec(),
        }),
    }
}

/// The `.inc` other versions of the recipe require or include: one in the target layer
/// first, then by the recipes' order (layer priority, then version).
fn find_inc(
    cx: &GenContext<'_>,
    layer: &Layer,
    others: &[RecipeRow],
) -> Result<Option<(query::FileRow, Option<Remote>)>> {
    let mut found: Vec<query::FileRow> = Vec::new();
    for r in others {
        for e in query::edges_from(cx.index, r.file.id)? {
            if e.kind != "require" && e.kind != "include" {
                continue;
            }
            if let Some(t) = e.target
                && t.kind == FileKind::Include
                && !found.iter().any(|f| f.id == t.id)
            {
                found.push(t);
            }
        }
    }
    let pick = found
        .iter()
        .find(|f| f.layer == layer.name)
        .or_else(|| found.first())
        .cloned();
    let Some(row) = pick else { return Ok(None) };
    let mut words: Vec<String> = Vec::new();
    for s in query::statement_by_name(cx.index, row.id, "SRC_URI")? {
        if let Some(v) = s.value {
            words.extend(v.split_whitespace().map(str::to_string));
        }
    }
    let remote = parse_source(Some(&words.join(" ")))
        .ok()
        .and_then(|s| s.remote);
    Ok(Some((row, remote)))
}

/// A `.bb` that requires the shared `.inc` and carries only version data.
fn with_inc(cx: &GenContext<'_>, inc: &Inc, notes: &mut Vec<String>) -> Result<String> {
    let mut out = format!("require {}\n\n", inc.require);
    notes.push(format!(
        "other versions share {}; this file requires it and sets only version data",
        inc.shown
    ));
    let ignored: Vec<&str> = [
        "source",
        "branch",
        "build",
        "license",
        "summary",
        "homepage",
        "go_import",
    ]
    .into_iter()
    .filter(|k| cx.request.options.contains_key(*k))
    .collect();
    if !ignored.is_empty() {
        notes.push(format!(
            "ignored because {} sets them: {}",
            inc.shown,
            ignored.join(", ")
        ));
    }
    lic_chksum(cx, &mut out, notes)?;
    let depends = opt_words(cx, "depends")?;
    if !depends.is_empty() {
        out.push_str(&format!("\nDEPENDS += {}\n", quote(&depends.join(" "))?));
    }
    match &inc.remote {
        Some(Remote::Git { .. }) => {
            out.push('\n');
            srcrev(cx, &mut out, notes)?;
        }
        Some(Remote::Tarball { .. }) => {
            out.push('\n');
            sha256(cx, &mut out, notes)?;
        }
        None => {}
    }
    rdepends(cx, &mut out)?;
    Ok(out)
}

fn lic_chksum(cx: &GenContext<'_>, out: &mut String, notes: &mut Vec<String>) -> Result<()> {
    match opt_text(cx, "lic_files_chksum")? {
        Some(v) => out.push_str(&format!("LIC_FILES_CHKSUM = {}\n", quote(v)?)),
        None => {
            out.push_str(
                "# sous-chef: supply LIC_FILES_CHKSUM, e.g. \"file://LICENSE;md5=<md5sum of the file>\"\n",
            );
            notes.push(
                "supply LIC_FILES_CHKSUM (option `lic_files_chksum`): the license file(s) in the source and their md5sum".into(),
            );
        }
    }
    Ok(())
}

fn srcrev(cx: &GenContext<'_>, out: &mut String, notes: &mut Vec<String>) -> Result<()> {
    match opt_text(cx, "srcrev")? {
        Some(v) if v.chars().all(|c| c.is_ascii_alphanumeric()) => {
            out.push_str(&format!("SRCREV = \"{v}\"\n"));
        }
        Some(v) => return Err(invalid(format!("srcrev {v:?} must be a commit id"))),
        None => {
            out.push_str("# sous-chef: supply SRCREV, the commit to build\n");
            notes.push("supply SRCREV (option `srcrev`): the full commit id to build".into());
        }
    }
    Ok(())
}

fn sha256(cx: &GenContext<'_>, out: &mut String, notes: &mut Vec<String>) -> Result<()> {
    match opt_text(cx, "sha256sum")? {
        Some(v) if v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()) => {
            out.push_str(&format!(
                "SRC_URI[sha256sum] = \"{}\"\n",
                v.to_ascii_lowercase()
            ));
        }
        Some(v) => {
            return Err(invalid(format!(
                "sha256sum {v:?} must be 64 hexadecimal digits"
            )));
        }
        None => {
            out.push_str("# sous-chef: supply SRC_URI[sha256sum], the sha256 of the tarball\n");
            notes.push(
                "supply SRC_URI[sha256sum] (option `sha256sum`): the sha256 of the downloaded tarball".into(),
            );
        }
    }
    Ok(())
}

fn rdepends(cx: &GenContext<'_>, out: &mut String) -> Result<()> {
    let rdepends = opt_words(cx, "rdepends")?;
    if !rdepends.is_empty() {
        out.push_str(&format!(
            "\nRDEPENDS:${{PN}} += {}\n",
            quote(&rdepends.join(" "))?
        ));
    }
    Ok(())
}

/// The `S` line (or none) and the `SRC_URI` words for the target releases.
struct SourceLayout {
    src_uri: Vec<String>,
    s: Option<&'static str>,
}

#[allow(clippy::too_many_arguments)]
fn full(
    cx: &GenContext<'_>,
    layer: &Layer,
    dir: &Path,
    pn: &str,
    pv: &str,
    releases: &[Release],
    notes: &mut Vec<String>,
) -> Result<String> {
    let build = Build::parse(opt_text(cx, "build")?)?;
    let source = parse_source(opt_text(cx, "source")?)?;
    let license = opt_text(cx, "license")?.ok_or_else(|| {
        invalid("recipe needs `license` (an SPDX expression such as MIT, or CLOSED)")
    })?;
    let mut out = String::new();

    // Description block and license.
    let summary = match opt_text(cx, "summary")? {
        Some(s) => s.to_string(),
        None => {
            notes.push(
                "SUMMARY defaults to the recipe name; describe the recipe (option `summary`)"
                    .into(),
            );
            pn.to_string()
        }
    };
    out.push_str(&format!("SUMMARY = {}\n", quote(&summary)?));
    if let Some(h) = opt_text(cx, "homepage")? {
        out.push_str(&format!("HOMEPAGE = {}\n", quote(h)?));
    }
    out.push_str(&format!("LICENSE = {}\n", quote(license)?));
    if license != "CLOSED" {
        lic_chksum(cx, &mut out, notes)?;
    }

    // Dependencies.
    let depends = opt_words(cx, "depends")?;
    if !depends.is_empty() {
        out.push_str(&format!("\nDEPENDS = {}\n", quote(&depends.join(" "))?));
    }

    // Sources.
    let go_import = if build == Build::Go {
        Some(go_import(cx, &source)?)
    } else {
        None
    };
    if let Some(gi) = &go_import {
        out.push_str(&format!("\nGO_IMPORT = {}\n", quote(gi)?));
    }
    let layout = layout(cx, &source, build, pn, pv, releases, cx.ws, notes)?;
    if !layout.src_uri.is_empty() {
        out.push('\n');
        out.push_str(&assign_list("SRC_URI", &layout.src_uri));
        match &source.remote {
            Some(Remote::Git { .. }) => srcrev(cx, &mut out, notes)?,
            Some(Remote::Tarball { .. }) => sha256(cx, &mut out, notes)?,
            None => {}
        }
    } else {
        notes.push("no `source` given: the recipe has no SRC_URI".into());
    }
    if !source.local.is_empty() {
        notes.push(format!(
            "put the local files ({}) in {}/ (or in {pn}/ or {pn}-{pv}/ beside it)",
            source
                .local
                .iter()
                .map(|w| w
                    .trim_start_matches("file://")
                    .split(';')
                    .next()
                    .unwrap_or(""))
                .collect::<Vec<_>>()
                .join(", "),
            show(layer, &layer.root.join(dir).join("files")),
        ));
    }
    if let Some(s) = layout.s {
        out.push_str(&format!("\nS = \"{s}\"\n"));
    }

    // Build system.
    if let Some(class) = build.class() {
        out.push_str(&format!("\ninherit {class}\n"));
        if find_class(cx.ws, class, false).is_none() {
            notes.push(format!(
                "class {class} was not found in the indexed layers; it is the OE-core name, so the layer needs OE-core (or the layer that provides it) in BBLAYERS"
            ));
        }
        build_notes(build, notes);
    } else {
        notes.push(
            "build `none`: no class is inherited; add do_compile / do_install as needed".into(),
        );
    }

    // Packaging.
    if build == Build::KernelModule {
        out.push_str(&format!("\nRPROVIDES:${{PN}} += \"kernel-module-{pn}\"\n"));
    }
    rdepends(cx, &mut out)?;
    Ok(out)
}

fn build_notes(build: Build, notes: &mut Vec<String>) {
    match build {
        Build::Cargo => notes.push(
            "cargo: list the crate dependencies as crate:// entries in SRC_URI (from Langdale, `inherit cargo-update-recipe-crates` and run `bitbake -c update_crates <pn>`)".into(),
        ),
        Build::Hatchling => notes.push(
            "python_hatchling.bbclass is not in OE-core kirkstone; use another backend for a layer that claims kirkstone".into(),
        ),
        Build::KernelModule => notes.push(
            "kernel-module: the source needs a Makefile that builds against KERNEL_SRC (see meta-skeleton hello-mod); RPROVIDES assumes the module is <pn>.ko, rename kernel-module-<name> otherwise".into(),
        ),
        _ => {}
    }
}

fn go_import(cx: &GenContext<'_>, source: &Source) -> Result<String> {
    if let Some(gi) = opt_text(cx, "go_import")? {
        return Ok(gi.to_string());
    }
    match &source.remote {
        Some(Remote::Git { url }) => {
            let (base, _) = split_params(url);
            let rest = base.split_once("://").map(|(_, r)| r).unwrap_or(&base);
            Ok(rest
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_string())
        }
        _ => Err(invalid(
            "build `go` needs a git `source` or the `go_import` option (the Go import path)",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn layout(
    cx: &GenContext<'_>,
    source: &Source,
    build: Build,
    pn: &str,
    pv: &str,
    releases: &[Release],
    ws: &Workspace,
    notes: &mut Vec<String>,
) -> Result<SourceLayout> {
    let mut src_uri: Vec<String> = Vec::new();
    let mut s = None;
    let mut local_subdir = false;
    match &source.remote {
        Some(Remote::Git { url }) => {
            let (base, mut params) = split_params(url);
            let base = match base.split_once("://") {
                Some((scheme, rest)) if scheme != "git" && scheme != "gitsm" => {
                    set_param(&mut params, "protocol", scheme);
                    format!("git://{rest}")
                }
                _ => base,
            };
            let branch = match opt_text(cx, "branch")? {
                Some(b) => Some(b.to_string()),
                None => params
                    .iter()
                    .find(|(k, _)| k == "branch")
                    .map(|(_, v)| v.clone()),
            };
            match branch {
                Some(b) => {
                    params.retain(|(k, _)| k != "branch");
                    params.push(("branch".into(), b));
                }
                None if params.iter().any(|(k, _)| k == "nobranch") => {}
                None => {
                    return Err(invalid(
                        "a git source needs a branch: pass `branch` or put ;branch=<name> in the URL (BitBake warns and guesses master without one)",
                    ));
                }
            }
            if build == Build::Go {
                go_destsuffix(&mut params, releases, ws, notes);
            } else if !params.iter().any(|(k, _)| k == "destsuffix") {
                params.push(("destsuffix".into(), "${BP}".into()));
                notes.push(
                    "the git source unpacks into ${BP} (destsuffix=${BP}), so the default S works on every release from kirkstone to wrynose; from whinlatter that is the git fetcher's default and the parameter can be dropped".into(),
                );
            }
            src_uri.push(join_params(&base, &params));
        }
        Some(Remote::Tarball { url }) => {
            let bp = format!("{pn}-{pv}");
            let u = if url.contains(&bp) {
                url.replace(&bp, "${BP}")
            } else {
                url.replace(pv, "${PV}")
            };
            src_uri.push(u);
            notes.push(format!(
                "the default S assumes the tarball unpacks into {bp}/; set S otherwise"
            ));
        }
        None => {
            if !source.local.is_empty() {
                let no_unpackdir = lacking(releases, Feature::UnpackDir);
                let no_workdir = lacking(releases, Feature::WorkdirAsSourceDir);
                if no_unpackdir.is_empty() {
                    s = Some("${UNPACKDIR}");
                } else if no_workdir.is_empty() {
                    s = Some("${WORKDIR}");
                } else {
                    local_subdir = true;
                    notes.push(format!(
                        "the layer claims releases with UNPACKDIR ({}) and without it ({}): the local files unpack into ${{BP}} (subdir=${{BP}}), which is the default S on all of them",
                        lacking(releases, Feature::WorkdirAsSourceDir).join(" "),
                        no_unpackdir.join(" ")
                    ));
                }
            }
        }
    }
    for l in &source.local {
        if local_subdir && !l.contains(";subdir=") {
            src_uri.push(format!("{l};subdir=${{BP}}"));
        } else {
            src_uri.push(l.clone());
        }
    }
    Ok(SourceLayout { src_uri, s })
}

/// Go: `destsuffix=${GO_SRCURI_DESTSUFFIX}` from Styhead (5.1 migration guide); before it,
/// `go_do_unpack` sets the destination itself and the variable does not exist.
fn go_destsuffix(
    params: &mut Vec<(String, String)>,
    releases: &[Release],
    ws: &Workspace,
    notes: &mut Vec<String>,
) {
    if params.iter().any(|(k, _)| k == "destsuffix") {
        return;
    }
    let lacking_ud = lacking(releases, Feature::UnpackDir);
    let having: Vec<&str> = releases
        .iter()
        .filter(|r| r.has(Feature::UnpackDir))
        .map(|r| r.codename())
        .collect();
    let use_new = if lacking_ud.is_empty() {
        true
    } else if having.is_empty() {
        false
    } else {
        let new = ws.release.release.has(Feature::UnpackDir);
        notes.push(format!(
            "go: no SRC_URI form works on every release the layer claims; this follows the workspace release ({}) and will not work on {}",
            ws.release.release.codename(),
            if new { lacking_ud.join(" ") } else { having.join(" ") }
        ));
        new
    };
    if use_new {
        params.push(("destsuffix".into(), "${GO_SRCURI_DESTSUFFIX}".into()));
    }
}
