//! z1-env: sous-chef's own environment variables are namespaced `SOUS_CHEF_*`; any other
//! name it reads is on [`ALLOWED`], with the reason it reads it.
//!
//! Why: a generic name read by sous-chef is also in the environment of every process it
//! starts, and some of those (BitBake, kas) give such names a meaning of their own; the
//! reverse holds too (`TMPDIR`, docs/bitbake-semantics.md "The environment BitBake takes
//! from its caller"). A deliberate foreign read is fine; an accidental one is a collision
//! waiting to happen.
//!
//! How the reads are found: every `.rs` file under `src/`, comments removed, is searched
//! for `env::var(`, `env::var_os(`, `env::vars(`, `env::vars_os(`, `env::temp_dir(` (std
//! reads `TMPDIR`), `env::home_dir(` (`HOME`), and the compile-time `env!(` /
//! `option_env!(`. The argument of `var` / `var_os` is resolved to names:
//!
//! - a string literal;
//! - a constant (`NAME` or `path::NAME`) declared anywhere in `src/` as `const NAME: &str =
//!   "..."` or as an array of string literals;
//! - a parameter `k` of a closure `let f = |k...| ... env::var_os(k)`: every call `f(<arg>)`
//!   in the same file is resolved in turn;
//! - the variable of a `for k in <constant>` loop.
//!
//! Anything else (an expression, `env::vars()`, a `use std::env::var` import that would
//! hide the call) is a failure: restructure the read so one of the forms above applies.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;

/// The foreign names sous-chef reads on purpose. A trailing `*` matches a prefix.
const ALLOWED: &[(&str, &str)] = &[
    (
        "HOME",
        "the cache root's fallback, $HOME/.cache (index::cache_root, XDG base directory spec)",
    ),
    (
        "XDG_CACHE_HOME",
        "the cache root (index::cache_root, XDG base directory spec)",
    ),
    (
        "BUILDDIR",
        "detecting a build shell that sourced the init script (initialised mode, HostEnv)",
    ),
    (
        "BBPATH",
        "detecting a build shell that sourced the init script (initialised mode, HostEnv)",
    ),
    (
        "PATH",
        "finding bitbake and kas for mode detection (HostEnv, find_on_path)",
    ),
    (
        "KAS_*",
        "kas honours KAS_WORK_DIR, KAS_BUILD_DIR, KAS_MACHINE, KAS_DISTRO, KAS_TARGET, KAS_TASK; sous-chef follows them (KasEnv)",
    ),
    (
        "TMPDIR",
        "to say in status that it is withheld from BitBake (bitbake::env::STRIPPED_ENV, withheld_env); and std::env::temp_dir, the last-resort cache directory",
    ),
    (
        "CARGO_*",
        "compile time only (env!): the package version and the manifest directory, set by Cargo",
    ),
];

/// One read found in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Read {
    file: String,
    line: usize,
    /// The names read; empty when it could not be resolved.
    names: Vec<String>,
    /// Why it could not be resolved.
    problem: Option<String>,
}

/// Two copies of `src`, byte for byte the same length (so a position in one is the same
/// in the other, and line numbers stay right): `code`, with comments blanked, and `bare`,
/// also with the inside of every string literal blanked, where calls are searched for.
/// Blanking replaces each byte but `\n` with a space.
struct Masked {
    code: String,
    bare: String,
}

fn mask(src: &str) -> Masked {
    let b = src.as_bytes();
    let mut code = b.to_vec();
    let mut bare = b.to_vec();
    let blank = |v: &mut Vec<u8>, from: usize, to: usize| {
        for x in &mut v[from..to] {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            blank(&mut code, start, i);
            blank(&mut bare, start, i);
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            let i2 = i.min(b.len());
            blank(&mut code, start, i2);
            blank(&mut bare, start, i2);
        } else if c == b'r'
            && matches!(b.get(i + 1), Some(b'"') | Some(b'#'))
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
        {
            // A raw string r"..." / r#"..."#.
            i += 1;
            let mut hashes = 0;
            while b.get(i) == Some(&b'#') {
                hashes += 1;
                i += 1;
            }
            if b.get(i) != Some(&b'"') {
                continue;
            }
            i += 1;
            let body = i;
            while i < b.len()
                && !(b[i] == b'"' && (0..hashes).all(|h| b.get(i + 1 + h) == Some(&b'#')))
            {
                i += 1;
            }
            blank(&mut bare, body, i.min(b.len()));
            i = (i + 1 + hashes).min(b.len());
        } else if c == b'"' {
            i += 1;
            let body = i;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            blank(&mut bare, body, i.min(b.len()));
            i = (i + 1).min(b.len());
        } else if c == b'\'' {
            // A char literal ('"', '\'', '\n') or a lifetime ('a): skip a literal whole.
            if b.get(i + 1) == Some(&b'\\') {
                let mut j = i + 3;
                while j < b.len() && b[j] != b'\'' {
                    j += 1;
                }
                i = (j + 1).min(b.len());
            } else if b.get(i + 2) == Some(&b'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    // Only ASCII bytes were written, each over one byte: still UTF-8.
    Masked {
        code: String::from_utf8(code).unwrap(),
        bare: String::from_utf8(bare).unwrap(),
    }
}

/// The argument of the call whose `(` is at `open`: the matching `)` is found in `m.bare`
/// (no string can unbalance it), the text is taken from `m.code` (literals intact).
fn argument(m: &Masked, open: usize) -> &str {
    let mut depth = 0;
    for (j, c) in m.bare[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &m.code[open + 1..open + j];
                }
            }
            _ => {}
        }
    }
    &m.code[open + 1..]
}

/// Every string constant in the sources: name to its value(s).
fn constants(files: &[(String, Masked)]) -> BTreeMap<String, Vec<String>> {
    let decl = Regex::new(r"const\s+([A-Z][A-Z0-9_]*)\s*:\s*[^=]+=\s*([^;]+);").unwrap();
    let lit = Regex::new(r#""([^"\\]*)""#).unwrap();
    let mut out = BTreeMap::new();
    for (_, m) in files {
        let text = &m.code;
        for c in decl.captures_iter(text) {
            let value = c[2].trim();
            if value.starts_with('"') || value.starts_with('[') {
                let names: Vec<String> =
                    lit.captures_iter(value).map(|l| l[1].to_string()).collect();
                if !names.is_empty() {
                    out.insert(c[1].to_string(), names);
                }
            }
        }
    }
    out
}

/// Resolves one argument expression (of a read at `at` in `m`) to names, or says why it
/// cannot.
fn resolve(
    arg: &str,
    m: &Masked,
    at: usize,
    consts: &BTreeMap<String, Vec<String>>,
    depth: usize,
) -> Result<Vec<String>, String> {
    let arg = arg.trim().trim_start_matches('&').trim();
    let lit = Regex::new(r#"^"([^"\\]*)"$"#).unwrap();
    if let Some(c) = lit.captures(arg) {
        return Ok(vec![c[1].to_string()]);
    }
    let path = Regex::new(r"^(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Za-z_][A-Za-z0-9_]*)$").unwrap();
    let Some(last) = path.captures(arg).map(|c| c[1].to_string()) else {
        return Err(format!(
            "argument `{arg}` is not a literal, a constant or a closure/loop variable"
        ));
    };
    if last.starts_with(|c: char| c.is_ascii_uppercase()) {
        return consts
            .get(&last)
            .cloned()
            .ok_or_else(|| format!("constant `{last}` is not a string constant declared in src/"));
    }
    if depth > 3 {
        return Err(format!("`{arg}`: closures nested too deeply to follow"));
    }
    let before = &m.bare[..at];
    let closure = Regex::new(&format!(r"let\s+(\w+)\s*=\s*(?:move\s+)?\|\s*{last}\b")).unwrap();
    let looped = Regex::new(&format!(r"for\s+{last}\s+in\s+&?([\w:]+)")).unwrap();
    let last_match = |re: &Regex| {
        re.captures_iter(before)
            .last()
            .map(|c| (c.get(0).unwrap().start(), c[1].to_string()))
    };
    match (last_match(&closure), last_match(&looped)) {
        (Some((ca, f)), l) if l.as_ref().is_none_or(|(la, _)| ca > *la) => {
            // The calls `f(<arg>)` (not `x.f(` or `p::f(`) from the closure to the end of
            // its function, taken as the next `fn`.
            let end = Regex::new(r"\bfn\s+\w+")
                .unwrap()
                .find(&m.bare[ca..])
                .map_or(m.bare.len(), |e| ca + e.start());
            let call = Regex::new(&format!(r"(?:^|[^:.\w]){f}\s*\(")).unwrap();
            let mut names = Vec::new();
            let mut found = false;
            for c in call.find_iter(&m.bare[ca..end]) {
                let open = ca + c.end() - 1;
                found = true;
                names.extend(resolve(argument(m, open), m, open, consts, depth + 1)?);
            }
            if !found {
                return Err(format!("closure `{f}` is never called with a name"));
            }
            Ok(names)
        }
        (_, Some((_, c))) => resolve(&c, m, at, consts, depth + 1),
        _ => Err(format!(
            "`{arg}` is neither a closure parameter nor a loop variable over a constant"
        )),
    }
}

/// Every environment read in `files` (path, text).
fn scan(files: &[(String, String)]) -> Vec<Read> {
    let masked: Vec<(String, Masked)> = files.iter().map(|(p, t)| (p.clone(), mask(t))).collect();
    let consts = constants(&masked);
    let call = Regex::new(
        r"\benv::(var|var_os|vars|vars_os|temp_dir|home_dir)\s*\(|\b(env|option_env)!\s*\(",
    )
    .unwrap();
    let import =
        Regex::new(r"use\s+std::env::(\{[^}]*\}|var\w*|vars\w*|temp_dir|home_dir)").unwrap();
    let mut out = Vec::new();
    for (file, m) in &masked {
        let line_of = |pos: usize| m.bare[..pos].matches('\n').count() + 1;
        for i in import.find_iter(&m.bare) {
            out.push(Read {
                file: file.clone(),
                line: line_of(i.start()),
                names: vec![],
                problem: Some(format!(
                    "`{}` hides environment reads from this test; call std::env::var / var_os by path",
                    i.as_str()
                )),
            });
        }
        for c in call.captures_iter(&m.bare) {
            let whole = c.get(0).unwrap();
            let open = whole.end() - 1;
            let what = c.get(1).or_else(|| c.get(2)).unwrap().as_str();
            let (names, problem) = match what {
                "vars" | "vars_os" => (
                    vec![],
                    Some(
                        "reads the whole environment; read each name with env::var_os".to_string(),
                    ),
                ),
                "temp_dir" => (vec!["TMPDIR".to_string()], None),
                "home_dir" => (vec!["HOME".to_string()], None),
                _ => match resolve(argument(m, open), m, open, &consts, 0) {
                    Ok(n) => (n, None),
                    Err(e) => (vec![], Some(e)),
                },
            };
            out.push(Read {
                file: file.clone(),
                line: line_of(whole.start()),
                names,
                problem,
            });
        }
    }
    out
}

fn allowed(name: &str) -> Option<&'static str> {
    if name.starts_with("SOUS_CHEF_") {
        return Some("SOUS_CHEF_*");
    }
    ALLOWED
        .iter()
        .find(|(pat, _)| match pat.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == *pat,
        })
        .map(|(pat, _)| *pat)
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn every_environment_read_is_namespaced_or_allowlisted() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    rs_files(&root.join("src"), &mut paths);
    paths.sort();
    let files: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            (
                p.strip_prefix(root).unwrap().display().to_string(),
                std::fs::read_to_string(p).unwrap(),
            )
        })
        .collect();
    let reads = scan(&files);
    assert!(reads.len() >= 5, "the scan found too little: {reads:?}");

    let mut failures = Vec::new();
    let mut used = BTreeSet::new();
    for r in &reads {
        if let Some(p) = &r.problem {
            failures.push(format!("{}:{}: {p}", r.file, r.line));
            continue;
        }
        for n in &r.names {
            match allowed(n) {
                Some(pat) => {
                    used.insert(pat);
                }
                None => failures.push(format!(
                    "{}:{}: reads {n}, which is neither SOUS_CHEF_* nor allowlisted",
                    r.file, r.line
                )),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "environment reads that break the naming rule:\n  {}\n\nFix: give sous-chef's own variable the SOUS_CHEF_ prefix; or, if the foreign name is read on purpose (another tool defines it), add it to ALLOWED in tests/env_names.rs with a one-line reason; or restructure the read so its name is a literal, a constant, or a closure/loop variable this test can follow.",
        failures.join("\n  ")
    );
    // No stale entries: each allowlisted name is still read.
    let stale: Vec<&str> = ALLOWED
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !used.contains(p))
        .collect();
    assert!(
        stale.is_empty(),
        "ALLOWED in tests/env_names.rs lists names nothing reads any more: {stale:?}; remove them"
    );
}

/// The scanner itself, on made-up sources: every form it must follow, and what it refuses.
#[test]
fn the_scan_follows_literals_constants_closures_and_loops() {
    let src = r##"
pub const ONE: &str = "SOUS_CHEF_ONE";
const LIST: [&str; 2] = ["LIST_A", "LIST_B"];
fn a() {
    let _ = std::env::var("LIT"); // env::var("IN_A_COMMENT")
    /* std::env::var("IN_A_BLOCK") */
    let s = "env::var(\"IN_A_STRING\")";
    let r = r#"env::var("IN_A_RAW_STRING")"#;
    let _ = std::env::var(ONE);
    let _ = std::env::var_os(crate::x::ONE);
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let _ = var("CLOSED_A");
    let _ = var(LIST);
    for name in LIST {
        let _ = std::env::var_os(name);
    }
    let _ = std::env::temp_dir();
    let _ = env!("CARGO_PKG_VERSION");
}
fn b(x: &str) {
    let _ = std::env::var(format!("{x}_DYN"));
    let _ = std::env::vars();
    let _ = std::env::var(x);
}
"##;
    let reads = scan(&[("x.rs".into(), src.into())]);
    let names: Vec<String> = reads.iter().flat_map(|r| r.names.clone()).collect();
    assert_eq!(
        names,
        [
            "LIT",
            "SOUS_CHEF_ONE",
            "SOUS_CHEF_ONE",
            "CLOSED_A",
            "LIST_A",
            "LIST_B",
            "LIST_A",
            "LIST_B",
            "TMPDIR",
            "CARGO_PKG_VERSION"
        ],
        "{reads:#?}"
    );
    let problems: Vec<(usize, &str)> = reads
        .iter()
        .filter_map(|r| r.problem.as_deref().map(|p| (r.line, p)))
        .collect();
    assert_eq!(problems.len(), 3, "{problems:#?}");
    assert!(problems[0].1.contains("is not a literal"), "{problems:?}");
    assert!(problems[1].1.contains("whole environment"), "{problems:?}");
    assert!(
        problems[2]
            .1
            .contains("neither a closure parameter nor a loop variable"),
        "{problems:?}"
    );
    assert_eq!(reads.iter().find(|r| r.names == ["LIT"]).unwrap().line, 5);

    // An import that would hide the calls is refused.
    let reads = scan(&[(
        "y.rs".into(),
        "use std::env::var;\nfn c() { let _ = var(\"X\"); }\n".into(),
    )]);
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert!(reads[0].problem.as_deref().unwrap().contains("hides"));

    assert_eq!(allowed("SOUS_CHEF_HOOK_BUDGET_MS"), Some("SOUS_CHEF_*"));
    assert_eq!(allowed("KAS_MACHINE"), Some("KAS_*"));
    assert_eq!(allowed("TMP"), None);
}
