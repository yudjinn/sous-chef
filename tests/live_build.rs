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

// ---------------------------------------------------------------------------------------
// sigs: `EXTRA_OECONF:append = " --disable-dict"` in a scratch bbappend, curl rebuilt.

use sous_chef::bitbake::sigs::{self, Change};

/// The real `bitbake-diffsigs -t curl do_configure` after the change: the task's own
/// basehash and `EXTRA_OECONF`, printed as a unified diff (the value starts with a newline)
/// whose `+` line is the whole value; the word added is `--disable-dict` alone.
#[test]
fn real_diffsigs_configure_unified_diff_word_added() {
    let d = sigs::parse(&sample("sigs/diffsigs-curl-do_configure.out"), "");
    assert_eq!(d.unknown, 0, "{d:?}");
    assert!(d.own_basehash_changed());
    assert_eq!(d.max_depth(), 0);
    let causes = d.causes();
    let Change::VarValue { var, diff, old, .. } = &causes
        .iter()
        .find(|i| matches!(i.change, Change::VarValue { .. }))
        .unwrap()
        .change
    else {
        unreachable!()
    };
    assert_eq!(var, "EXTRA_OECONF");
    assert!(old.is_none());
    assert_eq!(diff[0], "@@ -1,4 +1,4 @@");
    // The context lines are the PACKAGECONFIG words the value's python tests.
    assert_eq!(diff.last().unwrap(), " PACKAGECONFIG{openssl} = Set");
    assert_eq!(
        sigs::word_changes(diff),
        (vec!["--disable-dict".to_string()], Vec::<String>::new())
    );
}

/// `do_compile` and `do_install` re-ran because of `do_configure`: the recursion is
/// indented four spaces per level, the root cause is the same `EXTRA_OECONF` change.
#[test]
fn real_diffsigs_compile_and_install_chain() {
    let d = sigs::parse(&sample("sigs/diffsigs-curl-do_compile.out"), "");
    assert_eq!(d.unknown, 0, "{d:?}");
    assert!(!d.own_basehash_changed());
    assert_eq!(
        d.path(),
        vec![(
            "curl:do_configure",
            "7ee05dbc4c2109710f0305e9cd4e5d8039f415e527fd25750d9eb14c37d610b6",
            "8a1a4041e93f80660ce296a8dc8805c8827549a7be2be702db5fb4c6cc39e6cd"
        )]
    );
    let d = sigs::parse(&sample("sigs/diffsigs-curl-do_install.out"), "");
    assert_eq!(d.unknown, 0, "{d:?}");
    assert_eq!(d.max_depth(), 2);
    let path: Vec<&str> = d.path().iter().map(|p| p.0).collect();
    assert_eq!(path, ["curl:do_compile", "curl:do_configure"]);
    assert_eq!(d.causes()[0].depth, 2);
    let var = d
        .causes()
        .into_iter()
        .find_map(|i| match &i.change {
            Change::VarValue { diff, .. } => Some(sigs::added_words(diff)),
            _ => None,
        })
        .unwrap();
    assert_eq!(var, ["--disable-dict"]);
}

/// When the previous client's server has not exited yet, `bitbake-diffsigs` prints
/// `NOTE: Reconnecting to bitbake server...` and `NOTE: Retrying server connection (#1)...`
/// lines first (seen live: 7 of them); they are not comparison output.
#[test]
fn real_diffsigs_client_notes_are_not_unknown_lines() {
    let text = sample("sigs/diffsigs-curl-do_compile-reconnect.out");
    assert!(text.starts_with("NOTE: Reconnecting to bitbake server..."));
    let d = sigs::parse(&text, "");
    assert_eq!(d.unknown, 0);
    assert_eq!(d.path().len(), 1);
}
