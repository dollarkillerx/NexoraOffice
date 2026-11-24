//! Model providers. Two wire formats cover them: Anthropic's Messages API
//! and the OpenAI-compatible Chat Completions API (OpenAI, DeepSeek, and
//! local or self-hosted servers such as Ollama). Each takes the
//! provider-neutral conversation, streams the reply over server-sent
//! events, sends its text to the caller as it arrives, and hands back the
//! finished message and why it stopped.
//!
//! Everything runs on the caller's thread and blocks; a cancel flag stops a
//! stream between chunks.

mod anthropic;
mod openai;

use std::sync::atomic::AtomicBool;

use aster_tools::ToolSpec;
use serde_json::Value;

use crate::conversation::{ContentBlock, Message};
use crate::error::AiError;
use crate::http::{self, Body};
use crate::sse;

/// Which wire format a provider speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Anthropic,
    OpenAiCompatible,
}

impl ProviderKind {
    pub fn key(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAiCompatible => "openai_compatible",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "anthropic" => Some(ProviderKind::Anthropic),
            "openai_compatible" => Some(ProviderKind::OpenAiCompatible),
            _ => None,
        }
    }
}

/// The providers the settings offer ready-made, and `Custom` for any other
/// OpenAI-compatible server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    Anthropic,
    OpenAi,
    DeepSeek,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Anthropic,
        Preset::OpenAi,
        Preset::DeepSeek,
        Preset::Custom,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Preset::Anthropic => "anthropic",
            Preset::OpenAi => "openai",
            Preset::DeepSeek => "deepseek",
            Preset::Custom => "custom",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Preset::ALL.into_iter().find(|preset| preset.key() == key)
    }

    pub fn kind(self) -> ProviderKind {
        match self {
            Preset::Anthropic => ProviderKind::Anthropic,
            _ => ProviderKind::OpenAiCompatible,
        }
    }

    /// The provider's name as the settings show it before the user renames
    /// it. These are product names, so they are not translated.
    pub fn display_name(self) -> &'static str {
        match self {
            Preset::Anthropic => "Anthropic",
            Preset::OpenAi => "OpenAI",
            Preset::DeepSeek => "DeepSeek",
            Preset::Custom => "Custom",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Preset::Anthropic => anthropic::DEFAULT_BASE_URL,
            Preset::OpenAi => "https://api.openai.com/v1",
            Preset::DeepSeek => "https://api.deepseek.com",
            // Ollama's OpenAI-compatible endpoint, the most common local
            // server.
            Preset::Custom => "http://localhost:11434/v1",
        }
    }

    /// The model to start with. Only Anthropic has one; for the others the
    /// user picks from [`Provider::list_models`], since their model lists
    /// change too often to hard-code.
    pub fn default_model(self) -> &'static str {
        match self {
            Preset::Anthropic => anthropic::DEFAULT_MODEL,
            _ => "",
        }
    }

    /// Whether the provider needs an API key. Local servers usually do not.
    pub fn needs_key(self) -> bool {
        !matches!(self, Preset::Custom)
    }
}

/// One configured provider, as the settings file keeps it. It never holds
/// the API key, which lives in the system key store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Stable and unique among the providers; names the key in the key
    /// store.
    pub id: String,
    pub kind: ProviderKind,
    pub preset: Preset,
    pub name: String,
    pub base_url: String,
    pub model: String,
}

impl ProviderConfig {
    pub fn from_preset(id: impl Into<String>, preset: Preset) -> Self {
        Self {
            id: id.into(),
            kind: preset.kind(),
            preset,
            name: preset.display_name().to_string(),
            base_url: preset.default_base_url().to_string(),
            model: preset.default_model().to_string(),
        }
    }

    /// The caller name for tool calls and the audit log:
    /// `ai:<provider>/<model>`.
    pub fn caller(&self) -> String {
        format!("ai:{}/{}", self.id, self.model)
    }

    /// The base URL without trailing slashes.
    pub(crate) fn base(&self) -> &str {
        self.base_url.trim().trim_end_matches('/')
    }
}

/// A tool as the model sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// The JSON Schema of the arguments.
    pub schema: Value,
}

/// The tool list for the model, sorted by name. Built once and reused, it is
/// byte for byte the same on every request, which keeps the providers'
/// prompt caches valid.
pub fn tool_defs(specs: &[ToolSpec]) -> Vec<ToolDef> {
    let mut tools: Vec<ToolDef> = specs
        .iter()
        .map(|spec| ToolDef {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            schema: spec.schema(),
        })
        .collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

/// Why the model stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// It finished its answer.
    EndTurn,
    /// It wants the tool calls in its message run.
    ToolUse,
    /// It ran into the output limit, mid-answer.
    MaxTokens,
    /// It, or the provider's safety filter, declined.
    Refusal,
    /// Anything else, by the provider's name for it.
    Other(String),
}

/// One finished reply.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelResponse {
    pub content: Vec<ContentBlock>,
    pub stop: StopReason,
}

/// A model a provider offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    /// A readable name, when the provider gives one.
    pub name: Option<String>,
}

/// What one request to a model carries.
pub struct Request<'a> {
    pub system: &'a str,
    pub tools: &'a [ToolDef],
    pub messages: &'a [Message],
}

/// A model provider, ready to call.
pub trait Provider: Send + Sync {
    fn config(&self) -> &ProviderConfig;

    /// Send `request` and stream the reply: `on_text` gets the text as it
    /// arrives. Blocks until the reply is complete, fails, or `cancel` is
    /// set ([`AiError::Cancelled`]).
    fn stream(
        &self,
        request: &Request<'_>,
        on_text: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<ModelResponse, AiError>;

    /// The models the provider offers, sorted by id.
    fn list_models(&self, cancel: &AtomicBool) -> Result<Vec<ModelInfo>, AiError>;
}

/// A provider for `config`, with its API key if it has one. The key is
/// kept in memory only and sent only to `config.base_url`.
pub fn connect(config: ProviderConfig, api_key: Option<String>) -> Box<dyn Provider> {
    let api_key = api_key.filter(|key| !key.trim().is_empty());
    match config.kind {
        ProviderKind::Anthropic => Box::new(anthropic::Anthropic::new(config, api_key)),
        ProviderKind::OpenAiCompatible => Box::new(openai::OpenAi::new(config, api_key)),
    }
}

/// Reads one provider's stream: each server-sent event goes to `event`,
/// then `finish` builds the reply.
pub(crate) trait Decoder {
    fn event(&mut self, event: &sse::Event, on_text: &mut dyn FnMut(&str)) -> Result<(), AiError>;
    fn finish(self) -> Result<ModelResponse, AiError>;
}

/// Read `body` as server-sent events through `decoder`.
pub(crate) fn decode<D: Decoder>(
    mut body: Body,
    mut decoder: D,
    on_text: &mut dyn FnMut(&str),
    cancel: &AtomicBool,
) -> Result<ModelResponse, AiError> {
    let mut parser = sse::Parser::default();
    while let Some(chunk) = body.next(cancel)? {
        for event in parser.feed(&chunk) {
            decoder.event(&event, on_text)?;
        }
    }
    if let Some(event) = parser.finish() {
        decoder.event(&event, on_text)?;
    }
    decoder.finish()
}

/// A tool call's input from the JSON text the model streamed: the object,
/// or an empty object and the text when it is not a JSON object.
pub(crate) fn parse_input(json: &str) -> (Value, Option<String>) {
    if json.trim().is_empty() {
        return (Value::Object(Default::default()), None);
    }
    match serde_json::from_str::<Value>(json) {
        Ok(value @ Value::Object(_)) => (value, None),
        _ => (Value::Object(Default::default()), Some(json.to_string())),
    }
}

pub(crate) fn parse_json(data: &str) -> Result<Value, AiError> {
    serde_json::from_str(data).map_err(|_| {
        AiError::BadResponse(format!(
            "an event is not JSON: {}",
            crate::error::shorten(data)
        ))
    })
}

/// The reply was cut before the provider said it was complete.
pub(crate) fn cut_off() -> AiError {
    AiError::Network("the connection closed before the reply was complete".into())
}

/// Fetch a model list as JSON.
pub(crate) fn fetch_json(
    agent: &ureq::Agent,
    request: http::HttpRequest,
    cancel: &AtomicBool,
) -> Result<Value, AiError> {
    let text = http::send(agent, request).text(cancel)?;
    serde_json::from_str(&text)
        .map_err(|_| AiError::BadResponse("the model list is not JSON".into()))
}

/// The `data` array of a model list (both formats use it).
pub(crate) fn models_from(
    list: &Value,
    name_field: Option<&str>,
) -> Result<Vec<ModelInfo>, AiError> {
    let data = list
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::BadResponse("the model list has no data".into()))?;
    let mut models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.to_string();
            let name = name_field
                .and_then(|field| model.get(field))
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(ModelInfo { id, name })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_list_is_sorted_and_stable() {
        let first = tool_defs(aster_tools::tools());
        let second = tool_defs(aster_tools::tools());
        assert_eq!(first, second);
        assert!(first.windows(2).all(|pair| pair[0].name < pair[1].name));
        let text = |tools: &[ToolDef]| {
            tools
                .iter()
                .map(|tool| tool.schema.to_string())
                .collect::<String>()
        };
        assert_eq!(text(&first), text(&second));
    }

    #[test]
    fn inputs_must_be_json_objects() {
        assert_eq!(parse_input(""), (serde_json::json!({}), None));
        assert_eq!(
            parse_input(r#"{"a":1}"#),
            (serde_json::json!({"a": 1}), None)
        );
        assert_eq!(
            parse_input(r#"{"a":"#),
            (serde_json::json!({}), Some(r#"{"a":"#.into()))
        );
        assert_eq!(
            parse_input("[1]"),
            (serde_json::json!({}), Some("[1]".into()))
        );
    }

    #[test]
    fn presets_fill_in_their_defaults() {
        let config = ProviderConfig::from_preset("main", Preset::Anthropic);
        assert_eq!(config.kind, ProviderKind::Anthropic);
        assert_eq!(config.base_url, "https://api.anthropic.com");
        assert_eq!(config.model, "claude-opus-5-5");
        assert_eq!(config.caller(), "ai:main/claude-opus-5-5");
        let deepseek = ProviderConfig::from_preset("ds", Preset::DeepSeek);
        assert_eq!(deepseek.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(deepseek.model, "");
        for preset in Preset::ALL {
            assert_eq!(Preset::from_key(preset.key()), Some(preset));
        }
    }
}
