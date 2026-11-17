//! T1.2: truncated and byte-shuffled fixture files never panic, and statement spans are
//! ordered and non-overlapping.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sous_chef::parse::lexer::decode;
use sous_chef::parse::{ParsedFile, StatementKind, parse};

/// Deterministic xorshift generator (no `rand` dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn check(bytes: &[u8], what: &str) {
    let src = decode(bytes);
    let pf: ParsedFile = parse(&src);
    let lines = u32::try_from(src.split_inclusive(['\n', '\r']).count()).unwrap();
    let mut prev: Option<(u32, usize)> = None;
    for s in &pf.statements {
        assert!(
            s.span.start >= 1 && s.span.start <= s.span.end,
            "{what}: {s:?}"
        );
        assert!(
            s.span.end <= lines.max(1),
            "{what}: span past end of file: {s:?}"
        );
        assert!(
            s.bytes.start <= s.bytes.end && s.bytes.end <= src.len(),
            "{what}: {s:?}"
        );
        assert!(
            src.is_char_boundary(s.bytes.start) && src.is_char_boundary(s.bytes.end),
            "{what}: {s:?}"
        );
        if let Some((end_line, end_byte)) = prev {
            assert!(
                end_line < s.span.start,
                "{what}: overlapping spans at {s:?}"
            );
            assert!(
                end_byte < s.bytes.start,
                "{what}: overlapping bytes at {s:?}"
            );
        }
        prev = Some((s.span.end, s.bytes.end));
        if let StatementKind::Function { body, .. } | StatementKind::PyDef { body, .. } = &s.kind {
            let empty = body.start == body.end + 1;
            assert!(
                body.start > s.span.start && (empty || body.end <= s.span.end),
                "{what}: body outside statement: {s:?}"
            );
        }
    }
    for d in &pf.diagnostics {
        assert!(
            d.span.start >= 1 && d.span.start <= d.span.end,
            "{what}: {d:?}"
        );
    }
}

fn fixture_files() -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = common::files_under(&common::fixture_path("syntax"))
        .into_iter()
        .chain(common::files_under(&common::fixture_ws_root()))
        .filter(|p| {
            p.extension().is_some_and(|e| {
                ["bb", "bbappend", "inc", "bbclass", "conf"].contains(&e.to_str().unwrap_or(""))
            })
        })
        .collect();
    files.sort();
    assert!(files.len() > 40, "only {} fixture files", files.len());
    files
        .into_iter()
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect()
}

#[test]
fn unmodified_fixtures_hold_the_invariants() {
    for (name, bytes) in fixture_files() {
        check(&bytes, &name);
    }
}

#[test]
fn every_truncation() {
    for (name, bytes) in fixture_files() {
        for n in 0..=bytes.len() {
            check(&bytes[..n], &format!("{name} truncated to {n}"));
        }
    }
}

#[test]
fn shuffled_bytes_lines_and_mutations() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (name, bytes) in fixture_files() {
        for round in 0..40 {
            // Full byte shuffle (produces invalid UTF-8 and stray `\r`, `{`, `\`).
            let mut b = bytes.clone();
            for i in (1..b.len()).rev() {
                let j = rng.below(i + 1);
                b.swap(i, j);
            }
            check(&b, &format!("{name} byte shuffle {round}"));

            // Line shuffle: keeps lines intact but breaks function/def/continuation structure.
            let mut lines: Vec<&[u8]> = bytes.split_inclusive(|c| *c == b'\n').collect();
            for i in (1..lines.len()).rev() {
                let j = rng.below(i + 1);
                lines.swap(i, j);
            }
            check(&lines.concat(), &format!("{name} line shuffle {round}"));

            // Point mutations with bytes that matter to the lexer and parser.
            let mut b = bytes.clone();
            const SPECIAL: &[u8] = b"\\\r\n{}()#\"'=:[]$ \t\xff\xc3";
            for _ in 0..4 {
                if !b.is_empty() {
                    let i = rng.below(b.len());
                    b[i] = SPECIAL[rng.below(SPECIAL.len())];
                }
            }
            check(&b, &format!("{name} mutation {round}"));
        }
    }
}

#[test]
fn long_lines_parse_in_linear_time() {
    // Would take minutes with a quadratic name split or assignment match.
    let n = 200_000;
    let name = "A_".repeat(n);
    check(
        format!("{name}x = \"1\"\n").as_bytes(),
        "long legacy-looking name",
    );
    let colons = "a:".repeat(n);
    check(
        format!("{colons}x = \"1\"\n").as_bytes(),
        "long override chain",
    );
    let cont = "A = \"x \\\n".repeat(n / 10);
    check(cont.as_bytes(), "many continuation lines");
    let value = format!("A = \"{}\"\n", "x\"".repeat(n));
    check(value.as_bytes(), "many quotes in a value");
}

#[test]
fn pathological_inputs() {
    for src in [
        "",
        "\n",
        "\r",
        "\\",
        "\\\n\\\n\\",
        "}",
        "{",
        "() {",
        "python () {",
        "def",
        "def f(d):",
        "\u{feff}",
        "\u{feff}\\",
        "A = \"",
        "A[",
        "A[] = \"x\"",
        "addtask",
        "addtask  before ",
        "addtask x before",
        "export",
        "unset A[",
        "\u{fffd}\u{fffd} = \"x\"",
        "é = \"x\"",
        "A:${ = \"x\"",
        "A_${_append = \"x\"",
    ] {
        check(src.as_bytes(), &format!("{src:?}"));
    }
}
