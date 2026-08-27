use cc_switch_core::AppType;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const BUILTIN_PLUGIN_ID: &str = "org.cc-switch.builtin";
pub const BUILTIN_PLUGIN_VERSION: &str = "0.1.0";
pub const CONTRACT_MAJOR: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;
const MAX_NAME_CHARS: usize = 80;
const MAX_VALUE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterReference {
    pub plugin_id: String,
    pub plugin_version: String,
    pub adapter_id: String,
    pub contract_major: u32,
    pub schema_version: u32,
    #[serde(default, flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecord {
    pub id: String,
    pub app_id: String,
    pub adapter: AdapterReference,
    pub name: String,
    pub settings: Map<String, Value>,
    #[serde(default, flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDraft {
    pub app_id: String,
    pub adapter: AdapterReference,
    pub name: String,
    pub settings: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderUpdate {
    pub name: String,
    pub settings: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldKind {
    Text,
    Url,
    Secret,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    pub placeholder: String,
    pub help: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterDescriptor {
    pub app_id: String,
    pub display_name: String,
    pub reference: AdapterReference,
    pub fields: Vec<FormField>,
}

impl AdapterDescriptor {
    fn built_in(
        app: AppType,
        adapter_id: &str,
        display_name: &str,
        fields: Vec<FormField>,
    ) -> Self {
        Self {
            app_id: app.as_str().to_owned(),
            display_name: display_name.to_owned(),
            reference: AdapterReference {
                plugin_id: BUILTIN_PLUGIN_ID.to_owned(),
                // This version identifies stored data ownership. It only changes
                // alongside an explicit compatibility or migration path.
                plugin_version: BUILTIN_PLUGIN_VERSION.to_owned(),
                adapter_id: adapter_id.to_owned(),
                contract_major: CONTRACT_MAJOR,
                schema_version: SCHEMA_VERSION,
                extensions: Map::new(),
            },
            fields,
        }
    }
}

fn field(
    key: &str,
    label: &str,
    kind: FieldKind,
    required: bool,
    placeholder: &str,
    help: &str,
) -> FormField {
    FormField {
        key: key.to_owned(),
        label: label.to_owned(),
        kind,
        required,
        placeholder: placeholder.to_owned(),
        help: help.to_owned(),
    }
}

pub fn built_in_adapters() -> Vec<AdapterDescriptor> {
    vec![
        AdapterDescriptor::built_in(
            AppType::Claude,
            "builtin.claude.api-key",
            "Claude API",
            vec![
                field(
                    "baseUrl",
                    "Base URL",
                    FieldKind::Url,
                    false,
                    "https://api.anthropic.com",
                    "Leave empty to use the default Anthropic endpoint.",
                ),
                field(
                    "apiKey",
                    "API key",
                    FieldKind::Secret,
                    true,
                    "sk-ant-…",
                    "Stored only in CC Switch Lite's private provider file.",
                ),
                field(
                    "model",
                    "Model",
                    FieldKind::Text,
                    false,
                    "claude-sonnet-4-6",
                    "Optional model override.",
                ),
            ],
        ),
        AdapterDescriptor::built_in(
            AppType::Codex,
            "builtin.codex.api-key",
            "OpenAI API",
            vec![
                field(
                    "baseUrl",
                    "Base URL",
                    FieldKind::Url,
                    false,
                    "https://api.openai.com/v1",
                    "Leave empty to use the default OpenAI endpoint.",
                ),
                field(
                    "apiKey",
                    "API key",
                    FieldKind::Secret,
                    true,
                    "sk-…",
                    "Stored only in CC Switch Lite's private provider file.",
                ),
                field(
                    "model",
                    "Model",
                    FieldKind::Text,
                    false,
                    "gpt-5",
                    "Optional model override.",
                ),
            ],
        ),
    ]
}

pub fn adapter_for_reference(
    app_id: &str,
    reference: &AdapterReference,
) -> Option<AdapterDescriptor> {
    built_in_adapters()
        .into_iter()
        .find(|adapter| adapter.app_id == app_id && adapter.reference == *reference)
}

pub fn validate_name(name: &str) -> Result<String, String> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return Err("Provider name is required".to_owned());
    }
    if normalized.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "Provider name must be at most {MAX_NAME_CHARS} characters"
        ));
    }
    Ok(normalized.to_owned())
}

pub fn validate_settings(
    descriptor: &AdapterDescriptor,
    settings: &Map<String, Value>,
) -> Result<(), String> {
    for (key, value) in settings {
        let Some(field) = descriptor.fields.iter().find(|field| field.key == *key) else {
            return Err(format!("Unknown setting '{key}'"));
        };
        let Some(value) = value.as_str() else {
            return Err(format!("Setting '{}' must be a string", field.label));
        };
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("Setting '{}' is too large", field.label));
        }
        if field.kind == FieldKind::Url
            && !value.is_empty()
            && !(value.starts_with("https://") || value.starts_with("http://"))
        {
            return Err(format!(
                "Setting '{}' must use http:// or https://",
                field.label
            ));
        }
    }

    for field in descriptor.fields.iter().filter(|field| field.required) {
        let present = settings
            .get(&field.key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        if !present {
            return Err(format!("Setting '{}' is required", field.label));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: Value) -> Map<String, Value> {
        value.as_object().expect("settings object").clone()
    }

    #[test]
    fn descriptors_are_stable_and_schema_driven() {
        let adapters = built_in_adapters();

        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters[0].reference.adapter_id, "builtin.claude.api-key");
        assert_eq!(adapters[1].reference.adapter_id, "builtin.codex.api-key");
        assert!(adapters
            .iter()
            .all(|adapter| adapter.fields.iter().any(|field| {
                field.key == "apiKey" && field.kind == FieldKind::Secret && field.required
            })));
    }

    #[test]
    fn validation_rejects_unknown_missing_and_malformed_settings() {
        let descriptor = &built_in_adapters()[0];

        assert!(validate_settings(descriptor, &settings(json!({"apiKey": "secret"}))).is_ok());
        assert!(validate_settings(descriptor, &settings(json!({}))).is_err());
        assert!(validate_settings(
            descriptor,
            &settings(json!({"apiKey": "secret", "unknown": "value"}))
        )
        .is_err());
        assert!(validate_settings(
            descriptor,
            &settings(json!({"apiKey": "secret", "baseUrl": "file:///tmp/config"}))
        )
        .is_err());
    }
}
