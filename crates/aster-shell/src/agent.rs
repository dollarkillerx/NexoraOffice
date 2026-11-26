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
use aster_shell::doc_tools;
use aster_shell::i18n;
use aster_shell::sheet_tools;
use aster_shell::tabs::{DocKind, TabId};
use aster_tools::{
    Approval, ApprovalRequest, Approver, Audit, DocumentInfo, Host, Invoker, Level, Policy, Reason,
    ToolSpec,
};
use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{
    AfterSave, AppWindow, ApprovalItem, ChatItem, Content, Shell, after_edit, after_sheet_change,
    after_tab_change, deliver, refresh_tabs, spawn_save, start_load, start_sheet_save,
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
    /// Questions on screen: the card, where the answer goes, and whether
    /// an MCP client asked (those are refused when MCP stops).
    approvals: Vec<(ApprovalItem, mpsc::Sender<Approval>, bool)>,
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
    /// Stops the running turn's tool calls: questions withdrawn, calls not
    /// started yet refused.
    turn_stop: Option<aster_ai::TurnStop>,
    /// Whether a key is stored, by provider id: the key store is asked once.
    has_key: HashMap<String, bool>,
    /// The assistant's reply being streamed, by row.
    streaming: Option<usize>,
    /// Tool calls on screen, by call id.
    tool_rows: HashMap<String, usize>,
    /// Events from the running turn, applied in order.
    inbox: Arc<Mutex<Vec<AgentEvent>>>,
    /// A drain of the inbox is on its way to the interface thread: the
    /// worker posts at most one at a time, however fast fragments arrive.
    drain_posted: Arc<AtomicBool>,
    /// The text of the reply being streamed. Fragments are added here and
    /// the row is replaced once per drain, not once per fragment.
    streaming_text: String,
    /// The models each provider offers, by provider id.
    models: HashMap<String, ModelList>,
    /// Providers whose key was deleted because their endpoint moved to
    /// another host, until a key is saved again.
    keys_cleared: std::collections::HashSet<String>,
    /// Why the last attempt to save a key was refused, shown where the key
    /// state is until the next attempt.
    key_note: Option<&'static str>,
    /// The other conversations of this run, most recent first. Kept in
    /// memory only, like the one on screen.
    chats: Vec<Chat>,
    /// The conversation the running turn belongs to. Its events and result
    /// go to that conversation only, even after another was put on screen.
    turn_chat: Option<u64>,
    /// The connections of the MCP clients the settings page lists, in its
    /// order, for its "Disconnect" buttons.
    mcp_connections: Vec<u64>,
}

/// A conversation put aside while another is on screen.
struct Chat {
    id: u64,
    title: String,
    conversation: Vec<aster_ai::Message>,
    items: Vec<ChatItem>,
}

/// At most this many conversations are kept aside; older ones are let go.
const MAX_CHATS: usize = 20;

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
            drain_posted: Arc::new(AtomicBool::new(false)),
            streaming_text: String::new(),
            models: HashMap::new(),
            keys_cleared: Default::default(),
            key_note: None,
            chats: Vec::new(),
            turn_stop: None,
            turn_chat: None,
            mcp_connections: Vec::new(),
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
        .and_then(|(id, _, _)| document_path(shell, *id));
    let can_insert = shell
        .active_markdown()
        .is_some_and(|editor| editor.view_mode.shows_source() && !editor.locked());
    let Some(ai) = &mut shell.ai else {
        return;
    };
    let policy = ai.policy.lock().map(|p| p.clone()).unwrap_or_default();
    match active {
        Some((id, DocKind::Markdown | DocKind::Sheets | DocKind::Writer, title)) => {
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
    ui.set_ai_chats(crate::model(
        ai.chats
            .iter()
            .map(|chat| SharedString::from(chat.title.as_str()))
            .collect(),
    ));
    ui.set_ai_can_insert(can_insert);
    // The last answer, if nothing was asked after it: only it can be asked
    // again.
    let rows = ai.items.row_count();
    let last_request = (0..rows)
        .rev()
        .find(|&row| ai.items.row_data(row).is_some_and(|i| i.kind == 0));
    let last_reply = (0..rows)
        .rev()
        .find(|&row| ai.items.row_data(row).is_some_and(|i| i.kind == 1))
        .filter(|&row| last_request.is_none_or(|request| row > request));
    ui.set_ai_last_reply(last_reply.map_or(-1, |row| row as i32));

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
    let cleared = provider
        .as_ref()
        .is_some_and(|p| ai.keys_cleared.contains(&p.id));
    let elsewhere = provider.as_ref().is_some_and(|p| !p.key_matches_host());
    ui.set_ai_key_state(
        if let Some(note) = ai.key_note {
            note
        } else if has_key && elsewhere {
            strings.ai_key_elsewhere
        } else if has_key {
            strings.ai_key_saved
        } else if cleared {
            strings.ai_key_cleared
        } else {
            strings.ai_key_missing
        }
        .into(),
    );
    ui.set_ai_insecure(provider.as_ref().is_some_and(|p| {
        p.base_url
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("http://")
            && !aster_ai::is_loopback_url(&p.base_url)
    }));
    ui.set_ai_allow_insecure(provider.as_ref().is_some_and(|p| p.allow_insecure));
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
            let clients = server.clients();
            ai.mcp_connections = clients.iter().map(|c| c.connection).collect();
            let names: Vec<String> = clients.into_iter().map(|c| c.name).collect();
            ui.set_mcp_client_names(crate::model(
                names
                    .iter()
                    .map(|name| SharedString::from(name.as_str()))
                    .collect(),
            ));
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
    if ai.mcp.is_none() {
        ai.mcp_connections.clear();
        ui.set_mcp_client_names(crate::model(Vec::new()));
    }
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
    ai.key_note = None;
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
        let old_origin = aster_ai::url_origin(&provider.base_url);
        let moved = old_origin != aster_ai::url_origin(&url);
        provider.base_url = url;
        let id = provider.id.clone();
        if moved {
            // A key belongs to the origin it was saved for (scheme, host
            // and port): one typed for one service must not go to another,
            // nor over plain http where it was saved for https. It is
            // deleted, and the user saves it again for the new address.
            // Insecure http is allowed per address, so that goes too. A key
            // that is still there (deleting it failed, or it was never
            // recorded with an origin) stays bound to the old origin, so it
            // cannot follow the address; saving a key again binds it anew.
            provider.key_host = provider.key_host.take().or(old_origin);
            provider.allow_insecure = false;
            if key_known(ai, &id) {
                let _ = ai.keys.delete(&id);
                ai.has_key.insert(id.clone(), false);
                ai.keys_cleared.insert(id.clone());
            }
        }
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
        // The key is bound to the address it is saved for; without a
        // scheme there is no address to bind it to.
        let origin = selected_mut(ai).and_then(|provider| {
            provider.base_url = aster_ai::with_scheme(&provider.base_url);
            aster_ai::url_origin(&provider.base_url)
        });
        ai.key_note = None;
        if origin.is_none() {
            ai.key_note = Some(strings.ai_key_needs_address);
            refresh(ui, shell);
            return;
        }
        match ai.keys.set(&id, key.trim()) {
            Ok(()) => {
                ai.has_key.insert(id.clone(), true);
                ai.keys_cleared.remove(&id);
                // The key is for this address only.
                if let Some(provider) = selected_mut(ai) {
                    provider.key_host = origin;
                }
                save_settings(ai);
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
    let id = provider.id.clone();
    let weak = ui.as_weak();
    let started = std::thread::Builder::new()
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
        });
    if let Err(error) = started
        && let Some(ai) = &mut shell.ai
    {
        ai.models.insert(id, ModelList::Failed(error.to_string()));
    }
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

/// Allow plain http to a computer other than this one for the selected
/// provider, after the warning on the settings page.
pub fn set_allow_insecure(ui: &AppWindow, shell: &mut Shell, on: bool) {
    if let Some(ai) = &mut shell.ai
        && let Some(provider) = selected_mut(ai)
    {
        provider.allow_insecure = on;
        let id = provider.id.clone();
        ai.models.remove(&id);
        save_settings(ai);
    }
    refresh(ui, shell);
    if on {
        list_models_if_new(ui, shell);
    }
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
        .filter(|tab| {
            matches!(
                tab.kind,
                DocKind::Markdown | DocKind::Sheets | DocKind::Writer
            )
        })
        .map(|tab| (tab.id, tab.title.clone()))
    else {
        return context;
    };
    let path = document_path(shell, id);
    let Some(policy) = shell
        .ai
        .as_ref()
        .and_then(|ai| ai.policy.lock().ok().map(|p| p.clone()))
    else {
        return context;
    };
    if policy.level_of(&id.0.to_string(), path.as_deref()) == Level::Off {
        return context;
    }
    context.document_id = Some(id.0.to_string());
    context.document_title = Some(title);
    if let Some(Content::Doc(tab)) = shell.contents.get(&id) {
        let selected = tab.view.selected_text();
        if !selected.is_empty() {
            context.selection = Some(selected);
        }
        return context;
    }
    if let Some(Content::Sheet(tab)) = shell.contents.get(&id) {
        // Where the user is in the workbook; the cells themselves are read
        // with read_sheet.
        let sheet = tab.view.sheet().map_or("", |sheet| sheet.name.as_str());
        context.selected_cells = Some(format!("{sheet}!{}", tab.view.selection()));
        return context;
    }
    let Some(editor) = shell.markdown(id) else {
        return context;
    };
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
    let chat = ai.conversation_id;
    ai.turn_chat = Some(chat);
    let stop = aster_ai::TurnStop::new();
    ai.turn_stop = Some(stop.clone());
    let mut conversation = ai.conversation.clone();
    let invoker = ai.invoker.clone();
    let options = ai.settings.agent_options();
    let conversation_id = format!("chat-{}", ai.conversation_id);
    let inbox = ai.inbox.clone();
    let posted = ai.drain_posted.clone();
    let weak = ui.as_weak();
    let started = std::thread::Builder::new()
        .name("aster-assistant".into())
        .spawn(move || {
            let model = connect(provider.clone(), key);
            let tools = tool_defs(aster_tools::tools());
            let executor = InvokerExecutor::new(&invoker, &provider, &conversation_id, stop);
            let mut sink = |event: AgentEvent| {
                if let Ok(mut inbox) = inbox.lock() {
                    inbox.push(event);
                }
                if !posted.swap(true, Ordering::AcqRel) {
                    let _ = weak.upgrade_in_event_loop(|ui| {
                        deliver(ui, Box::new(drain));
                    });
                }
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
                            // as it was before it. One put aside meanwhile
                            // gets its result where it is kept.
                            if result.is_ok() {
                                if ai.conversation_id == chat {
                                    ai.conversation = conversation;
                                } else if let Some(kept) =
                                    ai.chats.iter_mut().find(|kept| kept.id == chat)
                                {
                                    kept.conversation = conversation;
                                }
                            }
                            ai.cancel = None;
                            ai.turn_chat = None;
                            ai.turn_stop = None;
                            ai.streaming = None;
                        }
                        refresh(ui, shell);
                    }),
                );
            });
        });
    if let Err(error) = started {
        // Without a thread there is no turn: say so instead of waiting.
        let strings = shell.language.strings();
        if let Some(ai) = &mut shell.ai {
            ai.cancel = None;
            ai.turn_chat = None;
            ai.turn_stop = None;
            ai.conversation.pop();
            ai.push(
                3,
                i18n::fill(strings.ai_error, "message", &error.to_string()),
                String::new(),
                0,
            );
        }
    }
    refresh(ui, shell);
}

/// Put the streamed text into its row.
fn flush_stream(ai: &mut Assistant) {
    if let Some(row) = ai.streaming
        && let Some(mut item) = ai.items.row_data(row)
    {
        item.text = ai.streaming_text.as_str().into();
        ai.items.set_row_data(row, item);
    }
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
    // Cleared before taking the events: anything that arrives after this
    // posts a drain of its own.
    ai.drain_posted.store(false, Ordering::Release);
    let events: Vec<AgentEvent> = ai
        .inbox
        .lock()
        .map(|mut inbox| std::mem::take(&mut *inbox))
        .unwrap_or_default();
    // What a turn of a conversation no longer on screen still says (it was
    // stopped when another was started) does not belong to this one.
    if ai.turn_chat.is_some_and(|chat| chat != ai.conversation_id) {
        return;
    }
    for event in events {
        if !matches!(event, AgentEvent::TextDelta(_)) {
            flush_stream(ai);
        }
        match event {
            AgentEvent::TextDelta(delta) => match ai.streaming {
                Some(row) if row < ai.items.row_count() => ai.streaming_text.push_str(&delta),
                _ => {
                    ai.push(1, String::new(), String::new(), 0);
                    ai.streaming = Some(ai.items.row_count() - 1);
                    ai.streaming_text = delta;
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
    flush_stream(ai);
    let _ = ui;
}

/// A quick action from the panel: a request ready-made in the interface
/// language, sent like one the user typed (0 rewrite, 1 proofread, 2
/// summarise, 3 continue writing at the caret, 4 translate).
pub fn quick(ui: &AppWindow, shell: &mut Shell, action: i32) {
    let strings = shell.language.strings();
    let prompt = match action {
        0 => strings.ai_prompt_rewrite,
        1 => strings.ai_prompt_proofread,
        2 => strings.ai_prompt_summarize,
        3 => strings.ai_prompt_continue,
        _ => strings.ai_prompt_translate,
    };
    send(ui, shell, prompt);
}

pub fn stop(ui: &AppWindow, shell: &mut Shell) {
    if let Some(ai) = &shell.ai {
        ai.stop_turn();
    }
    refresh(ui, shell);
}

impl Assistant {
    /// Stop the running turn: its request to the model, its questions on
    /// screen, and its tool calls not started yet.
    fn stop_turn(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(stop) = &self.turn_stop {
            stop.stop(&self.invoker);
        }
    }

    /// The title of the conversation on screen: its first request.
    fn current_title(&self) -> Option<String> {
        self.items
            .iter()
            .find(|item| item.kind == 0)
            .map(|item| aster_ai::title(&item.text))
    }

    /// Put the conversation on screen aside, if it has anything in it, and
    /// clear the screen. Its "allow for this chat" answers stay with it.
    fn stash_current(&mut self) {
        if let Some(title) = self.current_title() {
            let chat = Chat {
                id: self.conversation_id,
                title,
                conversation: std::mem::take(&mut self.conversation),
                items: self.items.iter().collect(),
            };
            self.chats.retain(|kept| kept.id != chat.id);
            self.chats.insert(0, chat);
            for dropped in self.chats.drain(MAX_CHATS.min(self.chats.len())..) {
                self.invoker.end_session(&format!("chat-{}", dropped.id));
            }
        } else {
            self.invoker
                .end_session(&format!("chat-{}", self.conversation_id));
        }
        self.conversation.clear();
        self.items.clear();
        self.tool_rows.clear();
        self.streaming = None;
    }

    /// A number for a conversation never used before.
    fn fresh_id(&self) -> u64 {
        let kept = self.chats.iter().map(|chat| chat.id).max().unwrap_or(0);
        self.conversation_id.max(kept) + 1
    }
}

/// Start a new conversation. The one on screen is kept aside, with its
/// "allow for this chat" answers, and can be gone back to.
pub fn new_chat(ui: &AppWindow, shell: &mut Shell) {
    if let Some(ai) = &mut shell.ai {
        ai.stop_turn();
        ai.stash_current();
        ai.conversation_id = ai.fresh_id();
    }
    refresh(ui, shell);
}

/// Go back to a conversation put aside (`index` into the list the panel
/// shows). Not while a turn runs.
pub fn switch_chat(ui: &AppWindow, shell: &mut Shell, index: i32) {
    if let Some(ai) = &mut shell.ai
        && ai.cancel.is_none()
        && let Ok(index) = usize::try_from(index)
        && index < ai.chats.len()
    {
        let chat = ai.chats.remove(index);
        ai.stash_current();
        ai.conversation_id = chat.id;
        ai.conversation = chat.conversation;
        ai.items.set_vec(chat.items);
    }
    refresh(ui, shell);
}

/// What a reply's buttons do: 0 copy it, 1 insert it at the caret of the
/// document in front, 2 put it in place of the selection there, 3 ask
/// again (the last reply only).
pub fn reply_action(ui: &AppWindow, shell: &mut Shell, row: i32, action: i32) {
    let Some(text) = shell.ai.as_ref().and_then(|ai| {
        let item = ai.items.row_data(usize::try_from(row).ok()?)?;
        (item.kind == 1).then(|| item.text.to_string())
    }) else {
        return;
    };
    match action {
        0 => ui.invoke_copy_to_clipboard(text.into()),
        1 | 2 => {
            let Some(editor) = shell.active_markdown() else {
                return;
            };
            if !editor.view_mode.shows_source() {
                return;
            }
            let selection = editor.document.selection().range();
            let range = if action == 1 {
                selection.end..selection.end
            } else {
                selection
            };
            let end = range.start + text.len();
            if editor
                .apply_external(range, &text, Selection::caret(end))
                .is_ok()
            {
                after_edit(ui, shell, true);
                ui.invoke_focus_document();
            }
        }
        _ => retry(ui, shell),
    }
}

/// Ask the last request again: the last answer and everything after the
/// request go, and the request is sent as it was.
fn retry(ui: &AppWindow, shell: &mut Shell) {
    let Some(ai) = &mut shell.ai else {
        return;
    };
    if ai.cancel.is_some() {
        return;
    }
    let Some(row) = (0..ai.items.row_count())
        .rev()
        .find(|&row| ai.items.row_data(row).is_some_and(|item| item.kind == 0))
    else {
        return;
    };
    let Some(request) = ai.items.row_data(row).map(|item| item.text.to_string()) else {
        return;
    };
    while ai.items.row_count() > row {
        ai.items.remove(ai.items.row_count() - 1);
    }
    aster_ai::back_to_last_request(&mut ai.conversation);
    ai.tool_rows.clear();
    ai.streaming = None;
    send(ui, shell, &request);
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
    if !on {
        // Questions from clients that are gone are refused, so that no
        // answer given later can reach anything.
        let waiting: Vec<i32> = ai
            .approvals
            .iter()
            .filter(|(_, _, mcp)| *mcp)
            .map(|(item, _, _)| item.id)
            .collect();
        for id in waiting {
            if let Some(answer) = take_card(ui, shell, id) {
                let _ = answer.send(Approval::Deny);
            }
        }
    }
    refresh(ui, shell);
}

/// Stop the MCP server before the program ends, so the socket goes.
/// Close one MCP client's connection, from the settings page. Its
/// questions on screen are withdrawn.
pub fn disconnect_client(ui: &AppWindow, shell: &mut Shell, index: i32) {
    if let Some(ai) = &shell.ai
        && let Some(server) = &ai.mcp
        && let Some(&connection) = usize::try_from(index)
            .ok()
            .and_then(|index| ai.mcp_connections.get(index))
    {
        server.disconnect(connection);
    }
    refresh(ui, shell);
}

pub fn shut_down(shell: &mut Shell) {
    if let Some(ai) = &mut shell.ai {
        if let Some(cancel) = &ai.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(mut server) = ai.mcp.take() {
            server.stop();
        }
        for (_, answer, _) in ai.approvals.drain(..) {
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
        // Set once this question is no longer waited for, so that a card
        // the interface gets to only afterwards is not shown at all.
        let given_up = Arc::new(AtomicBool::new(false));
        let shown = on_ui(&self.ui, UI_WAIT, {
            let given_up = given_up.clone();
            move |ui, shell| {
                if given_up.load(Ordering::SeqCst) {
                    return false;
                }
                show_card(ui, shell, request, sender)
            }
        });
        if shown != Some(true) {
            // The interface did not answer in time: it may still show the
            // card later. Make sure it does not stay.
            given_up.store(true, Ordering::SeqCst);
            let _ = self.ui.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| {
                        take_card(ui, shell, id);
                    }),
                );
            });
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

    /// The caller gave up on the question (an MCP client cancelled or was
    /// disconnected): the card goes and the wait ends as a refusal. Posted,
    /// never waited for: this can be called on the interface thread itself.
    fn withdraw(&self, id: u64) {
        let id = id as i32;
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    if let Some(answer) = take_card(ui, shell, id) {
                        let _ = answer.send(Approval::Deny);
                    }
                }),
            );
        });
    }
}

/// Text for a card, at most `limit` characters, saying how long it was.
fn shortened(text: &str, limit: usize, strings: &i18n::Strings) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit).collect();
    format!(
        "{kept}{}",
        i18n::fill(strings.approval_cut, "chars", &count.to_string())
    )
}

/// What a call would do, for its card: a line of facts, the text it would
/// replace or delete, and the text it would write. Read from the document
/// as it is now, on the interface thread.
fn effect(shell: &mut Shell, request: &ApprovalRequest) -> (String, String, String) {
    const SHOWN: usize = 600;
    let strings = shell.language.strings();
    let args = &request.args;
    let text_arg = |name: &str| args[name].as_str().unwrap_or_default().to_owned();
    let written = |text: String| {
        if text.is_empty() {
            strings.approval_nothing.to_owned()
        } else {
            shortened(&text, SHOWN, strings)
        }
    };
    let id = request
        .document
        .as_ref()
        .and_then(|d| d.id.parse().ok())
        .map(TabId);
    if let Some(Content::Doc(tab)) = id.and_then(|id| shell.contents.get(&id)) {
        let (facts, removed, new) = doc_tools::card(tab, request.tool.name, args, strings);
        return (facts, shortened(&removed, SHOWN, strings), written(new));
    }
    if request.tool.name == "write_cells" {
        let Some(Content::Sheet(tab)) = id.and_then(|id| shell.contents.get(&id)) else {
            return (String::new(), String::new(), String::new());
        };
        let entries = sheet_tools::entries(args).unwrap_or_default();
        let new = written(sheet_tools::as_rows(&entries));
        let Some((sheet, range, now)) = sheet_tools::before_write(tab, args) else {
            return (String::new(), String::new(), new);
        };
        let count = u64::from(range.rows()) * u64::from(range.cols());
        let facts = i18n::fill(
            &i18n::fill(
                &i18n::fill(strings.approval_cells, "sheet", &sheet),
                "range",
                &range.to_string(),
            ),
            "count",
            &count.to_string(),
        );
        return (facts, shortened(&now, SHOWN, strings), new);
    }
    let editor = id.and_then(|id| shell.markdown(id));
    match request.tool.name {
        "replace_lines" => {
            let (Some(editor), Some(first), Some(last)) = (
                editor,
                args["start_line"].as_u64(),
                args["end_line"].as_u64(),
            ) else {
                return (String::new(), String::new(), written(text_arg("text")));
            };
            let text = editor.document.text();
            let total = text.len_lines() as u64;
            let facts = i18n::fill(
                &i18n::fill(
                    &i18n::fill(strings.approval_lines, "start", &first.to_string()),
                    "end",
                    &last.to_string(),
                ),
                "total",
                &total.to_string(),
            );
            let removed = if first >= 1 && first <= last && last <= total {
                let start = text.line_to_byte(first as usize - 1);
                let end = if last < total {
                    text.line_to_byte(last as usize) - 1
                } else {
                    text.len_bytes()
                };
                shortened(&text.byte_slice(start..end).to_string(), SHOWN, strings)
            } else {
                String::new()
            };
            (facts, removed, written(text_arg("text")))
        }
        "replace_selection" => {
            let Some(editor) = editor else {
                return (String::new(), String::new(), written(text_arg("text")));
            };
            let range = editor.document.selection().range();
            let selected = editor.document.text().byte_slice(range).to_string();
            let facts = i18n::fill(
                strings.approval_selection,
                "chars",
                &selected.chars().count().to_string(),
            );
            (
                facts,
                shortened(&selected, SHOWN, strings),
                written(text_arg("text")),
            )
        }
        "find_and_replace" => {
            let pattern = text_arg("find");
            let regex = args["regex"].as_bool().unwrap_or(false);
            let query = aster_markdown::search::Query {
                pattern: pattern.clone(),
                case_sensitive: args["case_sensitive"].as_bool().unwrap_or(true),
                whole_word: false,
                regex,
            };
            // Counting reads the whole text; past this size the card says "?".
            let count = editor
                .filter(|e| e.document.len() <= 16 * 1024 * 1024)
                .and_then(|e| {
                    let finder = aster_markdown::search::Finder::new(&query).ok()??;
                    let whole = e.document.text().to_string();
                    finder
                        .find_all(&whole, &AtomicBool::new(false), |_| {})
                        .map(|found| found.matches.len().to_string())
                })
                .unwrap_or_else(|| "?".to_owned());
            let scope = if args["all"].as_bool().unwrap_or(true) {
                // With a limit, the card counts what will be replaced.
                let count = match (args["limit"].as_u64(), count.parse::<u64>()) {
                    (Some(limit), Ok(found)) => limit.min(found).to_string(),
                    _ => count.clone(),
                };
                i18n::fill(strings.approval_all, "count", &count)
            } else {
                i18n::fill(strings.approval_first, "count", &count)
            };
            let facts = i18n::fill(
                &i18n::fill(
                    &i18n::fill(
                        strings.approval_find,
                        "pattern",
                        &format!("“{}”", shortened(&pattern, 200, strings)),
                    ),
                    "kind",
                    if regex {
                        strings.approval_regex
                    } else {
                        strings.approval_plain
                    },
                ),
                "scope",
                &scope,
            );
            (facts, String::new(), written(text_arg("replace")))
        }
        "insert_text" => {
            let place = match args["position"].as_str().unwrap_or("cursor") {
                "start" => strings.approval_at_start.to_owned(),
                "end" => strings.approval_at_end.to_owned(),
                "line" => i18n::fill(
                    strings.approval_at_line,
                    "line",
                    &args["line"].as_u64().unwrap_or(0).to_string(),
                ),
                _ => strings.approval_at_cursor.to_owned(),
            };
            (
                i18n::fill(strings.approval_where, "where", &place),
                String::new(),
                written(text_arg("text")),
            )
        }
        "create_document" => (
            i18n::fill(strings.approval_new_title, "title", &text_arg("title")),
            String::new(),
            written(text_arg("text")),
        ),
        "save_document" => {
            let path = editor
                .and_then(|e| e.path.clone())
                .or_else(|| id.and_then(|id| document_path(shell, id)))
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            (
                i18n::fill(strings.approval_file, "path", &path),
                String::new(),
                String::new(),
            )
        }
        _ => (String::new(), String::new(), String::new()),
    }
}

fn show_card(
    ui: &AppWindow,
    shell: &mut Shell,
    request: ApprovalRequest,
    answer: mpsc::Sender<Approval>,
) -> bool {
    // Withdrawn before it could be shown.
    if request.token.is_cancelled() {
        return false;
    }
    let strings = shell.language.strings();
    let (facts, removed, written) = effect(shell, &request);
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
    let mcp = request.origin.is_mcp();
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
        facts: facts.into(),
        removed: removed.into(),
        preview: written.into(),
    };
    ai.approval_items.push(item.clone());
    ai.approvals.push((item, answer, mcp));
    // Open the panel for the question without moving the keyboard.
    ui.set_assistant_open(true);
    true
}

fn take_card(_ui: &AppWindow, shell: &mut Shell, id: i32) -> Option<mpsc::Sender<Approval>> {
    let ai = shell.ai.as_mut()?;
    let index = ai.approvals.iter().position(|(item, _, _)| item.id == id)?;
    let (_, answer, _) = ai.approvals.remove(index);
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
        DocKind::Writer => "word",
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
            path: document_path(shell, id),
            kind: kind_name(kind).to_owned(),
            modified,
        })
        .collect()
}

/// The file a document is kept in, which folder rules apply to.
fn document_path(shell: &mut Shell, id: TabId) -> Option<PathBuf> {
    match shell.contents.get(&id)? {
        Content::Markdown(editor) => editor.path.clone(),
        Content::Sheet(tab) => tab.path.clone(),
        Content::Doc(tab) => tab.path.clone(),
        _ => None,
    }
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
            "save_document" => self.save(tab_id(document)?, None),
            _ => self.on_document(tab_id(document)?, name, args, None),
        }
    }

    fn fingerprint(
        &self,
        tool: &ToolSpec,
        document: Option<&DocumentInfo>,
        args: &Value,
    ) -> Option<String> {
        let id = tab_id(document).ok()?;
        let name = tool.name;
        let args = args.clone();
        on_ui(&self.ui, UI_WAIT, move |_, shell| {
            fingerprint_of(shell, id, name, &args)
        })
        .flatten()
    }

    fn run_checked(
        &self,
        tool: &ToolSpec,
        document: Option<&DocumentInfo>,
        args: &Value,
        expect: Option<&str>,
    ) -> Result<String, String> {
        let Some(expect) = expect.map(str::to_owned) else {
            return self.run(tool, document, args);
        };
        match tool.name {
            "save_document" => self.save(tab_id(document)?, Some(expect)),
            name if tool.on_document => {
                self.on_document(tab_id(document)?, name, args.clone(), Some(expect))
            }
            _ => self.run(tool, document, args),
        }
    }
}

/// What an approval card about `tool` on tab `id` was shown against: the
/// document's text version, and for the tools that act at the selection,
/// the selection too. An approved call runs only while this is unchanged.
fn fingerprint_of(shell: &mut Shell, id: TabId, tool: &str, args: &Value) -> Option<String> {
    if let Some(Content::Doc(tab)) = shell.contents.get(&id) {
        return Some(format!("r{}", tab.view.revision()));
    }
    if let Some(Content::Sheet(tab)) = shell.contents.get(&id) {
        return Some(match tool {
            "write_cells" => sheet_tools::fingerprint(tab, args),
            // Saving also checks where to: see the Markdown case below.
            _ => format!(
                "d{}:{}:{}",
                tab.history.depth(),
                tab.edited.len(),
                tab.path
                    .as_deref()
                    .map_or_else(String::new, |path| path.display().to_string())
            ),
        });
    }
    let editor = shell.markdown(id)?;
    let version = editor.text_version();
    let selection = editor.document.selection().range();
    Some(match tool {
        "replace_selection" | "insert_text" => {
            format!("v{version}:{}-{}", selection.start, selection.end)
        }
        // Where it would be saved too: a document saved under another name
        // while the user was deciding is not saved there unasked.
        "save_document" => format!(
            "v{version}:{}",
            editor
                .path
                .as_deref()
                .map_or_else(String::new, |path| path.display().to_string())
        ),
        _ => format!("v{version}"),
    })
}

impl ShellHost {
    /// Run a document tool on the interface thread. With `expect`, only if
    /// the document is still as the user was shown it, checked in the same
    /// step as the change so that nothing can come in between.
    fn on_document(
        &self,
        id: TabId,
        name: &'static str,
        args: Value,
        expect: Option<String>,
    ) -> Result<String, String> {
        on_ui(&self.ui, UI_WAIT, move |ui, shell| {
            run_on_document(ui, shell, id, name, &args, expect.as_deref())
        })
        .unwrap_or_else(|| Err(NO_ANSWER.to_owned()))
    }
}

/// The work of a document tool on the interface thread: the check against
/// what the user was shown, the tool, and what follows a change.
fn run_on_document(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    name: &str,
    args: &Value,
    expect: Option<&str>,
) -> Result<String, String> {
    {
        {
            if let Some(expect) = expect
                && fingerprint_of(shell, id, name, args).as_deref() != Some(expect)
            {
                return Err(aster_tools::CHANGED_WHILE_ASKING.to_owned());
            }
            let language = shell.language.tag();
            if let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) {
                let (result, changes) = sheet_tools::run(tab, name, args, language)?;
                if !changes.is_empty() {
                    if shell.tabs.active_id() == Some(id) {
                        after_sheet_change(ui, shell, changes);
                    } else {
                        tab.recalc(&changes);
                        if shell.sync_modified() {
                            refresh_tabs(ui, shell);
                        }
                    }
                }
                return Ok(result);
            }
            if let Some(Content::Doc(tab)) = shell.contents.get_mut(&id) {
                let (result, changed) = doc_tools::run(tab, name, args)?;
                if changed {
                    if shell.tabs.active_id() == Some(id) {
                        after_edit(ui, shell, true);
                    } else {
                        // Not in front: its modified mark and recovery record
                        // follow now; the toolbar and outline are made when it
                        // comes to the front.
                        if shell.sync_modified() {
                            refresh_tabs(ui, shell);
                        }
                        shell.sync_doc_records();
                    }
                }
                return Ok(result);
            }
            let Some(editor) = shell.markdown(id) else {
                return Err("only Markdown documents can be read and changed for now".to_owned());
            };
            let result = assistant::run(editor, name, args);
            if result.is_ok() && !tool_reads_only(name) {
                after_edit(ui, shell, true);
            }
            result
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

    fn save(&self, id: TabId, expect: Option<String>) -> Result<String, String> {
        let started = on_ui(&self.ui, UI_WAIT, move |ui, shell| {
            if let Some(expect) = &expect
                && fingerprint_of(shell, id, "save_document", &Value::Null).as_ref() != Some(expect)
            {
                return Err(aster_tools::CHANGED_WHILE_ASKING);
            }
            if let Some(Content::Sheet(tab)) = shell.contents.get(&id) {
                let path = tab
                    .path
                    .clone()
                    .ok_or("this workbook was never saved; ask the user to save it first")?;
                if tab.saving {
                    return Err("a save of this workbook is already running");
                }
                start_sheet_save(ui, shell, id, path, AfterSave::Nothing);
                return Ok(());
            }
            let editor = shell
                .markdown(id)
                .ok_or("only Markdown documents and workbooks can be saved this way")?;
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
                match shell.contents.get(&id)? {
                    Content::Sheet(tab) => Some((tab.saving, tab.is_modified())),
                    Content::Markdown(editor) => Some((editor.saving, editor.is_modified())),
                    _ => None,
                }
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
    fn a_save_is_checked_against_the_path_as_well() {
        use aster_shell::i18n::Language;
        use aster_shell::theme::ThemeMode;
        let mut shell = Shell::new(ThemeMode::Light, Language::ZhHans);
        let id = shell.open_untitled(DocKind::Markdown);
        shell.markdown(id).unwrap().path = Some(PathBuf::from("/notes/a.md"));
        let before = fingerprint_of(&mut shell, id, "save_document", &Value::Null).unwrap();
        // The same text, saved meanwhile under another name.
        shell.markdown(id).unwrap().path = Some(PathBuf::from("/notes/b.md"));
        assert_ne!(
            fingerprint_of(&mut shell, id, "save_document", &Value::Null),
            Some(before)
        );
        // Other tools look at the text only.
        let edit = fingerprint_of(&mut shell, id, "replace_lines", &Value::Null).unwrap();
        shell.markdown(id).unwrap().path = Some(PathBuf::from("/notes/c.md"));
        assert_eq!(
            fingerprint_of(&mut shell, id, "replace_lines", &Value::Null),
            Some(edit)
        );
    }

    /// A call on a Word tab goes the way the user's own edit does: the tab
    /// is marked modified, one undo takes the call back, and an approval
    /// given for an earlier version of the document is void.
    #[test]
    fn a_word_tool_call_is_an_ordinary_edit_of_the_tab() {
        use aster_shell::i18n::Language;
        use aster_shell::theme::ThemeMode;
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let mut shell = Shell::new(ThemeMode::Light, Language::En);
        let id = shell.open_untitled(DocKind::Writer);
        let modified = |shell: &mut Shell| {
            shell
                .tabs
                .tabs()
                .iter()
                .find(|tab| tab.id == id)
                .is_some_and(|tab| tab.modified)
        };
        let text = |shell: &mut Shell| match shell.contents.get(&id) {
            Some(Content::Doc(tab)) => tab.view.document().plain_text(),
            _ => panic!("a Word tab"),
        };
        assert!(!modified(&mut shell));
        let args = json!({ "document": id.0.to_string(), "block": 1, "text": "Hello\nWorld" });
        let shown = fingerprint_of(&mut shell, id, "replace_paragraphs", &args).unwrap();
        let read = run_on_document(&ui, &mut shell, id, "read_word", &json!({}), None).unwrap();
        assert!(read.contains("\"total_blocks\":1"), "{read}");
        // Reading changes nothing.
        assert!(!modified(&mut shell));
        assert_eq!(
            fingerprint_of(&mut shell, id, "replace_paragraphs", &args).as_ref(),
            Some(&shown)
        );
        let done = run_on_document(
            &ui,
            &mut shell,
            id,
            "replace_paragraphs",
            &args,
            Some(&shown),
        );
        assert!(done.is_ok(), "{done:?}");
        assert_eq!(text(&mut shell), "Hello\nWorld");
        assert!(modified(&mut shell));
        // The same approval cannot be used again: the document is another.
        let again = run_on_document(
            &ui,
            &mut shell,
            id,
            "replace_paragraphs",
            &args,
            Some(&shown),
        );
        assert_eq!(again, Err(aster_tools::CHANGED_WHILE_ASKING.to_owned()));
        // The user undoes what the assistant did, in one step.
        match shell.contents.get_mut(&id) {
            Some(Content::Doc(tab)) => {
                assert!(tab.view.undo());
            }
            _ => panic!("a Word tab"),
        }
        assert_eq!(text(&mut shell), "");
        // A call that is wrong changes nothing and says why.
        let wrong = run_on_document(
            &ui,
            &mut shell,
            id,
            "delete_blocks",
            &json!({ "document": id.0.to_string(), "block": 4 }),
            None,
        );
        assert!(wrong.unwrap_err().contains("block 4 is outside"));
        // The Markdown tools do not work on it, and the error points the way.
        let markdown = run_on_document(
            &ui,
            &mut shell,
            id,
            "replace_lines",
            &json!({ "document": id.0.to_string(), "start_line": 1, "end_line": 1, "text": "x" }),
            None,
        );
        assert!(markdown.unwrap_err().contains("replace_paragraphs"));
    }

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
