//! Checks against the output of the first real build (y1-build, 2026-11-18: `bitbake curl`
//! on the kas build of yocto-kiss; docs/live-verification.md, "The first real build").
//! Every input is a recorded, path-scrubbed copy under `tests/fixtures/outputs/live/y1-build/`
//! (see its README entry); nothing here needs BitBake or the live directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::bitbake::pkgdata::{self, Found, PkgData};
use sous_chef::error::Result;

fn sample_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/outputs/live/y1-build")
}

fn sample(name: &str) -> String {
    fs::read_to_string(sample_dir().join(name)).unwrap()
}

/// What `Workspace::confine` does for a path inside the workspace: the canonical path
/// (symlinks resolved, which the `runtime-reverse/` lookup relies on).
fn open_dir(p: &Path) -> Result<PathBuf> {
    Ok(fs::canonicalize(p)?)
}

/// A path as `oe-pkgdata-util find-path` prints it: it `json.loads` the raw `FILES_INFO`
/// value, so the `\uXXXX` escapes `json.dumps` wrote stay escaped (one backslash).
fn as_oe_prints(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

/// `find_path` over the real package data of curl and ca-certificates gives the same
/// `(package, path)` pairs as `oe-pkgdata-util find-path '*'` (1062 lines), once the one
/// escaped name is compared as oe-pkgdata-util prints it.
#[test]
fn real_pkgdata_find_path_equals_oe_pkgdata_util() {
    let dir = sample_dir().join("pkgdata/dogbonedark");
    let pd = PkgData::new(dir, &open_dir);
    assert!(pd.present());
    let re = pkgdata::glob_regex("*").unwrap();
    let ours: BTreeSet<String> = pd
        .find_path(&re)
        .into_iter()
        .map(|(p, path)| format!("{}: {}", p.name, as_oe_prints(&path)))
        .collect();
    let theirs: BTreeSet<String> = sample("oe-pkgdata-util-find-path.out")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(theirs.len(), 1062);
    assert_eq!(ours, theirs);
    // The decoded name is the file's real name on the target.
    let hit = pd
        .find_path(&pkgdata::glob_regex("*NetLock*").unwrap())
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>();
    assert_eq!(
        hit,
        vec!["/usr/share/ca-certificates/mozilla/NetLock_Arany_=Class_Gold=_Főtanúsítvány.crt"]
    );
}

/// Sizes: `FILES_INFO` sizes equal `oe-pkgdata-util list-pkg-files -l -p curl`, and
/// `PKGSIZE` is their sum (no hard links here).
#[test]
fn real_pkgdata_sizes_equal_list_pkg_files_long() {
    let dir = sample_dir().join("pkgdata/dogbonedark");
    let pd = PkgData::new(dir, &open_dir);
    let mut theirs: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut current = String::new();
    for line in sample("oe-pkgdata-util-list-pkg-files-l-curl.out").lines() {
        if let Some(pkg) = line.strip_suffix(':')
            && !line.starts_with('\t')
        {
            current = pkg.to_string();
            theirs.entry(current.clone()).or_default();
        } else if let Some(rest) = line.strip_prefix('\t') {
            let (path, size) = rest.rsplit_once('\t').unwrap();
            theirs
                .get_mut(&current)
                .unwrap()
                .insert(path.trim_end().to_string(), size.trim().parse().unwrap());
        }
    }
    // list-pkg-files -p lists the packaged packages only.
    assert_eq!(
        theirs.keys().cloned().collect::<Vec<_>>(),
        [
            "curl", "curl-dbg", "curl-dev", "curl-doc", "curl-src", "libcurl"
        ]
    );
    for (pkg, files) in &theirs {
        let p = pd.package(pkg).unwrap();
        assert!(p.packaged, "{pkg}");
        let ours = p.files().unwrap();
        assert_eq!(&ours, files, "{pkg}");
        let sum: u64 = ours.values().sum();
        assert_eq!(p.get("PKGSIZE"), Some(sum.to_string().as_str()), "{pkg}");
    }
}

/// Renamed package and lookups, as `oe-pkgdata-util lookup-recipe`, `package-info` and
/// `list-pkgs -p curl [-r]` answered on the real build.
#[test]
fn real_pkgdata_renamed_libcurl_and_lookups() {
    let dir = sample_dir().join("pkgdata/dogbonedark");
    let pd = PkgData::new(dir, &open_dir);
    // libcurl is renamed by debian.bbclass: PKG:libcurl = libcurl4.
    let found = pd.lookup("libcurl4");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.name, "libcurl");
    assert_eq!(found[0].1, Found::Reverse);
    assert_eq!(found[0].0.runtime_name(), "libcurl4");
    // `libcurl` is in its own RPROVIDES (`RPROVIDES:libcurl:  libcurl (=8.19.0)`): the
    // runtime-rprovides directory is named without the version constraint.
    let found = pd.lookup("libcurl");
    assert_eq!(found[0].1, Found::RProvides("libcurl".into()));
    assert_eq!(found[0].0.get("RPROVIDES"), Some("libcurl (=8.19.0)"));
    for name in ["curl", "libcurl", "libcurl4", "curl-dev"] {
        let p = &pd.lookup(name)[0].0;
        assert_eq!(p.get("PN"), Some("curl"), "{name}");
    }
    // package-info: `<pkg> 8.19.0-r0 curl 8.19.0-r0 <PKGSIZE>`.
    let info = sample("oe-pkgdata-util.out");
    for (name, size) in [
        ("curl", "156605"),
        ("libcurl", "432452"),
        ("libcurl4", "432452"),
    ] {
        assert!(info.contains(&format!("{name} 8.19.0-r0 curl 8.19.0-r0 {size}\n")));
        assert_eq!(pd.lookup(name)[0].0.get("PKGSIZE"), Some(size));
    }
    // list-pkgs -p curl: the packaged ones of PACKAGES; -r gives the runtime names.
    let packages = pd.recipe_packages("curl").unwrap();
    assert_eq!(
        packages,
        [
            "curl-src",
            "curl-dbg",
            "libcurl",
            "curl-staticdev",
            "curl-dev",
            "curl-doc",
            "curl-locale",
            "curl"
        ]
    );
    let mut packaged: Vec<String> = Vec::new();
    let mut runtime: Vec<String> = Vec::new();
    for n in &packages {
        let p = pd.package(n).unwrap();
        if p.packaged {
            packaged.push(n.clone());
            runtime.push(p.runtime_name().to_string());
        }
    }
    packaged.sort();
    runtime.sort();
    assert_eq!(
        packaged,
        [
            "curl", "curl-dbg", "curl-dev", "curl-doc", "curl-src", "libcurl"
        ]
    );
    assert_eq!(
        runtime,
        [
            "curl", "curl-dbg", "curl-dev", "curl-doc", "curl-src", "libcurl4"
        ]
    );
}
