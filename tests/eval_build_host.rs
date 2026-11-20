//! build-host: `sous-chef.toml [build_host]` gives the build host's `os.uname()[4]` (`arch`)
//! and `os.uname()[0].lower()` (`os`). With it, the python subset evaluates those two
//! expressions and what OE-core builds from them is certain; without it, they stay
//! unevaluated and flagged, with a cause naming the key to set.
//!
//! The fixture copies the real lines (docs/bitbake-semantics.md, "The host's `os.uname()`"):
//! OE-core `meta/conf/bitbake.conf` (Wrynose lines 137-140, 177-178, 826, 938; the same
//! text on Scarthgap 134-137 and Kirkstone 130-133 for `BUILD_*`)
//!
//! ```text
//! BUILD_ARCH := "${@os.uname()[4]}"
//! BUILD_OS := "${@os.uname()[0].lower()}"
//! BUILD_VENDOR = ""
//! BUILD_SYS = "${BUILD_ARCH}${BUILD_VENDOR}-${BUILD_OS}"
//! SDKMACHINE ??= "${BUILD_ARCH}"
//! SDK_OS = "${BUILD_OS}"
//! include conf/machine-sdk/${SDKMACHINE}.conf
//! TRANSLATED_TARGET_ARCH ??= "${@d.getVar('TARGET_ARCH').replace("_", "-")}"
//! ```
//!
//! and `meta/classes-recipe/native.bbclass` lines 20-21 (`TARGET_ARCH = "${BUILD_ARCH}"`,
//! `TARGET_OS = "${BUILD_OS}"`), into a temporary copy of the fixture workspace, whose
//! `OVERRIDES` starts `${TARGET_OS}:${TRANSLATED_TARGET_ARCH}` as OE-core's does.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::config::BuildHost;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

/// The fixture workspace with the OE-core host lines, a machine-sdk file for `x86_64`, a
/// native-like class and a recipe using it.
fn host_ws() -> (tempfile::TempDir, std::path::PathBuf) {
    let (tmp, root) = temp_ws();
    append(
        &root,
        "meta-core/conf/bitbake.conf",
        "\nBUILD_ARCH := \"${@os.uname()[4]}\"\n\
         BUILD_OS := \"${@os.uname()[0].lower()}\"\n\
         BUILD_VENDOR = \"\"\n\
         BUILD_SYS = \"${BUILD_ARCH}${BUILD_VENDOR}-${BUILD_OS}\"\n\
         SDKMACHINE ??= \"${BUILD_ARCH}\"\n\
         SDK_OS = \"${BUILD_OS}\"\n\
         SDK_ARCH ?= \"unset-sdk-arch\"\n\
         include conf/machine-sdk/${SDKMACHINE}.conf\n\
         TRANSLATED_TARGET_ARCH ??= \"${@d.getVar('TARGET_ARCH').replace(\"_\", \"-\")}\"\n\
         RAW_SYSNAME = \"${@os.uname()[0]}\"\n",
    );
    write(
        &root,
        "meta-core/conf/machine-sdk/x86_64.conf",
        "SDK_ARCH = \"x86_64\"\n",
    );
    write(
        &root,
        "meta-core/classes/hostnative.bbclass",
        "TARGET_ARCH = \"${BUILD_ARCH}\"\nTARGET_OS = \"${BUILD_OS}\"\n",
    );
    write(
        &root,
        "meta-mine/recipes-app/hosttool/hosttool_1.0.bb",
        "LICENSE = \"CLOSED\"\ninherit hostnative\n\
         ON_ARCH = \"generic\"\nON_ARCH:x86-64 = \"on-x86-64\"\n\
         ON_OS = \"generic\"\nON_OS:linux = \"on-linux\"\n",
    );
    (tmp, root)
}

fn evaluate(root: &Path, host: BuildHost, cache: &ConfigCache) -> DataStore {
    let mut cfg = ws_config(root);
    cfg.build_host = host;
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.build_host, cfg.build_host);
    let idx_dir = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(idx_dir.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let t = Target {
        recipe: Some("hosttool".into()),
        ..Target::default()
    };
    eval::evaluate_with(cache, &idx, &ws, &t).unwrap()
}

fn host(arch: Option<&str>, os: Option<&str>) -> BuildHost {
    BuildHost {
        arch: arch.map(str::to_string),
        os: os.map(str::to_string),
    }
}

fn certain(ds: &DataStore, name: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(
        !v.unevaluated_python && v.unresolved.is_empty(),
        "{name} not certain: {v:?}; {:?}",
        ds.flag_causes(name)
    );
    v.text
}

fn flagged(ds: &DataStore, name: &str) -> Vec<String> {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(
        v.unevaluated_python || !v.unresolved.is_empty(),
        "{name} not flagged: {v:?}"
    );
    ds.flag_causes(name)
}

/// Without `[build_host]`: today's behaviour. Every value built from `os.uname()` is
/// flagged, and the cause says which key would give it.
#[test]
fn without_build_host_the_host_values_stay_flagged() {
    let (_tmp, root) = host_ws();
    let ds = evaluate(&root, BuildHost::default(), &ConfigCache::new());
    let causes = flagged(&ds, "BUILD_ARCH");
    assert!(
        causes.iter().any(|c| c.contains("os.uname()[4]")
            && c.contains("give [build_host] arch in sous-chef.toml")),
        "{causes:?}"
    );
    let causes = flagged(&ds, "BUILD_OS");
    assert!(
        causes
            .iter()
            .any(|c| c.contains("give [build_host] os in sous-chef.toml")),
        "{causes:?}"
    );
    for name in [
        "BUILD_SYS",
        "SDKMACHINE",
        "SDK_OS",
        "SDK_ARCH",
        "OVERRIDES",
        "ON_ARCH",
        "ON_OS",
    ] {
        flagged(&ds, name);
    }
    assert!(!ds.replay().var_notes.contains_key("BUILD_ARCH"));
    assert_eq!(certain(&ds, "LICENSE"), "CLOSED");
}

/// With `arch = "x86_64"`, `os = "linux"` (the recorded dumps' `BUILD_ARCH` / `BUILD_OS`):
/// the values are what BitBake computes on such a host: `BUILD_SYS` `x86_64-linux`, the
/// machine-sdk file for `x86_64` read, `OVERRIDES` starting `linux:x86-64`, and the
/// override variants it selects applied. `os.uname()[0]` itself (not lowered) stays
/// flagged: its case is not given.
#[test]
fn with_build_host_the_host_values_are_exact() {
    let (_tmp, root) = host_ws();
    let ds = evaluate(
        &root,
        host(Some("x86_64"), Some("linux")),
        &ConfigCache::new(),
    );
    assert_eq!(certain(&ds, "BUILD_ARCH"), "x86_64");
    assert_eq!(certain(&ds, "BUILD_OS"), "linux");
    assert_eq!(certain(&ds, "BUILD_SYS"), "x86_64-linux");
    assert_eq!(certain(&ds, "SDKMACHINE"), "x86_64");
    assert_eq!(certain(&ds, "SDK_OS"), "linux");
    assert_eq!(certain(&ds, "SDK_ARCH"), "x86_64");
    assert_eq!(certain(&ds, "TARGET_ARCH"), "x86_64");
    assert_eq!(certain(&ds, "TRANSLATED_TARGET_ARCH"), "x86-64");
    let ovr = certain(&ds, "OVERRIDES");
    assert!(ovr.starts_with("linux:x86-64:pn-hosttool:"), "{ovr}");
    assert_eq!(certain(&ds, "ON_ARCH"), "on-x86-64");
    assert_eq!(certain(&ds, "ON_OS"), "on-linux");
    let causes = flagged(&ds, "RAW_SYSNAME");
    assert!(
        causes.iter().any(|c| c.contains("not lowered")),
        "{causes:?}"
    );
    // The `:=` no longer shows the python: a note says where the value came from.
    let notes = &ds.replay().var_notes["BUILD_ARCH"];
    assert!(
        notes.iter().any(|n| n.contains("os.uname()[4]")
            && n.contains("sous-chef.toml [build_host] (arch = \"x86_64\", os = \"linux\")")),
        "{notes:?}"
    );
    assert!(
        ds.replay().var_notes["BUILD_OS"]
            .iter()
            .any(|n| n.contains("os.uname()[0].lower()"))
    );
}

/// Only `arch` given: `BUILD_ARCH` is exact, `BUILD_OS` and what reads it stay flagged.
#[test]
fn each_value_is_used_only_when_given() {
    let (_tmp, root) = host_ws();
    let ds = evaluate(&root, host(Some("aarch64"), None), &ConfigCache::new());
    assert_eq!(certain(&ds, "BUILD_ARCH"), "aarch64");
    assert_eq!(certain(&ds, "SDKMACHINE"), "aarch64");
    // No machine-sdk file for aarch64: the include reads nothing, as in BitBake.
    assert_eq!(certain(&ds, "SDK_ARCH"), "unset-sdk-arch");
    assert_eq!(certain(&ds, "TRANSLATED_TARGET_ARCH"), "aarch64");
    for name in ["BUILD_OS", "BUILD_SYS", "SDK_OS", "OVERRIDES", "ON_OS"] {
        flagged(&ds, name);
    }
    assert!(!ds.replay().var_notes.contains_key("BUILD_OS"));
}

/// The configuration cache keeps passes with different `[build_host]` apart.
#[test]
fn the_configuration_cache_is_per_build_host() {
    let (_tmp, root) = host_ws();
    let cache = ConfigCache::new();
    let a = evaluate(&root, host(Some("x86_64"), Some("linux")), &cache);
    let b = evaluate(&root, BuildHost::default(), &cache);
    let c = evaluate(&root, host(Some("aarch64"), Some("linux")), &cache);
    assert_eq!(certain(&a, "BUILD_SYS"), "x86_64-linux");
    flagged(&b, "BUILD_SYS");
    assert_eq!(certain(&c, "BUILD_SYS"), "aarch64-linux");
    assert_eq!(cache.passes(), 3);
}
