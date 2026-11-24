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

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::AtomicBool;

use aster_tools::ToolSpec;
use serde_json::Value;

use crate::conversation::{ContentBlock, Message};
use crate::error::AiError;
use crate::http::{self, Body};
use crate::limits::{
    MAX_RESPONSE_CHARS, MAX_TOOL_CALLS, MAX_TOOL_INPUT_BYTES, MAX_TOOL_NAME_BYTES,
};
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
    /// Let requests go over plain `http://` to an address that is not this
    /// computer. Off by default: on any other network, the API key and the
    /// whole conversation could be read on the way.
    pub allow_insecure: bool,
    /// Where the API key was saved for: the origin from [`url_origin`]
    /// (scheme, host and port), so a key typed for `http://localhost:11434`
    /// goes neither to another port nor, once the address turns to https or
    /// back, over another scheme. Settings written before origins were kept
    /// hold a bare host, compared as a host. `None` for keys saved before
    /// either was kept; those go to whatever the address is, as they always
    /// did.
    pub key_host: Option<String>,
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
            allow_insecure: false,
            key_host: None,
        }
    }

    /// Whether the saved key may go to the current address: there is no
    /// recorded host, or it is the address's host.
    pub fn key_matches_host(&self) -> bool {
        match parse_url(&self.base_url) {
            Some(uri) => key_matches(self.key_host.as_deref(), &uri),
            None => self.key_host.is_none(),
        }
    }

    /// Whether a request to `url` may go out, `with_key` saying whether it
    /// would carry the API key. Checked before every request, on the full
    /// URL, parsed the way the HTTP client parses it, so the host checked
    /// is the host connected to.
    pub(crate) fn check_request(&self, url: &str, with_key: bool) -> Result<(), AiError> {
        let Some(uri) = parse_url(url) else {
            return Err(AiError::NotConfigured(format!(
                "the address is not valid: {}",
                crate::error::shorten(url)
            )));
        };
        let scheme = uri.scheme_str().unwrap_or("").to_ascii_lowercase();
        match scheme.as_str() {
            "https" => {}
            "http" if self.allow_insecure || is_loopback_uri(&uri) => {}
            "http" => {
                return Err(AiError::NotConfigured(
                    "plain http:// is only used for this computer (localhost); use https:// or allow insecure connections for this provider".into(),
                ));
            }
            _ => {
                return Err(AiError::NotConfigured(
                    "the address must start with https:// (or http:// for this computer)".into(),
                ));
            }
        }
        if with_key && !key_matches(self.key_host.as_deref(), &uri) {
            return Err(AiError::NotConfigured(
                "the API key was saved for another address; save it again".into(),
            ));
        }
        Ok(())
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

/// The host of `url`, lowercased, without the port, the user info or the
/// brackets of an IPv6 address: `https://user@API.example.com:8443/v1` is
/// `api.example.com`, `http://[::1]:11434` is `::1`. `None` when the URL
/// has no host or cannot be parsed. It reads the URL as the HTTP client
/// does, so for `http://127.0.0.1@evil.com` it is `evil.com`.
pub fn url_host(url: &str) -> Option<String> {
    uri_host(&parse_url(url)?)
}

/// `url` parsed as the HTTP client parses it, when it is a full URL with a
/// scheme. A bare `localhost:11434` parses too, as an authority alone, but
/// the client cannot send to it, so it names no host here.
fn parse_url(url: &str) -> Option<ureq::http::Uri> {
    url.trim()
        .parse::<ureq::http::Uri>()
        .ok()
        .filter(|uri| uri.scheme().is_some())
}

fn uri_host(uri: &ureq::http::Uri) -> Option<String> {
    // The same steps as the client's own `Authority::host`, done here so an
    // odd authority gives `None` rather than a panic: the part after the
    // last `@`, then up to the `]` of an IPv6 address or the `:` of a port.
    let authority = uri.authority()?.as_str();
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        &rest[..rest.find(']')?]
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Whether `url` names this computer: the host is `localhost`, an address in
/// 127.0.0.0/8 or `::1`. Names that merely start with one of those
/// (`localhost.example.com`) do not count. Only these may be reached over
/// plain `http://` unless the provider allows insecure connections.
pub fn is_loopback_url(url: &str) -> bool {
    parse_url(url).is_some_and(|uri| is_loopback_uri(&uri))
}

fn is_loopback_uri(uri: &ureq::http::Uri) -> bool {
    let Some(host) = uri_host(uri) else {
        return false;
    };
    host == "localhost"
        || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
        || host
            .parse::<Ipv6Addr>()
            .is_ok_and(|ip| ip == Ipv6Addr::LOCALHOST)
}

/// The origin of `url`: lowercased scheme, host and port, the port written
/// out even where it is the scheme's default (`https://API.example.com/v1`
/// is `https://api.example.com:443`, `http://[::1]:11434` is
/// `http://[::1]:11434`). `None` when the URL has no scheme or host.
pub fn url_origin(url: &str) -> Option<String> {
    uri_origin(&parse_url(url)?)
}

fn uri_origin(uri: &ureq::http::Uri) -> Option<String> {
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let host = uri_host(uri)?;
    let port = match uri.port_u16() {
        Some(port) => port,
        None => match scheme.as_str() {
            "https" => 443,
            "http" => 80,
            _ => return None,
        },
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    Some(format!("{scheme}://{host}:{port}"))
}

/// Whether a key saved for `saved` may go to `uri`: the same origin, or for
/// an older setting holding a bare host, the same host.
fn key_matches(saved: Option<&str>, uri: &ureq::http::Uri) -> bool {
    let Some(saved) = saved else {
        return true;
    };
    let saved = saved.trim();
    if saved.contains("://") {
        // Written by hand, the origin may lack its port or have capitals:
        // compare it as `url_origin` would give it.
        return url_origin(saved).is_some_and(|saved| uri_origin(uri) == Some(saved));
    }
    let saved = saved.trim_start_matches('[').trim_end_matches(']');
    uri_host(uri).is_some_and(|host| host.eq_ignore_ascii_case(saved))
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
/// kept in memory only and sent only to `config.base_url`: redirects are not
/// followed. Every request is checked first: plain `http://` goes only to
/// this computer unless [`ProviderConfig::allow_insecure`] is set, and the
/// key goes only to [`ProviderConfig::key_host`] when one is recorded; a
/// request that fails either check is not sent and ends with
/// [`AiError::NotConfigured`].
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
        for event in parser.feed(&chunk)? {
            decoder.event(&event, on_text)?;
        }
    }
    if let Some(event) = parser.finish()? {
        decoder.event(&event, on_text)?;
    }
    decoder.finish()
}

/// Count `more` into a reply's text, which may not go past
/// [`MAX_RESPONSE_CHARS`]. Checked before the text is kept or shown.
pub(crate) fn add_text(total: &mut usize, more: &str) -> Result<(), AiError> {
    *total += more.chars().count();
    if *total > MAX_RESPONSE_CHARS {
        return Err(AiError::TooLarge(format!(
            "the reply is longer than {MAX_RESPONSE_CHARS} characters"
        )));
    }
    Ok(())
}

/// A reply may ask for at most [`MAX_TOOL_CALLS`] calls; `count` is how
/// many it would have with the new one.
pub(crate) fn check_tool_count(count: usize) -> Result<(), AiError> {
    if count > MAX_TOOL_CALLS {
        return Err(AiError::TooLarge(format!(
            "the reply asks for more than {MAX_TOOL_CALLS} tool calls"
        )));
    }
    Ok(())
}

/// A tool name is 1 to [`MAX_TOOL_NAME_BYTES`] of `A-Z a-z 0-9 _ -`, as the
/// providers themselves require. Anything else would be shown in the
/// approval dialog and written to the audit log as the model chose it.
pub(crate) fn check_tool_name(name: &str) -> Result<(), AiError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(AiError::BadResponse(format!(
            "the model asked for a tool with an invalid name ({} bytes); names are up to {MAX_TOOL_NAME_BYTES} letters, digits, _ or -",
            name.len()
        )))
    }
}

/// A tool input of `len` bytes of JSON text may be kept.
pub(crate) fn check_tool_input(len: usize) -> Result<(), AiError> {
    if len > MAX_TOOL_INPUT_BYTES {
        return Err(AiError::TooLarge(format!(
            "a tool input is larger than {} MiB",
            MAX_TOOL_INPUT_BYTES / (1024 * 1024)
        )));
    }
    Ok(())
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

    #[test]
    fn hosts_are_read_as_the_client_reads_them() {
        assert_eq!(
            url_host("https://user:pw@API.Example.com:8443/v1").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(url_host(" http://[::1]:11434/v1 ").as_deref(), Some("::1"));
        assert_eq!(
            url_host("http://127.0.0.1@evil.com").as_deref(),
            Some("evil.com")
        );
        assert_eq!(
            url_host("https://api.anthropic.com").as_deref(),
            Some("api.anthropic.com")
        );
        assert_eq!(url_host("not a url"), None);
        assert_eq!(url_host("/v1/models"), None);
        assert_eq!(url_host("localhost:11434"), None);
        assert_eq!(url_host(""), None);
    }

    #[test]
    fn only_this_computer_is_loopback() {
        for url in [
            "http://localhost:11434/v1",
            "http://LOCALHOST",
            "http://127.0.0.1",
            "http://127.8.9.10:8080/v1",
            "http://[::1]",
            "http://[::1]:11434/v1",
            "http://[0:0:0:0:0:0:0:1]/",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in [
            "http://192.168.1.5",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "http://127.0.0.1@evil.com",
            "http://localhost@evil.com/v1",
            "http://evil.com#@localhost",
            "http://[::2]",
            "http://128.0.0.1",
            "http://0.0.0.0",
            "localhost:11434",
            "",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }

    fn custom(base_url: &str) -> ProviderConfig {
        let mut config = ProviderConfig::from_preset("c", Preset::Custom);
        config.base_url = base_url.into();
        config
    }

    #[test]
    fn plain_http_goes_only_to_this_computer_unless_allowed() {
        let allowed = |config: &ProviderConfig, url: &str| config.check_request(url, false);
        assert!(allowed(&custom(""), "http://127.0.0.1:8080/v1/models").is_ok());
        assert!(allowed(&custom(""), "http://[::1]/v1/models").is_ok());
        assert!(allowed(&custom(""), "https://192.168.1.5/v1/models").is_ok());
        for url in [
            "http://192.168.1.5/v1/models",
            "http://localhost.evil.com/v1/models",
            "http://127.0.0.1@evil.com/v1/models",
            "ftp://127.0.0.1/v1/models",
            "127.0.0.1/v1/models",
        ] {
            assert_eq!(
                allowed(&custom(""), url).unwrap_err().kind(),
                "not_configured",
                "{url}"
            );
        }
        let mut insecure = custom("");
        insecure.allow_insecure = true;
        assert!(allowed(&insecure, "http://192.168.1.5/v1/models").is_ok());
        // Allowing insecure connections does not allow other schemes.
        assert!(allowed(&insecure, "ftp://192.168.1.5/v1/models").is_err());
    }

    #[test]
    fn origins_have_scheme_host_and_port() {
        assert_eq!(
            url_origin("https://API.Example.com/v1").as_deref(),
            Some("https://api.example.com:443")
        );
        assert_eq!(
            url_origin("http://localhost:11434/v1").as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(
            url_origin("http://[::1]/v1").as_deref(),
            Some("http://[::1]:80")
        );
        assert_eq!(url_origin("localhost:11434"), None);
    }

    #[test]
    fn a_key_saved_for_an_origin_keeps_to_its_port_and_scheme() {
        let mut config = custom("http://localhost:11434/v1");
        config.key_host = url_origin(&config.base_url);
        assert!(
            config
                .check_request("http://localhost:11434/v1/models", true)
                .is_ok()
        );
        // Another port on the same computer: another service.
        assert!(
            config
                .check_request("http://localhost:8080/v1/models", true)
                .is_err()
        );
        // The same host over another scheme.
        let mut secure = custom("https://api.example.com/v1");
        secure.key_host = url_origin(&secure.base_url);
        secure.allow_insecure = true;
        assert!(
            secure
                .check_request("https://api.example.com:443/v1/x", true)
                .is_ok()
        );
        assert!(
            secure
                .check_request("http://api.example.com/v1/x", true)
                .is_err()
        );
        // Without the key the request may go.
        assert!(
            config
                .check_request("http://localhost:8080/v1/models", false)
                .is_ok()
        );
        // An origin written by hand without its port means the default one.
        secure.key_host = Some("HTTPS://api.example.com".into());
        assert!(
            secure
                .check_request("https://api.example.com/v1/x", true)
                .is_ok()
        );
        // An older setting with a bare host is compared as a host.
        secure.key_host = Some("api.example.com".into());
        assert!(
            secure
                .check_request("https://api.example.com:8443/v1/x", true)
                .is_ok()
        );
    }

    #[test]
    fn a_key_goes_only_to_the_host_it_was_saved_for() {
        let mut config = custom("https://api.example.com/v1");
        assert!(config.key_matches_host(), "no saved host: as before");
        assert!(
            config
                .check_request("https://api.example.com/v1/models", true)
                .is_ok()
        );

        config.key_host = Some("api.example.com".into());
        assert!(config.key_matches_host());
        assert!(
            config
                .check_request("https://api.example.com/v1/models", true)
                .is_ok()
        );

        config.base_url = "https://other.example.com/v1".into();
        assert!(!config.key_matches_host());
        let error = config
            .check_request("https://other.example.com/v1/models", true)
            .unwrap_err();
        assert_eq!(error.kind(), "not_configured");
        assert!(error.to_string().contains("save it again"), "{error}");
        // Without the key, nothing secret goes out, so the request may.
        assert!(
            config
                .check_request("https://other.example.com/v1/models", false)
                .is_ok()
        );

        // The saved host is compared as `url_host` gives it.
        config.base_url = "http://[::1]:11434/v1".into();
        config.key_host = Some("[::1]".into());
        assert!(config.key_matches_host());
        config.base_url = "https://API.EXAMPLE.com".into();
        config.key_host = Some("Api.Example.Com".into());
        assert!(config.key_matches_host());
        // A base URL without a host never matches a saved host.
        config.base_url = "nowhere".into();
        assert!(!config.key_matches_host());
    }

    #[test]
    fn tool_names_are_checked() {
        assert!(check_tool_name("read_document").is_ok());
        assert!(check_tool_name("a-B_9").is_ok());
        assert!(check_tool_name(&"n".repeat(MAX_TOOL_NAME_BYTES)).is_ok());
        for bad in [
            String::new(),
            "n".repeat(MAX_TOOL_NAME_BYTES + 1),
            "read document".into(),
            "read\u{1b}[2Jdocument".into(),
            "名前".into(),
            "a.b".into(),
            "a/b".into(),
        ] {
            assert_eq!(
                check_tool_name(&bad).unwrap_err().kind(),
                "bad_response",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn counts_and_sizes_are_bounded() {
        let mut total = 0;
        add_text(&mut total, &"字".repeat(MAX_RESPONSE_CHARS)).unwrap();
        assert_eq!(add_text(&mut total, "x").unwrap_err().kind(), "too_large");
        assert!(check_tool_count(MAX_TOOL_CALLS).is_ok());
        assert!(check_tool_count(MAX_TOOL_CALLS + 1).is_err());
        assert!(check_tool_input(MAX_TOOL_INPUT_BYTES).is_ok());
        assert!(check_tool_input(MAX_TOOL_INPUT_BYTES + 1).is_err());
    }
}
