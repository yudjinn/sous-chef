//! Scaffold generator: `class` (T6.3).
//!
//! Creates `<name>.bbclass` in the target layer. Options: [`OPTIONS`].
//!
//! - `scope`: `recipe` (default; for `inherit`) or `global` (for `INHERIT +=`).
//! - `tasks`: task names (`configure`, `do_deploy_docs`), strings such as
//!   `"deploy_docs after do_install before do_build"`, or `{ "name", "after", "before" }`
//!   objects. Each becomes `<class>_do_<task>` exported with `EXPORT_FUNCTIONS`; a task that
//!   OE-core already defines (fetch, unpack, patch, configure, compile, install) gets no
//!   `addtask`, any other one gets `addtask` with its ordering (default: after
//!   [`DEFAULT_AFTER`], before [`DEFAULT_BEFORE`]).
//! - `variables`: the interface, `{ "NAME": "default" }` or `["NAME=default", "NAME"]`,
//!   written with `?=` so recipes can override them.
//! - `description`: one line for the header comment.
//!
//! Location ("Releases" in bitbake-semantics.md): `classes-recipe/` or `classes-global/`
//! when every release the layer is checked against ([`Layer::lint_releases`]) has
//! [`Feature::SplitClassDirs`]; otherwise `classes/`, which every release searches (a layer
//! that claims Kirkstone must use it: BitBake 2.0 never looks in the split directories).
//!
//! [`Layer::lint_releases`]: crate::workspace::Layer::lint_releases

use super::recipe::{check_parses, invalid, lacking, opt_text, quote, show};
use super::{FileAction, GenContext, Generator, Kind, Plan, PlannedFile};
use crate::error::Result;
use crate::release::Feature;
use crate::workspace::resolve::class_candidates;

/// Every option `class` accepts.
pub const OPTIONS: &[&str] = &["scope", "tasks", "variables", "description"];

pub struct ClassGen;

impl Generator for ClassGen {
    fn kind(&self) -> Kind {
        Kind::Class
    }

    fn options(&self) -> &'static [&'static str] {
        OPTIONS
    }

    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        plan(cx)
    }
}

/// Tasks OE-core's `base.bbclass` (and `patch.bbclass` for `patch`) already adds; a class
/// overrides them through `EXPORT_FUNCTIONS` without an `addtask`.
const EXISTING_TASKS: &[&str] = &[
    "fetch",
    "unpack",
    "patch",
    "configure",
    "compile",
    "install",
];

/// Ordering of a new task when none is given.
pub const DEFAULT_AFTER: &str = "do_install";
pub const DEFAULT_BEFORE: &str = "do_build";

struct Task {
    /// Without `do_`.
    name: String,
    after: Vec<String>,
    before: Vec<String>,
}

fn task_word(w: &str) -> Result<String> {
    let bare = w.trim().trim_start_matches("do_");
    if bare.is_empty()
        || !bare
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(invalid(format!(
            "task {w:?} must be lower-case letters, digits and `_` (e.g. deploy_docs)"
        )));
    }
    Ok(bare.to_string())
}

fn parse_task_text(text: &str) -> Result<Task> {
    let mut words = text.split_whitespace();
    let name = task_word(words.next().unwrap_or(""))?;
    let (mut after, mut before) = (Vec::new(), Vec::new());
    let mut into: Option<bool> = None; // Some(true): after
    for w in words {
        match w {
            "after" => into = Some(true),
            "before" => into = Some(false),
            t => match into {
                Some(true) => after.push(format!("do_{}", task_word(t)?)),
                Some(false) => before.push(format!("do_{}", task_word(t)?)),
                None => {
                    return Err(invalid(format!(
                        "task {text:?}: write `name after do_x before do_y`"
                    )));
                }
            },
        }
    }
    Ok(Task {
        name,
        after,
        before,
    })
}

fn tasks(cx: &GenContext<'_>) -> Result<Vec<Task>> {
    let items: Vec<serde_json::Value> = match cx.request.options.get("tasks") {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(serde_json::Value::String(s)) => s
            .split(',')
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect(),
        Some(_) => {
            return Err(invalid(
                "option `tasks` is a list of task names, `name after x before y` strings or { name, after, before } objects",
            ));
        }
    };
    let words = |v: Option<&serde_json::Value>| -> Result<Vec<String>> {
        let list: Vec<String> = match v {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::String(s)) => {
                s.split_whitespace().map(str::to_string).collect()
            }
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| invalid("task orderings are task names"))
                })
                .collect::<Result<_>>()?,
            Some(_) => return Err(invalid("task orderings are task names")),
        };
        list.iter()
            .map(|t| Ok(format!("do_{}", task_word(t)?)))
            .collect()
    };
    let mut out: Vec<Task> = Vec::new();
    for v in items {
        let t = match &v {
            serde_json::Value::String(s) => parse_task_text(s)?,
            serde_json::Value::Object(o) => Task {
                name: task_word(o.get("name").and_then(|n| n.as_str()).unwrap_or(""))?,
                after: words(o.get("after"))?,
                before: words(o.get("before"))?,
            },
            _ => {
                return Err(invalid(
                    "each task is a name or a { name, after, before } object",
                ));
            }
        };
        if t.name == "build" {
            return Err(invalid(
                "do_build cannot be defined by a class; it only orders other tasks",
            ));
        }
        if out.iter().any(|o| o.name == t.name) {
            return Err(invalid(format!("task {} is listed twice", t.name)));
        }
        out.push(t);
    }
    Ok(out)
}

fn variables(cx: &GenContext<'_>) -> Result<Vec<(String, String)>> {
    let pairs: Vec<(String, String)> = match cx.request.options.get("variables") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Object(m)) => m
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(s) => Ok((k.trim().to_string(), s.clone())),
                serde_json::Value::Number(n) => Ok((k.trim().to_string(), n.to_string())),
                serde_json::Value::Null => Ok((k.trim().to_string(), String::new())),
                _ => Err(invalid(format!("default of {k} must be a string"))),
            })
            .collect::<Result<_>>()?,
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .map(|v| {
                let s = v
                    .as_str()
                    .ok_or_else(|| invalid("`variables` entries are NAME or NAME=default"))?;
                Ok(match s.split_once('=') {
                    Some((k, d)) => (k.trim().to_string(), d.trim().trim_matches('"').to_string()),
                    None => (s.trim().to_string(), String::new()),
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(invalid(
                "option `variables` is { \"NAME\": \"default\" } or a list of NAME=default",
            ));
        }
    };
    for (k, _) in &pairs {
        let ok = k.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && k.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(invalid(format!(
                "interface variable {k:?} must be upper-case letters, digits and `_`"
            )));
        }
    }
    Ok(pairs)
}

fn plan(cx: &GenContext<'_>) -> Result<Plan> {
    let layer = cx.target_layer()?;
    let name = cx
        .request
        .name
        .as_deref()
        .map(|s| s.trim().trim_end_matches(".bbclass"))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("class needs `name`"))?;
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        || name.starts_with('-')
    {
        return Err(invalid(format!(
            "class name {name:?} must be lower-case letters, digits, `_` or `-`"
        )));
    }
    let global = match opt_text(cx, "scope")? {
        None | Some("recipe") => false,
        Some("global") => true,
        Some(other) => {
            return Err(invalid(format!(
                "option `scope` is recipe or global, not {other:?}"
            )));
        }
    };
    let tasks = tasks(cx)?;
    if !tasks.is_empty() && name.contains('-') {
        return Err(invalid(format!(
            "class name {name:?} contains `-`: BitBake refuses EXPORT_FUNCTIONS of shell tasks from such a class; use `_`"
        )));
    }
    let vars = variables(cx)?;
    let description = opt_text(cx, "description")?;

    let releases = layer.lint_releases(cx.ws);
    let no_split = lacking(&releases, Feature::SplitClassDirs);
    let dir = if !no_split.is_empty() {
        "classes"
    } else if global {
        "classes-global"
    } else {
        "classes-recipe"
    };
    let mut notes = Vec::new();
    if !no_split.is_empty() {
        notes.push(format!(
            "classes/ because {} claims {}, which do{} not search classes-recipe/ or classes-global/ (BitBake 2.0); classes/ is searched by every release",
            layer.name,
            no_split.join(" "),
            if no_split.len() == 1 { "es" } else { "" }
        ));
    }
    let path = layer.root.join(dir).join(format!("{name}.bbclass"));

    // Existing classes of that name.
    let existing = class_candidates(cx.ws, name, global);
    if let Some(first) = existing.first() {
        let shown = cx
            .ws
            .loc_for(first, crate::types::LineSpan::line(1))
            .map(|l| format!("{}:{}", l.layer, l.path.display()))
            .unwrap_or_else(|| first.display().to_string());
        notes.push(format!(
            "a class {name} already exists at {shown}; the class search order (BBPATH, split directories first) decides which one `inherit {name}` loads"
        ));
    }

    let content = class_text(name, global, description, &vars, &tasks)?;
    check_parses(&format!("{dir}/{name}.bbclass"), &content)?;
    notes.push(if global {
        format!(
            "enable it with INHERIT += \"{name}\" in a distro or local configuration ({})",
            show(layer, &path)
        )
    } else {
        format!(
            "use it with `inherit {name}` in a recipe ({})",
            show(layer, &path)
        )
    });
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

fn class_text(
    name: &str,
    global: bool,
    description: Option<&str>,
    vars: &[(String, String)],
    tasks: &[Task],
) -> Result<String> {
    let mut out = format!("# {name}.bbclass\n#\n");
    match description {
        Some(d) => out.push_str(&format!("# {d}\n#\n")),
        None => out.push_str(&format!(
            "# Logic shared by the {} that inherit this class.\n#\n",
            if global { "configurations" } else { "recipes" }
        )),
    }
    if global {
        out.push_str(&format!(
            "# Use: INHERIT += \"{name}\" in a configuration file.\n"
        ));
    } else {
        out.push_str(&format!("# Use: inherit {name}\n"));
    }
    if !vars.is_empty() {
        out.push_str("#\n# Interface (weak defaults, ?=; set them in the recipe to override):\n");
        let w = vars.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, d) in vars {
            out.push_str(&format!("#   {k:<w$}  default {}\n", quote(d)?));
        }
    }
    let new_tasks: Vec<&Task> = tasks
        .iter()
        .filter(|t| !EXISTING_TASKS.contains(&t.name.as_str()))
        .collect();
    if !tasks.is_empty() {
        out.push_str("#\n# Tasks (exported with EXPORT_FUNCTIONS; a recipe's own do_<task> replaces them):\n");
        for t in tasks {
            if EXISTING_TASKS.contains(&t.name.as_str()) {
                out.push_str(&format!(
                    "#   do_{}: replaces the default implementation\n",
                    t.name
                ));
            } else {
                let (after, before) = ordering(t);
                out.push_str(&format!(
                    "#   do_{}: new task, after {} before {}\n",
                    t.name,
                    after.join(" "),
                    before.join(" ")
                ));
            }
        }
    }
    if !vars.is_empty() {
        out.push('\n');
        for (k, d) in vars {
            out.push_str(&format!("{k} ?= {}\n", quote(d)?));
        }
    }
    for t in tasks {
        out.push_str(&format!("\n{name}_do_{}() {{\n", t.name));
        if t.name == "install" {
            out.push_str("\tinstall -d ${D}\n");
        } else {
            out.push_str("\t:\n");
        }
        out.push_str("}\n");
        if new_tasks.iter().any(|n| n.name == t.name) {
            let (after, before) = ordering(t);
            out.push_str(&format!(
                "addtask {} after {} before {}\n",
                t.name,
                after.join(" "),
                before.join(" ")
            ));
        }
    }
    if !tasks.is_empty() {
        out.push_str(&format!(
            "\nEXPORT_FUNCTIONS {}\n",
            tasks
                .iter()
                .map(|t| format!("do_{}", t.name))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    Ok(out)
}

fn ordering(t: &Task) -> (Vec<String>, Vec<String>) {
    let after = if t.after.is_empty() {
        vec![DEFAULT_AFTER.to_string()]
    } else {
        t.after.clone()
    };
    let before = if t.before.is_empty() {
        vec![DEFAULT_BEFORE.to_string()]
    } else {
        t.before.clone()
    };
    (after, before)
}
