//! Helpers for the optional `summary_focus` tool argument.

use serde_json::Value;
use tinyjuice_bus::tools::{SUMMARY_FOCUS_ARG, summary_focus_property};

/// Whether a schema declares TinyJuice's exact `summary_focus` property.
pub fn declares_summary_focus(parameters: &Value) -> bool {
    parameters
        .get("properties")
        .and_then(|properties| properties.get(SUMMARY_FOCUS_ARG))
        == Some(&summary_focus_property())
}

/// Remove `summary_focus` from a tool call and return nonblank text.
pub fn take_summary_focus(arguments: &mut Value) -> Option<String> {
    arguments
        .as_object_mut()?
        .remove(SUMMARY_FOCUS_ARG)?
        .as_str()
        .map(str::trim)
        .filter(|focus| !focus.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
