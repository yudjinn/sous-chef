//! T3.3: override finalisation, driven by the "Overrides" block of
//! `docs/bitbake-semantics.md` (one table case per row) plus the extra cases listed in
//! `docs/milestones/M3-evaluator.md`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sous_chef::eval::{DataStore, OpKind, OpStatus, VarOp};
use sous_chef::parse::split_var;
use sous_chef::types::{AssignOp, LineSpan, Loc};

use OpStatus::{Applied, InactiveOverride, Removed, Shadowed};

fn loc(line: u32) -> Loc {
    Loc {
        layer: "meta-test".into(),
        path: "recipes-test/t/t_1.0.bb".into(),
        lines: LineSpan::line(line),
    }
}

fn op(ds: &mut DataStore, name: &str, op: &str, value: &str, line: u32) {
    let (name, flag) = match name.split_once('[') {
        Some((n, f)) => (n, Some(f.trim_end_matches(']').to_string())),
        None => (name, None),
    };
    ds.apply(VarOp {
        var: split_var(name, flag),
        op: op.parse::<AssignOp>().unwrap(),
        value: value.into(),
        loc: loc(line),
        via: vec![],
        kind: OpKind::Statement,
    });
}

/// A store with `OVERRIDES` set to `overrides` (line 1), then `ops` from line 2 on.
fn store(overrides: &str, ops: &[(&str, &str, &str)]) -> DataStore {
    let mut ds = DataStore::new();
    op(&mut ds, "OVERRIDES", "=", overrides, 1);
    for (i, (n, o, v)) in ops.iter().enumerate() {
        op(&mut ds, n, o, v, i as u32 + 2);
    }
    ds
}

fn statuses(ds: &DataStore, name: &str) -> Vec<OpStatus> {
    ds.history(name).into_iter().map(|(_, s)| s).collect()
}

fn text(ds: &DataStore, name: &str) -> Option<String> {
    ds.value(name).map(|v| v.text)
}

struct Case {
    /// Which row of the "Overrides" block (or extra case) this is.
    row: &'static str,
    overrides: &'static str,
    ops: &'static [(&'static str, &'static str, &'static str)],
    want: Option<&'static str>,
    /// Status of each op on `VAR`, in order.
    status: &'static [OpStatus],
}

const CASES: &[Case] = &[
    // VAR:append = " x"      appended when the variable is expanded; NO space is added
    Case {
        row: "append: no space added",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:append", "=", "x"),
            ("VAR:append", "=", " y"),
        ],
        want: Some("ax y"),
        status: &[Applied, Applied, Applied],
    },
    Case {
        row: "append: applied at expansion, after later assignments",
        overrides: "foo",
        ops: &[
            ("VAR:append", "=", " x"),
            ("VAR", "=", "a"),
            ("VAR", "+=", "b"),
        ],
        want: Some("a b x"),
        status: &[Applied, Applied, Applied],
    },
    // VAR:prepend = "x "     prepended when expanded; NO space is added
    Case {
        row: "prepend: no space added, later prepends go first",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:prepend", "=", "x"),
            ("VAR:prepend", "=", "y "),
        ],
        want: Some("y xa"),
        status: &[Applied, Applied, Applied],
    },
    // VAR:remove = "x"       removes whitespace-separated items when expanded; applied last
    Case {
        row: "remove: whole words, after appends, whitespace kept",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a x b xx"),
            ("VAR:append", "=", " x"),
            ("VAR:remove", "=", "x"),
        ],
        want: Some("a  b xx "),
        status: &[Applied, Removed, Applied],
    },
    Case {
        row: "remove: applied after expansion",
        overrides: "foo",
        ops: &[
            ("X", "=", "x y"),
            ("VAR", "=", "${X} z"),
            ("VAR:remove", "=", "${Y}"),
            ("Y", "=", "y"),
        ],
        want: Some("x  z"),
        status: &[Applied, Applied],
    },
    // VAR:foo = "x"          if "foo" is in OVERRIDES, this replaces VAR
    Case {
        row: "variant: active replaces VAR",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR:foo", "=", "x")],
        want: Some("x"),
        status: &[Shadowed, Applied],
    },
    Case {
        row: "variant: inactive",
        overrides: "bar",
        ops: &[("VAR", "=", "a"), ("VAR:foo", "=", "x")],
        want: Some("a"),
        status: &[Applied, InactiveOverride],
    },
    Case {
        row: "variant: replaces VAR even if VAR is assigned later",
        overrides: "foo",
        ops: &[("VAR:foo", "=", "x"), ("VAR", "=", "a")],
        want: Some("x"),
        status: &[Applied, Shadowed],
    },
    // VAR:append:foo = " x"  appended to VAR only if "foo" is in OVERRIDES
    Case {
        row: "conditional append: active",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR:append:foo", "=", " x")],
        want: Some("a x"),
        status: &[Applied, Applied],
    },
    Case {
        row: "conditional append: foo inactive",
        overrides: "bar",
        ops: &[("VAR", "=", "a"), ("VAR:append:foo", "=", " x")],
        want: Some("a"),
        status: &[Applied, InactiveOverride],
    },
    Case {
        row: "conditional append: chained condition needs every element",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR:append:foo:bar", "=", " x")],
        want: Some("a"),
        status: &[Applied, InactiveOverride],
    },
    // VAR:foo:append = " x"  appended to the variable VAR:foo (which then replaces VAR if foo
    //                        is active)
    Case {
        row: "variant append: active",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:foo", "=", "b"),
            ("VAR:foo:append", "=", " x"),
        ],
        want: Some("b x"),
        status: &[Shadowed, Applied, Applied],
    },
    Case {
        row: "variant append: inactive",
        overrides: "bar",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:foo", "=", "b"),
            ("VAR:foo:append", "=", " x"),
        ],
        want: Some("a"),
        status: &[Applied, InactiveOverride, InactiveOverride],
    },
    Case {
        row: "variant append: alone, it still replaces VAR",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR:foo:append", "=", " x")],
        want: Some(" x"),
        status: &[Shadowed, Applied],
    },
    // VAR:foo += "x"         operates on VAR:foo at parse time; it does NOT append to VAR
    Case {
        row: "variant +=: active, VAR:foo was unset",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR:foo", "+=", "x")],
        want: Some(" x"),
        status: &[Shadowed, Applied],
    },
    Case {
        row: "variant +=: active, builds on VAR:foo",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:foo", "=", "f"),
            ("VAR:foo", "+=", "x"),
        ],
        want: Some("f x"),
        status: &[Shadowed, Applied, Applied],
    },
    Case {
        row: "variant +=: inactive",
        overrides: "bar",
        ops: &[("VAR", "=", "a"), ("VAR:foo", "+=", "x")],
        want: Some("a"),
        status: &[Applied, InactiveOverride],
    },
    // Extra: two active variants with different priority (rightmost in OVERRIDES wins).
    Case {
        row: "priority: b is right of a",
        overrides: "a:b",
        ops: &[
            ("VAR", "=", "base"),
            ("VAR:a", "=", "A"),
            ("VAR:b", "=", "B"),
        ],
        want: Some("B"),
        status: &[Shadowed, Shadowed, Applied],
    },
    Case {
        row: "priority: a is right of b",
        overrides: "b:a",
        ops: &[
            ("VAR", "=", "base"),
            ("VAR:a", "=", "A"),
            ("VAR:b", "=", "B"),
        ],
        want: Some("A"),
        status: &[Shadowed, Applied, Shadowed],
    },
    Case {
        row: "priority: forcevariable",
        overrides: "qemux86:class-target:forcevariable",
        ops: &[
            ("VAR:forcevariable", "=", "forced"),
            ("VAR:qemux86", "=", "m"),
            ("VAR:class-target", "=", "c"),
        ],
        want: Some("forced"),
        status: &[Applied, Shadowed, Shadowed],
    },
    // Extra: chained overrides.
    Case {
        row: "chained: all elements active",
        overrides: "a:b",
        ops: &[
            ("VAR", "=", "base"),
            ("VAR:a", "=", "A"),
            ("VAR:a:b", "=", "AB"),
        ],
        want: Some("AB"),
        status: &[Shadowed, Shadowed, Applied],
    },
    Case {
        row: "chained: beats its prefix whatever the order",
        overrides: "b:a",
        ops: &[
            ("VAR", "=", "base"),
            ("VAR:a", "=", "A"),
            ("VAR:a:b", "=", "AB"),
        ],
        want: Some("AB"),
        status: &[Shadowed, Shadowed, Applied],
    },
    Case {
        row: "chained: one element inactive",
        overrides: "a",
        ops: &[
            ("VAR", "=", "base"),
            ("VAR:a", "=", "A"),
            ("VAR:a:b", "=", "AB"),
        ],
        want: Some("A"),
        status: &[Shadowed, Applied, InactiveOverride],
    },
    Case {
        row: "chained: only the second element active",
        overrides: "b",
        ops: &[("VAR", "=", "base"), ("VAR:a:b", "=", "AB")],
        want: Some("base"),
        status: &[Applied, InactiveOverride],
    },
    // Extra: `:remove` of an item added by `:append`.
    Case {
        row: "remove an appended item",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:append", "=", " b"),
            ("VAR:remove", "=", "b"),
        ],
        want: Some("a "),
        status: &[Applied, Removed, Applied],
    },
    Case {
        row: "conditional remove of an appended item: inactive",
        overrides: "bar",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR:append", "=", " b"),
            ("VAR:remove:foo", "=", "b"),
        ],
        want: Some("a b"),
        status: &[Applied, Applied, InactiveOverride],
    },
    // Parse-time statuses.
    Case {
        row: "later = shadows earlier",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR", "=", "b")],
        want: Some("b"),
        status: &[Shadowed, Applied],
    },
    Case {
        row: "?= after = does nothing",
        overrides: "foo",
        ops: &[("VAR", "=", "a"), ("VAR", "?=", "b")],
        want: Some("a"),
        status: &[Applied, Shadowed],
    },
    Case {
        row: "??= used only when nothing else set the variable",
        overrides: "foo",
        ops: &[("VAR", "??=", "w1"), ("VAR", "??=", "w2")],
        want: Some("w2"),
        status: &[Shadowed, Applied],
    },
    Case {
        row: "??= ignored by +=",
        overrides: "foo",
        ops: &[("VAR", "??=", "w"), ("VAR", "+=", "x")],
        want: Some(" x"),
        status: &[Shadowed, Applied],
    },
    Case {
        row: "append chain keeps every contributor",
        overrides: "foo",
        ops: &[
            ("VAR", "=", "a"),
            ("VAR", "+=", "b"),
            ("VAR", ".=", "c"),
            ("VAR", "=+", "z"),
        ],
        want: Some("z a bc"),
        status: &[Applied, Applied, Applied, Applied],
    },
];

#[test]
fn overrides_table() {
    let mut failures = Vec::new();
    for c in CASES {
        let ds = store(c.overrides, c.ops);
        let got = text(&ds, "VAR");
        let st = statuses(&ds, "VAR");
        if got.as_deref() != c.want || st != c.status {
            failures.push(format!(
                "{}: value {:?} (want {:?}), statuses {:?} (want {:?})",
                c.row, got, c.want, st, c.status
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn history_order_and_locations() {
    let ds = store(
        "foo",
        &[
            ("VAR", "=", "a"),
            ("OTHER", "=", "o"),
            ("VAR:append:foo", "=", " x"),
        ],
    );
    let h = ds.history("VAR");
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].0.loc, loc(2));
    assert_eq!(h[1].0.loc, loc(4));
    assert_eq!(h[1].0.var.overrides, vec!["append", "foo"]);
}

#[test]
fn unset_shadows_earlier_operations() {
    let mut ds = store("foo", &[("VAR", "=", "a"), ("VAR:append", "=", " x")]);
    ds.unset("VAR", None, loc(10), vec![]);
    op(&mut ds, "VAR", "=", "b", 11);
    assert_eq!(text(&ds, "VAR").as_deref(), Some("b"));
    assert_eq!(statuses(&ds, "VAR"), vec![Shadowed, Shadowed, Applied]);
    assert_eq!(ds.unsets("VAR")[0].loc, loc(10));
}

#[test]
fn unset_detaches_variants() {
    let mut ds = store("foo", &[("VAR", "=", "a"), ("VAR:foo", "=", "f")]);
    ds.unset("VAR", None, loc(10), vec![]);
    op(&mut ds, "VAR", "=", "b", 11);
    // VAR:foo is no longer a variant of VAR (BitBake delVar drops overridedata[VAR]).
    assert_eq!(text(&ds, "VAR").as_deref(), Some("b"));
    assert_eq!(text(&ds, "VAR:foo").as_deref(), Some("f"));
    // Assigning VAR:foo again registers it again.
    op(&mut ds, "VAR:foo", "=", "g", 12);
    assert_eq!(text(&ds, "VAR").as_deref(), Some("g"));
}

#[test]
fn overrides_list_is_expanded_split_and_ordered() {
    let ds = store(
        "${MACHINE}::pn-${PN}:${DISTROOVERRIDES}:forcevariable",
        &[
            ("MACHINE", "=", "qemux86"),
            ("PN", "=", "hello"),
            ("DISTROOVERRIDES", "=", "testdistro"),
        ],
    );
    assert_eq!(
        ds.overrides(),
        vec!["qemux86", "pn-hello", "testdistro", "forcevariable"]
    );
}

#[test]
fn overrides_depending_on_overrides() {
    let ds = store(
        "${MACHINEOVERRIDES}:base",
        &[
            ("MACHINEOVERRIDES", "=", "m"),
            ("MACHINEOVERRIDES:append:base", "=", ":soc"),
            ("VAR", "=", "a"),
            ("VAR:soc", "=", "s"),
        ],
    );
    // First pass (no overrides): "m:base"; second pass (m, base active): "m:soc:base".
    // They differ, so this is the unstable case BitBake rejects.
    let v = ds.value("VAR").unwrap();
    assert!(v.unresolved.contains(&"OVERRIDES".to_string()), "{v:?}");

    let ds = store(
        "${MACHINEOVERRIDES}:base",
        &[
            ("MACHINEOVERRIDES", "=", "m:soc"),
            ("VAR", "=", "a"),
            ("VAR:soc", "=", "s"),
        ],
    );
    assert_eq!(ds.overrides(), vec!["m", "soc", "base"]);
    let v = ds.value("VAR").unwrap();
    assert_eq!(v.text, "s");
    assert!(v.unresolved.is_empty());
}

#[test]
fn python_in_overrides_flags_override_dependent_values_only() {
    let ds = store(
        "${TARGET_OS}:${@d.getVar('TARGET_ARCH').replace('_', '-')}:foo",
        &[
            ("TARGET_OS", "=", "linux"),
            ("PLAIN", "=", "p"),
            ("PLAIN:append", "=", " q"),
            ("VAR", "=", "a"),
            ("VAR:x86-64", "=", "x"),
        ],
    );
    let plain = ds.value("PLAIN").unwrap();
    assert_eq!(plain.text, "p q");
    assert!(!plain.unevaluated_python);
    let v = ds.value("VAR").unwrap();
    // The python entry might be "x86-64": the static answer is flagged, not presented as
    // fact.
    assert_eq!(v.text, "a");
    assert!(v.unevaluated_python);
    let ov = ds.overrides_value().unwrap();
    assert!(ov.unevaluated_python);
}

#[test]
fn undefined_in_overrides_reported_on_dependent_values() {
    let ds = store(
        "layer-${FILE_LAYERNAME}:foo",
        &[("VAR", "=", "a"), ("VAR:layer-meta", "=", "l")],
    );
    let v = ds.value("VAR").unwrap();
    assert_eq!(v.text, "a");
    assert_eq!(v.unresolved, vec!["FILE_LAYERNAME"]);
}

#[test]
fn expansion_uses_finalised_values() {
    let ds = store(
        "foo",
        &[
            ("A", "=", "${B}/${C}"),
            ("B", "=", "b"),
            ("B:append", "=", "+"),
            ("C", "=", "c"),
            ("C:foo", "=", "cfoo"),
        ],
    );
    assert_eq!(text(&ds, "A").as_deref(), Some("b+/cfoo"));
    assert_eq!(ds.expand("${A}-${C}").text, "b+/cfoo-cfoo");
}

#[test]
fn value_flags_python_and_unresolved() {
    let ds = store(
        "foo",
        &[
            ("A", "=", "${@d.getVar('X')} ${MISSING}"),
            ("A:append", "=", " ${B}"),
            ("B", "=", "b"),
        ],
    );
    let v = ds.value("A").unwrap();
    assert_eq!(v.text, "${@d.getVar('X')} ${MISSING} b");
    assert!(v.unevaluated_python);
    assert_eq!(v.unresolved, vec!["MISSING"]);
}

#[test]
fn self_reference_is_reported_not_looped() {
    let ds = store("foo", &[("A", "=", "${A} x")]);
    let v = ds.value("A").unwrap();
    assert_eq!(v.text, "${A} x");
    assert_eq!(v.unresolved, vec!["A"]);
}

#[test]
fn undefined_variable_has_no_value() {
    let ds = store("foo", &[]);
    assert!(ds.value("NOPE").is_none());
    assert!(ds.history("NOPE").is_empty());
}

#[test]
fn remove_on_empty_value_is_a_no_op() {
    let ds = store("foo", &[("VAR", "=", ""), ("VAR:remove", "=", "x")]);
    assert_eq!(text(&ds, "VAR").as_deref(), Some(""));
}

#[test]
fn variant_removes_carry_only_when_they_matched_inside_the_variant() {
    // BitBake carries a variant's `:remove` to the final value only if it removed something
    // from the variant's own value.
    let ds = store(
        "foo",
        &[
            ("VAR", "=", "a"),
            ("VAR:foo", "=", "x y"),
            ("VAR:foo:remove", "=", "y"),
            ("VAR:append", "=", " y"),
        ],
    );
    assert_eq!(text(&ds, "VAR").as_deref(), Some("x  "));
    let ds = store(
        "foo",
        &[
            ("VAR", "=", "a"),
            ("VAR:foo", "=", "x y"),
            ("VAR:foo:remove", "=", "z"),
            ("VAR:append", "=", " z"),
        ],
    );
    assert_eq!(text(&ds, "VAR").as_deref(), Some("x y z"));
}

#[test]
fn package_variables_after_key_expansion() {
    let mut ds = store(
        "foo",
        &[
            ("PN", "=", "hello"),
            ("FILES", "=", "/all"),
            ("FILES:${PN}-dev", "=", "/usr/include"),
            ("FILES:${PN}-dev:append", "=", " /usr/lib/*.so"),
            ("FILES:${PN}-dev:remove", "=", "/usr/include"),
        ],
    );
    assert!(ds.expand_keys().is_empty());
    assert_eq!(
        text(&ds, "FILES:hello-dev").as_deref(),
        Some(" /usr/lib/*.so")
    );
    // The name may be given unexpanded.
    assert_eq!(
        text(&ds, "FILES:${PN}-dev").as_deref(),
        Some(" /usr/lib/*.so")
    );
    // A package name is not an active override: FILES itself is untouched.
    assert_eq!(text(&ds, "FILES").as_deref(), Some("/all"));
    assert_eq!(
        statuses(&ds, "FILES"),
        vec![
            Applied,
            InactiveOverride,
            InactiveOverride,
            InactiveOverride
        ]
    );
    // History of the package variable: its own operations only, statuses relative to it.
    assert_eq!(
        statuses(&ds, "FILES:hello-dev"),
        vec![Removed, Applied, Applied]
    );
}

#[test]
fn flags_are_expanded() {
    let ds = store(
        "foo",
        &[
            ("A[doc]", "=", "about ${B}"),
            ("B", "=", "b"),
            ("A[x]", "??=", "w"),
        ],
    );
    assert_eq!(ds.flag("A", "doc").unwrap().text, "about b");
    assert_eq!(ds.flag("A", "x").unwrap().text, "w");
    assert!(ds.flag("A", "nope").is_none());
    // Flags are not values.
    assert!(ds.value("A").is_none());
}

#[test]
fn immediate_sees_overrides() {
    let ds = store(
        "foo",
        &[
            ("B", "=", "b"),
            ("B:foo", "=", "bfoo"),
            ("A", ":=", "${B}"),
            ("B:foo", "=", "later"),
        ],
    );
    assert_eq!(text(&ds, "A").as_deref(), Some("bfoo"));
}

#[test]
fn fork_reuses_configuration() {
    let conf = store(
        "${MACHINE}",
        &[("MACHINE", "=", "qemux86"), ("VAR", "=", "a")],
    );
    assert_eq!(conf.overrides(), vec!["qemux86"]);
    let mut r1 = conf.fork();
    op(&mut r1, "VAR:qemux86", "=", "m", 20);
    let mut r2 = conf.fork();
    op(&mut r2, "MACHINE", "=", "boardx", 20);
    op(&mut r2, "VAR:qemux86", "=", "m", 21);
    assert_eq!(text(&conf, "VAR").as_deref(), Some("a"));
    assert_eq!(text(&r1, "VAR").as_deref(), Some("m"));
    assert_eq!(r2.overrides(), vec!["boardx"]);
    assert_eq!(text(&r2, "VAR").as_deref(), Some("a"));
}
