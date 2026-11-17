//! T1.2: every statement form in docs/bitbake-semantics.md, legacy detection, and recovery.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sous_chef::parse::{
    IncludeMode, Lang, ParsedFile, Statement, StatementKind, parse, parse_bytes,
};
use sous_chef::types::{AssignOp, LineSpan, VarRef};

fn fixture(rel: &str) -> ParsedFile {
    parse_bytes(&std::fs::read(common::fixture_path(rel)).unwrap())
}

fn syntax(name: &str) -> ParsedFile {
    fixture(&format!("syntax/{name}"))
}

fn sp(a: u32, b: u32) -> LineSpan {
    LineSpan::new(a, b)
}

fn ov(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// `(base, overrides, flag, op, value, export)` of an assignment.
type A = (String, Vec<String>, Option<String>, AssignOp, String, bool);

fn assign(s: &Statement) -> A {
    match &s.kind {
        StatementKind::Assign {
            var,
            op,
            value,
            export,
        } => (
            var.base.clone(),
            var.overrides.clone(),
            var.flag.clone(),
            *op,
            value.clone(),
            *export,
        ),
        k => panic!("not an assignment at {}: {k:?}", s.span),
    }
}

fn assigns(pf: &ParsedFile) -> Vec<A> {
    pf.statements.iter().map(assign).collect()
}

fn a(base: &str, ovs: &[&str], flag: Option<&str>, op: AssignOp, value: &str) -> A {
    (
        base.into(),
        ov(ovs),
        flag.map(str::to_string),
        op,
        value.into(),
        false,
    )
}

fn clean(pf: &ParsedFile) {
    assert!(pf.diagnostics.is_empty(), "{:?}", pf.diagnostics);
}

// ---- assignments ----------------------------------------------------------------------

#[test]
fn all_eight_operators() {
    use AssignOp::*;
    let cases = [
        ("assign-set.bb", Set),
        ("assign-default.bb", Default),
        ("assign-weak-default.bb", WeakDefault),
        ("assign-immediate.bb", Immediate),
        ("assign-append.bb", Append),
        ("assign-prepend.bb", Prepend),
        ("assign-append-dot.bb", AppendDot),
        ("assign-prepend-dot.bb", PrependDot),
    ];
    for (file, op) in cases {
        let pf = syntax(file);
        clean(&pf);
        assert!(
            assigns(&pf).iter().any(|x| x.3 == op),
            "{file} has no {op} assignment"
        );
    }
    assert_eq!(
        assigns(&syntax("assign-immediate.bb"))[1],
        a("SNAPSHOT", &[], None, Immediate, "${BASE} two")
    );
    assert_eq!(
        assigns(&syntax("assign-prepend-dot.bb"))[1],
        a("PATHVAR", &[], None, PrependDot, "/usr/")
    );
    assert_eq!(
        assigns(&syntax("assign-append-dot.bb"))[1],
        a("PATHVAR", &[], None, AppendDot, "/bin")
    );
}

#[test]
fn set_spacing_and_empty_value() {
    let pf = syntax("assign-set.bb");
    let v = assigns(&pf);
    assert_eq!(v[1], a("EMPTY", &[], None, AssignOp::Set, ""));
    assert_eq!(
        v[2],
        a(
            "SPACED",
            &[],
            None,
            AssignOp::Set,
            "spaces around the operator"
        )
    );
    assert_eq!(
        v[3],
        a("NOSPACE", &[], None, AssignOp::Set, "no spaces at all")
    );
    let spans: Vec<_> = pf.statements.iter().map(|s| s.span).collect();
    assert_eq!(spans, [sp(1, 1), sp(2, 2), sp(3, 3), sp(4, 4)]);
}

#[test]
fn quotes() {
    let v = assigns(&syntax("assign-quotes.bb"));
    assert_eq!(v[0].4, "single quoted with \"double\" inside");
    assert_eq!(v[1].4, "double quoted with 'single' inside");
    assert_eq!(v[2].4, "a # inside quotes is not a comment");
}

#[test]
fn flags() {
    use AssignOp::*;
    let pf = syntax("assign-flags.bb");
    clean(&pf);
    assert_eq!(
        assigns(&pf),
        [
            a("do_compile", &[], Some("dirs"), Set, "${B}"),
            a(
                "do_compile",
                &[],
                Some("depends"),
                Append,
                "virtual/foo:do_populate_sysroot"
            ),
            a("SRC_URI", &[], Some("sha256sum"), Set, &"0".repeat(64)),
            a(
                "PACKAGECONFIG",
                &[],
                Some("ssl"),
                Set,
                "--with-ssl,--without-ssl,openssl"
            ),
            a("VAR", &[], Some("doc"), Default, "documentation flag"),
            a("do_install", &[], Some("noexec"), Set, "1"),
        ]
    );
}

#[test]
fn names_with_braces_colons_dashes_slashes_dots_plus() {
    let pf = syntax("assign-names.bb");
    clean(&pf);
    let v = assigns(&pf);
    assert_eq!(
        v[0],
        a(
            "FILES",
            &["${PN}-dev"],
            None,
            AssignOp::Set,
            "${includedir}"
        )
    );
    assert_eq!(
        v[1],
        a("RDEPENDS", &["${PN}-ptest"], None, AssignOp::Append, "bash")
    );
    assert_eq!(v[2].0, "PREFERRED_PROVIDER_virtual/kernel");
    assert_eq!(v[3].0, "PREFERRED_VERSION_gcc-cross-${TARGET_ARCH}");
    assert_eq!(v[4].0, "MY.DOTTED+NAME");
    assert!(v.iter().all(|x| x.1.len() <= 1));
}

#[test]
fn export_forms() {
    let pf = syntax("export.bb");
    clean(&pf);
    let k: Vec<_> = pf.statements.iter().map(|s| s.kind.clone()).collect();
    assert!(assign(&pf.statements[0]).5);
    assert_eq!(assign(&pf.statements[0]).0, "CC");
    assert_eq!(
        k[1],
        StatementKind::Export {
            name: "PKG_CONFIG_PATH".into()
        }
    );
    let third = assign(&pf.statements[2]);
    assert_eq!(
        (third.0.as_str(), third.3, third.5),
        ("SPACED_EXPORT", AssignOp::Default, true)
    );
}

#[test]
fn unset_forms() {
    let pf = syntax("unset.bb");
    clean(&pf);
    assert_eq!(
        pf.statements[1].kind,
        StatementKind::Unset {
            name: "FOO".into(),
            flag: None
        }
    );
    assert_eq!(
        pf.statements[3].kind,
        StatementKind::Unset {
            name: "do_compile".into(),
            flag: Some("dirs".into())
        }
    );
}

#[test]
fn override_forms() {
    use AssignOp::*;
    let pf = syntax("overrides.bb");
    clean(&pf);
    assert_eq!(
        assigns(&pf),
        [
            a("VAR", &[], None, Set, "base"),
            a("VAR", &["append"], None, Set, " appended"),
            a("VAR", &["prepend"], None, Set, "prepended "),
            a("VAR", &["remove"], None, Set, "base"),
            a("VAR", &["qemux86"], None, Set, "machine value"),
            a("VAR", &["append", "qemux86"], None, Set, " machine append"),
            a(
                "VAR",
                &["qemux86", "append"],
                None,
                Set,
                " append to the variant"
            ),
            a(
                "VAR",
                &["qemux86"],
                None,
                Append,
                "parse-time append to the variant"
            ),
            a(
                "VAR",
                &["pn-hello", "class-target"],
                None,
                Set,
                "chained overrides"
            ),
            a(
                "FILES",
                &["${PN}-dev", "append"],
                None,
                Set,
                " ${libdir}/cmake"
            ),
        ]
    );
}

#[test]
fn continuation_value_is_joined() {
    let pf = syntax("continuation.bb");
    clean(&pf);
    assert_eq!(pf.statements[0].span, sp(1, 4));
    assert_eq!(
        assign(&pf.statements[0]).4,
        "https://example.org/a.tar.gz            file://one.patch            \
         file://two.patch            "
    );
    assert_eq!(pf.statements[1].span, sp(5, 5));
}

#[test]
fn comments_are_not_statements() {
    let pf = syntax("comments.bb");
    let spans: Vec<_> = pf.statements.iter().map(|s| s.span).collect();
    assert_eq!(spans, [sp(4, 4), sp(8, 8)]);
}

// ---- functions ------------------------------------------------------------------------

/// `(name base, overrides, lang, fakeroot, statement span, body)`.
type F = (
    Option<(String, Vec<String>)>,
    Lang,
    bool,
    LineSpan,
    LineSpan,
);

fn funcs(pf: &ParsedFile) -> Vec<F> {
    pf.statements
        .iter()
        .filter_map(|s| match &s.kind {
            StatementKind::Function {
                name,
                lang,
                fakeroot,
                body,
            } => Some((
                name.as_ref().map(|n| (n.base.clone(), n.overrides.clone())),
                *lang,
                *fakeroot,
                s.span,
                *body,
            )),
            _ => None,
        })
        .collect()
}

fn named(base: &str, ovs: &[&str]) -> Option<(String, Vec<String>)> {
    Some((base.into(), ov(ovs)))
}

#[test]
fn shell_functions() {
    let pf = syntax("function-shell.bb");
    clean(&pf);
    assert_eq!(
        funcs(&pf),
        [
            (
                named("do_install", &[]),
                Lang::Shell,
                false,
                sp(1, 6),
                sp(2, 5)
            ),
            (
                named("helper_function", &[]),
                Lang::Shell,
                false,
                sp(8, 10),
                sp(9, 9)
            ),
        ]
    );
}

#[test]
fn python_functions() {
    let pf = syntax("function-python.bb");
    clean(&pf);
    assert_eq!(
        funcs(&pf),
        [
            (
                named("do_report", &[]),
                Lang::Python,
                false,
                sp(1, 6),
                sp(2, 5)
            ),
            (
                named("do_report", &["append"]),
                Lang::Python,
                false,
                sp(8, 10),
                sp(9, 9)
            ),
        ]
    );
}

#[test]
fn anonymous_functions() {
    let pf = syntax("function-anonymous.bb");
    clean(&pf);
    assert_eq!(
        funcs(&pf),
        [
            (None, Lang::Python, false, sp(1, 3), sp(2, 2)),
            (None, Lang::Python, false, sp(5, 7), sp(6, 6)),
        ]
    );
}

#[test]
fn fakeroot_functions() {
    let pf = syntax("function-fakeroot.bb");
    clean(&pf);
    assert_eq!(
        funcs(&pf),
        [
            (named("do_bar", &[]), Lang::Shell, true, sp(1, 3), sp(2, 2)),
            (named("do_baz", &[]), Lang::Python, true, sp(5, 8), sp(6, 7)),
        ]
    );
}

#[test]
fn override_syntax_in_function_names() {
    let pf = syntax("function-override.bb");
    clean(&pf);
    let names: Vec<_> = funcs(&pf).into_iter().map(|f| f.0).collect();
    assert_eq!(
        names,
        [
            named("do_install", &["append"]),
            named("do_install", &["prepend", "class-target"]),
            named("do_configure", &["qemux86"]),
        ]
    );
}

#[test]
fn python_defs() {
    let pf = syntax("pydef.bb");
    clean(&pf);
    assert_eq!(
        pf.statements[0].kind,
        StatementKind::PyDef {
            name: "helper".into(),
            body: sp(2, 4)
        }
    );
    assert_eq!(pf.statements[0].span, sp(1, 4));
    assert_eq!(
        pf.statements[1].kind,
        StatementKind::PyDef {
            name: "second".into(),
            body: sp(7, 7)
        }
    );
    assert_eq!(assign(&pf.statements[2]).0, "AFTER");
    assert_eq!(pf.statements[2].span, sp(8, 8));
}

#[test]
fn crlf_statements() {
    let pf = syntax("crlf.bb");
    clean(&pf);
    assert_eq!(assign(&pf.statements[0]).4, "crlf line endings");
    assert_eq!(assign(&pf.statements[1]).4, "continued      value");
    assert_eq!(
        funcs(&pf),
        [(named("do_x", &[]), Lang::Shell, false, sp(4, 6), sp(5, 5))]
    );
}

// ---- directives -----------------------------------------------------------------------

#[test]
fn inherit_and_inherit_defer() {
    let pf = syntax("inherit.bb");
    clean(&pf);
    let k: Vec<_> = pf
        .statements
        .iter()
        .map(|s| match &s.kind {
            StatementKind::Inherit { classes, deferred } => (classes.as_str(), *deferred),
            k => panic!("{k:?}"),
        })
        .collect();
    assert_eq!(
        k,
        [
            ("autotools", false),
            ("pkgconfig gettext", false),
            ("${BUILD_SYSTEM_CLASS}", false),
            ("${@'native' if d.getVar('IS_NATIVE') == '1' else ''}", true),
            ("python3native", true),
        ]
    );
}

#[test]
fn include_require_include_all() {
    let pf = syntax("include.bb");
    clean(&pf);
    let k: Vec<_> = pf
        .statements
        .iter()
        .map(|s| match &s.kind {
            StatementKind::Include { path, mode } => (path.as_str(), *mode),
            k => panic!("{k:?}"),
        })
        .collect();
    assert_eq!(
        k,
        [
            ("hello.inc", IncludeMode::Require),
            ("recipes-core/hello/${BPN}-common.inc", IncludeMode::Require),
            ("optional.inc", IncludeMode::Include),
            (
                "${@bb.utils.contains('DISTRO_FEATURES', 'x11', 'x11.inc', '', d)}",
                IncludeMode::Include
            ),
        ]
    );
    let pf = syntax("include-all.conf");
    clean(&pf);
    assert_eq!(
        pf.statements[0].kind,
        StatementKind::Include {
            path: "conf/distro/include/extra.inc".into(),
            mode: IncludeMode::IncludeAll
        }
    );
}

fn task(name: &str, before: &[&str], after: &[&str]) -> StatementKind {
    StatementKind::AddTask {
        name: name.into(),
        before: ov(before),
        after: ov(after),
    }
}

#[test]
fn addtask_and_deltask_forms() {
    let pf = syntax("addtask.bb");
    clean(&pf);
    let k: Vec<_> = pf.statements.iter().map(|s| s.kind.clone()).collect();
    assert_eq!(
        k,
        [
            task("do_deploy", &["do_build"], &["do_install"]),
            task("deploy_extra", &[], &[]),
            task(
                "package_qa",
                &["do_build"],
                &["do_package", "do_packagedata"]
            ),
            task("do_check", &["do_build", "do_image"], &["do_compile"]),
            task("do_after_only", &[], &["do_compile"]),
            StatementKind::DelTask {
                name: "do_configure".into()
            },
            StatementKind::DelTask {
                name: "package_write_rpm".into()
            },
        ]
    );
}

#[test]
fn addtask_bitbake_quirks() {
    // Several task names; a trailing `#` comment is dropped.
    let pf = parse("addtask a b after c # why\n");
    clean(&pf);
    assert_eq!(pf.statements[0].kind, task("a b", &[], &["c"]));
    // Only `before` then only `after`, in that order.
    let pf = parse("addtask x before b after a\n");
    assert_eq!(pf.statements[0].kind, task("x", &["b"], &["a"]));
    // BitBake concatenates two `after` groups that a `before` separates without a space.
    let pf = parse("addtask x after a before b after c\n");
    assert_eq!(pf.statements[0].kind, task("x", &["b"], &["ac"]));
    assert_eq!(pf.diagnostics.len(), 1);
}

#[test]
fn addhandler_and_export_functions() {
    let pf = syntax("addhandler.bb");
    clean(&pf);
    assert_eq!(
        pf.statements[0].kind,
        StatementKind::AddHandler {
            name: "my_handler".into()
        }
    );
    assert_eq!(
        assign(&pf.statements[1]),
        a(
            "my_handler",
            &[],
            Some("eventmask"),
            AssignOp::Set,
            "bb.event.BuildStarted"
        )
    );
    assert_eq!(
        funcs(&pf),
        [(
            named("my_handler", &[]),
            Lang::Python,
            false,
            sp(3, 5),
            sp(4, 4)
        )]
    );

    let pf = syntax("exportfuncs.bbclass");
    clean(&pf);
    assert_eq!(
        pf.statements.last().unwrap().kind,
        StatementKind::ExportFunctions {
            names: ov(&["do_compile", "do_install"])
        }
    );
    assert_eq!(funcs(&pf).len(), 2);
}

#[test]
fn bitbake_match_order() {
    // ConfHandler tries the assignment before `include`/`export`/`unset`...
    let pf = parse("include = \"x\"\nunset ?= \"z\"\nexport_X = \"w\"\n");
    let bases: Vec<_> = pf.statements.iter().map(|s| assign(s).0).collect();
    assert_eq!(bases, ["include", "unset", "export_X"]);
    // `export = "y"` matches with the `export ` prefix and an empty name, which BitBake
    // rejects ("Empty variable name in assignment").
    let pf = parse("export = \"y\"\n");
    assert!(matches!(
        pf.statements[0].kind,
        StatementKind::Unknown { .. }
    ));
    assert_eq!(pf.diagnostics.len(), 1);
    // ...but BBHandler tries `inherit` and `addtask` first.
    let pf = parse("inherit = \"x\"\n");
    assert_eq!(
        pf.statements[0].kind,
        StatementKind::Inherit {
            classes: "= \"x\"".into(),
            deferred: false
        }
    );
    // Keywords need whitespace after them.
    let pf = parse("inherit_deferred = \"1\"\nincludes = \"1\"\n");
    assert_eq!(assign(&pf.statements[0]).0, "inherit_deferred");
    assert_eq!(assign(&pf.statements[1]).0, "includes");
}

// ---- recovery -------------------------------------------------------------------------

#[test]
fn garbage_becomes_unknown_and_the_parser_recovers() {
    let pf = syntax("malformed-garbage.bb");
    let kinds: Vec<_> = pf
        .statements
        .iter()
        .map(|s| {
            (
                s.span.start,
                matches!(s.kind, StatementKind::Unknown { .. }),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (1, false),
            (2, true),
            (3, true),
            (4, true),
            (5, true),
            (6, false)
        ]
    );
    assert_eq!(
        pf.statements[1].kind,
        StatementKind::Unknown {
            text: "this is not bitbake at all".into()
        }
    );
    let diag_lines: Vec<_> = pf.diagnostics.iter().map(|d| d.span.start).collect();
    assert_eq!(diag_lines, [2, 3, 4, 5]);
    assert_eq!(assign(&pf.statements[5]).4, "the parser recovers");
}

#[test]
fn unterminated_function_swallows_the_rest() {
    let pf = syntax("malformed-unterminated-function.bb");
    assert_eq!(pf.statements.len(), 2);
    assert_eq!(
        funcs(&pf),
        [(
            named("do_broken", &[]),
            Lang::Shell,
            false,
            sp(2, 5),
            sp(3, 5)
        )]
    );
    assert_eq!(pf.diagnostics.len(), 1);
}

#[test]
fn bad_continuation() {
    let pf = syntax("malformed-bad-continuation.bb");
    assert_eq!(pf.statements[1].span, sp(2, 3));
    assert_eq!(
        assign(&pf.statements[1]).4,
        "backslash followed by spaces     next line"
    );
    assert!(matches!(
        pf.statements[2].kind,
        StatementKind::Unknown { .. }
    ));
    let diag_lines: Vec<_> = pf.diagnostics.iter().map(|d| d.span.start).collect();
    assert_eq!(diag_lines, [2, 4, 4]);
}

#[test]
fn leading_whitespace_is_unparsed() {
    let pf = parse("  A = \"1\"\n");
    assert!(matches!(
        pf.statements[0].kind,
        StatementKind::Unknown { .. }
    ));
    assert_eq!(pf.diagnostics.len(), 1);
}

#[test]
fn bom_and_no_final_newline() {
    let pf = syntax("bom.bb");
    assert_eq!(assign(&pf.statements[0]).0, "BOM");
    assert_eq!(pf.diagnostics.len(), 1, "BitBake does not strip the BOM");
    let pf = syntax("no-final-newline.bb");
    clean(&pf);
    assert_eq!(assign(&pf.statements[0]).4, "no newline at end of file");
}

#[test]
fn invalid_utf8_never_fails() {
    let pf = parse_bytes(b"A = \"\xff\xfe\"\nB = \"2\"\n");
    assert_eq!(assign(&pf.statements[0]).4, "\u{fffd}\u{fffd}");
    assert_eq!(assign(&pf.statements[1]).4, "2");
}

// ---- legacy syntax --------------------------------------------------------------------

fn var_of(s: &Statement) -> &VarRef {
    match &s.kind {
        StatementKind::Assign { var, .. } => var,
        StatementKind::Function {
            name: Some(var), ..
        } => var,
        k => panic!("{k:?}"),
    }
}

#[test]
fn legacy_true_positives_in_syntax_fixture() {
    let pf = syntax("legacy.bb");
    clean(&pf);
    let at = |line: u32| {
        pf.statements
            .iter()
            .find(|s| s.span.start == line)
            .map(var_of)
            .unwrap()
    };
    let expect = [
        (2, "DEPENDS", ov(&["append"])),
        (3, "SRC_URI", ov(&["prepend"])),
        (4, "EXTRA_OECONF", ov(&["remove"])),
        (5, "RDEPENDS", ov(&["${PN}"])),
        (6, "FILES", ov(&["${PN}-dev"])),
        (7, "EXTRA_OECONF", ov(&["append", "qemux86"])),
        (8, "do_install", ov(&["append"])),
    ];
    for (line, base, ovs) in expect {
        let v = at(line);
        assert!(v.legacy_syntax, "line {line}: {v:?}");
        assert_eq!((v.base.as_str(), &v.overrides), (base, &ovs), "line {line}");
    }
}

#[test]
fn legacy_false_positives_in_syntax_fixture() {
    let pf = syntax("legacy.bb");
    for s in pf.statements.iter().filter(|s| s.span.start >= 13) {
        let v = var_of(s);
        assert!(!v.legacy_syntax, "line {}: {v:?}", s.span.start);
        assert!(v.overrides.is_empty(), "line {}: {v:?}", s.span.start);
    }
    let names: Vec<_> = pf
        .statements
        .iter()
        .filter(|s| s.span.start >= 13)
        .map(|s| var_of(s).base.clone())
        .collect();
    assert_eq!(
        names,
        [
            "SRCREV_machine",
            "BBFILE_PRIORITY_core",
            "LAYERSERIES_COMPAT_core",
            "EXTRA_OECONF",
            "PACKAGE_BEFORE_PN",
            "do_install"
        ]
    );
}

#[test]
fn legacy_layer_fixture() {
    let flagged = |rel: &str| -> Vec<(u32, String)> {
        fixture(rel)
            .statements
            .iter()
            .filter_map(|s| match &s.kind {
                StatementKind::Assign { var, .. }
                | StatementKind::Function {
                    name: Some(var), ..
                } if var.legacy_syntax => Some((s.span.start, var.base.clone())),
                _ => None,
            })
            .collect()
    };
    let l = |xs: &[(u32, &str)]| -> Vec<(u32, String)> {
        xs.iter().map(|(n, s)| (*n, s.to_string())).collect()
    };
    assert_eq!(
        flagged("ws/meta-legacy/recipes-legacy/oldapp/oldapp_1.0.bb"),
        l(&[
            (14, "DEPENDS"),
            (16, "EXTRA_OECONF"),
            (17, "SRC_URI"),
            (19, "RDEPENDS"),
            (20, "FILES"),
            (21, "ALLOW_EMPTY"),
            (27, "do_install"),
            (31, "pkg_postinst"),
        ])
    );
    assert_eq!(
        flagged("ws/meta-legacy/recipes-legacy/oldapp/oldapp_%.bbappend"),
        l(&[(1, "FILESEXTRAPATHS")])
    );
    assert_eq!(
        flagged("ws/meta-legacy/classes/oldclass.bbclass"),
        l(&[(4, "do_configure"), (8, "EXTRA_OECONF")])
    );
    assert!(flagged("ws/meta-legacy/conf/layer.conf").is_empty());
}

#[test]
fn modern_workspace_has_no_legacy_and_no_diagnostics() {
    for path in common::files_under(&common::fixture_ws_root()) {
        let rel = path.to_string_lossy();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !["bb", "bbappend", "inc", "bbclass", "conf"].contains(&ext) {
            continue;
        }
        let pf = parse_bytes(&std::fs::read(&path).unwrap());
        assert!(pf.diagnostics.is_empty(), "{rel}: {:?}", pf.diagnostics);
        if rel.contains("meta-legacy") {
            continue;
        }
        for s in &pf.statements {
            if let StatementKind::Assign { var, .. }
            | StatementKind::Function {
                name: Some(var), ..
            } = &s.kind
            {
                assert!(!var.legacy_syntax, "{rel}:{}: {var:?}", s.span);
            }
        }
    }
}
