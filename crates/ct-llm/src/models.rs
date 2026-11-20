//! Model list returned by the CLI's stream-json `initialize` control request.

use serde_json::Value;

/// Fixed (not random) so tests can preset the matching `control_response`.
/// Safe because `list_models` sends at most one initialize request per process.
pub const LIST_MODELS_REQUEST_ID: &str = "claudetranslator-list-models";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub value: String,
    pub display_name: String,
    pub description: String,
    pub effort_levels: Vec<String>,
}

/// One entry of the initialize response's `models` array, or `None` if unusable.
/// `supportedEffortLevels` is absent for models without effort support (e.g. haiku),
/// which maps to no effort levels; a non-string-list value also maps to none.
pub fn parse_model_info(raw: &Value) -> Option<ModelInfo> {
    let obj = raw.as_object()?;
    let value = obj.get("value").and_then(Value::as_str)?;
    if value.is_empty() {
        return None;
    }
    let display_name = obj
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(value);
    let description = obj.get("description").and_then(Value::as_str).unwrap_or("");
    let effort_levels = obj
        .get("supportedEffortLevels")
        .and_then(Value::as_array)
        .and_then(|levels| {
            levels
                .iter()
                .map(|l| l.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
    Some(ModelInfo {
        value: value.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        effort_levels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_entry() {
        let m = parse_model_info(&json!({
            "value": "opus", "displayName": "Opus 5.5", "description": "d",
            "supportsEffort": true, "supportedEffortLevels": ["low", "max"]
        }))
        .unwrap();
        assert_eq!(
            m,
            ModelInfo {
                value: "opus".into(),
                display_name: "Opus 5.5".into(),
                description: "d".into(),
                effort_levels: vec!["low".into(), "max".into()],
            }
        );
    }

    #[test]
    fn missing_effort_levels_means_none() {
        let m = parse_model_info(
            &json!({"value": "haiku", "displayName": "Haiku 4.5", "description": "fast"}),
        )
        .unwrap();
        assert!(m.effort_levels.is_empty());
        let m =
            parse_model_info(&json!({"value": "x", "supportedEffortLevels": ["a", 1]})).unwrap();
        assert!(m.effort_levels.is_empty());
    }

    #[test]
    fn display_name_falls_back_to_value_and_description_to_empty() {
        let m = parse_model_info(&json!({"value": "sonnet", "displayName": ""})).unwrap();
        assert_eq!(m.display_name, "sonnet");
        assert_eq!(m.description, "");
    }

    #[test]
    fn unusable_entries_are_none() {
        for raw in [
            json!({"displayName": "no value"}),
            json!("not a dict"),
            json!({"value": ""}),
            json!({"value": 3}),
            json!(null),
        ] {
            assert_eq!(parse_model_info(&raw), None, "{raw}");
        }
    }
}
