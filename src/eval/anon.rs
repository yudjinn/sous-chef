//! When an anonymous python write cannot happen.
//!
//! The replay never runs anonymous python; every variable a function writes by name is
//! listed in `ReplayNotes::python_vars` and its value carries `unevaluated_python`
//! (`replay.rs`, `Replay::anonymous`). Many of those writes are conditional, and on a given
//! recipe the condition is often decided by static values alone. This module finds, for a
//! function body, the conditions it can decide exactly, and after the recipe is finalised
//! ([`resolve`]) takes a name out of `python_vars` when **every** write to it is proved not
//! to happen. Nothing is evaluated that python would evaluate differently: a condition is
//! decided only from certain values (the `pyexpr` subset, `DataStore::python_value`), and
//! anything else keeps the write possible. The rules and the BitBake / OE-core source are in
//! `docs/bitbake-semantics.md`, "Anonymous python writes that cannot happen".
//!
//! Two kinds of condition are recognised:
//!
//! - **An early return** at the top of the function: `if <cond>:` / `return` as its first
//!   statements (after comments, blank lines and `import` lines), `<cond>` in the `pyexpr`
//!   subset. BitBake runs every anonymous function as `def __anon_<line>_<file>(d):` and
//!   calls them one after the other (`ast.MethodNode.eval`, `ast.runAnonFuncs`), so when
//!   `<cond>` is true the function writes nothing at all (`multilib_script.bbclass`:
//!   `if not d.getVar("MULTILIB_VARIANTS"): return`).
//! - **Known OE-core code** whose writes are guarded by conditions on static values,
//!   recognised line for line (indentation included) in the body ([`Guard`]): the
//!   `PACKAGECONFIG` lists, the `hg://` fetcher dependency and the "multimachine"
//!   `PACKAGE_ARCH` switch of `base.bbclass`'s anonymous python. Code that differs in any line
//!   is not recognised, and its writes stay possible.
//!
//! Why taking a name out when all its writes are disproved is sound: suppose some write to
//! `X` did happen, and take the first one. Until then `X` has its static value, which is
//! what its condition was decided with (the name is taken out of `python_vars` while the
//! condition is decided); every other value the condition read was certain, so not written
//! by any anonymous python. The condition was then false, and the write did not happen.
//!
//! The proof covers literal writes, so a name is **not** taken out while a function that
//! may run (not proved to return at once) writes a value through a computed name that may
//! be it (`AnonPython::computed_writes`: `d.setVar(v, ...)` with `v` of unknown value; a
//! name built from a literal prefix, `"do_image_%s" % t`, only reaches names with that
//! prefix; acc-unfollowed). As everywhere in the evaluator, a function that hands `d` to
//! other code (`AnonPython::opaque`) is warned about but does not flag other values; that
//! contract is unchanged here.

use std::path::Path;
use std::sync::Arc;

use super::DataStore;
use super::pyexpr::PyVal;
use crate::types::Loc;

/// A condition of known OE-core code on one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Guard {
    /// `base.bbclass` PACKAGECONFIG handling: `appendVar(<name>, <list>)` through its local
    /// wrapper, which returns at once on an empty list. The list gets the flag's item
    /// `in_item` for each `PACKAGECONFIG[flag]` whose flag is in `PACKAGECONFIG`, and item
    /// `out_item` for the others.
    PackageconfigList {
        in_item: usize,
        out_item: Option<usize>,
    },
    /// `base.bbclass`: `d.appendVar("EXTRANATIVEPATH", ...)` for a `SRC_URI` entry whose
    /// scheme is `hg`.
    HgScheme,
    /// `base.bbclass` "multimachine" handling: `d.setVar('PACKAGE_ARCH', "${MACHINE_ARCH}")`
    /// when a `file://` entry of `SRC_URI` is found in a `FILESPATH` directory named after
    /// `MACHINE`, or a `PACKAGE_ARCH_<pkg>` equals `MACHINE_ARCH`; never when `PACKAGE_ARCH`
    /// already equals `MACHINE_ARCH`.
    Multimachine,
    /// Every write of the name in the body is inside `if <cond>:` / `elif <cond>:`
    /// statements: per write, the conditions of the statements around it (innermost
    /// first). A write whose conditions include a false one does not run.
    Sites(Vec<Vec<String>>),
}

/// What [`analyse`] found in one function body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Conditions {
    /// The conditions of the leading `if <cond>: return` statements, as written.
    pub(crate) early_returns: Vec<String>,
    /// Names as written (before expansion) whose every write in the body is under `Guard`.
    pub(crate) guarded: Vec<(String, Guard)>,
}

/// The conditions [`analyse`] can decide in an anonymous function's text (the lines the
/// statement covers, `python () {` and `}` included).
pub(crate) fn analyse(text: &str) -> Conditions {
    let lines = body_lines(text);
    let mut out = Conditions {
        early_returns: early_returns(&lines),
        guarded: Vec::new(),
    };
    if let Some(g) = packageconfig_guards(&lines, text) {
        out.guarded.extend(g);
    }
    if count_block(&lines, HG_SCHEME) == 1
        && count_block(&lines, SRC_URI_LOOP) == 1
        && literal_writes(text, "EXTRANATIVEPATH") == 1
    {
        out.guarded
            .push(("EXTRANATIVEPATH".into(), Guard::HgScheme));
    }
    if count_block(&lines, MULTIMACHINE) == 1
        && count_block(&lines, SRC_URI_LOOP) == 1
        && count_lines_containing(&lines, "srcuri =") == 1
        && literal_writes(text, "PACKAGE_ARCH") == 2
    {
        out.guarded
            .push(("PACKAGE_ARCH".into(), Guard::Multimachine));
    }
    for (name, sites) in write_sites(&lines, text) {
        if out.guarded.iter().any(|(n, _)| *n == name) {
            continue;
        }
        out.guarded.push((name, Guard::Sites(sites)));
    }
    out
}

/// For each name written by a literal `d.<write>('NAME', ...)` call (the calls
/// `literal_writes` counts), when every such call is inside at least one `if` / `elif`:
/// the conditions around each call. Nothing when the body's structure cannot be read
/// safely: a tab, a triple-quoted string, an assignment to `d`, `bb` or `oe` (the
/// conditions would not mean what they say), a call that does not start its line or sits
/// in a nested `def`, or a line above it that closes no block properly (a continued
/// statement).
fn write_sites(lines: &[&str], text: &str) -> Vec<(String, Vec<Vec<String>>)> {
    static WRITE: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"^\s*d\.(?:setVar|setVarFilter|appendVar|prependVar|delVar|renameVar|setVarFlag|setVarFlags|appendVarFlag|prependVarFlag|delVarFlag|delVarFlags)\s*\(\s*(?:'([^'\\]*)'|"([^"\\]*)")"#,
        )
        .ok()
    });
    static REBIND: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?m)(?:^\s*(?:d|bb|oe)\s*(?:=[^=]|[+\-*/|&]=)|\bfor\s+(?:d|bb|oe)\b|\bas\s+(?:d|bb|oe)\b|\bdef\s+\w+\s*\([^)]*\b(?:bb|oe)\b)").ok()
    });
    let (Some(write), Some(rebind)) = (WRITE.as_ref(), REBIND.as_ref()) else {
        return Vec::new();
    };
    if text.contains('\t')
        || text.contains("\"\"\"")
        || text.contains("'''")
        || rebind.is_match(text)
    {
        return Vec::new();
    }
    let mut by_name: Vec<(String, Vec<Option<Vec<String>>>)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(c) = write.captures(l) else { continue };
        let Some(name) = c
            .get(1)
            .or_else(|| c.get(2))
            .map(|m| m.as_str().to_string())
        else {
            continue;
        };
        let conds = enclosing_conditions(lines, i);
        match by_name.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => v.push(conds),
            None => by_name.push((name, vec![conds])),
        }
    }
    let mut out = Vec::new();
    for (name, sites) in by_name {
        // Every call that writes this literal name was found on its own line.
        if literal_writes_exact(text, &name) != sites.len() {
            continue;
        }
        let Some(sites) = sites
            .into_iter()
            .map(|s| s.filter(|c| !c.is_empty()))
            .collect::<Option<Vec<Vec<String>>>>()
        else {
            continue;
        };
        out.push((name, sites));
    }
    out
}

/// The conditions of the `if` / `elif` statements around line `i` of the body, innermost
/// first; `None` when a line above it that has less indentation is not a block header
/// (`if`, `elif`, `else`, `for`, `while`, `with`, `try`, `except`, `finally`, each ending
/// in `:`), or is a `def` / `class`.
fn enclosing_conditions(lines: &[&str], i: usize) -> Option<Vec<String>> {
    const HEADERS: &[&str] = &[
        "else:", "for ", "while ", "with ", "try:", "except", "finally:",
    ];
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
        if !t.ends_with(':') {
            return None;
        }
        if let Some(c) = t.strip_prefix("if ").or_else(|| t.strip_prefix("elif ")) {
            out.push(c[..c.len() - 1].trim().to_string());
        } else if !HEADERS.iter().any(|h| t.starts_with(h)) {
            return None;
        }
        cur = ind;
        if cur == 0 {
            break;
        }
    }
    Some(out)
}

/// [`literal_writes`] for exactly `name` (the literal ends right after it).
fn literal_writes_exact(text: &str, name: &str) -> usize {
    const METHODS: &str = "setVar|setVarFilter|appendVar|prependVar|delVar|renameVar|setVarFlag|setVarFlags|appendVarFlag|prependVarFlag|delVarFlag|delVarFlags";
    let Ok(re) = regex::Regex::new(&format!(
        r#"\bd\.(?:{METHODS})\s*\(\s*(?:'{n}'|"{n}")"#,
        n = regex::escape(name)
    )) else {
        return usize::MAX;
    };
    re.find_iter(text).count()
}

/// The body of the function: the lines between `python () {` and the closing `}`.
fn body_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.first().is_some_and(|l| l.trim_end().ends_with('{')) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|l| l.trim() == "}") {
        lines.pop();
    }
    lines
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

/// The leading `if <cond>:` / `return` statements. Comment, blank and `import` lines may
/// come first; a line indented with a tab, or anything else, ends the search.
fn early_returns(lines: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut base: Option<usize> = None;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            i += 1;
            continue;
        }
        if l.starts_with('\t') {
            break;
        }
        let ind = indent(l);
        if *base.get_or_insert(ind) != ind {
            break;
        }
        if !t.contains(';')
            && (t.starts_with("import ") || (t.starts_with("from ") && t.contains(" import ")))
        {
            i += 1;
            continue;
        }
        let Some(cond) = t.strip_prefix("if ") else {
            break;
        };
        // `if <cond>: return` on one line.
        if let Some(c) = cond.strip_suffix(": return") {
            out.push(c.trim().to_string());
            i += 1;
            continue;
        }
        let Some(cond) = cond.strip_suffix(':') else {
            break;
        };
        // The body of the `if` is exactly `return` (comments allowed around it), then a line
        // back at the function's indentation, or the end.
        let mut j = i + 1;
        let mut ret = false;
        let mut ok = true;
        while j < lines.len() {
            let t2 = lines[j].trim();
            if t2.is_empty() || t2.starts_with('#') {
                j += 1;
                continue;
            }
            if lines[j].starts_with('\t') {
                ok = false;
                break;
            }
            let ind2 = indent(lines[j]);
            if ind2 <= ind {
                break;
            }
            if ret || t2 != "return" {
                ok = false;
                break;
            }
            ret = true;
            j += 1;
        }
        if !ok || !ret {
            break;
        }
        out.push(cond.trim().to_string());
        i = j;
    }
    out
}

/// How often `block` (lines, indentation relative to its first line) occurs in `lines`
/// with the same relative indentation; blank lines are skipped on both sides.
fn count_block(lines: &[&str], block: &str) -> usize {
    let want: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
    let Some(first) = want.first() else {
        return 0;
    };
    let base = indent(first);
    let want: Vec<(usize, &str)> = want
        .iter()
        .map(|l| (indent(l).saturating_sub(base), l.trim_end().trim_start()))
        .collect();
    let have: Vec<(usize, &str)> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| (indent(l), l.trim_end().trim_start()))
        .collect();
    let mut n = 0;
    for start in 0..have.len() {
        if start + want.len() > have.len() {
            break;
        }
        let at = have[start].0;
        let same = want.iter().enumerate().all(|(k, (wi, wt))| {
            let (hi, ht) = have[start + k];
            hi == at + wi && ht == *wt
        });
        if same {
            n += 1;
        }
    }
    n
}

fn count_lines_containing(lines: &[&str], s: &str) -> usize {
    lines.iter().filter(|l| l.contains(s)).count()
}

/// Calls of the datastore's write methods (as `replay::python_writes` recognises them)
/// whose first argument is the string literal `name`, or starts with it.
fn literal_writes(text: &str, name: &str) -> usize {
    const METHODS: &[&str] = &[
        "setVar",
        "setVarFilter",
        "appendVar",
        "prependVar",
        "delVar",
        "renameVar",
        "setVarFlag",
        "setVarFlags",
        "appendVarFlag",
        "prependVarFlag",
        "delVarFlag",
        "delVarFlags",
    ];
    let Ok(re) = regex::Regex::new(&format!(
        r#"\bd\.(?:{})\s*\(\s*['"]{}"#,
        METHODS.join("|"),
        regex::escape(name)
    )) else {
        return usize::MAX;
    };
    re.find_iter(text).count()
}

/// The `PACKAGECONFIG` handling of `base.bbclass`'s anonymous python (OE-core Wrynose
/// `33a6bd6c`; the same lines in Scarthgap and Kirkstone): the four wrapper calls, each
/// guarded by its list.
fn packageconfig_guards(lines: &[&str], text: &str) -> Option<Vec<(String, Guard)>> {
    if count_block(lines, PKGCONF_HEAD) != 1
        || count_block(lines, PKGCONF_WRAPPER) != 1
        || count_block(lines, PKGCONF_LISTS) != 1
    {
        return None;
    }
    let targets = [
        (
            "DEPENDS",
            Guard::PackageconfigList {
                in_item: 2,
                out_item: None,
            },
        ),
        (
            "RDEPENDS:${PN}",
            Guard::PackageconfigList {
                in_item: 3,
                out_item: None,
            },
        ),
        (
            "RRECOMMENDS:${PN}",
            Guard::PackageconfigList {
                in_item: 4,
                out_item: None,
            },
        ),
        (
            "PACKAGECONFIG_CONFARGS",
            Guard::PackageconfigList {
                in_item: 0,
                out_item: Some(1),
            },
        ),
    ];
    let mut out = Vec::new();
    for (name, g) in targets {
        // Written nowhere else in the body: no other wrapper call, no `d.<write>(name`.
        let call = format!("appendVar('{name}',");
        if text.matches(call.as_str()).count() != 1 || literal_writes(text, name) != 0 {
            continue;
        }
        out.push((name.to_string(), g));
    }
    Some(out)
}

const PKGCONF_HEAD: &str = r#"pkgconfigflags = d.getVarFlags("PACKAGECONFIG") or {}
if pkgconfigflags:
    pkgconfig = (d.getVar('PACKAGECONFIG') or "").split()"#;

const PKGCONF_WRAPPER: &str = r#"def appendVar(varname, appends):
    if not appends:
        return"#;

const PKGCONF_LISTS: &str = r#"extradeps = []
extrardeps = []
extrarrecs = []
extraconf = []
for flag, flagval in sorted(pkgconfigflags.items()):
    items = flagval.split(",")
    num = len(items)
    if num > 6:
        bb.error("%s: PACKAGECONFIG[%s] Only enable,disable,depend,rdepend,rrecommend,conflict_packageconfig can be specified!"
            % (d.getVar('PN'), flag))

    if flag in pkgconfig:
        if num >= 3 and items[2]:
            extradeps.append(items[2])
        if num >= 4 and items[3]:
            extrardeps.append(items[3])
        if num >= 5 and items[4]:
            extrarrecs.append(items[4])
        if num >= 1 and items[0]:
            extraconf.append(items[0])
    elif num >= 2 and items[1]:
            extraconf.append(items[1])

    if num >= 6 and items[5]:
        conflicts = set(items[5].split())
        invalid = conflicts.difference(set(pkgconfigflags.keys()))
        if invalid:
            bb.error("%s: PACKAGECONFIG[%s] Invalid conflict package config%s '%s' specified."
                % (d.getVar('PN'), flag, 's' if len(invalid) > 1 else '', ' '.join(invalid)))

        if flag in pkgconfig:
            intersec = conflicts.intersection(set(pkgconfig))
            if intersec:
                bb.fatal("%s: PACKAGECONFIG[%s] Conflict package config%s '%s' set in PACKAGECONFIG."
                    % (d.getVar('PN'), flag, 's' if len(intersec) > 1 else '', ' '.join(intersec)))

appendVar('DEPENDS', extradeps)
appendVar('RDEPENDS:${PN}', extrardeps)
appendVar('RRECOMMENDS:${PN}', extrarrecs)
appendVar('PACKAGECONFIG_CONFARGS', extraconf)"#;

const SRC_URI_LOOP: &str = r#"srcuri = d.getVar('SRC_URI')
for uri_string in srcuri.split():
    uri = bb.fetch.URI(uri_string)"#;

const HG_SCHEME: &str = r#"elif uri.scheme == "hg":
    d.appendVar("EXTRANATIVEPATH", ' python3-native ')"#;

const MULTIMACHINE: &str = r#"# 'multimachine' handling
mach_arch = d.getVar('MACHINE_ARCH')
pkg_arch = d.getVar('PACKAGE_ARCH')

if (pkg_arch == mach_arch):
    # Already machine specific - nothing further to do
    return

#
# We always try to scan SRC_URI for urls with machine overrides
# unless the package sets SRC_URI_OVERRIDES_PACKAGE_ARCH=0
#
override = d.getVar('SRC_URI_OVERRIDES_PACKAGE_ARCH')
if override != '0':
    paths = []
    fpaths = (d.getVar('FILESPATH') or '').split(':')
    machine = d.getVar('MACHINE')
    for p in fpaths:
        if os.path.basename(p) == machine and os.path.isdir(p):
            paths.append(p)

    if paths:
        for s in srcuri.split():
            if not s.startswith("file://"):
                continue
            fetcher = bb.fetch2.Fetch([s], d)
            local = fetcher.localpath(s)
            for mp in paths:
                if local.startswith(mp):
                    #bb.note("overriding PACKAGE_ARCH from %s to %s for %s" % (pkg_arch, mach_arch, pn))
                    d.setVar('PACKAGE_ARCH', "${MACHINE_ARCH}")
                    return

packages = d.getVar('PACKAGES').split()
for pkg in packages:
    pkgarch = d.getVar("PACKAGE_ARCH_%s" % pkg)

    # We could look for != PACKAGE_ARCH here but how to choose
    # if multiple differences are present?
    # Look through PACKAGE_ARCHS for the priority order?
    if pkgarch and pkgarch == mach_arch:
        d.setVar('PACKAGE_ARCH', "${MACHINE_ARCH}")"#;

// ---------------------------------------------------------------------------------------
// Deciding the conditions on the finalised recipe
// ---------------------------------------------------------------------------------------

/// `d.getVar(name)` as python reads it: `Some(None)` unset, `None` when not certain.
fn get_var(ds: &DataStore, name: &str) -> Option<Option<String>> {
    if name.contains(['\'', '\\', '\n']) {
        return None;
    }
    match ds.python_value(&format!("d.getVar('{name}')"))? {
        PyVal::None => Some(None),
        PyVal::Str(s) => Some(Some(s)),
        _ => None,
    }
}

/// `str.split()` (no argument): `None` when the text holds a character python and Rust
/// disagree on (the separators `\x1c`..`\x1f` are whitespace for python only).
fn py_split(s: &str) -> Option<Vec<&str>> {
    if s.chars().any(|c| ('\x1c'..='\x1f').contains(&c)) {
        return None;
    }
    Some(s.split_whitespace().collect())
}

/// Why the early return `cond` is taken, or `None` when that cannot be told.
pub(crate) fn early_return_taken(ds: &DataStore, cond: &str) -> Option<String> {
    let v = ds.python_value(cond)?;
    v.truthy()
        .ok()?
        .then(|| format!("it returns at once: `{cond}` is true"))
}

/// Why the write under `guard` does not happen, or `None` when it may. `is_dir` tells
/// whether a path is a directory (the overlay counts).
pub(crate) fn guard_false(
    ds: &DataStore,
    guard: &Guard,
    is_dir: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    match guard {
        Guard::PackageconfigList { in_item, out_item } => {
            packageconfig_empty(ds, *in_item, *out_item)
        }
        Guard::HgScheme => {
            let src = get_var(ds, "SRC_URI")??;
            let words = py_split(&src)?;
            // `bb.fetch.URI(...).scheme` is `urllib.parse.urlparse`'s scheme, lower-cased:
            // a word without `hg:` (any case) anywhere cannot have it.
            (!words.iter().any(|w| w.to_ascii_lowercase().contains("hg:")))
                .then(|| "no SRC_URI entry has the hg scheme".to_string())
        }
        Guard::Multimachine => multimachine_unchanged(ds, is_dir),
        Guard::Sites(sites) => {
            let mut falses = Vec::new();
            for conds in sites {
                let f = conds.iter().find(|c| {
                    ds.python_value(c)
                        .and_then(|v| v.truthy().ok())
                        .is_some_and(|t| !t)
                })?;
                if !falses.contains(f) {
                    falses.push(f.clone());
                }
            }
            let list: Vec<String> = falses.iter().map(|c| format!("`{c}`")).collect();
            Some(format!(
                "every write is under a false condition: {}",
                list.join(", ")
            ))
        }
    }
}

fn packageconfig_empty(ds: &DataStore, in_item: usize, out_item: Option<usize>) -> Option<String> {
    // The flags as `d.getVarFlags("PACKAGECONFIG")` returns them (unexpanded, `??=`
    // defaults for flags that have no value): only when nothing python or an uncertain
    // file may have set a flag.
    let n = ds.replay();
    if n.python_vars.contains("PACKAGECONFIG")
        || n.uncertain_vars.contains("PACKAGECONFIG")
        || n.unresolved_vars.contains_key("PACKAGECONFIG")
    {
        return None;
    }
    let flags = ds.flag_names("PACKAGECONFIG");
    if flags.is_empty() {
        return Some("PACKAGECONFIG has no flags, so the list is empty".into());
    }
    // `export` / `unexport` hold an integer in BitBake (`flagval.split` would fail).
    if flags.iter().any(|f| f == "export" || f == "unexport") {
        return None;
    }
    let mut pkgconfig: Option<Vec<String>> = None;
    for f in &flags {
        let raw = ds.raw_flag("PACKAGECONFIG", f)?;
        let items: Vec<&str> = raw.split(',').collect();
        let nonempty = |i: usize| items.get(i).is_some_and(|s| !s.is_empty());
        let if_in = nonempty(in_item);
        let if_out = out_item.is_some_and(nonempty);
        if !if_in && !if_out {
            continue;
        }
        if pkgconfig.is_none() {
            let v = get_var(ds, "PACKAGECONFIG")?.unwrap_or_default();
            pkgconfig = Some(py_split(&v)?.into_iter().map(str::to_string).collect());
        }
        let on = pkgconfig.as_ref().is_some_and(|p| p.iter().any(|x| x == f));
        if (on && if_in) || (!on && if_out) {
            return None;
        }
    }
    Some("no PACKAGECONFIG flag gives the list an item, so the wrapper returns at once".into())
}

fn multimachine_unchanged(ds: &DataStore, is_dir: &dyn Fn(&Path) -> bool) -> Option<String> {
    let mach_arch = get_var(ds, "MACHINE_ARCH")?;
    let pkg_arch = get_var(ds, "PACKAGE_ARCH");
    if pkg_arch.as_ref() == Some(&mach_arch) {
        return Some("PACKAGE_ARCH already equals MACHINE_ARCH".into());
    }
    let first = no_machine_file(ds, is_dir)?;
    let second = no_machine_package(ds, mach_arch.as_deref())?;
    Some(format!("{first}, and {second}"))
}

/// The `file://` branch cannot set it.
fn no_machine_file(ds: &DataStore, is_dir: &dyn Fn(&Path) -> bool) -> Option<String> {
    if get_var(ds, "SRC_URI_OVERRIDES_PACKAGE_ARCH") == Some(Some("0".into())) {
        return Some("SRC_URI_OVERRIDES_PACKAGE_ARCH is 0".into());
    }
    if let Some(Some(src)) = get_var(ds, "SRC_URI")
        && py_split(&src).is_some_and(|w| !w.iter().any(|s| s.starts_with("file://")))
    {
        return Some("SRC_URI has no file:// entry".into());
    }
    let machine = get_var(ds, "MACHINE")?;
    let fpaths = get_var(ds, "FILESPATH")?.unwrap_or_default();
    for p in fpaths.split(':') {
        // `os.path.basename(p) == machine`: the text after the last `/`.
        let base = p.rsplit('/').next().unwrap_or(p);
        if machine.as_deref() != Some(base) {
            continue;
        }
        let path = Path::new(p);
        // A relative entry would be found from BitBake's working directory.
        if !path.is_absolute() || is_dir(path) {
            return None;
        }
    }
    Some("no FILESPATH directory is named after MACHINE".into())
}

/// The `PACKAGE_ARCH_<pkg>` branch cannot set it.
fn no_machine_package(ds: &DataStore, mach_arch: Option<&str>) -> Option<String> {
    const PREFIX: &str = "PACKAGE_ARCH_";
    let n = ds.replay();
    let maybe_somewhere = ds.names().iter().any(|k| k.starts_with(PREFIX))
        || n.python_vars.iter().any(|k| k.starts_with(PREFIX))
        || n.uncertain_vars.iter().any(|k| k.starts_with(PREFIX))
        || n.unresolved_vars.keys().any(|k| k.starts_with(PREFIX))
        || n.incomplete_keys.iter().any(|k| {
            let lit = k.split("${").next().unwrap_or(k);
            PREFIX.starts_with(lit) || lit.starts_with(PREFIX)
        });
    if !maybe_somewhere {
        return Some("no PACKAGE_ARCH_<pkg> is set".into());
    }
    let packages = get_var(ds, "PACKAGES")??;
    for pkg in py_split(&packages)? {
        if let Some(v) = get_var(ds, &format!("{PREFIX}{pkg}"))?
            && !v.is_empty()
            && Some(v.as_str()) == mach_arch
        {
            return None;
        }
    }
    Some("no PACKAGE_ARCH_<pkg> equals MACHINE_ARCH".into())
}

/// One anonymous-python write that was proved not to happen on this recipe.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NoWrite {
    /// The name as recorded in `python_vars`.
    pub name: String,
    /// Per write (function) of it: the function, and why that write does not happen.
    pub why: Vec<(Loc, String)>,
}

/// After finalisation: takes out of `python_vars` every name whose writes by anonymous
/// python are all proved not to happen (see the module documentation), repeating until
/// nothing changes. Names some other source put there (an event handler that is not run,
/// a filter not computed: `ReplayNotes::fixed_python_vars`) stay.
pub(crate) fn resolve(ds: &mut DataStore, is_dir: &dyn Fn(&Path) -> bool) {
    loop {
        let mut changed = false;
        let candidates: Vec<String> = ds
            .notes
            .python_vars
            .iter()
            .filter(|e| !ds.notes.fixed_python_vars.contains(*e))
            .filter(|e| {
                ds.notes
                    .anonymous
                    .iter()
                    .any(|a| a.entries.iter().any(|(n, _)| n == *e))
            })
            .cloned()
            .collect();
        for e in candidates {
            let mut trial = ds.fork();
            {
                let notes = Arc::make_mut(&mut trial.notes);
                notes.python_vars.remove(&e);
            }
            trial.ovr_cache = Default::default();
            let mut why = Vec::new();
            let mut all = true;
            for a in ds.notes.anonymous.iter() {
                for (n, g) in &a.entries {
                    if *n != e {
                        continue;
                    }
                    let early = a
                        .early_returns
                        .iter()
                        .find_map(|c| early_return_taken(&trial, c));
                    let r =
                        early.or_else(|| g.as_ref().and_then(|g| guard_false(&trial, g, is_dir)));
                    match r {
                        Some(r) => why.push((a.loc.clone(), r)),
                        None => all = false,
                    }
                }
                if !all {
                    break;
                }
            }
            if !all {
                continue;
            }
            // A write through a computed name that may be this one, in a function not
            // proved to return at once, keeps it possible (acc-unfollowed): the proof covers
            // the literal writes only.
            let computed = ds.notes.anonymous.iter().any(|a| {
                a.computed_writes.iter().any(|p| {
                    p.as_ref()
                        .is_none_or(|p| e.starts_with(p.as_str()) || p.starts_with(e.as_str()))
                }) && !a
                    .early_returns
                    .iter()
                    .any(|c| early_return_taken(&trial, c).is_some())
            });
            if computed {
                continue;
            }
            Arc::make_mut(&mut trial.notes)
                .no_writes
                .push(NoWrite { name: e, why });
            *ds = trial;
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_returns_are_the_leading_ifs() {
        let text = "python () {\n    # Do nothing if multilib isn't being used\n    if not d.getVar(\"MULTILIB_VARIANTS\"):\n        return\n    # Do nothing for native/cross\n    if bb.data.inherits_class('native', d) or bb.data.inherits_class('cross', d):\n        return\n\n    for entry in (d.getVar(\"MULTILIB_SCRIPTS\") or \"\").split():\n        d.appendVar(\"FILES:\" + pkg, \" x\")\n}";
        let c = analyse(text);
        assert_eq!(
            c.early_returns,
            vec![
                "not d.getVar(\"MULTILIB_VARIANTS\")".to_string(),
                "bb.data.inherits_class('native', d) or bb.data.inherits_class('cross', d)"
                    .to_string()
            ]
        );
        // One line, imports first.
        let c = analyse(
            "python () {\n    import os\n    if d.getVar('X') != '1': return\n    d.setVar('Y', '1')\n}",
        );
        assert_eq!(c.early_returns, vec!["d.getVar('X') != '1'".to_string()]);
    }

    #[test]
    fn an_if_that_does_more_than_return_is_no_early_return() {
        for text in [
            // A write before the `if`.
            "python () {\n    d.setVar('Y', '1')\n    if not d.getVar('X'):\n        return\n}",
            // The `if` body writes, then returns.
            "python () {\n    if not d.getVar('X'):\n        d.setVar('Y', '1')\n        return\n}",
            // `return` with a value, or a deeper statement after it.
            "python () {\n    if not d.getVar('X'):\n        return 1\n}",
            "python () {\n    if not d.getVar('X'):\n        return\n        d.setVar('Y', '1')\n}",
            // A comment after the colon, a continued condition, a statement after `;`.
            "python () {\n    if not d.getVar('X'):  # why\n        return\n}",
            "python () {\n    if not d.getVar('X') and \\\n            d.getVar('Z'):\n        return\n}",
            "python () {\n    import os; d.setVar('Y', '1')\n    if not d.getVar('X'):\n        return\n}",
        ] {
            let c = analyse(text);
            assert!(c.early_returns.is_empty(), "{text}: {:?}", c.early_returns);
        }
        // With `else`, the first `if` is an early return; the `else` line ends the search
        // (it is not an `if`), and the `else` branch only runs when the condition is false.
        let c = analyse(
            "python () {\n    if not d.getVar('X'):\n        return\n    else:\n        d.setVar('Y', '1')\n}",
        );
        assert_eq!(c.early_returns, vec!["not d.getVar('X')".to_string()]);
    }

    #[test]
    fn blocks_match_with_their_indentation_only() {
        let lines = vec!["    if a:", "        return", "    b()"];
        assert_eq!(count_block(&lines, "if a:\n    return"), 1);
        assert_eq!(count_block(&lines, "if a:\nreturn"), 0);
        let lines = vec!["    if a:", "    return"];
        assert_eq!(count_block(&lines, "if a:\n    return"), 0);
    }

    #[test]
    fn literal_writes_count_prefixes() {
        let t = "d.setVar('PACKAGE_ARCH', x)\nd.setVar(\"PACKAGE_ARCH_%s\" % p, y)\nd.getVar('PACKAGE_ARCH')";
        assert_eq!(literal_writes(t, "PACKAGE_ARCH"), 2);
        assert_eq!(literal_writes(t, "PACKAGE_ARCH_"), 1);
    }
}
