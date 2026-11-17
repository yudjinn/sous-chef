//! T5.2: the `slice` tool. Each selector, the cap marker, an unknown handle, two selectors.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sous_chef::bitbake::JobTable;
use sous_chef::handles::{Handle, HandleKind, HandleStore};
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::slice::{self, Args, CAP_BYTES};
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

struct Fixture {
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn fixture() -> Fixture {
    let cache = tempfile::tempdir().unwrap();
    let config = common::fixture_config();
    let workspace = Workspace::discover(&config).unwrap();
    let index = Index::open_in(cache.path(), &workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    Fixture { _cache: cache, ctx }
}

impl Fixture {
    fn put(&self, kind: HandleKind, text: &str) -> Handle {
        self.ctx
            .handles
            .put(kind, "bitbake test", text.as_bytes())
            .unwrap()
    }
}

fn args(h: &Handle) -> Args {
    Args {
        handle: h.0.clone(),
        lines: None,
        grep: None,
        context: None,
        var: None,
        failures: None,
        page: None,
        budget_tokens: None,
        format: None,
    }
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// The `text` column of every row (after the header, before the notes).
fn texts(out: &str) -> Vec<String> {
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("line\ttext"), "{out}");
    lines
        .filter_map(|l| l.split_once('\t').map(|(_, t)| t.to_string()))
        .collect()
}

fn line_numbers(out: &str) -> Vec<String> {
    out.lines()
        .skip(1)
        .filter_map(|l| l.split_once('\t').map(|(n, _)| n.to_string()))
        .collect()
}

async fn run(f: &Fixture, a: Args) -> String {
    slice::run(&f.ctx, a).await.unwrap()
}

async fn run_err(f: &Fixture, a: Args) -> String {
    slice::run(&f.ctx, a).await.unwrap_err().to_string()
}

#[tokio::test]
async fn lines_selects_a_range() {
    let f = fixture();
    let h = f.put(HandleKind::Text, &numbered(50));
    let out = run(
        &f,
        Args {
            lines: Some("10-12".into()),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(texts(&out), ["line 10", "line 11", "line 12"]);
    assert_eq!(line_numbers(&out), ["10", "11", "12"]);
    assert!(out.contains(&h.0), "{out}");
    // An end past the last line is clamped; a start past it is an error.
    let out = run(
        &f,
        Args {
            lines: Some("49-80".into()),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(texts(&out), ["line 49", "line 50"]);
    let e = run_err(
        &f,
        Args {
            lines: Some("51-60".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("50 lines"), "{e}");
    let e = run_err(
        &f,
        Args {
            lines: Some("x".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("bad line range"), "{e}");
}

#[tokio::test]
async fn grep_with_context_groups_matches() {
    let f = fixture();
    let text = "a\nb\nmatch one\nc\nd\ne\nf\nmatch two\ng\n";
    let h = f.put(HandleKind::Log, text);
    let out = run(
        &f,
        Args {
            grep: Some("^match".into()),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(texts(&out), ["match one", "", "match two"]);
    assert_eq!(line_numbers(&out), ["3", "--", "8"]);
    assert!(out.contains("2 matching line(s)"), "{out}");

    let out = run(
        &f,
        Args {
            grep: Some("^match".into()),
            context: Some(1),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(line_numbers(&out), ["2", "3", "4", "--", "7", "8", "9"]);

    // Context wide enough to join the groups: no separator.
    let out = run(
        &f,
        Args {
            grep: Some("^match".into()),
            context: Some(2),
            ..args(&h)
        },
    )
    .await;
    assert!(!line_numbers(&out).contains(&"--".to_string()), "{out}");

    let out = run(
        &f,
        Args {
            grep: Some("nothing".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(texts(&out).is_empty());
    assert!(out.contains("no line matches"), "{out}");

    let e = run_err(
        &f,
        Args {
            grep: Some("(".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("bad grep pattern"), "{e}");
    let e = run_err(
        &f,
        Args {
            context: Some(2),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("context applies only to grep"), "{e}");
}

const ENV_DUMP: &str = "\
#
# $SRC_URI [2 operations]
#   set /src/meta-mine/recipes/hello/hello_1.0.bb:5
#     \"file://a.patch\"
#   :append /src/meta-mine/recipes/hello/hello_1.0.bbappend:2
#     \" file://b.patch\"
# pre-expansion value:
#   \"file://a.patch file://b.patch\"
SRC_URI=\"file://a.patch file://b.patch\"
#
# $SRC_URIX
SRC_URIX=\"no\"
#
# $PATH [1 operations]
#   set /src/conf/bitbake.conf:10
#     \"/usr/bin\"
# pre-expansion value:
#   \"/usr/bin\"
export PATH=\"/usr/bin\"
#
# line: 12, file: /src/meta-core/classes/base.bbclass
do_compile() {
\toe_runmake
}
";

#[tokio::test]
async fn var_selects_history_and_final_assignment() {
    let f = fixture();
    let h = f.put(HandleKind::Env, ENV_DUMP);
    let out = run(
        &f,
        Args {
            var: Some("SRC_URI".into()),
            ..args(&h)
        },
    )
    .await;
    let t = texts(&out);
    assert_eq!(
        t.first().map(String::as_str),
        Some("# $SRC_URI [2 operations]")
    );
    assert_eq!(
        t.last().map(String::as_str),
        Some("SRC_URI=\"file://a.patch file://b.patch\"")
    );
    assert_eq!(t.len(), 8, "{out}");
    assert!(!out.contains("SRC_URIX"), "{out}");

    let out = run(
        &f,
        Args {
            var: Some("PATH".into()),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(
        texts(&out).last().map(String::as_str),
        Some("export PATH=\"/usr/bin\"")
    );

    let out = run(
        &f,
        Args {
            var: Some("do_compile".into()),
            ..args(&h)
        },
    )
    .await;
    let t = texts(&out);
    assert_eq!(
        t.first().map(String::as_str),
        Some("# line: 12, file: /src/meta-core/classes/base.bbclass")
    );
    assert_eq!(t.last().map(String::as_str), Some("}"));

    let e = run_err(
        &f,
        Args {
            var: Some("MISSING".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("not found") && e.contains("MISSING"), "{e}");

    // `var` needs an env handle.
    let log = f.put(HandleKind::Log, ENV_DUMP);
    let e = run_err(
        &f,
        Args {
            var: Some("SRC_URI".into()),
            ..args(&log)
        },
    )
    .await;
    assert!(e.contains("env handle"), "{e}");
}

#[tokio::test]
async fn failures_selects_error_lines() {
    let f = fixture();
    let text = "NOTE: start\n\
                ERROR: hello-1.0-r0 do_compile: oe_runmake failed\n\
                NOTE: middle\n\
                NOTE: more\n\
                ERROR: Task (hello_1.0.bb:do_compile) failed with exit code '1'\n\
                NOTE: end\n";
    for kind in [HandleKind::Log, HandleKind::Build] {
        let h = f.put(kind, text);
        let out = run(
            &f,
            Args {
                failures: Some(true),
                ..args(&h)
            },
        )
        .await;
        // The detector (bitbake::logs) returns each error with its context lines, so the
        // two errors and the lines around them come back; the leading NOTE does not.
        let numbers = line_numbers(&out);
        assert!(numbers.contains(&"2".to_string()), "{out}");
        assert!(numbers.contains(&"5".to_string()), "{out}");
        assert!(!numbers.contains(&"1".to_string()), "{out}");
        assert!(texts(&out).iter().any(|t| t.starts_with("ERROR: Task (")));
    }
    let clean = f.put(HandleKind::Log, "NOTE: all good\n");
    let out = run(
        &f,
        Args {
            failures: Some(true),
            ..args(&clean)
        },
    )
    .await;
    assert!(out.contains("no error blocks found"), "{out}");

    let env = f.put(HandleKind::Env, text);
    let e = run_err(
        &f,
        Args {
            failures: Some(true),
            ..args(&env)
        },
    )
    .await;
    assert!(e.contains("log or build handle"), "{e}");
}

#[tokio::test]
async fn pages_split_the_output_and_point_at_the_next() {
    let f = fixture();
    let h = f.put(HandleKind::Text, &numbered(5000));
    let first = run(&f, args(&h)).await; // no selector: page 1
    let explicit = run(
        &f,
        Args {
            page: Some(1),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(first, explicit);
    assert!(first.len() <= CAP_BYTES, "{}", first.len());
    assert_eq!(line_numbers(&first).first().map(String::as_str), Some("1"));
    let n_first = line_numbers(&first).len();
    assert!(first.contains("page 1/"), "{first}");
    assert!(
        first.contains(&format!("next: slice handle={} page=2", h.0)),
        "{first}"
    );

    let second = run(
        &f,
        Args {
            page: Some(2),
            ..args(&h)
        },
    )
    .await;
    let expect = (n_first + 1).to_string();
    assert_eq!(line_numbers(&second).first(), Some(&expect));

    // The last page has no "next".
    let total: String = first
        .lines()
        .find_map(|l| {
            l.split("page 1/")
                .nth(1)
                .map(|r| r.split(' ').next().unwrap_or("").to_string())
        })
        .unwrap();
    let last: u32 = total.parse().unwrap();
    let out = run(
        &f,
        Args {
            page: Some(last),
            ..args(&h)
        },
    )
    .await;
    assert!(!out.contains("next:"), "{out}");
    assert_eq!(line_numbers(&out).last().map(String::as_str), Some("5000"));
    let e = run_err(
        &f,
        Args {
            page: Some(last + 1),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains(&format!("{last} page(s)")), "{e}");
    let e = run_err(
        &f,
        Args {
            page: Some(0),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("1-based"), "{e}");

    // A short output is one page.
    let small = f.put(HandleKind::Text, "only\n");
    let out = run(&f, args(&small)).await;
    assert!(out.contains("page 1/1"), "{out}");
    assert_eq!(texts(&out), ["only"]);
}

#[tokio::test]
async fn cap_marker_says_what_was_cut_and_how_to_continue() {
    let f = fixture();
    let h = f.put(HandleKind::Log, &numbered(5000));
    let out = run(
        &f,
        Args {
            lines: Some("1-5000".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(out.len() <= CAP_BYTES, "{}", out.len());
    let nums = line_numbers(&out);
    let last_kept: usize = nums.last().unwrap().parse().unwrap();
    assert!(last_kept > 100 && last_kept < 5000, "{last_kept}");
    let marker = out
        .lines()
        .find(|l| l.starts_with("cut "))
        .expect("cut marker");
    assert!(
        marker.contains(&format!(
            "cut {} line(s) from line {}",
            5000 - last_kept,
            last_kept + 1
        )),
        "{marker}"
    );
    assert!(
        marker.contains(&format!(
            "next: slice handle={} lines={}-5000",
            h.0,
            last_kept + 1
        )),
        "{marker}"
    );

    // The suggested call returns the next part.
    let next = run(
        &f,
        Args {
            lines: Some(format!("{}-5000", last_kept + 1)),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(
        line_numbers(&next).first(),
        Some(&(last_kept + 1).to_string())
    );

    // grep is capped the same way.
    let out = run(
        &f,
        Args {
            grep: Some("line".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(out.len() <= CAP_BYTES);
    assert!(out.contains("or narrow the pattern"), "{out}");

    // A smaller budget lowers the cap.
    let out = run(
        &f,
        Args {
            lines: Some("1-5000".into()),
            budget_tokens: Some(300),
            ..args(&h)
        },
    )
    .await;
    assert!(out.len() <= 1200, "{}", out.len());
    assert!(out.contains("over the 1200-byte cap"), "{out}");

    // One huge line is shortened, not dropped.
    let huge = f.put(HandleKind::Log, &"z".repeat(100_000));
    let out = run(&f, args(&huge)).await;
    assert!(out.len() <= CAP_BYTES);
    assert!(out.contains("bytes of this line]"), "{out}");
}

#[tokio::test]
async fn json_format_keeps_rows_and_notes() {
    let f = fixture();
    let h = f.put(HandleKind::Text, &numbered(3));
    let out = run(
        &f,
        Args {
            lines: Some("2-3".into()),
            format: Some(Format::Json),
            ..args(&h)
        },
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["header"], serde_json::json!(["line", "text"]));
    assert_eq!(
        v["rows"],
        serde_json::json!([["2", "line 2"], ["3", "line 3"]])
    );
    assert!(
        v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "lines 2-3")
    );
}

#[tokio::test]
async fn unknown_and_malformed_handles_are_errors() {
    let f = fixture();
    let e = run_err(&f, args(&Handle("h:0123456789".into()))).await;
    assert!(e.contains("h:0123456789"), "{e}");
    let e = run_err(&f, args(&Handle("nope".into()))).await;
    assert!(e.contains("nope"), "{e}");
}

#[tokio::test]
async fn two_selectors_are_rejected() {
    let f = fixture();
    let h = f.put(HandleKind::Log, &numbered(10));
    let e = run_err(
        &f,
        Args {
            lines: Some("1-2".into()),
            grep: Some("x".into()),
            ..args(&h)
        },
    )
    .await;
    assert!(
        e.contains("exactly one selector") && e.contains("lines, grep"),
        "{e}"
    );
    let e = run_err(
        &f,
        Args {
            page: Some(1),
            failures: Some(true),
            ..args(&h)
        },
    )
    .await;
    assert!(e.contains("failures, page"), "{e}");
    // `failures: false` is not a selector.
    let out = run(
        &f,
        Args {
            failures: Some(false),
            lines: Some("1".into()),
            ..args(&h)
        },
    )
    .await;
    assert_eq!(texts(&out), ["line 1"]);
}
