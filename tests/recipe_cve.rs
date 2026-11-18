//! `recipe show=cve` on temporary copies of the fixture workspace with a recipe that carries
//! `CVE_STATUS` entries, a status group, CVE patches by name and by header, and a recorded
//! CVE check JSON report.
//!
//! The OE-core pieces added to the copy are hand-written from the source, not copied from a
//! build: a short `conf/cve-check-map.conf` (entries as in OE-core Wrynose), a
//! `lib/oe/cve_check.py` that holds only the function names sous-chef reads its rules from
//! (`decode_cve_status`, the `"cpe"` comparison, `parse_cves_from_patch_contents`), and a
//! `deploy/cve/vuln_cve.json` in the shape `vex.bbclass` / `cve-check.bbclass`
//! `cve_write_data_json` writes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use sous_chef::bitbake::JobTable;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::recipe::{self, Show};
use sous_chef::workspace::Workspace;

const MAP: &str = "\
# Hand-written subset of OE-core conf/cve-check-map.conf (Wrynose).
CVE_CHECK_STATUSMAP[patched] = \"Patched\"
CVE_CHECK_STATUSMAP[backported-patch] = \"Patched\"
CVE_CHECK_STATUSMAP[fixed-version] = \"Patched\"
CVE_CHECK_STATUSMAP[unpatched] = \"Unpatched\"
CVE_CHECK_STATUSMAP[ignored] = \"Ignored\"
CVE_CHECK_STATUSMAP[cpe-incorrect] = \"Ignored\"
CVE_CHECK_STATUSMAP[not-applicable-config] = \"Ignored\"
CVE_CHECK_STATUSMAP[not-applicable-platform] = \"Ignored\"
CVE_CHECK_STATUSMAP[unknown] = \"Unknown\"
";

const LIB_NEW: &str = "\
# Hand-written marker file: only the names sous-chef reads its rules from.
def parse_cves_from_patch_contents(patch_contents):
    pass

def decode_cve_status(d, cve):
    if len(status_split) >= 4 and status_split[1].strip() == \"cpe\":
        pass
";

const LIB_KIRKSTONE: &str = "\
# Hand-written marker file: OE-core Kirkstone has get_patched_cves with the CVE: tag
# regular expression and no decode_cve_status.
def get_patched_cves(d):
    cve_match = re.compile(r\"CVE:( CVE-\\d{4}-\\d+)+\")
";

const RECIPE: &str = "\
SUMMARY = \"CVE view test\"
LICENSE = \"MIT\"
SRC_URI = \"https://example.org/vuln-${PV}.tar.gz \\
           file://CVE-2024-11111.patch \\
           file://fix-overflow.patch \\
           file://CVE-2024-22222-and-more.patch \\
           file://plain.patch \\
           \"
CVE_PRODUCT = \"acme:vuln\"
CVE_STATUS[CVE-2023-00001] = \"not-applicable-config: needs the frobnicator\"
CVE_STATUS[CVE-2024-11111] = \"fixed-version: fixed in 1.0\"
CVE_STATUS[CVE-2023-00002] = \"maybe-later: who knows\"
CVE_STATUS[CVE-2023-00003] = \"cpe-incorrect: cpe:other:thing:not ours\"
CVE_STATUS_GROUPS = \"CVE_STATUS_WIN CVE_STATUS_NONE\"
CVE_STATUS_WIN = \"CVE-2023-10001 CVE-2023-10002\"
CVE_STATUS_WIN[status] = \"not-applicable-platform: Windows only\"
";

const BBAPPEND: &str = "\
FILESEXTRAPATHS:prepend := \"${THISDIR}/files:\"
SRC_URI += \"file://CVE-2025-44444.patch\"
CVE_STATUS[CVE-2025-50000] = \"cpe-incorrect: the NVD entry is for another vuln\"
";

const PATCH_BY_BOTH: &str = "\
From 1 Mon Sep 17 00:00:00 2001
Subject: [PATCH] fix the first one

CVE: CVE-2024-11111
Upstream-Status: Backport [https://example.org/vuln/commit/1]
---
";

const PATCH_BY_HEADER: &str = "\
Subject: [PATCH] fix an overflow

CVE: CVE-2024-33333 CVE-2024-33334
Upstream-Status: Submitted [https://example.org/vuln/pull/2]
---
";

const PATCH_BY_NAME: &str = "\
Subject: [PATCH] fix the second one

Upstream-Status: Pending
---
";

const PATCH_PLAIN: &str = "\
Subject: [PATCH] build fix

Upstream-Status: Inappropriate [oe specific]
---
";

const PATCH_APPEND: &str = "\
Subject: [PATCH] vendor fix

CVE: CVE-2025-44444
Upstream-Status: Backport [https://example.org/vuln/commit/4]
---
";

/// Hand-written in the shape of `cve_write_data_json` (cve-check.bbclass, Scarthgap): one
/// package, issues with `id`, `status`, scores and `detail`.
const REPORT: &str = r#"{
  "version": "1",
  "package": [
    {
      "name": "vuln",
      "layer": "meta-mine",
      "version": "1.0",
      "products": [{"product": "vuln", "cvesInRecord": "Yes"}],
      "issue": [
        {"id": "CVE-2023-00001", "summary": "s", "scorev2": "0.0", "scorev3": "5.3", "scorev4": "0.0", "vector": "NETWORK", "vectorString": "x", "status": "Ignored", "link": "https://nvd.nist.gov/vuln/detail/CVE-2023-00001", "detail": "not-applicable-config", "description": "needs the frobnicator"},
        {"id": "CVE-2024-11111", "summary": "s", "scorev2": "5.0", "scorev3": "7.5", "scorev4": "0.0", "vector": "NETWORK", "vectorString": "x", "status": "Patched", "link": "https://nvd.nist.gov/vuln/detail/CVE-2024-11111"},
        {"id": "CVE-2024-90001", "summary": "s", "scorev2": "6.8", "scorev3": "9.8", "scorev4": "0.0", "vector": "NETWORK", "vectorString": "x", "status": "Unpatched", "link": "https://nvd.nist.gov/vuln/detail/CVE-2024-90001"},
        {"id": "CVE-2024-90002", "summary": "s", "scorev2": "4.3", "scorev3": "0.0", "scorev4": "0.0", "vector": "LOCAL", "vectorString": "x", "status": "Unpatched", "link": "https://nvd.nist.gov/vuln/detail/CVE-2024-90002"}
      ]
    }
  ]
}
"#;

struct Fixture {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: String,
    ctx: Ctx,
}

fn write(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append(p: &Path, text: &str) {
    let mut s = fs::read_to_string(p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

/// A copy of the fixture with the `vuln` recipe and the OE-core CVE pieces; `lib` is the
/// text of `meta-core/lib/oe/cve_check.py`; `report` adds the recorded JSON.
fn fixture(lib: Option<&str>, report: bool, extra: &str) -> Fixture {
    fixture_in(lib, report, extra, None)
}

/// [`fixture`] with `release` as `release` of sous-chef.toml (since x7 the CVE rules come
/// from the release table; `lib` is only a cross-check).
fn fixture_in(lib: Option<&str>, report: bool, extra: &str, release: Option<&str>) -> Fixture {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    write(&root.join("meta-core/conf/cve-check-map.conf"), MAP);
    append(
        &root.join("meta-core/conf/bitbake.conf"),
        "DEPLOY_DIR ?= \"${TMPDIR}/deploy\"\nrequire conf/cve-check-map.conf\n",
    );
    if let Some(lib) = lib {
        write(&root.join("meta-core/lib/oe/cve_check.py"), lib);
    }
    let dir = root.join("meta-mine/recipes-sec/vuln");
    write(&dir.join("vuln_1.0.bb"), &format!("{RECIPE}{extra}"));
    write(&dir.join("files/CVE-2024-11111.patch"), PATCH_BY_BOTH);
    write(&dir.join("files/fix-overflow.patch"), PATCH_BY_HEADER);
    write(
        &dir.join("files/CVE-2024-22222-and-more.patch"),
        PATCH_BY_NAME,
    );
    write(&dir.join("files/plain.patch"), PATCH_PLAIN);
    let vdir = root.join("meta-vendor/recipes-sec/vuln");
    write(&vdir.join("vuln_%.bbappend"), BBAPPEND);
    write(&vdir.join("files/CVE-2025-44444.patch"), PATCH_APPEND);
    if report {
        write(&root.join("build/tmp/deploy/cve/vuln_cve.json"), REPORT);
    }
    let mut config = common::ws_config(&root);
    config.release = release.map(str::to_string);
    let cache = tempfile::tempdir().unwrap();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Fixture {
        _cache: cache,
        _tmp: tmp,
        root: root.display().to_string(),
        ctx: Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
    }
}

async fn cve(f: &Fixture) -> String {
    recipe::run(
        &f.ctx,
        recipe::Args {
            name: "vuln".into(),
            version: None,
            show: Show::Cve,
            mc: None,
            budget_tokens: Some(5000),
            format: None,
        },
    )
    .await
    .unwrap()
    .replace(f.root.as_str(), "<ws>")
}

#[tokio::test]
async fn cve_view_with_report() {
    let f = fixture(Some(LIB_NEW), true, "");
    let out = cve(&f).await;
    insta::assert_snapshot!(out);
    // The facts the view exists for.
    assert!(out.contains("fix-overflow.patch declares CVE-2024-33333 CVE-2024-33334 in its header, but its name has no CVE id"), "{out}");
    assert!(out.contains("CVE-2024-22222-and-more.patch is named for CVE-2024-22222, but its header has no line starting with CVE:"), "{out}");
    assert!(out.contains("patched by CVE-2024-11111.patch and also in CVE_STATUS \"fixed-version\"; CVE_STATUS wins, with a warning"), "{out}");
    assert!(
        out.contains("status \"maybe-later\" is not in CVE_CHECK_STATUSMAP"),
        "{out}"
    );
    assert!(
        out.contains("CVE_STATUS_GROUPS names CVE_STATUS_NONE, which is not set"),
        "{out}"
    );
    assert!(out.contains("the status is for other:thing, which matches no CVE_PRODUCT word (acme:vuln): get_patched_cves skips it"), "{out}");
    assert!(out.contains("build\t-\tUnpatched 2, Patched 1, Ignored 1 (version 1.0)\tbuild:tmp/deploy/cve/vuln_cve.json"), "{out}");
    assert!(
        out.contains("unpatched\tCVE-2024-90001\tv3 9.8, v2 6.8"),
        "{out}"
    );
    assert!(!out.contains("to produce one"), "{out}");
}

#[tokio::test]
async fn cve_view_without_report_says_how_to_produce_it() {
    let f = fixture(Some(LIB_NEW), false, "");
    let out = cve(&f).await;
    // The fixture layers have no cve-check / vex class.
    assert!(
        out.contains("note: no CVE check report for this recipe in the build directory, and the layers have no cve-check, vex or sbom-cve-check class"),
        "{out}"
    );
    assert!(!out.contains("\nbuild\t"), "{out}");
    // With a vex class in the layers, the note names the task.
    let (tmp_root, _) = (f.root.clone(), ());
    let class = Path::new(&tmp_root).join("meta-core/classes/vex.bbclass");
    write(
        &class,
        "# hand-written stand-in\naddtask generate_vex before do_build\n",
    );
    let out = cve(&f).await;
    assert!(
        out.contains("INHERIT += \"vex\", then bitbake -c generate_vex vuln"),
        "{out}"
    );
}

#[tokio::test]
async fn cve_view_kirkstone_rules() {
    // x7: the rules come from the release (`release = "kirkstone"`); the Kirkstone
    // lib/oe/cve_check.py agrees, so no cross-check note.
    let f = fixture_in(
        Some(LIB_KIRKSTONE),
        false,
        "CVE_CHECK_IGNORE = \"CVE-2020-00001 CVE-2020-00002\"\n",
        Some("kirkstone"),
    );
    let out = cve(&f).await;
    assert!(
        out.contains("ignore\tCVE-2020-00001\tCVE_CHECK_IGNORE (not checked)\t-\tmeta-mine:recipes-sec/vuln/vuln_1.0.bb:17"),
        "{out}"
    );
    assert!(
        out.contains(
            "CVE_STATUS has no effect here (kirkstone has no decode_cve_status in lib/oe/cve_check.py)"
        ),
        "{out}"
    );
    assert!(
        out.contains("note: CVE rules of kirkstone: no CVE_STATUS"),
        "{out}"
    );
    assert!(!out.contains("differs from"), "{out}");
    assert!(!out.contains("\nstatus\t"), "{out}");
    // The Kirkstone header rule: `CVE: CVE-a CVE-b` (space separated), anywhere.
    assert!(out.contains("patch\tCVE-2024-33334\tby header"), "{out}");
    assert!(
        out.contains("patch headers: the tag CVE: followed by space-separated ids"),
        "{out}"
    );
}

#[tokio::test]
async fn cve_rules_follow_the_release_and_the_library_is_a_cross_check() {
    use sous_chef::bitbake::cve::CveRules;
    use sous_chef::release::Release;
    // The release table, checked on OE-core's branches (bitbake-semantics.md, "Releases").
    let r = |rel| {
        let c = CveRules::for_release(rel);
        (c.status, c.cpe, c.line_rule)
    };
    assert_eq!(r(Release::Kirkstone), (false, false, false));
    assert_eq!(r(Release::Mickledore), (false, false, false));
    assert_eq!(r(Release::Nanbield), (true, false, false));
    assert_eq!(r(Release::Scarthgap), (true, false, false));
    assert_eq!(r(Release::Styhead), (true, true, false));
    assert_eq!(r(Release::Walnascar), (true, true, true));
    assert_eq!(r(Release::Wrynose), (true, true, true));
    // A Wrynose workspace whose lib/oe/cve_check.py is Kirkstone's: Wrynose rules, a note.
    let f = fixture(
        Some(LIB_KIRKSTONE),
        false,
        "CVE_CHECK_IGNORE = \"CVE-2020-00001\"\n",
    );
    let out = cve(&f).await;
    assert!(
        out.contains("note: CVE rules of wrynose: CVE_STATUS"),
        "{out}"
    );
    assert!(
        out.contains(
            "note: meta-core:lib/oe/cve_check.py differs from wrynose's OE-core in CVE_STATUS, \
             the cpe: form of CVE_STATUS, the patch header rule (a backport or a patched \
             OE-core?); the rules of wrynose were used"
        ),
        "{out}"
    );
    assert!(out.contains("from CVE_CHECK_IGNORE"), "{out}");
}

#[tokio::test]
async fn cve_view_folds_cve_check_ignore_into_cve_status() {
    let f = fixture(
        Some(LIB_NEW),
        false,
        "CVE_CHECK_IGNORE = \"CVE-2023-00001\"\n",
    );
    let out = cve(&f).await;
    // extend_cve_status: CVE_CHECK_IGNORE replaces the direct CVE_STATUS entry.
    assert!(
        out.contains("status\tCVE-2023-00001\tIgnored (ignored) (from CVE_CHECK_IGNORE)"),
        "{out}"
    );
}

#[tokio::test]
async fn cve_view_json() {
    let f = fixture(Some(LIB_NEW), true, "");
    let out = recipe::run(
        &f.ctx,
        recipe::Args {
            name: "vuln".into(),
            version: None,
            show: Show::Cve,
            mc: None,
            budget_tokens: Some(5000),
            format: Some(sous_chef::render::Format::Json),
        },
    )
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["header"][0], "kind");
    assert!(v["rows"].as_array().unwrap().len() > 10);
}
