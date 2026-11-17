//! Streaming parser for `bitbake -e` output (T5.5), and [`compare`], which sets the static
//! evaluator's final values against BitBake's.
//!
//! The format was confirmed against real output of BitBake 2.18.0 (`bitbake -e curl` on
//! the live environment, recorded in `tests/fixtures/outputs/envdump/`) and against the
//! code that writes it: `lib/bb/cooker.py` `showEnvironment`, `lib/bb/data.py`
//! `emit_var` / `emit_env` and `lib/bb/data_smart.py` `IncludeHistory.emit` /
//! `VariableHistory.emit`. docs/bitbake-semantics.md ("Build output") has the description;
//! in short, in order:
//!
//! 1. Anything a wrapper or the cooker printed first (kas log lines, `NOTE:` lines,
//!    `Loading cache...done.`, `Parsing recipes...done.`). Skipped and counted as
//!    [`ParseStats::leading`].
//! 2. `#`, `# INCLUDE HISTORY:`, `#`, then one `# <path>` line per configuration file,
//!    indented two spaces per include level, `includes:` after a file that includes others.
//! 3. Every variable that is not a function, sorted by name, as a block:
//!    `#`, `# $NAME` (one operation) or `# $NAME [N operations]`; per operation
//!    `#   <op> <file>:<line>` (plus ` [<function>]` and only the file's basename when a
//!    python function such as `appendVar` recorded it) and `#     [<flag>] "<detail>"`
//!    (the flag part only for a flag operation; continuation lines start `#     `); with
//!    more than one operation `# pre-expansion value:` and `#   "<value>"` (continuation
//!    lines start with `#` alone); then the final assignment, `NAME="<value>"` or
//!    `export NAME="<value>"`, or `unset NAME` when the variable has the `unexport` flag,
//!    or nothing when the value is `None` (only flags were set, or every operation was
//!    conditional). A variable without history prints `#   [no history recorded]`, `#`,
//!    `#   "<value>"`. Override variants (`T:task-clean`) are blocks of their own, and the
//!    base block repeats their operations as `override[<override>]:<op>`.
//! 4. Shell functions, sorted: `# line: <n>, file: <file>`, `NAME() {`, the body, `}`, a
//!    blank line. Functions have no history block.
//! 5. Python functions, sorted: a blank line, `python NAME () {`, the body as stored, `}`
//!    (on the body's last line when the body does not end with a newline), a blank line.
//!    No location is printed for them.
//!
//! Values in assignments are escaped by `emit_var`: `"`, `` ` `` and `$` get a backslash,
//! and each newline becomes ` \` plus a newline; backslashes themselves are not escaped.
//! Details and pre-expansion values in comments are not escaped at all.
//!
//! The parser never panics: a line it does not recognise is skipped and counted
//! ([`ParseStats::skipped`], with the first few kept as examples). It reads one line at a
//! time ([`Parser::push`]) and hands over each variable or function as soon as its block
//! ends ([`for_each_entry`]), so a caller that wants one variable keeps one
//! ([`parse_filtered`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};
use std::path::Path;

use crate::error::Result;
use crate::eval::{self, DataStore, Target};
use crate::index::Index;
use crate::types::LineSpan;
use crate::workspace::Workspace;

/// How many skipped lines [`ParseStats::examples`] keeps.
pub const MAX_EXAMPLES: usize = 5;

/// A parsed `bitbake -e` dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvDump {
    /// By name as printed in `# $NAME` (override variants such as `T:task-clean` are
    /// entries of their own).
    pub variables: BTreeMap<String, Variable>,
    /// Shell and python functions, by name.
    pub functions: BTreeMap<String, Function>,
    /// The include history, in order.
    pub includes: Vec<Include>,
    pub stats: ParseStats,
}

/// Line counts from a parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// Lines read.
    pub lines: u32,
    /// Lines before the dump started (wrapper and cooker output).
    pub leading: u32,
    /// Lines after the start that were not recognised and were skipped.
    pub skipped: u32,
    /// The first [`MAX_EXAMPLES`] skipped lines, with their 1-based line numbers.
    pub examples: Vec<(u32, String)>,
    /// Whether the dump started (`# INCLUDE HISTORY:`, or a variable or function header).
    /// False means the text was not `bitbake -e` output, or BitBake failed before printing.
    pub found_header: bool,
}

/// One file of the include history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    /// 0 for a top-level file.
    pub depth: u32,
    /// As printed (absolute, or relative to `BBPATH` such as `conf/site.conf`).
    pub path: String,
}

/// Whether the final assignment was exported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Export {
    /// `NAME="..."`, or no assignment.
    #[default]
    No,
    /// `export NAME="..."`.
    Exported,
    /// `unset NAME`: the variable has the `unexport` flag, and `bitbake -e` prints no value.
    Unexported,
}

/// One variable block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    /// The block's lines (from `# $NAME` to the assignment), 1-based.
    pub span: LineSpan,
    /// The history, in the order BitBake printed it: the variable's own operations in the
    /// order they ran, then the operations of its override variants (`override[...]:`).
    pub ops: Vec<EnvOp>,
    /// `# pre-expansion value:` (printed only with more than one operation), or the value
    /// of a variable without history. `"None"` is how BitBake prints an unset value.
    pub pre_expansion: Option<String>,
    /// The final, expanded value, unescaped. `None` when no assignment was printed (the
    /// value is `None`, the variable is unexported, or its expansion failed).
    pub value: Option<String>,
    pub export: Export,
    /// `#   [no history recorded]`.
    pub no_history: bool,
    /// `# expansion of NAME threw <Error>: <message>` (then there is no block).
    pub error: Option<String>,
}

/// The kind of a history operation, from the operation text BitBake prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    /// `set`: `=`, or `setVar` from python.
    Set,
    /// `set?`: `?=`.
    Default,
    /// `set` of the `_defaultval` flag: `??=` (the flag is removed from [`EnvOp::flag`]).
    WeakDefault,
    /// `immediate`: `:=`.
    Immediate,
    /// `append`: `+=`, or `appendVar` / `appendVarFlag` from python (no space added).
    Append,
    /// `prepend`: `=+`, or `prependVar` from python.
    Prepend,
    /// `postdot`: `.=`.
    AppendDot,
    /// `predot`: `=.`.
    PrependDot,
    /// `:append`, `:append[cond]`: the `:append` override; the condition is in
    /// [`EnvOp::condition`].
    OverrideAppend,
    /// `:prepend`, `:prepend[cond]`.
    OverridePrepend,
    /// `:remove`, `:remove[cond]`.
    OverrideRemove,
    /// `exported`: `export NAME` (a set of the `export` flag).
    Exported,
    /// `del`: `unset NAME` or `delVar`.
    Del,
    /// `delFlag`.
    DelFlag,
    /// `rename from <old>`: the key was renamed when it was expanded (`FILES:${PN}`
    /// becomes `FILES:curl`).
    RenameFrom(String),
    /// `rename (to)`.
    RenameTo,
    /// `from env`: taken from the environment BitBake started in.
    FromEnv,
    /// `auto env export`: exported because it came from the environment.
    EnvExport,
    /// Any other operation text, as printed.
    Other(String),
}

/// One history operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvOp {
    pub kind: OpKind,
    /// The operation text as printed, e.g. `override[class-native]:set`.
    pub raw: String,
    /// From the `override[...]` prefix: the override variant this operation belongs to
    /// (split at `:`), empty for the variable's own operations.
    pub overrides: Vec<String>,
    /// The condition of `:append[cond]` / `:prepend[cond]` / `:remove[cond]` (split at
    /// `:`).
    pub condition: Vec<String>,
    /// As printed: absolute, or a basename when [`EnvOp::func`] is set.
    pub file: String,
    pub line: Option<u32>,
    /// The python function that recorded the operation (`appendVar`, `expandKeys`).
    pub func: Option<String>,
    pub flag: Option<String>,
    /// The detail: the value as written in the statement (not the result).
    pub value: String,
}

/// A shell or python function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub python: bool,
    /// From `# line: N, file: F` (shell functions only; `autogenerated` for an
    /// `EXPORT_FUNCTIONS` stub).
    pub file: Option<String>,
    pub line: Option<u32>,
    /// The body (expanded, as BitBake would run it), without the braces.
    pub body: String,
    /// The block's lines (from `# line:` or the header to the closing `}`), 1-based.
    pub span: LineSpan,
}

/// One item of the dump, handed over as soon as its block ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Variable(Variable),
    Function(Function),
    Include(Include),
}

impl Entry {
    /// The variable or function name (`None` for an include).
    pub fn name(&self) -> Option<&str> {
        match self {
            Entry::Variable(v) => Some(&v.name),
            Entry::Function(f) => Some(&f.name),
            Entry::Include(_) => None,
        }
    }

    /// The block's lines (`None` for an include).
    pub fn span(&self) -> Option<LineSpan> {
        match self {
            Entry::Variable(v) => Some(v.span),
            Entry::Function(f) => Some(f.span),
            Entry::Include(_) => None,
        }
    }
}

impl EnvOp {
    /// Every override the operation depends on: its variant's overrides, then its
    /// condition.
    pub fn conditions(&self) -> impl Iterator<Item = &str> {
        self.overrides
            .iter()
            .chain(self.condition.iter())
            .map(String::as_str)
    }

    /// `file:line` as printed, with the path made lexically absolute (`build/../meta`
    /// becomes `meta`), or the workspace pointer (`layer:path:line`) when the file is in
    /// the workspace.
    pub fn location(&self, ws: Option<&Workspace>) -> String {
        location(ws, &self.file, self.line)
    }
}

/// `file:line` for a location printed by BitBake: a workspace pointer when `file` is an
/// absolute path inside the workspace (through [`Workspace::loc_for`]), else the path,
/// made lexically absolute when it is absolute.
pub fn location(ws: Option<&Workspace>, file: &str, line: Option<u32>) -> String {
    let path = Path::new(file);
    if path.is_absolute() {
        if let (Some(ws), Some(n)) = (ws, line)
            && let Some(loc) = ws.loc_for(path, LineSpan::line(n))
        {
            return loc.to_string();
        }
        let shown = lexical(path);
        return match line {
            Some(n) => format!("{shown}:{n}"),
            None => shown,
        };
    }
    match line {
        Some(n) => format!("{file}:{n}"),
        None => file.to_string(),
    }
}

/// `..` and `.` removed without touching the file system.
fn lexical(path: &Path) -> String {
    use std::path::Component;
    let mut out: Vec<&std::ffi::OsStr> = Vec::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let mut s = String::new();
    for p in out {
        s.push('/');
        s.push_str(&p.to_string_lossy());
    }
    if s.is_empty() { "/".into() } else { s }
}

impl Variable {
    /// An empty block for `name` starting at `line`.
    pub fn new(name: &str, line: u32) -> Variable {
        Variable {
            name: name.to_string(),
            span: LineSpan::line(line),
            ops: Vec::new(),
            pre_expansion: None,
            value: None,
            export: Export::No,
            no_history: false,
            error: None,
        }
    }

    /// Operations on the value (no flag; `??=` included).
    pub fn value_ops(&self) -> impl Iterator<Item = &EnvOp> {
        self.ops.iter().filter(|o| o.flag.is_none())
    }

    /// Operations on flag `flag`.
    pub fn flag_ops<'a>(&'a self, flag: &'a str) -> impl Iterator<Item = &'a EnvOp> + 'a {
        self.ops
            .iter()
            .filter(move |o| o.flag.as_deref() == Some(flag))
    }

    /// The flags that have operations, sorted.
    pub fn flag_names(&self) -> Vec<String> {
        let set: BTreeSet<&str> = self.ops.iter().filter_map(|o| o.flag.as_deref()).collect();
        set.into_iter().map(str::to_string).collect()
    }

    /// The value of flag `flag` when the history gives it without guessing: the last
    /// operation on the flag is a plain set (`set`, `immediate`, `exported`, `auto env
    /// export`), or the only operation is a default. `bitbake -e` prints no final flag
    /// values, so anything else (appends, removals, python) gives `None`.
    pub fn flag_value(&self, flag: &str) -> Option<String> {
        let ops: Vec<&EnvOp> = self.flag_ops(flag).collect();
        let last = ops.last()?;
        if !last.overrides.is_empty() || !last.condition.is_empty() {
            return None;
        }
        match last.kind {
            OpKind::Set | OpKind::Immediate | OpKind::Exported | OpKind::EnvExport => {
                Some(last.value.clone())
            }
            OpKind::Default | OpKind::WeakDefault if ops.len() == 1 => Some(last.value.clone()),
            _ => None,
        }
    }
}

impl EnvDump {
    pub fn variable(&self, name: &str) -> Option<&Variable> {
        self.variables.get(name)
    }

    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.get(name)
    }

    /// The final value of `name`, if BitBake printed one.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.variables.get(name)?.value.as_deref()
    }

    /// BitBake's final `OVERRIDES`, split at `:` (empty when the dump has none).
    pub fn overrides(&self) -> Vec<String> {
        self.value("OVERRIDES")
            .map(|v| {
                v.split(':')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `text` with every `${NAME}` replaced by the dump's final value of `NAME` (one pass;
    /// final values are already expanded). References without a value are left as they
    /// are.
    pub fn expand(&self, text: &str) -> String {
        expand_with(text, |n| self.value(n).map(str::to_string))
    }

    fn add(&mut self, e: Entry) {
        match e {
            Entry::Variable(v) => {
                self.variables.insert(v.name.clone(), v);
            }
            Entry::Function(f) => {
                self.functions.insert(f.name.clone(), f);
            }
            Entry::Include(i) => self.includes.push(i),
        }
    }
}

/// `text` with `${NAME}` replaced through `value` (one pass).
pub fn expand_with(text: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find('}') {
            Some(j) => {
                let name = &after[..j];
                match value(name) {
                    Some(v) if !name.is_empty() && !name.starts_with('@') => out.push_str(&v),
                    _ => {
                        out.push_str("${");
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `${NAME}` references in `text` (not `${@...}`).
pub fn references(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        let after = &rest[i + 2..];
        match after.find('}') {
            Some(j) => {
                let name = &after[..j];
                if !name.is_empty() && !name.starts_with('@') {
                    out.push(name.to_string());
                }
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    out
}

// ---- reading ----

/// Parses a whole dump.
pub fn parse<R: BufRead>(reader: R) -> io::Result<EnvDump> {
    parse_filtered(reader, |_| true)
}

/// Parses a dump held in memory.
pub fn parse_str(text: &str) -> EnvDump {
    parse(io::Cursor::new(text.as_bytes())).unwrap_or_default()
}

/// Parses a dump, keeping only the variables and functions whose name `keep` accepts
/// (and the include history). Every line is still read and counted.
pub fn parse_filtered<R: BufRead>(reader: R, keep: impl Fn(&str) -> bool) -> io::Result<EnvDump> {
    let mut dump = EnvDump::default();
    let stats = for_each_entry(reader, |e| {
        if e.name().is_none_or(&keep) {
            dump.add(e);
        }
    })?;
    dump.stats = stats;
    Ok(dump)
}

/// Reads a dump line by line and hands each entry to `f` as soon as its block ends.
/// Invalid UTF-8 is replaced, never an error; only a read error is.
pub fn for_each_entry<R: BufRead>(
    mut reader: R,
    mut f: impl FnMut(Entry),
) -> io::Result<ParseStats> {
    let mut p = Parser::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        p.push(&String::from_utf8_lossy(&buf));
        for e in p.drain() {
            f(e);
        }
    }
    p.finish();
    for e in p.drain() {
        f(e);
    }
    Ok(p.stats)
}

/// Runs the parser over lines already split (no line terminators).
pub fn for_each_line_entry<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    mut f: impl FnMut(Entry),
) -> ParseStats {
    let mut p = Parser::new();
    for l in lines {
        p.push(l);
        for e in p.drain() {
            f(e);
        }
    }
    p.finish();
    for e in p.drain() {
        f(e);
    }
    p.stats
}

// ---- the state machine ----

/// The line-at-a-time parser. [`Parser::push`] each line, take finished entries with
/// [`Parser::drain`], call [`Parser::finish`] at the end.
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
    line: u32,
    stats: ParseStats,
    /// `# line: N, file: F` seen on the line given, waiting for its function.
    pending_loc: Option<(u32, Option<u32>, Option<String>)>,
    out: Vec<Entry>,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Preamble,
    Includes,
    Idle,
    Var(Box<VarBuild>),
    Func(Box<FuncBuild>),
}

#[derive(Debug)]
struct VarBuild {
    var: Variable,
    sub: VarSub,
    /// The operation whose detail lines are being read.
    op: Option<OpBuild>,
    /// Last line that belongs to the block.
    last: u32,
}

#[derive(Debug)]
struct OpBuild {
    op: EnvOp,
    /// Whether the first detail line was read.
    started: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum VarSub {
    Ops,
    /// After `[no history recorded]`, before its `#`.
    NoHistory,
    /// After `# pre-expansion value:` (or the no-history `#`), before the value line.
    PreFirst,
    /// Reading the pre-expansion value; `hash` when the previous line was exactly `#`
    /// (a separator, or an empty line of the value: the next line decides).
    Pre {
        hash: bool,
    },
    /// Reading a multi-line assignment.
    Value {
        exported: bool,
        lines: Vec<String>,
    },
}

#[derive(Debug)]
struct FuncBuild {
    func: Function,
    /// Line number of the header.
    header: u32,
    body: Vec<String>,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Statistics so far.
    pub fn stats(&self) -> &ParseStats {
        &self.stats
    }

    /// Finished entries, in order.
    pub fn drain(&mut self) -> std::vec::Drain<'_, Entry> {
        self.out.drain(..)
    }

    /// One line, without its terminator (a trailing `\r` is dropped).
    pub fn push(&mut self, line: &str) {
        self.line = self.line.saturating_add(1);
        self.stats.lines = self.line;
        let line = line.strip_suffix('\r').unwrap_or(line);
        match std::mem::take(&mut self.state) {
            State::Preamble => self.preamble(line),
            State::Includes => self.includes(line),
            State::Idle => self.idle(line),
            State::Var(b) => self.var(*b, line),
            State::Func(b) => self.func(*b, line),
        }
    }

    /// Ends the input: a block still open is finished.
    pub fn finish(&mut self) {
        match std::mem::take(&mut self.state) {
            State::Var(b) => self.end_var(*b),
            State::Func(b) => self.end_func(*b),
            State::Preamble | State::Includes | State::Idle => {}
        }
    }

    fn skip(&mut self, line: &str) {
        self.skip_at(self.line, line);
    }

    fn skip_at(&mut self, at: u32, line: &str) {
        self.stats.skipped += 1;
        if self.stats.examples.len() < MAX_EXAMPLES {
            self.stats.examples.push((at, line.to_string()));
        }
    }

    fn preamble(&mut self, line: &str) {
        if line == "# INCLUDE HISTORY:" {
            self.stats.found_header = true;
            self.state = State::Includes;
        } else if line.starts_with("# $") || line.starts_with("# line: ") {
            self.stats.found_header = true;
            self.idle(line);
        } else {
            if line != "#" {
                self.stats.leading += 1;
            }
            self.state = State::Preamble;
        }
    }

    fn includes(&mut self, line: &str) {
        if line == "#" {
            self.state = State::Includes;
            return;
        }
        if !line.starts_with("# $")
            && let Some(rest) = line.strip_prefix("# ")
        {
            let spaces = rest.len() - rest.trim_start_matches(' ').len();
            let path = rest.trim();
            let path = path.strip_suffix(" includes:").unwrap_or(path);
            if !path.is_empty() {
                self.out.push(Entry::Include(Include {
                    depth: u32::try_from(spaces / 2).unwrap_or(u32::MAX),
                    path: path.to_string(),
                }));
                self.state = State::Includes;
                return;
            }
        }
        self.idle(line);
    }

    fn idle(&mut self, line: &str) {
        self.state = State::Idle;
        if line.is_empty() || line == "#" {
            return;
        }
        if let Some(rest) = line.strip_prefix("# $") {
            match var_header(rest) {
                Some(name) => {
                    self.state = State::Var(Box::new(VarBuild {
                        var: Variable::new(name, self.line),
                        sub: VarSub::Ops,
                        op: None,
                        last: self.line,
                    }));
                }
                None => self.skip(line),
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("# line: ") {
            let (n, file) = func_location(rest);
            self.pending_loc = Some((self.line, n, file));
            return;
        }
        if let Some(rest) = line.strip_prefix("# expansion of ") {
            if let Some((name, err)) = rest.split_once(" threw ") {
                let mut v = Variable::new(name, self.line);
                v.error = Some(err.to_string());
                self.out.push(Entry::Variable(v));
            } else {
                self.skip(line);
            }
            return;
        }
        if let Some(name) = python_header(line) {
            self.pending_loc = None;
            self.state = State::Func(Box::new(FuncBuild {
                func: Function {
                    name: name.to_string(),
                    python: true,
                    file: None,
                    line: None,
                    body: String::new(),
                    span: LineSpan::line(self.line),
                },
                header: self.line,
                body: Vec::new(),
            }));
            return;
        }
        if let Some(name) = shell_header(line) {
            let (start, n, file) = match self.pending_loc.take() {
                Some((at, n, file)) if at + 1 == self.line => (at, n, file),
                _ => (self.line, None, None),
            };
            self.state = State::Func(Box::new(FuncBuild {
                func: Function {
                    name: name.to_string(),
                    python: false,
                    file,
                    line: n,
                    body: String::new(),
                    span: LineSpan::line(start),
                },
                header: self.line,
                body: Vec::new(),
            }));
            return;
        }
        if line.starts_with("export -f ") {
            return;
        }
        // An assignment or `unset` without a history block.
        if let Some((_, name, _)) = value_start(line) {
            let b = VarBuild {
                var: Variable::new(name, self.line),
                sub: VarSub::Ops,
                op: None,
                last: self.line,
            };
            self.var_line(b, line);
            return;
        }
        if let Some(name) = line.strip_prefix("unset ").map(str::trim)
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
        {
            let mut v = Variable::new(name, self.line);
            v.export = Export::Unexported;
            self.out.push(Entry::Variable(v));
            return;
        }
        self.skip(line);
    }

    fn var(&mut self, mut b: VarBuild, line: &str) {
        match std::mem::replace(&mut b.sub, VarSub::Ops) {
            VarSub::Value {
                exported,
                mut lines,
            } => {
                lines.push(line.to_string());
                b.last = self.line;
                if line.ends_with('"') {
                    b.var.value = Some(unescape_value(&lines));
                    b.var.export = if exported {
                        Export::Exported
                    } else {
                        Export::No
                    };
                    self.end_var(b);
                } else {
                    b.sub = VarSub::Value { exported, lines };
                    self.state = State::Var(Box::new(b));
                }
            }
            VarSub::Pre { hash } => {
                let closed = b
                    .var
                    .pre_expansion
                    .as_deref()
                    .is_some_and(|p| p.ends_with('"'));
                // A variable block starts after a `#`; the function section starts
                // directly after the last variable.
                if closed && ((hash && is_block_start(line)) || is_function_start(line)) {
                    self.end_var(b);
                    self.idle(line);
                    return;
                }
                if hash {
                    // The `#` was an empty line of the value.
                    if let Some(p) = b.var.pre_expansion.as_mut() {
                        p.push('\n');
                    }
                }
                if line == "#" {
                    b.sub = VarSub::Pre { hash: true };
                    self.state = State::Var(Box::new(b));
                } else if let Some(rest) = line.strip_prefix('#') {
                    if let Some(p) = b.var.pre_expansion.as_mut() {
                        p.push('\n');
                        p.push_str(rest);
                    }
                    b.last = self.line;
                    b.sub = VarSub::Pre { hash: false };
                    self.state = State::Var(Box::new(b));
                } else {
                    self.var_line(b, line);
                }
            }
            VarSub::PreFirst => {
                if let Some(rest) = line.strip_prefix("#   \"") {
                    b.var.pre_expansion = Some(rest.to_string());
                    b.last = self.line;
                    b.sub = VarSub::Pre { hash: false };
                    self.state = State::Var(Box::new(b));
                } else {
                    self.var_line(b, line);
                }
            }
            VarSub::NoHistory => {
                if line == "#" {
                    b.sub = VarSub::PreFirst;
                    self.state = State::Var(Box::new(b));
                } else {
                    self.var_line(b, line);
                }
            }
            VarSub::Ops => self.var_line(b, line),
        }
    }

    /// A line in the history part of a block.
    fn var_line(&mut self, mut b: VarBuild, line: &str) {
        if let Some(rest) = line.strip_prefix("#     ") {
            if let Some(op) = b.op.as_mut() {
                if op.started {
                    op.op.value.push('\n');
                    op.op.value.push_str(rest);
                } else {
                    let (flag, text) = split_detail(rest);
                    op.op.flag = flag;
                    op.op.value = text.to_string();
                    op.started = true;
                }
                b.last = self.line;
            } else {
                self.skip(line);
            }
            self.state = State::Var(Box::new(b));
            return;
        }
        if let Some(rest) = line.strip_prefix("#   ") {
            if rest == "[no history recorded]" {
                finish_op(&mut b);
                b.var.no_history = true;
                b.last = self.line;
                b.sub = VarSub::NoHistory;
                self.state = State::Var(Box::new(b));
                return;
            }
            if !rest.starts_with([' ', '"'])
                && let Some(op) = op_header(rest)
            {
                finish_op(&mut b);
                b.op = Some(OpBuild { op, started: false });
                b.last = self.line;
                self.state = State::Var(Box::new(b));
                return;
            }
        }
        if line == "# pre-expansion value:" {
            finish_op(&mut b);
            b.last = self.line;
            b.sub = VarSub::PreFirst;
            self.state = State::Var(Box::new(b));
            return;
        }
        if line == "#" || line.is_empty() || is_block_start(line) {
            self.end_var(b);
            self.idle(line);
            return;
        }
        if let Some((exported, _name, rest)) = value_start(line) {
            finish_op(&mut b);
            b.last = self.line;
            if is_closed(rest) {
                b.var.value = Some(unescape_value(&[rest.to_string()]));
                b.var.export = if exported {
                    Export::Exported
                } else {
                    Export::No
                };
                self.end_var(b);
            } else {
                b.sub = VarSub::Value {
                    exported,
                    lines: vec![rest.to_string()],
                };
                self.state = State::Var(Box::new(b));
            }
            return;
        }
        if let Some(name) = line.strip_prefix("unset ")
            && !name.trim().is_empty()
        {
            finish_op(&mut b);
            b.last = self.line;
            b.var.export = Export::Unexported;
            self.end_var(b);
            return;
        }
        self.skip(line);
        self.state = State::Var(Box::new(b));
    }

    fn end_var(&mut self, mut b: VarBuild) {
        finish_op(&mut b);
        if let Some(p) = b.var.pre_expansion.as_mut()
            && p.ends_with('"')
        {
            p.pop();
        }
        b.var.span = LineSpan::new(b.var.span.start, b.last.max(b.var.span.start));
        self.out.push(Entry::Variable(b.var));
        self.state = State::Idle;
    }

    fn func(&mut self, mut b: FuncBuild, line: &str) {
        // A function ends where the next one starts: `# line:` (shell) or a python header,
        // both at column 0 and never part of a body BitBake prints.
        let boundary =
            python_header(line).is_some() || (!b.func.python && line.starts_with("# line: "));
        if boundary {
            self.end_func(b);
            self.idle(line);
            return;
        }
        b.body.push(line.to_string());
        self.state = State::Func(Box::new(b));
    }

    fn end_func(&mut self, mut b: FuncBuild) {
        // After the closing `}` BitBake prints only blank lines. Comment lines there are
        // not part of the function: skip and count them.
        let mut k = b.body.len();
        while k > 0 && (b.body[k - 1].is_empty() || b.body[k - 1].starts_with('#')) {
            k -= 1;
        }
        if k > 0 && b.body[k - 1] == "}" {
            for (i, l) in b.body[k..].iter().enumerate() {
                if !l.is_empty() {
                    let at = b
                        .header
                        .saturating_add(u32::try_from(k + i + 1).unwrap_or(u32::MAX));
                    self.skip_at(at, l);
                }
            }
            b.body.truncate(k);
        } else {
            while b.body.last().is_some_and(|l| l.is_empty()) {
                b.body.pop();
            }
        }
        // The closing `}` is the last line kept (its own line, or the end of the body's
        // last line); without one the input was cut and the body is kept as it is.
        let end = b
            .header
            .saturating_add(u32::try_from(b.body.len()).unwrap_or(u32::MAX));
        match b.body.last_mut() {
            Some(l) if l == "}" => {
                b.body.pop();
            }
            Some(l) if l.ends_with('}') => {
                l.pop();
            }
            _ => {}
        }
        b.func.body = b.body.join("\n");
        b.func.span = LineSpan::new(b.func.span.start, end.max(b.func.span.start));
        self.out.push(Entry::Function(b.func));
        self.state = State::Idle;
    }
}

fn finish_op(b: &mut VarBuild) {
    if let Some(mut o) = b.op.take() {
        if o.op.value.ends_with('"') {
            o.op.value.pop();
        }
        // `??=` is a set of the `_defaultval` flag (`lib/bb/parse/ast.py`).
        if o.op.kind == OpKind::Set
            && let Some(f) = o.op.flag.clone()
        {
            if f == "_defaultval" {
                o.op.kind = OpKind::WeakDefault;
                o.op.flag = None;
            } else if let Some(real) = f.strip_prefix("_defaultval_flag_") {
                o.op.kind = OpKind::WeakDefault;
                o.op.flag = Some(real.to_string());
            }
        }
        b.var.ops.push(o.op);
    }
}

/// Whether `line` starts another block (so a held `#` was a separator).
fn is_block_start(line: &str) -> bool {
    line.starts_with("# $")
        || line.starts_with("# line: ")
        || line.starts_with("# expansion of ")
        || python_header(line).is_some()
        || shell_header(line).is_some()
}

/// Whether `line` starts a function (or its `# line:` comment).
fn is_function_start(line: &str) -> bool {
    line.starts_with("# line: ") || python_header(line).is_some() || shell_header(line).is_some()
}

/// `NAME` or `NAME [N operations]` after `# $`.
fn var_header(rest: &str) -> Option<&str> {
    let name = match rest.rsplit_once(" [") {
        Some((n, tail))
            if tail
                .strip_suffix(" operations]")
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) =>
        {
            n
        }
        _ => rest,
    };
    (!name.is_empty() && !name.contains(char::is_whitespace)).then_some(name)
}

/// `N, file: F` after `# line: `.
fn func_location(rest: &str) -> (Option<u32>, Option<String>) {
    match rest.split_once(", file: ") {
        Some((n, f)) => (
            n.trim().parse().ok(),
            Some(f.to_string()).filter(|f| f != "None" && !f.is_empty()),
        ),
        None => (rest.trim().parse().ok(), None),
    }
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && !s.contains(char::is_whitespace) && !s.contains(['(', ')', '"', '=', '#'])
}

/// `python NAME () {`.
fn python_header(line: &str) -> Option<&str> {
    let name = line.strip_prefix("python ")?.strip_suffix(" () {")?;
    is_name(name).then_some(name)
}

/// `NAME() {`.
fn shell_header(line: &str) -> Option<&str> {
    let name = line.strip_suffix("() {")?;
    is_name(name).then_some(name)
}

/// `NAME="rest` or `export NAME="rest`: (exported, name, text after the opening quote).
fn value_start(line: &str) -> Option<(bool, &str, &str)> {
    let (exported, l) = match line.strip_prefix("export ") {
        Some(r) => (true, r),
        None => (false, line),
    };
    let (name, rest) = l.split_once("=\"")?;
    (!name.is_empty() && !name.starts_with('#') && !name.contains(char::is_whitespace))
        .then_some((exported, name, rest))
}

/// Whether the text after an opening quote already holds the closing one.
fn is_closed(rest: &str) -> bool {
    rest.ends_with('"')
}

/// The value of an assignment from its lines (the first without `NAME="`): the closing
/// quote removed, each ` \` line end turned back into a newline, `\"`, `` \` `` and `\$`
/// unescaped (`emit_var` escapes nothing else).
fn unescape_value(lines: &[String]) -> String {
    let n = lines.len();
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        let part = if i + 1 == n {
            l.strip_suffix('"').unwrap_or(l)
        } else {
            l.strip_suffix(" \\")
                .or_else(|| l.strip_suffix('\\'))
                .unwrap_or(l)
        };
        if i > 0 {
            out.push('\n');
        }
        let mut chars = part.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\'
                && let Some(&next) = chars.peek()
                && matches!(next, '"' | '`' | '$')
            {
                out.push(next);
                chars.next();
            } else {
                out.push(c);
            }
        }
    }
    out
}

/// `[flag] "text` or `"text`: the flag and the text after the opening quote.
fn split_detail(rest: &str) -> (Option<String>, &str) {
    if let Some(r) = rest.strip_prefix('[')
        && let Some((flag, text)) = r.split_once("] ")
        && !flag.is_empty()
    {
        return (
            Some(flag.to_string()),
            text.strip_prefix('"').unwrap_or(text),
        );
    }
    (None, rest.strip_prefix('"').unwrap_or(rest))
}

/// `<op> <file>:<line>[ [func]]` after `#   `.
fn op_header(rest: &str) -> Option<EnvOp> {
    let (head, func) = match rest.strip_suffix(']').and_then(|r| r.rsplit_once(" [")) {
        Some((h, f)) if !f.is_empty() && !f.contains(' ') => (h, Some(f.to_string())),
        _ => (rest, None),
    };
    let (raw, at) = head.rsplit_once(' ')?;
    let (file, line) = match at.rsplit_once(':') {
        Some((f, n)) if !f.is_empty() => (f, n.parse().ok()),
        _ => (at, None),
    };
    if raw.is_empty() {
        return None;
    }
    let (overrides, inner) = match raw
        .strip_prefix("override[")
        .and_then(|r| r.split_once("]:"))
    {
        Some((o, inner)) => (split_overrides(o), inner),
        None => (Vec::new(), raw),
    };
    let (kind, condition) = op_kind(inner);
    Some(EnvOp {
        kind,
        raw: raw.to_string(),
        overrides,
        condition,
        file: file.to_string(),
        line,
        func,
        flag: None,
        value: String::new(),
    })
}

fn split_overrides(s: &str) -> Vec<String> {
    s.split(':')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn op_kind(op: &str) -> (OpKind, Vec<String>) {
    for (prefix, kind) in [
        (":append", OpKind::OverrideAppend),
        (":prepend", OpKind::OverridePrepend),
        (":remove", OpKind::OverrideRemove),
    ] {
        if let Some(rest) = op.strip_prefix(prefix) {
            if rest.is_empty() {
                return (kind, Vec::new());
            }
            if let Some(c) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                return (kind, split_overrides(c));
            }
        }
    }
    let kind = match op {
        "set" => OpKind::Set,
        "set?" => OpKind::Default,
        "immediate" => OpKind::Immediate,
        "append" => OpKind::Append,
        "prepend" => OpKind::Prepend,
        "postdot" => OpKind::AppendDot,
        "predot" => OpKind::PrependDot,
        "exported" => OpKind::Exported,
        "del" => OpKind::Del,
        "delFlag" => OpKind::DelFlag,
        "rename (to)" => OpKind::RenameTo,
        "from env" => OpKind::FromEnv,
        "auto env export" => OpKind::EnvExport,
        _ => match op.strip_prefix("rename from ") {
            Some(old) => OpKind::RenameFrom(old.to_string()),
            None => OpKind::Other(op.to_string()),
        },
    };
    (kind, Vec::new())
}

// ---- compare ----

/// How a static final value relates to BitBake's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agreement {
    /// The same text (or neither has a value).
    Equal,
    /// The same words, different whitespace.
    Whitespace,
    /// Different, and the static value depends on python it did not run.
    StaticPython,
    /// Different, and the static value has references it could not expand.
    StaticUnresolved,
    /// BitBake printed no value because the variable is unexported (`unset NAME`), or its
    /// expansion failed in BitBake; nothing to compare.
    NotPrinted,
    /// Only the static evaluator has a value.
    OnlyStatic,
    /// Only BitBake has a value.
    OnlyBitbake,
    /// A real difference.
    Mismatch,
}

impl Agreement {
    pub fn as_str(self) -> &'static str {
        match self {
            Agreement::Equal => "equal",
            Agreement::Whitespace => "whitespace",
            Agreement::StaticPython => "static-python",
            Agreement::StaticUnresolved => "static-unresolved",
            Agreement::NotPrinted => "not-printed",
            Agreement::OnlyStatic => "only-static",
            Agreement::OnlyBitbake => "only-bitbake",
            Agreement::Mismatch => "mismatch",
        }
    }
}

/// One variable compared.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Comparison {
    pub name: String,
    pub static_value: Option<String>,
    pub bitbake_value: Option<String>,
    pub agreement: Agreement,
}

/// Static against authoritative final values.
///
/// `ds` is the static evaluation of the same recipe (or of the configuration), `dump` the
/// parsed `bitbake -e` output for it. `names` are the variables to compare; empty means
/// every variable the dump printed a value for. Results are in the order of `names`, or
/// by name.
///
/// Classification, first match wins: neither has a value, or the texts are equal:
/// `Equal`; BitBake unexported the variable or failed to expand it: `NotPrinted`; the
/// static value depends on unevaluated python: `StaticPython`; it has unresolved
/// references: `StaticUnresolved`; one side has no value: `OnlyStatic` / `OnlyBitbake`;
/// the same words: `Whitespace`; otherwise `Mismatch`.
pub fn compare(ds: &DataStore, dump: &EnvDump, names: &[&str]) -> Vec<Comparison> {
    let all: Vec<String>;
    let names: Vec<&str> = if names.is_empty() {
        all = dump
            .variables
            .values()
            .filter(|v| v.value.is_some())
            .map(|v| v.name.clone())
            .collect();
        all.iter().map(String::as_str).collect()
    } else {
        names.to_vec()
    };
    names
        .into_iter()
        .map(|name| {
            let st = ds.value(name);
            let bb = dump.variable(name);
            let bb_value = bb.and_then(|v| v.value.clone());
            let static_value = st.as_ref().map(|v| v.text.clone());
            let agreement = match (&st, &bb_value) {
                (None, None)
                    if bb.is_some_and(|v| v.export == Export::Unexported || v.error.is_some()) =>
                {
                    Agreement::NotPrinted
                }
                (None, None) => Agreement::Equal,
                (Some(s), Some(b)) if &s.text == b => Agreement::Equal,
                (Some(_), None)
                    if bb.is_some_and(|v| v.export == Export::Unexported || v.error.is_some()) =>
                {
                    Agreement::NotPrinted
                }
                (Some(s), _) if s.unevaluated_python => Agreement::StaticPython,
                (Some(s), _) if !s.unresolved.is_empty() => Agreement::StaticUnresolved,
                (Some(_), None) => Agreement::OnlyStatic,
                (None, Some(_)) => Agreement::OnlyBitbake,
                (Some(s), Some(b)) => {
                    if s.text.split_whitespace().eq(b.split_whitespace()) {
                        Agreement::Whitespace
                    } else {
                        Agreement::Mismatch
                    }
                }
            };
            Comparison {
                name: name.to_string(),
                static_value,
                bitbake_value: bb_value,
                agreement,
            }
        })
        .collect()
}

/// [`compare`] for a target: evaluates it statically first (the caller has refreshed the
/// index), then compares with `dump`, which must be `bitbake -e` output for the same
/// recipe.
pub fn compare_target(
    index: &Index,
    ws: &Workspace,
    target: &Target,
    dump: &EnvDump,
    names: &[&str],
) -> Result<Vec<Comparison>> {
    let ds = eval::evaluate(index, ws, target)?;
    Ok(compare(&ds, dump, names))
}
