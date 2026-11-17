//! Hook handler for Grep and Read results (T7.2): adds index context next to the result,
//! as `additionalContext`. The tool output is never changed.
//!
//! - **Grep**, `content` mode: every hit line in an indexed Yocto file that falls inside a
//!   multi-line item (a function, a continued assignment) gets the item and its span, e.g.
//!   `meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]`; hits in the
//!   same item are grouped. Every mode: each recipe a hit belongs to (the `.bb` itself, or
//!   the recipe a `.bbappend` applies to) gets one line with its bbappend and include
//!   counts and a pointer to `recipe` / `var`.
//! - **Read**: when the range starts or ends inside a multi-line item, one note per cut
//!   naming the item and where it begins or ends. When a whole `.bb` was read, one line
//!   listing the bbappends and the `require` / `include` / `include_all` files that also
//!   contribute, as pointers.
//!
//! Files that are not Yocto files, are outside the workspace, are not indexed, or changed
//! since they were indexed are skipped ([`super::Env::fresh_file`]).
//!
//! The `Grep` `content` text format is not documented by Claude Code; see
//! `plugin/README.md` ("Not verified") for what is assumed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::{Ctx, Env, Event, Handler, POST_TOOL_USE, Response, is_yocto_path};
use crate::error::Result;
use crate::index::query::{self, FileRow, ItemRow};
use crate::types::{FileKind, LineSpan};

/// Hit lines looked at per Grep call; the rest are ignored.
const MAX_HITS: usize = 200;
/// Item lines in one response.
const MAX_ITEM_LINES: usize = 40;
/// Recipe lines in one response.
const MAX_RECIPES: usize = 8;

/// `PostToolUse` on `Grep` and `Read`.
pub struct GrepRead;

impl Handler for GrepRead {
    fn name(&self) -> &'static str {
        "grep_read"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Grep", "Read"]
    }

    fn handle(&self, cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>> {
        match ev.tool_name.as_str() {
            "Grep" => grep(cx, ev),
            "Read" => read(cx, ev),
            _ => Ok(None),
        }
    }
}

// ---- shared ----

/// The label of an item inside `[...]`: its name as written, with the keyword for
/// statements that are not assignments or functions. `None` for unparsed lines.
pub fn item_label(item: &ItemRow) -> Option<String> {
    let name = item.name.as_deref().unwrap_or_default();
    let label = match item.kind.as_str() {
        "unknown" => return None,
        "function" if name == "__anonymous" => "anonymous python".to_string(),
        "function" | "assign" | "export" | "unset" => name.to_string(),
        "pydef" => format!("def {name}"),
        "include" => format!("{} {name}", item.op.as_deref().unwrap_or("include")),
        "inherit" => format!("{} {name}", item.op.as_deref().unwrap_or("inherit")),
        other => format!("{other} {name}"),
    };
    let label = label.trim().to_string();
    (!label.is_empty()).then_some(label)
}

/// `[label A-B]`.
fn bracket(item: &ItemRow) -> Option<String> {
    Some(format!("[{} {}]", item_label(item)?, item.loc.lines))
}

/// The multi-line item covering `line` of `file`, if any.
fn multiline_item(env: &Env<'_>, file: i64, line: u32) -> Result<Option<ItemRow>> {
    Ok(query::enclosing_item(env.index, file, line)?
        .filter(|i| i.loc.lines.end > i.loc.lines.start && item_label(i).is_some()))
}

/// `layer:path` of a file.
fn file_ptr(f: &FileRow) -> String {
    format!("{}:{}", f.layer, f.path.display())
}

fn file_name(f: &FileRow) -> String {
    f.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| f.path.display().to_string())
}

/// The files a recipe pulls in with `require` / `include` / `include_all`, at any depth,
/// in parse order, without repeats.
fn included_files(env: &Env<'_>, recipe: i64) -> Result<Vec<(String, FileRow)>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for node in query::tree(env.index, recipe)? {
        if !matches!(node.kind.as_str(), "require" | "include" | "include_all") {
            continue;
        }
        if let Some(f) = node.file
            && f.id != recipe
            && seen.insert(f.id)
        {
            out.push((node.kind, f));
        }
    }
    Ok(out)
}

/// The recipe files a hit in `file` belongs to: the file itself for a `.bb`, the recipes a
/// `.bbappend` applies to. Other kinds have none.
fn recipes_of(env: &Env<'_>, file: &FileRow) -> Result<Vec<FileRow>> {
    Ok(match file.kind {
        FileKind::Recipe => vec![file.clone()],
        FileKind::Append => query::edges_from(env.index, file.id)?
            .into_iter()
            .filter(|e| e.kind == "bbappend")
            .filter_map(|e| e.target)
            .collect(),
        _ => Vec::new(),
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One line about a recipe: its bbappend and include counts and how to see the merged
/// result.
fn recipe_line(env: &Env<'_>, recipe: &FileRow) -> Result<String> {
    let appends = query::appends_for(env.index, recipe.id)?.len();
    let includes = included_files(env, recipe.id)?.len();
    let pn = recipe.pn.clone().unwrap_or_else(|| file_name(recipe));
    Ok(format!(
        "recipe {} ({}): {}, {}; `recipe {pn}` / `var <NAME> recipe={pn}` give the merged view.",
        file_name(recipe),
        recipe.layer,
        plural(appends, "bbappend", "bbappends"),
        plural(includes, "included file", "included files"),
    ))
}

// ---- Grep ----

/// Compiles a constant pattern once. `None` (never in practice) means "matches nothing".
fn cached(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// `<path>:<line>:` at the start of a ripgrep content line, for a Yocto file.
fn hit_captures(l: &str) -> Option<regex::Captures<'_>> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    cached(&RE, r"^(.+?\.(?:bb|bbappend|inc|bbclass|conf)):(\d+):")?.captures(l)
}

/// `<path>:<count>` (count mode).
fn count_captures(l: &str) -> Option<regex::Captures<'_>> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    cached(&RE, r"^(.+?\.(?:bb|bbappend|inc|bbclass|conf)):\d+$")?.captures(l)
}

/// `<line>:` (content of a single-file search, no file name).
fn line_captures(l: &str) -> Option<regex::Captures<'_>> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    cached(&RE, r"^(\d+):")?.captures(l)
}

/// One Grep hit: the file and, in content mode, the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub line: Option<u32>,
}

/// The Yocto hits in a Grep result (at most [`MAX_HITS`]). Relative paths are taken
/// against the search directory, or `cwd`.
pub fn grep_hits(ev: &Event) -> Vec<Hit> {
    let resp = &ev.tool_response;
    let mode = resp
        .get("mode")
        .and_then(Value::as_str)
        .or_else(|| ev.input_str("output_mode"))
        .unwrap_or("files_with_matches");
    let search = ev
        .input_str("path")
        .and_then(|p| ev.abs_path(p))
        .or_else(|| ev.cwd.clone());
    let single_file = search.clone().filter(|p| p.is_file());
    let base = match (&search, &single_file) {
        (Some(s), None) => Some(s.clone()),
        _ => ev.cwd.clone(),
    };
    let resolve = |p: &str| -> Option<PathBuf> {
        let p = Path::new(p);
        if p.is_absolute() {
            Some(p.to_path_buf())
        } else {
            base.as_ref().map(|b| b.join(p))
        }
    };
    let content = resp.get("content").and_then(Value::as_str).unwrap_or("");
    let mut hits = Vec::new();
    match mode {
        "content" => {
            for l in content.lines() {
                if hits.len() >= MAX_HITS {
                    break;
                }
                if let Some(c) = hit_captures(l) {
                    let line = c[2].parse().ok();
                    if let Some(path) = resolve(&c[1]) {
                        hits.push(Hit { path, line });
                    }
                } else if let Some(f) = &single_file
                    && is_yocto_path(f)
                    && let Some(c) = line_captures(l)
                {
                    hits.push(Hit {
                        path: f.clone(),
                        line: c[1].parse().ok(),
                    });
                }
            }
        }
        _ => {
            let mut names: Vec<String> = resp
                .get("filenames")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() && mode == "count" {
                names = content
                    .lines()
                    .filter_map(|l| count_captures(l).map(|c| c[1].to_string()))
                    .collect();
            }
            for n in names.iter().take(MAX_HITS) {
                if let Some(path) = resolve(n) {
                    hits.push(Hit { path, line: None });
                }
            }
        }
    }
    hits.retain(|h| is_yocto_path(&h.path));
    hits
}

fn grep(cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>> {
    let hits = grep_hits(ev);
    if hits.is_empty() {
        return Ok(None);
    }
    let mut env = cx.env()?;
    // (file, item id) -> index into `items`, so hits in one item share a line.
    let mut items: Vec<(i64, i64, String, Vec<u32>, String)> = Vec::new();
    let mut recipe_ids: Vec<FileRow> = Vec::new();
    let mut seen_files = HashSet::new();
    for hit in &hits {
        env.check_budget()?;
        let Some(file) = env.fresh_file(&hit.path)? else {
            continue;
        };
        if seen_files.insert(file.id) {
            for r in recipes_of(&env, &file)? {
                if !recipe_ids.iter().any(|x| x.id == r.id) {
                    recipe_ids.push(r);
                }
            }
        }
        let Some(line) = hit.line else { continue };
        let Some(item) = multiline_item(&env, file.id, line)? else {
            continue;
        };
        let Some(label) = bracket(&item) else {
            continue;
        };
        match items
            .iter_mut()
            .find(|(f, i, ..)| *f == file.id && *i == item.id)
        {
            Some(entry) => entry.3.push(line),
            None => items.push((file.id, item.id, file_ptr(&file), vec![line], label)),
        }
    }
    let mut out = Vec::new();
    let total_items = items.len();
    for (_, _, ptr, lines, label) in items.into_iter().take(MAX_ITEM_LINES) {
        let lines: Vec<String> = lines.iter().map(u32::to_string).collect();
        out.push(format!("{ptr}:{} in {label}", lines.join(",")));
    }
    if total_items > MAX_ITEM_LINES {
        out.push(format!("(+{} more items)", total_items - MAX_ITEM_LINES));
    }
    let total_recipes = recipe_ids.len();
    for r in recipe_ids.iter().take(MAX_RECIPES) {
        env.check_budget()?;
        out.push(recipe_line(&env, r)?);
    }
    if total_recipes > MAX_RECIPES {
        out.push(format!("(+{} more recipes)", total_recipes - MAX_RECIPES));
    }
    if out.is_empty() {
        return Ok(None);
    }
    let mut text = String::from("sous-chef index context for the Yocto hits above:\n");
    text.push_str(&out.join("\n"));
    Ok(Some(Response::context(text)))
}

// ---- Read ----

/// The parts of a Read `tool_response` the handler uses.
///
/// Real Read events (Claude Code 2.1.326, `tests/fixtures/hooks/captured/`) keep the final
/// `\n` of a file that ends in one in `content`, and count the empty "line" after it in
/// `numLines` and `totalLines` (10 for a 9-line file). So the number of lines returned is
/// taken from `content` (its `\n`-separated parts; `numLines` only when `content` is
/// absent), and the handler clamps the range to the lines the file on disk really has
/// ([`line_count`], [`ReadRange::end_in`]). `totalLines` is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRange {
    pub path: PathBuf,
    /// First line returned, 1-based (confirmed with a captured event).
    pub start: u32,
    /// Lines returned: the `\n`-separated parts of `content`, one more than the file has
    /// when the read reached a final newline ([`ReadRange::end_in`] clamps it).
    pub lines: u32,
    /// The whole-file read was cut to the first page by the token cap.
    pub truncated: bool,
}

impl ReadRange {
    /// The last line returned, given that the file has `total` lines; `None` when the read
    /// returned no line of the file.
    pub fn end_in(&self, total: u32) -> Option<u32> {
        if self.lines == 0 || self.start > total {
            return None;
        }
        Some(self.start.saturating_add(self.lines - 1).min(total))
    }
}

/// Lines in a file's bytes, as BitBake and an editor number them: a final `\n` ends the
/// last line, it does not start another.
pub fn line_count(bytes: &[u8]) -> u32 {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    let partial = usize::from(bytes.last().is_some_and(|b| *b != b'\n'));
    u32::try_from(newlines + partial).unwrap_or(u32::MAX)
}

/// The range of a text Read, or `None` for other results (images, PDFs, notebooks) and
/// anything unexpected.
pub fn read_range(ev: &Event) -> Option<ReadRange> {
    let resp = &ev.tool_response;
    if resp.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let file = resp.get("file")?;
    let num = |k: &str| {
        file.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let path = file
        .get("filePath")
        .and_then(Value::as_str)
        .or_else(|| ev.input_str("file_path"))
        .and_then(|p| ev.abs_path(p))?;
    let lines = match file.get("content") {
        Some(Value::String(c)) if c.is_empty() => 0,
        Some(Value::String(c)) => u32::try_from(c.split('\n').count()).ok()?,
        Some(_) => return None,
        None => num("numLines")?,
    };
    Some(ReadRange {
        path,
        start: num("startLine")?.max(1),
        lines,
        truncated: file
            .get("truncatedByTokenCap")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn read(cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>> {
    let Some(r) = read_range(ev) else {
        return Ok(None);
    };
    if !is_yocto_path(&r.path) || r.lines == 0 {
        return Ok(None);
    }
    let mut env = cx.env()?;
    let Some(file) = env.fresh_file(&r.path)? else {
        return Ok(None);
    };
    // `fresh_file` checked that the bytes on disk are the indexed ones, so this line count
    // is the one the index's spans refer to.
    let total = line_count(&std::fs::read(env.ws.confine(&r.path)?)?);
    let Some(end) = r.end_in(total) else {
        return Ok(None);
    };
    let mut out = Vec::new();
    if r.start > 1
        && let Some(item) = multiline_item(&env, file.id, r.start)?
        && item.loc.lines.start < r.start
        && let Some(label) = bracket(&item)
    {
        out.push(format!(
            "The read starts at line {} inside {label}, which begins at line {} ({}).",
            r.start, item.loc.lines.start, item.loc
        ));
    }
    if end < total
        && let Some(item) = multiline_item(&env, file.id, end)?
        && item.loc.lines.end > end
        && let Some(label) = bracket(&item)
    {
        out.push(format!(
            "The read ends at line {end} inside {label}, which ends at line {} ({}).",
            item.loc.lines.end, item.loc
        ));
    }
    let whole = r.start == 1 && end >= total && !r.truncated;
    if whole && file.kind == FileKind::Recipe {
        env.check_budget()?;
        let mut parts = Vec::new();
        for (kind, f) in included_files(&env, file.id)? {
            parts.push(format!("{} ({kind})", file_ptr(&f)));
        }
        for f in query::appends_for(env.index, file.id)? {
            parts.push(format!("{} (bbappend)", file_ptr(&f)));
        }
        if !parts.is_empty() {
            out.push(format!(
                "{} is not the whole recipe; these files also contribute: {}.",
                file_name(&file),
                parts.join(", ")
            ));
        }
    }
    if out.is_empty() {
        return Ok(None);
    }
    let mut text = String::from("sous-chef index context for this read of ");
    text.push_str(
        &file
            .loc(LineSpan::new(r.start, end.max(r.start)))
            .to_string(),
    );
    text.push_str(":\n");
    text.push_str(&out.join("\n"));
    Ok(Some(Response::context(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(json: &str) -> Event {
        Event::parse(json).unwrap()
    }

    #[test]
    fn content_hits_absolute_relative_and_single_file() {
        let e = ev(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Grep","cwd":"/w",
            "tool_input":{"pattern":"x","path":"sub"},
            "tool_response":{"mode":"content","numFiles":2,"filenames":[],
              "content":"/a/b.conf.d/x.bb:3:foo\nrel/y.bbappend:12:bar:baz\nz.c:4:no\n/a/q.inc-5-context\n--\n/a/q.inc:6:x"}}"#,
        );
        assert_eq!(
            grep_hits(&e),
            vec![
                Hit {
                    path: "/a/b.conf.d/x.bb".into(),
                    line: Some(3)
                },
                Hit {
                    path: "/w/sub/rel/y.bbappend".into(),
                    line: Some(12)
                },
                Hit {
                    path: "/a/q.inc".into(),
                    line: Some(6)
                },
            ]
        );
    }

    #[test]
    fn files_and_count_modes() {
        let e = ev(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Grep","cwd":"/w",
            "tool_input":{"pattern":"x"},
            "tool_response":{"mode":"files_with_matches","numFiles":2,"filenames":["/a/x.bb","/a/x.c","r.inc"]}}"#,
        );
        let hits = grep_hits(&e);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].path, PathBuf::from("/w/r.inc"));
        let e = ev(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Grep","cwd":"/w",
            "tool_input":{"pattern":"x","output_mode":"count"},
            "tool_response":{"numFiles":1,"filenames":[],"content":"/a/x.bb:3\n/a/x.c:1"}}"#,
        );
        assert_eq!(
            grep_hits(&e),
            vec![Hit {
                path: "/a/x.bb".into(),
                line: None
            }]
        );
    }

    #[test]
    fn read_range_needs_a_text_result() {
        let e = ev(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Read","cwd":"/w",
            "tool_input":{"file_path":"/a/x.bb"},
            "tool_response":{"type":"image","file":{"base64":"","type":"image/png","originalSize":1}}}"#,
        );
        assert_eq!(read_range(&e), None);
        let e = ev(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Read","cwd":"/w",
            "tool_input":{"file_path":"/a/x.bb","offset":5,"limit":2},
            "tool_response":{"type":"text","file":{"filePath":"/a/x.bb","content":"a\nb","numLines":2,"startLine":5,"totalLines":9}}}"#,
        );
        assert_eq!(
            read_range(&e),
            Some(ReadRange {
                path: "/a/x.bb".into(),
                start: 5,
                lines: 2,
                truncated: false
            })
        );
    }

    #[test]
    fn lines_come_from_the_content_not_the_phantom_count() {
        // A whole 3-line file that ends in a newline, as Claude Code 2.1.326 reports it.
        let e = ev(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Read","cwd":"/w",
            "tool_input":{"file_path":"/w/x.bb"},
            "tool_response":{"type":"text","file":{"filePath":"x.bb","content":"a\nb\nc\n","numLines":4,"startLine":1,"totalLines":4}}}"#,
        );
        let r = read_range(&e).unwrap();
        assert_eq!(r.path, PathBuf::from("/w/x.bb"));
        assert_eq!(r.lines, 4);
        assert_eq!(r.end_in(line_count(b"a\nb\nc\n")), Some(3));
        // A read whose last line is an empty line inside the file is not clamped.
        assert_eq!(r.end_in(5), Some(4));
        assert_eq!(r.end_in(0), None);
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a"), 1);
        assert_eq!(line_count(b"a\n\n"), 2);
    }
}
