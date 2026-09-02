use cc_switch_core::{
    AppType, SimpleProviderField, SimpleProviderFormDescriptor, SimpleProviderValues,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::{Host, Url};

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

impl AdapterReference {
    pub fn same_identity(&self, other: &Self) -> bool {
        self.plugin_id == other.plugin_id
            && self.plugin_version == other.plugin_version
            && self.adapter_id == other.adapter_id
            && self.contract_major == other.contract_major
            && self.schema_version == other.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecord {
    pub id: String,
    pub revision: u64,
    pub app_id: String,
    pub adapter: AdapterReference,
    pub name: String,
    pub settings: Map<String, Value>,
    #[serde(skip)]
    pub category: Option<String>,
    #[serde(skip)]
    pub metadata: Value,
    #[serde(default, flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentProvider {
    pub id: String,
    pub revision: u64,
}

impl From<&ProviderRecord> for CurrentProvider {
    fn from(provider: &ProviderRecord) -> Self {
        Self {
            id: provider.id.clone(),
            revision: provider.revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDraft {
    pub app_id: String,
    pub adapter: AdapterReference,
    pub name: String,
    pub settings: Map<String, Value>,
}

pub(crate) struct NativeImport {
    pub native_id: String,
    pub draft: ProviderDraft,
    pub name_is_explicit: bool,
    pub category: Option<String>,
    pub metadata: Value,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderUpdate {
    pub expected_revision: u64,
    pub name: String,
    pub settings: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleProviderDraft {
    pub app_id: String,
    pub name: String,
    pub values: SimpleProviderValues,
    #[serde(default)]
    pub preset_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderPresentation {
    pub website_url: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleProviderUpdate {
    pub expected_revision: u64,
    pub name: String,
    pub values: SimpleProviderValues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldKind {
    Text,
    Url,
    Secret,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Stable ownership marker for providers stored in CC Switch's native schema.
///
/// Native providers do not belong to a Lite plugin. The reference is derived
/// from the core application identifier and is therefore never persisted into
/// the shared provider row.
pub fn native_adapter_reference(app: &AppType) -> AdapterReference {
    AdapterReference {
        plugin_id: BUILTIN_PLUGIN_ID.to_owned(),
        plugin_version: BUILTIN_PLUGIN_VERSION.to_owned(),
        adapter_id: format!("builtin.{}.native", app.as_str()),
        contract_major: CONTRACT_MAJOR,
        schema_version: SCHEMA_VERSION,
        extensions: Map::new(),
    }
}

/// Whether Lite may mutate a provider owned by the shared native catalog.
///
/// Full CC Switch keeps a few provider categories whose live configuration is
/// outside Lite's config-writer boundary. They remain visible in the shared
/// catalog, but Lite must not edit, apply, or delete them.
pub fn is_lite_writable(provider: &ProviderRecord) -> bool {
    let Ok(app) = provider.app_id.parse::<AppType>() else {
        return false;
    };
    if !uses_direct_native_protocol(&app, provider) {
        return false;
    }
    let managed_provider = matches!(
        provider
            .metadata
            .get("providerType")
            .and_then(Value::as_str),
        Some("github_copilot" | "codex_oauth" | "xai_oauth")
    ) || provider
        .metadata
        .pointer("/authBinding/source")
        .and_then(Value::as_str)
        == Some("managed_account")
        || provider
            .settings
            .get("env")
            .and_then(Value::as_object)
            .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
            .and_then(Value::as_str)
            .or_else(|| provider.settings.get("baseUrl").and_then(Value::as_str))
            .is_some_and(|endpoint| {
                let endpoint = endpoint.to_ascii_lowercase();
                endpoint.contains("githubcopilot.com")
                    || endpoint.contains("chatgpt.com/backend-api/codex")
            });
    if managed_provider {
        return false;
    }
    if !provider
        .adapter
        .same_identity(&native_adapter_reference(&app))
    {
        return adapter_for_reference(&provider.app_id, &provider.adapter).is_some();
    }

    match app {
        AppType::OpenCode => {
            !matches!(provider.category.as_deref(), Some("omo") | Some("omo-slim"))
        }
        AppType::ClaudeDesktop => {
            provider.id == "claude-desktop-official"
                || provider.category.as_deref() == Some("official")
                || provider
                    .metadata
                    .get("claudeDesktopMode")
                    .and_then(Value::as_str)
                    != Some("proxy")
        }
        AppType::Hermes => {
            provider.settings.get("_cc_source").and_then(Value::as_str) != Some("providers_dict")
        }
        _ => true,
    }
}

/// Whether the simple editor can safely round-trip this stored provider.
pub fn is_lite_simple_editable(provider: &ProviderRecord) -> bool {
    is_lite_writable(provider)
        && provider.category.as_deref() != Some("official")
        && provider.extensions.contains_key("simpleValues")
}

fn uses_direct_native_protocol(app: &AppType, provider: &ProviderRecord) -> bool {
    if !matches!(app, AppType::Claude | AppType::Codex | AppType::GrokBuild) {
        return true;
    }
    if provider.metadata.get("isFullUrl").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let format = provider
        .metadata
        .get("apiFormat")
        .or_else(|| provider.settings.get("apiFormat"))
        .or_else(|| provider.settings.get("api_format"));
    let format_is_direct = match (app, format) {
        (_, None | Some(Value::Null)) => true,
        (AppType::Claude, Some(Value::String(format))) => matches!(
            format.trim(),
            "anthropic" | "anthropic_messages" | "anthropic-messages"
        ),
        (AppType::Codex | AppType::GrokBuild, Some(Value::String(format))) => {
            is_responses_protocol(format)
        }
        _ => false,
    };
    if !format_is_direct {
        return false;
    }
    if *app == AppType::Claude {
        return !provider
            .settings
            .get("openrouterCompatMode")
            .or_else(|| provider.settings.get("openrouter_compat_mode"))
            .is_some_and(compatibility_flag_enabled);
    }
    config_uses_responses(app, &provider.settings)
}

fn config_uses_responses(app: &AppType, settings: &Map<String, Value>) -> bool {
    let Some(config) = settings.get("config").and_then(Value::as_str) else {
        return true;
    };
    let Ok(document) = config.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    let protocol = if *app == AppType::GrokBuild {
        document
            .get("models")
            .and_then(|models| models.get("default"))
            .and_then(toml_edit::Item::as_str)
            .and_then(|selected| document.get("model")?.get(selected))
            .and_then(|model| model.get("api_backend"))
            .and_then(toml_edit::Item::as_str)
    } else {
        None
    }
    .or_else(|| {
        document
            .get("model_provider")
            .and_then(toml_edit::Item::as_str)
            .and_then(|selected| document.get("model_providers")?.get(selected))
            .and_then(|provider| provider.get("wire_api"))
            .and_then(toml_edit::Item::as_str)
    });
    protocol.is_none_or(is_responses_protocol)
}

fn is_responses_protocol(protocol: &str) -> bool {
    matches!(
        protocol.trim(),
        "responses" | "openai_responses" | "openai-responses"
    )
}

fn compatibility_flag_enabled(value: &Value) -> bool {
    match value {
        Value::Bool(enabled) => *enabled,
        Value::Number(number) => number.as_i64().is_some_and(|number| number != 0),
        Value::String(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

pub fn native_adapters() -> Vec<AdapterDescriptor> {
    AppType::all()
        .map(|app| AdapterDescriptor {
            app_id: app.as_str().to_owned(),
            display_name: "Native configuration".to_owned(),
            reference: native_adapter_reference(&app),
            fields: Vec::new(),
        })
        .collect()
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
                    "Stored privately by Lite and copied to Claude Code when switched.",
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
                    "Stored privately by Lite and copied to Codex when switched.",
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
        .find(|adapter| adapter.app_id == app_id && adapter.reference.same_identity(reference))
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

#[cfg(test)]
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
        if field.kind == FieldKind::Url && !value.is_empty() && !is_safe_http_url(value) {
            return Err(format!(
                "Setting '{}' must be HTTPS (or loopback HTTP) without credentials, query, or fragment",
                field.label
            ));
        }
    }

    for field in descriptor.fields.iter().filter(|field| field.required) {
        let present = settings
            .get(&field.key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        if !present {
            return Err(format!("Setting '{}' is required", field.label));
        }
    }
    Ok(())
}

pub fn validate_simple_provider_values(
    descriptor: &SimpleProviderFormDescriptor,
    values: &SimpleProviderValues,
) -> Result<(), String> {
    for field in descriptor.fields {
        let (label, value) = match field.key {
            SimpleProviderField::BaseUrl => ("Base URL", values.base_url.as_str()),
            SimpleProviderField::ApiKey => ("API key", values.api_key.as_str()),
            SimpleProviderField::Model => ("Model", values.model.as_str()),
        };
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("{label} is too large"));
        }
        if field.key == SimpleProviderField::BaseUrl
            && !value.trim().is_empty()
            && !is_safe_http_url(value.trim())
        {
            return Err(
                "Base URL must be HTTPS (or loopback HTTP) without credentials, query, or fragment"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn is_safe_http_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        (url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(&url)))
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
        }
        None => false,
    }
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
    fn native_adapters_cover_every_core_application() {
        let adapters = native_adapters();

        assert_eq!(
            adapters
                .iter()
                .map(|adapter| adapter.app_id.clone())
                .collect::<Vec<_>>(),
            AppType::all()
                .map(|app| app.as_str().to_owned())
                .collect::<Vec<_>>()
        );
        assert!(adapters.iter().all(|adapter| {
            adapter.reference == native_adapter_reference(&adapter.app_id.parse().unwrap())
                && adapter.fields.is_empty()
        }));
    }

    #[test]
    fn lite_rejects_full_version_only_provider_modes() {
        let native_provider = |app: AppType| ProviderRecord {
            id: "provider".to_owned(),
            revision: 1,
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name: "Provider".to_owned(),
            settings: Map::new(),
            category: None,
            metadata: json!({}),
            extensions: Map::new(),
        };

        let mut omo = native_provider(AppType::OpenCode);
        omo.category = Some("omo".to_owned());
        assert!(!is_lite_writable(&omo));

        let mut desktop_proxy = native_provider(AppType::ClaudeDesktop);
        desktop_proxy.metadata = json!({"claudeDesktopMode": "proxy"});
        assert!(!is_lite_writable(&desktop_proxy));

        let mut hermes_dictionary = native_provider(AppType::Hermes);
        hermes_dictionary
            .settings
            .insert("_cc_source".to_owned(), json!("providers_dict"));
        assert!(!is_lite_writable(&hermes_dictionary));

        let mut aggregator = native_provider(AppType::Claude);
        aggregator.category = Some("aggregator".to_owned());
        aggregator.metadata = json!({"apiFormat": "anthropic"});
        assert!(is_lite_writable(&aggregator));

        for provider_type in ["github_copilot", "codex_oauth", "xai_oauth"] {
            let mut managed = native_provider(AppType::Claude);
            managed.metadata = json!({"providerType": provider_type});
            assert!(!is_lite_writable(&managed));
        }

        let mut managed_binding = native_provider(AppType::Codex);
        managed_binding.metadata = json!({"authBinding": {"source": "managed_account"}});
        assert!(!is_lite_writable(&managed_binding));

        for endpoint in [
            "https://api.githubcopilot.com",
            "https://chatgpt.com/backend-api/codex",
        ] {
            let mut legacy_managed = native_provider(AppType::Claude);
            legacy_managed
                .settings
                .insert("env".to_owned(), json!({"ANTHROPIC_BASE_URL": endpoint}));
            assert!(!is_lite_writable(&legacy_managed));
        }
        let mut legacy_form_managed = native_provider(AppType::Claude);
        legacy_form_managed.adapter = built_in_adapters()[0].reference.clone();
        legacy_form_managed
            .settings
            .insert("baseUrl".to_owned(), json!("https://api.githubcopilot.com"));
        assert!(!is_lite_writable(&legacy_form_managed));

        for metadata in [
            json!({"apiFormat": "openai_chat"}),
            json!({"apiFormat": "openai_responses"}),
            json!({"apiFormat": "gemini_native"}),
            json!({"isFullUrl": true}),
        ] {
            let mut routed = native_provider(AppType::Claude);
            routed.metadata = metadata;
            assert!(!is_lite_writable(&routed));
        }
        let mut legacy_routed = native_provider(AppType::Claude);
        legacy_routed
            .settings
            .insert("openrouter_compat_mode".to_owned(), json!(true));
        assert!(!is_lite_writable(&legacy_routed));

        let mut direct = native_provider(AppType::Claude);
        direct.metadata = json!({"apiFormat": "anthropic"});
        assert!(is_lite_writable(&direct));

        for app in [AppType::Codex, AppType::GrokBuild] {
            let mut routed = native_provider(app.clone());
            routed.metadata = json!({"apiFormat": "openai_chat"});
            assert!(!is_lite_writable(&routed));

            let mut direct = native_provider(app);
            direct.metadata = json!({"apiFormat": "openai_responses"});
            assert!(is_lite_writable(&direct));
        }
        let mut codex_chat = native_provider(AppType::Codex);
        codex_chat.settings.insert(
            "config".to_owned(),
            json!(
                r#"model_provider = "custom"
[model_providers.custom]
wire_api = "chat"
"#
            ),
        );
        assert!(!is_lite_writable(&codex_chat));
        let mut grok_chat = native_provider(AppType::GrokBuild);
        grok_chat.settings.insert(
            "config".to_owned(),
            json!(
                r#"[models]
default = "custom"
[model.custom]
api_backend = "anthropic"
"#
            ),
        );
        assert!(!is_lite_writable(&grok_chat));

        assert!(is_lite_writable(&native_provider(AppType::Pi)));
    }

    #[test]
    fn lite_keeps_external_adapter_records_read_only() {
        let app = AppType::Claude;
        let mut provider = ProviderRecord {
            id: "provider".to_owned(),
            revision: 1,
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name: "Provider".to_owned(),
            settings: Map::new(),
            category: None,
            metadata: json!({}),
            extensions: Map::new(),
        };

        provider.adapter.plugin_id = "dev.example.adapter".to_owned();
        provider.adapter.adapter_id = "example.claude".to_owned();
        assert!(!is_lite_writable(&provider));

        provider.adapter = built_in_adapters()[0].reference.clone();
        assert!(is_lite_writable(&provider));
    }

    #[test]
    fn validation_rejects_unknown_missing_and_malformed_settings() {
        let descriptor = &built_in_adapters()[0];

        assert!(validate_settings(descriptor, &settings(json!({"apiKey": "secret"}))).is_ok());
        assert!(validate_settings(descriptor, &settings(json!({}))).is_err());
        assert!(validate_settings(descriptor, &settings(json!({"apiKey": "   "}))).is_err());
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
        assert!(validate_settings(
            descriptor,
            &settings(json!({"apiKey": "secret", "baseUrl": "https://"}))
        )
        .is_err());
        assert!(validate_settings(
            descriptor,
            &settings(json!({
                "apiKey": "secret",
                "baseUrl": "http://remote.example.com"
            }))
        )
        .is_err());
        assert!(validate_settings(
            descriptor,
            &settings(json!({"apiKey": "secret", "baseUrl": "http://127.0.0.1:8080"}))
        )
        .is_ok());
        assert!(validate_settings(
            descriptor,
            &settings(json!({
                "apiKey": "secret",
                "baseUrl": "https://gateway.example/v1?tenant=private"
            }))
        )
        .is_err());
        assert!(validate_settings(
            descriptor,
            &settings(json!({
                "apiKey": "secret",
                "baseUrl": "https://user:password@proxy.example.com"
            }))
        )
        .is_err());
    }

    #[test]
    fn simple_validation_accepts_only_safe_bounded_endpoints() {
        let descriptor = cc_switch_core::simple_provider_form(&AppType::Claude);
        let values = |base_url: &str| SimpleProviderValues::new(base_url, "secret", "");

        assert!(
            validate_simple_provider_values(descriptor, &values("https://example.com/v1")).is_ok()
        );
        assert!(
            validate_simple_provider_values(descriptor, &values("http://localhost:8080/v1"))
                .is_ok()
        );
        assert!(
            validate_simple_provider_values(descriptor, &values("http://example.com/v1")).is_err()
        );
        assert!(validate_simple_provider_values(
            descriptor,
            &SimpleProviderValues::new("", "x".repeat(MAX_VALUE_BYTES + 1), ""),
        )
        .is_err());
    }
}
