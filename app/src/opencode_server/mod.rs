#![cfg(not(target_family = "wasm"))]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Local, TimeZone, Utc};
use reqwest::{
    header::{HeaderValue, AUTHORIZATION},
    Client, Method, Url,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use settings::Setting as _;
use warpui::{r#async::Timer, Entity, ModelContext, SingletonEntity};

use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::settings::{
    OpenCodeServerSettings, OpenCodeServerSettingsChangedEvent, DEFAULT_OPENCODE_SERVER_URL,
};

const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_millis(500),
    Duration::from_secs(1),
];
const ACTIVE_SESSION_POLL_INTERVAL: Duration = Duration::from_secs(5);
const PENDING_REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(750);
const DEFAULT_SESSION_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeServerStatus {
    Disabled,
    Disconnected { message: String },
    Connected,
    Loading,
}

impl OpenCodeServerStatus {
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
pub struct OpenCodeSessionSummary {
    pub id: String,
    pub title: String,
    pub directory: Option<PathBuf>,
    pub project_id: Option<String>,
    pub updated_at: Option<i64>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeSessionDetail {
    pub summary: OpenCodeSessionSummary,
    pub items: Vec<OpenCodeConversationItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeModelInfo {
    pub id: String,
    pub display_name: String,
    pub provider_id: String,
    pub model_id: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeConversationItem {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodePendingPermission {
    pub id: String,
    pub session_id: String,
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenCodePermissionReply {
    Once,
    Always,
    Reject,
}

impl OpenCodePermissionReply {
    pub fn label(self) -> &'static str {
        match self {
            Self::Once => "Allow once",
            Self::Always => "Always allow",
            Self::Reject => "Reject",
        }
    }

    fn wire_value(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeQuestionInfo {
    pub header: String,
    pub question: String,
    pub options: Vec<OpenCodeQuestionOption>,
    pub multiple: bool,
    pub custom: bool,
}

impl OpenCodeQuestionInfo {
    pub fn allows_custom_answer(&self) -> bool {
        self.custom || self.options.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodePendingQuestion {
    pub id: String,
    pub session_id: String,
    pub questions: Vec<OpenCodeQuestionInfo>,
}

impl OpenCodePendingQuestion {
    pub fn title(&self) -> String {
        self.single_question()
            .map(|question| question.header.clone())
            .filter(|header| !header.trim().is_empty())
            .unwrap_or_else(|| "OpenCode needs input".to_string())
    }

    pub fn message(&self) -> String {
        self.single_question()
            .map(|question| question.question.clone())
            .filter(|question| !question.trim().is_empty())
            .unwrap_or_else(|| "OpenCode is waiting for your response.".to_string())
    }

    pub fn single_question(&self) -> Option<&OpenCodeQuestionInfo> {
        if self.questions.len() == 1 {
            self.questions.first()
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeServerModelEvent {
    StatusChanged,
    SessionsChanged,
    ActiveSessionChanged,
    PendingRequestsChanged,
    ModelsChanged,
    OpenConversation {
        conversation_id: AIConversationId,
        session_id: String,
    },
}

pub struct OpenCodeServerModel {
    status: OpenCodeServerStatus,
    sessions: Vec<OpenCodeSessionSummary>,
    active_session: Option<OpenCodeSessionDetail>,
    local_turn_session_id: Option<String>,
    pending_permissions: Vec<OpenCodePendingPermission>,
    pending_questions: Vec<OpenCodePendingQuestion>,
    pending_request_poll_generation: u64,
    models: Vec<OpenCodeModelInfo>,
    selected_model_id: Option<String>,
    project_roots: Vec<PathBuf>,
    opening_session_id: Option<String>,
    active_session_poll_generation: u64,
    conversation_id_by_session_id: HashMap<String, AIConversationId>,
    session_id_by_conversation_id: HashMap<AIConversationId, String>,
}

struct OpenCodeRefreshSnapshot {
    sessions: Vec<OpenCodeSessionSummary>,
    models: Vec<OpenCodeModelInfo>,
}

struct OpenCodePromptResult {
    output_items: Vec<OpenCodeConversationItem>,
    detail: Option<OpenCodeSessionDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenCodePendingRequests {
    permissions: Vec<OpenCodePendingPermission>,
    questions: Vec<OpenCodePendingQuestion>,
}

impl OpenCodeServerModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(
            &OpenCodeServerSettings::handle(ctx),
            |model, event, ctx| match event {
                OpenCodeServerSettingsChangedEvent::OpenCodeServerEnabled { .. }
                | OpenCodeServerSettingsChangedEvent::OpenCodeServerUrl { .. }
                | OpenCodeServerSettingsChangedEvent::OpenCodeServerUsername { .. }
                | OpenCodeServerSettingsChangedEvent::OpenCodeServerPassword { .. }
                | OpenCodeServerSettingsChangedEvent::OpenCodeImportedProjectPaths { .. } => {
                    model.refresh(ctx);
                }
            },
        );

        let mut model = Self {
            status: OpenCodeServerStatus::Disabled,
            sessions: Vec::new(),
            active_session: None,
            local_turn_session_id: None,
            pending_permissions: Vec::new(),
            pending_questions: Vec::new(),
            pending_request_poll_generation: 0,
            models: Vec::new(),
            selected_model_id: None,
            project_roots: Vec::new(),
            opening_session_id: None,
            active_session_poll_generation: 0,
            conversation_id_by_session_id: HashMap::new(),
            session_id_by_conversation_id: HashMap::new(),
        };
        model.refresh(ctx);
        model
    }

    pub fn status(&self) -> &OpenCodeServerStatus {
        &self.status
    }

    pub fn sessions(&self) -> &[OpenCodeSessionSummary] {
        &self.sessions
    }

    pub fn active_session(&self) -> Option<&OpenCodeSessionDetail> {
        self.active_session.as_ref()
    }

    pub fn pending_permission_for_conversation(
        &self,
        conversation_id: AIConversationId,
    ) -> Option<&OpenCodePendingPermission> {
        let session_id = self.session_id_by_conversation_id.get(&conversation_id)?;
        self.pending_permissions
            .iter()
            .find(|permission| permission.session_id == *session_id)
    }

    pub fn pending_question_for_conversation(
        &self,
        conversation_id: AIConversationId,
    ) -> Option<&OpenCodePendingQuestion> {
        let session_id = self.session_id_by_conversation_id.get(&conversation_id)?;
        self.pending_questions
            .iter()
            .find(|question| question.session_id == *session_id)
    }

    pub fn opening_session_id(&self) -> Option<&str> {
        self.opening_session_id.as_deref()
    }

    pub fn models(&self) -> &[OpenCodeModelInfo] {
        &self.models
    }

    pub fn has_model_choices(&self) -> bool {
        !self.models.is_empty()
    }

    pub fn selected_model_id(&self) -> Option<&str> {
        self.selected_model_id.as_deref()
    }

    pub fn selected_model_display_name(&self) -> String {
        self.selected_model_id()
            .and_then(|model_id| self.models.iter().find(|model| model.id == model_id))
            .map(|model| model.display_name.clone())
            .unwrap_or_else(|| "opencode".to_string())
    }

    pub fn set_selected_model_id(
        &mut self,
        model_id: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let model_id = model_id.filter(|model_id| {
            self.models
                .iter()
                .any(|model| model.id.as_str() == model_id.as_str())
        });
        if self.selected_model_id == model_id {
            return;
        }
        self.selected_model_id = model_id;
        ctx.emit(OpenCodeServerModelEvent::ModelsChanged);
    }

    #[allow(dead_code)]
    pub fn conversation_id_for_session(&self, session_id: &str) -> Option<AIConversationId> {
        self.conversation_id_by_session_id.get(session_id).copied()
    }

    #[allow(dead_code)]
    pub fn session_id_for_conversation(&self, conversation_id: AIConversationId) -> Option<&str> {
        self.session_id_by_conversation_id
            .get(&conversation_id)
            .map(String::as_str)
    }

    pub fn is_opencode_conversation(&self, conversation_id: AIConversationId) -> bool {
        self.session_id_by_conversation_id
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
        let settings = OpenCodeServerSettings::as_ref(ctx);
        if !*settings.enabled {
            self.stop_active_session_polling();
            self.status = OpenCodeServerStatus::Disabled;
            self.sessions.clear();
            self.active_session = None;
            self.local_turn_session_id = None;
            self.pending_permissions.clear();
            self.pending_questions.clear();
            self.models.clear();
            self.selected_model_id = None;
            self.opening_session_id = None;
            self.conversation_id_by_session_id.clear();
            self.session_id_by_conversation_id.clear();
            ctx.emit(OpenCodeServerModelEvent::StatusChanged);
            ctx.emit(OpenCodeServerModelEvent::SessionsChanged);
            ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
            ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);
            ctx.emit(OpenCodeServerModelEvent::ModelsChanged);
            return;
        }

        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.stop_active_session_polling();
                self.status = OpenCodeServerStatus::Disconnected {
                    message: error.to_string(),
                };
                self.sessions.clear();
                self.active_session = None;
                self.local_turn_session_id = None;
                self.pending_permissions.clear();
                self.pending_questions.clear();
                self.models.clear();
                self.opening_session_id = None;
                ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                ctx.emit(OpenCodeServerModelEvent::SessionsChanged);
                ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);
                ctx.emit(OpenCodeServerModelEvent::ModelsChanged);
                return;
            }
        };

        let project_roots = self.project_roots.clone();
        let imported_project_paths = settings.imported_project_paths.value().clone();
        self.status = OpenCodeServerStatus::Loading;
        ctx.emit(OpenCodeServerModelEvent::StatusChanged);

        let _ = ctx.spawn(
            async move {
                list_project_sessions_with_backoff(&config, project_roots, imported_project_paths)
                    .await
            },
            |model, result, ctx| {
                match result {
                    Ok(snapshot) => {
                        model.status = OpenCodeServerStatus::Connected;
                        model.sessions = snapshot.sessions;
                        model.models = snapshot.models;
                        if model
                            .selected_model_id
                            .as_ref()
                            .is_some_and(|selected_model_id| {
                                !model
                                    .models
                                    .iter()
                                    .any(|model| model.id == *selected_model_id)
                            })
                        {
                            model.selected_model_id = None;
                        }
                        ctx.emit(OpenCodeServerModelEvent::SessionsChanged);
                        ctx.emit(OpenCodeServerModelEvent::ModelsChanged);
                    }
                    Err(error) => {
                        model.stop_active_session_polling();
                        model.status = OpenCodeServerStatus::Disconnected {
                            message: format!("{error:#}"),
                        };
                        model.sessions.clear();
                        model.active_session = None;
                        model.local_turn_session_id = None;
                        model.pending_permissions.clear();
                        model.pending_questions.clear();
                        model.models.clear();
                        ctx.emit(OpenCodeServerModelEvent::SessionsChanged);
                        ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                        ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);
                        ctx.emit(OpenCodeServerModelEvent::ModelsChanged);
                    }
                }
                ctx.emit(OpenCodeServerModelEvent::StatusChanged);
            },
        );
    }

    pub fn open_session_as_conversation(
        &mut self,
        session_id: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let settings = OpenCodeServerSettings::as_ref(ctx);
        if !*settings.enabled {
            return;
        }
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = OpenCodeServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                return;
            }
        };
        let fallback_summary = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned();
        let conversation_id = self
            .conversation_id_by_session_id
            .entry(session_id.clone())
            .or_insert_with(AIConversationId::new)
            .to_owned();
        self.session_id_by_conversation_id
            .insert(conversation_id, session_id.clone());
        self.opening_session_id = Some(session_id.clone());
        self.stop_active_session_polling();
        self.active_session = None;
        self.local_turn_session_id = None;
        ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);

        let _ = ctx.spawn(
            async move { read_session(&config, &session_id, fallback_summary).await },
            move |model, result, ctx| {
                model.opening_session_id = None;
                match result {
                    Ok(detail) => {
                        let session_id = detail.summary.id.clone();
                        model.cache_session_detail_as_conversation(conversation_id, &detail, ctx);
                        model.active_session = Some(detail);
                        model.start_active_session_polling(session_id.clone(), ctx);
                        ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                        ctx.emit(OpenCodeServerModelEvent::OpenConversation {
                            conversation_id,
                            session_id,
                        });
                    }
                    Err(error) => {
                        model.status = OpenCodeServerStatus::Disconnected {
                            message: format!("{error:#}"),
                        };
                        ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                        ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                    }
                }
            },
        );
    }

    pub fn start_new_conversation(&mut self, ctx: &mut ModelContext<Self>) {
        let settings = OpenCodeServerSettings::as_ref(ctx);
        if !*settings.enabled {
            return;
        }
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = OpenCodeServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                return;
            }
        };

        let project_roots = self.project_roots.clone();
        let imported_project_paths = settings.imported_project_paths.value().clone();
        self.stop_active_session_polling();
        self.active_session = None;
        self.local_turn_session_id = None;
        ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);

        let _ = ctx.spawn(
            async move {
                let projects = list_projects(&config).await?;
                let directories =
                    matched_project_directories(&projects, &project_roots, &imported_project_paths);
                let directory = directories
                    .into_iter()
                    .next()
                    .or_else(|| project_roots.into_iter().next())
                    .or_else(|| imported_project_paths.into_iter().next())
                    .ok_or_else(|| anyhow!("No OpenCode project directory is available"))?;
                create_session(&config, directory).await
            },
            move |model, result, ctx| match result {
                Ok(detail) => {
                    let session_id = detail.summary.id.clone();
                    let conversation_id = model
                        .conversation_id_by_session_id
                        .entry(session_id.clone())
                        .or_insert_with(AIConversationId::new)
                        .to_owned();
                    model.cache_session_detail_as_conversation(conversation_id, &detail, ctx);
                    upsert_session_summary(&mut model.sessions, detail.summary.clone());
                    model.active_session = Some(detail);
                    model.status = OpenCodeServerStatus::Connected;
                    model.start_active_session_polling(session_id.clone(), ctx);
                    ctx.emit(OpenCodeServerModelEvent::SessionsChanged);
                    ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                    ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                    ctx.emit(OpenCodeServerModelEvent::OpenConversation {
                        conversation_id,
                        session_id,
                    });
                }
                Err(error) => {
                    model.status = OpenCodeServerStatus::Disconnected {
                        message: format!("{error:#}"),
                    };
                    ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                    ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                }
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
        let Some(session_id) = self
            .session_id_by_conversation_id
            .get(&conversation_id)
            .cloned()
        else {
            log::warn!("No OpenCode session mapping for conversation {conversation_id:?}");
            return false;
        };

        let settings = OpenCodeServerSettings::as_ref(ctx);
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = OpenCodeServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                return false;
            }
        };

        let fallback_summary = self
            .active_session
            .as_ref()
            .filter(|session| session.summary.id == session_id)
            .map(|session| session.summary.clone())
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .cloned()
            });
        let working_directory = fallback_summary
            .as_ref()
            .and_then(|session| session.directory.as_ref())
            .and_then(|directory| directory.to_str())
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
                log::error!("Could not append OpenCode exchange: {error:?}");
                return false;
            }
        };

        self.local_turn_session_id = Some(session_id.clone());
        let selected_model = self.selected_model();
        let callback_session_id = session_id.clone();
        self.start_pending_request_polling(session_id.clone(), ctx);
        let _ = ctx.spawn(
            async move {
                prompt_session(
                    &config,
                    &session_id,
                    fallback_summary,
                    &prompt,
                    selected_model,
                )
                .await
            },
            move |model, result, ctx| {
                let (output_text, is_error, detail) = match result {
                    Ok(result) => {
                        let output_text = opencode_items_to_agent_text(&result.output_items);
                        (
                            if output_text.trim().is_empty() {
                                "OpenCode returned no response text.".to_string()
                            } else {
                                output_text
                            },
                            false,
                            result.detail,
                        )
                    }
                    Err(error) => (format!("OpenCode server error: {error:#}"), true, None),
                };

                model.local_turn_session_id = None;
                model.clear_pending_requests_for_session(&callback_session_id, ctx);
                if let Some(detail) = detail {
                    model.apply_polled_session_detail(detail, ctx);
                }
                model.start_active_session_polling(callback_session_id.clone(), ctx);
                let update_result =
                    BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                        history.update_codex_exchange_output(
                            conversation_id,
                            exchange_id,
                            output_text.clone(),
                            true,
                            is_error,
                            terminal_view_id,
                            ctx,
                        )
                    });
                if let Err(error) = update_result {
                    log::error!("Could not update OpenCode exchange output: {error:?}");
                }
                if let Some(active_session) = &mut model.active_session {
                    if active_session.summary.id == callback_session_id {
                        active_session.items.push(OpenCodeConversationItem {
                            role: if is_error { "error" } else { "assistant" }.to_string(),
                            text: output_text,
                        });
                        ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
                    }
                }
            },
        );
        true
    }

    pub fn resolve_pending_permission(
        &mut self,
        request_id: String,
        reply: OpenCodePermissionReply,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(permission) = self
            .pending_permissions
            .iter()
            .find(|permission| permission.id == request_id)
            .cloned()
        else {
            return;
        };
        let settings = OpenCodeServerSettings::as_ref(ctx);
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("OpenCode permission reply skipped: {error:#}");
                return;
            }
        };
        let directory = self.directory_for_session(&permission.session_id);
        self.pending_permissions
            .retain(|pending| pending.id != request_id);
        ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);

        let _ = ctx.spawn(
            async move {
                reply_permission(&config, &request_id, directory.as_deref(), reply).await
            },
            move |_, result, _| {
                if let Err(error) = result {
                    log::warn!("OpenCode permission reply failed: {error:#}");
                }
            },
        );
    }

    pub fn resolve_pending_question(
        &mut self,
        request_id: String,
        answers: Vec<Vec<String>>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(question) = self
            .pending_questions
            .iter()
            .find(|question| question.id == request_id)
            .cloned()
        else {
            return;
        };
        let settings = OpenCodeServerSettings::as_ref(ctx);
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("OpenCode question reply skipped: {error:#}");
                return;
            }
        };
        let directory = self.directory_for_session(&question.session_id);
        self.pending_questions
            .retain(|pending| pending.id != request_id);
        ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);

        let _ = ctx.spawn(
            async move { reply_question(&config, &request_id, directory.as_deref(), answers).await },
            move |_, result, _| {
                if let Err(error) = result {
                    log::warn!("OpenCode question reply failed: {error:#}");
                }
            },
        );
    }

    pub fn reject_pending_question(&mut self, request_id: String, ctx: &mut ModelContext<Self>) {
        let Some(question) = self
            .pending_questions
            .iter()
            .find(|question| question.id == request_id)
            .cloned()
        else {
            return;
        };
        let settings = OpenCodeServerSettings::as_ref(ctx);
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("OpenCode question rejection skipped: {error:#}");
                return;
            }
        };
        let directory = self.directory_for_session(&question.session_id);
        self.pending_questions
            .retain(|pending| pending.id != request_id);
        ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);

        let _ = ctx.spawn(
            async move { reject_question(&config, &request_id, directory.as_deref()).await },
            move |_, result, _| {
                if let Err(error) = result {
                    log::warn!("OpenCode question rejection failed: {error:#}");
                }
            },
        );
    }

    fn directory_for_session(&self, session_id: &str) -> Option<PathBuf> {
        self.active_session
            .as_ref()
            .filter(|session| session.summary.id == session_id)
            .and_then(|session| session.summary.directory.clone())
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(|session| session.directory.clone())
            })
    }

    fn selected_model(&self) -> Option<OpenCodeSelectedModel> {
        self.selected_model_id
            .as_ref()
            .and_then(|selected_model_id| {
                self.models
                    .iter()
                    .find(|model| model.id == *selected_model_id)
            })
            .map(|model| OpenCodeSelectedModel {
                provider_id: model.provider_id.clone(),
                model_id: model.model_id.clone(),
            })
    }

    fn stop_active_session_polling(&mut self) {
        self.active_session_poll_generation = self.active_session_poll_generation.wrapping_add(1);
    }

    fn start_active_session_polling(&mut self, session_id: String, ctx: &mut ModelContext<Self>) {
        self.stop_active_session_polling();
        let generation = self.active_session_poll_generation;
        self.schedule_active_session_poll(session_id, generation, ctx);
    }

    fn schedule_active_session_poll(
        &mut self,
        session_id: String,
        generation: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        let _ = ctx.spawn(
            async move {
                Timer::after(ACTIVE_SESSION_POLL_INTERVAL).await;
                session_id
            },
            move |model, session_id, ctx| {
                if model.should_poll_active_session(&session_id, generation) {
                    model.poll_active_session(session_id, generation, ctx);
                }
            },
        );
    }

    fn should_poll_active_session(&self, session_id: &str, generation: u64) -> bool {
        self.active_session_poll_generation == generation
            && self
                .active_session
                .as_ref()
                .is_some_and(|session| session.summary.id == session_id)
    }

    fn poll_active_session(
        &mut self,
        session_id: String,
        generation: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.local_turn_session_id.as_deref() == Some(session_id.as_str()) {
            self.schedule_active_session_poll(session_id, generation, ctx);
            return;
        }

        let settings = OpenCodeServerSettings::as_ref(ctx);
        if !*settings.enabled {
            return;
        }
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.status = OpenCodeServerStatus::Disconnected {
                    message: error.to_string(),
                };
                ctx.emit(OpenCodeServerModelEvent::StatusChanged);
                return;
            }
        };
        let fallback_summary = self
            .active_session
            .as_ref()
            .filter(|session| session.summary.id == session_id)
            .map(|session| session.summary.clone())
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .cloned()
            });
        let read_session_id = session_id.clone();

        let _ = ctx.spawn(
            async move { read_session(&config, &read_session_id, fallback_summary).await },
            move |model, result, ctx| {
                if !model.should_poll_active_session(&session_id, generation) {
                    return;
                }
                match result {
                    Ok(detail) => model.apply_polled_session_detail(detail, ctx),
                    Err(error) => {
                        log::warn!("OpenCode session poll failed: {error:#}");
                    }
                }
                if model.should_poll_active_session(&session_id, generation) {
                    model.schedule_active_session_poll(session_id, generation, ctx);
                }
            },
        );
    }

    fn start_pending_request_polling(&mut self, session_id: String, ctx: &mut ModelContext<Self>) {
        self.pending_request_poll_generation = self.pending_request_poll_generation.wrapping_add(1);
        let generation = self.pending_request_poll_generation;
        self.poll_pending_requests(session_id, generation, ctx);
    }

    fn schedule_pending_request_poll(
        &mut self,
        session_id: String,
        generation: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        let _ = ctx.spawn(
            async move {
                Timer::after(PENDING_REQUEST_POLL_INTERVAL).await;
            },
            move |model, _, ctx| {
                model.poll_pending_requests(session_id, generation, ctx);
            },
        );
    }

    fn should_poll_pending_requests(&self, session_id: &str, generation: u64) -> bool {
        generation == self.pending_request_poll_generation
            && (self.local_turn_session_id.as_deref() == Some(session_id)
                || self
                    .pending_permissions
                    .iter()
                    .any(|permission| permission.session_id == session_id)
                || self
                    .pending_questions
                    .iter()
                    .any(|question| question.session_id == session_id))
    }

    fn poll_pending_requests(
        &mut self,
        session_id: String,
        generation: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.should_poll_pending_requests(&session_id, generation) {
            return;
        }

        let settings = OpenCodeServerSettings::as_ref(ctx);
        let config = match OpenCodeServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("OpenCode pending request poll skipped: {error:#}");
                return;
            }
        };
        let directory = self.directory_for_session(&session_id);
        let callback_session_id = session_id.clone();

        let _ = ctx.spawn(
            async move { list_pending_requests(&config, directory.as_deref()).await },
            move |model, result, ctx| {
                if !model.should_poll_pending_requests(&callback_session_id, generation) {
                    return;
                }
                match result {
                    Ok(requests) => {
                        model.apply_pending_requests(callback_session_id.clone(), requests, ctx);
                    }
                    Err(error) => {
                        log::warn!("OpenCode pending request poll failed: {error:#}");
                    }
                }
                if model.should_poll_pending_requests(&callback_session_id, generation) {
                    model.schedule_pending_request_poll(callback_session_id, generation, ctx);
                }
            },
        );
    }

    fn apply_pending_requests(
        &mut self,
        session_id: String,
        requests: OpenCodePendingRequests,
        ctx: &mut ModelContext<Self>,
    ) {
        let permissions = requests
            .permissions
            .into_iter()
            .filter(|permission| permission.session_id == session_id)
            .collect::<Vec<_>>();
        let questions = requests
            .questions
            .into_iter()
            .filter(|question| question.session_id == session_id)
            .collect::<Vec<_>>();
        if self.pending_permissions == permissions && self.pending_questions == questions {
            return;
        }
        self.pending_permissions = permissions;
        self.pending_questions = questions;
        ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);
    }

    fn clear_pending_requests_for_session(
        &mut self,
        session_id: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        let permission_count = self.pending_permissions.len();
        let question_count = self.pending_questions.len();
        self.pending_permissions
            .retain(|permission| permission.session_id != session_id);
        self.pending_questions
            .retain(|question| question.session_id != session_id);
        if self.pending_permissions.len() != permission_count
            || self.pending_questions.len() != question_count
        {
            ctx.emit(OpenCodeServerModelEvent::PendingRequestsChanged);
        }
    }

    fn apply_polled_session_detail(
        &mut self,
        detail: OpenCodeSessionDetail,
        ctx: &mut ModelContext<Self>,
    ) {
        let session_id = detail.summary.id.clone();
        let summary_changed = upsert_session_summary(&mut self.sessions, detail.summary.clone());
        let active_session_changed = self.active_session.as_ref() != Some(&detail);
        if active_session_changed {
            if let Some(conversation_id) =
                self.conversation_id_by_session_id.get(&session_id).copied()
            {
                let conversation =
                    opencode_session_detail_to_ai_conversation(conversation_id, &detail);
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    history.refresh_cached_codex_conversation(conversation, ctx);
                });
            }
            self.active_session = Some(detail);
            ctx.emit(OpenCodeServerModelEvent::ActiveSessionChanged);
        }
        if summary_changed {
            ctx.emit(OpenCodeServerModelEvent::SessionsChanged);
        }
    }

    fn cache_session_detail_as_conversation(
        &mut self,
        conversation_id: AIConversationId,
        detail: &OpenCodeSessionDetail,
        ctx: &mut ModelContext<Self>,
    ) {
        let session_id = detail.summary.id.clone();
        let conversation = opencode_session_detail_to_ai_conversation(conversation_id, detail);
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, _ctx| {
            history.cache_external_conversation(conversation);
        });
        self.conversation_id_by_session_id
            .insert(session_id.clone(), conversation_id);
        self.session_id_by_conversation_id
            .insert(conversation_id, session_id);
    }
}

impl Entity for OpenCodeServerModel {
    type Event = OpenCodeServerModelEvent;
}

impl SingletonEntity for OpenCodeServerModel {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenCodeServerConfig {
    server_url: Url,
    basic_auth_header: Option<HeaderValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenCodeSelectedModel {
    provider_id: String,
    model_id: String,
}

impl OpenCodeServerConfig {
    fn from_settings(settings: &OpenCodeServerSettings) -> Result<Self> {
        Self::from_values(
            settings.server_url.value(),
            settings.username.value(),
            settings.password.value(),
        )
    }

    fn from_values(server_url: &str, username: &str, password: &str) -> Result<Self> {
        let server_url = normalize_opencode_server_url(server_url)?;
        let username = username.trim();
        let password = password.trim();
        let basic_auth_header = if !username.is_empty() && !password.is_empty() {
            Some(HeaderValue::from_str(&basic_auth_header(
                username, password,
            ))?)
        } else if is_loopback_url(&server_url) {
            None
        } else {
            bail!("OpenCode server credentials are required for non-loopback URLs");
        };
        Ok(Self {
            server_url,
            basic_auth_header,
        })
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.server_url.clone();
        url.set_path(path.trim_start_matches('/'));
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    fn request(&self, client: &Client, method: Method, url: Url) -> reqwest::RequestBuilder {
        let request = client.request(method, url);
        if let Some(header) = &self.basic_auth_header {
            request.header(AUTHORIZATION, header.clone())
        } else {
            request
        }
    }
}

pub fn normalize_opencode_server_url(input: &str) -> Result<Url> {
    let trimmed = input.trim();
    let raw = if trimmed.is_empty() {
        DEFAULT_OPENCODE_SERVER_URL.to_string()
    } else if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url =
        Url::parse(&raw).with_context(|| format!("Invalid OpenCode server URL {raw:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("OpenCode server URL must use http or https");
    }
    if url.host_str().is_none() {
        bail!("OpenCode server URL must include a host");
    }
    if url.port().is_none() {
        url.set_port(Some(4096))
            .map_err(|_| anyhow!("Could not set OpenCode server port"))?;
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub fn opencode_start_command(server_url: &str) -> String {
    let Ok(url) = normalize_opencode_server_url(server_url) else {
        return "opencode serve".to_string();
    };
    let host = url.host_str().unwrap_or("127.0.0.1");
    let port = url.port().unwrap_or(4096);
    format!("opencode serve --hostname {host} --port {port}")
}

pub fn opencode_session_updated_at_utc(session: &OpenCodeSessionSummary) -> Option<DateTime<Utc>> {
    session.updated_at.and_then(timestamp_to_utc)
}

fn basic_auth_header(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        BASE64_STANDARD.encode(format!("{username}:{password}"))
    )
}

fn is_loopback_url(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        matches!(host, "localhost" | "localhost.localdomain")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    })
}

fn directory_url(config: &OpenCodeServerConfig, path: &str, directory: Option<&Path>) -> Url {
    let mut url = config.endpoint(path);
    if let Some(directory) = directory {
        url.query_pairs_mut()
            .append_pair("directory", &directory.to_string_lossy());
    }
    url
}

async fn list_project_sessions_with_backoff(
    config: &OpenCodeServerConfig,
    project_roots: Vec<PathBuf>,
    imported_project_paths: Vec<PathBuf>,
) -> Result<OpenCodeRefreshSnapshot> {
    let mut last_error = None;
    for delay in RECONNECT_BACKOFF {
        if delay > Duration::from_millis(0) {
            Timer::after(delay).await;
        }
        match list_project_sessions(config, &project_roots, &imported_project_paths).await {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("OpenCode server refresh failed")))
}

async fn list_project_sessions(
    config: &OpenCodeServerConfig,
    project_roots: &[PathBuf],
    imported_project_paths: &[PathBuf],
) -> Result<OpenCodeRefreshSnapshot> {
    check_health(config).await?;
    let projects = list_projects(config).await?;
    let directories = matched_project_directories(&projects, project_roots, imported_project_paths);
    let models = list_models(config).await.unwrap_or_else(|error| {
        log::warn!("OpenCode provider metadata failed: {error:#}");
        Vec::new()
    });

    let mut by_id = BTreeMap::new();
    for directory in directories {
        let statuses = list_session_status(config, &directory)
            .await
            .unwrap_or_default();
        let sessions = list_sessions_for_directory(config, &directory, &statuses).await?;
        for session in sessions {
            by_id.insert(session.id.clone(), session);
        }
    }

    let mut sessions = by_id.into_values().collect::<Vec<_>>();
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.title.cmp(&b.title))
    });
    Ok(OpenCodeRefreshSnapshot { sessions, models })
}

async fn check_health(config: &OpenCodeServerConfig) -> Result<()> {
    #[derive(Deserialize)]
    struct Health {
        healthy: bool,
        version: Option<String>,
    }

    let client = Client::new();
    let url = config.endpoint("/global/health");
    let response = config
        .request(&client, Method::GET, url)
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .context("Could not reach OpenCode server")?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("OpenCode health check returned {status}: {text}");
    }
    let health: Health = response
        .json()
        .await
        .context("Could not parse OpenCode health response")?;
    if !health.healthy {
        bail!(
            "OpenCode server reported unhealthy{}",
            health
                .version
                .map(|version| format!(" ({version})"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

async fn request_json<T: DeserializeOwned>(
    config: &OpenCodeServerConfig,
    method: Method,
    url: Url,
    body: Option<Value>,
) -> Result<T> {
    let client = Client::new();
    let mut request = config.request(&client, method, url.clone());
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("OpenCode request failed: {url}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("OpenCode server returned {status}: {text}");
    }
    response
        .json()
        .await
        .with_context(|| format!("Could not parse OpenCode response from {url}"))
}

async fn list_projects(config: &OpenCodeServerConfig) -> Result<Vec<OpenCodeProjectWire>> {
    request_json(config, Method::GET, config.endpoint("/project"), None).await
}

async fn list_models(config: &OpenCodeServerConfig) -> Result<Vec<OpenCodeModelInfo>> {
    let response: ProviderListWire = request_json(
        config,
        Method::GET,
        config.endpoint("/config/providers"),
        None,
    )
    .await?;
    Ok(provider_list_to_models(response))
}

async fn list_session_status(
    config: &OpenCodeServerConfig,
    directory: &Path,
) -> Result<HashMap<String, String>> {
    let response: HashMap<String, Value> = request_json(
        config,
        Method::GET,
        directory_url(config, "/session/status", Some(directory)),
        None,
    )
    .await?;
    Ok(response
        .into_iter()
        .filter_map(|(session_id, value)| {
            status_label_from_value(&value).map(|status| (session_id, status))
        })
        .collect())
}

async fn list_sessions_for_directory(
    config: &OpenCodeServerConfig,
    directory: &Path,
    statuses: &HashMap<String, String>,
) -> Result<Vec<OpenCodeSessionSummary>> {
    let mut url = directory_url(config, "/session", Some(directory));
    url.query_pairs_mut()
        .append_pair("limit", &DEFAULT_SESSION_LIMIT.to_string());
    let sessions: Vec<OpenCodeSessionWire> = request_json(config, Method::GET, url, None).await?;
    Ok(sessions
        .into_iter()
        .map(|session| {
            let mut summary = session.into_summary();
            if summary.directory.is_none() {
                summary.directory = Some(directory.to_path_buf());
            }
            summary.status = statuses.get(&summary.id).cloned();
            summary
        })
        .collect())
}

async fn read_session(
    config: &OpenCodeServerConfig,
    session_id: &str,
    fallback_summary: Option<OpenCodeSessionSummary>,
) -> Result<OpenCodeSessionDetail> {
    let directory = fallback_summary
        .as_ref()
        .and_then(|summary| summary.directory.as_deref());
    let path = format!("/session/{session_id}");
    let mut summary: OpenCodeSessionSummary = request_json::<OpenCodeSessionWire>(
        config,
        Method::GET,
        directory_url(config, &path, directory),
        None,
    )
    .await
    .map(OpenCodeSessionWire::into_summary)
    .or_else(|error| fallback_summary.clone().ok_or(error))?;
    if summary.directory.is_none() {
        summary.directory = fallback_summary
            .as_ref()
            .and_then(|fallback| fallback.directory.clone());
    }
    let message_path = format!("/session/{session_id}/message");
    let messages: Vec<OpenCodeMessageWire> = request_json(
        config,
        Method::GET,
        directory_url(config, &message_path, summary.directory.as_deref()),
        None,
    )
    .await?;
    Ok(OpenCodeSessionDetail {
        summary,
        items: messages_to_conversation_items(&messages),
    })
}

async fn create_session(
    config: &OpenCodeServerConfig,
    directory: PathBuf,
) -> Result<OpenCodeSessionDetail> {
    let summary: OpenCodeSessionSummary = request_json::<OpenCodeSessionWire>(
        config,
        Method::POST,
        directory_url(config, "/session", Some(&directory)),
        Some(json!({ "title": "New OpenCode conversation" })),
    )
    .await?
    .into_summary();
    let session_id = summary.id.clone();
    read_session(config, &session_id, Some(summary)).await
}

async fn prompt_session(
    config: &OpenCodeServerConfig,
    session_id: &str,
    fallback_summary: Option<OpenCodeSessionSummary>,
    prompt: &str,
    selected_model: Option<OpenCodeSelectedModel>,
) -> Result<OpenCodePromptResult> {
    let directory = fallback_summary
        .as_ref()
        .and_then(|summary| summary.directory.as_deref());
    let mut body = json!({
        "parts": [
            {
                "type": "text",
                "text": prompt,
            }
        ],
    });
    if let Some(model) = selected_model {
        body["model"] = json!({
            "providerID": model.provider_id,
            "modelID": model.model_id,
        });
    }
    let path = format!("/session/{session_id}/message");
    let response: OpenCodeMessageWire = request_json(
        config,
        Method::POST,
        directory_url(config, &path, directory),
        Some(body),
    )
    .await?;
    let detail = read_session(config, session_id, fallback_summary)
        .await
        .ok();
    Ok(OpenCodePromptResult {
        output_items: messages_to_conversation_items(&[response]),
        detail,
    })
}

async fn list_pending_requests(
    config: &OpenCodeServerConfig,
    directory: Option<&Path>,
) -> Result<OpenCodePendingRequests> {
    let permissions: Vec<OpenCodePermissionRequestWire> = request_json(
        config,
        Method::GET,
        directory_url(config, "/permission", directory),
        None,
    )
    .await?;
    let questions: Vec<OpenCodeQuestionRequestWire> = request_json(
        config,
        Method::GET,
        directory_url(config, "/question", directory),
        None,
    )
    .await?;
    Ok(OpenCodePendingRequests {
        permissions: permissions
            .into_iter()
            .map(OpenCodePermissionRequestWire::into_pending)
            .collect(),
        questions: questions
            .into_iter()
            .map(OpenCodeQuestionRequestWire::into_pending)
            .collect(),
    })
}

async fn reply_permission(
    config: &OpenCodeServerConfig,
    request_id: &str,
    directory: Option<&Path>,
    reply: OpenCodePermissionReply,
) -> Result<bool> {
    let path = format!("/permission/{request_id}/reply");
    request_json(
        config,
        Method::POST,
        directory_url(config, &path, directory),
        Some(json!({ "reply": reply.wire_value() })),
    )
    .await
}

async fn reply_question(
    config: &OpenCodeServerConfig,
    request_id: &str,
    directory: Option<&Path>,
    answers: Vec<Vec<String>>,
) -> Result<bool> {
    let path = format!("/question/{request_id}/reply");
    request_json(
        config,
        Method::POST,
        directory_url(config, &path, directory),
        Some(json!({ "answers": answers })),
    )
    .await
}

async fn reject_question(
    config: &OpenCodeServerConfig,
    request_id: &str,
    directory: Option<&Path>,
) -> Result<bool> {
    let path = format!("/question/{request_id}/reject");
    request_json(
        config,
        Method::POST,
        directory_url(config, &path, directory),
        None,
    )
    .await
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct OpenCodeProjectWire {
    #[allow(dead_code)]
    id: String,
    worktree: PathBuf,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct OpenCodePermissionRequestWire {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    permission: String,
    #[serde(default)]
    patterns: Vec<String>,
    #[serde(default)]
    always: Vec<String>,
}

impl OpenCodePermissionRequestWire {
    fn into_pending(self) -> OpenCodePendingPermission {
        OpenCodePendingPermission {
            id: self.id,
            session_id: self.session_id,
            permission: self.permission,
            patterns: self.patterns,
            always: self.always,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct OpenCodeQuestionRequestWire {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(default)]
    questions: Vec<OpenCodeQuestionInfoWire>,
}

impl OpenCodeQuestionRequestWire {
    fn into_pending(self) -> OpenCodePendingQuestion {
        OpenCodePendingQuestion {
            id: self.id,
            session_id: self.session_id,
            questions: self
                .questions
                .into_iter()
                .map(OpenCodeQuestionInfoWire::into_info)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct OpenCodeQuestionInfoWire {
    #[serde(default)]
    header: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    options: Vec<OpenCodeQuestionOptionWire>,
    #[serde(default)]
    multiple: bool,
    #[serde(default)]
    custom: bool,
}

impl OpenCodeQuestionInfoWire {
    fn into_info(self) -> OpenCodeQuestionInfo {
        OpenCodeQuestionInfo {
            header: self.header,
            question: self.question,
            options: self
                .options
                .into_iter()
                .map(OpenCodeQuestionOptionWire::into_option)
                .collect(),
            multiple: self.multiple,
            custom: self.custom,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct OpenCodeQuestionOptionWire {
    label: String,
    #[serde(default)]
    description: String,
}

impl OpenCodeQuestionOptionWire {
    fn into_option(self) -> OpenCodeQuestionOption {
        OpenCodeQuestionOption {
            label: self.label,
            description: self.description,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeSessionWire {
    id: String,
    title: Option<String>,
    slug: Option<String>,
    #[serde(rename = "projectID")]
    project_id: Option<String>,
    directory: Option<PathBuf>,
    time: Option<OpenCodeSessionTimeWire>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeSessionTimeWire {
    updated: Option<Value>,
}

impl OpenCodeSessionWire {
    fn into_summary(self) -> OpenCodeSessionSummary {
        OpenCodeSessionSummary {
            id: self.id.clone(),
            title: self
                .title
                .filter(|title| !title.trim().is_empty())
                .or(self.slug)
                .unwrap_or(self.id),
            directory: self.directory,
            project_id: self.project_id,
            updated_at: self
                .time
                .and_then(|time| time.updated)
                .and_then(timestamp_value_to_i64),
            status: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderListWire {
    #[serde(default, alias = "all")]
    providers: Vec<OpenCodeProviderWire>,
    #[serde(default)]
    default: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeProviderWire {
    id: String,
    name: Option<String>,
    #[serde(default)]
    models: HashMap<String, OpenCodeProviderModelWire>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeProviderModelWire {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeMessageWire {
    info: OpenCodeMessageInfoWire,
    #[serde(default)]
    parts: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeMessageInfoWire {
    role: String,
    error: Option<Value>,
    finish: Option<Value>,
}

fn provider_list_to_models(response: ProviderListWire) -> Vec<OpenCodeModelInfo> {
    let mut models = Vec::new();
    for provider in response.providers {
        let provider_name = provider.name.unwrap_or_else(|| provider.id.clone());
        let default_model_id = response.default.get(&provider.id).cloned();
        for (model_key, model) in provider.models {
            let model_id = model.id.unwrap_or(model_key);
            let model_name = model.name.unwrap_or_else(|| model_id.clone());
            let id = format!("{}/{}", provider.id, model_id);
            models.push(OpenCodeModelInfo {
                id,
                display_name: format!("{provider_name}: {model_name}"),
                provider_id: provider.id.clone(),
                model_id: model_id.clone(),
                is_default: default_model_id.as_deref() == Some(model_id.as_str()),
            });
        }
    }
    models.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    models
}

fn matched_project_directories(
    projects: &[OpenCodeProjectWire],
    project_roots: &[PathBuf],
    imported_project_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut directories = Vec::new();
    for root in project_roots {
        if let Some(project) = projects
            .iter()
            .filter(|project| root == &project.worktree || root.starts_with(&project.worktree))
            .max_by_key(|project| project.worktree.components().count())
        {
            if seen.insert(project.worktree.clone()) {
                directories.push(project.worktree.clone());
            }
        }
    }

    for imported in imported_project_paths {
        let directory = projects
            .iter()
            .filter(|project| {
                imported == &project.worktree || imported.starts_with(&project.worktree)
            })
            .max_by_key(|project| project.worktree.components().count())
            .map(|project| project.worktree.clone())
            .unwrap_or_else(|| imported.clone());
        if seen.insert(directory.clone()) {
            directories.push(directory);
        }
    }
    directories
}

fn messages_to_conversation_items(
    messages: &[OpenCodeMessageWire],
) -> Vec<OpenCodeConversationItem> {
    messages
        .iter()
        .filter_map(|message| message_to_conversation_item(message))
        .collect()
}

fn message_to_conversation_item(message: &OpenCodeMessageWire) -> Option<OpenCodeConversationItem> {
    let role = message.info.role.clone();
    let mut texts = Vec::new();
    for part in &message.parts {
        if role == "user" {
            if part_type(part) == Some("text") {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            continue;
        }

        if let Some(text) = part_to_response_text(part) {
            texts.push(text);
        }
    }
    if role != "user" {
        if let Some(error) = error_text(&message.info.error) {
            texts.push(format!("Error: {error}"));
        }
        if finish_value_is_aborted(&message.info.finish) {
            texts.push("Aborted.".to_string());
        }
    }
    let text = texts
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.trim().is_empty()).then_some(OpenCodeConversationItem { role, text })
}

fn part_type(part: &Value) -> Option<&str> {
    part.get("type").and_then(Value::as_str)
}

fn part_to_response_text(part: &Value) -> Option<String> {
    match part_type(part)? {
        "text" => part
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        "reasoning" => part
            .get("text")
            .and_then(Value::as_str)
            .map(|text| format!("Reasoning: {text}")),
        "tool" => tool_part_to_text(part),
        "patch" => patch_part_to_text(part),
        "subtask" => subtask_part_to_text(part),
        "agent" => agent_part_to_text(part),
        "error" => error_text(&Some(part.clone())).map(|error| format!("Error: {error}")),
        part_type if part_type.contains("abort") => Some("Aborted.".to_string()),
        _ => None,
    }
}

fn tool_part_to_text(part: &Value) -> Option<String> {
    let tool = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
    let state = part.get("state")?;
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| state.get("type").and_then(Value::as_str));
    match status {
        Some("completed") => {
            let title = state.get("title").and_then(Value::as_str);
            let output = state
                .get("output")
                .map(readable_json)
                .filter(|output| !output.trim().is_empty());
            Some(match (title, output) {
                (Some(title), Some(output)) => format!("Tool {tool}: {title}\n{output}"),
                (Some(title), None) => format!("Tool {tool}: {title}"),
                (None, Some(output)) => format!("Tool {tool} output:\n{output}"),
                (None, None) => format!("Tool {tool} completed."),
            })
        }
        Some("error") => {
            let error =
                error_text(&state.get("error").cloned()).unwrap_or_else(|| readable_json(state));
            Some(format!("Tool {tool} error: {error}"))
        }
        Some(status) => Some(format!("Tool {tool}: {status}")),
        None => Some(format!("Tool {tool}: {}", readable_json(state))),
    }
}

fn patch_part_to_text(part: &Value) -> Option<String> {
    let files = part
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|file| {
                    file.as_str().map(ToOwned::to_owned).or_else(|| {
                        file.get("path")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if files.is_empty() {
        Some("Patch applied.".to_string())
    } else {
        Some(format!("Patch applied: {}", files.join(", ")))
    }
}

fn subtask_part_to_text(part: &Value) -> Option<String> {
    let agent = part.get("agent").and_then(Value::as_str);
    let description = part
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| part.get("prompt").and_then(Value::as_str))?;
    Some(match agent {
        Some(agent) => format!("Subtask ({agent}): {description}"),
        None => format!("Subtask: {description}"),
    })
}

fn agent_part_to_text(part: &Value) -> Option<String> {
    part.get("text")
        .and_then(Value::as_str)
        .map(|text| format!("Agent: {text}"))
}

fn opencode_session_detail_to_ai_conversation(
    conversation_id: AIConversationId,
    detail: &OpenCodeSessionDetail,
) -> AIConversation {
    let mut conversation = AIConversation::new_with_id(conversation_id, false);
    conversation.set_fallback_display_title(detail.summary.title.clone());
    conversation.set_exclude_from_navigation(true);

    let working_directory = detail
        .summary
        .directory
        .as_ref()
        .and_then(|directory| directory.to_str())
        .map(ToOwned::to_owned);
    let start_time = detail
        .summary
        .updated_at
        .and_then(timestamp_to_local)
        .unwrap_or_else(Local::now);

    let mut current_query = None;
    let mut output_items = Vec::new();
    let mut appended_any = false;
    for item in &detail.items {
        if item.role == "user" {
            if current_query.is_some() || !output_items.is_empty() {
                append_opencode_restored_exchange(
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
        append_opencode_restored_exchange(
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
            Some("No messages were returned for this OpenCode conversation.".to_string()),
            None,
            false,
            start_time,
        );
    }

    conversation
}

fn append_opencode_restored_exchange(
    conversation: &mut AIConversation,
    query: Option<String>,
    output_items: &[OpenCodeConversationItem],
    working_directory: Option<String>,
    start_time: DateTime<Local>,
) {
    let output = opencode_items_to_agent_text(output_items);
    let _ = conversation.append_codex_exchange(
        query,
        (!output.trim().is_empty()).then_some(output),
        working_directory,
        false,
        start_time,
    );
}

fn opencode_items_to_agent_text(items: &[OpenCodeConversationItem]) -> String {
    items
        .iter()
        .filter(|item| item.role != "user" && !item.text.trim().is_empty())
        .map(|item| {
            if item.role == "assistant" {
                item.text.clone()
            } else {
                format!("{}: {}", item.role, item.text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn upsert_session_summary(
    sessions: &mut Vec<OpenCodeSessionSummary>,
    summary: OpenCodeSessionSummary,
) -> bool {
    if let Some(existing) = sessions.iter_mut().find(|session| session.id == summary.id) {
        if existing == &summary {
            return false;
        }
        *existing = summary;
    } else {
        sessions.push(summary);
    }
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.title.cmp(&b.title))
    });
    true
}

fn readable_json(value: &Value) -> String {
    value.as_str().map(ToOwned::to_owned).unwrap_or_else(|| {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    })
}

fn error_text(value: &Option<Value>) -> Option<String> {
    let value = value.as_ref()?;
    if value.is_null() {
        return None;
    }
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| Some(readable_json(value)))
}

fn status_label_from_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("status")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            value
                .get("state")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            value
                .get("error")
                .and_then(error_text_from_value)
                .map(|error| format!("error: {error}"))
        })
        .or_else(|| {
            value
                .get("running")
                .and_then(Value::as_bool)
                .and_then(|running| running.then(|| "running".to_string()))
        })
}

fn error_text_from_value(value: &Value) -> Option<String> {
    error_text(&Some(value.clone()))
}

fn finish_value_is_aborted(value: &Option<Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    value.as_str().is_some_and(|finish| {
        finish.eq_ignore_ascii_case("abort") || finish.eq_ignore_ascii_case("aborted")
    }) || value
        .get("reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| {
            reason.eq_ignore_ascii_case("abort") || reason.eq_ignore_ascii_case("aborted")
        })
}

fn timestamp_value_to_i64(value: Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn timestamp_to_utc(timestamp: i64) -> Option<DateTime<Utc>> {
    let milliseconds = if timestamp.abs() < 10_000_000_000 {
        timestamp * 1000
    } else {
        timestamp
    };
    Utc.timestamp_millis_opt(milliseconds).single()
}

fn timestamp_to_local(timestamp: i64) -> Option<DateTime<Local>> {
    timestamp_to_utc(timestamp).map(DateTime::<Local>::from)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
