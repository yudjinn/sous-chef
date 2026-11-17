//! Scaffold generator: `inc_split` (T6.4).
//!
//! Moves the statements that every version of a recipe shares into `<pn>.inc` beside them,
//! and makes each `.bb` `require` it. The recipe is the `recipe` option or the tool's
//! `name`; its versions are the `.bb` files of that `PN` in the target layer (all of them, or
//! the `versions` option). Options: [`OPTIONS`].
//!
//! **What moves.** The statements common to all versions: the same statement (an assignment
//! with the same name, operator and value; a python function with the same lines; anything
//! else with the same normalised text, [`helpers::normalise`]) in the same relative order (a
//! longest common subsequence, newest version first). Never moved, as in
//! `org/duplicate-versions`: version data ([`helpers::is_version_data`]: `PV`, `PR`, `PE`,
//! `SRCREV*`, `LIC_FILES_CHKSUM`, `SRC_URI` checksum flags) and `require` / `include` lines.
//! A statement that differs between versions (a version-specific patch in `SRC_URI`) is
//! not common and stays. A comment block directly above a moved statement (no blank line
//! between) moves with it; other comments stay where they are. The `.inc` takes the newest
//! version's text; when another version's comment differs, that comment stays in its `.bb`.
//!
//! **The new `.bb`.** The original file with the moved statements taken out and
//! `require <pn>.inc` where the first of them was; everything else keeps its text and
//! order.
//!
//! **Safety check.** The split changes the parse order in two ways, and the plan is refused
//! (no files, the notes say why) when either could change a value:
//!
//! - a statement that stays in a `.bb` after the first moved statement but before a later
//!   one now comes after that later one. Each such pair must commute: they set no common
//!   variable or flag, neither expands immediately (`:=`, `${...}` in `inherit` / `include`)
//!   something the other sets, and the classes and files an `inherit` or `require` pulls in
//!   (from the evaluation of the version, [`crate::eval::evaluate`]) count as part of it.
//!   Immediate expansion follows references through the stored values and `OVERRIDES`; one
//!   that reaches python is treated as reading everything;
//! - inside the `.inc`, BitBake sets `FILE` to the `.inc`, so `PN` is `defaultpkgname` and
//!   `PV` `1.0` there (bitbake-semantics.md, "Replay"). A moved statement (or a class it
//!   inherits) that expands `FILE`, `PN`, `PV`, `BPN`, `BP`, `P` or `PF` immediately, or
//!   reaches python doing so, is refused. `THISDIR` and `FILE_DIRNAME` are the same (the
//!   `.inc` is in the recipe's directory).
//!
//! Lazy references (`=`, `+=`, ...) are expanded at the end of parsing, after `FILE` is
//! the recipe again, so the order of statements that do not interact does not matter.
//!
//! Also refused: versions in different directories, an existing `<pn>.inc`, a version that
//! does not parse cleanly, and versions with nothing in common.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::recipe::{check_parses, check_pn, invalid, opt_text, opt_words, show};
use super::{FileAction, GenContext, Generator, Kind, Plan, PlannedFile, sha256_hex};
use crate::error::{Error, Result};
use crate::eval::{self, DataStore, Target};
use crate::index::query::{self, RecipeRow};
use crate::lint::helpers;
use crate::parse::{Lang, Statement, StatementKind, parse};
use crate::types::AssignOp;
use crate::workspace::{Layer, Workspace};

/// Every option `inc_split` accepts.
pub const OPTIONS: &[&str] = &["recipe", "versions"];

pub struct IncSplitGen;

impl Generator for IncSplitGen {
    fn kind(&self) -> Kind {
        Kind::IncSplit
    }

    fn options(&self) -> &'static [&'static str] {
        OPTIONS
    }

    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        plan(cx)
    }
}

/// Names BitBake computes from `FILE`: different while the `.inc` is parsed.
const FILE_DERIVED: &[&str] = &["FILE", "PN", "PV", "BPN", "BP", "P", "PF"];

/// Names computed from the directory of `FILE`: the same in the `.inc`. Expanding them
/// immediately is safe and is not followed further.
const SAME_DIR: &[&str] = &["THISDIR", "FILE_DIRNAME"];

/// Most conflicts listed in a refusal.
const MAX_LISTED: usize = 8;

/// Most names followed when closing an immediate expansion over its references.
const MAX_CLOSURE: usize = 400;

static VAR_REF: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\$\{([a-zA-Z0-9\-_+./~:]+)\}").ok());

/// One version of the recipe.
struct Version {
    recipe: RecipeRow,
    /// Absolute, confined.
    abs: PathBuf,
    sha: String,
    /// The file's lines, each with its line ending.
    lines: Vec<String>,
    stmts: Vec<Stmt>,
}

impl Version {
    fn file_name(&self) -> String {
        self.recipe
            .file
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// `layer:path:line` of a statement.
    fn at(&self, s: &Stmt) -> String {
        format!(
            "{}:{}:{}",
            self.recipe.file.layer,
            self.recipe.file.path.display(),
            s.st.span.start
        )
    }

    /// The text of 1-based lines `a..=b`.
    fn text(&self, a: u32, b: u32) -> String {
        let a = a.max(1) as usize;
        let b = (b as usize).min(self.lines.len());
        if a > b {
            return String::new();
        }
        self.lines[a - 1..b].concat()
    }
}

/// One top-level statement of a version.
struct Stmt {
    st: Statement,
    /// First line of the comment block attached to it (directly above, no blank line), or
    /// the statement's own first line.
    first: u32,
    /// What two statements must share to be "the same".
    key: String,
    /// Not version data and not `require` / `include`.
    movable: bool,
    /// Short description for notes.
    label: String,
    effects: Effects,
}

/// What a statement does to the datastore, as far as the order of statements matters.
#[derive(Debug, Clone, Default)]
struct Effects {
    /// `NAME` (value, any override form), `NAME[flag]`, `NAME[*]` (every flag), and pseudo
    /// names (`__tasks`, `inherit:<class>`, `include:<path>`, ...).
    writes: BTreeSet<String>,
    /// Names expanded immediately, closed over their references.
    reads: BTreeSet<String>,
    /// An immediate expansion reaches python or too many names: reads anything.
    reads_any: bool,
    /// Effects unknown (a file pulled in could not be read, or no evaluation).
    opaque: bool,
}

impl Effects {
    fn merge(&mut self, o: Effects) {
        self.writes.extend(o.writes);
        self.reads.extend(o.reads);
        self.reads_any |= o.reads_any;
        self.opaque |= o.opaque;
    }
}

fn plan(cx: &GenContext<'_>) -> Result<Plan> {
    let layer = cx.target_layer()?;
    let pn = match opt_text(cx, "recipe")? {
        Some(r) => r,
        None => cx
            .request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("inc_split needs the recipe: `name` or the `recipe` option"))?,
    };
    check_pn(pn)?;
    let recipes = pick_versions(cx, layer, pn)?;

    // Same directory, and no `<pn>.inc` there yet.
    let dir = recipes[0]
        .file
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if let Some(other) = recipes
        .iter()
        .find(|r| r.file.path.parent().unwrap_or(Path::new("")) != dir)
    {
        return Ok(refused(vec![format!(
            "{} and {} are in different directories; a shared .inc must sit beside every version (move them together first)",
            label(&recipes[0]),
            label(other)
        )]));
    }
    let inc_name = format!("{pn}.inc");
    let inc_rel = dir.join(&inc_name);
    let inc_abs = layer.root.join(&inc_rel);
    if std::fs::symlink_metadata(&inc_abs).is_ok() {
        return Ok(refused(vec![format!(
            "{} already exists; move the shared statements into it by hand",
            show(layer, &inc_abs)
        )]));
    }

    let mut versions: Vec<Version> = Vec::with_capacity(recipes.len());
    for r in recipes {
        match load(cx, layer, r)? {
            Ok(v) => versions.push(v),
            Err(why) => return Ok(refused(vec![why])),
        }
    }

    let common = common_statements(&versions);
    if common.first().is_none_or(Vec::is_empty) {
        return Ok(refused(vec![format!(
            "the versions ({}) share no statement other than version data and require / include lines",
            names(&versions)
        )]));
    }

    let problems = safety_problems(&versions, &common);
    if !problems.is_empty() {
        let mut notes = vec![format!(
            "the split would change what BitBake parses for {}:",
            names(&versions)
        )];
        let n = problems.len();
        notes.extend(problems.into_iter().take(MAX_LISTED));
        if n > MAX_LISTED {
            notes.push(format!("+{} more", n - MAX_LISTED));
        }
        notes.push(
            "make these statements the same in every version, or reorder them by hand, then re-run"
                .into(),
        );
        return Ok(refused(notes));
    }

    // The `.inc`, from the newest version.
    let reference = &versions[0];
    let inc = inc_text(reference, &common[0]);
    check_parses(&inc_rel.display().to_string(), &inc)?;

    let mut files = vec![PlannedFile {
        path: inc_abs.clone(),
        content: inc,
        action: FileAction::Create,
    }];
    let mut notes = vec![format!(
        "{} statement{} shared by {} move{} into {}; each version requires it",
        common[0].len(),
        if common[0].len() == 1 { "" } else { "s" },
        names(&versions),
        if common[0].len() == 1 { "s" } else { "" },
        show(layer, &inc_abs)
    )];
    for (vi, v) in versions.iter().enumerate() {
        let (content, kept_comments) = bb_text(
            v,
            &common[vi],
            &reference_comments(reference, &common[0]),
            &inc_name,
        );
        check_parses(&v.recipe.file.path.display().to_string(), &content)?;
        let kept = kept_summary(v, &common[vi]);
        notes.push(format!("{} keeps {kept}", v.file_name()));
        for line in kept_comments {
            notes.push(format!(
                "{}:{}:{line}: this comment differs from the one in {} and stays in the .bb",
                v.recipe.file.layer,
                v.recipe.file.path.display(),
                reference.file_name()
            ));
        }
        files.push(PlannedFile {
            path: v.abs.clone(),
            content,
            action: FileAction::Modify {
                before_sha: v.sha.clone(),
            },
        });
    }
    let others: Vec<RecipeRow> = query::find_recipes(cx.index, pn)?
        .into_iter()
        .filter(|r| !versions.iter().any(|v| v.recipe.file.id == r.file.id))
        .collect();
    for r in others {
        notes.push(format!(
            "{} is another version of {pn} and is not changed",
            label(&r)
        ));
    }
    Ok(Plan {
        files,
        notes,
        new_layer: None,
    })
}

/// A plan with no files: the notes say why nothing is planned.
fn refused(why: Vec<String>) -> Plan {
    let mut notes = vec!["nothing planned: the split is not safe or not possible".to_string()];
    notes.extend(why);
    Plan {
        files: Vec::new(),
        notes,
        new_layer: None,
    }
}

fn label(r: &RecipeRow) -> String {
    format!("{}:{}", r.file.layer, r.file.path.display())
}

fn names(versions: &[Version]) -> String {
    helpers::and_list(&versions.iter().map(Version::file_name).collect::<Vec<_>>())
}

/// The versions of `pn` in `layer`, newest first: all of them, or those the `versions`
/// option names. At least two.
fn pick_versions(cx: &GenContext<'_>, layer: &Layer, pn: &str) -> Result<Vec<RecipeRow>> {
    let all = query::find_recipes(cx.index, pn)?;
    if all.is_empty() {
        let like = query::recipe_names_like(cx.index, pn, 10)?;
        return Err(Error::NotFound {
            what: if like.is_empty() {
                format!("recipe {pn} in the workspace")
            } else {
                format!("recipe {pn}; similar: {}", like.join(", "))
            },
        });
    }
    let mut mine: Vec<RecipeRow> = all
        .iter()
        .filter(|r| r.file.layer == layer.name)
        .cloned()
        .collect();
    if mine.is_empty() {
        let mut where_: Vec<String> = all.iter().map(|r| r.file.layer.clone()).collect();
        where_.dedup();
        return Err(invalid(format!(
            "{pn} has no version in {}; it is in {} (inc_split changes the recipe's own files, so they must be in a writable layer)",
            layer.name,
            where_.join(", ")
        )));
    }
    let wanted = opt_words(cx, "versions")?;
    if !wanted.is_empty() {
        for w in &wanted {
            if !mine.iter().any(|r| &r.pv == w) {
                return Err(invalid(format!(
                    "{pn} has no version {w} in {}; versions there: {}",
                    layer.name,
                    mine.iter()
                        .map(|r| r.pv.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        mine.retain(|r| wanted.contains(&r.pv));
    }
    mine.sort_by(|a, b| query::vercmp(&b.pv, &a.pv).then_with(|| a.file.path.cmp(&b.file.path)));
    if mine.len() < 2 {
        return Err(invalid(format!(
            "inc_split needs two or more versions of {pn} in {}; found {}",
            layer.name,
            mine.iter()
                .map(|r| r.pv.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(mine)
}

/// Reads, parses and evaluates one version. The inner `Err` is a reason to refuse.
fn load(
    cx: &GenContext<'_>,
    layer: &Layer,
    recipe: RecipeRow,
) -> Result<std::result::Result<Version, String>> {
    let abs = cx.ws.confine(&layer.root.join(&recipe.file.path))?;
    let bytes = std::fs::read(&abs)?;
    let sha = sha256_hex(&bytes);
    let text = crate::parse::lexer::decode(&bytes).into_owned();
    let parsed = parse(&text);
    let bad = parsed
        .statements
        .iter()
        .find(|s| matches!(s.kind, StatementKind::Unknown { .. }))
        .map(|s| s.span.start)
        .or_else(|| parsed.diagnostics.first().map(|d| d.span.start));
    if let Some(line) = bad {
        return Ok(Err(format!(
            "{}:{line} does not parse cleanly; fix it first (lint names the problem)",
            label(&recipe)
        )));
    }
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    if let Some(last) = lines.last_mut()
        && !last.ends_with('\n')
    {
        last.push('\n');
    }
    let target = Target {
        recipe: Some(label(&recipe)),
        ..Target::default()
    };
    let ds = eval::evaluate(cx.index, cx.ws, &target).ok();
    let mut stmts: Vec<Stmt> = Vec::with_capacity(parsed.statements.len());
    let mut prev_end = 0u32;
    for st in parsed.statements {
        let first = attached_comment(&lines, prev_end, st.span.start);
        prev_end = st.span.end;
        let source = text.get(st.bytes.clone()).unwrap_or("");
        let movable = match &st.kind {
            StatementKind::Include { .. } => false,
            StatementKind::Assign { var, .. } => {
                !helpers::is_version_data(&var.base, var.flag.as_deref())
            }
            StatementKind::Unknown { .. } => false,
            _ => true,
        };
        let mut effects = effects_of(&st.kind, ds.as_ref());
        if matches!(
            st.kind,
            StatementKind::Inherit { .. } | StatementKind::Include { .. }
        ) {
            effects.merge(pulled_in(cx.ws, &recipe, &st, ds.as_ref()));
        }
        stmts.push(Stmt {
            key: key_of(&st.kind, source),
            label: label_of(&st.kind, source),
            movable,
            first,
            effects,
            st,
        });
    }
    Ok(Ok(Version {
        recipe,
        abs,
        sha,
        lines,
        stmts,
    }))
}

/// The first line of the comment block directly above line `start` (no blank line between,
/// not inside the previous statement), or `start`.
fn attached_comment(lines: &[String], prev_end: u32, start: u32) -> u32 {
    let mut first = start;
    while first > prev_end + 1 {
        let Some(l) = lines.get(first as usize - 2) else {
            break;
        };
        if l.trim_start().starts_with('#') {
            first -= 1;
        } else {
            break;
        }
    }
    first
}

/// What two statements must share to be "the same" statement.
fn key_of(kind: &StatementKind, source: &str) -> String {
    match kind {
        StatementKind::Assign {
            var,
            op,
            value,
            export,
        } => format!(
            "assign\u{0}{export}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{value}",
            var.legacy_syntax,
            helpers::colon_name(var),
            var.flag.as_deref().unwrap_or(""),
            op.as_str()
        ),
        // Indentation is meaning in python: only trailing whitespace is ignored.
        StatementKind::Function {
            lang: Lang::Python, ..
        }
        | StatementKind::PyDef { .. } => format!(
            "python\u{0}{}",
            source
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        ),
        _ => format!("other\u{0}{}", helpers::normalise(source)),
    }
}

fn label_of(kind: &StatementKind, source: &str) -> String {
    match kind {
        StatementKind::Assign { var, op, .. } => {
            let mut lhs = helpers::colon_name(var);
            if let Some(f) = &var.flag {
                lhs.push_str(&format!("[{f}]"));
            }
            format!("{lhs} {}", op.as_str())
        }
        StatementKind::Function { name: Some(v), .. } => format!("{}()", helpers::colon_name(v)),
        StatementKind::Function { name: None, .. } => "anonymous python".into(),
        StatementKind::PyDef { name, .. } => format!("def {name}"),
        _ => helpers::clip(source.lines().next().unwrap_or("").trim()),
    }
}

// ---------------------------------------------------------------------------------------
// Effects

fn refs(text: &str) -> BTreeSet<String> {
    match VAR_REF.as_ref() {
        Some(re) => re
            .captures_iter(text)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect(),
        None => BTreeSet::new(),
    }
}

/// Records an immediate expansion of `text`.
fn immediate(text: &str, ds: Option<&DataStore>, eff: &mut Effects) {
    if text.contains("${@") {
        eff.reads_any = true;
        return;
    }
    let names = refs(text);
    if names.is_empty() {
        return;
    }
    if names.iter().all(|n| SAME_DIR.contains(&n.as_str())) {
        eff.reads.extend(names);
        return;
    }
    let mut seeds = names;
    seeds.insert("OVERRIDES".to_string());
    match closure(ds, seeds) {
        Some(all) => eff.reads.extend(all),
        None => eff.reads_any = true,
    }
}

/// `seeds` and every name their stored values (all assignments, override variants and
/// appends included) reference, transitively. `None` when python is reached, when there is
/// no datastore, or past [`MAX_CLOSURE`] names. `FILE`-derived and directory names are kept
/// but not followed (their stored values are python that the replay computes).
fn closure(ds: Option<&DataStore>, seeds: BTreeSet<String>) -> Option<BTreeSet<String>> {
    let ds = ds?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = seeds.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > MAX_CLOSURE {
            return None;
        }
        if FILE_DERIVED.contains(&name.as_str()) || SAME_DIR.contains(&name.as_str()) {
            continue;
        }
        let mut texts: Vec<String> = ds
            .history(&name)
            .into_iter()
            .map(|(op, _)| op.value)
            .collect();
        texts.extend(ds.raw(&name));
        for t in texts {
            if t.contains("${@") {
                return None;
            }
            for r in refs(&t) {
                if !seen.contains(&r) {
                    queue.push_back(r);
                }
            }
        }
    }
    Some(seen)
}

/// `do_` prefixed, as `bb.build.ensure_task_prefix` does.
fn task(name: &str) -> String {
    if name.starts_with("do_") {
        name.to_string()
    } else {
        format!("do_{name}")
    }
}

/// The direct effects of one statement (not the files it pulls in).
fn effects_of(kind: &StatementKind, ds: Option<&DataStore>) -> Effects {
    let mut e = Effects::default();
    match kind {
        StatementKind::Assign {
            var,
            op,
            value,
            export,
        } => {
            if var.base.contains("${") {
                e.opaque = true;
            }
            e.writes.insert(match &var.flag {
                Some(f) => format!("{}[{f}]", var.base),
                None => var.base.clone(),
            });
            if *export {
                e.writes.insert(format!("{}[export]", var.base));
            }
            if *op == AssignOp::Immediate {
                immediate(value, ds, &mut e);
            }
        }
        StatementKind::Export { name } => {
            e.writes.insert(format!("{name}[export]"));
        }
        StatementKind::Unset { name, flag } => match flag {
            Some(f) => {
                e.writes.insert(format!("{name}[{f}]"));
            }
            None => {
                e.writes.insert(name.clone());
                e.writes.insert(format!("{name}[*]"));
            }
        },
        StatementKind::Function { name: Some(v), .. } => {
            if v.base.contains("${") {
                e.opaque = true;
            }
            e.writes.insert(v.base.clone());
            e.writes.insert(format!("{}[*]", v.base));
        }
        StatementKind::Function { name: None, .. } => {
            e.writes.insert("__anonymous".into());
        }
        StatementKind::PyDef { name, .. } => {
            e.writes.insert(name.clone());
            e.writes.insert(format!("{name}[*]"));
        }
        StatementKind::Inherit { classes, deferred } => {
            immediate(classes, ds, &mut e);
            for w in classes.split_whitespace() {
                e.writes.insert(format!("inherit:{w}"));
            }
            if *deferred {
                e.writes.insert("__inherit_defer".into());
            }
        }
        StatementKind::Include { path, .. } => {
            immediate(path, ds, &mut e);
            e.writes.insert(format!("include:{}", path.trim()));
        }
        StatementKind::AddTask {
            name,
            before,
            after,
        } => {
            e.writes.insert("__tasks".into());
            for n in name
                .split_whitespace()
                .chain(before.iter().map(String::as_str))
                .chain(after.iter().map(String::as_str))
            {
                e.writes.insert(format!("{}[*]", task(n)));
            }
        }
        StatementKind::DelTask { name } => {
            e.writes.insert("__tasks".into());
            for n in name.split_whitespace() {
                e.writes.insert(format!("{}[*]", task(n)));
            }
        }
        StatementKind::AddHandler { .. } => {
            e.writes.insert("__handlers".into());
        }
        StatementKind::ExportFunctions { names } => {
            for n in names {
                e.writes.insert(n.clone());
                e.writes.insert(format!("{n}[*]"));
            }
        }
        StatementKind::Unknown { .. } => e.opaque = true,
        StatementKind::AddFragments { .. } => e.opaque = true,
        StatementKind::AddPyLib { .. } => e.opaque = true,
    }
    e
}

/// The effects of the files an `inherit` / `require` / `include` statement of `recipe`
/// pulled in when the version was evaluated (classes already inherited are not parsed
/// again, so they add nothing). Opaque without an evaluation or when a file cannot be read.
fn pulled_in(
    ws: &Workspace,
    recipe: &RecipeRow,
    st: &Statement,
    ds: Option<&DataStore>,
) -> Effects {
    let mut e = Effects::default();
    let Some(data) = ds else {
        e.opaque = true;
        return e;
    };
    for visit in data.files() {
        let via_this = visit.via.iter().any(|l| {
            l.layer == recipe.file.layer
                && l.path == recipe.file.path
                && l.lines.start == st.span.start
        });
        if !via_this {
            continue;
        }
        let text = ws
            .abs_path(&visit.loc)
            .and_then(|p| ws.confine(&p).ok())
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| crate::parse::lexer::decode(&b).into_owned());
        let Some(text) = text else {
            e.opaque = true;
            continue;
        };
        for s in parse(&text).statements {
            e.merge(effects_of(&s.kind, ds));
        }
    }
    e
}

/// Why two statements do not commute, if they may not.
fn conflict(a: &Effects, b: &Effects) -> Option<String> {
    if a.opaque || b.opaque {
        return Some("what one of them does is not known statically".into());
    }
    for x in &a.writes {
        if let Some(y) = b.writes.iter().find(|y| keys_conflict(x, y)) {
            return Some(format!(
                "both set {}",
                show_key(if x.ends_with("[*]") { y } else { x })
            ));
        }
    }
    for (r, w) in [(a, b), (b, a)] {
        if r.reads_any && !w.writes.is_empty() {
            return Some(
                "one expands a value immediately that may depend on anything (python, or too many references)"
                    .into(),
            );
        }
        if let Some(n) = r.reads.iter().find(|n| w.writes.contains(*n)) {
            return Some(format!(
                "{n} is expanded immediately by one and set by the other"
            ));
        }
    }
    None
}

fn split_key(k: &str) -> (&str, Option<&str>) {
    match k.strip_suffix(']').and_then(|s| s.split_once('[')) {
        Some((n, f)) => (n, Some(f)),
        None => (k, None),
    }
}

fn keys_conflict(x: &str, y: &str) -> bool {
    if x == y {
        return true;
    }
    match (split_key(x), split_key(y)) {
        ((nx, Some(fx)), (ny, Some(fy))) => nx == ny && (fx == "*" || fy == "*"),
        _ => false,
    }
}

fn show_key(k: &str) -> String {
    if k == "__tasks" {
        "the task list (addtask / deltask)".into()
    } else if k == "__anonymous" {
        "the anonymous python queue".into()
    } else if k == "__handlers" {
        "the event handler list".into()
    } else if k == "__inherit_defer" {
        "the deferred inherit list".into()
    } else if let Some(c) = k.strip_prefix("inherit:") {
        format!("`inherit {c}`")
    } else if let Some(p) = k.strip_prefix("include:") {
        format!("the include of {p}")
    } else if let Some(n) = k.strip_suffix("[*]") {
        format!("flags of {n}")
    } else {
        k.to_string()
    }
}

// ---------------------------------------------------------------------------------------
// Common statements and the safety check

/// Longest common subsequence of two key lists, as index pairs.
fn lcs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut t = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// For each version, the indices (into its `stmts`) of the statements common to all
/// versions, in order; the k-th entries of every list are the same statement.
fn common_statements(versions: &[Version]) -> Vec<Vec<usize>> {
    let movable = |v: &Version| -> Vec<usize> {
        v.stmts
            .iter()
            .enumerate()
            .filter(|(_, s)| s.movable)
            .map(|(i, _)| i)
            .collect()
    };
    let mut common: Vec<Vec<usize>> = Vec::with_capacity(versions.len());
    common.push(movable(&versions[0]));
    for v in &versions[1..] {
        let cand = movable(v);
        let a: Vec<&str> = common[0]
            .iter()
            .map(|&i| versions[0].stmts[i].key.as_str())
            .collect();
        let b: Vec<&str> = cand.iter().map(|&j| v.stmts[j].key.as_str()).collect();
        let pairs = lcs(&a, &b);
        for list in common.iter_mut() {
            *list = pairs.iter().map(|&(i, _)| list[i]).collect();
        }
        common.push(pairs.iter().map(|&(_, j)| cand[j]).collect());
    }
    common
}

/// Everything that makes the split unsafe, one note each.
fn safety_problems(versions: &[Version], common: &[Vec<usize>]) -> Vec<String> {
    let mut out = Vec::new();
    for (vi, v) in versions.iter().enumerate() {
        let moved = &common[vi];
        // Inside the `.inc`, FILE is the `.inc`.
        for &m in moved {
            let s = &v.stmts[m];
            let e = &s.effects;
            if e.opaque {
                out.push(format!(
                    "{} `{}`: what it pulls in could not be read, so moving it cannot be checked",
                    v.at(s),
                    s.label
                ));
            } else if e.reads_any {
                out.push(format!(
                    "{} `{}` expands a value immediately that may depend on FILE (python, or too many references); inside the .inc FILE is the .inc",
                    v.at(s),
                    s.label
                ));
            } else if let Some(n) = e.reads.iter().find(|n| FILE_DERIVED.contains(&n.as_str())) {
                out.push(format!(
                    "{} `{}` expands {n} immediately; inside the .inc FILE is the .inc, so PN is defaultpkgname and PV 1.0 there",
                    v.at(s),
                    s.label
                ));
            }
        }
        // Statements that stay but now come after later moved ones.
        let Some(&first) = moved.first() else {
            continue;
        };
        for (ri, r) in v.stmts.iter().enumerate() {
            if ri <= first || moved.contains(&ri) {
                continue;
            }
            for &c in moved.iter().filter(|&&c| c > ri) {
                let cs = &v.stmts[c];
                if let Some(why) = conflict(&r.effects, &cs.effects) {
                    out.push(format!(
                        "{} `{}` would move into the .inc, ahead of {} `{}`, which stays in the .bb: {why}",
                        v.at(cs),
                        cs.label,
                        v.at(r),
                        r.label
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Output

/// The `.inc`: the common statements of `v` with their attached comments, blank lines kept
/// where the file had them.
fn inc_text(v: &Version, moved: &[usize]) -> String {
    let mut out = String::new();
    let mut prev_end: Option<u32> = None;
    for &m in moved {
        let s = &v.stmts[m];
        if let Some(pe) = prev_end {
            let gap = v.text(pe + 1, s.first.saturating_sub(1));
            if gap.lines().any(|l| l.trim().is_empty()) {
                out.push('\n');
            }
        }
        out.push_str(&v.text(s.first, s.st.span.end));
        prev_end = Some(s.st.span.end);
    }
    out
}

/// The normalised attached comment of each common statement of the reference version.
fn reference_comments(v: &Version, moved: &[usize]) -> Vec<String> {
    moved
        .iter()
        .map(|&m| {
            let s = &v.stmts[m];
            comment_key(&v.text(s.first, s.st.span.start.saturating_sub(1)))
        })
        .collect()
}

/// A comment block for comparison: lines trimmed, whitespace runs collapsed.
fn comment_key(text: &str) -> String {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A line of the new `.bb`: text, and whether it belongs to a statement (kept verbatim).
struct Line {
    text: String,
    in_statement: bool,
}

/// The new `.bb` for `v`, and the lines of comments kept because they differ from the
/// reference's.
fn bb_text(
    v: &Version,
    moved: &[usize],
    ref_comments: &[String],
    inc_name: &str,
) -> (String, Vec<u32>) {
    let mut drop = vec![false; v.lines.len() + 1];
    let mut kept_comments = Vec::new();
    for (k, &m) in moved.iter().enumerate() {
        let s = &v.stmts[m];
        let comment = comment_key(&v.text(s.first, s.st.span.start.saturating_sub(1)));
        let same = comment.is_empty() || ref_comments.get(k).is_some_and(|c| *c == comment);
        let from = if same {
            s.first
        } else {
            kept_comments.push(s.first);
            s.st.span.start
        };
        for l in from..=s.st.span.end {
            if let Some(d) = drop.get_mut(l as usize) {
                *d = true;
            }
        }
    }
    let mut in_stmt = vec![false; v.lines.len() + 1];
    for s in &v.stmts {
        for l in s.st.span.start..=s.st.span.end {
            if let Some(d) = in_stmt.get_mut(l as usize) {
                *d = true;
            }
        }
    }
    let insert_at = moved.first().map(|&m| {
        let s = &v.stmts[m];
        if drop.get(s.first as usize).copied().unwrap_or(false) {
            s.first
        } else {
            s.st.span.start
        }
    });
    let mut out: Vec<Line> = Vec::new();
    for (i, text) in v.lines.iter().enumerate() {
        let n = i as u32 + 1;
        if Some(n) == insert_at {
            out.push(Line {
                text: format!("require {inc_name}\n"),
                in_statement: true,
            });
            out.push(Line {
                text: "\n".into(),
                in_statement: false,
            });
        }
        if drop[n as usize] {
            continue;
        }
        out.push(Line {
            text: text.clone(),
            in_statement: in_stmt[n as usize],
        });
    }
    // Blank lines outside statements: at most one in a row, none at either end.
    let mut text = String::new();
    let mut blank_run = true;
    for l in &out {
        let blank = !l.in_statement && l.text.trim().is_empty();
        if blank {
            if blank_run {
                continue;
            }
            blank_run = true;
            text.push('\n');
        } else {
            blank_run = false;
            text.push_str(&l.text);
        }
    }
    while text.ends_with("\n\n") {
        text.pop();
    }
    (text, kept_comments)
}

/// `LIC_FILES_CHKSUM and SRC_URI[sha256sum]`: what stays in a `.bb`, for the notes.
fn kept_summary(v: &Version, moved: &[usize]) -> String {
    let kept: Vec<&Stmt> = v
        .stmts
        .iter()
        .enumerate()
        .filter(|(i, _)| !moved.contains(i))
        .map(|(_, s)| s)
        .collect();
    if kept.is_empty() {
        return "only the require".into();
    }
    let labels: Vec<String> = kept
        .iter()
        .map(|s| match &s.st.kind {
            StatementKind::Assign { .. } => s
                .label
                .rsplit_once(' ')
                .map(|(lhs, _)| lhs.to_string())
                .unwrap_or_else(|| s.label.clone()),
            _ => s.label.clone(),
        })
        .collect();
    helpers::and_list(&labels)
}
