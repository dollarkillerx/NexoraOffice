//! The built-in assistant and MCP, wired into the window.
//!
//! One [`Invoker`] serves both: the assistant's turns run on a thread of
//! their own and MCP clients on the server's threads, and every tool call
//! they make comes here, to [`ShellHost`], which does the work on the
//! interface thread (documents live there) and sends the answer back.
//! Questions for the user are cards in the assistant panel; the panel opens
//! for them without taking the keyboard, so typing in a document is never
//! interrupted. A card nobody answers counts as Deny after two minutes.
//!
//! What the user allows is kept in `assistant.json` in the data folder:
//! folder rules, the level of new documents, and whether MCP is on.
//! Per-document grants last until the program quits.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use std::collections::HashMap;

use aster_ai::provider::Preset;
use aster_ai::{
    AgentEvent, AiError, AiSettings, InvokerExecutor, KeyStore, ProviderConfig, SystemKeyStore,
    TurnContext, TurnEnd, connect, tool_defs, user_message,
};
use aster_markdown::Selection;
use aster_mcp::{McpClient, McpConfig, McpEvents, McpServer};
use aster_shell::assistant;
use aster_shell::i18n;
use aster_shell::tabs::{DocKind, TabId};
use aster_tools::{
    Approval, ApprovalRequest, Approver, Audit, DocumentInfo, Host, Invoker, Level, Policy, Reason,
    ToolSpec,
};
use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{
    AfterSave, AppWindow, ApprovalItem, ChatItem, Shell, after_edit, after_tab_change, deliver,
    spawn_save, start_load,
};

/// How long a tool waits for the interface thread before giving up.
const UI_WAIT: Duration = Duration::from_secs(20);

/// Run `job` with the shell on the interface thread and wait for its
/// answer. `None` if the window is gone or it took longer than `wait`.
fn on_ui<R: Send + 'static>(
    weak: &slint::Weak<AppWindow>,
    wait: Duration,
    job: impl FnOnce(&AppWindow, &mut Shell) -> R + Send + 'static,
) -> Option<R> {
    let (sender, answer) = mpsc::channel();
    weak.upgrade_in_event_loop(move |ui| {
        deliver(
            ui,
            Box::new(move |ui, shell| {
                let _ = sender.send(job(ui, shell));
            }),
        );
    })
    .ok()?;
    answer.recv_timeout(wait).ok()
}

/// The assistant's and MCP's state in the shell.
pub struct Assistant {
    pub invoker: Arc<Invoker>,
    policy: Arc<Mutex<Policy>>,
    items: Rc<VecModel<ChatItem>>,
    approvals: Vec<(ApprovalItem, mpsc::Sender<Approval>)>,
    approval_items: Rc<VecModel<ApprovalItem>>,
    access_file: Option<PathBuf>,
    mcp_enabled: bool,
    mcp: Option<McpServer>,
    mcp_error: Option<String>,
    pub settings: AiSettings,
    settings_file: Option<PathBuf>,
    keys: Arc<dyn KeyStore>,
    /// The conversation so far, as the model sees it.
    conversation: Vec<aster_ai::Message>,
    conversation_id: u64,
    /// Set while a turn runs; setting the flag stops it.
    cancel: Option<Arc<AtomicBool>>,
    /// Whether a key is stored, by provider id: the key store is asked once.
    has_key: HashMap<String, bool>,
    /// The assistant's reply being streamed, by row.
    streaming: Option<usize>,
    /// Tool calls on screen, by call id.
    tool_rows: HashMap<String, usize>,
    /// Events from the running turn, applied in order.
    inbox: Arc<Mutex<Vec<AgentEvent>>>,
    /// The models each provider offers, by provider id.
    models: HashMap<String, ModelList>,
}

/// What listing a provider's models came to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelList {
    Loading,
    Ready(Vec<String>),
    Failed(String),
}

impl Assistant {
    /// Set up the assistant for the window: the tool layer, saved access
    /// rules and settings, and MCP if it was left on.
    pub fn start(ui: &AppWindow, shell: &mut Shell) {
        let data = aster_platform::paths::data_dir();
        let access_file = data.as_ref().map(|dir| dir.join("assistant.json"));
        let settings_file = data.as_ref().map(|dir| dir.join("ai-settings.json"));
        let (policy, mcp_enabled) = access_file.as_deref().map(load_access).unwrap_or_default();
        let policy = Arc::new(Mutex::new(policy));
        let audit = data
            .as_ref()
            .and_then(|dir| Audit::open(&dir.join("audit").join("tools.jsonl")).ok())
            .unwrap_or_else(Audit::in_memory);
        let weak = ui.as_weak();
        let invoker = Arc::new(Invoker::new(
            policy.clone(),
            Arc::new(ShellHost { ui: weak.clone() }),
            Arc::new(Cards { ui: weak }),
            Arc::new(audit),
        ));
        let settings = settings_file
            .as_deref()
            .map(AiSettings::load)
            .unwrap_or_default();
        let items = Rc::new(VecModel::default());
        let approval_items = Rc::new(VecModel::default());
        ui.set_ai_items(ModelRc::from(items.clone()));
        ui.set_ai_approvals(ModelRc::from(approval_items.clone()));
        shell.ai = Some(Assistant {
            invoker,
            policy,
            items,
            approvals: Vec::new(),
            approval_items,
            access_file,
            mcp_enabled: false,
            mcp: None,
            mcp_error: None,
            settings,
            settings_file,
            keys: Arc::new(SystemKeyStore),
            conversation: Vec::new(),
            conversation_id: 1,
            cancel: None,
            has_key: HashMap::new(),
            streaming: None,
            tool_rows: HashMap::new(),
            inbox: Arc::new(Mutex::new(Vec::new())),
            models: HashMap::new(),
        });
        if mcp_enabled {
            set_mcp(ui, shell, true);
        }
        refresh(ui, shell);
    }

    fn save_access(&self) {
        let Some(path) = &self.access_file else {
            return;
        };
        let Ok(policy) = self.policy.lock() else {
            return;
        };
        let folders: Vec<Value> = policy
            .folders
            .iter()
            .map(
                |(path, level)| json!({ "path": path.display().to_string(), "level": level.key() }),
            )
            .collect();
        let state = json!({
            "folders": folders,
            "untitled": policy.untitled.key(),
            "mcp": self.mcp_enabled,
        });
        let _ = aster_storage::write_atomically(path, state.to_string().as_bytes());
    }

    fn push(&self, kind: i32, text: String, detail: String, state: i32) {
        self.items.push(ChatItem {
            kind,
            text: text.into(),
            detail: detail.into(),
            state,
        });
    }
}

/// Folder rules, the level of new documents, and whether MCP is on.
fn load_access(path: &Path) -> (Policy, bool) {
    let mut policy = Policy::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return (policy, false);
    };
    let Ok(state) = serde_json::from_str::<Value>(&text) else {
        return (policy, false);
    };
    for folder in state["folders"].as_array().into_iter().flatten() {
        if let (Some(path), Some(level)) = (
            folder["path"].as_str(),
            folder["level"].as_str().and_then(Level::from_key),
        ) {
            policy.set_folder(PathBuf::from(path), level);
        }
    }
    policy.untitled = state["untitled"]
        .as_str()
        .and_then(Level::from_key)
        .unwrap_or_default();
    (policy, state["mcp"].as_bool().unwrap_or(false))
}

fn level_index(level: Level) -> i32 {
    Level::ALL.iter().position(|l| *l == level).unwrap_or(0) as i32
}

fn level_at(index: i32) -> Level {
    Level::ALL
        .get(index.max(0) as usize)
        .copied()
        .unwrap_or_default()
}

/// Bring the panel up to date: the document in front and its access, the
/// settings page, MCP's state.
pub fn refresh(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let active = shell
        .tabs
        .active()
        .map(|tab| (tab.id, tab.kind, tab.title.clone()));
    let folder = shell.tree.as_ref().map(|tree| tree.root().to_owned());
    let path = active
        .as_ref()
        .and_then(|(id, _, _)| shell.markdown(*id).and_then(|e| e.path.clone()));
    let Some(ai) = &mut shell.ai else {
        return;
    };
    let policy = ai.policy.lock().map(|p| p.clone()).unwrap_or_default();
    match active {
        Some((id, DocKind::Markdown, title)) => {
            let level = policy.level_of(&id.0.to_string(), path.as_deref());
            ui.set_ai_access_level(level_index(level));
            ui.set_ai_access_label(i18n::fill(strings.ai_access_label, "name", &title).into());
        }
        _ => ui.set_ai_access_level(-1),
    }
    match &folder {
        Some(folder) => {
            let name = folder.file_name().map_or_else(
                || folder.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            ui.set_ai_folder_label(i18n::fill(strings.ai_folder_access, "name", &name).into());
            ui.set_ai_folder_level(level_index(policy.level_of_path(folder)));
        }
        None => ui.set_ai_folder_label(SharedString::new()),
    }
    ui.set_ai_untitled_level(level_index(policy.untitled));

    let provider = ai.settings.selected_provider().cloned();
    ui.set_ai_provider(provider.as_ref().map_or(0, |p| {
        Preset::ALL
            .iter()
            .position(|preset| *preset == p.preset)
            .unwrap_or(0) as i32
    }));
    if let Some(provider) = &provider {
        ui.set_ai_base_url(provider.base_url.clone().into());
        ui.set_ai_model(provider.model.clone().into());
    }
    let has_key = provider.as_ref().is_some_and(|p| key_known(ai, &p.id));
    ui.set_ai_key_state(
        if has_key {
            strings.ai_key_saved
        } else {
            strings.ai_key_missing
        }
        .into(),
    );
    ui.set_ai_configured(
        provider
            .as_ref()
            .is_some_and(|p| !p.model.trim().is_empty() && (has_key || !p.preset.needs_key())),
    );
    let (models, state) = match provider.as_ref().and_then(|p| ai.models.get(&p.id)) {
        Some(ModelList::Ready(ids)) => (
            ids.clone(),
            i18n::fill(strings.ai_models_count, "count", &ids.len().to_string()),
        ),
        Some(ModelList::Loading) => (Vec::new(), strings.ai_models_loading.to_owned()),
        Some(ModelList::Failed(error)) => (
            Vec::new(),
            i18n::fill(strings.ai_models_failed, "error", error),
        ),
        None if provider
            .as_ref()
            .is_some_and(|p| !has_key && p.preset.needs_key()) =>
        {
            (Vec::new(), strings.ai_models_need_key.to_owned())
        }
        None => (Vec::new(), String::new()),
    };
    ui.set_ai_models(ModelRc::new(VecModel::from(
        models
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )));
    ui.set_ai_models_state(state.into());
    ui.set_ai_redact(ai.settings.redact);
    ui.set_ai_running(ai.cancel.is_some());

    ui.set_mcp_enabled(ai.mcp_enabled);
    let (status, clients) = match (&ai.mcp, &ai.mcp_error) {
        (_, Some(error)) => (
            i18n::fill(strings.mcp_failed, "error", error),
            String::new(),
        ),
        (Some(server), None) => {
            let names: Vec<String> = server.clients().into_iter().map(|c| c.name).collect();
            (
                i18n::fill(
                    strings.mcp_on_note,
                    "path",
                    &server.socket_path().display().to_string(),
                ),
                if names.is_empty() {
                    String::new()
                } else {
                    i18n::fill(strings.mcp_clients, "names", &names.join(", "))
                },
            )
        }
        (None, None) => (String::new(), String::new()),
    };
    ui.set_mcp_status(status.into());
    ui.set_mcp_clients(clients.into());
    match aster_mcp::bridge_path() {
        Some(bridge) => {
            let setup = aster_mcp::client_setup(&bridge);
            ui.set_mcp_setup_cli(setup.claude_code.into());
            ui.set_mcp_setup_json(setup.json_config.into());
        }
        None => {
            ui.set_mcp_setup_cli(strings.mcp_no_bridge.into());
            ui.set_mcp_setup_json(SharedString::new());
        }
    }
}

/// Whether the key store holds a key for a provider, asked once.
fn key_known(ai: &mut Assistant, id: &str) -> bool {
    if let Some(known) = ai.has_key.get(id) {
        return *known;
    }
    let known = ai.keys.get(id).ok().flatten().is_some();
    ai.has_key.insert(id.to_owned(), known);
    known
}

// --- Settings --------------------------------------------------------------

fn save_settings(ai: &Assistant) {
    if let Some(path) = &ai.settings_file {
        let _ = ai.settings.save(path);
    }
}

/// Choose a provider: one of the ready-made ones, or the custom one.
pub fn set_provider(ui: &AppWindow, shell: &mut Shell, index: i32) {
    let Some(ai) = &mut shell.ai else {
        return;
    };
    let Some(preset) = Preset::ALL.get(index.max(0) as usize).copied() else {
        return;
    };
    let id = match ai.settings.providers.iter().find(|p| p.preset == preset) {
        Some(provider) => provider.id.clone(),
        None => {
            let provider = ProviderConfig::from_preset(preset.key(), preset);
            let id = provider.id.clone();
            ai.settings.providers.push(provider);
            id
        }
    };
    ai.settings.selected = Some(id);
    save_settings(ai);
    refresh(ui, shell);
    list_models_if_new(ui, shell);
}

fn selected_mut(ai: &mut Assistant) -> Option<&mut ProviderConfig> {
    let id = ai.settings.selected.clone()?;
    ai.settings.providers.iter_mut().find(|p| p.id == id)
}

pub fn set_base_url(ui: &AppWindow, shell: &mut Shell, url: &str) {
    if let Some(ai) = &mut shell.ai
        && let Some(provider) = selected_mut(ai)
    {
        let url = url.trim();
        let url = if url.is_empty() {
            provider.preset.default_base_url().to_owned()
        } else {
            url.to_owned()
        };
        let changed = provider.base_url != url;
        provider.base_url = url;
        let id = provider.id.clone();
        if changed {
            // Another endpoint, other models.
            ai.models.remove(&id);
        }
        save_settings(ai);
    }
    refresh(ui, shell);
    list_models_if_new(ui, shell);
}

pub fn set_model(ui: &AppWindow, shell: &mut Shell, model: &str) {
    if let Some(ai) = &mut shell.ai
        && let Some(provider) = selected_mut(ai)
    {
        provider.model = model.trim().to_owned();
        save_settings(ai);
    }
    refresh(ui, shell);
}

pub fn save_key(ui: &AppWindow, shell: &mut Shell, key: &str) {
    let strings = shell.language.strings();
    if let Some(ai) = &mut shell.ai
        && let Some(id) = ai.settings.selected.clone()
        && !key.trim().is_empty()
    {
        match ai.keys.set(&id, key.trim()) {
            Ok(()) => {
                ai.has_key.insert(id.clone(), true);
                // A new key may see other models.
                ai.models.remove(&id);
            }
            Err(error) => ai.push(
                3,
                i18n::fill(strings.ai_error, "message", &error.0),
                String::new(),
                0,
            ),
        }
    }
    refresh(ui, shell);
    list_models(ui, shell);
}

/// Ask the selected provider for its models, on a thread of its own. Models
/// that cannot chat (embeddings, speech, pictures) are left out. When no
/// model is set yet, the first one is taken.
pub fn list_models(ui: &AppWindow, shell: &mut Shell) {
    let Some(ai) = &mut shell.ai else {
        return;
    };
    let Some(provider) = ai.settings.selected_provider().cloned() else {
        return;
    };
    let key = ai.keys.get(&provider.id).ok().flatten();
    if key.is_none() && provider.preset.needs_key() {
        ai.models.remove(&provider.id);
        refresh(ui, shell);
        return;
    }
    ai.models.insert(provider.id.clone(), ModelList::Loading);
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("aster-models".into())
        .spawn(move || {
            let never = AtomicBool::new(false);
            let listed = connect(provider.clone(), key).list_models(&never);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| {
                        let Some(ai) = &mut shell.ai else {
                            return;
                        };
                        let list = match listed {
                            Ok(models) => {
                                let mut ids: Vec<String> = models
                                    .into_iter()
                                    .map(|m| m.id)
                                    .filter(|id| can_chat(id))
                                    .collect();
                                ids.sort();
                                ids.dedup();
                                ModelList::Ready(ids)
                            }
                            Err(error) => ModelList::Failed(error.to_string()),
                        };
                        if let ModelList::Ready(ids) = &list
                            && let Some(first) = ids.first()
                            && let Some(entry) = ai
                                .settings
                                .providers
                                .iter_mut()
                                .find(|p| p.id == provider.id)
                            && entry.model.trim().is_empty()
                        {
                            entry.model = first.clone();
                            save_settings(ai);
                        }
                        ai.models.insert(provider.id.clone(), list);
                        refresh(ui, shell);
                    }),
                );
            });
        })
        .expect("cannot start a thread to list the models");
    refresh(ui, shell);
}

/// List the selected provider's models unless they are listed or being
/// listed already.
fn list_models_if_new(ui: &AppWindow, shell: &mut Shell) {
    let listed = shell.ai.as_ref().is_some_and(|ai| {
        ai.settings.selected.as_ref().is_some_and(|id| {
            matches!(
                ai.models.get(id),
                Some(ModelList::Ready(_) | ModelList::Loading)
            )
        })
    });
    if !listed {
        list_models(ui, shell);
    }
}

/// Whether a model id names a model one can chat with: providers list
/// embedding, speech, picture and moderation models in the same place.
fn can_chat(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ![
        "embed",
        "tts",
        "whisper",
        "dall-e",
        "audio",
        "image",
        "moderation",
        "babbage",
        "davinci",
        "realtime",
        "transcribe",
        "search",
        "rerank",
        "sora",
    ]
    .iter()
    .any(|word| id.contains(word))
}

pub fn delete_key(ui: &AppWindow, shell: &mut Shell) {
    if let Some(ai) = &mut shell.ai
        && let Some(id) = ai.settings.selected.clone()
    {
        let _ = ai.keys.delete(&id);
        ai.has_key.insert(id, false);
    }
    refresh(ui, shell);
}

pub fn set_redact(ui: &AppWindow, shell: &mut Shell, on: bool) {
    if let Some(ai) = &mut shell.ai {
        ai.settings.redact = on;
        save_settings(ai);
    }
    refresh(ui, shell);
}

// --- Conversation ----------------------------------------------------------

/// What the user is looking at, for the model: the document in front if the
/// assistant may see it, and the selection in it.
fn context(shell: &mut Shell) -> TurnContext {
    let mut context = TurnContext::default();
    let Some((id, title)) = shell
        .tabs
        .active()
        .filter(|tab| tab.kind == DocKind::Markdown)
        .map(|tab| (tab.id, tab.title.clone()))
    else {
        return context;
    };
    let Some(policy) = shell
        .ai
        .as_ref()
        .and_then(|ai| ai.policy.lock().ok().map(|p| p.clone()))
    else {
        return context;
    };
    let Some(editor) = shell.markdown(id) else {
        return context;
    };
    if policy.level_of(&id.0.to_string(), editor.path.as_deref()) == Level::Off {
        return context;
    }
    context.document_id = Some(id.0.to_string());
    context.document_title = Some(title);
    let range = editor.document.selection().range();
    if !range.is_empty() {
        context.selection = Some(editor.document.text().byte_slice(range).to_string());
    }
    context
}

/// Send what the user typed and run a turn on a thread of its own.
pub fn send(ui: &AppWindow, shell: &mut Shell, text: &str) {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return;
    }
    let context = context(shell);
    let Some(ai) = &mut shell.ai else {
        return;
    };
    if ai.cancel.is_some() {
        return;
    }
    let Some(provider) = ai.settings.selected_provider().cloned() else {
        return;
    };
    let key = ai.keys.get(&provider.id).ok().flatten();
    ai.push(0, text.clone(), String::new(), 0);
    ai.conversation.push(user_message(&text, &context));
    ai.streaming = None;
    let cancel = Arc::new(AtomicBool::new(false));
    ai.cancel = Some(cancel.clone());
    let mut conversation = ai.conversation.clone();
    let invoker = ai.invoker.clone();
    let options = ai.settings.agent_options();
    let conversation_id = format!("chat-{}", ai.conversation_id);
    let inbox = ai.inbox.clone();
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("aster-assistant".into())
        .spawn(move || {
            let model = connect(provider.clone(), key);
            let tools = tool_defs(aster_tools::tools());
            let executor = InvokerExecutor::new(&invoker, &provider, &conversation_id);
            let mut sink = |event: AgentEvent| {
                if let Ok(mut inbox) = inbox.lock() {
                    inbox.push(event);
                }
                let _ = weak.upgrade_in_event_loop(|ui| {
                    deliver(ui, Box::new(drain));
                });
            };
            let result = aster_ai::run(
                &*model,
                &mut conversation,
                &tools,
                &executor,
                &options,
                &mut sink,
                &cancel,
            );
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| {
                        drain(ui, shell);
                        if let Some(ai) = &mut shell.ai {
                            // A stopped or failed turn leaves the conversation
                            // as it was before it.
                            if result.is_ok() {
                                ai.conversation = conversation;
                            }
                            ai.cancel = None;
                            ai.streaming = None;
                        }
                        refresh(ui, shell);
                    }),
                );
            });
        })
        .expect("cannot start the assistant's thread");
    refresh(ui, shell);
}

/// Show the events that have come in from the running turn, in order.
fn drain(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let titles: HashMap<String, String> = shell
        .tabs
        .tabs()
        .iter()
        .map(|tab| (tab.id.0.to_string(), tab.title.clone()))
        .collect();
    let Some(ai) = &mut shell.ai else {
        return;
    };
    let events: Vec<AgentEvent> = ai
        .inbox
        .lock()
        .map(|mut inbox| std::mem::take(&mut *inbox))
        .unwrap_or_default();
    for event in events {
        match event {
            AgentEvent::TextDelta(delta) => match ai.streaming {
                Some(row) if row < ai.items.row_count() => {
                    if let Some(mut item) = ai.items.row_data(row) {
                        item.text = format!("{}{delta}", item.text).into();
                        ai.items.set_row_data(row, item);
                    }
                }
                _ => {
                    ai.push(1, delta, String::new(), 0);
                    ai.streaming = Some(ai.items.row_count() - 1);
                }
            },
            AgentEvent::ToolCallStarted { id, name, input } => {
                ai.streaming = None;
                let target = input["document"]
                    .as_str()
                    .and_then(|doc| titles.get(doc).cloned())
                    .or_else(|| input["path"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                ai.push(2, name, target, 0);
                ai.tool_rows.insert(id, ai.items.row_count() - 1);
            }
            AgentEvent::ToolCallFinished { id, result, .. } => {
                if let Some(row) = ai.tool_rows.remove(&id)
                    && let Some(mut item) = ai.items.row_data(row)
                {
                    item.state = if result.is_error { 2 } else { 1 };
                    ai.items.set_row_data(row, item);
                }
            }
            AgentEvent::Done { end } => {
                ai.streaming = None;
                if end != TurnEnd::Finished {
                    ai.push(4, strings.ai_stopped.to_owned(), String::new(), 0);
                }
            }
            AgentEvent::Error(AiError::Cancelled) => {
                ai.streaming = None;
                ai.push(4, strings.ai_stopped.to_owned(), String::new(), 0);
            }
            AgentEvent::Error(error) => {
                ai.streaming = None;
                ai.push(
                    3,
                    i18n::fill(strings.ai_error, "message", &error.to_string()),
                    String::new(),
                    0,
                );
            }
            AgentEvent::Redacted { .. } | AgentEvent::StepFinished { .. } => {}
        }
    }
    let _ = ui;
}

pub fn stop(ui: &AppWindow, shell: &mut Shell) {
    if let Some(ai) = &shell.ai
        && let Some(cancel) = &ai.cancel
    {
        cancel.store(true, Ordering::Relaxed);
    }
    refresh(ui, shell);
}

/// Start over: the conversation and its "allow for this chat" answers go.
pub fn new_chat(ui: &AppWindow, shell: &mut Shell) {
    if let Some(ai) = &mut shell.ai {
        if let Some(cancel) = &ai.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        ai.invoker
            .end_session(&format!("chat-{}", ai.conversation_id));
        ai.conversation_id += 1;
        ai.conversation.clear();
        ai.items.clear();
        ai.tool_rows.clear();
        ai.streaming = None;
    }
    refresh(ui, shell);
}

// --- Access ----------------------------------------------------------------

/// The access level of the Markdown document in front, for this session.
pub fn set_document_access(ui: &AppWindow, shell: &mut Shell, index: i32) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    if let Some(ai) = &shell.ai
        && let Ok(mut policy) = ai.policy.lock()
    {
        policy.grant(&id.0.to_string(), Some(level_at(index)));
    }
    refresh(ui, shell);
}

pub fn set_folder_access(ui: &AppWindow, shell: &mut Shell, index: i32) {
    let Some(folder) = shell.tree.as_ref().map(|tree| tree.root().to_owned()) else {
        return;
    };
    if let Some(ai) = &shell.ai {
        if let Ok(mut policy) = ai.policy.lock() {
            policy.set_folder(folder, level_at(index));
        }
        ai.save_access();
    }
    refresh(ui, shell);
}

pub fn set_untitled_access(ui: &AppWindow, shell: &mut Shell, index: i32) {
    if let Some(ai) = &shell.ai {
        if let Ok(mut policy) = ai.policy.lock() {
            policy.untitled = level_at(index);
        }
        ai.save_access();
    }
    refresh(ui, shell);
}

// --- MCP -----------------------------------------------------------------

struct McpWatch {
    ui: slint::Weak<AppWindow>,
}

impl McpEvents for McpWatch {
    fn client_connected(&self, _: &McpClient) {
        self.changed();
    }

    fn client_disconnected(&self, _: u64) {
        self.changed();
    }
}

impl McpWatch {
    fn changed(&self) {
        let _ = self.ui.upgrade_in_event_loop(|ui| {
            deliver(ui, Box::new(refresh));
        });
    }
}

pub fn set_mcp(ui: &AppWindow, shell: &mut Shell, on: bool) {
    let Some(ai) = &mut shell.ai else {
        return;
    };
    ai.mcp_enabled = on;
    ai.mcp_error = None;
    if on {
        if ai.mcp.is_none() {
            let events = Arc::new(McpWatch { ui: ui.as_weak() });
            match McpServer::start(McpConfig::default(), ai.invoker.clone(), events) {
                Ok(server) => ai.mcp = Some(server),
                Err(error) => ai.mcp_error = Some(error.to_string()),
            }
        }
    } else if let Some(mut server) = ai.mcp.take() {
        server.stop();
    }
    ai.save_access();
    refresh(ui, shell);
}

/// Stop the MCP server before the program ends, so the socket goes.
pub fn shut_down(shell: &mut Shell) {
    if let Some(ai) = &mut shell.ai {
        if let Some(cancel) = &ai.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(mut server) = ai.mcp.take() {
            server.stop();
        }
        for (_, answer) in ai.approvals.drain(..) {
            let _ = answer.send(Approval::Deny);
        }
    }
}

// --- Approvals ---------------------------------------------------------------

struct Cards {
    ui: slint::Weak<AppWindow>,
}

impl Approver for Cards {
    fn ask(&self, request: ApprovalRequest, timeout: Duration) -> Approval {
        let id = request.id as i32;
        let (sender, answer) = mpsc::channel();
        let shown = on_ui(&self.ui, UI_WAIT, move |ui, shell| {
            show_card(ui, shell, request, sender)
        });
        if shown != Some(true) {
            return Approval::Deny;
        }
        match answer.recv_timeout(timeout) {
            Ok(choice) => choice,
            Err(_) => {
                let _ = on_ui(&self.ui, UI_WAIT, move |ui, shell| {
                    take_card(ui, shell, id);
                });
                Approval::TimedOut
            }
        }
    }
}

/// What a card shows of the text a tool would write.
fn preview_of(args: &Value) -> String {
    let text = ["text", "replace", "find"]
        .iter()
        .filter_map(|name| args[*name].as_str())
        .next()
        .unwrap_or_default();
    let mut shown: String = text.chars().take(400).collect();
    if text.chars().count() > 400 {
        shown.push('…');
    }
    shown
}

fn show_card(
    ui: &AppWindow,
    shell: &mut Shell,
    request: ApprovalRequest,
    answer: mpsc::Sender<Approval>,
) -> bool {
    let strings = shell.language.strings();
    let Some(ai) = &mut shell.ai else {
        return false;
    };
    let name = request
        .document
        .as_ref()
        .map(|d| d.title.clone())
        .unwrap_or_default();
    let action = match request.reason {
        Reason::Edit => i18n::fill(strings.approval_edit, "name", &name),
        Reason::Save => i18n::fill(strings.approval_save, "name", &name),
        Reason::Create => strings.approval_create.to_owned(),
        Reason::ReadOnly => i18n::fill(strings.approval_other, "tool", request.tool.name),
    };
    let title = match request.origin.caller.strip_prefix("mcp:") {
        Some(client) => i18n::fill(
            &i18n::fill(strings.approval_mcp, "client", client),
            "action",
            &action,
        ),
        None => i18n::fill(strings.approval_ai, "action", &action),
    };
    let item = ApprovalItem {
        id: request.id as i32,
        title: title.into(),
        detail: format!("{} · {}", request.tool.name, request.origin.caller).into(),
        preview: preview_of(&request.args).into(),
    };
    ai.approval_items.push(item.clone());
    ai.approvals.push((item, answer));
    // Open the panel for the question without moving the keyboard.
    ui.set_assistant_open(true);
    true
}

fn take_card(_ui: &AppWindow, shell: &mut Shell, id: i32) -> Option<mpsc::Sender<Approval>> {
    let ai = shell.ai.as_mut()?;
    let index = ai.approvals.iter().position(|(item, _)| item.id == id)?;
    let (_, answer) = ai.approvals.remove(index);
    if let Some(row) = (0..ai.approval_items.row_count()).find(|row| {
        ai.approval_items
            .row_data(*row)
            .is_some_and(|item| item.id == id)
    }) {
        ai.approval_items.remove(row);
    }
    Some(answer)
}

/// The user answered a card: 0 deny, 1 allow, 2 allow for this chat.
pub fn answer(ui: &AppWindow, shell: &mut Shell, id: i32, choice: i32) {
    if let Some(answer) = take_card(ui, shell, id) {
        let _ = answer.send(match choice {
            1 => Approval::Allow,
            2 => Approval::AllowSession,
            _ => Approval::Deny,
        });
    }
}

// --- Tools on the shell ------------------------------------------------------

struct ShellHost {
    ui: slint::Weak<AppWindow>,
}

fn kind_name(kind: DocKind) -> &'static str {
    match kind {
        DocKind::Markdown => "markdown",
        DocKind::Writer => "text",
        DocKind::Sheets => "spreadsheet",
        DocKind::Slides => "presentation",
        _ => "other",
    }
}

fn documents(shell: &mut Shell) -> Vec<DocumentInfo> {
    let tabs: Vec<_> = shell
        .tabs
        .tabs()
        .iter()
        .map(|tab| (tab.id, tab.kind, tab.title.clone(), tab.modified))
        .collect();
    tabs.into_iter()
        .map(|(id, kind, title, modified)| DocumentInfo {
            id: id.0.to_string(),
            title,
            path: shell.markdown(id).and_then(|editor| editor.path.clone()),
            kind: kind_name(kind).to_owned(),
            modified,
        })
        .collect()
}

fn tab_id(document: Option<&DocumentInfo>) -> Result<TabId, String> {
    document
        .and_then(|d| d.id.parse().ok())
        .map(TabId)
        .ok_or_else(|| "no such document".to_owned())
}

const NO_ANSWER: &str = "Aster Office did not answer in time; try again";

impl Host for ShellHost {
    fn documents(&self) -> Vec<DocumentInfo> {
        on_ui(&self.ui, UI_WAIT, |_, shell| documents(shell)).unwrap_or_default()
    }

    fn run(
        &self,
        tool: &ToolSpec,
        document: Option<&DocumentInfo>,
        args: &Value,
    ) -> Result<String, String> {
        let name = tool.name;
        let args = args.clone();
        match name {
            "list_folder" => list_folder(Path::new(args["path"].as_str().unwrap_or_default())),
            "open_file" => self.open_file(PathBuf::from(args["path"].as_str().unwrap_or_default())),
            "create_document" => on_ui(&self.ui, UI_WAIT, move |ui, shell| {
                let id = shell.open_untitled(DocKind::Markdown);
                if let Some(title) = args["title"].as_str().filter(|t| !t.trim().is_empty()) {
                    shell.tabs.rename(id, title.trim());
                }
                let text = args["text"].as_str().unwrap_or_default().to_owned();
                if let Some(editor) = shell.markdown(id) {
                    editor.apply_external(0..0, &text, Selection::caret(0))?;
                }
                after_tab_change(ui, shell);
                Ok(json!({ "document": id.0.to_string() }).to_string())
            })
            .unwrap_or_else(|| Err(NO_ANSWER.to_owned())),
            "save_document" => self.save(tab_id(document)?),
            _ => {
                let id = tab_id(document)?;
                on_ui(&self.ui, UI_WAIT, move |ui, shell| {
                    let Some(editor) = shell.markdown(id) else {
                        return Err(
                            "only Markdown documents can be read and changed for now".to_owned()
                        );
                    };
                    let result = assistant::run(editor, name, &args);
                    if result.is_ok() && !tool_reads_only(name) {
                        after_edit(ui, shell, true);
                    }
                    result
                })
                .unwrap_or_else(|| Err(NO_ANSWER.to_owned()))
            }
        }
    }
}

fn tool_reads_only(name: &str) -> bool {
    aster_tools::find(name).is_some_and(|tool| tool.annotations.read_only)
}

impl ShellHost {
    fn open_file(&self, path: PathBuf) -> Result<String, String> {
        let opens = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt"));
        if !opens || !path.is_file() {
            return Err("only Markdown and text files can be opened this way".to_owned());
        }
        let wanted = path.clone();
        let found = on_ui(&self.ui, UI_WAIT, move |ui, shell| {
            let found = shell.tab_of_file(&wanted);
            if found.is_none() {
                start_load(ui, shell, wanted);
            }
            found
        })
        .ok_or(NO_ANSWER)?;
        if let Some(id) = found {
            return Ok(json!({ "document": id.0.to_string() }).to_string());
        }
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            let wanted = path.clone();
            if let Some(Some(id)) = on_ui(&self.ui, UI_WAIT, move |_, shell| {
                shell.tab_of_file(&wanted)
            }) {
                return Ok(json!({ "document": id.0.to_string() }).to_string());
            }
        }
        Err("the file did not open in time".to_owned())
    }

    fn save(&self, id: TabId) -> Result<String, String> {
        let started = on_ui(&self.ui, UI_WAIT, move |ui, shell| {
            let editor = shell
                .markdown(id)
                .ok_or("only Markdown documents can be saved this way")?;
            let path = editor
                .path
                .clone()
                .ok_or("this document was never saved; ask the user to save it first")?;
            match shell.begin_save(id, path) {
                Some(job) => {
                    spawn_save(ui, job, AfterSave::Nothing);
                    Ok(())
                }
                None => Err("a save of this document is already running"),
            }
        })
        .ok_or(NO_ANSWER)?;
        started.map_err(str::to_owned)?;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            let state = on_ui(&self.ui, UI_WAIT, move |_, shell| {
                shell
                    .markdown(id)
                    .map(|e| (e.saving, e.document.is_modified()))
            });
            match state {
                Some(Some((false, modified))) => {
                    return if modified {
                        Err("the file could not be saved".to_owned())
                    } else {
                        Ok(json!({ "saved": true }).to_string())
                    };
                }
                Some(None) => return Err("the document was closed".to_owned()),
                _ => {}
            }
        }
        Err("saving did not finish in time".to_owned())
    }
}

/// The entries of a folder: names, and which are folders. Hidden entries
/// are left out.
fn list_folder(path: &Path) -> Result<String, String> {
    let entries =
        std::fs::read_dir(path).map_err(|error| format!("cannot read the folder: {error}"))?;
    let mut listed: Vec<(bool, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let folder = entry.file_type().is_ok_and(|t| t.is_dir());
            Some((!folder, name))
        })
        .collect();
    listed.sort();
    let total = listed.len();
    let entries: Vec<Value> = listed
        .into_iter()
        .take(500)
        .map(|(file, name)| json!({ "name": name, "folder": !file }))
        .collect();
    Ok(
        json!({ "path": path.display().to_string(), "entries": entries, "total": total })
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_models_one_can_chat_with_are_listed() {
        for id in [
            "deepseek-chat",
            "deepseek-reasoner",
            "gpt-5.1",
            "claude-opus-5-5",
            "qwen3:8b",
        ] {
            assert!(can_chat(id), "{id}");
        }
        for id in [
            "text-embedding-3-small",
            "whisper-1",
            "tts-1-hd",
            "dall-e-3",
            "gpt-image-1",
            "omni-moderation-latest",
            "babbage-002",
            "gpt-4o-realtime-preview",
            "gpt-4o-transcribe",
        ] {
            assert!(!can_chat(id), "{id}");
        }
    }
}
