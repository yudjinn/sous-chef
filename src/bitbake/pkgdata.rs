//! Reader for OE-core's package data directory (`PKGDATA_DIR`), written by
//! `do_packagedata` after a build. See the `pkgdata` tool in docs/mcp-tools.md and
//! "Package data" in docs/bitbake-semantics.md.
//!
//! Layout (OE-core `meta/lib/oe/packagedata.py` `emit_pkgdata` on Scarthgap and Wrynose,
//! `meta/classes/package.bbclass` `emit_pkgdata` on Kirkstone; the same on all three):
//!
//! - `<pn>`: `PACKAGES: <the recipe's packages>`;
//! - `runtime/<pkg>`: one `VAR:<pkg>: value` or `VAR: value` line per `PKGDATA_VARS`
//!   variable that is set (`PN PE PV PR PKGE PKGV PKGR LICENSE ... PKG FILES FILES_INFO
//!   ...`), then `FILERPROVIDES*`, `FILERDEPENDS*` and `PKGSIZE:<pkg>: <bytes>`; values are
//!   written with Python's `unicode_escape` codec; `FILES_INFO` is JSON (`{"/path": size}`);
//! - `runtime/<pkg>.packaged`: empty, present when the package was not empty (or
//!   `ALLOW_EMPTY`);
//! - `runtime-reverse/<runtime name>`: a symlink to `../runtime/<pkg>` (the runtime name is
//!   `PKG:<pkg>`, which renaming such as `debian.bbclass` changes), packaged ones only;
//! - `runtime-rprovides/<name>/<pkg>`: a symlink to `runtime/<pkg>` for each `RPROVIDES`
//!   word.
//!
//! Every read goes through the `confine` callback the caller gives (the tool passes
//! `Workspace::confine`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::error::{Error, Result};

/// Python's `codecs.getdecoder("unicode_escape")` on the ASCII text OE-core writes:
/// `\\ \' \" \a \b \f \n \r \t \v`, `\ooo`, `\xhh`, `\uXXXX`, `\UXXXXXXXX`, a backslash
/// before a newline removed; an unknown escape is kept as it is (as Python does).
pub fn decode_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let hex = |from: usize, n: usize| -> Option<char> {
        let digits: String = chars.get(from..from + n)?.iter().collect();
        if digits.len() != n || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        char::from_u32(u32::from_str_radix(&digits, 16).ok()?)
    };
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = chars[i + 1];
        let simple = match n {
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\u{b}'),
            _ => None,
        };
        if let Some(ch) = simple {
            out.push(ch);
            i += 2;
            continue;
        }
        if n == '\n' {
            i += 2;
            continue;
        }
        let (width, kind) = match n {
            'x' => (2, 16),
            'u' => (4, 16),
            'U' => (8, 16),
            '0'..='7' => (0, 8),
            _ => (0, 0),
        };
        if kind == 16 {
            if let Some(ch) = hex(i + 2, width) {
                out.push(ch);
                i += 2 + width;
            } else {
                out.push('\\');
                i += 1;
            }
            continue;
        }
        if kind == 8 {
            let mut j = i + 1;
            let mut v = 0u32;
            while j < chars.len() && j < i + 4 && ('0'..='7').contains(&chars[j]) {
                v = v * 8 + (chars[j] as u32 - '0' as u32);
                j += 1;
            }
            if let Some(ch) = char::from_u32(v) {
                out.push(ch);
            }
            i = j;
            continue;
        }
        out.push('\\');
        out.push(n);
        i += 2;
    }
    out
}

/// `oe.packagedata.read_pkgdatafile`: each line matching `(^.+?):\s+(.*)`, the value
/// decoded ([`decode_value`]), in file order (a later key replaces an earlier one).
pub fn parse_file(text: &str) -> Vec<(String, String)> {
    let Ok(re) = Regex::new(r"^(.+?):\s+(.*)$") else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(c) = re.captures(line) {
            let k = c[1].to_string();
            let v = decode_value(&c[2]);
            if let Some(slot) = out.iter_mut().find(|(kk, _)| *kk == k) {
                slot.1 = v;
            } else {
                out.push((k, v));
            }
        }
    }
    out
}

/// The value of `var` for `pkg` in a runtime file: `VAR:<pkg>` first, else `VAR`
/// (`write_if_exists` writes one or the other).
pub fn get<'a>(kv: &'a [(String, String)], var: &str, pkg: &str) -> Option<&'a str> {
    let full = format!("{var}:{pkg}");
    kv.iter()
        .find(|(k, _)| *k == full)
        .or_else(|| kv.iter().find(|(k, _)| k == var))
        .map(|(_, v)| v.as_str())
}

/// `fnmatch.fnmatchcase`: `*` (any text, `/` included), `?`, `[seq]`, `[!seq]`; the whole
/// name must match; case-sensitive.
pub fn glob_regex(pattern: &str) -> Result<Regex> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut re = String::from("^(?s:");
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                let mut j = i;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    re.push_str("\\[");
                } else {
                    let mut set: String = chars[i..j].iter().collect();
                    i = j + 1;
                    // Characters Rust's class syntax gives a meaning Python's does not.
                    for (from, to) in [("\\", "\\\\"), ("[", "\\["), ("&", "\\&"), ("~", "\\~")] {
                        set = set.replace(from, to);
                    }
                    let set = match set.strip_prefix('!') {
                        Some(rest) => format!("^{rest}"),
                        None if set.starts_with('^') => format!("\\{set}"),
                        None => set,
                    };
                    re.push('[');
                    re.push_str(&set);
                    re.push(']');
                }
            }
            other => re.push_str(&regex::escape(&other.to_string())),
        }
    }
    re.push_str(")$");
    Regex::new(&re).map_err(|e| Error::Invalid {
        msg: format!("bad glob {pattern:?}: {e}"),
    })
}

/// A package data directory and the check every read passes.
pub struct PkgData<'a> {
    pub dir: PathBuf,
    confine: &'a dyn Fn(&Path) -> Result<PathBuf>,
}

/// One package's runtime file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The recipe-space name (`runtime/<name>`).
    pub name: String,
    pub data: Vec<(String, String)>,
    /// `runtime/<name>.packaged` exists.
    pub packaged: bool,
}

impl Package {
    pub fn get(&self, var: &str) -> Option<&str> {
        get(&self.data, var, &self.name)
    }

    /// The runtime name: `PKG:<pkg>`, else the name.
    pub fn runtime_name(&self) -> &str {
        self.get("PKG").unwrap_or(&self.name)
    }

    /// `FILES_INFO` decoded: path to size. `None` when absent or not JSON.
    pub fn files(&self) -> Option<BTreeMap<String, u64>> {
        let v = self.get("FILES_INFO")?;
        let j: serde_json::Value = serde_json::from_str(v).ok()?;
        let obj = j.as_object()?;
        Some(
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                .collect(),
        )
    }
}

/// How a package name was found ([`PkgData::lookup`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// `runtime-rprovides/<name>/<pkg>`: `<name>` is in the `RPROVIDES` of `<pkg>` (or is
    /// `<pkg>` itself).
    RProvides(String),
    /// `runtime-reverse/<name>`: a runtime (possibly renamed) name.
    Reverse,
    /// `runtime/<name>`: a recipe-space name.
    Runtime,
}

impl<'a> PkgData<'a> {
    pub fn new(dir: PathBuf, confine: &'a dyn Fn(&Path) -> Result<PathBuf>) -> PkgData<'a> {
        PkgData { dir, confine }
    }

    fn read(&self, p: &Path) -> Option<String> {
        let c = (self.confine)(p).ok()?;
        fs::read(c)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn list(&self, p: &Path) -> Vec<String> {
        let Ok(c) = (self.confine)(p) else {
            return Vec::new();
        };
        let Ok(rd) = fs::read_dir(c) else {
            return Vec::new();
        };
        let mut v: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn exists(&self, p: &Path) -> bool {
        (self.confine)(p).is_ok_and(|c| c.exists())
    }

    /// `runtime/<pkg>` read, by recipe-space name.
    pub fn package(&self, name: &str) -> Option<Package> {
        let p = self.dir.join("runtime").join(name);
        let text = self.read(&p)?;
        Some(Package {
            name: name.to_string(),
            data: parse_file(&text),
            packaged: self.exists(&self.dir.join("runtime").join(format!("{name}.packaged"))),
        })
    }

    /// The package(s) a name means, as `oe-pkgdata-util lookup-recipe` / `package-info`
    /// look it up: the entries of `runtime-rprovides/<name>/` first, else
    /// `runtime-reverse/<name>` (a runtime name), else (sous-chef) `runtime/<name>`.
    pub fn lookup(&self, name: &str) -> Vec<(Package, Found)> {
        let prov = self.dir.join("runtime-rprovides").join(name);
        let mut out = Vec::new();
        for f in self.list(&prov) {
            if let Some(p) = self.package(&f) {
                out.push((p, Found::RProvides(f)));
            }
        }
        if !out.is_empty() {
            return out;
        }
        let rev = self.dir.join("runtime-reverse").join(name);
        if let Ok(c) = (self.confine)(&rev)
            && let Some(file) = c.file_name().and_then(|n| n.to_str())
            && let Some(p) = self.package(file)
        {
            return vec![(p, Found::Reverse)];
        }
        if let Some(p) = self.package(name) {
            return vec![(p, Found::Runtime)];
        }
        Vec::new()
    }

    /// `PKGDATA_DIR/<pn>`'s `PACKAGES`, `None` when the recipe has no package data.
    pub fn recipe_packages(&self, pn: &str) -> Option<Vec<String>> {
        let text = self.read(&self.dir.join(pn))?;
        let kv = parse_file(&text);
        Some(
            kv.iter()
                .find(|(k, _)| k == "PACKAGES")
                .map(|(_, v)| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
        )
    }

    /// Every runtime file name (without `.packaged`), sorted.
    pub fn runtime_names(&self) -> Vec<String> {
        self.list(&self.dir.join("runtime"))
            .into_iter()
            .filter(|n| !n.ends_with(".packaged"))
            .collect()
    }

    /// `oe-pkgdata-util find-path`: every `(package, path)` whose `FILES_INFO` has a path
    /// matching the glob, packages in name order, paths sorted.
    pub fn find_path(&self, glob: &Regex) -> Vec<(Package, String)> {
        let mut out = Vec::new();
        for n in self.runtime_names() {
            let Some(p) = self.package(&n) else {
                continue;
            };
            let Some(files) = p.files() else {
                continue;
            };
            let hits: Vec<String> = files.keys().filter(|f| glob.is_match(f)).cloned().collect();
            for h in hits {
                out.push((p.clone(), h));
            }
        }
        out
    }

    /// Whether the directory looks like package data (has `runtime/`).
    pub fn present(&self) -> bool {
        self.exists(&self.dir.join("runtime"))
    }
}

/// `pkgdata` directories under `<build>/tmp*/pkgdata/`: `(machine dir name, path)`.
pub fn find_dirs(build_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(build_dir) else {
        return out;
    };
    let mut tmps: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("tmp"))
        })
        .collect();
    tmps.sort();
    for t in tmps {
        let Ok(rd) = fs::read_dir(t.join("pkgdata")) else {
            continue;
        };
        let mut v: Vec<(String, PathBuf)> = rd
            .flatten()
            .filter(|e| e.path().join("runtime").is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_escape() {
        assert_eq!(decode_value(r"a\\b"), r"a\b");
        assert_eq!(decode_value(r"caf\xe9"), "café");
        assert_eq!(decode_value(r"€ \U0001F600"), "€ 😀");
        assert_eq!(decode_value(r"line\nnext\ttab"), "line\nnext\ttab");
        assert_eq!(decode_value(r"\q"), r"\q");
        // JSON inside a value: the JSON escape survives, one backslash left.
        let bs = '\\';
        assert_eq!(
            decode_value(&format!("{{\"{bs}{bs}u00e9\": 1}}")),
            format!("{{\"{bs}u00e9\": 1}}")
        );
        assert_eq!(decode_value(r"\101\0"), "A\0");
    }

    #[test]
    fn globs() {
        let re = glob_regex("/usr/bin/*").unwrap();
        assert!(re.is_match("/usr/bin/hello"));
        assert!(re.is_match("/usr/bin/sub/dir"));
        assert!(!re.is_match("/usr/sbin/x"));
        let re = glob_regex("/lib/lib?.so.[0-9]").unwrap();
        assert!(re.is_match("/lib/libc.so.6"));
        assert!(!re.is_match("/lib/libc.so.x"));
        let re = glob_regex("*[!a-z]").unwrap();
        assert!(re.is_match("x1"));
        assert!(!re.is_match("xy"));
        assert!(glob_regex("/a/(b)+").unwrap().is_match("/a/(b)+"));
    }

    #[test]
    fn file_lines() {
        let kv = parse_file("PN: hello\nFILES:hello: /usr/bin/*\nPKG:hello: hello\nnot a line\n");
        assert_eq!(get(&kv, "PN", "hello"), Some("hello"));
        assert_eq!(get(&kv, "FILES", "hello"), Some("/usr/bin/*"));
        assert_eq!(kv.len(), 3);
    }
}
