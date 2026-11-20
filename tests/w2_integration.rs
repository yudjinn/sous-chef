//! Wave 2 integration: the pieces that streams wrote against each other's stubs, checked
//! once they were connected.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{fixture_path, fixture_ws};
use sous_chef::bitbake::logs::task_definition;
use sous_chef::index::Index;
use sous_chef::mcp::tools::slice::failure_blocks;

fn indexed() -> (tempfile::TempDir, Index) {
    let ws = fixture_ws();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    (cache, index)
}

#[test]
fn task_definition_points_at_the_recipe_function() {
    let ws = fixture_ws();
    let (_cache, index) = indexed();
    let loc = task_definition(&index, &ws, "app", "do_compile").unwrap();
    assert_eq!(loc.layer, "meta-mine");
    assert!(loc.path.ends_with("recipes-app/app/app_0.1.bb"), "{loc:?}");
    assert_eq!(loc.lines.start, 21);
}

#[test]
fn task_definition_falls_back_to_the_class_default() {
    let ws = fixture_ws();
    let (_cache, index) = indexed();
    // hello defines no do_compile; autotoolsish exports autotoolsish_do_compile.
    let loc = task_definition(&index, &ws, "hello", "do_compile").unwrap();
    assert_eq!(loc.layer, "meta-core");
    assert!(
        loc.path.ends_with("classes-recipe/autotoolsish.bbclass"),
        "{loc:?}"
    );
    assert_eq!(loc.lines.start, 22);
}

#[test]
fn task_definition_falls_back_to_the_global_base_class() {
    // B16: app keeps the default do_fetch, which only the globally inherited base.bbclass
    // defines (`base_do_fetch`); it is in no recipe's inherit tree.
    let ws = fixture_ws();
    let (_cache, index) = indexed();
    let loc = task_definition(&index, &ws, "app", "do_fetch").unwrap();
    assert_eq!(loc.layer, "meta-core");
    assert!(loc.path.ends_with("classes/base.bbclass"), "{loc:?}");
    let text = std::fs::read_to_string(fixture_path("ws/meta-core/classes/base.bbclass")).unwrap();
    let line = text
        .lines()
        .position(|l| l.starts_with("python base_do_fetch()"))
        .unwrap();
    assert_eq!(loc.lines.start as usize, line + 1);
    // The recipe's own definition still wins over the base default.
    let loc = task_definition(&index, &ws, "app", "do_compile").unwrap();
    assert!(loc.path.ends_with("recipes-app/app/app_0.1.bb"), "{loc:?}");
}

#[test]
fn task_definition_is_none_when_the_index_has_no_answer() {
    let ws = fixture_ws();
    let (_cache, index) = indexed();
    assert!(task_definition(&index, &ws, "no-such-recipe", "do_compile").is_none());
    assert!(task_definition(&index, &ws, "app", "do_nothing").is_none());
}

#[test]
fn slice_failures_use_the_log_detector() {
    let text = std::fs::read_to_string(fixture_path("outputs/log.do_compile.48213")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let blocks = failure_blocks(&lines);
    assert!(!blocks.is_empty());
    // The compiler's own error line carries no `ERROR:` prefix; only the detector finds it.
    let compiler = lines
        .iter()
        .position(|l| l.contains("error: implicit declaration"))
        .unwrap();
    assert!(!lines[compiler].contains("ERROR:"));
    assert!(
        blocks.iter().any(|b| b.contains(&compiler)),
        "{blocks:?} should cover line {compiler}"
    );
    // Blocks are in order and inside the text.
    assert!(blocks.windows(2).all(|w| w[0].start <= w[1].start));
    assert!(blocks.iter().all(|b| b.end <= lines.len()));
}

#[test]
fn slice_failures_cover_a_generic_error_line() {
    let lines = [
        "NOTE: start",
        "ERROR: something nobody recognises",
        "NOTE: end",
    ];
    assert!(failure_blocks(&lines).iter().any(|b| b.contains(&1)));
}
