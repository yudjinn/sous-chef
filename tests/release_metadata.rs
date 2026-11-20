//! The version is the same everywhere a release reads it (docs/releasing.md): `Cargo.toml`,
//! `sous-chef --version`, the plugin manifest (checked in `plugin_packaging.rs`) and a
//! `CHANGELOG.md` section, whose body the release workflow uses as the release notes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn version_flag_prints_the_cargo_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("sous-chef {VERSION}\n")
    );
}

#[test]
fn changelog_has_a_section_for_the_version() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("CHANGELOG.md"))
        .unwrap();
    let heading = format!("## [{VERSION}] - ");
    let start = text
        .lines()
        .position(|l| l.starts_with(&heading))
        .unwrap_or_else(|| panic!("CHANGELOG.md has no `{heading}<date>` section"));
    // The section has a body before the next heading, and a link reference at the end.
    let body: Vec<&str> = text
        .lines()
        .skip(start + 1)
        .take_while(|l| !l.starts_with("## "))
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(!body.is_empty(), "the {VERSION} section is empty");
    assert!(
        text.contains(&format!(
            "[{VERSION}]: https://github.com/yudjinn/sous-chef/releases/tag/v{VERSION}"
        )),
        "CHANGELOG.md lacks the link reference for {VERSION}"
    );
}
