//! The `COMPATIBLE_MACHINE` / `COMPATIBLE_HOST` skip checks against what BitBake really
//! skipped on the live workspace (w10). Ignored by default; returns at once without
//! `SOUS_CHEF_LIVE`. Starts no BitBake: it reads the existing outputs only.
//!
//! The values BitBake computed are used as a check, not as an input to the evaluator:
//! `MACHINEOVERRIDES` and `HOST_SYS` are read from the `bitbake -e` dumps in
//! `$SOUS_CHEF_LIVE/runs/differential/env/` (`curl` for target recipes, `gmp-native` for
//! recipes that inherit `native`, `nativesdk-zlib` for `nativesdk`); every collected recipe
//! of the bitbake-setup build is evaluated statically (machine `dogbonedark`, distro `kiss`)
//! for its own `COMPATIBLE_MACHINE`, `COMPATIBLE_HOST` and `SKIP_RECIPE[PN]`, and the checks
//! the skip notes make (`eval::compatible_machine`, `eval::compatible_host`) are run with the
//! real values. BitBake's answer: a recipe with a dump was parsed, so not skipped; the
//! `bitbake-layers show-appends` output (`runs/differential/show-appends.out`) marks
//! recipes with bbappends `(skipped)` or not; the parse summary says how many were skipped
//! in all (435, variants included, any reason).
//!
//! ```sh
//! SOUS_CHEF_LIVE=~/Development/sous-chef-live cargo test --release --test skip_live -- --ignored --nocapture
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::bitbake::envdump;
use sous_chef::config::Config;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::{Index, query};
use sous_chef::types::FileKind;
use sous_chef::workspace::Workspace;

fn live() -> Option<PathBuf> {
    std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from)
}

fn build_dir(live: &Path) -> PathBuf {
    std::env::var_os("SOUS_CHEF_BUILD")
        .map(PathBuf::from)
        .unwrap_or_else(|| live.join("bitbake-builds/kiss-wrynose-dogbonedark/build"))
}

/// `(MACHINEOVERRIDES, HOST_SYS, FILE)` of a dump.
fn real(live: &Path, target: &str) -> (String, String, String) {
    let text = fs::read_to_string(live.join(format!("runs/differential/env/{target}.env")))
        .unwrap_or_else(|e| panic!("dump of {target}: {e}"));
    let d = envdump::parse_str(&text);
    let get = |n: &str| d.value(n).unwrap_or("").to_string();
    (get("MACHINEOVERRIDES"), get("HOST_SYS"), get("FILE"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Target,
    Native,
    Nativesdk,
    /// cross, crosssdk, cross-canadian: their MACHINEOVERRIDES / HOST_SYS are not in a dump.
    Cross,
}

fn kind(ds: &DataStore) -> Kind {
    let has = |c: &str| {
        ds.files()
            .iter()
            .any(|v| v.kind == FileKind::Class && v.loc.path.file_name().is_some_and(|f| f == c))
    };
    if has("cross.bbclass") || has("crosssdk.bbclass") || has("cross-canadian.bbclass") {
        Kind::Cross
    } else if has("nativesdk.bbclass") {
        Kind::Nativesdk
    } else if has("native.bbclass") {
        Kind::Native
    } else {
        Kind::Target
    }
}

/// A static value: `Some(None)` unset or empty, `None` not known statically.
fn static_text(ds: &DataStore, name: &str) -> Option<Option<String>> {
    if ds.replay().python_vars.contains(name) {
        return None;
    }
    match ds.value(name) {
        None => Some(None),
        Some(v) if v.unevaluated_python || !v.unresolved.is_empty() => None,
        Some(v) if v.text.trim().is_empty() => Some(None),
        Some(v) => Some(Some(v.text)),
    }
}

#[derive(Debug, Default)]
struct Verdict {
    skip: Vec<String>,
    unknown: Vec<String>,
}

/// The checks of OE-core's base.bbclass (Wrynose), with the real values for `kind`.
fn verdict(ds: &DataStore, mo: &str, host: &str) -> Verdict {
    let mut v = Verdict::default();
    if let Some(Some(pn)) = static_text(ds, "PN")
        && let Some(msg) = ds.flag("SKIP_RECIPE", &pn)
        && !msg.unevaluated_python
        && !msg.text.is_empty()
    {
        v.skip.push(format!("SKIP_RECIPE[{pn}]"));
    }
    match static_text(ds, "COMPATIBLE_MACHINE") {
        Some(None) => {}
        None => v.unknown.push("COMPATIBLE_MACHINE not static".into()),
        Some(Some(need)) => match eval::compatible_machine(&need, mo.split(':')) {
            Some(true) => {}
            Some(false) => v.skip.push(format!("COMPATIBLE_MACHINE = \"{need}\"")),
            None => v
                .unknown
                .push(format!("COMPATIBLE_MACHINE = \"{need}\" not compiled")),
        },
    }
    match static_text(ds, "COMPATIBLE_HOST") {
        Some(None) => {}
        None => v.unknown.push("COMPATIBLE_HOST not static".into()),
        Some(Some(need)) => match eval::compatible_host(&need, host) {
            Some(true) => {}
            Some(false) => v.skip.push(format!("COMPATIBLE_HOST = \"{need}\"")),
            None => v
                .unknown
                .push(format!("COMPATIBLE_HOST = \"{need}\" not compiled")),
        },
    }
    v
}

/// `bitbake-layers show-appends`: recipe file name -> skipped.
fn show_appends(live: &Path) -> BTreeMap<String, bool> {
    let text =
        fs::read_to_string(live.join("runs/differential/show-appends.out")).unwrap_or_default();
    let mut out = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if let Some(name) = line.strip_suffix(" (skipped):") {
            out.insert(name.to_string(), true);
        } else if let Some(name) = line.strip_suffix(':')
            && name.ends_with(".bb")
        {
            out.entry(name.to_string()).or_insert(false);
        }
    }
    out
}

#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn skip_checks_agree_with_bitbake() {
    let Some(live) = live() else { return };
    let build = build_dir(&live);
    let mut cfg = Config {
        build_dir: Some(build.clone()),
        work_dir: build,
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    let ws = Workspace::discover(&cfg).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();

    let (target_mo, target_host, _) = real(&live, "curl");
    let (native_mo, native_host, _) = real(&live, "gmp-native");
    let (sdk_mo, sdk_host, _) = real(&live, "nativesdk-zlib");
    println!(
        "real values: target MACHINEOVERRIDES={target_mo:?} HOST_SYS={target_host:?}; native {native_mo:?} {native_host:?}; nativesdk {sdk_mo:?} {sdk_host:?}"
    );
    // Recipes BitBake parsed (a dump exists): their files, canonical.
    let mut parsed: BTreeSet<PathBuf> = BTreeSet::new();
    for e in fs::read_dir(live.join("runs/differential/env"))
        .unwrap()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(target) = name.strip_suffix(".env") else {
            continue;
        };
        let (_, _, file) = real(&live, target);
        // A variant's dump says the variant was parsed, not the base; only target dumps.
        if target.ends_with("-native") || target.starts_with("nativesdk-") || file.is_empty() {
            continue;
        }
        if let Ok(p) = fs::canonicalize(&file) {
            parsed.insert(p);
        }
    }
    let appends = show_appends(&live);

    let cache = ConfigCache::new();
    let recipes =
        query::files_by_kind_in(&idx, &[FileKind::Recipe], query::FileSet::COLLECTED).unwrap();
    let mut by_kind: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut skip: Vec<(String, Kind, Vec<String>)> = Vec::new();
    let mut unknown: Vec<(String, Vec<String>)> = Vec::new();
    let mut failed = Vec::new();
    let mut bugs = Vec::new();
    let mut reverse = Vec::new();
    let mut agree = 0;
    let mut notes_would = 0;
    let mut notes_may = 0;
    for f in &recipes {
        let label = format!("{}:{}", f.layer, f.path.display());
        let t = Target {
            recipe: Some(label.clone()),
            version: None,
            machine: Some("dogbonedark".into()),
            distro: Some("kiss".into()),
        };
        let ds = match eval::evaluate_with(&cache, &idx, &ws, &t) {
            Ok(ds) => ds,
            Err(e) => {
                failed.push(format!("{label}: {e}"));
                continue;
            }
        };
        let k = kind(&ds);
        *by_kind.entry(k).or_default() += 1;
        let (mo, host) = match k {
            Kind::Target => (&target_mo, &target_host),
            Kind::Native => (&native_mo, &native_host),
            Kind::Nativesdk => (&sdk_mo, &sdk_host),
            Kind::Cross => continue,
        };
        let v = verdict(&ds, mo, host);
        // What the notes say today, from static values alone.
        let notes = &ds.replay().target_notes;
        let note_would = notes
            .iter()
            .any(|n| n.starts_with("note: BitBake would skip this recipe"));
        let note_may = notes
            .iter()
            .any(|n| n.starts_with("note: BitBake may skip this recipe"));
        if note_would {
            notes_would += 1;
            if v.skip.is_empty() && v.unknown.is_empty() {
                bugs.push(format!(
                    "{label}: the note says BitBake would skip it, the real values say it is compatible: {notes:?}"
                ));
            }
        } else if note_may {
            notes_may += 1;
        } else if !v.skip.is_empty() {
            bugs.push(format!(
                "{label}: no skip note, but the real values skip it ({:?})",
                v.skip
            ));
        }
        let abs = ws
            .abs_path(&f.loc(sous_chef::types::LineSpan::line(1)))
            .and_then(|p| fs::canonicalize(p).ok());
        let file_name = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let we_skip = !v.skip.is_empty();
        if we_skip && abs.as_ref().is_some_and(|a| parsed.contains(a)) {
            bugs.push(format!(
                "{label}: would skip ({:?}) but BitBake parsed it (bitbake -e dump)",
                v.skip
            ));
        }
        if let Some(&bb_skipped) = appends.get(&file_name) {
            match (we_skip, bb_skipped, v.unknown.is_empty()) {
                (true, false, _) => bugs.push(format!(
                    "{label}: would skip ({:?}) but show-appends lists it as not skipped",
                    v.skip
                )),
                (false, true, true) => reverse.push(format!(
                    "{label}: BitBake skipped it (show-appends) for a reason these checks do not see"
                )),
                (false, true, false) => reverse.push(format!(
                    "{label}: BitBake skipped it; here not known ({:?})",
                    v.unknown
                )),
                _ => agree += 1,
            }
        }
        if we_skip {
            skip.push((label, k, v.skip));
        } else if !v.unknown.is_empty() {
            unknown.push((label, v.unknown));
        }
    }
    println!(
        "collected recipes: {}; evaluated by kind: {by_kind:?}; evaluation failed: {}",
        recipes.len(),
        failed.len()
    );
    for f in &failed {
        println!("  failed: {f}");
    }
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for (_, _, r) in &skip {
        for x in r {
            let key = x.split(" = ").next().unwrap_or(x).to_string();
            *reasons.entry(key).or_default() += 1;
        }
    }
    println!(
        "would skip with BitBake's MACHINEOVERRIDES / HOST_SYS: {} recipes ({reasons:?}); not decidable statically: {}",
        skip.len(),
        unknown.len()
    );
    for (l, k, r) in &skip {
        println!("  skip\t{l}\t{k:?}\t{}", r.join("; "));
    }
    for (l, r) in &unknown {
        println!("  unknown\t{l}\t{}", r.join("; "));
    }
    println!(
        "against BitBake: {} recipes with a bitbake -e dump; show-appends lists {} recipe files ({} skipped), agreement on {agree}",
        parsed.len(),
        appends.len(),
        appends.values().filter(|s| **s).count()
    );
    println!(
        "the notes from static values alone: would skip {notes_would}, may skip {notes_may} (of the target, native and nativesdk recipes)"
    );
    for r in &reverse {
        println!("  reverse\t{r}");
    }
    for b in &bugs {
        println!("  BUG\t{b}");
    }
    assert!(bugs.is_empty(), "{bugs:#?}");
}
