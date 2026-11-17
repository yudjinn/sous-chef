//! T8.1: differential test of the static evaluator against BitBake on the live workspace.
//!
//! Ignored by default. Run it with the live environment (docs/live-environment.md):
//!
//! ```sh
//! SOUS_CHEF_LIVE=~/Development/sous-chef-live scripts/differential.sh       # BitBake side
//! SOUS_CHEF_LIVE=~/Development/sous-chef-live cargo test --release --test differential -- --ignored --nocapture
//! ```
//!
//! The BitBake side is `bitbake -e <recipe>` in the bitbake-setup build (parse only), run by
//! `scripts/differential.sh`; its raw dumps are in `$SOUS_CHEF_LIVE/runs/differential/env/`.
//! This test runs the script itself for any dump that is missing. The static side is
//! `eval::evaluate` on the workspace discovered from the same build directory, with the
//! index in `$SOUS_CHEF_LIVE/runs/differential/cache` (never in the user's cache).
//!
//! Each compared value is put in exactly one class (docs/differential.md): equal, equal
//! after normalising whitespace, flagged (the static value carries `unevaluated_python` or
//! `unresolved`), known gap (a documented limitation, named), or bug. The report goes to
//! `$SOUS_CHEF_LIVE/runs/differential/report.txt`, and a small summary per recipe to
//! `tests/fixtures/outputs/live/differential/` (with the live prefix replaced by
//! `{{LIVE}}`).
//!
//! The dump reader here is deliberately minimal (final `VAR="value"` / `export VAR="value"`
//! lines and the `# $VAR` history comments; functions skipped). It should be replaced by
//! the `bitbake -e` parser of `src/bitbake/envdump.rs` (T5.5) and its compare helper once
//! that is merged.
//!
//! What fails the test: only what the milestone says it must fail on, a **bug** on a tracked
//! variable (`tracked`) in the sample evaluated with the build's machine and distro. The
//! expected classes (flagged, known gap) never fail it, and the `@default` probe (machine
//! and distro left to the configuration) is reported, not asserted.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sous_chef::config::Config;
use sous_chef::eval::providers::Providers;
use sous_chef::eval::{self, DataStore, OpStatus, Target, Value};
use sous_chef::index::Index;
use sous_chef::types::FileKind;
use sous_chef::workspace::Workspace;

/// Keep in sync with `RECIPES` in `scripts/differential.sh`.
const RECIPES: &[&str] = &[
    "curl",
    "sl",
    "linux-kiss",
    "u-boot-kiss",
    "kiss-image",
    "packagegroup-core-boot",
    "zlib",
    "openssl",
    "busybox",
    "dropbear",
    "ncurses",
    "dbus",
    "systemd-boot",
    "util-linux",
    "gmp-native",
    "nativesdk-zlib",
    "quilt-native",
    "expat",
    "glib-2.0",
    "base-files",
    "python3",
    "bash",
    "opencsd",
    "virtual/kernel",
    "virtual/bootloader",
];

/// Compared for every recipe (`${PN}` is expanded on each side).
const VARS: &[&str] = &[
    "PN",
    "PV",
    "PR",
    "FILE",
    "FILE_LAYERNAME",
    "SRC_URI",
    "DEPENDS",
    "RDEPENDS:${PN}",
    "PACKAGECONFIG",
    "PACKAGES",
    "FILES:${PN}",
    "OVERRIDES",
    "MACHINEOVERRIDES",
    "DISTROOVERRIDES",
    "S",
    "B",
    "WORKDIR",
    "LICENSE",
    "INHERIT",
    "PROVIDES",
    "EXTRA_OECONF",
    "EXTRA_OECMAKE",
    "CFLAGS",
    "FILESPATH",
    "MACHINE",
    "DISTRO",
    "TMPDIR",
];

/// The milestone's tracked variables: a bug on one of these fails the test in strict mode.
fn tracked(name: &str) -> bool {
    const T: &[&str] = &[
        "SRC_URI",
        "DEPENDS",
        "PACKAGECONFIG",
        "EXTRA_OECONF",
        "EXTRA_OECMAKE",
        "S",
        "B",
        "PV",
        "LICENSE",
        "OVERRIDES",
        "FILESPATH",
    ];
    T.contains(&name)
        || name.starts_with("RDEPENDS:")
        || name.starts_with("FILES:")
        || name.starts_with("PREFERRED_PROVIDER_")
}

// ---------------------------------------------------------------------------------------
// Minimal `bitbake -e` reader (to be replaced by src/bitbake/envdump.rs)
// ---------------------------------------------------------------------------------------

/// One operation from the `# $VAR [n operations]` history comment.
#[derive(Debug, Clone)]
struct DumpOp {
    /// `set`, `:append[class-target]`, `append`, `set?`, ...
    op: String,
    /// As printed: an absolute path for a statement, a basename for python.
    file: String,
    line: Option<u32>,
    /// `[func]`: the python function that made the change (`__anon_...`, a handler, or a
    /// BitBake internal such as `handleVirtRecipeProviders`).
    func: Option<String>,
    /// The value line starts with `[flag]`: a flag operation, not the variable's value.
    flag: bool,
}

#[derive(Debug, Clone, Default)]
struct DumpVar {
    value: String,
    ops: Vec<DumpOp>,
    pre: Option<String>,
}

#[derive(Debug, Default)]
struct Dump {
    vars: HashMap<String, DumpVar>,
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.peek() {
                Some('"') | Some('$') => {
                    out.push(it.next().unwrap());
                    continue;
                }
                Some('\n') => {
                    // ` \` + newline is how emit_var prints a newline in the value.
                    it.next();
                    if out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\n');
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

/// True when `s` (the text after the opening quote) ends with an unescaped `"`.
fn closed(s: &str) -> bool {
    if !s.ends_with('"') {
        return false;
    }
    let body = &s[..s.len() - 1];
    let bs = body.chars().rev().take_while(|&c| c == '\\').count();
    bs % 2 == 0
}

fn parse_history_op(line: &str) -> Option<DumpOp> {
    // "#   <op> <file>:<line>[ [func]]" (the value follows on the next line, indented more).
    let rest = line.strip_prefix("#   ")?;
    if rest.starts_with(' ') {
        return None;
    }
    let (op, rest) = rest.split_once(' ')?;
    let (loc, func) = match rest.rsplit_once(" [") {
        Some((l, f)) if f.ends_with(']') => (l, Some(f.trim_end_matches(']').to_string())),
        _ => (rest, None),
    };
    let (file, line) = match loc.rsplit_once(':') {
        Some((f, l)) => (f.to_string(), l.parse().ok()),
        None => (loc.to_string(), None),
    };
    Some(DumpOp {
        op: op.to_string(),
        file,
        line,
        func,
        flag: false,
    })
}

fn parse_dump(text: &str) -> Dump {
    let mut dump = Dump::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut hist: Vec<DumpOp> = Vec::new();
    let mut pre: Option<String> = None;
    let mut in_pre = false;
    while i < lines.len() {
        let l = lines[i];
        i += 1;
        if let Some(c) = l.strip_prefix('#') {
            if c.starts_with(" $") {
                hist.clear();
                pre = None;
                in_pre = false;
            } else if c == " pre-expansion value:" {
                in_pre = true;
            } else if in_pre {
                let v = c.trim_start();
                pre = Some(
                    v.strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(v)
                        .to_string(),
                );
                in_pre = false;
            } else if let Some(op) = parse_history_op(l) {
                hist.push(op);
            } else if let Some(v) = l.strip_prefix("#     ")
                && v.starts_with('[')
                // `??=` is stored as the `_defaultval` flag but is an operation on the value.
                && !v.starts_with("[_defaultval]")
                && let Some(last) = hist.last_mut()
            {
                last.flag = true;
            }
            continue;
        }
        // Functions: `name() {` / `python name () {` ... `}`.
        if l.ends_with('{') && l.contains("()") {
            while i < lines.len() && lines[i] != "}" {
                i += 1;
            }
            i += 1;
            hist.clear();
            pre = None;
            continue;
        }
        let body = l.strip_prefix("export ").unwrap_or(l);
        let Some(eq) = body.find("=\"") else {
            continue;
        };
        let name = &body[..eq];
        if name.is_empty() || name.contains(' ') {
            continue;
        }
        let mut val = body[eq + 2..].to_string();
        while !closed(&val) && i < lines.len() {
            val.push('\n');
            val.push_str(lines[i]);
            i += 1;
        }
        val.pop(); // closing quote
        dump.vars.insert(
            name.to_string(),
            DumpVar {
                value: unescape(&val),
                ops: std::mem::take(&mut hist),
                pre: pre.take(),
            },
        );
    }
    dump
}

// ---------------------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// Unset on both sides: not counted as a compared value.
    Unset,
    Equal,
    EqualWs,
    /// `true` when the literal text around the unevaluated parts matches BitBake's value.
    Flagged(bool),
    Gap(&'static str),
    Bug,
}

impl Class {
    fn label(&self) -> String {
        match self {
            Class::Unset => "unset".into(),
            Class::Equal => "equal".into(),
            Class::EqualWs => "equal-ws".into(),
            Class::Flagged(true) => "flagged".into(),
            Class::Flagged(false) => "flagged-literal-differs".into(),
            Class::Gap(g) => format!("gap:{g}"),
            Class::Bug => "bug".into(),
        }
    }
}

/// The documented gaps (docs/orchestration.md, "Open items", waves 2 to 4;
/// docs/bitbake-semantics.md, "What the static evaluator cannot do").
const GAP_PYTHON_RUN: &str = "python-at-parse"; // anonymous python, handlers, d.setVar in classes
const GAP_LAYERDIR: &str = "layerdir-canonical"; // LAYERDIR is the canonical root, not BBLAYERS text
const GAP_BBCLASSEXTEND: &str = "bbclassextend"; // variants not resolved

/// Not documented anywhere in docs/: a bug when it changes an unflagged value.
const VIRT_HINT: &str =
    "BB_RECIPE_VIRTUAL_PROVIDERS rewrite of DEPENDS (ast.handleVirtRecipeProviders) not modelled";

struct Ctx<'a> {
    bb: &'a Dump,
    ds: &'a DataStore,
    /// symlinked layer path prefix -> canonical, for `GAP_LAYERDIR`.
    path_map: &'a [(String, String)],
    memo: HashMap<String, (Class, String)>,
    stack: HashSet<String>,
}

fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn flagged(v: &Value) -> bool {
    v.unevaluated_python || !v.unresolved.is_empty()
}

/// Does `bb` match `st` when every `${@...}` and every `${VAR}` left in `st` may stand for
/// any text? Whitespace runs match any whitespace (python often yields empty strings).
fn literal_parts_match(st: &str, bb: &str) -> bool {
    let mut pat = String::from("(?s)^");
    let b = st.as_bytes();
    let mut i = 0;
    let mut lit = String::new();
    let flush = |lit: &mut String, pat: &mut String| {
        for (k, w) in lit.split_whitespace().enumerate() {
            if k > 0 {
                pat.push_str(r"\s+");
            }
            pat.push_str(&regex::escape(w));
        }
        lit.clear();
    };
    while i < b.len() {
        if b[i] == b'$' && b.get(i + 1) == Some(&b'{') {
            // Skip to the matching brace.
            let mut depth = 0usize;
            let mut j = i + 1;
            while j < b.len() {
                match b[j] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            flush(&mut lit, &mut pat);
            pat.push_str(r"\s*(?s:.*?)\s*");
            i = j + 1;
            continue;
        }
        let ch = st[i..].chars().next().unwrap();
        if ch.is_whitespace() {
            flush(&mut lit, &mut pat);
            pat.push_str(r"\s*");
        } else {
            lit.push(ch);
        }
        i += ch.len_utf8();
    }
    flush(&mut lit, &mut pat);
    pat.push_str(r"\s*$");
    regex::Regex::new(&pat)
        .map(|r| r.is_match(bb.trim()))
        .unwrap_or(false)
}

/// Variable names referenced as `${NAME}` in `text` (python references skipped).
fn refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'$'
            && b[i + 1] == b'{'
            && b.get(i + 2) != Some(&b'@')
            && let Some(end) = text[i + 2..].find('}')
        {
            let n = &text[i + 2..i + 2 + end];
            if !n.is_empty() && !n.contains('{') && !n.contains('$') {
                out.push(n.to_string());
            }
            i += 2 + end;
            continue;
        }
        i += 1;
    }
    out
}

impl Ctx<'_> {
    fn canon(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (from, to) in self.path_map {
            s = s.replace(from.as_str(), to.as_str());
        }
        s
    }

    /// Class of the variable `bb_name` (BitBake's key) / `st_name` (the static name) with
    /// the cause, following references to the root difference.
    fn classify(&mut self, bb_name: &str, st_name: &str) -> (Class, String) {
        if let Some(c) = self.memo.get(bb_name) {
            return c.clone();
        }
        if !self.stack.insert(bb_name.to_string()) {
            return (Class::Bug, "reference cycle while classifying".into());
        }
        let c = self.classify_inner(bb_name, st_name);
        self.stack.remove(bb_name);
        self.memo.insert(bb_name.to_string(), c.clone());
        c
    }

    fn classify_inner(&mut self, bb_name: &str, st_name: &str) -> (Class, String) {
        let bv = self.bb.vars.get(bb_name);
        let sv = self.ds.value(st_name);
        let (b, s) = match (bv, &sv) {
            (None, None) => return (Class::Unset, String::new()),
            (Some(b), Some(s)) => (b.value.as_str(), s.text.as_str()),
            (Some(b), None) => (b.value.as_str(), ""),
            (None, Some(s)) => ("", s.text.as_str()),
        };
        if bv.is_some() && sv.is_some() && b == s {
            return (Class::Equal, String::new());
        }
        if norm_ws(b) == norm_ws(s) {
            return (Class::EqualWs, String::new());
        }
        if let Some(sv) = &sv
            && flagged(sv)
        {
            let s = sv.text.as_str();
            let why = if sv.unevaluated_python {
                "unevaluated_python".to_string()
            } else {
                format!("unresolved: {}", sv.unresolved.join(" "))
            };
            let ok = literal_parts_match(s, b);
            let mut why = why;
            if !ok {
                if self.virt_rewrite(bb_name, s) {
                    why.push_str("; also: ");
                    why.push_str(VIRT_HINT);
                }
                let py = self.python_ops(bb_name);
                if !py.is_empty() {
                    why.push_str("; bitbake python ops: ");
                    why.push_str(&py.join("; "));
                }
            }
            return (Class::Flagged(ok), why);
        }
        if norm_ws(&self.canon(b)) == norm_ws(&self.canon(s)) {
            return (
                Class::Gap(GAP_LAYERDIR),
                "same after mapping layers/ symlinks to canonical roots".into(),
            );
        }
        // Follow references: the first referenced variable that differs decides.
        let mut names: Vec<String> = Vec::new();
        if let Some(pre) = bv.and_then(|v| v.pre.as_deref()) {
            names.extend(refs(pre));
        }
        if let Some(raw) = self.ds.raw(st_name) {
            names.extend(refs(&raw));
        }
        let mut seen = HashSet::new();
        for n in names {
            if !seen.insert(n.clone()) || n == bb_name {
                continue;
            }
            let (c, why) = self.classify(&n, &n);
            if !matches!(c, Class::Equal | Class::EqualWs | Class::Unset) {
                let why = if why.starts_with("via ") {
                    format!("via ${{{n}}} {why}")
                } else {
                    format!("via ${{{n}}}: {why}")
                };
                return (c, why);
            }
        }
        if self.virt_rewrite(bb_name, s) {
            return (Class::Bug, VIRT_HINT.into());
        }
        let py = self.python_ops(bb_name);
        if !py.is_empty() {
            return (Class::Gap(GAP_PYTHON_RUN), py.join("; "));
        }
        (Class::Bug, self.divergence(bb_name, st_name))
    }

    /// Operations BitBake made from python at parse time (anonymous python, handlers,
    /// `d.setVar` in classes and BitBake internals), as `op file:line [func]`.
    fn python_ops(&self, bb_name: &str) -> Vec<String> {
        let Some(b) = self.bb.vars.get(bb_name) else {
            return Vec::new();
        };
        b.ops
            .iter()
            .filter_map(|o| {
                o.func.as_ref().map(|f| {
                    // `__anon_<line>__<mangled absolute path>`: keep the line only (the
                    // path is the file printed before it, and holds the user's home).
                    let f = match f.strip_prefix("__anon_") {
                        Some(rest) => format!("__anon_{}", rest.split("__").next().unwrap_or("")),
                        None => f.clone(),
                    };
                    format!("{} {}:{} [{f}]", o.op, o.file, o.line.unwrap_or(0))
                })
            })
            .filter(|s| !s.contains("[expandKeys]") && !s.contains("[handleVirtRecipeProviders]"))
            .collect()
    }

    /// BitBake 2.18 replaces each `DEPENDS` word listed in `BB_RECIPE_VIRTUAL_PROVIDERS`
    /// with its `PREFERRED_PROVIDER_` (`ast.handleVirtRecipeProviders`, run in `finalize`);
    /// true when the static value still has such a word.
    fn virt_rewrite(&self, bb_name: &str, static_text: &str) -> bool {
        if bb_name != "DEPENDS" {
            return false;
        }
        let virt: Vec<&str> = self
            .bb
            .vars
            .get("BB_RECIPE_VIRTUAL_PROVIDERS")
            .map(|v| v.value.split_whitespace().collect())
            .unwrap_or_default();
        static_text.split_whitespace().any(|w| virt.contains(&w))
    }

    /// Where the two sides part: BitBake's last operation and the static side's last
    /// applied one.
    fn divergence(&self, bb_name: &str, st_name: &str) -> String {
        let b = self
            .bb
            .vars
            .get(bb_name)
            .and_then(|v| v.ops.last())
            .map(|o| format!("{} {}:{}", o.op, self.canon(&o.file), o.line.unwrap_or(0)))
            .unwrap_or_else(|| "unset".into());
        let base = st_name.split(':').next().unwrap_or(st_name);
        let s = self
            .ds
            .history(base)
            .into_iter()
            .rfind(|(_, st)| *st == OpStatus::Applied)
            .map(|(op, _)| format!("{} {:?}", op.loc, op.op))
            .unwrap_or_else(|| "no applied operation".into());
        format!("bitbake last op: {b}; static last applied: {s}")
    }
}

// ---------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------

struct Row {
    var: String,
    class: Class,
    why: String,
    bb: Option<String>,
    st: Option<Value>,
    /// The files in the variable's history (statements only, flag operations left out):
    /// those only in BitBake's, those only in the static one.
    files: Option<(Vec<String>, Vec<String>)>,
}

/// Files of the statements in `name`'s history on each side, canonical absolute paths.
/// BitBake's python operations (`[func]`) and flag operations are left out; the static
/// side's operations of every status are included (BitBake lists overridden ones too).
fn history_files(
    bb: &Dump,
    ds: &DataStore,
    ws: &Workspace,
    pmap: &[(String, String)],
    name: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let v = bb.vars.get(name)?;
    let canon = |s: &str| {
        let mut s = s.to_string();
        for (a, b) in pmap {
            s = s.replace(a.as_str(), b.as_str());
        }
        s
    };
    let b: std::collections::BTreeSet<String> = v
        .ops
        .iter()
        .filter(|o| o.func.is_none() && !o.flag && o.file.starts_with('/'))
        .map(|o| canon(&o.file))
        .collect();
    let s: std::collections::BTreeSet<String> = ds
        .history(name)
        .iter()
        .filter(|(op, _)| op.var.flag.is_none())
        .filter_map(|(op, _)| {
            ws.layers
                .iter()
                .find(|l| l.name == op.loc.layer)
                .map(|l| l.root.join(&op.loc.path))
                .or_else(|| {
                    (op.loc.layer == sous_chef::workspace::BUILD_LAYER)
                        .then(|| ws.build_dir.as_ref().map(|d| d.join(&op.loc.path)))
                        .flatten()
                })
                .map(|p| p.display().to_string())
        })
        .collect();
    Some((
        b.difference(&s).cloned().collect(),
        s.difference(&b).cloned().collect(),
    ))
}

struct RecipeResult {
    recipe: String,
    note: String,
    rows: Vec<Row>,
    /// Unfollowed references and anonymous python, for the report.
    details: Vec<String>,
}

fn live() -> Option<PathBuf> {
    std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from)
}

fn build_dir(live: &Path) -> PathBuf {
    std::env::var_os("SOUS_CHEF_BUILD")
        .map(PathBuf::from)
        .unwrap_or_else(|| live.join("bitbake-builds/kiss-wrynose-dogbonedark/build"))
}

fn dump_path(live: &Path, recipe: &str) -> PathBuf {
    live.join("runs/differential/env")
        .join(format!("{}.env", recipe.replace('/', "_")))
}

fn ensure_dumps(live: &Path) {
    let missing: Vec<&str> = RECIPES
        .iter()
        .copied()
        .filter(|r| {
            fs::metadata(dump_path(live, r))
                .map(|m| m.len() == 0)
                .unwrap_or(true)
        })
        .collect();
    let ws_missing = !live.join("runs/differential/show-layers.out").exists();
    if missing.is_empty() && !ws_missing {
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/differential.sh");
    let mut cmd = Command::new(script);
    cmd.env("SOUS_CHEF_LIVE", live);
    if !ws_missing {
        cmd.args(&missing);
    }
    let st = cmd.status().expect("running scripts/differential.sh");
    assert!(st.success(), "scripts/differential.sh failed");
}

fn workspace(live: &Path) -> (Workspace, Index) {
    let b = build_dir(live);
    let mut cfg = Config {
        build_dir: Some(b.clone()),
        work_dir: b,
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    let ws = Workspace::discover(&cfg).expect("discovering the live workspace");
    let mut idx = Index::open_in(&live.join("runs/differential/cache"), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    (ws, idx)
}

/// `layers/<x>` symlink prefixes of the bitbake-setup build and their targets.
fn path_map(build: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let layers = build.parent().unwrap().join("layers");
    if let Ok(rd) = fs::read_dir(&layers) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_symlink()
                && let Ok(c) = fs::canonicalize(&p)
            {
                out.push((format!("{}/", p.display()), format!("{}/", c.display())));
            }
        }
    }
    // Longest first, so `layers/meta-kiss/` does not shadow a longer name.
    out.sort_by_key(|(a, _)| std::cmp::Reverse(a.len()));
    out
}

const MACHINE: &str = "dogbonedark";
const DISTRO: &str = "kiss";

fn target(recipe: &str, explicit: bool) -> Target {
    Target {
        recipe: Some(recipe.to_string()),
        version: None,
        machine: explicit.then(|| MACHINE.into()),
        distro: explicit.then(|| DISTRO.into()),
    }
}

/// Variables the recipe's own files (the `.bb`, its `.inc` files and bbappends, not
/// classes or configuration) assign, as names to compare: the base, plus a package
/// override that names `${PN}` (`FILES:${PN}-dev`); operations and other overrides are
/// dropped (their effect is in the base's final value).
fn own_vars(ds: &DataStore) -> Vec<String> {
    let own: HashSet<(String, PathBuf)> = ds
        .files()
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                FileKind::Recipe | FileKind::Append | FileKind::Include
            )
        })
        .filter(|f| {
            !f.via.iter().any(|v| {
                v.path
                    .extension()
                    .is_some_and(|e| e == "bbclass" || e == "conf")
            })
        })
        .map(|f| (f.loc.layer.clone(), f.loc.path.clone()))
        .collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for name in ds.names() {
        if ds.is_function(&name) {
            continue;
        }
        for (op, _) in ds.history(&name) {
            if op.var.flag.is_some() || !own.contains(&(op.loc.layer.clone(), op.loc.path.clone()))
            {
                continue;
            }
            let mut key = op.var.base.clone();
            for o in &op.var.overrides {
                if matches!(o.as_str(), "append" | "prepend" | "remove") {
                    break;
                }
                if o.contains("${PN}") {
                    key.push(':');
                    key.push_str(o);
                } else {
                    break;
                }
            }
            // Functions are stored as pointers on the static side; they are not compared.
            let expanded = ds.expand(&key).text;
            if ds.is_function(&key)
                || ds.is_function(&expanded)
                || ds.value(&key).is_some_and(|v| is_pointer_list(&v.text))
            {
                continue;
            }
            if seen.insert(key.clone()) {
                out.push(key);
            }
        }
    }
    out
}

/// A function's static value: one `layer:path:A-B` pointer per line.
fn is_pointer_list(text: &str) -> bool {
    let re = regex::Regex::new(r"^[\w.+-]+:[^\s:]+:\d+(-\d+)?( -> \S+)?$").unwrap();
    !text.trim().is_empty()
        && text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| re.is_match(l.trim()))
}

fn bb_key(name: &str, bb: &Dump) -> String {
    let pn = bb.vars.get("PN").map(|v| v.value.as_str()).unwrap_or("");
    name.replace("${PN}", pn)
}

fn short(s: &str) -> String {
    let s = s.replace('\n', "\\n");
    if s.chars().count() > 240 {
        format!("{}...", s.chars().take(240).collect::<String>())
    } else {
        s
    }
}

fn run_recipe(
    live: &Path,
    ws: &Workspace,
    idx: &Index,
    pmap: &[(String, String)],
    recipe: &str,
    explicit: bool,
) -> RecipeResult {
    let label = if explicit {
        recipe.to_string()
    } else {
        format!("{recipe}@default")
    };
    let text = fs::read_to_string(dump_path(live, recipe)).unwrap_or_default();
    let bb = parse_dump(&text);
    let mut note = String::new();
    if bb.vars.is_empty() {
        let err = fs::read_to_string(format!("{}.err", dump_path(live, recipe).display()))
            .unwrap_or_default();
        return RecipeResult {
            recipe: label,
            note: format!("bitbake -e failed: {}", short(err.trim())),
            rows: Vec::new(),
            details: Vec::new(),
        };
    }
    // The static side: a virtual/ name through provider selection, else the name.
    let mut t = target(recipe, explicit);
    if recipe.starts_with("virtual/") {
        let p = Providers::new(idx, ws, t.machine.as_deref(), t.distro.as_deref()).unwrap();
        let set = p.providers(recipe).unwrap();
        match set.selected() {
            Some(sel) => {
                let f = &sel.selection.recipe.file;
                let _ = write!(note, "provider {} ({:?}); ", sel.pn, set.reason);
                t.recipe = Some(format!("{}:{}", f.layer, f.path.display()));
            }
            None => {
                return RecipeResult {
                    recipe: label,
                    note: format!("no static provider ({:?})", set.reason),
                    rows: Vec::new(),
                    details: Vec::new(),
                };
            }
        }
    }
    let ds = match eval::evaluate(idx, ws, &t) {
        Ok(ds) => ds,
        Err(e) => {
            let gap = if recipe.contains("-native") || recipe.starts_with("nativesdk-") {
                format!(" [{GAP_BBCLASSEXTEND}]")
            } else {
                String::new()
            };
            return RecipeResult {
                recipe: label,
                note: format!("{note}static evaluation failed{gap}: {e}"),
                rows: Vec::new(),
                details: Vec::new(),
            };
        }
    };
    let n = ds.replay();
    let _ = write!(
        note,
        "anonymous python {} ({} opaque); unfollowed references {}",
        n.anonymous.len(),
        n.anonymous.iter().filter(|a| a.opaque).count(),
        n.references.len()
    );

    let mut names: Vec<String> = VARS.iter().map(|s| s.to_string()).collect();
    for v in own_vars(&ds) {
        if !names.contains(&v) {
            names.push(v);
        }
    }
    let mut cx = Ctx {
        bb: &bb,
        ds: &ds,
        path_map: pmap,
        memo: HashMap::new(),
        stack: HashSet::new(),
    };
    let mut rows = Vec::new();
    for name in names {
        let key = bb_key(&name, &bb);
        let (class, why) = cx.classify(&key, &name);
        let files = (!key.contains(':'))
            .then(|| history_files(&bb, &ds, ws, pmap, &key))
            .flatten();
        rows.push(Row {
            var: key.clone(),
            class,
            why,
            bb: bb.vars.get(&key).map(|v| v.value.clone()),
            st: ds.value(&name),
            files,
        });
    }
    let mut details: Vec<String> = n
        .references
        .iter()
        .map(|r| {
            format!(
                "reference {} {:?} at {}: {:?}",
                r.kind,
                r.text,
                r.loc
                    .as_ref()
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "-".into()),
                r.problem
            )
        })
        .collect();
    details.extend(n.anonymous.iter().map(|a| {
        format!(
            "anonymous python {} opaque={} sets={:?}",
            a.loc, a.opaque, a.sets
        )
    }));
    RecipeResult {
        recipe: label,
        note,
        rows,
        details,
    }
}

/// Layers, priorities and bbappend order against `bitbake-layers`.
fn workspace_checks(live: &Path, ws: &Workspace, idx: &Index, pmap: &[(String, String)]) -> String {
    let mut out = String::new();
    let canon = |s: &str| -> String {
        let mut s = s.to_string();
        for (a, b) in pmap {
            s = s.replace(a.as_str(), b.as_str());
        }
        fs::canonicalize(&s)
            .map(|p| p.display().to_string())
            .unwrap_or(s)
    };
    // show-layers: "name  path  priority" after the "====" line.
    let text =
        fs::read_to_string(live.join("runs/differential/show-layers.out")).unwrap_or_default();
    let mut bb_layers = Vec::new();
    let mut started = false;
    for l in text.lines() {
        if l.starts_with("====") {
            started = true;
            continue;
        }
        if !started {
            continue;
        }
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() == 3 {
            bb_layers.push((
                w[0].to_string(),
                canon(w[1]),
                w[2].parse::<i32>().unwrap_or(-1),
            ));
        }
    }
    let _ = writeln!(
        out,
        "## Layers (bitbake-layers show-layers vs Workspace::discover)"
    );
    let st: Vec<(String, String, i32)> = ws
        .layers
        .iter()
        .map(|l| {
            (
                l.collections.first().cloned().unwrap_or_default(),
                l.root.display().to_string(),
                l.priority,
            )
        })
        .collect();
    let _ = writeln!(
        out,
        "bitbake: {} layers, static: {} layers",
        bb_layers.len(),
        st.len()
    );
    for (i, b) in bb_layers.iter().enumerate() {
        let s = st.get(i);
        let same = s.is_some_and(|s| s.0 == b.0 && s.1 == b.1 && s.2 == b.2);
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t| static: {:?}",
            if same { "same" } else { "DIFF" },
            b.0,
            b.1,
            b.2,
            s
        );
    }
    // show-appends: "<recipe>.bb[ (skipped)]:" then indented bbappend paths.
    let text =
        fs::read_to_string(live.join("runs/differential/show-appends.out")).unwrap_or_default();
    let _ = writeln!(
        out,
        "\n## bbappend order (bitbake-layers show-appends vs query::appends_for)"
    );
    let mut cur: Option<String> = None;
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut repeated: Vec<String> = Vec::new();
    for l in text.lines() {
        if let Some(p) = l.strip_prefix("  ") {
            if let Some(c) = &cur {
                groups.entry(c.clone()).or_default().push(canon(p.trim()));
            }
        } else if let Some(h) = l.strip_suffix(':')
            && h.contains(".bb")
        {
            // show-appends lists some recipes twice (with the same appends); keep the first.
            let h = h.trim_end_matches(" (skipped)").to_string();
            if groups.contains_key(&h) {
                repeated.push(h);
                cur = None;
            } else {
                cur = Some(h);
            }
        } else {
            cur = None;
        }
    }
    let _ = writeln!(
        out,
        "listed twice by show-appends (second listing skipped): {repeated:?}"
    );
    for (bbname, apps) in &groups {
        // Find the recipe file by name in the index.
        let rows = sous_chef::index::query::all_files(idx).unwrap();
        let rec = rows.iter().find(|f| {
            f.kind == FileKind::Recipe && f.path.file_name().is_some_and(|n| n == bbname.as_str())
        });
        let Some(rec) = rec else {
            let _ = writeln!(out, "DIFF\t{bbname}: recipe not found in the index");
            continue;
        };
        let st: Vec<String> = sous_chef::index::query::appends_for(idx, rec.id)
            .unwrap()
            .iter()
            .map(|f| {
                let root = ws
                    .layers
                    .iter()
                    .find(|l| l.name == f.layer)
                    .map(|l| l.root.clone());
                root.map(|r| r.join(&f.path).display().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let same = &st == apps;
        let _ = writeln!(
            out,
            "{}\t{bbname}\t{} bbappend(s){}",
            if same { "same" } else { "DIFF" },
            apps.len(),
            if same {
                String::new()
            } else {
                format!("\n\tbitbake: {apps:?}\n\tstatic:  {st:?}")
            }
        );
    }

    // Recipe files: the index's `.bb` rows against the files BBFILES matches.
    let _ = writeln!(
        out,
        "\n## Recipe files (index vs workspace::resolve::recipe_files)"
    );
    let bbfiles: HashSet<PathBuf> = sous_chef::workspace::resolve::recipe_files(ws)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "bb"))
        .collect();
    let indexed: Vec<PathBuf> = sous_chef::index::query::all_files(idx)
        .unwrap()
        .into_iter()
        .filter(|f| f.kind == FileKind::Recipe)
        .filter_map(|f| {
            ws.layers
                .iter()
                .find(|l| l.name == f.layer)
                .map(|l| l.root.join(&f.path))
        })
        .collect();
    let _ = writeln!(
        out,
        "indexed .bb: {}; matched by BBFILES: {}",
        indexed.len(),
        bbfiles.len()
    );
    for p in &indexed {
        if !bbfiles.contains(p) {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let pn = name.split('_').next().unwrap_or(&name).to_string();
            let ev = eval::evaluate(idx, ws, &target(&pn, true));
            let _ = writeln!(
                out,
                "indexed but not matched by BBFILES: {} (evaluate(\"{pn}\"): {})",
                p.display(),
                match ev {
                    Ok(ds) => format!(
                        "succeeds, FILE = {:?}",
                        ds.value("FILE").map(|v| v.text).unwrap_or_default()
                    ),
                    Err(e) => format!("error: {e}"),
                }
            );
        }
    }
    out
}

fn explicit_run(label: &str) -> bool {
    !label.ends_with("@default")
}

#[test]
#[ignore = "needs the live environment: set SOUS_CHEF_LIVE and run with --ignored"]
fn differential_against_bitbake() {
    let Some(live) = live() else {
        eprintln!("SOUS_CHEF_LIVE is not set; skipping");
        return;
    };
    ensure_dumps(&live);
    let (ws, idx) = workspace(&live);
    let pmap = path_map(&build_dir(&live));

    let mut report = String::new();
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_var: BTreeMap<String, usize> = BTreeMap::new();
    let mut tracked_bugs = Vec::new();
    let mut results = Vec::new();
    for r in RECIPES {
        results.push(run_recipe(&live, &ws, &idx, &pmap, r, true));
    }
    // The same recipes with MACHINE and DISTRO left to the workspace's configuration, as a
    // tool call without `machine` / `distro` arguments evaluates them.
    for r in ["curl", "sl", "virtual/kernel"] {
        results.push(run_recipe(&live, &ws, &idx, &pmap, r, false));
    }
    let _ = writeln!(
        report,
        "# Differential report: static evaluation vs bitbake -e\n"
    );
    let _ = writeln!(
        report,
        "recipe\tcompared\tequal\tequal-ws\tflagged\tgap\tbug\tunset-both\tnote"
    );
    for res in &results {
        let mut c: BTreeMap<&str, usize> = BTreeMap::new();
        for row in &res.rows {
            let k = match row.class {
                Class::Unset => "unset",
                Class::Equal => "equal",
                Class::EqualWs => "equal-ws",
                Class::Flagged(_) => "flagged",
                Class::Gap(_) => "gap",
                Class::Bug => "bug",
            };
            *c.entry(k).or_default() += 1;
            if explicit_run(&res.recipe) {
                *totals.entry(row.class.label()).or_default() += 1;
                // A correct value that still carries a warning (over-caution, not an error).
                if matches!(row.class, Class::Equal | Class::EqualWs)
                    && row.st.as_ref().is_some_and(flagged)
                {
                    *totals.entry("(equal, but flagged)".into()).or_default() += 1;
                }
            }
            if explicit_run(&res.recipe)
                && !matches!(row.class, Class::Equal | Class::EqualWs | Class::Unset)
            {
                *by_var
                    .entry(row.var.split(':').next().unwrap().to_string())
                    .or_default() += 1;
            }
            if explicit_run(&res.recipe) && row.class == Class::Bug && tracked(&row.var) {
                tracked_bugs.push(format!("{} {}", res.recipe, row.var));
            }
        }
        let g = |k| c.get(k).copied().unwrap_or(0);
        let _ = writeln!(
            report,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            res.recipe,
            res.rows.len() - g("unset"),
            g("equal"),
            g("equal-ws"),
            g("flagged"),
            g("gap"),
            g("bug"),
            g("unset"),
            res.note
        );
    }
    let total: usize = totals
        .iter()
        .filter(|(k, _)| k.as_str() != "unset" && !k.starts_with('('))
        .map(|(_, v)| v)
        .sum();
    let _ = writeln!(
        report,
        "\n## Totals ({total} values compared, explicit machine and distro; `unset` = unset on both sides, not counted)"
    );
    for (k, v) in &totals {
        let _ = writeln!(report, "{k}\t{v}");
    }
    let mut top: Vec<_> = by_var.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    let _ = writeln!(
        report,
        "\n## Variables by number of differences (any class but equal)"
    );
    for (k, v) in top.iter().take(25) {
        let _ = writeln!(report, "{k}\t{v}");
    }
    let _ = writeln!(report, "\n## Differences\n");
    for res in &results {
        for row in &res.rows {
            if matches!(row.class, Class::Equal | Class::EqualWs | Class::Unset) {
                continue;
            }
            let st = row.st.as_ref();
            let _ = writeln!(
                report,
                "{}\t{}\t{}\t{}\n\tbitbake: {}\n\tstatic:  {}{}",
                res.recipe,
                row.var,
                row.class.label(),
                row.why,
                row.bb
                    .as_deref()
                    .map(short)
                    .unwrap_or_else(|| "(unset)".into()),
                st.map(|v| short(&v.text))
                    .unwrap_or_else(|| "(unset)".into()),
                st.map(|v| format!(
                    " [python={} unresolved={:?}]",
                    v.unevaluated_python, v.unresolved
                ))
                .unwrap_or_default()
            );
        }
    }
    // Files in the history.
    let mut hist_total = 0;
    let mut hist_same = 0;
    let mut only_bb: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut only_st: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for res in results.iter().filter(|r| explicit_run(&r.recipe)) {
        for row in &res.rows {
            let Some((b, s)) = &row.files else { continue };
            hist_total += 1;
            if b.is_empty() && s.is_empty() {
                hist_same += 1;
            }
            for f in b {
                only_bb
                    .entry(f.clone())
                    .or_default()
                    .push(format!("{}:{}", res.recipe, row.var));
            }
            for f in s {
                only_st
                    .entry(f.clone())
                    .or_default()
                    .push(format!("{}:{}", res.recipe, row.var));
            }
        }
    }
    let _ = writeln!(
        report,
        "\n## Files in the history ({hist_total} variables without overrides; same set of files: {hist_same})\n"
    );
    let _ = writeln!(
        report,
        "### Files only in BitBake's history (file, count, first uses)"
    );
    for (f, uses) in &only_bb {
        let _ = writeln!(
            report,
            "{f}\t{}\t{}",
            uses.len(),
            uses.iter().take(4).cloned().collect::<Vec<_>>().join(" ")
        );
    }
    let _ = writeln!(
        report,
        "### Files only in the static history (file, count, first uses)"
    );
    for (f, uses) in &only_st {
        let _ = writeln!(
            report,
            "{f}\t{}\t{}",
            uses.len(),
            uses.iter().take(4).cloned().collect::<Vec<_>>().join(" ")
        );
    }

    let _ = writeln!(report, "\n## Unfollowed references and anonymous python\n");
    for res in &results {
        if explicit_run(&res.recipe) {
            for d in &res.details {
                let _ = writeln!(report, "{}\t{d}", res.recipe);
            }
        }
    }
    let _ = writeln!(report, "\n{}", workspace_checks(&live, &ws, &idx, &pmap));

    // Configuration only, with MACHINE/DISTRO left to the workspace (they are set through
    // OE_FRAGMENTS / addfragments in this build).
    let cfg = eval::evaluate(
        &idx,
        &ws,
        &Target {
            recipe: None,
            version: None,
            machine: None,
            distro: None,
        },
    )
    .unwrap();
    let _ = writeln!(
        report,
        "## Configuration without an explicit machine and distro\nMACHINE = {:?}\nDISTRO = {:?}",
        cfg.value("MACHINE"),
        cfg.value("DISTRO")
    );

    let out_dir = live.join("runs/differential");
    fs::write(out_dir.join("report.txt"), &report).unwrap();
    // Recorded summary in the repository: the per-recipe table and totals only.
    let rec_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/outputs/live/differential");
    fs::create_dir_all(&rec_dir).unwrap();
    let live_s = fs::canonicalize(&live).unwrap().display().to_string();
    let redact = |s: &str| {
        s.replace(&live_s, "{{LIVE}}")
            .replace(&live.display().to_string(), "{{LIVE}}")
    };
    let summary_end = report.find("\n## Differences").unwrap_or(report.len());
    fs::write(rec_dir.join("summary.txt"), redact(&report[..summary_end])).unwrap();
    // The workspace-level comparisons and the history-file comparison.
    let hist = report
        .find("\n## Files in the history")
        .unwrap_or(report.len());
    let refs_at = report
        .find("\n## Unfollowed references")
        .unwrap_or(report.len());
    let layers_at = report.find("\n## Layers").unwrap_or(report.len());
    fs::write(
        rec_dir.join("workspace.txt"),
        redact(&format!(
            "{}{}",
            &report[hist..refs_at],
            &report[layers_at..]
        )),
    )
    .unwrap();
    let mut classes = String::from("recipe\tvariable\tclass\tcause\n");
    for res in &results {
        for row in &res.rows {
            let _ = writeln!(
                classes,
                "{}\t{}\t{}\t{}",
                res.recipe,
                row.var,
                row.class.label(),
                short(&row.why)
            );
        }
    }
    fs::write(rec_dir.join("classes.tsv"), redact(&classes)).unwrap();
    println!("{report}");

    // The milestone's failure condition, on the sample evaluated as BitBake was run
    // (explicit machine and distro). The `@default` runs are a probe of the configuration
    // pass and are reported, not asserted (docs/differential.md, bug 1).
    assert!(
        tracked_bugs.is_empty(),
        "bugs on tracked variables: {tracked_bugs:?}"
    );
}

#[test]
fn dump_reader_basics() {
    let text = "#\n# $A [1 operations]\n#   set /x/a.conf:3\n#     \"1\"\n# pre-expansion value:\n#   \"${B} 1\"\nA=\"2 1\"\nexport C=\"q \\\"x\\\" \\$y\"\ndo_x() {\n\techo\n}\nD=\"one \\\ntwo\"\n";
    let d = parse_dump(text);
    assert_eq!(d.vars["A"].value, "2 1");
    assert_eq!(d.vars["A"].pre.as_deref(), Some("${B} 1"));
    assert_eq!(d.vars["A"].ops[0].file, "/x/a.conf");
    assert_eq!(d.vars["C"].value, "q \"x\" $y");
    assert_eq!(d.vars["D"].value, "one\ntwo");
    assert!(!d.vars.contains_key("do_x"));
    assert_eq!(refs("${A} ${@x} ${B_C}"), vec!["A", "B_C"]);
}
