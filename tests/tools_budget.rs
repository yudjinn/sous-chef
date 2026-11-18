//! The plugin's fixed context cost: what `tools/list` sends, tool by tool, and the skill.
//!
//! Every tool definition (name, description, input schema, `_meta`) is in the client's
//! context on every turn, so the total is pinned below a budget. Print the table with
//! `cargo test --test tools_budget -- --nocapture`.

use std::path::Path;

/// The bytes of one tool definition as the server sends it in `tools/list`.
fn tool_bytes(t: &sous_chef::mcp::ToolEntry) -> (usize, usize, usize) {
    let tool = t.to_rmcp();
    let all = serde_json::to_string(&tool).unwrap().len();
    let desc = t.description.len();
    let schema = serde_json::to_string(&*(t.schema)()).unwrap().len();
    (all, desc, schema)
}

fn total_tools_bytes() -> usize {
    sous_chef::mcp::TOOLS.iter().map(|t| tool_bytes(t).0).sum()
}

fn skill_bytes() -> usize {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/skills/yocto-layers/SKILL.md"),
    )
    .unwrap()
    .len()
}

#[test]
fn print_the_table() {
    println!("tool\ttotal\tdescription\tschema\test_tokens");
    for t in sous_chef::mcp::TOOLS {
        let (all, desc, schema) = tool_bytes(t);
        println!("{}\t{all}\t{desc}\t{schema}\t{}", t.name, all / 4);
    }
    let total = total_tools_bytes();
    let instr = sous_chef::mcp::instructions().len();
    println!("tools/list total\t{total}\t\t\t{}", total / 4);
    println!("server instructions\t{instr}\t\t\t{}", instr / 4);
    println!("SKILL.md\t{}\t\t\t{}", skill_bytes(), skill_bytes() / 4);
}

/// Budgets set a little above the totals of x7-cleanup, so the fixed overhead cannot creep
/// back. Raise them only with a reason in docs/mcp-tools.md ("Context cost").
const TOOLS_LIST_BUDGET: usize = 15_000;
const INSTRUCTIONS_BUDGET: usize = 800;
const SKILL_BUDGET: usize = 3_000;

#[test]
fn tools_list_stays_within_its_budget() {
    let total = total_tools_bytes();
    assert!(
        total <= TOOLS_LIST_BUDGET,
        "tools/list is {total} bytes, over the budget of {TOOLS_LIST_BUDGET}: shorten a \
         description or an argument doc (the long form belongs in docs/mcp-tools.md)"
    );
    let instr = sous_chef::mcp::instructions().len();
    assert!(instr <= INSTRUCTIONS_BUDGET, "instructions: {instr} bytes");
    let skill = skill_bytes();
    assert!(skill <= SKILL_BUDGET, "SKILL.md: {skill} bytes");
}

#[test]
#[ignore]
fn dump() {
    for t in sous_chef::mcp::TOOLS {
        println!("{}", serde_json::to_string(&t.to_rmcp()).unwrap());
    }
}
