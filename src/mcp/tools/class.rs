//! Tool `class`: a bbclass: its interface, users and outline. See docs/mcp-tools.md.
//!
//! The class is looked up as a recipe `inherit` would find it (`classes-recipe/` along all
//! of `BBPATH`, then `classes/`); a class found only in `classes-global/` is looked up as
//! an `INHERIT` would. Every indexed `<name>.bbclass` is listed, the one BitBake uses first.
//! `summary` and `outline` describe the winning file only.
//!
//! - `summary`: the file(s), the classes it inherits, tasks it adds (`addtask`, `deltask`,
//!   `addhandler`), `EXPORT_FUNCTIONS`, and the variables it sets with `?=` / `??=` (its
//!   interface).
//! - `users`: files that inherit it directly (`depth` 1) and transitively (through an
//!   include, another class or a bbappend), plus configuration files that name it in
//!   `INHERIT`.
//! - `outline`: its functions and tasks.

use schemars::JsonSchema;
use serde::Deserialize;

use super::recipe::{file_label, outline_kind};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, ClassFileRow, StatementFilter};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// Files (and which wins), inherits, tasks, `EXPORT_FUNCTIONS`, `?=` interface.
    #[default]
    Summary,
    /// Recipes and classes that inherit it, direct and transitive.
    Users,
    /// Functions and tasks.
    Outline,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Class name without `.bbclass`.
    pub name: String,
    /// `summary` (default), `users` or `outline`.
    #[serde(default)]
    pub show: Show,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let name = args.name.trim().trim_end_matches(".bbclass");
    if name.is_empty() || name.contains('/') {
        return Err(Error::Invalid {
            msg: "give a class name without directory or .bbclass, e.g. autotools".into(),
        });
    }
    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let found = find_class(&index, ctx, name)?;
    let mut t = match args.show {
        Show::Summary => summary(&index, &found)?,
        Show::Users => users(&index, name)?,
        Show::Outline => outline(&index, &found)?,
    };
    if name == "base" {
        t.note("note: base.bbclass is inherited by the global configuration, before every recipe");
    }
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

struct Found {
    files: Vec<ClassFileRow>,
    /// Ranked for `INHERIT` (the class exists only off the recipe search path).
    global: bool,
}

impl Found {
    fn winner(&self) -> Option<&ClassFileRow> {
        self.files
            .iter()
            .find(|c| c.rank.is_some())
            .or(self.files.first())
    }
}

fn find_class(index: &Index, ctx: &Ctx, name: &str) -> Result<Found> {
    let files = query::class_files(index, &ctx.workspace, name, false)?;
    if files.is_empty() {
        let candidates = class_names_like(index, name)?;
        if candidates.is_empty() {
            return Err(Error::NotFound {
                what: format!("class {name}"),
            });
        }
        return Err(Error::Ambiguous {
            what: format!("class name {name:?} (no class has exactly this name)"),
            candidates,
        });
    }
    if files.iter().all(|c| c.rank.is_none()) {
        let global = query::class_files(index, &ctx.workspace, name, true)?;
        if global.iter().any(|c| c.rank.is_some()) {
            return Ok(Found {
                files: global,
                global: true,
            });
        }
    }
    Ok(Found {
        files,
        global: false,
    })
}

fn summary(index: &Index, found: &Found) -> Result<Table> {
    let mut t = Table::new(&["field", "value", "loc"]);
    let winner = found.winner().map(|c| c.file.id);
    let search = if found.global { "INHERIT" } else { "inherit" };
    for c in &found.files {
        let state = match c.rank {
            _ if Some(c.file.id) == winner && c.rank.is_some() => format!("wins ({search})"),
            Some(_) => "shadowed".into(),
            None => format!("not on the {search} search path"),
        };
        t.row(&[
            "file",
            &format!("{state}, {} priority {}", c.file.layer, c.file.priority),
            &file_label(&c.file),
        ]);
    }
    let Some(id) = winner else {
        return Ok(t);
    };
    for e in query::edges_from(index, id)? {
        if e.kind != "inherit" {
            continue;
        }
        let mut v = e.target_text.clone();
        if e.target.is_none() {
            v.push_str(" (unresolved)");
        }
        t.row(&["inherits", &v, &e.via().to_string()]);
    }
    for s in query::statements_in(index, id, &StatementFilter::default())? {
        let field = match s.kind.as_str() {
            "addtask" => "task",
            "deltask" => "deltask",
            "addhandler" => "handler",
            "export_functions" => "EXPORT_FUNCTIONS",
            "assign"
                if matches!(s.op.as_deref(), Some("?=") | Some("??="))
                    && s.flag.is_none()
                    && s.overrides.is_empty() =>
            {
                "interface"
            }
            _ => continue,
        };
        let value = match field {
            "interface" => format!(
                "{} {} \"{}\"",
                s.name.as_deref().unwrap_or("?"),
                s.op.as_deref().unwrap_or(""),
                render::truncate_value(s.value.as_deref().unwrap_or(""), 80, None)
            ),
            "EXPORT_FUNCTIONS" => s.name.clone().unwrap_or_default(),
            _ => s.snippet.clone(),
        };
        t.row(&[field, &value, &s.loc.to_string()]);
    }
    if found.global {
        t.note("note: global class: ranked as INHERIT finds it (classes-global/, then classes/)");
    }
    Ok(t)
}

fn users(index: &Index, name: &str) -> Result<Table> {
    let mut t = Table::new(&["depth", "kind", "file", "via"]);
    for s in query::statements_by_base(index, "INHERIT", &StatementFilter::default())? {
        let named = s
            .value
            .as_deref()
            .is_some_and(|v| v.split_whitespace().any(|w| w == name));
        if s.kind == "assign" && named {
            t.row(&[
                "1",
                "INHERIT",
                &format!("{}:{}", s.loc.layer, s.loc.path.display()),
                &s.loc.to_string(),
            ]);
        }
    }
    for u in query::inheritors(index, name, true)? {
        t.row(&[
            &u.depth.to_string(),
            u.file.kind.as_str(),
            &file_label(&u.file),
            &u.via.to_string(),
        ]);
    }
    if t.is_empty() {
        t.note(
            "no users found (static: inherits whose argument needs evaluation are not resolved)",
        );
    }
    Ok(t)
}

fn outline(index: &Index, found: &Found) -> Result<Table> {
    let mut t = Table::new(&["name", "kind", "loc"]);
    let Some(w) = found.winner() else {
        return Ok(t);
    };
    for s in query::statements_in(index, w.file.id, &StatementFilter::default())? {
        if let Some(kind) = outline_kind(&s) {
            t.row(&[s.name.as_deref().unwrap_or("-"), &kind, &s.loc.to_string()]);
        }
    }
    Ok(t)
}

/// Private query (candidate for `index::query`): class names (file stems) containing `q`,
/// at most 20.
fn class_names_like(index: &Index, q: &str) -> Result<Vec<String>> {
    let mut st = index
        .conn()
        .prepare("SELECT path FROM files WHERE kind = 'bbclass' ORDER BY path")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    let q = q.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    for p in rows {
        let p = p?;
        let stem = p
            .rsplit('/')
            .next()
            .unwrap_or(&p)
            .trim_end_matches(".bbclass")
            .to_string();
        if stem.to_ascii_lowercase().contains(&q) && !out.contains(&stem) {
            out.push(stem);
        }
    }
    out.sort();
    out.truncate(20);
    Ok(out)
}
