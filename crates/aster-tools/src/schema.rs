//! Checking arguments against the catalogue's schemas. The schemas are
//! simple (an object of strings, integers and booleans, some with an enum),
//! so this checks exactly that much and no more.

use serde_json::Value;

/// Check `args` against `schema`. The message names the first problem.
pub fn check(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(args) = args.as_object() else {
        return Err("arguments must be a JSON object".to_owned());
    };
    let properties = schema["properties"].as_object();
    if let Some(required) = schema["required"].as_array() {
        for name in required.iter().filter_map(Value::as_str) {
            if !args.contains_key(name) {
                return Err(format!("missing argument `{name}`"));
            }
        }
    }
    for (name, value) in args {
        let Some(property) = properties.and_then(|p| p.get(name)) else {
            return Err(format!("unknown argument `{name}`"));
        };
        let fits = match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("integer") => value.is_i64() || value.is_u64(),
            Some("boolean") => value.is_boolean(),
            Some("number") => value.is_number(),
            _ => true,
        };
        if !fits {
            return Err(format!(
                "argument `{name}` must be a {}",
                property["type"].as_str().unwrap_or("value")
            ));
        }
        if let Some(choices) = property["enum"].as_array()
            && !choices.contains(value)
        {
            return Err(format!(
                "argument `{name}` must be one of {}",
                Value::Array(choices.clone())
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::find;
    use serde_json::json;

    #[test]
    fn arguments_are_checked_against_the_schema() {
        let schema = find("insert_text").unwrap().schema();
        assert_eq!(
            check(&schema, &json!({"document": "1", "text": "x"})),
            Ok(())
        );
        assert_eq!(
            check(&schema, &json!({"document": "1"})),
            Err("missing argument `text`".to_owned())
        );
        assert!(check(&schema, &json!({"document": 1, "text": "x"})).is_err());
        assert!(check(&schema, &json!({"document": "1", "text": "x", "extra": 1})).is_err());
        assert!(
            check(
                &schema,
                &json!({"document": "1", "text": "x", "position": "middle"})
            )
            .is_err()
        );
        assert!(check(&schema, &json!({"document": "1", "text": "x", "line": 3})).is_ok());
        assert!(check(&schema, &json!([1, 2])).is_err());
    }
}
