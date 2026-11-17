//! T0.3: the tool and rule registries match docs/mcp-tools.md and docs/lint-rules.md.

use std::collections::BTreeSet;

fn doc(name: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name))
        .unwrap_or_else(|e| panic!("reading {name}: {e}"))
}

/// Tool names: the "### `name`" headings of docs/mcp-tools.md.
fn documented_tools() -> Vec<String> {
    doc("docs/mcp-tools.md")
        .lines()
        .filter_map(|l| l.strip_prefix("### `"))
        .filter_map(|l| l.strip_suffix('`'))
        .map(str::to_string)
        .collect()
}

/// Rule ids: the first cell of each table row of docs/lint-rules.md.
fn documented_rules() -> BTreeSet<String> {
    doc("docs/lint-rules.md")
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn tool_registry_matches_docs() {
    let expected = [
        "recipe",
        "var",
        "overrides",
        "item",
        "search",
        "class",
        "layers",
        "deps",
        "lint",
        "scaffold",
        "bitbake",
        "log",
        "slice",
        "status",
    ];
    let registered: Vec<_> = sous_chef::mcp::TOOLS.iter().map(|t| t.name).collect();
    assert_eq!(registered, expected);
    assert_eq!(documented_tools(), expected);
}

#[test]
fn rule_registry_matches_docs() {
    let registered: BTreeSet<String> = sous_chef::lint::all_rules()
        .iter()
        .map(|r| r.id().to_string())
        .collect();
    let documented = documented_rules();
    assert_eq!(documented.len(), 38);
    assert_eq!(registered, documented);
    assert_eq!(sous_chef::lint::all_rules().len(), 38);
}

#[test]
fn every_tool_has_a_schema_and_description() {
    // Calling `run` needs a `Ctx`, which needs the index: covered from T2.5 on.
    for t in sous_chef::mcp::TOOLS {
        let tool = t.to_rmcp();
        assert!(
            tool.description.is_some_and(|d| !d.is_empty()),
            "{}",
            t.name
        );
    }
}
