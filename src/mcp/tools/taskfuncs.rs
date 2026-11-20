//! What BitBake runs with a task besides the task's own function: its `[prefuncs]` and
//! `[postfuncs]`, and the flags that change how each function runs (`[cleandirs]`,
//! `[dirs]`, `[lockfiles]`). Shared by `var` (asked for a task function) and
//! `recipe show=tasks`. See "What a task runs" in docs/bitbake-semantics.md.
//!
//! BitBake 2.18 `lib/bb/build.py`: `_exec_task` reads `getVarFlag(task, 'prefuncs',
//! expand=True)` and `postfuncs` from the task's data, then runs `exec_func` on each
//! prefunc (split on whitespace), the task, and each postfunc, in that order. `exec_func`
//! on its own (a function called as a helper) does not read them. `exec_func(func)` first
//! removes and recreates every `[cleandirs]` directory, creates every `[dirs]` directory and
//! runs in the last one, holds `[lockfiles]`, and warns `Function X doesn't exist` (then
//! goes on) when the function has no value.
//!
//! Anonymous python and event handlers (`staging.bbclass`'s `staging_taskhandler`,
//! `sstate.bbclass`) change these flags through computed task names at the end of
//! parsing. They are not run: [`may_write`] reads their bodies and names those whose
//! writes could reach the task.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::item::read_confined;
use crate::eval::{DataStore, OpStatus};
use crate::types::Loc;
use crate::workspace::Workspace;

/// The two flags `_exec_task` runs, in run order around the task.
pub const RUN_FLAGS: [&str; 2] = ["prefuncs", "postfuncs"];

/// At most this many [`may_write`] notes; the rest are counted.
pub const MAX_WRITER_NOTES: usize = 3;

/// One word of a `[prefuncs]` / `[postfuncs]` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncRef {
    /// The function name, as the expanded flag has it (as written when it did not expand).
    pub name: String,
    /// Where the function is defined: its pieces as pointers (`loc -> class_fn` for an
    /// `EXPORT_FUNCTIONS` stub), or why there is none.
    pub defined: String,
    /// The flag operation that put the word in the flag, when one is found.
    pub set_at: Option<Loc>,
    /// That operation as written (`+= "${RMINITDIR}"`).
    pub set_op: Option<String>,
}

/// The functions of one flag of a task, in run order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagFuncs {
    pub flag: &'static str,
    pub funcs: Vec<FuncRef>,
    /// Why the expanded value may differ in BitBake (python left unevaluated, unresolved
    /// references), or `None` when it is certain.
    pub uncertain: Option<String>,
}

/// The functions of `name[flag]`, `None` when the flag is not set (or is empty).
pub fn flag_funcs(ds: &DataStore, name: &str, flag: &'static str) -> Option<FlagFuncs> {
    let v = ds.flag(name, flag)?;
    let words: Vec<&str> = v.text.split_whitespace().collect();
    if words.is_empty() && !v.unevaluated_python && v.unresolved.is_empty() {
        return None;
    }
    // Which operation contributed each word: the first one (still in effect) whose own
    // expanded value has it.
    let ops: Vec<(Loc, String, Vec<String>)> = ds
        .flag_history(name, flag)
        .into_iter()
        .filter(|(_, st)| matches!(st, OpStatus::Applied | OpStatus::Removed))
        .map(|(op, _)| {
            let words = ds
                .expand(&op.value)
                .text
                .split_whitespace()
                .map(str::to_string)
                .collect();
            (op.loc.clone(), format!("{} \"{}\"", op.op, op.value), words)
        })
        .collect();
    let last = ops.last().map(|(l, s, _)| (l.clone(), s.clone()));
    let funcs = words
        .iter()
        .map(|w| {
            let (set_at, set_op) = ops
                .iter()
                .find(|(_, _, ws)| ws.iter().any(|x| x == w))
                .map(|(l, s, _)| (Some(l.clone()), Some(s.clone())))
                .or_else(|| last.clone().map(|(l, s)| (Some(l), Some(s))))
                .unwrap_or((None, None));
            FuncRef {
                name: (*w).to_string(),
                defined: defined_at(ds, w),
                set_at,
                set_op,
            }
        })
        .collect();
    let uncertain = if v.unevaluated_python || !v.unresolved.is_empty() {
        let mut why = String::new();
        if v.unevaluated_python {
            why.push_str("it holds python that is not evaluated");
        }
        let rest = super::var::unresolved_note(ds, &v);
        if !rest.is_empty() {
            if why.is_empty() {
                why.push_str("its value");
            }
            why.push_str(&rest);
        }
        Some(why)
    } else {
        None
    };
    Some(FlagFuncs {
        flag,
        funcs,
        uncertain,
    })
}

/// The `funcs` cell of `recipe show=tasks`: `pre: a b; post: c d` (names in run order, a
/// list whose value is uncertain marked `(uncertain)`), `-` when neither flag is set.
pub fn cell(ds: &DataStore, task: &str) -> String {
    let parts: Vec<String> = RUN_FLAGS
        .iter()
        .filter_map(|flag| {
            let ff = flag_funcs(ds, task, flag)?;
            let names: Vec<&str> = ff.funcs.iter().map(|f| f.name.as_str()).collect();
            let mark = if ff.uncertain.is_some() {
                " (uncertain)"
            } else {
                ""
            };
            Some(format!(
                "{}: {}{mark}",
                &flag[..flag.len() - "funcs".len()],
                names.join(" ")
            ))
        })
        .collect();
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join("; ")
    }
}

/// Where function `f` is defined, as pointers, or why there is none.
pub fn defined_at(ds: &DataStore, f: &str) -> String {
    if f.contains("${") {
        return "(not expanded)".into();
    }
    match ds.function(f) {
        Some(ps) if !ps.is_empty() && ds.is_function(f) => ps
            .iter()
            .map(|p| match &p.calls {
                Some(c) => format!("{} -> {c}", p.loc),
                None => p.loc.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(_) => "(set, but not as a function)".into(),
        None => "(not defined: BitBake warns \"Function doesn't exist\" and goes on)".into(),
    }
}

/// The flags of `task` other than the run lists that change how its function runs, each
/// with the value as written and the last operation that set it: `[cleandirs]` (removed
/// and recreated first), `[dirs]` (created; the last is the working directory),
/// `[lockfiles]` (held while it runs). Empty when none is set.
pub fn run_flags(ds: &DataStore, task: &str) -> Vec<String> {
    const SHOWN: [(&str, &str); 3] = [
        ("cleandirs", "removed and recreated first"),
        ("dirs", "created; it runs in the last"),
        ("lockfiles", "held while it runs"),
    ];
    let mut out = Vec::new();
    for (flag, what) in SHOWN {
        let Some(raw) = ds.raw_flag(task, flag) else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let at = ds
            .flag_history(task, flag)
            .into_iter()
            .filter(|(_, st)| *st == OpStatus::Applied)
            .map(|(op, _)| op.loc)
            .next_back();
        let at = at.map(|l| format!(" at {l}")).unwrap_or_default();
        out.push(format!("[{flag}] \"{}\"{at} ({what})", raw.trim()));
    }
    out
}

/// A function the replay did not run that may change `task[prefuncs]` or
/// `task[postfuncs]`, with the writes found in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MayWrite {
    /// `anonymous python` or `event handler <name>`.
    pub what: String,
    /// The function.
    pub at: Loc,
    pub writes: Vec<Write>,
}

/// One write to a run list in a function body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// `prepend`, `append`, `set` or `delete`.
    pub verb: &'static str,
    /// `prefuncs` or `postfuncs`.
    pub flag: String,
    /// The functions it adds, when written as a literal; else `None`.
    pub funcs: Option<String>,
    /// The task name is computed (a loop variable, a parameter), not written literally.
    pub computed: bool,
}

impl Write {
    /// `prepend sstate_task_prefunc to [prefuncs]`, `set [postfuncs]`.
    fn phrase(&self) -> String {
        match (self.verb, &self.funcs) {
            ("prepend" | "append", Some(f)) => format!("{} {f} to [{}]", self.verb, self.flag),
            ("set", Some(f)) => format!("set [{}] to \"{f}\"", self.flag),
            (v, _) => format!("{v} [{}]", self.flag),
        }
    }
}

impl MayWrite {
    /// The note line: `note: not run: <what> at <loc> may <writes> of <task | the tasks it
    /// computes>`.
    pub fn note(&self, task: &str) -> String {
        let phrases: Vec<String> = self.writes.iter().map(Write::phrase).collect();
        let of = if self.writes.iter().any(|w| w.computed) {
            "of the tasks it computes".to_string()
        } else {
            format!("of {task}")
        };
        format!(
            "note: not run: {} at {} may {} {of}",
            self.what,
            self.at,
            phrases.join(", ")
        )
    }
}

/// The anonymous python functions (not proved to return at once) and the event handlers of
/// the evaluation whose bodies write `[prefuncs]` / `[postfuncs]` through `d.setVarFlag`,
/// `appendVarFlag`, `prependVarFlag` or `delVarFlag`, either naming `task` or through a
/// computed task name. A write is left out when the static evaluator proves it does not
/// reach `task`: an enclosing `if` / `elif` whose condition evaluates to false
/// ([`DataStore::python_value`]), or a loop `for t in <list>` over the words of a variable
/// read with `d.getVar('X')` whose certain value does not hold `task`. Files that cannot
/// be read are skipped.
pub fn may_write(ws: &Workspace, ds: &DataStore, task: &str) -> Vec<MayWrite> {
    let notes = ds.replay();
    let mut bodies: Vec<(String, Loc)> = notes
        .anonymous
        .iter()
        .filter(|a| a.returns_early.is_none())
        .map(|a| ("anonymous python".to_string(), a.loc.clone()))
        .collect();
    for (h, _) in &notes.handlers {
        if let Some(ps) = ds.function(h) {
            for p in ps {
                let entry = (format!("event handler {h}"), p.loc);
                if !bodies.contains(&entry) {
                    bodies.push(entry);
                }
            }
        }
    }
    let mut files: HashMap<(String, std::path::PathBuf), Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for (what, loc) in bodies {
        let key = (loc.layer.clone(), loc.path.clone());
        let text = files
            .entry(key)
            .or_insert_with(|| {
                read_confined(ws, &loc)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
            })
            .as_deref();
        let Some(text) = text else { continue };
        let start = loc.lines.start.saturating_sub(1) as usize;
        let end = loc.lines.end as usize;
        let lines: Vec<&str> = text
            .lines()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        let writes = writes_in(ds, &lines, task);
        if !writes.is_empty() {
            out.push(MayWrite {
                what,
                at: loc.clone(),
                writes,
            });
        }
    }
    out
}

static WRITE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"\bd\.(setVarFlag|appendVarFlag|prependVarFlag|delVarFlag)\s*\(\s*([^,]+?)\s*,\s*['"](prefuncs|postfuncs)['"]\s*(?:,\s*(.*))?$"#,
    )
    .ok()
});
static LITERAL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"^(?:'([^'\\]*)'|"([^"\\]*)")"#).ok());
static GETVAR: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"d\.getVar\(\s*['"]([A-Za-z0-9_\-]+)['"]"#).ok());

/// The writes to `task`'s run lists in one function body (`lines`, the whole function).
fn writes_in(ds: &DataStore, lines: &[&str], task: &str) -> Vec<Write> {
    let (Some(write), Some(literal)) = (WRITE.as_ref(), LITERAL.as_ref()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let code = l.trim_start();
        if code.starts_with('#') {
            continue;
        }
        let Some(c) = write.captures(l) else { continue };
        let method = c.get(1).map_or("", |m| m.as_str());
        let target = c.get(2).map_or("", |m| m.as_str()).trim();
        let flag = c.get(3).map_or("", |m| m.as_str()).to_string();
        let computed = match literal.captures(target) {
            Some(n) => {
                let name = n.get(1).or_else(|| n.get(2)).map_or("", |m| m.as_str());
                if name != task {
                    continue;
                }
                false
            }
            None => true,
        };
        if guarded_false(ds, lines, i) {
            continue;
        }
        if computed && loop_excludes(ds, lines, i, target, task) {
            continue;
        }
        let funcs = c
            .get(4)
            .and_then(|v| literal.captures(v.as_str().trim()))
            .and_then(|n| n.get(1).or_else(|| n.get(2)))
            .map(|m| m.as_str().split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty());
        let verb = match method {
            "prependVarFlag" => "prepend",
            "appendVarFlag" => "append",
            "delVarFlag" => "delete",
            _ => "set",
        };
        let w = Write {
            verb,
            flag,
            funcs,
            computed,
        };
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

/// The block headers above line `i` (each less indented than the last), innermost first,
/// up to the function's own first line. Stops at a `def` (a nested function: its call
/// sites are not followed).
fn headers<'a>(lines: &[&'a str], i: usize) -> Vec<&'a str> {
    let mut cur = indent(lines[i]);
    let mut out = Vec::new();
    for l in lines[..i].iter().rev() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ind = indent(l);
        if ind >= cur {
            continue;
        }
        if t.starts_with("def ") || ind == 0 {
            break;
        }
        out.push(t);
        cur = ind;
    }
    out
}

/// An enclosing `if <cond>:` / `elif <cond>:` whose condition the static evaluator proves
/// false: the write cannot happen.
fn guarded_false(ds: &DataStore, lines: &[&str], i: usize) -> bool {
    headers(lines, i).iter().any(|h| {
        let cond = h
            .strip_prefix("if ")
            .or_else(|| h.strip_prefix("elif "))
            .and_then(|c| c.strip_suffix(':'));
        cond.and_then(|c| ds.python_value(c.trim()))
            .is_some_and(|v| v.truthy().is_ok_and(|t| !t))
    })
}

/// The write's task name is the variable of an enclosing `for <name> in <list>:` whose list
/// is built from one variable read with `d.getVar('X')` (directly, or through a local
/// assigned above), and `X`'s certain value does not hold `task` as a word.
fn loop_excludes(ds: &DataStore, lines: &[&str], i: usize, target: &str, task: &str) -> bool {
    let Some(getvar) = GETVAR.as_ref() else {
        return false;
    };
    let Some(list) = headers(lines, i).iter().find_map(|h| {
        let rest = h.strip_prefix("for ")?.strip_suffix(':')?;
        let (var, list) = rest.split_once(" in ")?;
        (var.trim() == target).then(|| list.trim().to_string())
    }) else {
        return false;
    };
    let source = if list.chars().all(|c| c.is_alphanumeric() || c == '_') {
        // The last assignment to the local above the loop.
        let prefix = format!("{list} =");
        match lines[..i]
            .iter()
            .rev()
            .map(|l| l.trim())
            .find(|l| l.starts_with(&prefix) && !l.starts_with(&format!("{list} ==")))
        {
            Some(l) => l[prefix.len()..].to_string(),
            None => return false,
        }
    } else {
        list
    };
    let names: Vec<&str> = getvar
        .captures_iter(&source)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let [name] = names.as_slice() else {
        return false;
    };
    if source.matches("d.").count() != 1 {
        return false;
    }
    if ds.replay().python_vars.contains(*name) {
        return false;
    }
    match ds.value(name) {
        Some(v) if !v.unevaluated_python && v.unresolved.is_empty() => {
            !v.text.split_whitespace().any(|w| w == task)
        }
        _ => false,
    }
}
