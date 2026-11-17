//! Scaffold generator: `bbappend` (T6.2).
//!
//! Appends to a recipe from the target layer. The recipe is the `recipe` option or the
//! tool's `name`; the tool's `version` picks a version of it (`%` allowed), else the one
//! BitBake uses, resolved as the `recipe` tool does
//! ([`crate::mcp::tools::recipe::resolve_selected`]: provider selection). Options:
//! [`OPTIONS`].
//!
//! - `version`: `exact` (`<pn>_<pv>.bbappend`), `wildcard` (`<pn>_%.bbappend`) or a prefix
//!   ending in `%` such as `1.%`. Default: the layer's existing bbappend for the recipe if
//!   there is one; else `exact` when patches are added (they are usually version-specific)
//!   and `wildcard` otherwise.
//! - `set`: an object `{ "<left-hand side>": "<value>" }` (the left-hand side may carry
//!   overrides and an operator, e.g. `"EXTRA_OECONF:append"` or `"PACKAGECONFIG ?="`), or an
//!   array of whole assignment lines. Each must parse as one assignment.
//! - `files`: names of patches or files (strings, or `{ "name", "content" }` objects whose
//!   content is written too), added to `SRC_URI` with `:append` and a leading space, under
//!   `<pn>/` next to the bbappend, with `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"`.
//! - `machine`: every change becomes conditional on that machine (`:append:<machine>`,
//!   `VAR:<machine>`), never `VAR:<machine> +=`.
//!
//! The file mirrors the recipe's directory (`recipes-<category>/<pn>/`). When the layer
//! already has a bbappend with the chosen name, or (with no `version` option) any bbappend
//! that applies to the recipe, the plan modifies it instead of creating a second file;
//! `FILESEXTRAPATHS` and `SRC_URI` entries it already has are not repeated.

use std::path::{Component, Path, PathBuf};

use super::recipe::{check_parses, check_pn, invalid, opt_text, opt_words, quote, show};
use super::{FileAction, GenContext, Generator, Kind, Plan, PlannedFile, file_sha256};
use crate::error::{Error, Result};
use crate::index::query::{self, FileRow, RecipeRow};
use crate::lint::helpers::{KnownOverrides, ListKind, is_package_only, list_kind_of};
use crate::mcp::tools::recipe::resolve_selected;
use crate::parse::{StatementKind, parse};
use crate::types::{AssignOp, VarRef};
use crate::workspace::Layer;
use crate::workspace::resolve::append_matches;

/// Every option `bbappend` accepts.
pub const OPTIONS: &[&str] = &["recipe", "version", "set", "files", "machine"];

pub struct AppendGen;

impl Generator for AppendGen {
    fn kind(&self) -> Kind {
        Kind::Bbappend
    }

    fn options(&self) -> &'static [&'static str] {
        OPTIONS
    }

    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        plan(cx)
    }
}

/// Variables that are distro or machine policy (`org/policy-in-recipe`): refused in `set`.
const POLICY: &[&str] = &["DISTRO_FEATURES", "MACHINE_FEATURES", "IMAGE_INSTALL"];

/// A file to add to `SRC_URI`.
struct AddFile {
    name: String,
    content: Option<String>,
}

fn plan(cx: &GenContext<'_>) -> Result<Plan> {
    let layer = cx.target_layer()?;
    let name = match opt_text(cx, "recipe")? {
        Some(r) => r,
        None => cx
            .request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("bbappend needs the recipe: `name` or the `recipe` option"))?,
    };
    check_pn(name)?;
    let mode = opt_text(cx, "version")?;
    let recipe = find_recipe(cx, name, cx.request.version.as_deref(), mode)?;
    let machine = match opt_text(cx, "machine")? {
        Some(m) => Some(check_machine(m)?),
        None => None,
    };
    let files = files(cx)?;
    let mut notes: Vec<String> = Vec::new();
    let lines = set_lines(cx, machine)?;
    if lines.is_empty() && files.is_empty() {
        return Err(invalid(
            "nothing to append: pass `set` (variables) and/or `files` (patches or files)",
        ));
    }

    let dir = recipe_dir(&recipe)?;
    let existing = existing_appends(cx, layer, &recipe)?;
    let file_name = match mode {
        Some(m) => Some(append_name(&recipe, m)?),
        None => None,
    };
    let target: (PathBuf, Option<&FileRow>) = match &file_name {
        Some(n) => {
            let found = existing.iter().find(|f| {
                f.path
                    .file_name()
                    .is_some_and(|x| x.to_string_lossy() == *n)
            });
            match found {
                Some(f) => (f.path.clone(), Some(f)),
                None => {
                    for other in &existing {
                        notes.push(format!(
                            "{}:{} also applies to {}",
                            other.layer,
                            other.path.display(),
                            recipe_label(&recipe)
                        ));
                    }
                    (dir.join(n), None)
                }
            }
        }
        None => match existing.last() {
            Some(f) => (f.path.clone(), Some(f)),
            None => {
                let patches = files.iter().any(|f| is_patch(&f.name));
                let m = if patches { "exact" } else { "wildcard" };
                notes.push(format!(
                    "version: {m} ({}); pass `version` to choose exact, wildcard or a prefix such as 1.%",
                    if patches {
                        "patches usually fit one version"
                    } else {
                        "no version-specific data"
                    }
                ));
                (dir.join(append_name(&recipe, m)?), None)
            }
        },
    };
    let (rel, existing_row) = target;
    let path = layer.root.join(&rel);
    let append_dir = rel.parent().map(Path::to_path_buf).unwrap_or_default();

    if recipe.file.layer == layer.name {
        notes.push(format!(
            "{} is in {} itself; editing the recipe may be simpler than appending to it",
            recipe_label(&recipe),
            layer.name
        ));
    }
    if let Some(m) = machine {
        let known = KnownOverrides::from_index(cx.index)?;
        if !known.machines.contains(m) {
            notes.push(format!(
                "machine {m} has no conf/machine/{m}.conf in the indexed layers"
            ));
        }
    }

    // What the existing file already has.
    let before: Option<String> = match existing_row {
        Some(_) => Some(std::fs::read_to_string(cx.ws.confine(&path)?)?),
        None => None,
    };
    let has_extra = before.as_deref().is_some_and(|t| {
        parse(t).statements.iter().any(|s| {
            matches!(&s.kind, StatementKind::Assign { var, .. } if var.base == "FILESEXTRAPATHS")
        })
    });

    let mut blocks: Vec<String> = Vec::new();
    let mut planned: Vec<PlannedFile> = Vec::new();
    if !files.is_empty() {
        if !has_extra {
            blocks.push("FILESEXTRAPATHS:prepend := \"${THISDIR}/${PN}:\"\n".into());
        }
        let mut entries: Vec<String> = Vec::new();
        for f in &files {
            let entry = format!("file://{}", f.name);
            if before.as_deref().is_some_and(|t| t.contains(&entry)) {
                notes.push(format!(
                    "{entry} is already in the bbappend; not added again"
                ));
                continue;
            }
            entries.push(entry);
            let file_path = layer.root.join(&append_dir).join(&recipe.pn).join(&f.name);
            match &f.content {
                Some(c) => planned.push(PlannedFile {
                    path: file_path,
                    content: c.clone(),
                    action: FileAction::Create,
                }),
                None if file_path.exists() => {}
                None => notes.push(format!("copy {} to {}", f.name, show(layer, &file_path))),
            }
        }
        if !entries.is_empty() {
            let lhs = match machine {
                Some(m) => format!("SRC_URI:append:{m}"),
                None => "SRC_URI:append".to_string(),
            };
            blocks.push(format!("{lhs} = \" {}\"\n", entries.join(" ")));
        }
    }
    if !lines.is_empty() {
        blocks.push(lines.join(""));
    }
    if blocks.is_empty() {
        return Err(invalid(format!(
            "{} already has everything requested; nothing to change",
            show(layer, &path)
        )));
    }

    let (content, action) = match (&before, existing_row) {
        (Some(old), Some(_)) => {
            let mut text = old.clone();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&blocks.join("\n"));
            let sha = file_sha256(&cx.ws.confine(&path)?)?;
            notes.push(format!(
                "{} already appends to {}; the changes are added to it",
                show(layer, &path),
                recipe_label(&recipe)
            ));
            (text, FileAction::Modify { before_sha: sha })
        }
        _ => (blocks.join("\n"), FileAction::Create),
    };
    check_parses(&rel.display().to_string(), &content)?;
    let mut all = vec![PlannedFile {
        path,
        content,
        action,
    }];
    all.extend(planned);
    Ok(Plan {
        files: all,
        notes,
        new_layer: None,
    })
}

fn recipe_label(r: &RecipeRow) -> String {
    format!("{}:{}", r.file.layer, r.file.path.display())
}

/// The recipe to append to: `version` (the tool argument) wins; else a `version` option
/// prefix (`1.%`); else the preferred version.
fn find_recipe(
    cx: &GenContext<'_>,
    name: &str,
    version: Option<&str>,
    mode: Option<&str>,
) -> Result<RecipeRow> {
    let prefix = mode.filter(|m| m.ends_with('%'));
    let pick = version.map(str::trim).filter(|v| !v.is_empty()).or(prefix);
    // No version asked for: the file BitBake uses, resolved as `recipe` does (provider
    // selection: PREFERRED_VERSION, DEFAULT_PREFERENCE, layer priority, version).
    let found = match pick {
        None if !query::find_recipes(cx.index, name)?.is_empty() => {
            Some(resolve_selected(cx.index, cx.ws, name, None)?.0)
        }
        _ => query::preferred_recipe(cx.index, name, pick)?,
    };
    match found {
        Some(r) => {
            if let Some(p) = prefix
                && !query::version_matches(&r.pv, p)
            {
                return Err(invalid(format!(
                    "version {p} does not match {} (PV {})",
                    recipe_label(&r),
                    r.pv
                )));
            }
            Ok(r)
        }
        None => {
            let all = query::find_recipes(cx.index, name)?;
            if !all.is_empty() {
                return Err(Error::NotFound {
                    what: format!(
                        "version {} of recipe {name}; versions: {}",
                        pick.unwrap_or(""),
                        all.iter()
                            .map(|r| r.pv.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            let like = query::recipe_names_like(cx.index, name, 10)?;
            Err(Error::NotFound {
                what: if like.is_empty() {
                    format!("recipe {name} in the workspace")
                } else {
                    format!("recipe {name}; similar: {}", like.join(", "))
                },
            })
        }
    }
}

/// The recipe's directory relative to its layer, when it is `recipes-<category>/...`.
fn recipe_dir(r: &RecipeRow) -> Result<PathBuf> {
    let parent = r.file.path.parent().unwrap_or(Path::new(""));
    let first = parent
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    if first.starts_with("recipes-") {
        Ok(parent.to_path_buf())
    } else {
        Ok(PathBuf::from("recipes-misc").join(&r.pn))
    }
}

/// The target layer's bbappends that apply to the recipe, in application order.
fn existing_appends(cx: &GenContext<'_>, layer: &Layer, r: &RecipeRow) -> Result<Vec<FileRow>> {
    Ok(query::appends_for(cx.index, r.file.id)?
        .into_iter()
        .filter(|f| f.layer == layer.name)
        .collect())
}

/// The bbappend file name for a `version` mode.
fn append_name(r: &RecipeRow, mode: &str) -> Result<String> {
    let name = match mode {
        "exact" => format!("{}_{}.bbappend", r.pn, r.pv),
        "wildcard" | "%" => format!("{}_%.bbappend", r.pn),
        p if p.ends_with('%') && !p.contains(['/', '_', ' ']) => {
            format!("{}_{p}.bbappend", r.pn)
        }
        other => {
            return Err(invalid(format!(
                "option `version` is exact, wildcard or a prefix ending in % (such as 1.%), not {other:?}"
            )));
        }
    };
    let recipe_name = r
        .file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !append_matches(Path::new(&name), Path::new(&recipe_name)) {
        return Err(invalid(format!("{name} would not apply to {recipe_name}")));
    }
    Ok(name)
}

/// A machine name usable as an override: lower-case letters, digits, `-`, `_`, `+`, `.`.
fn check_machine(m: &str) -> Result<&str> {
    let ok = m
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && m.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_+.".contains(c));
    if ok {
        Ok(m)
    } else {
        Err(invalid(format!(
            "machine {m:?} must be lower-case letters, digits, `-`, `_`, `+` or `.` (it becomes an override)"
        )))
    }
}

fn is_patch(name: &str) -> bool {
    name.ends_with(".patch") || name.ends_with(".diff")
}

fn files(cx: &GenContext<'_>) -> Result<Vec<AddFile>> {
    let items: Vec<AddFile> = match cx.request.options.get("files") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::new();
            for v in arr {
                out.push(match v {
                    serde_json::Value::String(s) => AddFile {
                        name: s.trim().to_string(),
                        content: None,
                    },
                    serde_json::Value::Object(o) => AddFile {
                        name: o
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.trim().to_string())
                            .ok_or_else(|| invalid("each `files` object needs a `name`"))?,
                        content: match o.get("content") {
                            None | Some(serde_json::Value::Null) => None,
                            Some(serde_json::Value::String(c)) => Some(c.clone()),
                            Some(_) => return Err(invalid("`files` content must be a string")),
                        },
                    },
                    _ => {
                        return Err(invalid(
                            "option `files` is a list of names or { name, content } objects",
                        ));
                    }
                });
            }
            out
        }
        Some(_) => opt_words(cx, "files")?
            .into_iter()
            .map(|name| AddFile {
                name,
                content: None,
            })
            .collect(),
    };
    for f in &items {
        let p = Path::new(&f.name);
        let plain = !f.name.is_empty()
            && p.components().all(|c| matches!(c, Component::Normal(_)))
            && !f.name.contains(['"', '\'', ';', ' ', '\\', '$']);
        if !plain {
            return Err(invalid(format!(
                "file name {:?} must be a relative path without `..`, spaces, quotes or `;`",
                f.name
            )));
        }
    }
    Ok(items)
}

/// One assignment of `set`, parsed.
struct SetLine {
    var: VarRef,
    op: AssignOp,
    value: String,
    text: String,
}

/// The `set` option as finished lines (each ending in a newline), machine-conditional when
/// `machine` is given.
fn set_lines(cx: &GenContext<'_>, machine: Option<&str>) -> Result<Vec<String>> {
    let raw: Vec<String> = match cx.request.options.get("set") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Object(map)) => {
            let mut out = Vec::new();
            for (k, v) in map {
                let value = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
                    _ => return Err(invalid(format!("`set` value of {k} must be a string"))),
                };
                let k = k.trim();
                let has_op = AssignOp::ALL.iter().any(|op| k.ends_with(op.as_str()));
                out.push(if has_op {
                    format!("{k} {}", quote(&value)?)
                } else {
                    format!("{k} = {}", quote(&value)?)
                });
            }
            out
        }
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| invalid("`set` lines must be strings"))
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(invalid(
                "option `set` is an object { \"VAR[:override]\": \"value\" } or a list of assignment lines",
            ));
        }
    };
    let mut out = Vec::new();
    for line in raw {
        let parsed = parse_one(&line)?;
        let fixed = normalise(parsed, machine)?;
        out.push(format!("{fixed}\n"));
    }
    Ok(out)
}

fn parse_one(line: &str) -> Result<SetLine> {
    let parsed = parse(line);
    let bad = || {
        invalid(format!(
            "`set` entry {line:?} is not one BitBake assignment (VAR = \"value\")"
        ))
    };
    if !parsed.diagnostics.is_empty() || parsed.statements.len() != 1 {
        return Err(bad());
    }
    match parsed.statements.into_iter().next().map(|s| s.kind) {
        Some(StatementKind::Assign {
            var,
            op,
            value,
            export: false,
        }) => Ok(SetLine {
            var,
            op,
            value,
            text: line.to_string(),
        }),
        _ => Err(bad()),
    }
}

const OPERATORS: [&str; 3] = ["append", "prepend", "remove"];

/// Applies the conventions to one assignment and makes it machine-conditional.
fn normalise(l: SetLine, machine: Option<&str>) -> Result<String> {
    let v = &l.var;
    if v.legacy_syntax {
        return Err(invalid(format!(
            "`set` entry {:?} uses the old `_` override syntax; write overrides with `:` (VAR:append)",
            l.text
        )));
    }
    if v.base == "FILESEXTRAPATHS" {
        return Err(invalid(
            "FILESEXTRAPATHS is added by `files`; do not set it through `set`",
        ));
    }
    if POLICY.contains(&v.base.as_str())
        || v.base.starts_with("PREFERRED_PROVIDER")
        || v.base.starts_with("PREFERRED_VERSION")
    {
        return Err(invalid(format!(
            "{} is distro, machine or image policy: set it in the distro or machine conf (or the image recipe), not in a bbappend",
            v.base
        )));
    }
    let conditional = v.overrides.iter().any(|o| !OPERATORS.contains(&o.as_str()));
    let has_operator = v.overrides.iter().any(|o| OPERATORS.contains(&o.as_str()));
    if is_package_only(&v.base) && !conditional {
        return Err(invalid(format!(
            "{} is package-scoped; name the package: {}:${{PN}}",
            v.base, v.base
        )));
    }
    let plus = matches!(
        l.op,
        AssignOp::Append | AssignOp::Prepend | AssignOp::AppendDot | AssignOp::PrependDot
    );
    if plus && !v.overrides.is_empty() {
        return Err(invalid(format!(
            "`set` entry {:?}: `{}` on an overridden variable replaces it when the override is active; use {}:append",
            l.text,
            l.op.as_str(),
            v.base
        )));
    }
    if v.flag.is_some() && machine.is_some() {
        return Err(invalid(format!(
            "`set` entry {:?}: a flag cannot be made machine-conditional",
            l.text
        )));
    }
    let list = list_kind_of(&v.base);
    let mut overrides = v.overrides.clone();
    let mut op = l.op;
    let mut value = l.value.clone();
    // `+=` and friends become :append / :prepend when they need an override.
    if machine.is_some() && plus {
        let (o, lead, trail) = match l.op {
            AssignOp::Append => ("append", true, false),
            AssignOp::AppendDot => ("append", false, false),
            AssignOp::Prepend => ("prepend", false, true),
            _ => ("prepend", false, false),
        };
        overrides.push(o.to_string());
        op = AssignOp::Set;
        if lead && !value.starts_with(' ') {
            value = format!(" {value}");
        }
        if trail && !value.ends_with(' ') {
            value.push(' ');
        }
    }
    if let Some(m) = machine {
        overrides.push(m.to_string());
    }
    // :append with a leading space, :prepend with a trailing one, on space lists.
    if list == ListKind::Space || list == ListKind::Unknown && has_operator {
        if overrides.iter().any(|o| o == "append") && !value.starts_with(' ') {
            value = format!(" {value}");
        }
        if overrides.iter().any(|o| o == "prepend") && !value.ends_with(' ') {
            value.push(' ');
        }
    }
    let mut lhs = v.base.clone();
    for o in &overrides {
        lhs.push(':');
        lhs.push_str(o);
    }
    if let Some(f) = &v.flag {
        lhs.push_str(&format!("[{f}]"));
    }
    Ok(format!("{lhs} {} {}", op.as_str(), quote(&value)?))
}
