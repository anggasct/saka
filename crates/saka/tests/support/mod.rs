use serde_json::{Value, json};

pub fn schema_errors(schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    if let Some(any) = schema["anyOf"].as_array() {
        let ok = any.iter().any(|s| {
            let mut e = Vec::new();
            schema_errors(s, value, at, &mut e);
            e.is_empty()
        });
        if !ok {
            errors.push(format!("{at}: no alternative matches {value}"));
        }
        return;
    }
    if let Some(kind) = schema["type"].as_str() {
        let ok = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !ok {
            errors.push(format!("{at}: want {kind}, got {value}"));
            return;
        }
    }
    if let Some(allowed) = schema["enum"].as_array()
        && !allowed.contains(value)
    {
        errors.push(format!("{at}: {value} not in {allowed:?}"));
    }
    if let Some(required) = schema["required"].as_array() {
        for key in required {
            let key = key.as_str().unwrap();
            if value.get(key).is_none() {
                errors.push(format!("{at}: missing {key}"));
            }
        }
    }
    if schema["additionalProperties"] == false
        && let (Some(props), Some(object)) = (schema["properties"].as_object(), value.as_object())
    {
        for key in object.keys().filter(|k| !props.contains_key(*k)) {
            errors.push(format!("{at}: unexpected {key}"));
        }
    }
    if let Some(props) = schema["properties"].as_object() {
        for (key, sub) in props {
            if let Some(v) = value.get(key) {
                schema_errors(sub, v, &format!("{at}.{key}"), errors);
            }
        }
    }
    if let Some(items) = schema.get("items") {
        for (i, v) in value.as_array().into_iter().flatten().enumerate() {
            schema_errors(items, v, &format!("{at}[{i}]"), errors);
        }
    }
}

pub fn nullable(kind: &str) -> Value {
    json!({ "anyOf": [{ "type": kind }, { "type": "null" }] })
}
