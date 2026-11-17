//! T0.4: the fixture tree exists, every file is UTF-8, and the helpers work.

mod common;

use std::path::Path;

#[test]
fn every_fixture_file_is_utf8() {
    let files = common::files_under(&common::fixtures_dir());
    assert!(files.len() > 60, "only {} fixture files", files.len());
    for f in files {
        let bytes = std::fs::read(&f).unwrap();
        assert!(
            String::from_utf8(bytes).is_ok(),
            "{} is not UTF-8",
            f.display()
        );
    }
}

#[test]
fn expected_fixture_tree_exists() {
    for rel in [
        "README.md",
        "lint/README.md",
        "ws/sous-chef.toml",
        "ws/build/conf/bblayers.conf",
        "ws/build/conf/local.conf",
        "ws/meta-core/conf/layer.conf",
        "ws/meta-core/conf/bitbake.conf",
        "ws/meta-core/conf/machine/qemux86.conf",
        "ws/meta-core/conf/machine/boardx.conf",
        "ws/meta-core/conf/distro/testdistro.conf",
        "ws/meta-core/classes/base.bbclass",
        "ws/meta-core/classes-recipe/autotoolsish.bbclass",
        "ws/meta-core/classes-global/sanityish.bbclass",
        "ws/meta-core/recipes-core/hello/hello_1.0.bb",
        "ws/meta-core/recipes-core/hello/hello_1.1.bb",
        "ws/meta-core/recipes-core/hello/hello.inc",
        "ws/meta-core/recipes-core/hello/files/hello.patch",
        "ws/meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "ws/meta-vendor/conf/layer.conf",
        "ws/meta-vendor/recipes-core/hello/hello_%.bbappend",
        "ws/meta-vendor/recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb",
        "ws/meta-mine/conf/layer.conf",
        "ws/meta-mine/recipes-core/hello/hello_1.1.bbappend",
        "ws/meta-mine/recipes-app/app/app_0.1.bb",
        "ws/meta-mine/recipes-app/orphan/orphan_9.%.bbappend",
        "ws/meta-legacy/conf/layer.conf",
        "ws/meta-legacy/recipes-legacy/oldapp/oldapp_1.0.bb",
        "syntax/crlf.bb",
        "syntax/malformed-unterminated-function.bb",
        "syntax/malformed-bad-continuation.bb",
        "syntax/malformed-garbage.bb",
    ] {
        assert!(common::fixture_path(rel).is_file(), "missing fixture {rel}");
    }
}

#[test]
fn byte_level_fixtures_kept_their_bytes() {
    let crlf = std::fs::read(common::fixture_path("syntax/crlf.bb")).unwrap();
    assert!(
        crlf.windows(2).any(|w| w == b"\r\n"),
        "crlf.bb lost its CRLF"
    );
    let bom = std::fs::read(common::fixture_path("syntax/bom.bb")).unwrap();
    assert!(bom.starts_with(&[0xef, 0xbb, 0xbf]), "bom.bb lost its BOM");
    let nonl = std::fs::read(common::fixture_path("syntax/no-final-newline.bb")).unwrap();
    assert!(!nonl.ends_with(b"\n"));
    let cont =
        std::fs::read_to_string(common::fixture_path("syntax/malformed-bad-continuation.bb"))
            .unwrap();
    assert!(
        cont.contains("\\   \n"),
        "trailing whitespace after \\ was stripped"
    );
}

#[test]
fn legacy_layer_is_not_in_bblayers() {
    let bblayers =
        std::fs::read_to_string(common::fixture_path("ws/build/conf/bblayers.conf")).unwrap();
    assert!(!bblayers.contains("meta-legacy"));
    for l in ["meta-core", "meta-vendor", "meta-mine"] {
        assert!(bblayers.contains(l), "{l}");
    }
}

#[test]
fn helpers_work() {
    let cfg = common::fixture_config();
    assert_eq!(cfg.writable_layers, ["meta-mine"]);
    assert!(cfg.build_dir.as_deref().is_some_and(Path::is_dir));

    let (_tmp, root) = common::temp_ws();
    let copied = common::files_under(&root).len();
    let original = common::files_under(&common::fixture_ws_root()).len();
    assert_eq!(copied, original);
    std::fs::write(root.join("meta-mine/new.inc"), "X = \"1\"\n").unwrap();
    assert!(!common::fixture_path("ws/meta-mine/new.inc").exists());
}
