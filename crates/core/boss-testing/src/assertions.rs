//! Custom assertion helpers with agent-friendly failure messages.

use serde_json::Value;

/// Assert that a JSON value contains a specific field with an expected value.
/// Useful for asserting on response bodies.
pub fn assert_json_field(value: &Value, field: &str, expected: &Value) {
    let actual = value.get(field);
    if actual != Some(expected) {
        panic!(
            "\n  JSON field assertion failed\n  field: {}\n  expected: {}\n  actual: {}\n  full value: {}\n",
            field,
            expected,
            actual
                .map(|v| v.to_string())
                .unwrap_or_else(|| "MISSING".to_string()),
            value,
        );
    }
}

/// Assert that a JSON value has a specific field present (any value).
pub fn assert_json_has_field(value: &Value, field: &str) {
    if value.get(field).is_none() {
        panic!(
            "\n  JSON field {} not found\n  full value: {}\n",
            field, value,
        );
    }
}
