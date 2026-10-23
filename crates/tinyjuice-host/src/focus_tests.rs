use super::*;
use serde_json::json;

#[test]
fn focus_is_removed_and_trimmed() {
    let mut arguments = json!({ "url": "u", "summary_focus": "  pricing  " });
    assert_eq!(
        take_summary_focus(&mut arguments).as_deref(),
        Some("pricing")
    );
    assert_eq!(arguments, json!({ "url": "u" }));
}

#[test]
fn blank_and_nonstring_focus_values_are_removed_without_a_value() {
    for value in [json!("  "), json!(4), json!(null)] {
        let mut arguments = json!({ "summary_focus": value });
        assert_eq!(take_summary_focus(&mut arguments), None);
        assert_eq!(arguments, json!({}));
    }
}

#[test]
fn only_the_bus_focus_property_is_recognized() {
    let property = summary_focus_property();
    assert!(declares_summary_focus(
        &json!({"properties":{"summary_focus":property}})
    ));
    assert!(!declares_summary_focus(
        &json!({"properties":{"summary_focus":{"type":"integer"}}})
    ));
}

#[test]
fn absent_focus_and_nonobject_arguments_are_unchanged() {
    let mut arguments = json!({ "url": "u" });
    assert_eq!(take_summary_focus(&mut arguments), None);
    assert_eq!(arguments, json!({ "url": "u" }));

    let mut not_an_object = json!("raw");
    assert_eq!(take_summary_focus(&mut not_an_object), None);
    assert_eq!(not_an_object, json!("raw"));
}

#[test]
fn focus_declaration_rejects_missing_properties_and_noncanonical_schema() {
    assert!(!declares_summary_focus(&json!({})));
    assert!(!declares_summary_focus(&json!({ "properties": {} })));
    assert!(!declares_summary_focus(
        &json!({ "properties": { "summary_focus": { "type": "integer" } } })
    ));
}

#[test]
fn shared_focus_property_and_argument_name_are_pinned() {
    assert_eq!(
        summary_focus_property(),
        json!({
            "type": "string",
            "description": "What you need from this result. A large result is summarized around this; the full output stays retrievable."
        })
    );
    assert_eq!(SUMMARY_FOCUS_ARG, "summary_focus");
}
