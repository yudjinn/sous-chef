//! T5.5: the `bitbake -e` parser on recorded output (`tests/fixtures/outputs/envdump/`,
//! BitBake 2.18.0), on forms taken from BitBake's `emit_var` that the recording does not
//! show, and on malformed input. Also `compare`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod envdump_support;

use std::path::Path;

use sous_chef::bitbake::envdump::{
    self, Agreement, EnvDump, Export, OpKind, compare, compare_target, location,
};
use sous_chef::eval::Target;
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

use envdump_support::{LiveWs, cut_markers, fixture_at, fixture_text};

const LIVE: &str = "/srv/live";

fn dump() -> EnvDump {
    envdump::parse_str(&fixture_at(Path::new(LIVE)))
}

#[test]
fn header_noise_and_include_history() {
    let text = fixture_text();
    let d = dump();
    assert!(d.stats.found_header);
    // kas's ten log lines, then the cooker's seven (NOTE:, Loading cache..., Parsing...).
    assert_eq!(d.stats.leading, 17);
    // Every unrecognised line is a cut marker of the trimmed fixture.
    assert_eq!(
        d.stats.skipped as usize,
        cut_markers(&text),
        "{:?}",
        d.stats.examples
    );
    assert!(d.stats.examples[0].1.starts_with("# [fixture] "));
    assert_eq!(d.stats.lines as usize, text.lines().count());

    assert_eq!(d.includes.len(), 77);
    assert_eq!(
        d.includes[0].path,
        format!("{LIVE}/yocto-kiss/build/conf/bblayers.conf")
    );
    assert_eq!(d.includes[0].depth, 0);
    let site = d
        .includes
        .iter()
        .find(|i| i.path == "conf/site.conf")
        .unwrap();
    assert_eq!(site.depth, 1);
    // `includes:` is not part of the path.
    assert!(d.includes.iter().all(|i| !i.path.ends_with("includes:")));
    assert!(d.includes.iter().any(|i| i.depth == 8));
}

#[test]
fn final_values() {
    let d = dump();
    assert_eq!(d.variables.len(), 50);
    assert_eq!(d.value("PN"), Some("curl"));
    assert_eq!(d.value("PV"), Some("8.19.0"));
    assert_eq!(d.value("FILE_LAYERNAME"), Some("core"));
    assert_eq!(
        d.value("WORKDIR"),
        Some(
            format!(
                "{LIVE}/yocto-kiss/build/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0"
            )
            .as_str()
        )
    );
    assert_eq!(
        d.value("T"),
        Some(
            format!(
                "{LIVE}/yocto-kiss/build/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0/temp"
            )
            .as_str()
        )
    );
    assert!(
        d.value("S")
            .unwrap()
            .ends_with("/curl/8.19.0/sources/curl-8.19.0")
    );
    assert!(
        d.value("SRC_URI")
            .unwrap()
            .starts_with("https://curl.se/download/curl-8.19.0.tar.xz ")
    );
    assert!(d.value("DEPENDS").unwrap().contains("virtual/compilerlibs"));
    assert_eq!(
        d.overrides(),
        [
            "linux-gnueabi",
            "arm",
            "pn-curl",
            "layer-core",
            "armv7a",
            "ti-soc",
            "ti33x",
            "dogbonedark",
            "kiss",
            "class-target",
            "toolchain-gcc",
            "libc-glibc",
            "forcevariable"
        ]
    );
    // Names with `:` and `/`; override variants are blocks of their own.
    assert_eq!(
        d.value("PREFERRED_PROVIDER_virtual/kernel"),
        Some("linux-kiss")
    );
    assert_eq!(d.value("ABIEXTENSION:class-crosssdk"), Some(""));
    assert_eq!(
        d.value("T:task-clean"),
        Some(format!("{LIVE}/yocto-kiss/build/tmp/log/cleanlogs/curl").as_str())
    );
    assert_eq!(d.value("ALLOW_EMPTY:curl-dbg"), Some("1"));
    assert!(d.value("FILES:curl").unwrap().starts_with("/usr/bin/* "));
    // `${PN}` in a name is expanded with the dump's own values.
    assert_eq!(d.expand("FILES:${PN}"), "FILES:curl");
    assert_eq!(d.expand("${NOPE}-${PN}"), "${NOPE}-curl");
}

#[test]
fn exported_unexported_and_unset() {
    let d = dump();
    let ar = d.variable("AR").unwrap();
    assert_eq!(ar.export, Export::Exported);
    assert_eq!(ar.value.as_deref(), Some("arm-oe-linux-gnueabi-gcc-ar"));
    assert_eq!(ar.ops[0].kind, OpKind::Exported);
    assert_eq!(ar.ops[0].flag.as_deref(), Some("export"));
    assert_eq!(ar.ops[0].file, "ast.py");
    assert_eq!(ar.ops[0].func.as_deref(), Some("eval"));
    assert_eq!(ar.flag_value("export").as_deref(), Some("1"));
    assert_eq!(d.variable("PWD").unwrap().export, Export::Exported);

    // `unset SHELL` is the unexport flag, not a missing variable: no value is printed.
    let sh = d.variable("SHELL").unwrap();
    assert_eq!(sh.export, Export::Unexported);
    assert_eq!(sh.value, None);
    assert_eq!(sh.pre_expansion.as_deref(), Some("/usr/bin/zsh"));
    assert_eq!(sh.ops[0].kind, OpKind::FromEnv);
    assert_eq!(sh.ops[1].kind, OpKind::EnvExport);
    assert_eq!(sh.ops[2].flag.as_deref(), Some("unexport"));
    assert_eq!(sh.ops[3].kind, OpKind::Default);

    // A value of None prints no assignment: every operation is a flag or an override.
    let ae = d.variable("ALLOW_EMPTY").unwrap();
    assert_eq!(ae.value, None);
    assert_eq!(ae.export, Export::No);
    assert_eq!(ae.pre_expansion.as_deref(), Some("None"));
    assert_eq!(ae.ops.len(), 7);
    assert_eq!(ae.ops[1].overrides, ["curl-dbg"]);
    assert_eq!(ae.ops[1].kind, OpKind::Set);
    assert_eq!(
        ae.ops[2].kind,
        OpKind::RenameFrom("ALLOW_EMPTY:${PN}-dbg".into())
    );
    assert_eq!(ae.ops[2].file, "data.py");
    assert_eq!(ae.ops[2].line, Some(97));
    assert_eq!(ae.ops[2].func.as_deref(), Some("expandKeys"));
    // One operation: no count, no pre-expansion value, and here no value either.
    let alt = d.variable("ALTERNATIVE").unwrap();
    assert_eq!(
        (
            alt.ops.len(),
            alt.value.as_deref(),
            alt.pre_expansion.as_deref()
        ),
        (1, None, None)
    );
}

#[test]
fn operations_overrides_and_flags() {
    let d = dump();
    let src = d.variable("SRC_URI").unwrap();
    assert_eq!(src.ops.len(), 6);
    let kinds: Vec<(&OpKind, Option<&str>)> = src
        .ops
        .iter()
        .map(|o| (&o.kind, o.flag.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        [
            (&OpKind::Append, Some("vardepsexclude")),
            (&OpKind::Set, None),
            (&OpKind::Set, Some("doc")),
            (&OpKind::Set, None),
            (&OpKind::OverrideAppend, None),
            (&OpKind::Set, Some("sha256sum")),
        ]
    );
    let app = &src.ops[4];
    assert_eq!(app.condition, ["class-nativesdk"]);
    assert_eq!(app.value, "     file://environment.d-curl.sh ");
    assert_eq!(app.raw, ":append[class-nativesdk]");
    assert_eq!(
        app.file,
        format!(
            "{LIVE}/yocto-kiss/build/../openembedded-core/meta/recipes-support/curl/curl_8.19.0.bb"
        )
    );
    assert_eq!(app.line, Some(46));
    assert_eq!(
        src.flag_value("sha256sum").as_deref(),
        Some("4eb41489790d19e190d7ac7e18e82857cdd68af8f4e66b292ced562d333f11df")
    );
    // An append to a flag gives no derivable value.
    assert_eq!(src.flag_value("vardepsexclude"), None);
    assert_eq!(src.flag_names(), ["doc", "sha256sum", "vardepsexclude"]);
    assert_eq!(src.value_ops().count(), 3);

    // `??=` is a set of the `_defaultval` flag; the parser reports it as a weak default.
    let fl = d.variable("FILE_LAYERNAME").unwrap();
    assert_eq!(fl.ops[0].kind, OpKind::WeakDefault);
    assert_eq!(fl.ops[0].flag, None);
    assert_eq!(fl.ops[0].value, "config");
    assert_eq!(fl.ops[1].file, "cookerdata.py");
    assert_eq!(fl.ops[1].func.as_deref(), Some("_parse_recipe"));

    // PACKAGECONFIG: flags per option, a weak default, override variants.
    let pc = d.variable("PACKAGECONFIG").unwrap();
    assert_eq!(pc.ops.len(), 45);
    assert!(pc.flag_names().len() >= 40);
    assert_eq!(
        pc.flag_value("openssl").as_deref(),
        Some("--with-openssl,--without-openssl,openssl")
    );
    let native: Vec<_> = pc.ops.iter().filter(|o| !o.overrides.is_empty()).collect();
    assert_eq!(native.len(), 2);
    assert_eq!(native[0].overrides, ["class-native"]);
    assert_eq!(native[0].raw, "override[class-native]:set");
    assert_eq!(
        pc.value_ops()
            .filter(|o| o.kind == OpKind::WeakDefault)
            .count(),
        1
    );

    // A long history.
    assert_eq!(d.variable("ARMPKGARCH").unwrap().ops.len(), 97);
    // `:remove[cond]` and a multi-word condition.
    let pf = d.variable("PTESTS_FAST").unwrap();
    assert!(
        pf.ops
            .iter()
            .any(|o| o.kind == OpKind::OverrideRemove && o.condition == ["mips64"])
    );
    let deps = d.variable("DEPENDS").unwrap();
    assert!(
        deps.ops
            .iter()
            .any(|o| o.kind == OpKind::OverridePrepend && o.condition.is_empty())
    );
    assert!(
        deps.ops
            .iter()
            .any(|o| o.func.as_deref() == Some("appendVar") && o.file == "base.bbclass")
    );
}

#[test]
fn functions() {
    let d = dump();
    assert_eq!(d.functions.len(), 6);
    let c = d.function("autotools_do_compile").unwrap();
    assert!(!c.python);
    assert_eq!(c.body, "\toe_runmake");
    assert_eq!(c.line, Some(227));
    assert!(
        c.file
            .as_deref()
            .unwrap()
            .ends_with("classes-recipe/autotools.bbclass")
    );
    let dc = d.function("do_compile").unwrap();
    assert_eq!(dc.file.as_deref(), Some("autogenerated"));
    assert_eq!(dc.line, Some(1));
    assert_eq!(
        dc.body,
        "    # Export function set\n    autotools_do_compile"
    );
    assert!(
        d.function("do_compile_ptest")
            .unwrap()
            .body
            .contains("oe_runmake -C ")
    );

    let f = d.function("base_do_fetch").unwrap();
    assert!(f.python && f.file.is_none() && f.line.is_none());
    assert!(f.body.starts_with("\n    src_uri = "), "{:?}", f.body);
    assert!(f.body.ends_with("repr(e))"), "{:?}", f.body);
    let w = d.function("write_task_data").unwrap();
    assert!(
        w.body
            .starts_with("def write_task_data(status, logfile, e, d):")
    );
    assert!(w.body.ends_with("e.time)"));
    assert!(d.function("autotools_sitefiles").unwrap().python);
}

#[test]
fn spans_cover_each_block() {
    let text = fixture_at(Path::new(LIVE));
    let lines: Vec<&str> = text.lines().collect();
    let d = envdump::parse_str(&text);
    let at =
        |s: sous_chef::types::LineSpan| (lines[s.start as usize - 1], lines[s.end as usize - 1]);
    assert_eq!(
        at(d.variable("PN").unwrap().span),
        ("# $PN [2 operations]", "PN=\"curl\"")
    );
    assert_eq!(
        at(d.variable("SHELL").unwrap().span),
        ("# $SHELL [4 operations]", "unset SHELL")
    );
    let (first, last) = at(d.variable("ALTERNATIVE").unwrap().span);
    assert_eq!(first, "# $ALTERNATIVE");
    assert!(last.starts_with("#     [doc] "));
    let (first, last) = at(d.function("autotools_do_compile").unwrap().span);
    assert!(first.starts_with("# line: 227, file: "));
    assert_eq!(last, "}");
    let (first, last) = at(d.function("write_task_data").unwrap().span);
    assert_eq!(first, "python write_task_data () {");
    assert_eq!(last, "}");
}

#[test]
fn filtered_parse_keeps_only_the_names_asked_for() {
    let text = fixture_at(Path::new(LIVE));
    let d = envdump::parse_filtered(std::io::Cursor::new(text.as_bytes()), |n| {
        n == "PN" || n == "do_compile"
    })
    .unwrap();
    assert_eq!(d.variables.len(), 1);
    assert_eq!(d.functions.len(), 1);
    assert_eq!(d.stats, dump().stats);
}

#[test]
fn locations_map_into_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let live = LiveWs::new(tmp.path());
    let ws = Workspace::discover(&live.config).unwrap();
    let d = envdump::parse_str(&fixture_at(&live.root));
    let src = d.variable("SRC_URI").unwrap();
    assert_eq!(
        src.ops[3].location(Some(&ws)),
        "meta:recipes-support/curl/curl_8.19.0.bb:42"
    );
    // The build directory is the `build` pseudo-layer; `build/..` is resolved.
    let bbl = d.variable("BBLAYERS").unwrap();
    assert_eq!(bbl.ops[0].location(Some(&ws)), "build:conf/bblayers.conf:6");
    // Outside the workspace: the absolute path, without `..`.
    let arm = d.variable("ARMPKGARCH").unwrap();
    assert_eq!(
        arm.ops[0].location(Some(&ws)),
        "meta:conf/machine/include/arm/arch-arm.inc:4"
    );
    let kernel = d.variable("PREFERRED_PROVIDER_virtual/kernel").unwrap();
    assert_eq!(
        kernel.ops[0].location(Some(&ws)),
        format!(
            "{}/yocto-kiss/meta-kiss-ti/conf/machine/dogbonedark.conf:22",
            live.root.display()
        )
    );
    // A python helper's basename stays as printed.
    let fl = d.variable("FILE_LAYERNAME").unwrap();
    assert_eq!(fl.ops[1].location(Some(&ws)), "cookerdata.py:513");
    assert_eq!(location(None, "/a/b/../c.conf", Some(3)), "/a/c.conf:3");
    assert_eq!(location(None, "autogenerated", Some(1)), "autogenerated:1");
}

// ---- forms the recording does not show (from `lib/bb/data.py` `emit_var`) ----

#[test]
fn multi_line_values_and_escapes() {
    // emit_var escapes `"`, backtick and `$` with a backslash and turns each newline into
    // ` \` plus a newline; backslashes themselves are not escaped.
    let text = "\
#
# INCLUDE HISTORY:
#
# /x/conf/bblayers.conf
#
# $MULTI [2 operations]
#   set /x/a.conf:1
#     \"line one
#     \\\"two\\\" $HOME
#     \"
#   set /x/doc.conf:1
#     [doc] \"d\"
# pre-expansion value:
#   \"line one
#\\\"two\\\" $HOME
#
#\"
MULTI=\"line one \\
\\\\\"two\\\\\" \\$HOME \\`cmd\\` \\
 \\
\"
#
# $NOHIST
#   [no history recorded]
#
#   \"plain\"
NOHIST=\"plain\"
# expansion of BROKEN threw ExpansionError: Failure expanding variable BROKEN
#
# $LAST [2 operations]
#   set /x/a.conf:2
#     \"x\"
#   set /x/a.conf:3
#     [doc] \"y\"
# pre-expansion value:
#   \"None\"
# line: 4, file: /x/base.bbclass
do_x() {
\techo
}
";
    let d = envdump::parse_str(text);
    assert_eq!(d.stats.skipped, 0, "{:?}", d.stats.examples);
    let m = d.variable("MULTI").unwrap();
    assert_eq!(
        m.value.as_deref(),
        Some("line one\n\\\"two\\\" $HOME `cmd`\n\n")
    );
    assert_eq!(m.ops[0].value, "line one\n\\\"two\\\" $HOME\n");
    assert_eq!(
        m.pre_expansion.as_deref(),
        Some("line one\n\\\"two\\\" $HOME\n\n")
    );
    let n = d.variable("NOHIST").unwrap();
    assert!(n.no_history);
    assert_eq!(
        (n.value.as_deref(), n.pre_expansion.as_deref()),
        (Some("plain"), Some("plain"))
    );
    let b = d.variable("BROKEN").unwrap();
    assert!(b.error.as_deref().unwrap().starts_with("ExpansionError"));
    // The last variable runs straight into the shell functions.
    assert_eq!(
        d.variable("LAST").unwrap().pre_expansion.as_deref(),
        Some("None")
    );
    assert_eq!(d.function("do_x").unwrap().body, "\techo");
}

// ---- malformed input ----

#[test]
fn malformed_input_is_skipped_and_counted() {
    let text = "\
garbage before
#
# INCLUDE HISTORY:
#
# $
# $GOOD
#   set /x/a.conf:1
#     \"g\"
GOOD=\"g\"
this line means nothing
#     a detail without an operation
# $OPEN [2 operations]
#   set /x/a.conf:2
#     \"unterminated
NOT A LINE
# line: notanumber, file: /x/b.bbclass
do_y() {
\tno closing brace
";
    let d = envdump::parse_str(text);
    assert_eq!(d.stats.leading, 1);
    assert_eq!(d.value("GOOD"), Some("g"));
    assert_eq!(d.stats.skipped, 4, "{:?}", d.stats.examples);
    let open = d.variable("OPEN").unwrap();
    assert_eq!(open.ops[0].value, "unterminated");
    let y = d.function("do_y").unwrap();
    assert_eq!(y.line, None);
    assert_eq!(y.body, "\tno closing brace");

    // Not a dump at all.
    let none = envdump::parse_str("ERROR: Nothing PROVIDES 'nope'\n");
    assert!(!none.stats.found_header);
    assert!(none.variables.is_empty());
    assert_eq!(none.stats.leading, 1);
}

#[test]
fn any_prefix_and_invalid_utf8_parse_without_panic() {
    let text = fixture_at(Path::new(LIVE));
    let bytes = text.as_bytes();
    let mut k = 0;
    while k < bytes.len() {
        let _ = envdump::parse(std::io::Cursor::new(&bytes[..k])).unwrap();
        k += 997;
    }
    let mut broken = bytes[..20_000].to_vec();
    broken.extend_from_slice(&[0xff, 0xfe, b'\n', 0xc3, b'\n', b'\r', b'\n']);
    broken.extend_from_slice(&bytes[20_000..]);
    let d = envdump::parse(std::io::Cursor::new(&broken)).unwrap();
    assert_eq!(d.value("PN"), Some("curl"));
    // CRLF line ends are read like LF.
    let crlf = text.replace('\n', "\r\n");
    assert_eq!(envdump::parse_str(&crlf).value("PN"), Some("curl"));
}

/// The full recording, when the live environment is present (not in the normal gate).
#[test]
#[ignore = "needs the live environment ($SOUS_CHEF_LIVE/runs/bitbake-e-curl.out)"]
fn full_live_recording() {
    let live = std::env::var("SOUS_CHEF_LIVE").unwrap_or_else(|_| {
        format!(
            "{}/Development/sous-chef-live",
            std::env::var("HOME").unwrap()
        )
    });
    let path = Path::new(&live).join("runs/bitbake-e-curl.out");
    let text = std::fs::read_to_string(&path).unwrap();
    let d = envdump::parse_str(&text);
    assert_eq!(d.stats.skipped, 0, "{:?}", d.stats.examples);
    let headers = text.lines().filter(|l| l.starts_with("# $")).count();
    assert_eq!(d.variables.len(), headers);
    let assignments = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with(char::is_whitespace) && l.contains("=\""))
        .filter(|l| !l.contains("() {"))
        .count();
    let with_value = d.variables.values().filter(|v| v.value.is_some()).count();
    assert!(with_value <= assignments);
    let shell = text.lines().filter(|l| l.starts_with("# line: ")).count();
    let python = text
        .lines()
        .filter(|l| l.starts_with("python ") && l.ends_with(" () {"))
        .count();
    assert_eq!(d.functions.len(), shell + python);
}

// ---- compare ----

#[test]
fn compare_classifies_differences() {
    let ws = common::fixture_ws();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let target = Target {
        mc: None,
        recipe: Some("app".into()),
        version: None,
        machine: None,
        distro: None,
    };
    let ds = sous_chef::eval::evaluate(&index, &ws, &target).unwrap();
    let pn = ds.value("PN").unwrap().text;
    let python = ds.value("EXTRA_OEMAKE").unwrap();
    assert!(python.unevaluated_python);
    let spaced = ds.value("PV").unwrap().text;
    let text = format!(
        "\
#
# INCLUDE HISTORY:
#
# $EXTRA_OEMAKE
#   set /x/app.bb:9
#     \"GREETING=hello\"
EXTRA_OEMAKE=\"GREETING=hello\"
#
# $ONLYBB
#   set /x/a.conf:1
#     \"b\"
ONLYBB=\"b\"
#
# $PN
#   set /x/a.conf:1
#     \"{pn}\"
PN=\"{pn}\"
#
# $PV
#   set /x/a.conf:1
#     \"x\"
PV=\"  {spaced} \"
#
# $SHELL
#   set /x/a.conf:1
#     [unexport] \"1\"
unset SHELL
"
    );
    let dump = envdump::parse_str(&text);
    let c = compare(
        &ds,
        &dump,
        &["PN", "PV", "EXTRA_OEMAKE", "ONLYBB", "NOPE_NONE", "SHELL"],
    );
    let got: Vec<(&str, Agreement)> = c.iter().map(|c| (c.name.as_str(), c.agreement)).collect();
    assert_eq!(
        got,
        [
            ("PN", Agreement::Equal),
            ("PV", Agreement::Whitespace),
            ("EXTRA_OEMAKE", Agreement::StaticPython),
            ("ONLYBB", Agreement::OnlyBitbake),
            ("NOPE_NONE", Agreement::Equal),
            ("SHELL", Agreement::NotPrinted),
        ]
    );
    assert_eq!(c[0].bitbake_value.as_deref(), Some(pn.as_str()));
    // Empty names: every variable the dump printed a value for, by name.
    let all = compare(&ds, &dump, &[]);
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["EXTRA_OEMAKE", "ONLYBB", "PN", "PV"]);
    // The convenience form evaluates the target itself.
    let again = compare_target(&index, &ws, &target, &dump, &["PN"]).unwrap();
    assert_eq!(again[0].agreement, Agreement::Equal);
    // A real mismatch.
    let other = envdump::parse_str("# $PN\n#   set /x:1\n#     \"zzz\"\nPN=\"zzz\"\n");
    assert_eq!(
        compare(&ds, &other, &["PN"])[0].agreement,
        Agreement::Mismatch
    );
}
