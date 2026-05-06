#![cfg(not(target_family = "wasm"))]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use futures_util::{SinkExt as _, StreamExt as _};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use settings::Setting as _;
use warpui::{r#async::Timer, Entity, ModelContext, SingletonEntity};
use websocket::{Message, WebSocket, WebsocketMessage as _};

use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::settings::{
    CodexAppServerSettings, CodexAppServerSettingsChangedEvent, DEFAULT_CODEX_APP_SERVER_URL,
};

const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_millis(500),
    Duration::from_secs(1),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAppServerStatus {
    Disabled,
    Disconnected { message: String },
    Connected,
    Loading,
}

impl CodexAppServerStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Disabled => "Disabled".to_string(),
            Self::Disconnected { message } => format!("Disconnected: {message}"),
            Self::Connected => "Connected".to_string(),
            Self::Loading => "Loading".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexThreadSummary {
    pub id: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub updated_at: Option<String>,
    pub source_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexThreadDetail {
    pub summary: CodexThreadSummary,
    pub items: Vec<CodexConversationItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConversationItem {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexPendingApproval {
    request_id: JsonRpcId,
    pub method: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    pub reason: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub available_decisions: Vec<CodexApprovalDecision>,
}

impl CodexPendingApproval {
    fn as_conversation_item(&self) -> CodexConversationItem {
        let command = self
            .command
            .as_ref()
            .map(|command| format!("\nCommand: {command}"))
            .unwrap_or_default();
        let cwd = self
            .cwd
            .as_ref()
            .map(|cwd| format!("\nWorking directory: {cwd}"))
            .unwrap_or_default();

        CodexConversationItem {
            role: "approval".to_string(),
            text: format!("{}{command}{cwd}", self.reason),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodexApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[allow(dead_code)]
impl CodexApprovalDecision {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accept => "Accept",
            Self::AcceptForSession => "Accept for session",
            Self::Decline => "Decline",
            Self::Cancel => "Cancel",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "accept" => Some(Self::Accept),
            "acceptForSession" => Some(Self::AcceptForSession),
            "decline" => Some(Self::Decline),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    fn result(self) -> Value {
        match self {
            Self::Accept => json!("accept"),
            Self::AcceptForSession => json!("acceptForSession"),
            Self::Decline => json!("decline"),
            Self::Cancel => json!("cancel"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAppServerModelEvent {
    StatusChanged,
    ThreadsChanged,
    ActiveThreadChanged,
    OpenConversation {
        conversation_id: AIConversationId,
        thread_id: String,
    },
}

pub struct CodexAppServerModel {
    status: CodexAppServerStatus,
    threads: Vec<CodexThreadSummary>,
    active_thread: Option<CodexThreadDetail>,
    active_turn: Option<CodexActiveTurn>,
    project_roots: Vec<PathBuf>,
    opening_thread_id: Option<String>,
    conversation_id_by_thread_id: HashMap<String, AIConversationId>,
    thread_id_by_conversation_id: HashMap<AIConversationId, String>,
}

#[allow(dead_code)]
struct CodexActiveTurn {
    thread_id: String,
    client: JsonRpcSocket,
    pending_approval: CodexPendingApproval,
}

struct CodexTurnProgress {
    items: Vec<CodexConversationItem>,
    active_turn: Option<CodexActiveTurn>,
}

impl CodexAppServerModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(
            &CodexAppServerSettings::handle(ctx),
            |model, event, ctx| match event {
                CodexAppServerSettingsChangedEvent::CodexAppServerEnabled { .. }
                | CodexAppServerSettingsChangedEvent::CodexAppServerUrl { .. }
                | CodexAppServerSettingsChangedEvent::CodexImportedProjectPaths { .. }
                | CodexAppServerSettingsChangedEvent::CodexImportedThreadIds { .. }
                | CodexAppServerSettingsChangedEvent::CodexAppServerBearerToken { .. } => {
                    model.refresh(ctx);
                }
            },
        );

        let mut model = Self {
            status: CodexAppServerStatus::Disabled,
            threads: Vec::new(),
            active_thread: None,
            active_turn: None,
            project_roots: Vec::new(),
            opening_thread_id: None,
            conversation_id_by_thread_id: HashMap::new(),
            thread_id_by_conversation_id: HashMap::new(),
        };
        model.refresh(ctx);
        model
    }

    pub fn status(&self) -> &CodexAppServerStatus {
        &self.status
    }

    pub fn threads(&self) -> &[CodexThreadSummary] {
        &self.threads
    }

    pub fn active_thread(&self) -> Option<&CodexThreadDetail> {
        self.active_thread.as_ref()
    }

    #[allow(dead_code)]
    pub fn pending_approval(&self) -> Option<&CodexPendingApproval> {
        self.active_turn
            .as_ref()
            .map(|active_turn| &active_turn.pending_approval)
    }

    pub fn opening_thread_id(&self) -> Option<&str> {
        self.opening_thread_id.as_deref()
    }

    #[allow(dead_code)]
    pub fn conversation_id_for_thread(&self, thread_id: &str) -> Option<AIConversationId> {
        self.conversation_id_by_thread_id.get(thread_id).copied()
    }

    #[allow(dead_code)]
    pub fn thread_id_for_conversation(
        &self,
        conversation_id: AIConversationId,
    ) -> Option<&str> {
        self.thread_id_by_conversation_id
            .get(&conversation_id)
            .map(String::as_str)
    }

    pub fn is_codex_conversation(&self, conversation_id: AIConversationId) -> bool {
        self.thread_id_by_conversation_id
            .contains_key(&conversation_id)
    }

    pub fn set_project_roots(&mut self, roots: Vec<PathBuf>, ctx: &mut ModelContext<Self>) {
        let mut seen = BTreeSet::new();
        let roots = roots
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect::<Vec<_>>();
        if self.project_roots == roots {
            return;
        }
        self.project_roots = roots;
        self.refresh(ctx);
    }

    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        let settings = CodexAppServerSettings::as_ref(ctx);
        if !*settings.enabled {
            self.status = CodexAppServerStatus::Disabled;
            self.threads.clear();
            self.active_thread = None;
            self.active_turn = None;
            self.opening_thread_id = None;
            self.conversation_id_by_thread_id.clear();
            self.thread_id_by_conversation_id.clear();
            ctx.emit(CodexAppServerModelEvent::StatusChanged);
            ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
            ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
            return;
        }

        let config = match CodexAppServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = CodexAppServerStatus::Disconnected {
                    message: error.to_string(),
                };
                self.threads.clear();
                self.active_thread = None;
                self.active_turn = None;
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
                ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                return;
            }
        };
        let project_roots = self.project_roots.clone();
        let imported_project_paths = settings.imported_project_paths.value().clone();
        let imported_thread_ids = settings.imported_thread_ids.value().clone();

        self.status = CodexAppServerStatus::Loading;
        ctx.emit(CodexAppServerModelEvent::StatusChanged);

        let _ = ctx.spawn(
            async move {
                list_project_threads_with_backoff(
                    &config,
                    project_roots,
                    imported_project_paths,
                    imported_thread_ids,
                )
                .await
            },
            |model, result, ctx| {
                match result {
                    Ok(threads) => {
                        model.status = CodexAppServerStatus::Connected;
                        model.threads = threads;
                        ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                    }
                    Err(error) => {
                        model.status = CodexAppServerStatus::Disconnected {
                            message: format!("{error:#}"),
                        };
                        model.threads.clear();
                        model.active_thread = None;
                        model.active_turn = None;
                        ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                    }
                }
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
            },
        );
    }

    #[allow(dead_code)]
    pub fn open_thread(&mut self, thread_id: String, ctx: &mut ModelContext<Self>) {
        let settings = CodexAppServerSettings::as_ref(ctx);
        if !*settings.enabled {
            return;
        }
        let config = match CodexAppServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = CodexAppServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
                return;
            }
        };
        let fallback_summary = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .cloned();
        self.active_turn = None;

        let _ = ctx.spawn(
            async move { read_thread(&config, &thread_id, fallback_summary).await },
            |model, result, ctx| match result {
                Ok(detail) => {
                    model.active_thread = Some(detail);
                    ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                }
                Err(error) => {
                    model.status = CodexAppServerStatus::Disconnected {
                        message: format!("{error:#}"),
                    };
                    ctx.emit(CodexAppServerModelEvent::StatusChanged);
                }
            },
        );
    }

    pub fn open_thread_as_conversation(
        &mut self,
        thread_id: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let settings = CodexAppServerSettings::as_ref(ctx);
        if !*settings.enabled {
            return;
        }
        let config = match CodexAppServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = CodexAppServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
                return;
            }
        };
        let fallback_summary = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .cloned();
        let conversation_id = self
            .conversation_id_by_thread_id
            .entry(thread_id.clone())
            .or_insert_with(AIConversationId::new)
            .to_owned();
        self.thread_id_by_conversation_id
            .insert(conversation_id, thread_id.clone());
        self.opening_thread_id = Some(thread_id.clone());
        self.active_thread = None;
        self.active_turn = None;
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);

        let _ = ctx.spawn(
            async move { read_thread(&config, &thread_id, fallback_summary).await },
            move |model, result, ctx| {
                model.opening_thread_id = None;
                match result {
                    Ok(detail) => {
                        let thread_id = detail.summary.id.clone();
                        let conversation =
                            codex_thread_detail_to_ai_conversation(conversation_id, &detail);
                        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, _ctx| {
                            history.cache_external_conversation(conversation);
                        });
                        model.active_thread = Some(detail);
                        model
                            .conversation_id_by_thread_id
                            .insert(thread_id.clone(), conversation_id);
                        model
                            .thread_id_by_conversation_id
                            .insert(conversation_id, thread_id.clone());
                        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                        ctx.emit(CodexAppServerModelEvent::OpenConversation {
                            conversation_id,
                            thread_id,
                        });
                    }
                    Err(error) => {
                        model.status = CodexAppServerStatus::Disconnected {
                            message: format!("{error:#}"),
                        };
                        ctx.emit(CodexAppServerModelEvent::StatusChanged);
                        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                    }
                }
            },
        );
    }

    #[allow(dead_code)]
    pub fn submit_prompt(&mut self, prompt: String, ctx: &mut ModelContext<Self>) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let Some(active_thread_id) = self
            .active_thread
            .as_ref()
            .map(|thread| thread.summary.id.clone())
        else {
            return;
        };
        if let Some(active_thread) = &mut self.active_thread {
            active_thread.items.push(CodexConversationItem {
                role: "user".to_string(),
                text: prompt.clone(),
            });
            active_thread.items.push(CodexConversationItem {
                role: "codex".to_string(),
                text: "Waiting for Codex...".to_string(),
            });
        }
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        self.active_turn = None;

        let settings = CodexAppServerSettings::as_ref(ctx);
        let config = match CodexAppServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = CodexAppServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
                return;
            }
        };

        let turn_thread_id = active_thread_id.clone();
        let _ = ctx.spawn(
            async move { continue_thread(&config, &turn_thread_id, &prompt).await },
            move |model, result, ctx| {
                model.apply_turn_progress(&active_thread_id, result, ctx);
            },
        );
    }

    pub fn submit_conversation_prompt(
        &mut self,
        conversation_id: AIConversationId,
        prompt: String,
        terminal_view_id: warpui::EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return false;
        }
        let Some(thread_id) = self
            .thread_id_by_conversation_id
            .get(&conversation_id)
            .cloned()
        else {
            log::warn!("No Codex thread mapping for conversation {conversation_id:?}");
            return false;
        };

        let settings = CodexAppServerSettings::as_ref(ctx);
        let config = match CodexAppServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = CodexAppServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
                return false;
            }
        };

        let working_directory = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .and_then(|thread| thread.cwd.as_ref())
            .and_then(|cwd| cwd.to_str())
            .map(ToOwned::to_owned);
        let exchange_id = match BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
            history.append_codex_exchange(
                conversation_id,
                Some(prompt.clone()),
                None,
                working_directory,
                true,
                terminal_view_id,
                ctx,
            )
        }) {
            Ok(exchange_id) => exchange_id,
            Err(error) => {
                log::error!("Could not append Codex exchange: {error:?}");
                return false;
            }
        };

        self.active_turn = None;
        let callback_thread_id = thread_id.clone();
        let _ = ctx.spawn(
            async move { continue_thread(&config, &thread_id, &prompt).await },
            move |model, result, ctx| {
                let (output_text, is_finished, is_error, active_turn) = match result {
                    Ok(progress) => {
                        let output_text = codex_items_to_agent_text(&progress.items);
                        (
                            output_text,
                            progress.active_turn.is_none(),
                            false,
                            progress.active_turn,
                        )
                    }
                    Err(error) => (
                        format!("Codex app-server error: {error:#}"),
                        true,
                        true,
                        None,
                    ),
                };

                model.active_turn = active_turn;
                let update_result =
                    BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                        history.update_codex_exchange_output(
                            conversation_id,
                            exchange_id,
                            output_text.clone(),
                            is_finished,
                            is_error,
                            terminal_view_id,
                            ctx,
                        )
                    });
                if let Err(error) = update_result {
                    log::error!("Could not update Codex exchange output: {error:?}");
                }
                if let Some(active_thread) = &mut model.active_thread {
                    if active_thread.summary.id == callback_thread_id {
                        active_thread.items.push(CodexConversationItem {
                            role: if is_error { "error" } else { "codex" }.to_string(),
                            text: output_text,
                        });
                        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                    }
                }
            },
        );
        true
    }

    #[allow(dead_code)]
    pub fn resolve_pending_approval(
        &mut self,
        decision: CodexApprovalDecision,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(active_turn) = self.active_turn.take() else {
            return;
        };
        let thread_id = active_turn.thread_id.clone();
        self.append_items(
            &thread_id,
            vec![CodexConversationItem {
                role: "approval".to_string(),
                text: format!("{}.", decision.label()),
            }],
        );
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        let callback_thread_id = thread_id.clone();

        let _ = ctx.spawn(
            async move {
                let mut client = active_turn.client;
                client
                    .respond(active_turn.pending_approval.request_id, decision.result())
                    .await?;
                client.collect_turn_progress(thread_id).await
            },
            move |model, result, ctx| {
                model.apply_turn_progress(&callback_thread_id, result, ctx);
            },
        );
    }

    #[allow(dead_code)]
    fn apply_turn_progress(
        &mut self,
        active_thread_id: &str,
        result: Result<CodexTurnProgress>,
        ctx: &mut ModelContext<Self>,
    ) {
        let (items, active_turn) = match result {
            Ok(progress) if !progress.items.is_empty() || progress.active_turn.is_some() => {
                (progress.items, progress.active_turn)
            }
            Ok(_) => (vec![], None),
            Err(error) => (
                vec![CodexConversationItem {
                    role: "error".to_string(),
                    text: format!("{error:#}"),
                }],
                None,
            ),
        };

        self.active_turn = active_turn;
        self.append_items(active_thread_id, items);
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
    }

    #[allow(dead_code)]
    fn append_items(&mut self, active_thread_id: &str, items: Vec<CodexConversationItem>) {
        if let Some(active_thread) = &mut self.active_thread {
            if active_thread.summary.id == active_thread_id {
                if active_thread
                    .items
                    .last()
                    .is_some_and(|last| last.text == "Waiting for Codex...")
                {
                    active_thread.items.pop();
                }
                active_thread.items.extend(items);
            }
        }
    }
}

impl Entity for CodexAppServerModel {
    type Event = CodexAppServerModelEvent;
}

impl SingletonEntity for CodexAppServerModel {}

#[derive(Debug, Clone)]
pub struct CodexAppServerConfig {
    pub server_url: Url,
    pub bearer_token: Option<String>,
}

impl CodexAppServerConfig {
    fn from_settings(settings: &CodexAppServerSettings) -> Result<Self> {
        let server_url = normalize_codex_app_server_url(settings.server_url.value())?;
        let bearer_token = settings
            .bearer_token
            .value()
            .trim()
            .is_empty()
            .then_some(None)
            .unwrap_or_else(|| Some(settings.bearer_token.value().trim().to_string()));
        if !is_loopback_url(&server_url) && bearer_token.is_none() {
            bail!("Non-loopback app-server URLs require a bearer token.");
        }
        Ok(Self {
            server_url,
            bearer_token,
        })
    }
}

pub fn normalize_codex_app_server_url(input: &str) -> Result<Url> {
    let trimmed = input.trim();
    let value = if trimmed.is_empty() {
        DEFAULT_CODEX_APP_SERVER_URL.to_string()
    } else if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("ws://{trimmed}")
    };

    let mut url = Url::parse(&value).context("Codex app-server URL is invalid")?;
    match url.scheme() {
        "ws" | "wss" => {}
        "http" => {
            url.set_scheme("ws")
                .map_err(|_| anyhow!("Could not normalize http URL to ws"))?;
        }
        "https" => {
            url.set_scheme("wss")
                .map_err(|_| anyhow!("Could not normalize https URL to wss"))?;
        }
        scheme => bail!(
            "Codex app-server URL must use ws://, wss://, http://, or https://, not {scheme}://"
        ),
    }
    if url.host_str().is_none() {
        bail!("Codex app-server URL must include a host.");
    }
    if url.port().is_none() {
        url.set_port(Some(4500))
            .map_err(|_| anyhow!("Could not set default Codex app-server port"))?;
    }
    if url.path() == "/" {
        url.set_path("");
    }
    Ok(url)
}

pub fn health_url_for_app_server(server_url: &Url) -> Result<Url> {
    let mut url = server_url.clone();
    match url.scheme() {
        "ws" => {
            url.set_scheme("http")
                .map_err(|_| anyhow!("Could not derive health URL"))?;
        }
        "wss" => {
            url.set_scheme("https")
                .map_err(|_| anyhow!("Could not derive health URL"))?;
        }
        scheme => bail!("Unsupported Codex app-server scheme: {scheme}"),
    }
    url.set_path("/healthz");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub fn is_loopback_url(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|addr| addr.is_loopback()),
        None => false,
    }
}

pub fn codex_start_command(url: &str) -> String {
    match normalize_codex_app_server_url(url) {
        Ok(url) => {
            let host = url.host_str().unwrap_or("127.0.0.1");
            let port = url.port_or_known_default().unwrap_or(4500);
            format!("codex app-server --listen {host}:{port}")
        }
        Err(_) => "codex app-server --listen 127.0.0.1:4500".to_string(),
    }
}

pub fn codex_thread_updated_at_utc(thread: &CodexThreadSummary) -> Option<DateTime<Utc>> {
    thread
        .updated_at
        .as_deref()
        .and_then(parse_codex_timestamp)
        .map(|datetime| datetime.with_timezone(&Utc))
}

async fn health_check(config: &CodexAppServerConfig) -> Result<()> {
    let url = health_url_for_app_server(&config.server_url)?;
    let client = reqwest::Client::new();
    let mut request = client.get(url).timeout(HEALTH_TIMEOUT);
    if let Some(token) = &config.bearer_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.context("health request failed")?;
    if !response.status().is_success() {
        bail!("health request failed with HTTP {}", response.status());
    }
    Ok(())
}

async fn list_project_threads_with_backoff(
    config: &CodexAppServerConfig,
    project_roots: Vec<PathBuf>,
    imported_project_paths: Vec<PathBuf>,
    imported_thread_ids: Vec<String>,
) -> Result<Vec<CodexThreadSummary>> {
    let mut last_error = None;
    for (index, delay) in RECONNECT_BACKOFF.into_iter().enumerate() {
        if index > 0 {
            Timer::after(delay).await;
        }
        match async {
            health_check(config).await?;
            list_project_threads(
                config,
                project_roots.clone(),
                imported_project_paths.clone(),
                imported_thread_ids.clone(),
            )
            .await
        }
        .await
        {
            Ok(threads) => return Ok(threads),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("Codex app-server did not respond")))
}

async fn list_project_threads(
    config: &CodexAppServerConfig,
    project_roots: Vec<PathBuf>,
    imported_project_paths: Vec<PathBuf>,
    imported_thread_ids: Vec<String>,
) -> Result<Vec<CodexThreadSummary>> {
    let roots = dedupe_project_roots(project_roots, imported_project_paths);

    let mut by_id: BTreeMap<String, CodexThreadSummary> = BTreeMap::new();
    for root in roots {
        let mut cursor = Value::Null;
        loop {
            let result = json_rpc_request(
                config,
                "thread/list",
                json!({
                    "cursor": cursor,
                    "limit": 50,
                    "cwd": root,
                    "sortKey": "updated_at",
                    "archived": false,
                    "sourceKinds": ["cli", "vscode", "appServer"],
                }),
            )
            .await?;
            for mut thread in parse_thread_list(&result) {
                if thread.cwd.is_none() {
                    thread.cwd = Some(root.clone());
                }
                by_id.insert(thread.id.clone(), thread);
            }
            cursor = result.get("nextCursor").cloned().unwrap_or(Value::Null);
            if cursor.is_null() {
                break;
            }
        }
    }

    for thread_id in imported_thread_ids {
        if thread_id.trim().is_empty() || by_id.contains_key(thread_id.trim()) {
            continue;
        }
        if let Ok(detail) = read_thread(config, thread_id.trim(), None).await {
            by_id.insert(detail.summary.id.clone(), detail.summary);
        }
    }

    let mut threads = by_id.into_values().collect::<Vec<_>>();
    threads.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.title.cmp(&b.title))
    });
    Ok(threads)
}

fn dedupe_project_roots(
    mut project_roots: Vec<PathBuf>,
    imported_project_paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    project_roots.extend(imported_project_paths);
    let mut seen_roots = BTreeSet::new();
    project_roots.retain(|root| seen_roots.insert(root.clone()));
    project_roots
}

async fn read_thread(
    config: &CodexAppServerConfig,
    thread_id: &str,
    fallback_summary: Option<CodexThreadSummary>,
) -> Result<CodexThreadDetail> {
    let result = json_rpc_request(
        config,
        "thread/read",
        json!({
            "threadId": thread_id,
            "includeTurns": true,
        }),
    )
    .await?;
    Ok(parse_thread_detail(&result, fallback_summary, thread_id))
}

async fn continue_thread(
    config: &CodexAppServerConfig,
    thread_id: &str,
    prompt: &str,
) -> Result<CodexTurnProgress> {
    let mut client = JsonRpcSocket::connect(config).await?;
    client.initialize().await?;
    let _ = client
        .request(
            "thread/resume",
            json!({
                "threadId": thread_id,
            }),
        )
        .await?;
    let _ = client
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [
                    {
                        "type": "text",
                        "text": prompt,
                    }
                ],
            }),
        )
        .await?;

    client.collect_turn_progress(thread_id.to_string()).await
}

async fn json_rpc_request(
    config: &CodexAppServerConfig,
    method: &str,
    params: Value,
) -> Result<Value> {
    let mut client = JsonRpcSocket::connect(config).await?;
    client.initialize().await?;
    client.request(method, params).await
}

fn codex_thread_detail_to_ai_conversation(
    conversation_id: AIConversationId,
    detail: &CodexThreadDetail,
) -> AIConversation {
    let mut conversation = AIConversation::new_with_id(conversation_id, false);
    conversation.set_fallback_display_title(detail.summary.title.clone());
    conversation.set_exclude_from_navigation(true);

    let working_directory = detail
        .summary
        .cwd
        .as_ref()
        .and_then(|cwd| cwd.to_str())
        .map(ToOwned::to_owned);
    let start_time = detail
        .summary
        .updated_at
        .as_deref()
        .and_then(parse_codex_timestamp)
        .unwrap_or_else(Local::now);

    let mut current_query = None;
    let mut output_items = Vec::new();
    let mut appended_any = false;
    for item in &detail.items {
        if is_codex_user_item(&item.role) {
            if current_query.is_some() || !output_items.is_empty() {
                append_codex_restored_exchange(
                    &mut conversation,
                    current_query.take(),
                    &output_items,
                    working_directory.clone(),
                    start_time,
                );
                output_items.clear();
                appended_any = true;
            }
            current_query = Some(item.text.clone());
        } else {
            output_items.push(item.clone());
        }
    }

    if current_query.is_some() || !output_items.is_empty() {
        append_codex_restored_exchange(
            &mut conversation,
            current_query,
            &output_items,
            working_directory,
            start_time,
        );
        appended_any = true;
    }

    if !appended_any {
        let _ = conversation.append_codex_exchange(
            None,
            Some("No messages were returned for this Codex conversation.".to_string()),
            None,
            false,
            start_time,
        );
    }

    conversation
}

fn append_codex_restored_exchange(
    conversation: &mut AIConversation,
    query: Option<String>,
    output_items: &[CodexConversationItem],
    working_directory: Option<String>,
    start_time: DateTime<Local>,
) {
    let output = codex_items_to_agent_text(output_items);
    let _ = conversation.append_codex_exchange(
        query,
        (!output.trim().is_empty()).then_some(output),
        working_directory,
        false,
        start_time,
    );
}

fn codex_items_to_agent_text(items: &[CodexConversationItem]) -> String {
    let mut parts = Vec::new();
    let mut assistant_delta_buffer = String::new();

    for item in items.iter().filter(|item| !is_codex_user_item(&item.role)) {
        if item.text.trim().is_empty() {
            continue;
        }

        if is_codex_assistant_delta_item(&item.role) {
            assistant_delta_buffer.push_str(&item.text);
            continue;
        }

        if is_codex_completed_item(&item.role) && !assistant_delta_buffer.trim().is_empty() {
            if normalized_codex_text(&assistant_delta_buffer) == normalized_codex_text(&item.text) {
                continue;
            }
        }

        flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);

        if is_codex_assistant_item(&item.role) || is_codex_completed_item(&item.role) {
            parts.push(item.text.clone());
        } else {
            parts.push(format!("{}: {}", item.role, item.text));
        }
    }

    flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
    parts.join("\n\n")
}

fn flush_codex_assistant_delta_buffer(parts: &mut Vec<String>, buffer: &mut String) {
    if !buffer.trim().is_empty() {
        parts.push(buffer.clone());
    }
    buffer.clear();
}

fn normalized_codex_text(text: &str) -> String {
    text.split_whitespace().collect::<String>()
}

fn is_codex_user_item(role: &str) -> bool {
    let role = role.to_ascii_lowercase();
    role == "user" || role == "usermessage" || role.ends_with("/usermessage")
}

fn is_codex_assistant_item(role: &str) -> bool {
    let role = role.to_ascii_lowercase();
    role == "assistant"
        || role == "agent"
        || role == "codex"
        || role == "agentmessage"
        || role == "item/completed"
        || role.ends_with("/completed")
        || role.ends_with("/agentmessage/delta")
        || role.ends_with("/agentmessage")
}

fn is_codex_assistant_delta_item(role: &str) -> bool {
    let role = role.to_ascii_lowercase();
    role == "delta" || role.ends_with("/delta") || role.ends_with("/agentmessage/delta")
}

fn is_codex_completed_item(role: &str) -> bool {
    let role = role.to_ascii_lowercase();
    role == "item/completed" || role.ends_with("/completed")
}

fn parse_codex_timestamp(value: &str) -> Option<DateTime<Local>> {
    if let Ok(seconds) = value.parse::<i64>() {
        return Utc
            .timestamp_opt(seconds, 0)
            .single()
            .map(DateTime::<Local>::from);
    }
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.with_timezone(&Local))
}

struct JsonRpcSocket {
    sink: Box<dyn websocket::Sink>,
    stream: Box<dyn websocket::Stream>,
    next_id: u64,
}

impl JsonRpcSocket {
    async fn connect(config: &CodexAppServerConfig) -> Result<Self> {
        #[cfg(not(target_family = "wasm"))]
        let socket = {
            use websocket::IntoClientRequest as _;

            let mut request = config.server_url.as_str().into_client_request()?;
            if let Some(token) = &config.bearer_token {
                let value = format!("Bearer {token}");
                request.headers_mut().insert(
                    "Authorization",
                    websocket::tungstenite::http::HeaderValue::from_str(&value)?,
                );
            }
            WebSocket::connect(request, None).await?
        };

        let (sink, stream) = socket.split().await;
        Ok(Self {
            sink: Box::new(sink),
            stream: Box::new(stream),
            next_id: 1,
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "Slipstream",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {},
                }),
            )
            .await?;
        self.notify("initialized", json!({})).await?;
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "method": method,
            "params": params,
        });
        self.sink
            .send(Message::new_text(payload.to_string()))
            .await
            .map_err(|error| anyhow!("{error}"))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        self.sink
            .send(Message::new_text(payload.to_string()))
            .await
            .map_err(|error| anyhow!("{error}"))?;
        self.wait_for_response(JsonRpcId::Number(id), method, &payload)
            .await
    }

    async fn wait_for_response(
        &mut self,
        id: JsonRpcId,
        method: &str,
        request_payload: &Value,
    ) -> Result<Value> {
        while let Some(message) = self.stream.next().await {
            let message = message.map_err(|error| anyhow!("{error}"))?;
            let Some(text) = message.text() else {
                continue;
            };
            let payload: JsonRpcIncoming = serde_json::from_str(text)
                .with_context(|| format!("invalid JSON-RPC message: {text}"))?;
            if payload.id.as_ref() == Some(&id) {
                if let Some(error) = payload.error {
                    log::warn!(
                        "Codex app-server JSON-RPC request failed: method={method}, id={id:?}, code={}, message={}, request={request_payload}",
                        error.code,
                        error.message
                    );
                    bail!(
                        "Codex app-server {method} failed ({}): {}",
                        error.code,
                        error.message
                    );
                }
                return Ok(payload.result.unwrap_or(Value::Null));
            }
        }
        bail!("Codex app-server closed the connection before responding to {method}");
    }

    #[allow(dead_code)]
    async fn respond(&mut self, id: JsonRpcId, result: Value) -> Result<()> {
        let payload = json!({
            "id": id,
            "result": result,
        });
        self.sink
            .send(Message::new_text(payload.to_string()))
            .await
            .map_err(|error| anyhow!("{error}"))
    }

    async fn collect_turn_progress(mut self, thread_id: String) -> Result<CodexTurnProgress> {
        let mut items = Vec::new();
        loop {
            let next = futures_util::future::select(
                Box::pin(self.stream.next()),
                Box::pin(Timer::after(Duration::from_secs(300))),
            )
            .await;
            let message = match next {
                futures_util::future::Either::Left((message, _)) => message,
                futures_util::future::Either::Right((_, _)) => break,
            };
            let Some(message) = message else {
                break;
            };
            let message = message.map_err(|error| anyhow!("{error}"))?;
            let Some(text) = message.text() else {
                continue;
            };
            if let Ok(payload) = serde_json::from_str::<JsonRpcIncoming>(text) {
                let is_terminal = is_terminal_method(payload.method.as_deref());
                if let Some(approval) = parse_approval_request(
                    payload.method.as_deref(),
                    payload.id.clone(),
                    payload.params.as_ref(),
                ) {
                    items.push(approval.as_conversation_item());
                    return Ok(CodexTurnProgress {
                        items,
                        active_turn: Some(CodexActiveTurn {
                            thread_id,
                            client: self,
                            pending_approval: approval,
                        }),
                    });
                }
                if let Some(params) = payload.params {
                    if is_terminal {
                        items.extend(parse_conversation_items(&params));
                        break;
                    } else if let Some(item) =
                        parse_notification_item(payload.method.as_deref(), &params)
                    {
                        let is_terminal =
                            is_terminal_notification(payload.method.as_deref(), &item);
                        items.push(item);
                        if is_terminal {
                            break;
                        }
                    }
                } else if is_terminal {
                    break;
                }
            }
        }
        Ok(CodexTurnProgress {
            items,
            active_turn: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum JsonRpcId {
    Number(u64),
    String(String),
}

#[derive(Debug, Deserialize)]
struct JsonRpcIncoming {
    id: Option<JsonRpcId>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

fn parse_thread_list(result: &Value) -> Vec<CodexThreadSummary> {
    let values = result
        .get("threads")
        .or_else(|| result.get("items"))
        .or_else(|| result.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| result.as_array().cloned().unwrap_or_default());
    values.iter().filter_map(parse_thread_summary).collect()
}

fn parse_thread_detail(
    result: &Value,
    fallback_summary: Option<CodexThreadSummary>,
    fallback_id: &str,
) -> CodexThreadDetail {
    let parsed_summary = result
        .get("thread")
        .and_then(parse_thread_summary)
        .or_else(|| parse_thread_summary(result));
    let summary = match (parsed_summary, fallback_summary) {
        (Some(mut summary), Some(fallback)) => {
            if summary.cwd.is_none() {
                summary.cwd = fallback.cwd;
            }
            if summary.updated_at.is_none() {
                summary.updated_at = fallback.updated_at;
            }
            if summary.source_kind.is_none() {
                summary.source_kind = fallback.source_kind;
            }
            if summary.title == summary.id && fallback.title != fallback.id {
                summary.title = fallback.title;
            }
            summary
        }
        (Some(summary), None) => summary,
        (None, Some(summary)) => summary,
        (None, None) => CodexThreadSummary {
            id: fallback_id.to_string(),
            title: fallback_id.to_string(),
            cwd: None,
            updated_at: None,
            source_kind: None,
        },
    };
    let items = parse_conversation_items(result);
    CodexThreadDetail { summary, items }
}

fn parse_thread_summary(value: &Value) -> Option<CodexThreadSummary> {
    let id = string_field(value, &["id", "threadId", "thread_id", "conversationId"])?;
    let title = string_field(value, &["title", "name", "summary", "preview"])
        .or_else(|| string_field(value, &["initialPrompt", "initial_prompt"]))
        .unwrap_or_else(|| id.clone());
    let cwd =
        string_field(value, &["cwd", "workingDirectory", "working_directory"]).map(PathBuf::from);
    let updated_at = string_field(value, &["updatedAt", "updated_at", "lastUpdatedAt"]);
    let source_kind = string_field(value, &["sourceKind", "source_kind"]);
    Some(CodexThreadSummary {
        id,
        title,
        cwd,
        updated_at,
        source_kind,
    })
}

fn parse_conversation_items(value: &Value) -> Vec<CodexConversationItem> {
    let mut items = Vec::new();
    collect_items(value, &mut items);
    items
}

fn collect_items(value: &Value, items: &mut Vec<CodexConversationItem>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_items(value, items);
            }
        }
        Value::Object(map) => {
            if let Some(role) = string_field(value, &["role", "type", "kind"]) {
                if let Some(content) = map.get("content") {
                    let text = text_content_from_value(content);
                    if !text.trim().is_empty() {
                        items.push(CodexConversationItem { role, text });
                        return;
                    }
                }
            }
            if let Some(text) = text_from_value(value) {
                let role = string_field(value, &["role", "type", "kind"])
                    .unwrap_or_else(|| "codex".to_string());
                items.push(CodexConversationItem { role, text });
                return;
            }
            for key in [
                "thread", "turns", "items", "messages", "events", "entries", "content",
            ] {
                if let Some(child) = map.get(key) {
                    collect_items(child, items);
                }
            }
        }
        _ => {}
    }
}

fn text_content_from_value(value: &Value) -> String {
    match value {
        Value::Array(values) => values
            .iter()
            .map(text_content_from_value)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => text_from_value(value).unwrap_or_default(),
        Value::String(value) => value.clone(),
        _ => String::new(),
    }
}

fn parse_notification_item(method: Option<&str>, params: &Value) -> Option<CodexConversationItem> {
    text_from_value(params).map(|text| CodexConversationItem {
        role: method.unwrap_or("codex").to_string(),
        text,
    })
}

fn parse_approval_request(
    method: Option<&str>,
    request_id: Option<JsonRpcId>,
    params: Option<&Value>,
) -> Option<CodexPendingApproval> {
    let method = method?;
    if !is_approval_request(method) {
        return None;
    }
    let request_id = request_id?;
    let params = params?;
    let reason = string_field(params, &["reason"]).unwrap_or_else(|| {
        if method.contains("commandExecution") {
            "Codex is requesting command approval.".to_string()
        } else if method.contains("fileChange") {
            "Codex is requesting file change approval.".to_string()
        } else {
            "Codex is requesting approval.".to_string()
        }
    });
    let available_decisions = approval_decisions(params, method);

    Some(CodexPendingApproval {
        request_id,
        method: method.to_string(),
        thread_id: string_field(params, &["threadId", "thread_id"]),
        turn_id: string_field(params, &["turnId", "turn_id"]),
        item_id: string_field(params, &["itemId", "item_id"]),
        reason,
        command: command_field(params),
        cwd: string_field(params, &["cwd"]),
        available_decisions,
    })
}

fn approval_decisions(params: &Value, method: &str) -> Vec<CodexApprovalDecision> {
    let decisions = params
        .get("availableDecisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(CodexApprovalDecision::from_wire)
        .collect::<Vec<_>>();
    if !decisions.is_empty() {
        return decisions;
    }

    if method == "item/tool/requestUserInput" {
        vec![
            CodexApprovalDecision::Accept,
            CodexApprovalDecision::Decline,
            CodexApprovalDecision::Cancel,
        ]
    } else {
        vec![
            CodexApprovalDecision::Accept,
            CodexApprovalDecision::AcceptForSession,
            CodexApprovalDecision::Decline,
            CodexApprovalDecision::Cancel,
        ]
    }
}

fn command_field(value: &Value) -> Option<String> {
    match value.get("command") {
        Some(Value::Array(parts)) => {
            let command = parts
                .iter()
                .filter_map(|part| match part {
                    Value::String(value) => Some(value.clone()),
                    Value::Number(value) => Some(value.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!command.trim().is_empty()).then_some(command)
        }
        _ => string_field(value, &["command"]),
    }
}

fn is_approval_request(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/tool/requestUserInput"
    )
}

fn is_terminal_method(method: Option<&str>) -> bool {
    matches!(method, Some("turn/completed") | Some("turn/finished"))
}

fn is_terminal_notification(method: Option<&str>, item: &CodexConversationItem) -> bool {
    is_terminal_method(method)
        || (item.role == "status" && item.text.to_ascii_lowercase().contains("turn complete"))
}

fn text_from_value(value: &Value) -> Option<String> {
    if let Some(text) = string_field(
        value,
        &[
            "text", "delta", "message", "output", "content", "plan", "command", "error",
        ],
    ) {
        return Some(text);
    }
    value
        .get("item")
        .and_then(text_from_value)
        .or_else(|| value.get("event").and_then(text_from_value))
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        match value.get(*key) {
            Some(Value::String(value)) if !value.trim().is_empty() => return Some(value.clone()),
            Some(Value::Number(value)) => return Some(value.to_string()),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_url_with_default_scheme_and_port() {
        let url = normalize_codex_app_server_url("127.0.0.1").unwrap();
        assert_eq!(url.as_str(), "ws://127.0.0.1:4500/");
    }

    #[test]
    fn normalizes_http_to_websocket() {
        let url = normalize_codex_app_server_url("http://localhost:5000/").unwrap();
        assert_eq!(url.as_str(), "ws://localhost:5000/");
    }

    #[test]
    fn derives_health_url() {
        let url = normalize_codex_app_server_url("wss://codex.example.test:9443").unwrap();
        assert_eq!(
            health_url_for_app_server(&url).unwrap().as_str(),
            "https://codex.example.test:9443/healthz"
        );
    }

    #[test]
    fn detects_loopback_hosts() {
        assert!(is_loopback_url(
            &normalize_codex_app_server_url("localhost:4500").unwrap()
        ));
        assert!(is_loopback_url(
            &normalize_codex_app_server_url("127.0.0.1:4500").unwrap()
        ));
        assert!(!is_loopback_url(
            &normalize_codex_app_server_url("codex.example.test:4500").unwrap()
        ));
    }

    #[test]
    fn parses_thread_lists_from_common_shapes() {
        let payload = json!({
            "threads": [{
                "id": "abc",
                "title": "Implement feature",
                "cwd": "/tmp/project",
                "updated_at": "2026-05-06T12:00:00Z",
                "sourceKind": "cli"
            }]
        });
        let threads = parse_thread_list(&payload);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "abc");
        assert_eq!(
            threads[0].cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
    }

    #[test]
    fn merges_active_and_imported_project_roots_without_duplicates() {
        let roots = dedupe_project_roots(
            vec![PathBuf::from("/tmp/project"), PathBuf::from("/tmp/other")],
            vec![
                PathBuf::from("/tmp/project"),
                PathBuf::from("/tmp/imported"),
            ],
        );
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/tmp/project"),
                PathBuf::from("/tmp/other"),
                PathBuf::from("/tmp/imported"),
            ]
        );
    }

    #[test]
    fn parses_command_approval_requests() {
        let payload = json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "item-1",
            "reason": "Run tests?",
            "command": ["cargo", "test", "-p", "warp"],
            "cwd": "/tmp/project",
            "availableDecisions": ["accept", "decline", "cancel"]
        });
        let approval = parse_approval_request(
            Some("item/commandExecution/requestApproval"),
            Some(JsonRpcId::Number(99)),
            Some(&payload),
        )
        .unwrap();

        assert_eq!(approval.reason, "Run tests?");
        assert_eq!(approval.command.as_deref(), Some("cargo test -p warp"));
        assert_eq!(
            approval.available_decisions,
            vec![
                CodexApprovalDecision::Accept,
                CodexApprovalDecision::Decline,
                CodexApprovalDecision::Cancel,
            ]
        );
    }

    #[test]
    fn parses_codex_user_message_content_as_user_item() {
        let payload = json!({
            "thread": {
                "id": "thread-1",
                "turns": [{
                    "items": [{
                        "type": "userMessage",
                        "content": [{ "type": "text", "text": "hello codex" }]
                    }]
                }]
            }
        });

        let detail = parse_thread_detail(&payload, None, "thread-1");
        assert_eq!(
            detail.items,
            vec![CodexConversationItem {
                role: "userMessage".to_string(),
                text: "hello codex".to_string(),
            }]
        );
    }

    #[test]
    fn streamed_agent_deltas_are_joined_and_completed_duplicate_is_skipped() {
        let text = codex_items_to_agent_text(&[
            CodexConversationItem {
                role: "item/agentMessage/delta".to_string(),
                text: "You".to_string(),
            },
            CodexConversationItem {
                role: "item/agentMessage/delta".to_string(),
                text: "’".to_string(),
            },
            CodexConversationItem {
                role: "item/agentMessage/delta".to_string(),
                text: "re welcome.".to_string(),
            },
            CodexConversationItem {
                role: "item/completed".to_string(),
                text: "You’re welcome.".to_string(),
            },
        ]);

        assert_eq!(text, "You’re welcome.");
    }

    #[test]
    fn completed_item_without_deltas_renders_as_assistant_text() {
        let text = codex_items_to_agent_text(&[CodexConversationItem {
            role: "item/completed".to_string(),
            text: "Done.".to_string(),
        }]);

        assert_eq!(text, "Done.");
    }

    #[test]
    fn thread_read_merges_fallback_cwd_from_project_scoped_list() {
        let payload = json!({
            "thread": {
                "id": "thread-1",
                "title": "Project thread"
            }
        });
        let fallback = CodexThreadSummary {
            id: "thread-1".to_string(),
            title: "Project thread".to_string(),
            cwd: Some(PathBuf::from("/tmp/project")),
            updated_at: Some("2026-05-06T12:00:00Z".to_string()),
            source_kind: Some("cli".to_string()),
        };

        let detail = parse_thread_detail(&payload, Some(fallback), "thread-1");

        assert_eq!(
            detail.summary.cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
        assert_eq!(
            detail.summary.updated_at.as_deref(),
            Some("2026-05-06T12:00:00Z")
        );
    }

    #[test]
    fn converts_thread_detail_to_hidden_agent_conversation() {
        let detail = CodexThreadDetail {
            summary: CodexThreadSummary {
                id: "thread-1".to_string(),
                title: "Fix Codex bridge".to_string(),
                cwd: Some(PathBuf::from("/tmp/project")),
                updated_at: Some("1730831111".to_string()),
                source_kind: Some("cli".to_string()),
            },
            items: vec![
                CodexConversationItem {
                    role: "userMessage".to_string(),
                    text: "render this thread".to_string(),
                },
                CodexConversationItem {
                    role: "agentMessage".to_string(),
                    text: "Rendered.".to_string(),
                },
            ],
        };

        let conversation_id = AIConversationId::new();
        let conversation = codex_thread_detail_to_ai_conversation(conversation_id, &detail);

        assert_eq!(conversation.id(), conversation_id);
        assert!(conversation.should_exclude_from_navigation());
        assert_eq!(conversation.title().as_deref(), Some("render this thread"));
        let exchanges = conversation.root_task_exchanges().collect::<Vec<_>>();
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].format_input_for_copy(), "render this thread");
        assert_eq!(exchanges[0].format_output_for_copy(None), "Rendered.");
    }
}
