//! The assistant's settings and its API keys. The settings (providers,
//! which one is selected, redaction, the step limit) are a JSON file at a
//! path the shell chooses; reading is forgiving, so a field that is missing,
//! of the wrong type or unknown falls back to its default instead of losing
//! the rest. API keys are never in that file: they live in the system key
//! store (Keychain, Credential Manager, the Secret Service or keyutils)
//! behind the [`KeyStore`] trait, under the service `Aster Office` and the
//! account `ai.provider.<id>.key`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use crate::agent::{AgentOptions, DEFAULT_MAX_STEPS};
use crate::provider::{Preset, ProviderConfig, ProviderKind};

/// The most steps the settings allow per turn.
pub const MAX_STEPS_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiSettings {
    pub providers: Vec<ProviderConfig>,
    /// The id of the provider in use, one of `providers`.
    pub selected: Option<String>,
    /// Mask likely secrets before sending; on unless the user turns it off.
    pub redact: bool,
    /// Model requests per user turn.
    pub max_steps: usize,
}

impl Default for AiSettings {
    /// One entry for each ready-made provider, Anthropic selected.
    fn default() -> Self {
        let providers: Vec<ProviderConfig> = [Preset::Anthropic, Preset::OpenAi, Preset::DeepSeek]
            .into_iter()
            .map(|preset| ProviderConfig::from_preset(preset.key(), preset))
            .collect();
        Self {
            selected: providers.first().map(|provider| provider.id.clone()),
            providers,
            redact: true,
            max_steps: DEFAULT_MAX_STEPS,
        }
    }
}

impl AiSettings {
    /// The settings in `path`, or the defaults when the file is missing or
    /// not JSON.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| Self::from_json(&value))
            .unwrap_or_default()
    }

    /// Write the settings to `path`, through a temporary file so a crash
    /// never leaves half a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(&self.to_json()).map_err(io::Error::other)?;
        text.push('\n');
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, path)
    }

    pub fn to_json(&self) -> Value {
        let providers: Vec<Value> = self
            .providers
            .iter()
            .map(|provider| {
                json!({
                    "id": provider.id,
                    "kind": provider.kind.key(),
                    "preset": provider.preset.key(),
                    "name": provider.name,
                    "base_url": provider.base_url,
                    "model": provider.model,
                    "allow_insecure": provider.allow_insecure,
                    "key_host": provider.key_host,
                })
            })
            .collect();
        json!({
            "providers": providers,
            "selected": self.selected,
            "redact": self.redact,
            "max_steps": self.max_steps,
        })
    }

    pub fn from_json(value: &Value) -> Self {
        let defaults = Self::default();
        let providers = match value.get("providers").and_then(Value::as_array) {
            Some(list) => {
                let mut seen = HashSet::new();
                list.iter()
                    .filter_map(|entry| entry.as_object().and_then(provider_from))
                    .filter(|provider| seen.insert(provider.id.clone()))
                    .collect()
            }
            None => defaults.providers,
        };
        let selected = value
            .get("selected")
            .and_then(Value::as_str)
            .filter(|id| providers.iter().any(|provider| provider.id == *id))
            .map(str::to_string)
            .or_else(|| providers.first().map(|provider| provider.id.clone()));
        let max_steps = value
            .get("max_steps")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_STEPS, |steps| {
                (steps as usize).clamp(1, MAX_STEPS_LIMIT)
            });
        Self {
            providers,
            selected,
            redact: value
                .get("redact")
                .and_then(Value::as_bool)
                .unwrap_or(defaults.redact),
            max_steps,
        }
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    pub fn selected_provider(&self) -> Option<&ProviderConfig> {
        self.selected.as_deref().and_then(|id| self.provider(id))
    }

    /// The loop options these settings ask for.
    pub fn agent_options(&self) -> AgentOptions {
        AgentOptions {
            max_steps: self.max_steps,
            redact: self.redact,
        }
    }
}

/// One provider entry; `None` when it has no usable id. The other fields
/// fall back to the preset's defaults.
fn provider_from(entry: &Map<String, Value>) -> Option<ProviderConfig> {
    let text = |field: &str| entry.get(field).and_then(Value::as_str);
    let id = text("id").map(str::trim).filter(|id| !id.is_empty())?;
    let kind = text("kind").and_then(ProviderKind::from_key);
    let preset = text("preset")
        .and_then(Preset::from_key)
        .filter(|preset| kind.is_none_or(|kind| preset.kind() == kind))
        .unwrap_or(match kind {
            Some(ProviderKind::Anthropic) => Preset::Anthropic,
            _ => Preset::Custom,
        });
    let mut provider = ProviderConfig::from_preset(id, preset);
    if let Some(name) = text("name").filter(|name| !name.trim().is_empty()) {
        provider.name = name.to_string();
    }
    if let Some(base_url) = text("base_url").filter(|url| !url.trim().is_empty()) {
        provider.base_url = base_url.trim().to_string();
    }
    if let Some(model) = text("model") {
        provider.model = model.trim().to_string();
    }
    // Only a real `true` turns insecure connections on; anything else, a
    // missing field included, leaves them off.
    provider.allow_insecure = entry
        .get("allow_insecure")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    provider.key_host = text("key_host")
        .map(|host| host.trim().to_ascii_lowercase())
        .filter(|host| !host.is_empty());
    Some(provider)
}

/// The key store's service name.
pub const KEY_SERVICE: &str = "Aster Office";

/// The key store account for a provider's API key.
pub fn key_account(provider_id: &str) -> String {
    format!("ai.provider.{provider_id}.key")
}

/// A key store failure. The message never contains a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyStoreError(pub String);

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key store: {}", self.0)
    }
}

impl std::error::Error for KeyStoreError {}

/// Where API keys are kept, by provider id.
pub trait KeyStore: Send + Sync {
    /// The key, or `None` when there is none.
    fn get(&self, provider_id: &str) -> Result<Option<String>, KeyStoreError>;
    fn set(&self, provider_id: &str, key: &str) -> Result<(), KeyStoreError>;
    /// Remove the key; removing a key that is not there is not an error.
    fn delete(&self, provider_id: &str) -> Result<(), KeyStoreError>;
}

/// The operating system's key store, through `keyring`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemKeyStore;

impl SystemKeyStore {
    fn entry(provider_id: &str) -> Result<keyring::Entry, KeyStoreError> {
        keyring::Entry::new(KEY_SERVICE, &key_account(provider_id))
            .map_err(|error| KeyStoreError(error.to_string()))
    }
}

impl KeyStore for SystemKeyStore {
    fn get(&self, provider_id: &str) -> Result<Option<String>, KeyStoreError> {
        match Self::entry(provider_id)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(KeyStoreError(error.to_string())),
        }
    }

    fn set(&self, provider_id: &str, key: &str) -> Result<(), KeyStoreError> {
        Self::entry(provider_id)?
            .set_password(key)
            .map_err(|error| KeyStoreError(error.to_string()))
    }

    fn delete(&self, provider_id: &str) -> Result<(), KeyStoreError> {
        match Self::entry(provider_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(KeyStoreError(error.to_string())),
        }
    }
}

/// Keys in memory only: for tests, and for a session where the user chose
/// not to store the key. No `Debug`, so keys cannot end up in a log.
#[derive(Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<String, String>>,
}

impl MemoryKeyStore {
    fn keys(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, provider_id: &str) -> Result<Option<String>, KeyStoreError> {
        Ok(self.keys().get(provider_id).cloned())
    }

    fn set(&self, provider_id: &str, key: &str) -> Result<(), KeyStoreError> {
        self.keys().insert(provider_id.to_string(), key.to_string());
        Ok(())
    }

    fn delete(&self, provider_id: &str) -> Result<(), KeyStoreError> {
        self.keys().remove(provider_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("ai.json");
        let mut settings = AiSettings::default();
        let mut local = ProviderConfig::from_preset("local", Preset::Custom);
        local.name = "Ollama".into();
        local.model = "qwen3:8b".into();
        settings.providers.push(local);
        let mut lan = ProviderConfig::from_preset("lan", Preset::Custom);
        lan.base_url = "http://192.168.1.5:8080/v1".into();
        lan.allow_insecure = true;
        lan.key_host = Some("192.168.1.5".into());
        settings.providers.push(lan);
        settings.selected = Some("local".into());
        settings.redact = false;
        settings.max_steps = 10;
        settings.save(&path).unwrap();
        assert_eq!(AiSettings::load(&path), settings);
        let text = fs::read_to_string(&path).unwrap();
        let saved: Value = serde_json::from_str(&text).unwrap();
        for provider in saved["providers"].as_array().unwrap() {
            let fields: Vec<&str> = provider
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            for field in &fields {
                assert!(
                    [
                        "id",
                        "kind",
                        "preset",
                        "name",
                        "base_url",
                        "model",
                        "allow_insecure",
                        "key_host"
                    ]
                    .contains(field),
                    "no keys in the settings file: {field}"
                );
            }
        }
        assert_eq!(saved["providers"][4]["allow_insecure"], true);
        assert_eq!(saved["providers"][4]["key_host"], "192.168.1.5");
        assert_eq!(saved["providers"][0]["allow_insecure"], false);
        assert_eq!(saved["providers"][0]["key_host"], Value::Null);
    }

    #[test]
    fn insecure_and_key_host_default_to_off_and_none() {
        let value = json!({
            "providers": [
                {"id": "old", "kind": "openai_compatible", "base_url": "http://10.0.0.2/v1"},
                {"id": "odd", "kind": "openai_compatible", "allow_insecure": "true", "key_host": 5},
                {"id": "blank", "kind": "openai_compatible", "allow_insecure": 1, "key_host": "  "},
                {"id": "set", "kind": "openai_compatible", "allow_insecure": true, "key_host": " API.Example.com "}
            ]
        });
        let settings = AiSettings::from_json(&value);
        for id in ["old", "odd", "blank"] {
            let provider = settings.provider(id).unwrap();
            assert!(!provider.allow_insecure, "{id}");
            assert_eq!(provider.key_host, None, "{id}");
        }
        let set = settings.provider("set").unwrap();
        assert!(set.allow_insecure);
        assert_eq!(set.key_host.as_deref(), Some("api.example.com"));
        // Defaults have neither.
        for provider in AiSettings::default().providers {
            assert!(!provider.allow_insecure);
            assert_eq!(provider.key_host, None);
        }
    }

    #[test]
    fn missing_and_odd_fields_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            AiSettings::load(&dir.path().join("absent.json")),
            AiSettings::default()
        );
        let defaults = AiSettings::default();
        assert_eq!(defaults.selected.as_deref(), Some("anthropic"));
        assert!(defaults.redact);
        assert_eq!(defaults.max_steps, 25);
        assert_eq!(
            defaults.selected_provider().unwrap().model,
            "claude-opus-5-5"
        );

        let value = json!({
            "providers": [
                {"id": "a", "kind": "anthropic", "unknown": 1},
                {"id": "a", "kind": "openai_compatible"},
                {"kind": "anthropic"},
                {"id": "o", "preset": "openai", "model": " gpt-x "},
                {"id": "c", "kind": "openai_compatible", "base_url": "http://127.0.0.1:8080/v1"},
                "not an object"
            ],
            "selected": "gone",
            "redact": "yes",
            "max_steps": 1000,
            "future_field": true
        });
        let settings = AiSettings::from_json(&value);
        let ids: Vec<&str> = settings.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "o", "c"]);
        assert_eq!(settings.providers[0].base_url, "https://api.anthropic.com");
        assert_eq!(settings.providers[0].model, "claude-opus-5-5");
        assert_eq!(settings.providers[1].kind, ProviderKind::OpenAiCompatible);
        assert_eq!(settings.providers[1].base_url, "https://api.openai.com/v1");
        assert_eq!(settings.providers[1].model, "gpt-x");
        assert_eq!(settings.providers[2].preset, Preset::Custom);
        assert_eq!(settings.selected.as_deref(), Some("a"));
        assert!(settings.redact);
        assert_eq!(settings.max_steps, MAX_STEPS_LIMIT);

        let broken = dir.path().join("broken.json");
        fs::write(&broken, "{ not json").unwrap();
        assert_eq!(AiSettings::load(&broken), AiSettings::default());
    }

    #[test]
    fn keys_are_stored_by_provider() {
        assert_eq!(key_account("anthropic"), "ai.provider.anthropic.key");
        let store = MemoryKeyStore::default();
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", "k1").unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("k1"));
        store.delete("a").unwrap();
        store.delete("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
    }
}
