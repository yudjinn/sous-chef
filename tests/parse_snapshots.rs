//! T1.2: an `insta` snapshot of the parse of every file in `tests/fixtures/syntax/`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;

use sous_chef::parse::{ParsedFile, parse_bytes};

/// One line per statement (`span  bytes  kind`), then one per diagnostic.
fn render(pf: &ParsedFile) -> String {
    let mut out = String::new();
    for s in &pf.statements {
        writeln!(out, "{}\t{:?}\t{:?}", s.span, s.bytes, s.kind).unwrap();
    }
    for d in &pf.diagnostics {
        writeln!(out, "diagnostic {}: {}", d.span, d.message).unwrap();
    }
    out
}

#[test]
fn snapshot_every_syntax_fixture() {
    let dir = common::fixture_path("syntax");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    assert!(files.len() >= 35, "only {} syntax fixtures", files.len());
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).unwrap();
        let rendered = render(&parse_bytes(&bytes));
        insta::assert_snapshot!(name, rendered);
    }
}
