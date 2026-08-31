use cc_switch_core::AppType;
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderUpdate {
    pub expected_revision: u64,
    pub name: String,
    pub settings: Map<String, Value>,
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
    if managed_provider || provider.category.as_deref() == Some("aggregator") {
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
    fn lite_keeps_full_version_native_categories_read_only() {
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
        assert!(!is_lite_writable(&aggregator));

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
}
