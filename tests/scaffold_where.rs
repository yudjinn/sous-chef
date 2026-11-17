//! T6.5 `where`: table-driven placement advice for each intent on the fixture workspace,
//! for a writable target, an upstream target, several writable layers and a workspace
//! with no writable layer. `where` plans no files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod scaffold_gen;

use std::fs;

use scaffold_gen::*;
use serde_json::json;
use sous_chef::scaffold::{Kind, Request};

fn ask(name: Option<&str>, layer: Option<&str>, opts: serde_json::Value) -> Request {
    let mut r = request(Kind::Where, name.unwrap_or(""), None, opts);
    if name.is_none() {
        r.name = None;
    }
    r.layer = layer.map(str::to_string);
    r
}

/// The advice as text; asserts nothing was planned and nothing written.
fn advice(env: &mut Env, req: &Request) -> String {
    let o = env.run(req, true).unwrap();
    assert!(o.plan.files.is_empty(), "{:?}", o.plan.files);
    assert!(!o.written);
    o.plan.notes.join("\n")
}

/// `(case, name, layer, options, lines the advice must contain)`.
type Case = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    serde_json::Value,
    &'static [&'static str],
);

fn cases() -> Vec<Case> {
    vec![
        (
            "upstream recipe, variable, existing bbappend",
            Some("hello"),
            None,
            json!({"variable": "EXTRA_OECONF", "value": " --enable-x"}),
            &[
                "where: meta-mine:recipes-core/hello/hello_1.1.bbappend (the existing bbappend)",
                "which is not writable",
                "scaffold: scaffold kind=bbappend name=hello layer=meta-mine options={\"set\":{\"EXTRA_OECONF:append\":\" --enable-x\"}}",
                "note: meta-vendor:recipes-core/hello/hello_%.bbappend also applies",
                "now: EXTRA_OECONF is set last at meta-mine:recipes-core/hello/hello_1.1.bbappend:6",
            ],
        ),
        (
            "upstream recipe, add-patch, no bbappend in the writable layer",
            Some("libfoo"),
            None,
            json!({"intent": "add-patch"}),
            &[
                "where: a new bbappend in meta-mine: recipes-core/libfoo/libfoo_2.0.bbappend",
                "scaffold: scaffold kind=bbappend name=libfoo layer=meta-mine options={\"files\":[\"<name>.patch\"]}",
            ],
        ),
        (
            "upstream recipe, machine-specific variable",
            Some("libfoo"),
            None,
            json!({"variable": "EXTRA_OECONF", "machine": "boardx"}),
            &[
                "where: a new bbappend in meta-mine: recipes-core/libfoo/libfoo_%.bbappend",
                "\"machine\":\"boardx\"",
                "conf/machine/boardx.conf",
            ],
        ),
        (
            "upstream recipe, task",
            Some("hello"),
            None,
            json!({"task": "install"}),
            &[
                "where: meta-mine:recipes-core/hello/hello_1.1.bbappend (the existing bbappend)",
                "by hand: add `do_install:append() { ... }`",
                "now: do_install is made of",
                "note: do_install comes from a class",
            ],
        ),
        (
            "recipe in the writable layer, change-config",
            Some("app"),
            None,
            json!({"variable": "EXTRA_OEMAKE"}),
            &[
                "where: meta-mine:recipes-app/app/app_0.1.bb",
                "which is writable: change the recipe itself",
                "by hand: edit the file",
            ],
        ),
        (
            "recipe only",
            Some("libfoo"),
            None,
            json!({}),
            &[
                "where: a new bbappend in meta-mine: recipes-core/libfoo/libfoo_%.bbappend",
                "<VARIABLE>:append",
            ],
        ),
        (
            "distro policy variable",
            None,
            None,
            json!({"variable": "DISTRO_FEATURES"}),
            &[
                "where: a distro of your own in meta-mine: conf/distro/<new-distro>.conf with `require conf/distro/testdistro.conf`",
                "DISTRO_FEATURES is distro policy",
                "meta-core:conf/distro/testdistro.conf is in meta-core, which is not writable",
                "now: DISTRO_FEATURES is set last at build:conf/local.conf:13",
                "local.conf only for a developer-local trial",
            ],
        ),
        (
            "pin-version",
            Some("hello"),
            None,
            json!({"intent": "pin-version"}),
            &[
                "add `PREFERRED_VERSION_hello = \"...\"`",
                "now: hello 1.1 is used (preferred by layer priority and version)",
                "note: versions of hello: 1.1 (meta-core), 1.0 (meta-core)",
            ],
        ),
        (
            "change-provider",
            None,
            None,
            json!({"intent": "change-provider", "item": "virtual/foo"}),
            &[
                "PREFERRED_PROVIDER_virtual/foo is distro policy",
                "now: virtual/foo comes from libfoo-alt, set at meta-core:conf/distro/testdistro.conf:8 (providers: libfoo-alt, libfoo)",
            ],
        ),
        (
            "provider of the kernel: machine conf",
            None,
            None,
            json!({"variable": "PREFERRED_PROVIDER_virtual/kernel", "machine": "boardx"}),
            &[
                "conf/machine/<new-machine>.conf with `require conf/machine/boardx.conf`",
                "by OE convention it is set in the machine conf",
            ],
        ),
        (
            "set-machine-feature",
            None,
            None,
            json!({"intent": "set-machine-feature"}),
            &[
                "where: a machine of your own in meta-mine: conf/machine/<new-machine>.conf with `require conf/machine/qemux86.conf`",
                "MACHINE_FEATURES is hardware description",
                "local.conf only for a developer-local trial",
            ],
        ),
        (
            "add-package-to-image without an image recipe",
            Some("libfoo"),
            None,
            json!({"intent": "add-package-to-image"}),
            &[
                "where: the image recipe that should contain libfoo",
                "note: no image recipe",
                "`IMAGE_INSTALL:append = \" libfoo\"` in local.conf only for a developer-local trial",
            ],
        ),
    ]
}

#[test]
fn each_intent_on_the_fixture_workspace() {
    let mut env = Env::new();
    let mut all = String::new();
    for (case, name, layer, opts, want) in cases() {
        let text = advice(&mut env, &ask(name, layer, opts));
        for w in want {
            assert!(text.contains(w), "{case}: missing {w:?} in\n{text}");
        }
        assert!(!text.contains("<ws>") && !text.contains(&env.root.display().to_string()));
        all.push_str(&format!("## {case}\n{text}\n\n"));
    }
    insta::assert_snapshot!("where_table", all);
}

#[test]
fn image_recipes_are_found_and_upstream_images_get_a_bbappend() {
    let mut env = Env::edited(|root| {
        let core = root.join("meta-core");
        fs::write(
            core.join("classes-recipe/image.bbclass"),
            "IMAGE_INSTALL ?= \"\"\n",
        )
        .unwrap();
        let dir = core.join("recipes-core/images");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("tiny-image.bb"),
            "SUMMARY = \"Tiny image\"\nLICENSE = \"MIT\"\ninherit image\n",
        )
        .unwrap();
    });
    let text = advice(
        &mut env,
        &ask(
            Some("libfoo"),
            None,
            json!({"intent": "add-package-to-image"}),
        ),
    );
    assert!(
        text.contains("note: image recipes: meta-core:recipes-core/images/tiny-image.bb"),
        "{text}"
    );
    let text = advice(
        &mut env,
        &ask(
            None,
            None,
            json!({"intent": "add-package-to-image", "image": "tiny-image", "package": "libfoo"}),
        ),
    );
    assert!(
        text.contains("where: a bbappend in meta-mine: recipes-core/images/tiny-image_%.bbappend"),
        "{text}"
    );
    assert!(
        text.contains("`IMAGE_INSTALL:append = \" libfoo\"` in the bbappend"),
        "{text}"
    );
    // The recipe itself as `name`: it is an image.
    let text = advice(
        &mut env,
        &ask(
            Some("tiny-image"),
            None,
            json!({"variable": "IMAGE_INSTALL", "package": "app"}),
        ),
    );
    assert!(text.contains("tiny-image_%.bbappend"), "{text}");
}

#[test]
fn writable_target_layer() {
    let mut env = Env::new();
    let text = advice(
        &mut env,
        &ask(
            Some("libfoo"),
            Some("meta-mine"),
            json!({"variable": "PACKAGECONFIG"}),
        ),
    );
    assert!(
        text.contains("where: a new bbappend in meta-mine: recipes-core/libfoo/libfoo_%.bbappend"),
        "{text}"
    );
    assert!(!text.contains("not writable)"), "{text}");
    assert!(!text.contains("layer: "), "{text}");
}

#[test]
fn upstream_target_layer_names_the_writable_one() {
    let mut env = Env::new();
    let text = advice(
        &mut env,
        &ask(
            Some("hello"),
            Some("meta-core"),
            json!({"variable": "EXTRA_OECONF"}),
        ),
    );
    assert!(
        text.contains("note: meta-core is not writable (upstream: an update of the layer overwrites edits there); use meta-mine instead"),
        "{text}"
    );
    assert!(
        text.contains("where: meta-mine:recipes-core/hello/hello_1.1.bbappend"),
        "{text}"
    );
}

#[test]
fn no_writable_layer() {
    let mut env = Env::new();
    env.cfg.writable_layers.clear();
    env.rediscover();
    let text = advice(
        &mut env,
        &ask(Some("hello"), None, json!({"variable": "EXTRA_OECONF"})),
    );
    assert!(text.contains("note: no layer is writable"), "{text}");
    assert!(
        text.contains(
            "where: a new bbappend in <your writable layer>: recipes-core/hello/hello_%.bbappend"
        ),
        "{text}"
    );
    // A named layer that is not writable, in a workspace with none.
    let text = advice(
        &mut env,
        &ask(
            None,
            Some("meta-core"),
            json!({"variable": "DISTRO_FEATURES"}),
        ),
    );
    assert!(
        text.contains("meta-core is not writable, and no layer is"),
        "{text}"
    );
    assert!(text.contains("<your writable layer>"), "{text}");
}

#[test]
fn several_writable_layers_pick_by_content() {
    let mut env = Env::new();
    env.cfg.writable_layers = vec!["meta-mine".into(), "meta-vendor".into()];
    env.rediscover();
    // libfoo: no bbappend anywhere: highest priority.
    let text = advice(
        &mut env,
        &ask(Some("libfoo"), None, json!({"variable": "EXTRA_OECONF"})),
    );
    assert!(
        text.contains("layer: meta-mine of the writable layers meta-mine, meta-vendor (highest priority; none holds a bbappend of libfoo)"),
        "{text}"
    );
    // libfoo-alt is in meta-vendor itself, now writable: edit it there.
    let text = advice(
        &mut env,
        &ask(
            Some("libfoo-alt"),
            None,
            json!({"variable": "EXTRA_OECONF"}),
        ),
    );
    assert!(
        text.contains("where: meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb"),
        "{text}"
    );
    // hello 1.0: only the vendor bbappend applies.
    let mut req = ask(Some("hello"), None, json!({"variable": "EXTRA_OECONF"}));
    req.version = Some("1.0".into());
    let text = advice(&mut env, &req);
    assert!(
        text.contains("layer: meta-vendor of the writable layers meta-mine, meta-vendor, because it already holds a bbappend of hello"),
        "{text}"
    );
    assert!(
        text.contains(
            "where: meta-vendor:recipes-core/hello/hello_%.bbappend (the existing bbappend)"
        ),
        "{text}"
    );
}

#[test]
fn versions_without_an_inc_suggest_inc_split() {
    let mut env = Env::edited(|root| {
        let dir = root.join("meta-mine/recipes-app/app");
        let text = fs::read_to_string(dir.join("app_0.1.bb")).unwrap();
        fs::write(dir.join("app_0.2.bb"), text).unwrap();
    });
    let text = advice(
        &mut env,
        &ask(Some("app"), None, json!({"variable": "DEPENDS"})),
    );
    assert!(
        text.contains("`scaffold kind=inc_split name=app`"),
        "{text}"
    );
}

#[test]
fn errors() {
    let mut env = Env::new();
    let e = env.run(&ask(None, None, json!({})), false).unwrap_err();
    assert!(e.to_string().contains("where needs a recipe"), "{e}");
    let e = env
        .run(&ask(Some("hello"), None, json!({"intent": "fly"})), false)
        .unwrap_err();
    assert!(e.to_string().contains("unknown intent"), "{e}");
    let e = env
        .run(&ask(None, None, json!({"intent": "add-patch"})), false)
        .unwrap_err();
    assert!(e.to_string().contains("belongs to a recipe"), "{e}");
    let e = env
        .run(&ask(Some("nosuch"), None, json!({"variable": "X"})), false)
        .unwrap_err();
    assert!(e.to_string().contains("nosuch"), "{e}");
    let e = env
        .run(&ask(Some("hello"), None, json!({"colour": "x"})), false)
        .unwrap_err();
    assert!(e.to_string().contains("valid options"), "{e}");
}
