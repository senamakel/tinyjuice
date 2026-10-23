use super::*;

#[test]
fn repl_tool_constants_match_declarations() {
    let names: Vec<String> = repl_tool_declarations()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, REPL_TOOLS);
    assert_eq!(RETRIEVE, "juice_retrieve");
    assert_eq!(LEGACY_RETRIEVE, "tinyjuice_retrieve");
}

#[test]
fn juice_find_scope_schema_preserves_the_existing_wire_contract() {
    let find = repl_tool_declarations()
        .into_iter()
        .find(|declaration| declaration.name == "juice_find")
        .expect("juice_find is declared");

    assert_eq!(
        find.parameters["properties"]["scope"],
        serde_json::json!({ "type": "string" })
    );
}

#[test]
fn retrieve_declaration_matches_the_shipped_token_and_range_contract() {
    assert_eq!(RETRIEVE, "juice_retrieve");

    let schema = retrieve_tool_parameters();
    assert_eq!(schema["required"], serde_json::json!(["token"]));
    assert_eq!(schema["properties"]["token"]["type"], "string");
    assert_eq!(schema["properties"]["range"]["type"], "object");
    assert_eq!(
        schema["properties"]["range"]["properties"]["unit"]["enum"],
        serde_json::json!(["bytes", "lines"])
    );
    assert!(schema["properties"].get("hash").is_none());
    assert_eq!(
        RETRIEVE_TOOL_DESCRIPTION,
        "Return the whole original of a compacted or stored tool output, or a `range` \
         of its lines or bytes. Pass the token its footer names (or the hash in a \
         `⟦tj:…⟧` marker). To read or search only part of it, use juice_find with \
         the same value as `handle` when that tool is offered."
    );
}
