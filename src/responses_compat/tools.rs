use super::*;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResponseToolKind {
    Function,
    Custom,
}

#[derive(Debug, Clone)]
pub(super) struct ResponseToolMapping {
    pub(super) kind: ResponseToolKind,
    pub(super) namespace: Option<String>,
    pub(super) name: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ResponseToolMap {
    by_chat_name: HashMap<String, ResponseToolMapping>,
}

impl ResponseToolMap {
    pub(super) fn insert(
        &mut self,
        chat_name: String,
        kind: ResponseToolKind,
        namespace: Option<String>,
        name: String,
    ) {
        self.by_chat_name.insert(
            chat_name,
            ResponseToolMapping {
                kind,
                namespace,
                name,
            },
        );
    }

    pub(super) fn get(&self, chat_name: &str) -> Option<&ResponseToolMapping> {
        self.by_chat_name.get(chat_name)
    }
}

pub(super) fn map_tools(
    tools: &Value,
    tool_map: &mut ResponseToolMap,
) -> anyhow::Result<Vec<Value>> {
    let Some(items) = tools.as_array() else {
        anyhow::bail!("responses `tools` must be an array");
    };
    let mut mapped = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool definitions must be objects"))?;
        match object.get("type").and_then(Value::as_str) {
            Some("function") => {
                let name = response_tool_name(object, "function tool")?;
                mapped.push(function_tool_for_chat(object, &name, None)?);
                tool_map.insert(name.clone(), ResponseToolKind::Function, None, name);
            }
            Some("custom") => {
                let name = response_tool_name(object, "custom tool")?;
                mapped.push(custom_tool_for_chat(object, &name, None)?);
                tool_map.insert(name.clone(), ResponseToolKind::Custom, None, name);
            }
            Some("namespace") => {
                let namespace = string_field(object, "name")
                    .ok_or_else(|| anyhow::anyhow!("namespace tool requires name"))?;
                let namespace_description = object
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let Some(namespace_tools) = object.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                for namespace_tool in namespace_tools {
                    let namespace_tool = namespace_tool.as_object().ok_or_else(|| {
                        anyhow::anyhow!("namespace tool definitions must be objects")
                    })?;
                    let name = response_tool_name(namespace_tool, "namespace tool")?;
                    let chat_name = namespaced_tool_name(&namespace, &name);
                    match namespace_tool.get("type").and_then(Value::as_str) {
                        Some("function") => {
                            mapped.push(function_tool_for_chat(
                                namespace_tool,
                                &chat_name,
                                namespace_description.as_deref(),
                            )?);
                            tool_map.insert(
                                chat_name,
                                ResponseToolKind::Function,
                                Some(namespace.clone()),
                                name,
                            );
                        }
                        Some("custom") => {
                            mapped.push(custom_tool_for_chat(
                                namespace_tool,
                                &chat_name,
                                namespace_description.as_deref(),
                            )?);
                            tool_map.insert(
                                chat_name,
                                ResponseToolKind::Custom,
                                Some(namespace.clone()),
                                name,
                            );
                        }
                        Some(other) => {
                            anyhow::bail!("unsupported namespace tool type: {other}");
                        }
                        None => anyhow::bail!("namespace tool definition requires type"),
                    }
                }
            }
            Some(_) => {}
            None => anyhow::bail!("tool definition requires type"),
        }
    }
    Ok(mapped)
}

pub(super) fn map_tool_choice(
    tool_choice: &Value,
    _tool_map: &ResponseToolMap,
) -> anyhow::Result<Option<Value>> {
    if tool_choice.is_string() {
        return Ok(Some(tool_choice.clone()));
    }
    let Some(object) = tool_choice.as_object() else {
        anyhow::bail!("tool_choice must be a string or object");
    };
    if object.get("function").is_some() {
        return Ok(Some(tool_choice.clone()));
    }
    match object.get("type").and_then(Value::as_str) {
        Some("function" | "custom") => {
            let name = string_field(object, "name")
                .ok_or_else(|| anyhow::anyhow!("tool_choice requires name"))?;
            let chat_name = match string_field(object, "namespace") {
                Some(namespace) => namespaced_tool_name(&namespace, &name),
                None => name,
            };
            return Ok(Some(json!({
                "type": "function",
                "function": {
                    "name": chat_name,
                }
            })));
        }
        Some("namespace") => {
            return Ok(None);
        }
        _ => {}
    }
    Ok(None)
}

pub(super) fn response_tool_name(
    object: &Map<String, Value>,
    context: &str,
) -> anyhow::Result<String> {
    if let Some(name) = object
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        return Ok(name);
    }
    string_field(object, "name").ok_or_else(|| anyhow::anyhow!("{context} requires name"))
}

pub(super) fn function_tool_for_chat(
    object: &Map<String, Value>,
    chat_name: &str,
    namespace_description: Option<&str>,
) -> anyhow::Result<Value> {
    let mut function = object
        .get("function")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    function.insert("name".to_string(), Value::String(chat_name.to_string()));
    if !function.contains_key("description") {
        if let Some(description) = object.get("description") {
            function.insert("description".to_string(), description.clone());
        }
    }
    if let Some(namespace_description) = namespace_description {
        prepend_description(&mut function, namespace_description);
    }
    if !function.contains_key("parameters") {
        if let Some(parameters) = object.get("parameters") {
            function.insert("parameters".to_string(), parameters.clone());
        }
    }
    if !function.contains_key("strict") {
        if let Some(strict) = object.get("strict") {
            function.insert("strict".to_string(), strict.clone());
        }
    }
    Ok(json!({
        "type": "function",
        "function": Value::Object(function),
    }))
}

pub(super) fn custom_tool_for_chat(
    object: &Map<String, Value>,
    chat_name: &str,
    namespace_description: Option<&str>,
) -> anyhow::Result<Value> {
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(chat_name.to_string()));
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Free-form custom tool input.");
    function.insert(
        "description".to_string(),
        Value::String(format!(
            "{description}\nPut the exact custom tool input in the `input` string."
        )),
    );
    if let Some(namespace_description) = namespace_description {
        prepend_description(&mut function, namespace_description);
    }
    function.insert(
        "parameters".to_string(),
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Exact free-form input for this custom tool."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        }),
    );
    if let Some(strict) = object.get("strict") {
        function.insert("strict".to_string(), strict.clone());
    }
    Ok(json!({
        "type": "function",
        "function": Value::Object(function),
    }))
}

pub(super) fn prepend_description(function: &mut Map<String, Value>, namespace_description: &str) {
    if namespace_description.trim().is_empty() {
        return;
    }
    let existing = function
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let description = if existing.is_empty() {
        namespace_description.to_string()
    } else {
        format!("{namespace_description}\n{existing}")
    };
    function.insert("description".to_string(), Value::String(description));
}

pub(super) fn namespaced_tool_name(namespace: &str, name: &str) -> String {
    format!("{namespace}.{name}")
}

pub(super) fn custom_input_to_chat_arguments(input: &str) -> String {
    json!({ "input": input }).to_string()
}

pub(super) fn custom_input_from_chat_arguments(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(object)) => object
            .get("input")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Value::Object(object).to_string()),
        Ok(Value::String(input)) => input,
        Ok(value) => value.to_string(),
        Err(_) => arguments.to_string(),
    }
}
