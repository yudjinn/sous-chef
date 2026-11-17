//! T1.1: logical line spans over `tests/fixtures/syntax/`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sous_chef::parse::lexer::{FuncHeader, Lexed, LineKind, decode, lex};
use sous_chef::types::LineSpan;

fn lex_fixture(name: &str) -> (String, Lexed) {
    let bytes = std::fs::read(common::fixture_path(&format!("syntax/{name}"))).unwrap();
    let src = decode(&bytes).into_owned();
    let lexed = lex(&src);
    (src, lexed)
}

fn sp(a: u32, b: u32) -> LineSpan {
    LineSpan::new(a, b)
}

/// (span, short kind) for every logical line.
fn shape(l: &Lexed) -> Vec<(LineSpan, &'static str)> {
    l.lines
        .iter()
        .map(|l| {
            let k = match l.kind {
                LineKind::Blank => "blank",
                LineKind::Comment => "comment",
                LineKind::Function { .. } => "function",
                LineKind::Def { .. } => "def",
                LineKind::Directive => "directive",
                LineKind::Other => "other",
            };
            (l.span, k)
        })
        .collect()
}

#[test]
fn continuation_joins_four_physical_lines() {
    let (src, l) = lex_fixture("continuation.bb");
    assert_eq!(shape(&l), [(sp(1, 4), "other"), (sp(5, 5), "other")]);
    let first = &l.lines[0];
    // The `\` is dropped and nothing is inserted; leading indentation of the next line stays.
    assert_eq!(
        first.text,
        "SRC_URI = \"https://example.org/a.tar.gz            file://one.patch            \
         file://two.patch            \""
    );
    // Bytes run from the start of line 1 to the end of line 4's content.
    assert_eq!(
        &src[first.bytes.clone()],
        src.lines().take(4).collect::<Vec<_>>().join("\n")
    );
    assert!(l.diagnostics.is_empty());
}

#[test]
fn comments_and_blank_lines() {
    let (_, l) = lex_fixture("comments.bb");
    assert_eq!(
        shape(&l),
        [
            (sp(1, 1), "comment"),
            (sp(2, 2), "comment"),
            (sp(3, 3), "comment"),
            (sp(4, 4), "other"),
            (sp(5, 5), "blank"),
            (sp(6, 6), "comment"),
            (sp(7, 7), "blank"),
            (sp(8, 8), "other"),
        ]
    );
    // BitBake only accepts `#` in column 0; the indented comment gets a diagnostic.
    assert_eq!(l.diagnostics.len(), 1);
    assert_eq!(l.diagnostics[0].span, sp(3, 3));
}

#[test]
fn shell_function_body_ignores_inner_braces() {
    let (_, l) = lex_fixture("function-shell.bb");
    assert_eq!(
        shape(&l),
        [
            (sp(1, 6), "function"),
            (sp(7, 7), "blank"),
            (sp(8, 10), "function")
        ]
    );
    match &l.lines[0].kind {
        LineKind::Function {
            header,
            body,
            closed,
        } => {
            assert_eq!(
                header,
                &FuncHeader {
                    name: Some("do_install".into()),
                    python: false,
                    fakeroot: false
                }
            );
            assert_eq!(*body, sp(2, 5));
            assert!(closed);
        }
        k => panic!("{k:?}"),
    }
    assert_eq!(l.lines[0].text, "do_install() {");
}

#[test]
fn python_and_fakeroot_function_headers() {
    let (_, l) = lex_fixture("function-fakeroot.bb");
    let headers: Vec<_> = l
        .lines
        .iter()
        .filter_map(|l| match &l.kind {
            LineKind::Function { header, body, .. } => Some((l.span, header.clone(), *body)),
            _ => None,
        })
        .collect();
    assert_eq!(
        headers,
        [
            (
                sp(1, 3),
                FuncHeader {
                    name: Some("do_bar".into()),
                    python: false,
                    fakeroot: true
                },
                sp(2, 2)
            ),
            (
                sp(5, 8),
                FuncHeader {
                    name: Some("do_baz".into()),
                    python: true,
                    fakeroot: true
                },
                sp(6, 7)
            ),
        ]
    );
}

#[test]
fn def_blocks_include_inner_blank_lines_and_stop_at_column_zero() {
    let (_, l) = lex_fixture("pydef.bb");
    assert_eq!(
        shape(&l),
        [
            (sp(1, 4), "def"),
            (sp(5, 5), "blank"),
            (sp(6, 7), "def"),
            (sp(8, 8), "other"),
        ]
    );
    assert_eq!(
        l.lines[0].kind,
        LineKind::Def {
            name: "helper".into(),
            body: sp(2, 4)
        }
    );
    assert_eq!(
        l.lines[2].kind,
        LineKind::Def {
            name: "second".into(),
            body: sp(7, 7)
        }
    );
}

#[test]
fn crlf_input() {
    let (src, l) = lex_fixture("crlf.bb");
    assert_eq!(
        shape(&l),
        [
            (sp(1, 1), "other"),
            (sp(2, 3), "other"),
            (sp(4, 6), "function")
        ]
    );
    // `\r` never reaches the text, and the continuation still works.
    assert_eq!(l.lines[0].text, "A = \"crlf line endings\"");
    assert_eq!(l.lines[1].text, "B = \"continued      value\"");
    match &l.lines[2].kind {
        LineKind::Function { body, closed, .. } => {
            assert_eq!(*body, sp(5, 5));
            assert!(closed, "`}}\\r` closes the body");
        }
        k => panic!("{k:?}"),
    }
    // Byte ranges exclude the line terminator.
    assert_eq!(&src[l.lines[0].bytes.clone()], "A = \"crlf line endings\"");
    assert!(l.diagnostics.is_empty());
}

#[test]
fn unterminated_body_runs_to_end_of_file() {
    let (_, l) = lex_fixture("malformed-unterminated-function.bb");
    assert_eq!(shape(&l), [(sp(1, 1), "other"), (sp(2, 5), "function")]);
    match &l.lines[1].kind {
        LineKind::Function { body, closed, .. } => {
            assert_eq!(*body, sp(3, 5));
            assert!(!closed);
        }
        k => panic!("{k:?}"),
    }
    assert_eq!(l.diagnostics.len(), 1);
    assert_eq!(l.diagnostics[0].span, sp(2, 5));
    assert!(l.diagnostics[0].message.contains("never closed"));
}

#[test]
fn bad_continuations() {
    let (_, l) = lex_fixture("malformed-bad-continuation.bb");
    // Line 2 ends in `\` plus spaces: BitBake right-strips first, so it still continues.
    assert_eq!(
        shape(&l),
        [
            (sp(1, 1), "other"),
            (sp(2, 3), "other"),
            (sp(4, 4), "other")
        ]
    );
    let d: Vec<_> = l.diagnostics.iter().map(|d| d.span).collect();
    assert_eq!(d, [sp(2, 2), sp(4, 4)]);
    assert!(l.diagnostics[0].message.contains("whitespace after"));
    assert!(l.diagnostics[1].message.contains("end of file"));
}

#[test]
fn bom_and_missing_final_newline() {
    let (src, l) = lex_fixture("bom.bb");
    assert_eq!(shape(&l), [(sp(1, 1), "other")]);
    assert!(l.lines[0].text.starts_with("BOM = "));
    assert_eq!(l.lines[0].bytes.start, 3);
    assert_eq!(l.lines[0].bytes.end, src.trim_end().len());
    assert_eq!(l.diagnostics.len(), 1);

    let (src, l) = lex_fixture("no-final-newline.bb");
    assert_eq!(shape(&l), [(sp(1, 1), "other")]);
    assert_eq!(l.lines[0].bytes, 0..src.len());
}

#[test]
fn directives_are_classified() {
    let (_, l) = lex_fixture("inherit.bb");
    assert!(l.lines.iter().all(|l| l.kind == LineKind::Directive));
    let (_, l) = lex_fixture("addtask.bb");
    assert!(l.lines.iter().all(|l| l.kind == LineKind::Directive));
}

#[test]
fn bodies_are_not_joined_or_comment_checked() {
    // Inside a function `\` does not join and `#` is body text.
    let src = "do_x() {\n\techo a \\\n# not a comment\n}\nA = \"1\"\n";
    let l = lex(src);
    assert_eq!(shape(&l), [(sp(1, 4), "function"), (sp(5, 5), "other")]);
    assert!(l.diagnostics.is_empty());
}

#[test]
fn closing_brace_with_trailing_whitespace_closes() {
    let l = lex("do_x() {\n\t:\n}   \nA = \"1\"\n");
    assert_eq!(shape(&l), [(sp(1, 3), "function"), (sp(4, 4), "other")]);
}

#[test]
fn empty_function_body() {
    let l = lex("do_x() {\n}\n");
    match &l.lines[0].kind {
        LineKind::Function { body, closed, .. } => {
            assert!(closed);
            assert_eq!(
                *body,
                sp(2, 1),
                "empty body is start = header + 1, end = header"
            );
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn def_at_end_of_file_and_column_zero_comment() {
    // `#` in column 0 continues a def body (BitBake `__python_func_regexp__`); trailing
    // blank and comment lines are left out of the body.
    let l = lex("def f(d):\n    a = 1\n# inner\n    return a\n\n# after\nX = \"1\"\n");
    assert_eq!(
        shape(&l),
        [
            (sp(1, 4), "def"),
            (sp(5, 5), "blank"),
            (sp(6, 6), "comment"),
            (sp(7, 7), "other")
        ]
    );
    let l = lex("def f(d):\n    return 1");
    assert_eq!(shape(&l), [(sp(1, 2), "def")]);
}

#[test]
fn comment_continuation() {
    // A comment ending in `\` continues onto the next comment line.
    let l = lex("# a \\\n# b\nA = \"1\"\n");
    assert_eq!(shape(&l), [(sp(1, 2), "comment"), (sp(3, 3), "other")]);
    assert!(l.diagnostics.is_empty());
    // Onto a non-comment line: BitBake rejects it.
    let l = lex("# a \\\nA = \"1\"\n");
    assert_eq!(shape(&l), [(sp(1, 2), "comment")]);
    assert_eq!(l.diagnostics.len(), 1);
    // A comment inside a continued assignment: BitBake rejects it.
    let l = lex("A = \"1 \\\n# c \\\n2\"\n");
    assert_eq!(shape(&l), [(sp(1, 3), "other")]);
    assert_eq!(l.diagnostics.len(), 1);
    assert_eq!(l.diagnostics[0].span, sp(2, 2));
}

#[test]
fn non_utf8_is_decoded_lossily() {
    let bytes = b"A = \"caf\xe9\"\nB = \"ok\"\n";
    let src = decode(bytes);
    let l = lex(&src);
    assert_eq!(shape(&l), [(sp(1, 1), "other"), (sp(2, 2), "other")]);
    assert!(l.lines[0].text.contains('\u{fffd}'));
}
