//! `bblayers.conf` and `layer.conf` reading.
//!
//! BitBake behaviour reproduced here (BitBake `lib/bb/cookerdata.py`, `lib/bb/cooker.py`,
//! `lib/bb/parse/ast.py`):
//!
//! - `bblayers.conf` is found by walking up from the working directory looking for
//!   `conf/bblayers.conf`; `TOPDIR` is the directory that contains that `conf/`.
//! - `BBLAYERS` is expanded and split on whitespace. Each entry has a trailing `/` stripped
//!   and must be an existing directory.
//! - Each layer's `conf/layer.conf` is parsed in `BBLAYERS` order into one shared datastore,
//!   with `LAYERDIR` set to the layer path; after each file every `${LAYERDIR}` reference in
//!   the datastore is replaced by that layer's path (`expandVarref('LAYERDIR')`).
//! - `BBFILE_COLLECTIONS` is split on whitespace. `BBFILE_PRIORITY_<c>` must be an integer;
//!   an unset (or `0`) priority is computed as one more than the highest priority among the
//!   collection's `LAYERDEPENDS`, starting from the lowest explicit priority.
//! - `LAYERDEPENDS_<c>` uses the `explode_dep_versions2` format (`a (>= 2) b`): only the
//!   names are kept.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::types::AssignOp;

use super::Layer;

/// How deep [`scan_for_layers`] looks below the start directory when there is no build
/// directory (the start directory itself is depth 0).
pub const SCAN_DEPTH: usize = 4;

/// Walks up from `start` (inclusive) looking for `conf/bblayers.conf`; returns the
/// directory containing `conf/` (BitBake's `TOPDIR`).
pub fn find_build_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("conf").join("bblayers.conf").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// What `bblayers.conf` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BbLayers {
    /// Layer roots, expanded, in `BBLAYERS` order, trailing `/` stripped (not canonicalised).
    pub layers: Vec<PathBuf>,
    /// `BBLAYERS` entries that could not be expanded statically (python `${@...}` or a
    /// variable not assigned in the file), as written after partial expansion.
    pub unresolved: Vec<String>,
    /// Variables as left by `bblayers.conf` (unexpanded values), plus `TOPDIR`. Seed for
    /// reading the layer confs.
    vars: Vars,
}

/// Reads `<build_dir>/conf/bblayers.conf`. `build_dir` becomes `TOPDIR`.
pub fn read_bblayers(build_dir: &Path) -> Result<BbLayers> {
    let path = build_dir.join("conf").join("bblayers.conf");
    let src = read_text(&path)?;
    let mut vars = Vars::default();
    vars.set("TOPDIR", &build_dir.to_string_lossy());
    for a in conf_reader::assignments(&src) {
        vars.apply(&a);
    }
    let mut out = BbLayers::default();
    let raw = vars.get("BBLAYERS").unwrap_or_default();
    let expanded = vars.expand(&raw);
    for word in split_words(&expanded) {
        if word.contains("${") {
            out.unresolved.push(word);
            continue;
        }
        let trimmed = word.trim_end_matches('/');
        let p = if trimmed.is_empty() { "/" } else { trimmed };
        out.layers.push(PathBuf::from(p));
    }
    out.vars = vars;
    Ok(out)
}

/// The per-layer data read from `conf/layer.conf` files. `name` is the directory name
/// (disambiguation is done by the caller) and `writable` is false.
///
/// `roots` must be absolute and existing. `seed` is the datastore left by `bblayers.conf`
/// (or `None` without a build directory).
pub fn read_layer_confs(roots: &[PathBuf], seed: Option<&BbLayers>) -> Result<Vec<Layer>> {
    let mut vars = seed.map(|s| s.vars.clone()).unwrap_or_default();
    let mut layers = Vec::with_capacity(roots.len());
    for root in roots {
        let conf = root.join("conf").join("layer.conf");
        let src = read_text(&conf)?;
        let layerdir = root.to_string_lossy().into_owned();
        let before_coll = words_of(&vars, "BBFILE_COLLECTIONS");
        let before_files = words_of(&vars, "BBFILES");
        vars.set("LAYERDIR", &layerdir);
        for a in conf_reader::assignments(&src) {
            vars.apply(&a);
        }
        vars.replace_ref("LAYERDIR", &layerdir);
        let collections = added_words(&before_coll, &words_of(&vars, "BBFILE_COLLECTIONS"));
        let bbfiles = added_words(&before_files, &words_of(&vars, "BBFILES"));
        layers.push(Layer {
            name: root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| layerdir.clone()),
            root: root.clone(),
            collections,
            priority: 0,
            compat: Vec::new(),
            depends: Vec::new(),
            bbfiles,
            writable: false,
        });
    }

    // Per-collection values, read once every layer.conf has been parsed (as BitBake does
    // in handleCollections).
    let mut explicit: HashMap<String, Option<i32>> = HashMap::new();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for layer in &mut layers {
        for c in &layer.collections {
            let prio = match vars.get(&format!("BBFILE_PRIORITY_{c}")) {
                None => None,
                Some(raw) => {
                    let text = vars.expand(&raw);
                    let n: i32 = text.trim().parse().map_err(|_| Error::Config {
                        msg: format!(
                            "invalid value for BBFILE_PRIORITY_{c} ({text:?}) in {}; it must be an integer",
                            layer.root.join("conf/layer.conf").display()
                        ),
                    })?;
                    Some(n)
                }
            };
            explicit.insert(c.clone(), prio);
            let d = vars
                .get(&format!("LAYERDEPENDS_{c}"))
                .map(|raw| dep_names(&vars.expand(&raw)))
                .unwrap_or_default();
            deps.insert(c.clone(), d);
        }
        if let Some(c) = layer.collections.first() {
            layer.depends = deps.get(c).cloned().unwrap_or_default();
            layer.compat = vars
                .get(&format!("LAYERSERIES_COMPAT_{c}"))
                .map(|raw| split_words(&vars.expand(&raw)))
                .unwrap_or_default();
        }
    }
    let min_prio = explicit
        .values()
        .flatten()
        .copied()
        .filter(|&p| p != 0)
        .min()
        .unwrap_or(0);
    let mut memo: HashMap<String, i32> = HashMap::new();
    for layer in &mut layers {
        if let Some(c) = layer.collections.first() {
            layer.priority = collection_priority(c, &explicit, &deps, min_prio, &mut memo, 0);
        }
    }
    Ok(layers)
}

/// BitBake `calc_layer_priority`: an explicit non-zero priority is used as is; otherwise
/// the highest priority among the dependencies (at least `min_prio`) plus one.
fn collection_priority(
    c: &str,
    explicit: &HashMap<String, Option<i32>>,
    deps: &HashMap<String, Vec<String>>,
    min_prio: i32,
    memo: &mut HashMap<String, i32>,
    depth: usize,
) -> i32 {
    if let Some(&Some(p)) = explicit.get(c)
        && p != 0
    {
        return p;
    }
    if let Some(&p) = memo.get(c) {
        return p;
    }
    let mut max_dep = min_prio;
    if depth < 64 {
        for d in deps.get(c).map(Vec::as_slice).unwrap_or_default() {
            if explicit.contains_key(d) {
                let p = collection_priority(d, explicit, deps, min_prio, memo, depth + 1);
                max_dep = max_dep.max(p);
            }
        }
    }
    let p = max_dep.saturating_add(1);
    memo.insert(c.to_string(), p);
    p
}

/// Every directory at most `depth` levels below `start` (inclusive) that contains
/// `conf/layer.conf`, sorted by path. Hidden directories are skipped and symlinks are not
/// followed.
pub fn scan_for_layers(start: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(start.to_path_buf(), 0usize)];
    while let Some((dir, d)) = stack.pop() {
        if dir.join("conf").join("layer.conf").is_file() {
            out.push(dir.clone());
        }
        if d >= depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            stack.push((entry.path(), d + 1));
        }
    }
    out.sort();
    out
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| Error::Config {
        msg: format!("cannot read {}: {e}", path.display()),
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn words_of(vars: &Vars, name: &str) -> Vec<String> {
    vars.get(name)
        .map(|raw| split_words(&vars.expand(&raw)))
        .unwrap_or_default()
}

/// The words of `after` that the file added to `before`: the suffix when `after` extends
/// `before` (the usual `+=`), otherwise the words not already present.
fn added_words(before: &[String], after: &[String]) -> Vec<String> {
    if after.starts_with(before) {
        return after[before.len()..].to_vec();
    }
    after
        .iter()
        .filter(|w| !before.contains(w))
        .cloned()
        .collect()
}

/// Names from an `explode_dep_versions2` string: `a (>= 2) b` -> `[a, b]`.
fn dep_names(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Splits on whitespace, keeping a `${...}` (which may contain spaces) inside one word.
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            depth += 1;
            cur.push(ch);
            if let Some(c) = chars.next() {
                cur.push(c);
            }
            continue;
        }
        if depth > 0 {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            cur.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A tiny datastore: unexpanded values plus `??=` weak defaults (`_defaultval`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Vars {
    content: HashMap<String, String>,
    weak: HashMap<String, String>,
}

impl Vars {
    fn set(&mut self, name: &str, value: &str) {
        self.content.insert(name.to_string(), value.to_string());
    }

    /// `getVar(name, expand=False)`: the value, else the weak default.
    fn get(&self, name: &str) -> Option<String> {
        self.content
            .get(name)
            .or_else(|| self.weak.get(name))
            .cloned()
    }

    /// One assignment, with the operator semantics of `bb/parse/ast.py`: the current value
    /// used by `?=`, `+=` and friends ignores the weak default (`noweakdefault=True`).
    fn apply(&mut self, a: &conf_reader::Assign) {
        let cur = self.content.get(&a.name).cloned();
        let new = match a.op {
            AssignOp::WeakDefault => {
                self.weak.insert(a.name.clone(), a.value.clone());
                return;
            }
            AssignOp::Set => a.value.clone(),
            AssignOp::Default => match cur {
                Some(_) => return,
                None => a.value.clone(),
            },
            AssignOp::Immediate => self.expand(&a.value),
            AssignOp::Append => format!("{} {}", cur.unwrap_or_default(), a.value),
            AssignOp::Prepend => format!("{} {}", a.value, cur.unwrap_or_default()),
            AssignOp::AppendDot => format!("{}{}", cur.unwrap_or_default(), a.value),
            AssignOp::PrependDot => format!("{}{}", a.value, cur.unwrap_or_default()),
        };
        self.content.insert(a.name.clone(), new);
    }

    /// Replaces the literal reference `${name}` by `value` in every stored value.
    fn replace_ref(&mut self, name: &str, value: &str) {
        let pat = format!("${{{name}}}");
        for v in self.content.values_mut().chain(self.weak.values_mut()) {
            if v.contains(&pat) {
                *v = v.replace(&pat, value);
            }
        }
    }

    /// Expands `${VAR}` references recursively. Python (`${@...}`) and references to unset
    /// variables are left as written, as BitBake leaves unset references.
    fn expand(&self, s: &str) -> String {
        self.expand_depth(s, 0)
    }

    fn expand_depth(&self, s: &str, depth: usize) -> String {
        if depth > 32 || !s.contains("${") {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("${") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let Some(end) = matching_brace(after) else {
                out.push_str(&rest[i..]);
                return out;
            };
            let inner = &after[..end];
            let whole = &rest[i..i + 2 + end + 1];
            let is_name = !inner.is_empty()
                && inner
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_+./~:".contains(c));
            match self.get(inner).filter(|_| is_name) {
                Some(v) => out.push_str(&self.expand_depth(&v, depth + 1)),
                None => out.push_str(whole),
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// Byte index of the `}` closing a `${`, given the text after `${`.
fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// TEMPORARY (wave 1): a minimal assignment reader for `bblayers.conf` / `layer.conf`,
/// used only until the parser (`crate::parse::parse`, T1.2) is merged. It understands
/// comment lines, `\` continuations, an optional `export` prefix, the eight assignment
/// operators and quoted values; every other line is ignored. Replace it with `parse()`;
/// do not extend it.
mod conf_reader {
    use crate::types::AssignOp;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Assign {
        pub name: String,
        pub op: AssignOp,
        pub value: String,
    }

    pub(super) fn assignments(src: &str) -> Vec<Assign> {
        let src = src.strip_prefix('\u{feff}').unwrap_or(src);
        let mut out = Vec::new();
        let mut logical = String::new();
        for raw in src.lines() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if logical.is_empty() && line.trim_start().starts_with('#') {
                continue;
            }
            if let Some(cont) = line.strip_suffix('\\') {
                logical.push_str(cont);
                continue;
            }
            logical.push_str(line);
            if let Some(a) = assignment(&logical) {
                out.push(a);
            }
            logical.clear();
        }
        if let Some(a) = assignment(&logical) {
            out.push(a);
        }
        out
    }

    fn assignment(line: &str) -> Option<Assign> {
        let mut s = line.trim();
        if let Some(rest) = s.strip_prefix("export")
            && rest.starts_with(char::is_whitespace)
        {
            s = rest.trim_start();
        }
        let eq = s.find('=')?;
        let (lhs, rhs) = (&s[..eq], &s[eq + 1..]);
        let (name, op, rhs) = if let Some(n) = lhs.strip_suffix("??") {
            (n, AssignOp::WeakDefault, rhs)
        } else if let Some(n) = lhs.strip_suffix('?') {
            (n, AssignOp::Default, rhs)
        } else if let Some(n) = lhs.strip_suffix(':') {
            (n, AssignOp::Immediate, rhs)
        } else if let Some(n) = lhs.strip_suffix('+') {
            (n, AssignOp::Append, rhs)
        } else if let Some(n) = lhs.strip_suffix('.') {
            (n, AssignOp::AppendDot, rhs)
        } else if let Some(r) = rhs.strip_prefix('+') {
            (lhs, AssignOp::Prepend, r)
        } else if let Some(r) = rhs.strip_prefix('.') {
            (lhs, AssignOp::PrependDot, r)
        } else {
            (lhs, AssignOp::Set, rhs)
        };
        let name = name.trim();
        let valid_name = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_+./~${}".contains(c));
        if !valid_name {
            // Includes override-qualified names (`BBLAYERS:append`) and flags, which this
            // reader does not model.
            return None;
        }
        let rhs = rhs.trim();
        let quote = rhs.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = rhs.strip_prefix(quote)?.strip_suffix(quote)?;
        Some(Assign {
            name: name.to_string(),
            op,
            value: body.to_string(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn operators_and_continuations() {
            let src = "# c\nA = \"1\"\nB ?= '2'\nC ??= \"3\"\nD := \"4\"\nE += \"5\"\n\
                       F =+ \"6\"\nG .= \"7\"\nH =. \"8\"\nexport I = \"9\"\n\
                       J = \" \\\n  a \\\n  b \\\n  \"\nK:append = \"x\"\nL[doc] = \"y\"\n";
            let got = assignments(src);
            let ops: Vec<_> = got.iter().map(|a| (a.name.as_str(), a.op)).collect();
            assert_eq!(
                ops,
                vec![
                    ("A", AssignOp::Set),
                    ("B", AssignOp::Default),
                    ("C", AssignOp::WeakDefault),
                    ("D", AssignOp::Immediate),
                    ("E", AssignOp::Append),
                    ("F", AssignOp::Prepend),
                    ("G", AssignOp::AppendDot),
                    ("H", AssignOp::PrependDot),
                    ("I", AssignOp::Set),
                    ("J", AssignOp::Set),
                ]
            );
            assert_eq!(
                got[9].value.split_whitespace().collect::<Vec<_>>(),
                ["a", "b"]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assign(name: &str, op: AssignOp, value: &str) -> conf_reader::Assign {
        conf_reader::Assign {
            name: name.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn operator_semantics_follow_ast_py() {
        let mut v = Vars::default();
        v.apply(&assign("A", AssignOp::WeakDefault, "weak"));
        // ?= ignores the weak default (noweakdefault=True), so it sets.
        v.apply(&assign("A", AssignOp::Default, "strong"));
        assert_eq!(v.get("A").as_deref(), Some("strong"));
        v.apply(&assign("A", AssignOp::Default, "ignored"));
        assert_eq!(v.get("A").as_deref(), Some("strong"));
        v.apply(&assign("B", AssignOp::Append, "x"));
        assert_eq!(v.get("B").as_deref(), Some(" x"));
        v.apply(&assign("B", AssignOp::PrependDot, "p"));
        assert_eq!(v.get("B").as_deref(), Some("p x"));
        v.set("X", "1");
        v.apply(&assign("C", AssignOp::Immediate, "${X}"));
        v.set("X", "2");
        assert_eq!(v.get("C").as_deref(), Some("1"));
        v.apply(&assign("W", AssignOp::WeakDefault, "a"));
        v.apply(&assign("W", AssignOp::WeakDefault, "b"));
        assert_eq!(v.get("W").as_deref(), Some("b"));
    }

    #[test]
    fn expansion_leaves_python_and_unset() {
        let mut v = Vars::default();
        v.set("T", "/b");
        v.set("R", "${T}/..");
        assert_eq!(v.expand("${R}/x ${NOPE}/y"), "/b/../x ${NOPE}/y");
        let py = "${@os.path.abspath(os.path.dirname(d.getVar('FILE')) + '/../..')}";
        assert_eq!(v.expand(py), py);
        assert_eq!(
            split_words(&format!(" {py}/meta  a ")),
            vec![format!("{py}/meta"), "a".into()]
        );
    }

    #[test]
    fn dep_names_drop_versions() {
        assert_eq!(
            dep_names("core (>= 12) openembedded-layer"),
            ["core", "openembedded-layer"]
        );
        assert_eq!(dep_names(""), Vec::<String>::new());
    }

    #[test]
    fn unset_priority_is_one_above_dependencies() {
        let explicit: HashMap<String, Option<i32>> = [
            ("core".to_string(), Some(5)),
            ("a".to_string(), None),
            ("b".to_string(), Some(0)),
        ]
        .into();
        let deps: HashMap<String, Vec<String>> = [
            ("core".to_string(), vec![]),
            ("a".to_string(), vec!["core".to_string()]),
            ("b".to_string(), vec!["a".to_string()]),
        ]
        .into();
        let mut memo = HashMap::new();
        assert_eq!(
            collection_priority("a", &explicit, &deps, 0, &mut memo, 0),
            6
        );
        assert_eq!(
            collection_priority("b", &explicit, &deps, 0, &mut memo, 0),
            7
        );
    }
}
