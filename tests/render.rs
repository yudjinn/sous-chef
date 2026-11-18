//! T2.4: rendering. Budget and limit cut-off, empty tables, escaping, JSON parity,
//! freshness line, snippets and value truncation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use sous_chef::index::RefreshReport;
use sous_chef::render::{
    Format, JsonTable, Table, escape_cell, estimate_tokens, freshness_line, python_note, snippet,
    truncate_value, unescape_cell,
};
use sous_chef::types::{LineSpan, Loc};

fn table(n: usize) -> Table {
    let mut t = Table::new(&["name", "pointer", "snippet"]);
    for i in 0..n {
        let name = format!("do_task_{i}");
        let ptr = format!("meta-core:recipes-core/hello/hello_1.0.bb:{}", i + 1);
        t.row(&[&name, &ptr, "oe_runmake install DESTDIR=${D}"]);
    }
    t
}

/// Parses text output into (header, rows, marker, other trailing lines).
fn parse_text(out: &str) -> (Vec<String>, Vec<Vec<String>>, Option<String>) {
    let mut lines = out.lines();
    let header: Vec<String> = lines
        .next()
        .unwrap()
        .split('\t')
        .map(unescape_cell)
        .collect();
    let mut rows = Vec::new();
    let mut marker = None;
    for l in lines {
        if l.starts_with('+') && l.contains(" more ") {
            marker = Some(l.to_string());
        } else {
            rows.push(l.split('\t').map(unescape_cell).collect());
        }
    }
    (header, rows, marker)
}

fn marker_count(marker: &str) -> usize {
    marker[1..].split(' ').next().unwrap().parse().unwrap()
}

#[test]
fn everything_fits_no_marker() {
    let t = table(3);
    let out = t.render(600);
    assert_eq!(out.lines().count(), 4);
    assert!(!out.contains("more"));
    assert!(out.ends_with('\n'));
    assert!(out.starts_with("name\tpointer\tsnippet\n"));
}

#[test]
fn budget_cut_off_marker_fits_inside_budget() {
    let t = table(50);
    let header_and_marker = estimate_tokens(&format!(
        "name\tpointer\tsnippet\n+50 more over budget_tokens (budget_tokens={} shows all)\n",
        t.budget_for_all()
    ));
    for budget in header_and_marker..=1200 {
        let out = t.render(budget);
        assert!(
            estimate_tokens(&out) <= budget,
            "budget {budget}: {} tokens\n{out}",
            estimate_tokens(&out)
        );
        let (_, rows, marker) = parse_text(&out);
        match marker {
            Some(m) => {
                assert!(
                    m.ends_with(&format!(
                        " more over budget_tokens (budget_tokens={} shows all)",
                        t.budget_for_all()
                    )),
                    "{m}"
                );
                assert_eq!(rows.len() + marker_count(&m), 50, "budget {budget}");
                // Maximal: one more row (with a marker one smaller, or none) does not fit.
                let mut bigger = table(rows.len() + 1).render(usize::MAX);
                let left = 50 - rows.len() - 1;
                if left > 0 {
                    bigger.push_str(&format!(
                        "+{left} more over budget_tokens (budget_tokens={} shows all)\n",
                        t.budget_for_all()
                    ));
                }
                assert!(
                    estimate_tokens(&bigger) > budget,
                    "budget {budget}: {} rows kept but {} would fit",
                    rows.len(),
                    rows.len() + 1
                );
            }
            None => assert_eq!(rows.len(), 50, "budget {budget}"),
        }
    }
}

#[test]
fn budget_cut_marker_is_last_line_and_rows_are_a_prefix() {
    let t = table(50);
    let out = t.render(100);
    let last = out.lines().last().unwrap();
    assert!(
        last.starts_with('+')
            && last.ends_with(&format!(
                "more over budget_tokens (budget_tokens={} shows all)",
                t.budget_for_all()
            ))
    );
    // That budget returns every row.
    assert_eq!(t.render(t.budget_for_all()), t.render(usize::MAX));
    let (_, rows, _) = parse_text(&out);
    assert!(!rows.is_empty());
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r[0], format!("do_task_{i}"));
    }
}

#[test]
fn short_last_row_kept_when_all_fit_without_marker() {
    // header "h\n" (2 bytes) + "aaaaa\n" (6 bytes) = 8 bytes = 2 tokens. Dropping the row
    // would need a marker far larger than the row, so everything must be kept.
    let mut t = Table::new(&["h"]);
    t.row(&["aaaaa"]);
    assert_eq!(t.render(2), "h\naaaaa\n");
}

#[test]
fn tiny_budget_emits_header_and_marker_only() {
    let t = table(5);
    let out = t.render(0);
    assert_eq!(
        out,
        format!(
            "name\tpointer\tsnippet\n+5 more over budget_tokens (budget_tokens={} shows all)\n",
            t.budget_for_all()
        )
    );
}

#[test]
fn limit_cut_off() {
    let mut t = table(3);
    t.limited(7, 3);
    let out = t.render(600);
    assert_eq!(out.lines().count(), 5);
    assert_eq!(out.lines().last().unwrap(), "+7 more (limit=3)");

    // Both cuts: one line naming both.
    let mut t = table(40);
    t.limited(10, 40);
    let out = t.render(120);
    assert!(estimate_tokens(&out) <= 120);
    let last = out.lines().last().unwrap();
    let (_, rows, _) = parse_text(&out);
    assert_eq!(
        last,
        format!(
            "+{} more over budget_tokens (budget_tokens={} shows all); +10 more (limit=40)",
            40 - rows.len(),
            t.budget_for_all()
        )
    );

    // more == 0 means no cut.
    let mut t = table(2);
    t.limited(0, 2);
    assert!(!t.render(600).contains("more"));
}

#[test]
fn empty_table() {
    let t = Table::new(&["a", "b"]);
    assert!(t.is_empty());
    assert_eq!(t.render(600), "a\tb\n");
    assert_eq!(t.render(0), "a\tb\n");
    let j = t.to_json(600);
    assert_eq!(j.header, vec!["a", "b"]);
    assert!(j.rows.is_empty());
    assert_eq!(
        (j.more_over_budget, j.more_over_limit, j.limit),
        (0, 0, None)
    );
}

#[test]
fn tabs_and_newlines_inside_cells_are_escaped() {
    let mut t = Table::new(&["name", "value"]);
    t.row(&["A\tB", "line1\nline2\r\nback\\slash"]);
    let out = t.render(600);
    assert_eq!(out.lines().count(), 2, "{out}");
    let row = out.lines().nth(1).unwrap();
    assert_eq!(row, "A\\tB\tline1\\nline2\\r\\nback\\\\slash");
    assert_eq!(row.split('\t').count(), 2);
    let cells: Vec<String> = row.split('\t').map(unescape_cell).collect();
    assert_eq!(cells, vec!["A\tB", "line1\nline2\r\nback\\slash"]);
    assert_eq!(escape_cell("plain"), "plain");
}

#[test]
fn notes_are_kept_and_counted_in_the_budget() {
    let mut t = table(50);
    t.note("note: 3 values depend on python and were not evaluated; use authoritative=true");
    for budget in [60, 100, 300] {
        let out = t.render(budget);
        assert!(estimate_tokens(&out) <= budget, "budget {budget}");
        assert!(out.ends_with("use authoritative=true\n"));
        let marker = out.lines().rev().nth(1).unwrap();
        assert!(
            marker.contains("more over budget_tokens (budget_tokens="),
            "{marker}"
        );
    }
    let mut t = Table::new(&["a"]);
    t.note("two\nlines");
    assert_eq!(t.render(600), "a\ntwo\\nlines\n");
}

#[test]
fn json_parity_with_text() {
    let mut base = table(30);
    base.row(&["tab\there", "new\nline", "back\\slash"]);
    base.limited(4, 31);
    base.note("note: x");
    for budget in [0, 30, 80, 150, 400, 5000] {
        let text = base.render(budget);
        let (header, rows, marker) = parse_text(&text);
        let json: JsonTable = serde_json::from_str(&base.render_json(budget)).unwrap();
        assert_eq!(json, base.to_json(budget));
        assert_eq!(json.header, header);
        // parse_text treats the note as a row; drop it for comparison.
        let rows: Vec<Vec<String>> = rows.into_iter().filter(|r| r[0] != "note: x").collect();
        assert_eq!(json.rows, rows, "budget {budget}");
        assert_eq!(json.notes, vec!["note: x"]);
        assert_eq!(json.more_over_limit, 4);
        assert_eq!(json.limit, Some(31));
        let budget_cut = marker
            .as_deref()
            .filter(|m| m.contains("over budget_tokens"))
            .map_or(0, marker_count);
        assert_eq!(json.more_over_budget, budget_cut, "budget {budget}");
        assert_eq!(json.rows.len() + json.more_over_budget, 31);
    }
    // render_as dispatches on Format.
    assert_eq!(base.render_as(Format::Text, 600), base.render(600));
    assert_eq!(base.render_as(Format::Json, 600), base.render_json(600));
    // Raw (unescaped) values in JSON.
    let json = base.to_json(5000);
    assert_eq!(json.rows[30], vec!["tab\there", "new\nline", "back\\slash"]);
}

fn loc(layer: &str, path: &str) -> Loc {
    Loc {
        layer: layer.into(),
        path: PathBuf::from(path),
        lines: LineSpan::line(1),
    }
}

#[test]
fn freshness_line_only_when_something_changed() {
    assert_eq!(freshness_line(&RefreshReport::default()), None);
    let quiet = RefreshReport {
        scanned: 40,
        millis: 3,
        ..RefreshReport::default()
    };
    assert_eq!(freshness_line(&quiet), None);

    let r = RefreshReport {
        scanned: 40,
        reindexed: vec![
            loc("meta-foo", "recipes-bar/baz/baz_1.0.bb"),
            loc("meta-core", "conf/layer.conf"),
        ],
        removed: 0,
        millis: 5,
    };
    assert_eq!(
        freshness_line(&r).unwrap(),
        "reindexed 2 files: meta-foo:recipes-bar/baz/baz_1.0.bb, meta-core:conf/layer.conf"
    );

    let one = RefreshReport {
        reindexed: vec![loc("meta-a", "a.bb")],
        removed: 1,
        ..RefreshReport::default()
    };
    assert_eq!(
        freshness_line(&one).unwrap(),
        "reindexed 1 file: meta-a:a.bb; removed 1 file"
    );

    let many = RefreshReport {
        reindexed: (0..7).map(|i| loc("m", &format!("f{i}.bb"))).collect(),
        ..RefreshReport::default()
    };
    assert_eq!(
        freshness_line(&many).unwrap(),
        "reindexed 7 files: m:f0.bb, m:f1.bb, m:f2.bb, m:f3.bb, m:f4.bb, +2 more"
    );

    let removed = RefreshReport {
        removed: 3,
        ..RefreshReport::default()
    };
    assert_eq!(freshness_line(&removed).unwrap(), "removed 3 files");
}

#[test]
fn snippet_is_one_trimmed_line_of_at_most_120_chars() {
    assert_eq!(snippet("  SRC_URI = \"x\"  \n  more\n"), "SRC_URI = \"x\"");
    assert_eq!(snippet("\n\n  do_install() {\n"), "do_install() {");
    assert_eq!(snippet(""), "");
    let long = "A".repeat(200);
    let s = snippet(&long);
    assert_eq!(s.chars().count(), 120);
    assert!(s.ends_with("..."));
    let exact = "B".repeat(120);
    assert_eq!(snippet(&exact), exact);
    // Multi-byte characters are cut on a character boundary.
    let wide = "é".repeat(300);
    let s = snippet(&wide);
    assert_eq!(s.chars().count(), 120);
    assert!(s.starts_with("ééé"));
}

#[test]
fn value_truncation_with_marker() {
    assert_eq!(truncate_value("short", 600, None), "short");
    let v = "x".repeat(650);
    let t = truncate_value(&v, 600, None);
    assert_eq!(t, format!("{} ... [+50 chars]", "x".repeat(600)));
    let t = truncate_value(&v, 600, Some("h:0123456789"));
    assert_eq!(
        t,
        format!(
            "{} ... [+50 chars; slice handle=h:0123456789]",
            "x".repeat(600)
        )
    );
    let wide = "ü".repeat(10);
    assert_eq!(truncate_value(&wide, 4, None), "üüüü ... [+6 chars]");
}

#[test]
fn python_note_text() {
    assert_eq!(python_note(0), None);
    assert_eq!(
        python_note(2).unwrap(),
        "note: 2 values depend on python and were not evaluated; use authoritative=true"
    );
    assert!(python_note(1).unwrap().starts_with("note: 1 value depends"));
}

#[test]
fn estimate_tokens_rounds_up() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abc"), 1);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
}
