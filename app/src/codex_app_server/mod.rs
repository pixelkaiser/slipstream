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
use warpui::{r#async::Timer, Entity, EntityId, ModelContext, SingletonEntity};
use websocket::{Message, WebSocket, WebsocketMessage as _};

use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::agent::AIAgentExchangeId;
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
const ACTIVE_THREAD_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CODEX_APPROVAL_POLICY: &str = "on-request";
const CODEX_APPROVALS_REVIEWER: &str = "user";

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
pub struct CodexModelInfo {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConversationItem {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexUserInputQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexUserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub is_other: bool,
    pub is_secret: bool,
    pub options: Vec<CodexUserInputQuestionOption>,
    answer_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexPendingApprovalControl {
    ApprovalDecision(CodexApprovalDecision),
    UserInputOption {
        question_id: String,
        label: String,
        description: String,
    },
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
    pub user_input_questions: Vec<CodexUserInputQuestion>,
    decision_wire_values: HashMap<CodexApprovalDecision, Value>,
}

impl CodexPendingApproval {
    pub fn title(&self) -> String {
        self.single_user_input_question()
            .map(|question| question.header.clone())
            .filter(|header| !header.trim().is_empty())
            .unwrap_or_else(|| {
                if self.has_meaningful_user_input_question() {
                    "Codex needs input".to_string()
                } else {
                    "Codex needs approval".to_string()
                }
            })
    }

    pub fn message(&self) -> String {
        self.single_user_input_question()
            .map(|question| question.question.clone())
            .filter(|question| {
                !question.trim().is_empty() && !is_generic_codex_approval_reason(question)
            })
            .unwrap_or_else(|| self.defaulted_reason())
    }

    pub fn is_user_input_request(&self) -> bool {
        is_request_user_input_method(&self.method)
    }

    fn has_meaningful_user_input_question(&self) -> bool {
        self.user_input_questions.iter().any(|question| {
            !question.options.is_empty()
                || (!question.question.trim().is_empty()
                    && !is_generic_codex_approval_reason(&question.question))
        })
    }

    fn defaulted_reason(&self) -> String {
        if !is_generic_codex_approval_reason(&self.reason) {
            return self.reason.clone();
        }
        if self.command.is_some() {
            "Approve Codex to run this command?".to_string()
        } else if self.method.to_ascii_lowercase().contains("filechange") {
            "Approve Codex to apply the requested file changes?".to_string()
        } else if self.is_user_input_request() {
            "Approve Codex to continue with the proposed plan?".to_string()
        } else {
            "Approve this Codex request?".to_string()
        }
    }

    pub fn effective_available_decisions(&self) -> Vec<CodexApprovalDecision> {
        let has_option_question = self
            .user_input_questions
            .iter()
            .any(|question| !question.options.is_empty());
        if self.available_decisions.is_empty() && !has_option_question {
            default_approval_decisions(&self.method)
        } else {
            self.available_decisions.clone()
        }
    }

    pub fn controls(&self) -> Vec<CodexPendingApprovalControl> {
        let option_questions = self
            .user_input_questions
            .iter()
            .filter(|question| !question.options.is_empty())
            .collect::<Vec<_>>();
        if let [question] = option_questions.as_slice() {
            return question
                .options
                .iter()
                .map(|option| CodexPendingApprovalControl::UserInputOption {
                    question_id: question.id.clone(),
                    label: option.label.clone(),
                    description: option.description.clone(),
                })
                .collect();
        }

        let decisions = if self.available_decisions.is_empty() {
            default_approval_decisions(&self.method)
        } else {
            self.available_decisions.clone()
        };
        decisions
            .into_iter()
            .map(CodexPendingApprovalControl::ApprovalDecision)
            .collect()
    }

    pub fn single_user_input_question(&self) -> Option<&CodexUserInputQuestion> {
        if self.user_input_questions.len() == 1 {
            self.user_input_questions.first()
        } else {
            None
        }
    }

    fn approval_result(&self, decision: CodexApprovalDecision) -> Value {
        let wire_value = self
            .decision_wire_values
            .get(&decision)
            .cloned()
            .unwrap_or_else(|| json!(decision.wire_value()));
        json!({
            "decision": wire_value,
        })
    }

    fn user_input_result(&self, question_id: &str, answer: &str) -> Option<Value> {
        let question = self
            .user_input_questions
            .iter()
            .find(|question| question.id == question_id)?;

        if let Some(answer_index) = question.answer_index {
            let mut answers = vec![json!({ "answers": [] }); answer_index + 1];
            answers[answer_index] = json!({ "answers": [answer] });
            return Some(json!({ "answers": answers }));
        }

        Some(json!({
            "answers": {
                question_id: {
                    "answers": [answer],
                },
            },
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodexApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptForPrefix,
    Decline,
    Cancel,
}

#[allow(dead_code)]
impl CodexApprovalDecision {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accept => "Accept",
            Self::AcceptForSession => "Accept for session",
            Self::AcceptForPrefix => "Accept for prefix",
            Self::Decline => "Decline",
            Self::Cancel => "Cancel",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        match normalize_identifier(value).as_str() {
            "accept" | "approve" | "allow" | "yes" => Some(Self::Accept),
            "acceptforsession" | "approveforsession" | "allowforsession" => {
                Some(Self::AcceptForSession)
            }
            "acceptforprefix" | "approveforprefix" | "allowforprefix" => {
                Some(Self::AcceptForPrefix)
            }
            "acceptwithexecpolicyamendment" => Some(Self::AcceptForPrefix),
            "applynetworkpolicyamendment" => Some(Self::AcceptForSession),
            "decline" | "deny" | "reject" | "no" => Some(Self::Decline),
            "cancel" | "abort" => Some(Self::Cancel),
            _ => None,
        }
    }

    fn wire_value(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::AcceptForSession => "acceptForSession",
            Self::AcceptForPrefix => "acceptForPrefix",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAppServerModelEvent {
    StatusChanged,
    ThreadsChanged,
    ActiveThreadChanged,
    ModelsChanged,
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
    local_turn_thread_id: Option<String>,
    models: Vec<CodexModelInfo>,
    selected_model_id: Option<String>,
    project_roots: Vec<PathBuf>,
    opening_thread_id: Option<String>,
    active_thread_poll_generation: u64,
    started_thread: Option<CodexStartedThread>,
    conversation_id_by_thread_id: HashMap<String, AIConversationId>,
    thread_id_by_conversation_id: HashMap<AIConversationId, String>,
}

#[allow(dead_code)]
struct CodexActiveTurn {
    thread_id: String,
    client: JsonRpcSocket,
    pending_approval: CodexPendingApproval,
    conversation_context: Option<CodexConversationTurnContext>,
}

#[derive(Debug, Clone, Copy)]
struct CodexConversationTurnContext {
    conversation_id: AIConversationId,
    exchange_id: AIAgentExchangeId,
    terminal_view_id: EntityId,
}

struct CodexStartedThread {
    thread_id: String,
    client: JsonRpcSocket,
}

struct CodexTurnProgress {
    items: Vec<CodexConversationItem>,
    active_turn: Option<CodexActiveTurn>,
}

struct CodexThreadStart {
    detail: CodexThreadDetail,
    client: JsonRpcSocket,
}

struct CodexRefreshSnapshot {
    threads: Vec<CodexThreadSummary>,
    models: Vec<CodexModelInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexPromptCollaborationMode {
    Plan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexPromptRequest {
    prompt: String,
    collaboration_mode: Option<CodexPromptCollaborationMode>,
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
            local_turn_thread_id: None,
            models: Vec::new(),
            selected_model_id: None,
            project_roots: Vec::new(),
            opening_thread_id: None,
            active_thread_poll_generation: 0,
            started_thread: None,
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

    pub fn models(&self) -> &[CodexModelInfo] {
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
            .unwrap_or_else(|| "codex".to_string())
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
        ctx.emit(CodexAppServerModelEvent::ModelsChanged);
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

    pub fn pending_approval_for_conversation(
        &self,
        conversation_id: AIConversationId,
    ) -> Option<&CodexPendingApproval> {
        let thread_id = self.thread_id_by_conversation_id.get(&conversation_id)?;
        let active_turn = self.active_turn.as_ref()?;
        (active_turn.thread_id == *thread_id).then_some(&active_turn.pending_approval)
    }

    pub fn opening_thread_id(&self) -> Option<&str> {
        self.opening_thread_id.as_deref()
    }

    #[allow(dead_code)]
    pub fn conversation_id_for_thread(&self, thread_id: &str) -> Option<AIConversationId> {
        self.conversation_id_by_thread_id.get(thread_id).copied()
    }

    #[allow(dead_code)]
    pub fn thread_id_for_conversation(&self, conversation_id: AIConversationId) -> Option<&str> {
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
            self.stop_active_thread_polling();
            self.status = CodexAppServerStatus::Disabled;
            self.threads.clear();
            self.active_thread = None;
            self.active_turn = None;
            self.local_turn_thread_id = None;
            self.models.clear();
            self.selected_model_id = None;
            self.opening_thread_id = None;
            self.started_thread = None;
            self.conversation_id_by_thread_id.clear();
            self.thread_id_by_conversation_id.clear();
            ctx.emit(CodexAppServerModelEvent::StatusChanged);
            ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
            ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
            ctx.emit(CodexAppServerModelEvent::ModelsChanged);
            return;
        }

        let config = match CodexAppServerConfig::from_settings(settings) {
            Ok(config) => config,
            Err(error) => {
                self.stop_active_thread_polling();
                self.status = CodexAppServerStatus::Disconnected {
                    message: error.to_string(),
                };
                self.threads.clear();
                self.active_thread = None;
                self.active_turn = None;
                self.local_turn_thread_id = None;
                self.models.clear();
                self.started_thread = None;
                ctx.emit(CodexAppServerModelEvent::StatusChanged);
                ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                ctx.emit(CodexAppServerModelEvent::ModelsChanged);
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
                    Ok(snapshot) => {
                        model.status = CodexAppServerStatus::Connected;
                        model.threads = snapshot.threads;
                        model.models = snapshot.models;
                        if model
                            .selected_model_id
                            .as_ref()
                            .is_some_and(|selected_model_id| {
                                !model
                                    .models
                                    .iter()
                                    .any(|codex_model| codex_model.id == *selected_model_id)
                            })
                        {
                            model.selected_model_id = None;
                        }
                        ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                        ctx.emit(CodexAppServerModelEvent::ModelsChanged);
                    }
                    Err(error) => {
                        model.stop_active_thread_polling();
                        model.status = CodexAppServerStatus::Disconnected {
                            message: format!("{error:#}"),
                        };
                        model.threads.clear();
                        model.active_thread = None;
                        model.active_turn = None;
                        model.local_turn_thread_id = None;
                        model.models.clear();
                        model.started_thread = None;
                        ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                        ctx.emit(CodexAppServerModelEvent::ModelsChanged);
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
        self.stop_active_thread_polling();
        self.active_turn = None;
        self.local_turn_thread_id = None;
        self.started_thread = None;

        let _ = ctx.spawn(
            async move { read_thread(&config, &thread_id, fallback_summary).await },
            |model, result, ctx| match result {
                Ok(detail) => {
                    let thread_id = detail.summary.id.clone();
                    model.active_thread = Some(detail);
                    model.start_active_thread_polling(thread_id, ctx);
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

    pub fn open_thread_as_conversation(&mut self, thread_id: String, ctx: &mut ModelContext<Self>) {
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
        self.stop_active_thread_polling();
        self.active_thread = None;
        self.active_turn = None;
        self.local_turn_thread_id = None;
        self.started_thread = None;
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);

        let _ = ctx.spawn(
            async move { read_thread(&config, &thread_id, fallback_summary).await },
            move |model, result, ctx| {
                model.opening_thread_id = None;
                match result {
                    Ok(detail) => {
                        let thread_id = detail.summary.id.clone();
                        model.cache_thread_detail_as_conversation(conversation_id, &detail, ctx);
                        model.active_thread = Some(detail);
                        model.start_active_thread_polling(thread_id.clone(), ctx);
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

    pub fn delete_thread(&mut self, thread_id: String, ctx: &mut ModelContext<Self>) {
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

        let callback_thread_id = thread_id.clone();
        let _ = ctx.spawn(
            async move { archive_thread(&config, &thread_id).await },
            move |model, result, ctx| match result {
                Ok(()) => model.remove_thread_locally(&callback_thread_id, ctx),
                Err(error) => {
                    model.status = CodexAppServerStatus::Disconnected {
                        message: format!("{error:#}"),
                    };
                    ctx.emit(CodexAppServerModelEvent::StatusChanged);
                }
            },
        );
    }

    pub fn start_new_conversation(&mut self, ctx: &mut ModelContext<Self>) {
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

        let model_id = self.selected_model_id.clone();
        let cwd = self.project_roots.first().cloned();
        self.stop_active_thread_polling();
        self.active_thread = None;
        self.active_turn = None;
        self.local_turn_thread_id = None;
        self.started_thread = None;
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);

        let _ = ctx.spawn(
            async move { start_thread(&config, model_id.as_deref(), cwd.as_ref()).await },
            move |model, result, ctx| match result {
                Ok(started_thread) => {
                    let mut detail = started_thread.detail;
                    if detail.summary.title == detail.summary.id {
                        detail.summary.title = "New Codex conversation".to_string();
                    }
                    let thread_id = detail.summary.id.clone();
                    let conversation_id = model
                        .conversation_id_by_thread_id
                        .entry(thread_id.clone())
                        .or_insert_with(AIConversationId::new)
                        .to_owned();
                    model.cache_thread_detail_as_conversation(conversation_id, &detail, ctx);
                    upsert_thread_summary(&mut model.threads, detail.summary.clone());
                    model.active_thread = Some(detail);
                    model.started_thread = Some(CodexStartedThread {
                        thread_id: thread_id.clone(),
                        client: started_thread.client,
                    });
                    model.status = CodexAppServerStatus::Connected;
                    ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
                    ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
                    ctx.emit(CodexAppServerModelEvent::StatusChanged);
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
            },
        );
    }

    #[allow(dead_code)]
    pub fn submit_prompt(&mut self, prompt: String, ctx: &mut ModelContext<Self>) {
        let Some(prompt_request) = parse_codex_prompt_request(&prompt) else {
            return;
        };
        let Some(active_thread_id) = self
            .active_thread
            .as_ref()
            .map(|thread| thread.summary.id.clone())
        else {
            return;
        };
        let display_prompt = display_codex_prompt_request(&prompt_request);
        if let Some(active_thread) = &mut self.active_thread {
            active_thread.items.push(CodexConversationItem {
                role: "user".to_string(),
                text: display_prompt,
            });
            active_thread.items.push(CodexConversationItem {
                role: "codex".to_string(),
                text: "Waiting for Codex...".to_string(),
            });
        }
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        self.active_turn = None;
        self.local_turn_thread_id = Some(active_thread_id.clone());

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
        let model_id = self.selected_model_id.clone();
        let collaboration_mode = prompt_request.collaboration_mode;
        let collaboration_mode_model_id = self.collaboration_mode_model_id();
        let started_thread = self.take_started_thread(&turn_thread_id);
        let _ = ctx.spawn(
            async move {
                continue_thread(
                    &config,
                    &turn_thread_id,
                    &prompt_request.prompt,
                    model_id.as_deref(),
                    collaboration_mode,
                    collaboration_mode_model_id.as_deref(),
                    started_thread,
                )
                .await
            },
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
        let Some(prompt_request) = parse_codex_prompt_request(&prompt) else {
            return false;
        };
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
                Some(display_codex_prompt_request(&prompt_request)),
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
        self.local_turn_thread_id = Some(thread_id.clone());
        let callback_thread_id = thread_id.clone();
        let model_id = self.selected_model_id.clone();
        let collaboration_mode = prompt_request.collaboration_mode;
        let collaboration_mode_model_id = self.collaboration_mode_model_id();
        let started_thread = self.take_started_thread(&thread_id);
        let _ = ctx.spawn(
            async move {
                continue_thread(
                    &config,
                    &thread_id,
                    &prompt_request.prompt,
                    model_id.as_deref(),
                    collaboration_mode,
                    collaboration_mode_model_id.as_deref(),
                    started_thread,
                )
                .await
            },
            move |model, result, ctx| {
                model.apply_conversation_turn_progress(
                    &callback_thread_id,
                    CodexConversationTurnContext {
                        conversation_id,
                        exchange_id,
                        terminal_view_id,
                    },
                    result,
                    ctx,
                );
            },
        );
        true
    }

    fn apply_conversation_turn_progress(
        &mut self,
        active_thread_id: &str,
        conversation_context: CodexConversationTurnContext,
        result: Result<CodexTurnProgress>,
        ctx: &mut ModelContext<Self>,
    ) {
        let (output_text, is_finished, is_error, active_turn) = match result {
            Ok(progress) => {
                let output_text = codex_items_to_agent_text(&progress.items);
                (
                    output_text,
                    progress.active_turn.is_none(),
                    false,
                    progress.active_turn.map(|mut active_turn| {
                        active_turn.conversation_context = Some(conversation_context);
                        active_turn
                    }),
                )
            }
            Err(error) => (
                format!("Codex app-server error: {error:#}"),
                true,
                true,
                None,
            ),
        };

        self.active_turn = active_turn;
        self.local_turn_thread_id = None;
        if self.active_turn.is_none() {
            self.start_active_thread_polling(active_thread_id.to_string(), ctx);
        }

        let update_result = BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
            history.update_codex_exchange_output(
                conversation_context.conversation_id,
                conversation_context.exchange_id,
                output_text.clone(),
                is_finished,
                is_error,
                conversation_context.terminal_view_id,
                ctx,
            )
        });
        if let Err(error) = update_result {
            log::error!("Could not update Codex exchange output: {error:?}");
        }

        if let Some(active_thread) = &mut self.active_thread {
            if active_thread.summary.id == active_thread_id
                && (!output_text.trim().is_empty() || is_error)
            {
                active_thread.items.push(CodexConversationItem {
                    role: if is_error { "error" } else { "codex" }.to_string(),
                    text: output_text,
                });
            }
        }
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
    }

    fn take_started_thread(&mut self, thread_id: &str) -> Option<CodexStartedThread> {
        if should_resume_thread(
            self.started_thread
                .as_ref()
                .map(|started_thread| started_thread.thread_id.as_str()),
            thread_id,
        ) {
            return None;
        }
        self.started_thread.take()
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
        let conversation_context = active_turn.conversation_context;
        let result = active_turn.pending_approval.approval_result(decision);
        self.append_items(
            &thread_id,
            vec![CodexConversationItem {
                role: "approval".to_string(),
                text: format!("{}.", decision.label()),
            }],
        );
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        self.local_turn_thread_id = Some(thread_id.clone());
        let callback_thread_id = thread_id.clone();

        let _ = ctx.spawn(
            async move {
                let mut client = active_turn.client;
                client
                    .respond(active_turn.pending_approval.request_id, result)
                    .await?;
                client.collect_turn_progress(thread_id).await
            },
            move |model, result, ctx| {
                if let Some(conversation_context) = conversation_context {
                    model.apply_conversation_turn_progress(
                        &callback_thread_id,
                        conversation_context,
                        result,
                        ctx,
                    );
                } else {
                    model.apply_turn_progress(&callback_thread_id, result, ctx);
                }
            },
        );
    }

    #[allow(dead_code)]
    pub fn resolve_pending_user_input(
        &mut self,
        question_id: String,
        answer: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(active_turn) = self.active_turn.take() else {
            return;
        };
        let Some(result) = active_turn
            .pending_approval
            .user_input_result(&question_id, &answer)
        else {
            self.active_turn = Some(active_turn);
            return;
        };
        let thread_id = active_turn.thread_id.clone();
        let conversation_context = active_turn.conversation_context;
        self.append_items(
            &thread_id,
            vec![CodexConversationItem {
                role: "user".to_string(),
                text: answer,
            }],
        );
        ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        self.local_turn_thread_id = Some(thread_id.clone());
        let callback_thread_id = thread_id.clone();

        let _ = ctx.spawn(
            async move {
                let mut client = active_turn.client;
                client
                    .respond(active_turn.pending_approval.request_id, result)
                    .await?;
                client.collect_turn_progress(thread_id).await
            },
            move |model, result, ctx| {
                if let Some(conversation_context) = conversation_context {
                    model.apply_conversation_turn_progress(
                        &callback_thread_id,
                        conversation_context,
                        result,
                        ctx,
                    );
                } else {
                    model.apply_turn_progress(&callback_thread_id, result, ctx);
                }
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
        self.local_turn_thread_id = None;
        self.append_items(active_thread_id, items);
        if self.active_turn.is_none() {
            self.start_active_thread_polling(active_thread_id.to_string(), ctx);
        }
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

    fn stop_active_thread_polling(&mut self) {
        self.active_thread_poll_generation = self.active_thread_poll_generation.wrapping_add(1);
    }

    fn start_active_thread_polling(&mut self, thread_id: String, ctx: &mut ModelContext<Self>) {
        self.stop_active_thread_polling();
        let generation = self.active_thread_poll_generation;
        self.schedule_active_thread_poll(thread_id, generation, ctx);
    }

    fn schedule_active_thread_poll(
        &mut self,
        thread_id: String,
        generation: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        let _ = ctx.spawn(
            async move {
                Timer::after(ACTIVE_THREAD_POLL_INTERVAL).await;
                thread_id
            },
            move |model, thread_id, ctx| {
                if model.should_poll_active_thread(&thread_id, generation) {
                    model.poll_active_thread(thread_id, generation, ctx);
                }
            },
        );
    }

    fn should_poll_active_thread(&self, thread_id: &str, generation: u64) -> bool {
        self.active_thread_poll_generation == generation
            && self
                .active_thread
                .as_ref()
                .is_some_and(|thread| thread.summary.id == thread_id)
    }

    fn poll_active_thread(
        &mut self,
        thread_id: String,
        generation: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .active_turn
            .as_ref()
            .is_some_and(|active_turn| active_turn.thread_id == thread_id)
            || self.local_turn_thread_id.as_deref() == Some(thread_id.as_str())
        {
            self.schedule_active_thread_poll(thread_id, generation, ctx);
            return;
        }

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
            .active_thread
            .as_ref()
            .filter(|thread| thread.summary.id == thread_id)
            .map(|thread| thread.summary.clone())
            .or_else(|| {
                self.threads
                    .iter()
                    .find(|thread| thread.id == thread_id)
                    .cloned()
            });
        let read_thread_id = thread_id.clone();

        let _ = ctx.spawn(
            async move { read_thread(&config, &read_thread_id, fallback_summary).await },
            move |model, result, ctx| {
                if !model.should_poll_active_thread(&thread_id, generation) {
                    return;
                }
                match result {
                    Ok(detail) => model.apply_polled_thread_detail(detail, ctx),
                    Err(error) => {
                        log::warn!("Codex app-server thread/read poll failed: {error:#}");
                    }
                }
                if model.should_poll_active_thread(&thread_id, generation) {
                    model.schedule_active_thread_poll(thread_id, generation, ctx);
                }
            },
        );
    }

    fn apply_polled_thread_detail(
        &mut self,
        detail: CodexThreadDetail,
        ctx: &mut ModelContext<Self>,
    ) {
        let thread_id = detail.summary.id.clone();
        let summary_changed = upsert_thread_summary(&mut self.threads, detail.summary.clone());
        if !should_apply_polled_thread_detail(self.active_thread.as_ref(), &detail) {
            if summary_changed {
                ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
            }
            return;
        }

        let active_thread_changed = self.active_thread.as_ref() != Some(&detail);
        if active_thread_changed {
            if let Some(conversation_id) =
                self.conversation_id_by_thread_id.get(&thread_id).copied()
            {
                let conversation = codex_thread_detail_to_ai_conversation(conversation_id, &detail);
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    history.refresh_cached_codex_conversation(conversation, ctx);
                });
            }
            self.active_thread = Some(detail);
            ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        }
        if summary_changed {
            ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
        }
    }

    fn cache_thread_detail_as_conversation(
        &mut self,
        conversation_id: AIConversationId,
        detail: &CodexThreadDetail,
        ctx: &mut ModelContext<Self>,
    ) {
        let thread_id = detail.summary.id.clone();
        let conversation = codex_thread_detail_to_ai_conversation(conversation_id, detail);
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, _ctx| {
            history.cache_external_conversation(conversation);
        });
        self.conversation_id_by_thread_id
            .insert(thread_id.clone(), conversation_id);
        self.thread_id_by_conversation_id
            .insert(conversation_id, thread_id);
    }

    fn remove_thread_locally(&mut self, thread_id: &str, ctx: &mut ModelContext<Self>) {
        self.threads.retain(|thread| thread.id != thread_id);

        if self.opening_thread_id.as_deref() == Some(thread_id) {
            self.opening_thread_id = None;
        }
        if self.local_turn_thread_id.as_deref() == Some(thread_id) {
            self.local_turn_thread_id = None;
        }
        if self
            .active_turn
            .as_ref()
            .is_some_and(|turn| turn.thread_id == thread_id)
        {
            self.active_turn = None;
        }
        if self
            .active_thread
            .as_ref()
            .is_some_and(|thread| thread.summary.id == thread_id)
        {
            self.stop_active_thread_polling();
            self.active_thread = None;
            ctx.emit(CodexAppServerModelEvent::ActiveThreadChanged);
        }

        if let Some(conversation_id) = self.conversation_id_by_thread_id.remove(thread_id) {
            self.thread_id_by_conversation_id.remove(&conversation_id);
            let terminal_view_id = ActiveAgentViewsModel::as_ref(ctx)
                .get_terminal_view_id_for_conversation(conversation_id, ctx);
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                history.delete_conversation(conversation_id, terminal_view_id, ctx);
            });
        }

        ctx.emit(CodexAppServerModelEvent::ThreadsChanged);
    }

    fn collaboration_mode_model_id(&self) -> Option<String> {
        self.selected_model_id.clone().or_else(|| {
            self.models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| self.models.first())
                .map(|model| model.id.clone())
        })
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
) -> Result<CodexRefreshSnapshot> {
    let mut last_error = None;
    for (index, delay) in RECONNECT_BACKOFF.into_iter().enumerate() {
        if index > 0 {
            Timer::after(delay).await;
        }
        match async {
            health_check(config).await?;
            let threads = list_project_threads(
                config,
                project_roots.clone(),
                imported_project_paths.clone(),
                imported_thread_ids.clone(),
            )
            .await?;
            let models = match list_models(config).await {
                Ok(models) => models,
                Err(error) => {
                    log::warn!("Codex app-server model/list failed: {error:#}");
                    Vec::new()
                }
            };
            Ok(CodexRefreshSnapshot { threads, models })
        }
        .await
        {
            Ok(snapshot) => return Ok(snapshot),
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

async fn list_models(config: &CodexAppServerConfig) -> Result<Vec<CodexModelInfo>> {
    let mut by_id = BTreeMap::new();
    let mut cursor = Value::Null;
    loop {
        let result = json_rpc_request(
            config,
            "model/list",
            json!({
                "cursor": cursor,
                "limit": 100,
            }),
        )
        .await?;
        for model in parse_model_list(&result) {
            by_id.insert(model.id.clone(), model);
        }
        cursor = result.get("nextCursor").cloned().unwrap_or(Value::Null);
        if cursor.is_null() {
            break;
        }
    }
    Ok(by_id.into_values().collect())
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

fn upsert_thread_summary(
    threads: &mut Vec<CodexThreadSummary>,
    summary: CodexThreadSummary,
) -> bool {
    if let Some(existing) = threads.iter_mut().find(|thread| thread.id == summary.id) {
        if *existing == summary {
            false
        } else {
            *existing = summary;
            true
        }
    } else {
        threads.insert(0, summary);
        true
    }
}

fn should_resume_thread(started_thread_id: Option<&str>, thread_id: &str) -> bool {
    started_thread_id != Some(thread_id)
}

fn should_apply_polled_thread_detail(
    current: Option<&CodexThreadDetail>,
    next: &CodexThreadDetail,
) -> bool {
    !next.items.is_empty() || current.is_none_or(|current| current.items.is_empty())
}

fn parse_codex_prompt_request(prompt: &str) -> Option<CodexPromptRequest> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return None;
    }

    strip_codex_command(prompt, "/plan")
        .or_else(|| strip_codex_command(prompt, "/codex-plan"))
        .map(|prompt| CodexPromptRequest {
            prompt: prompt.trim_start().to_string(),
            collaboration_mode: Some(CodexPromptCollaborationMode::Plan),
        })
        .or_else(|| {
            Some(CodexPromptRequest {
                prompt: prompt.to_string(),
                collaboration_mode: None,
            })
        })
}

fn strip_codex_command<'a>(prompt: &'a str, command: &str) -> Option<&'a str> {
    let rest = prompt.strip_prefix(command)?;
    if rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

fn display_codex_prompt_request(request: &CodexPromptRequest) -> String {
    match request.collaboration_mode {
        Some(CodexPromptCollaborationMode::Plan) if request.prompt.is_empty() => {
            "/plan".to_string()
        }
        Some(CodexPromptCollaborationMode::Plan) => format!("/plan {}", request.prompt),
        None => request.prompt.clone(),
    }
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

async fn start_thread(
    config: &CodexAppServerConfig,
    model_id: Option<&str>,
    cwd: Option<&PathBuf>,
) -> Result<CodexThreadStart> {
    let mut client = JsonRpcSocket::connect(config).await?;
    client.initialize().await?;
    let result = client
        .request("thread/start", thread_start_params(model_id, cwd))
        .await?;
    Ok(CodexThreadStart {
        detail: parse_thread_detail(&result, None, "new-codex-thread"),
        client,
    })
}

async fn continue_thread(
    config: &CodexAppServerConfig,
    thread_id: &str,
    prompt: &str,
    model_id: Option<&str>,
    collaboration_mode: Option<CodexPromptCollaborationMode>,
    collaboration_mode_model_id: Option<&str>,
    started_thread: Option<CodexStartedThread>,
) -> Result<CodexTurnProgress> {
    let mut client = if let Some(started_thread) = started_thread {
        started_thread.client
    } else {
        let mut client = JsonRpcSocket::connect(config).await?;
        client.initialize().await?;
        let _ = client
            .request("thread/resume", thread_resume_params(thread_id, model_id))
            .await?;
        client
    };
    let _ = client
        .request(
            "turn/start",
            turn_start_params(
                thread_id,
                prompt,
                model_id,
                collaboration_mode,
                collaboration_mode_model_id,
            ),
        )
        .await?;

    client.collect_turn_progress(thread_id.to_string()).await
}

async fn archive_thread(config: &CodexAppServerConfig, thread_id: &str) -> Result<()> {
    let _ = json_rpc_request(config, "thread/archive", thread_archive_params(thread_id)).await?;
    Ok(())
}

fn thread_start_params(model_id: Option<&str>, cwd: Option<&PathBuf>) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("approvalPolicy".to_string(), json!(CODEX_APPROVAL_POLICY));
    params.insert(
        "approvalsReviewer".to_string(),
        json!(CODEX_APPROVALS_REVIEWER),
    );
    if let Some(model_id) = model_id {
        params.insert("model".to_string(), json!(model_id));
    }
    if let Some(cwd) = cwd.and_then(|cwd| cwd.to_str()) {
        params.insert("cwd".to_string(), json!(cwd));
    }
    Value::Object(params)
}

fn thread_resume_params(thread_id: &str, model_id: Option<&str>) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("threadId".to_string(), json!(thread_id));
    params.insert("approvalPolicy".to_string(), json!(CODEX_APPROVAL_POLICY));
    params.insert(
        "approvalsReviewer".to_string(),
        json!(CODEX_APPROVALS_REVIEWER),
    );
    if let Some(model_id) = model_id {
        params.insert("model".to_string(), json!(model_id));
    }
    Value::Object(params)
}

fn thread_archive_params(thread_id: &str) -> Value {
    json!({ "threadId": thread_id })
}

fn turn_start_params(
    thread_id: &str,
    prompt: &str,
    model_id: Option<&str>,
    collaboration_mode: Option<CodexPromptCollaborationMode>,
    collaboration_mode_model_id: Option<&str>,
) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("threadId".to_string(), json!(thread_id));
    params.insert("approvalPolicy".to_string(), json!(CODEX_APPROVAL_POLICY));
    params.insert(
        "approvalsReviewer".to_string(),
        json!(CODEX_APPROVALS_REVIEWER),
    );
    if let Some(model_id) = model_id {
        params.insert("model".to_string(), json!(model_id));
    }
    if let Some(collaboration_mode) = collaboration_mode {
        if let Some(model_id) = collaboration_mode_model_id.or(model_id) {
            params.insert(
                "collaborationMode".to_string(),
                codex_collaboration_mode_params(collaboration_mode, model_id),
            );
        }
    }
    params.insert(
        "input".to_string(),
        json!([
            {
                "type": "text",
                "text": prompt,
            }
        ]),
    );
    Value::Object(params)
}

fn codex_collaboration_mode_params(
    collaboration_mode: CodexPromptCollaborationMode,
    model_id: &str,
) -> Value {
    match collaboration_mode {
        CodexPromptCollaborationMode::Plan => json!({
            "mode": "plan",
            "settings": {
                "model": model_id,
                "developer_instructions": null,
                "reasoning_effort": null,
            },
        }),
    }
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
    let mut command_output_buffer = String::new();
    let mut last_command: Option<String> = None;

    for item in items.iter().filter(|item| !is_codex_user_item(&item.role)) {
        if item.text.trim().is_empty() {
            continue;
        }

        if is_codex_assistant_delta_item(&item.role) {
            assistant_delta_buffer.push_str(&item.text);
            continue;
        }

        if let Some((command, output)) = codex_completed_command_and_output(item) {
            if last_command
                .as_ref()
                .is_some_and(|last| normalized_codex_text(last) == normalized_codex_text(&command))
            {
                if let Some(output) = output {
                    flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
                    flush_codex_command_output_buffer(&mut parts, &mut command_output_buffer);
                    push_codex_command_output_part(&mut parts, &output, Some(&command));
                }
                continue;
            }

            flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
            flush_codex_command_output_buffer(&mut parts, &mut command_output_buffer);
            if let Some(output) = output {
                push_codex_command_output_part(&mut parts, &output, Some(&command));
            }
            continue;
        }

        if is_codex_completed_item(&item.role)
            && last_command.as_ref().is_some_and(|command| {
                normalized_codex_text(command) == normalized_codex_text(&item.text)
            })
        {
            continue;
        }

        if let Some(command) = codex_command_text_from_item(item) {
            flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
            flush_codex_command_output_buffer(&mut parts, &mut command_output_buffer);
            parts.push(format!(
                "commandExecution: {}",
                format_codex_command_for_agent_text(&command)
            ));
            last_command = Some(command);
            continue;
        }

        if is_codex_command_output_delta_item(&item.role) {
            if let Some(output) = codex_command_output_text(&item.text) {
                flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
                command_output_buffer.push_str(&output);
            }
            continue;
        }

        if is_codex_completed_item(&item.role) && !assistant_delta_buffer.trim().is_empty() {
            if normalized_codex_text(&assistant_delta_buffer) == normalized_codex_text(&item.text) {
                continue;
            }
        }

        flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
        flush_codex_command_output_buffer(&mut parts, &mut command_output_buffer);

        if is_codex_assistant_item(&item.role) || is_codex_completed_item(&item.role) {
            parts.push(item.text.clone());
        } else {
            parts.push(format!("{}: {}", item.role, item.text));
        }
    }

    flush_codex_assistant_delta_buffer(&mut parts, &mut assistant_delta_buffer);
    flush_codex_command_output_buffer(&mut parts, &mut command_output_buffer);
    parts.join("\n\n")
}

fn flush_codex_assistant_delta_buffer(parts: &mut Vec<String>, buffer: &mut String) {
    if !buffer.trim().is_empty() {
        parts.push(buffer.clone());
    }
    buffer.clear();
}

fn flush_codex_command_output_buffer(parts: &mut Vec<String>, buffer: &mut String) {
    if !buffer.trim().is_empty() {
        push_codex_command_output_part(parts, buffer, None);
    }
    buffer.clear();
}

fn push_codex_command_output_part(parts: &mut Vec<String>, output: &str, command: Option<&str>) {
    let output = match command {
        Some(command) => serde_json::json!({
            "command": command,
            "output": output,
        })
        .to_string(),
        None => serde_json::to_string(output).unwrap_or_else(|_| format!("{:?}", output)),
    };
    parts.push(format!("commandExecution/output: {output}"));
}

fn format_codex_command_for_agent_text(command: &str) -> String {
    if command.contains('\n') || command.contains('\r') {
        serde_json::to_string(command).unwrap_or_else(|_| format!("{:?}", command))
    } else {
        command.to_string()
    }
}

fn normalized_codex_text(text: &str) -> String {
    text.split_whitespace().collect::<String>()
}

fn normalized_codex_role(role: &str) -> String {
    role.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
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

fn codex_command_text_from_item(item: &CodexConversationItem) -> Option<String> {
    if is_codex_command_output_delta_item(&item.role) {
        return None;
    }

    let role = normalized_codex_role(&item.role);
    let is_command_role =
        role.contains("commandexecution") || role == "itemstarted" || role.ends_with("started");
    if !is_command_role {
        return None;
    }

    let command = codex_command_text(&item.text)?;
    if role.contains("commandexecution") || looks_like_codex_shell_command(&command) {
        Some(command)
    } else {
        None
    }
}

fn codex_command_text(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| command_field(&value))
        .or_else(|| (!text.trim().is_empty()).then(|| text.trim().to_string()))
}

fn codex_completed_command_and_output(
    item: &CodexConversationItem,
) -> Option<(String, Option<String>)> {
    if !is_codex_completed_item(&item.role) {
        return None;
    }
    let value = serde_json::from_str::<Value>(&item.text).ok()?;
    let command = command_field(&value)?;
    let output = codex_command_output_text_from_value(&value);
    Some((command, output))
}

fn looks_like_codex_shell_command(command: &str) -> bool {
    let command = command.trim();
    command.starts_with("/bin/")
        || command.contains(" -lc ")
        || command.starts_with("git ")
        || command.starts_with("cargo ")
        || command.starts_with("gh ")
        || command.starts_with("rg ")
        || command.starts_with("sed ")
        || command.starts_with("nl ")
        || command.starts_with("ls ")
        || command.contains(" && ")
        || command.contains(" | ")
}

fn is_codex_command_output_delta_item(role: &str) -> bool {
    let role = normalized_codex_role(role);
    role.contains("commandexecution") && role.contains("output")
}

fn codex_command_output_text(text: &str) -> Option<String> {
    let output = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| codex_command_output_text_from_value(&value))
        .unwrap_or_else(|| text.to_string());
    (!output.is_empty()).then_some(output)
}

fn codex_command_output_text_from_value(value: &Value) -> Option<String> {
    string_field(
        value,
        &[
            "body",
            "output",
            "aggregatedOutput",
            "aggregated_output",
            "delta",
            "text",
            "content",
            "message",
        ],
    )
    .or_else(|| {
        value
            .get("delta")
            .and_then(codex_command_output_text_from_value)
    })
    .or_else(|| {
        value
            .get("event")
            .and_then(codex_command_output_text_from_value)
    })
    .or_else(|| {
        value
            .get("item")
            .and_then(codex_command_output_text_from_value)
    })
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
        let _ = self.request("initialize", initialize_params()).await?;
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
                    capture_raw_approval_request(text);
                    return Ok(CodexTurnProgress {
                        items,
                        active_turn: Some(CodexActiveTurn {
                            thread_id,
                            client: self,
                            pending_approval: approval,
                            conversation_context: None,
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

fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "Slipstream",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "experimentalApi": true,
        },
    })
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

fn parse_model_list(result: &Value) -> Vec<CodexModelInfo> {
    let values = result
        .get("models")
        .or_else(|| result.get("items"))
        .or_else(|| result.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| result.as_array().cloned().unwrap_or_default());
    values.iter().filter_map(parse_model_info).collect()
}

fn parse_model_info(value: &Value) -> Option<CodexModelInfo> {
    let id = string_field(value, &["id", "model", "modelId", "model_id"])?;
    let display_name = string_field(value, &["displayName", "display_name", "name", "label"])
        .unwrap_or_else(|| id.clone());
    let is_default = bool_field(value, &["default", "isDefault", "is_default"]);
    Some(CodexModelInfo {
        id,
        display_name,
        is_default,
    })
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
    if let Some(item) = params.get("item") {
        if string_field(item, &["type", "kind"])
            .is_some_and(|item_type| normalize_identifier(&item_type).contains("commandexecution"))
        {
            return Some(CodexConversationItem {
                role: method.unwrap_or("codex").to_string(),
                text: item.to_string(),
            });
        }
    }

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
    let reason = string_field(params, &["reason", "message", "prompt"]).unwrap_or_else(|| {
        if method.contains("commandExecution") {
            "Codex is requesting command approval.".to_string()
        } else if method.contains("fileChange") {
            "Codex is requesting file change approval.".to_string()
        } else {
            "Codex is requesting approval.".to_string()
        }
    });
    let user_input_questions = user_input_questions(params);
    let (available_decisions, decision_wire_values) =
        approval_decisions(params, method, !user_input_questions.is_empty());

    Some(CodexPendingApproval {
        request_id,
        method: method.to_string(),
        thread_id: string_field(params, &["threadId", "thread_id"]),
        turn_id: string_field(params, &["turnId", "turn_id"]),
        item_id: string_field(params, &["itemId", "item_id", "approvalId", "approval_id"]),
        reason,
        command: command_field(params),
        cwd: string_field(params, &["cwd", "workingDirectory", "working_directory"]),
        available_decisions,
        user_input_questions,
        decision_wire_values,
    })
}

fn approval_decisions(
    params: &Value,
    method: &str,
    has_user_input_questions: bool,
) -> (
    Vec<CodexApprovalDecision>,
    HashMap<CodexApprovalDecision, Value>,
) {
    let mut decision_wire_values = HashMap::new();
    let mut decisions = Vec::new();
    let mut wire_values = Vec::new();
    collect_approval_decision_values(params, &mut wire_values);
    for wire_value in wire_values {
        let Some(decision) = approval_decision_from_wire_value(&wire_value) else {
            continue;
        };
        if !decisions.contains(&decision) {
            decisions.push(decision);
        }
        decision_wire_values.entry(decision).or_insert(wire_value);
    }
    if !decisions.is_empty() {
        return (decisions, decision_wire_values);
    }

    if is_request_user_input_method(method) && has_user_input_questions {
        (vec![], HashMap::new())
    } else {
        (default_approval_decisions(method), HashMap::new())
    }
}

fn user_input_questions(params: &Value) -> Vec<CodexUserInputQuestion> {
    let mut questions = Vec::new();
    collect_user_input_questions(params, &mut questions);
    let mut seen = BTreeSet::new();
    questions.retain(|question| {
        let options = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>()
            .join("\u{1f}");
        seen.insert(format!(
            "{}\u{1e}{}\u{1e}{}",
            question.header, question.question, options
        ))
    });
    questions
}

fn collect_user_input_questions(value: &Value, questions: &mut Vec<CodexUserInputQuestion>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_user_input_questions(value, questions);
            }
        }
        Value::Object(map) => {
            if let Some(question) = user_input_question(value, questions.len()) {
                questions.push(question);
            }

            for key in [
                "params",
                "input",
                "inputItems",
                "input_items",
                "questions",
                "request",
                "tool",
                "toolCall",
                "tool_call",
                "item",
                "event",
                "data",
                "payload",
                "arguments",
                "args",
                "content",
                "body",
            ] {
                if let Some(child) = map.get(key) {
                    collect_user_input_questions(child, questions);
                }
            }
        }
        Value::String(value) => {
            if let Some(parsed) = parse_embedded_json(value) {
                collect_user_input_questions(&parsed, questions);
            }
        }
        _ => {}
    }
}

fn user_input_question(value: &Value, fallback_index: usize) -> Option<CodexUserInputQuestion> {
    let has_options = value_has_array_field(value, &["options", "choices", "buttons", "answers"]);
    let has_question_flags =
        value_has_any_field(value, &["isOther", "is_other", "isSecret", "is_secret"]);
    let question = string_field(value, &["question", "prompt"])
        .or_else(|| {
            (has_options || has_question_flags)
                .then(|| string_field(value, &["message", "body", "text"]))?
        })
        .unwrap_or_default();
    if question.trim().is_empty() && !has_options && !has_question_flags {
        return None;
    }

    let explicit_id = string_field(value, &["id", "questionId", "question_id", "key", "name"]);
    let answer_index = explicit_id.is_none().then_some(fallback_index);
    let id = explicit_id.unwrap_or_else(|| format!("question-{}", fallback_index + 1));

    Some(CodexUserInputQuestion {
        id,
        header: string_field(value, &["header", "title", "label", "name"]).unwrap_or_default(),
        question,
        is_other: bool_field(value, &["isOther", "is_other"]),
        is_secret: bool_field(value, &["isSecret", "is_secret"]),
        options: user_input_question_options(value),
        answer_index,
    })
}

fn user_input_question_options(value: &Value) -> Vec<CodexUserInputQuestionOption> {
    ["options", "choices", "buttons", "answers"]
        .iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_array))
        .flatten()
        .filter_map(user_input_question_option)
        .collect()
}

fn user_input_question_option(value: &Value) -> Option<CodexUserInputQuestionOption> {
    match value {
        Value::String(label) if !label.trim().is_empty() => Some(CodexUserInputQuestionOption {
            label: label.clone(),
            description: String::new(),
        }),
        Value::Number(label) => Some(CodexUserInputQuestionOption {
            label: label.to_string(),
            description: String::new(),
        }),
        Value::Object(_) => Some(CodexUserInputQuestionOption {
            label: string_field(value, &["label", "title", "text", "value", "name", "id"])?,
            description: string_field(value, &["description", "subtitle", "detail", "details"])
                .unwrap_or_default(),
        }),
        _ => None,
    }
}

fn collect_approval_decision_values(value: &Value, decisions: &mut Vec<Value>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_approval_decision_values(value, decisions);
            }
        }
        Value::Object(map) => {
            for key in ["availableDecisions", "available_decisions"] {
                if let Some(value) = map.get(key) {
                    collect_decision_value(value, decisions);
                }
            }

            for key in [
                "params",
                "request",
                "tool",
                "toolCall",
                "tool_call",
                "item",
                "event",
                "data",
                "payload",
                "arguments",
                "args",
                "content",
                "body",
            ] {
                if let Some(child) = map.get(key) {
                    collect_approval_decision_values(child, decisions);
                }
            }
        }
        Value::String(value) => {
            if let Some(parsed) = parse_embedded_json(value) {
                collect_approval_decision_values(&parsed, decisions);
            }
        }
        _ => {}
    }
}

fn collect_decision_value(value: &Value, decisions: &mut Vec<Value>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_decision_value(value, decisions);
            }
        }
        Value::Object(_) => {
            if let Some(decision) = string_field(value, &["decision", "action", "value", "id"]) {
                decisions.push(json!(decision));
            } else {
                decisions.push(value.clone());
            }
        }
        Value::String(value) if !value.trim().is_empty() => decisions.push(json!(value)),
        _ => {}
    }
}

fn approval_decision_from_wire_value(value: &Value) -> Option<CodexApprovalDecision> {
    match value {
        Value::String(value) => CodexApprovalDecision::from_wire(value),
        Value::Object(map) => {
            if map.contains_key("acceptWithExecpolicyAmendment") {
                Some(CodexApprovalDecision::AcceptForPrefix)
            } else if map.contains_key("applyNetworkPolicyAmendment") {
                Some(CodexApprovalDecision::AcceptForSession)
            } else {
                string_field(value, &["decision", "action", "value", "id"])
                    .and_then(|value| CodexApprovalDecision::from_wire(&value))
            }
        }
        _ => None,
    }
}

fn default_approval_decisions(method: &str) -> Vec<CodexApprovalDecision> {
    if is_request_user_input_method(method) {
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
    if let Some(execve) = value.get("execve") {
        let program = string_field(execve, &["program"]);
        let argv = execve
            .get("argv")
            .and_then(Value::as_array)
            .map(|argv| {
                argv.iter()
                    .filter_map(|part| match part {
                        Value::String(value) => Some(value.clone()),
                        Value::Number(value) => Some(value.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let command = program
            .into_iter()
            .chain(argv)
            .collect::<Vec<_>>()
            .join(" ");
        if !command.trim().is_empty() {
            return Some(command);
        }
    }

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
    let normalized = normalize_identifier(method);
    normalized.contains("requestapproval")
        || normalized.contains("execapprovalrequest")
        || normalized.contains("applypatchapprovalrequest")
        || normalized.contains("requestpermissions")
        || normalized.contains("requestuserinput")
}

fn is_request_user_input_method(method: &str) -> bool {
    normalize_identifier(method).contains("requestuserinput")
}

fn is_generic_codex_approval_reason(reason: &str) -> bool {
    matches!(
        normalize_identifier(reason).as_str(),
        "codexisrequestingapproval"
            | "codexisrequestingcommandapproval"
            | "codexisrequestingfilechangeapproval"
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
            "text", "delta", "message", "output", "body", "content", "plan", "command", "error",
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

fn value_has_any_field(value: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| value.get(*key).is_some())
}

fn value_has_array_field(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .any(|key| value.get(*key).and_then(Value::as_array).is_some())
}

fn parse_embedded_json(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn bool_field(value: &Value, keys: &[&str]) -> bool {
    for key in keys {
        match value.get(*key) {
            Some(Value::Bool(value)) => return *value,
            Some(Value::String(value)) => match value.as_str() {
                "true" => return true,
                "false" => return false,
                _ => {}
            },
            _ => {}
        }
    }
    false
}

fn capture_raw_approval_request(raw_message: &str) {
    let Some(path) = codex_approval_capture_path() else {
        return;
    };
    let message = serde_json::from_str::<Value>(raw_message)
        .unwrap_or_else(|_| Value::String(raw_message.to_string()));
    let record = json!({
        "captured_at": Utc::now().to_rfc3339(),
        "message": message,
    });

    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!(
                "Could not create Codex approval capture directory {}: {error}",
                parent.display()
            );
            return;
        }
    }

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write as _;

            if let Err(error) = writeln!(file, "{record}") {
                log::warn!(
                    "Could not write Codex approval capture {}: {error}",
                    path.display()
                );
            }
        }
        Err(error) => {
            log::warn!(
                "Could not open Codex approval capture {}: {error}",
                path.display()
            );
        }
    }
}

fn codex_approval_capture_path() -> Option<PathBuf> {
    let value = std::env::var("SLIPSTREAM_CODEX_CAPTURE_APPROVALS").ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() || matches!(trimmed, "0" | "false" | "FALSE" | "off" | "OFF") {
        return None;
    }
    if matches!(trimmed, "1" | "true" | "TRUE" | "on" | "ON") {
        return Some(PathBuf::from(
            "/tmp/slipstream-codex-approval-requests.jsonl",
        ));
    }
    Some(PathBuf::from(trimmed))
}

#[cfg(test)]
mod pending_approval_e2e_tests;

#[cfg(test)]
mod snapshot_e2e_tests;

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
    fn parses_codex_model_list_from_common_shapes() {
        let payload = json!({
            "models": [
                {
                    "id": "gpt-5.4",
                    "displayName": "GPT-5.4",
                    "isDefault": true
                },
                {
                    "id": "gpt-5.4-codex",
                    "name": "GPT-5.4 Codex"
                }
            ]
        });

        let models = parse_model_list(&payload);

        assert_eq!(
            models,
            vec![
                CodexModelInfo {
                    id: "gpt-5.4".to_string(),
                    display_name: "GPT-5.4".to_string(),
                    is_default: true,
                },
                CodexModelInfo {
                    id: "gpt-5.4-codex".to_string(),
                    display_name: "GPT-5.4 Codex".to_string(),
                    is_default: false,
                },
            ]
        );
    }

    #[test]
    fn codex_model_label_stays_codex_until_user_selects_override() {
        let mut model = CodexAppServerModel {
            status: CodexAppServerStatus::Connected,
            threads: Vec::new(),
            active_thread: None,
            active_turn: None,
            local_turn_thread_id: None,
            models: vec![CodexModelInfo {
                id: "gpt-5.4-codex".to_string(),
                display_name: "GPT-5.4 Codex".to_string(),
                is_default: true,
            }],
            selected_model_id: None,
            project_roots: Vec::new(),
            opening_thread_id: None,
            active_thread_poll_generation: 0,
            started_thread: None,
            conversation_id_by_thread_id: HashMap::new(),
            thread_id_by_conversation_id: HashMap::new(),
        };

        assert_eq!(model.selected_model_display_name(), "codex");

        model.selected_model_id = Some("gpt-5.4-codex".to_string());

        assert_eq!(model.selected_model_display_name(), "GPT-5.4 Codex");
    }

    #[test]
    fn codex_request_params_include_model_only_when_selected() {
        assert_eq!(
            thread_resume_params("thread-1", None),
            json!({
                "threadId": "thread-1",
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
            })
        );
        assert_eq!(
            thread_resume_params("thread-1", Some("gpt-5.4-codex")),
            json!({
                "threadId": "thread-1",
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
                "model": "gpt-5.4-codex",
            })
        );
        assert_eq!(
            turn_start_params("thread-1", "hello", Some("gpt-5.4-codex"), None, None),
            json!({
                "threadId": "thread-1",
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
                "model": "gpt-5.4-codex",
                "input": [{
                    "type": "text",
                    "text": "hello",
                }],
            })
        );
    }

    #[test]
    fn codex_initialize_declares_experimental_api_capability() {
        assert_eq!(
            initialize_params(),
            json!({
                "clientInfo": {
                    "name": "Slipstream",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": true,
                },
            })
        );
    }

    #[test]
    fn parses_codex_plan_prompt_commands() {
        assert_eq!(
            parse_codex_prompt_request("/plan inspect the diff"),
            Some(CodexPromptRequest {
                prompt: "inspect the diff".to_string(),
                collaboration_mode: Some(CodexPromptCollaborationMode::Plan),
            })
        );
        assert_eq!(
            parse_codex_prompt_request("/codex-plan\ninspect the diff"),
            Some(CodexPromptRequest {
                prompt: "inspect the diff".to_string(),
                collaboration_mode: Some(CodexPromptCollaborationMode::Plan),
            })
        );
        assert_eq!(
            parse_codex_prompt_request("/planetary work")
                .unwrap()
                .collaboration_mode,
            None
        );
    }

    #[test]
    fn codex_plan_turn_params_include_collaboration_mode() {
        assert_eq!(
            turn_start_params(
                "thread-1",
                "inspect the diff",
                None,
                Some(CodexPromptCollaborationMode::Plan),
                Some("gpt-5.4-codex"),
            ),
            json!({
                "threadId": "thread-1",
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
                "collaborationMode": {
                    "mode": "plan",
                    "settings": {
                        "model": "gpt-5.4-codex",
                        "developer_instructions": null,
                        "reasoning_effort": null,
                    },
                },
                "input": [{
                    "type": "text",
                    "text": "inspect the diff",
                }],
            })
        );
    }

    #[test]
    fn first_turn_on_newly_started_thread_does_not_resume() {
        assert!(!should_resume_thread(Some("thread-1"), "thread-1"));
        assert!(should_resume_thread(Some("thread-2"), "thread-1"));
        assert!(should_resume_thread(None, "thread-1"));
    }

    #[test]
    fn polled_empty_thread_detail_does_not_replace_existing_items() {
        let current = CodexThreadDetail {
            summary: CodexThreadSummary {
                id: "thread-1".to_string(),
                title: "Existing thread".to_string(),
                cwd: None,
                updated_at: None,
                source_kind: None,
            },
            items: vec![CodexConversationItem {
                role: "agentMessage".to_string(),
                text: "Existing response".to_string(),
            }],
        };
        let next = CodexThreadDetail {
            summary: current.summary.clone(),
            items: Vec::new(),
        };

        assert!(!should_apply_polled_thread_detail(Some(&current), &next));
        assert!(should_apply_polled_thread_detail(None, &next));
    }

    #[test]
    fn codex_thread_start_params_include_current_project_cwd() {
        assert_eq!(
            thread_start_params(Some("gpt-5.4-codex"), Some(&PathBuf::from("/tmp/project"))),
            json!({
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
                "model": "gpt-5.4-codex",
                "cwd": "/tmp/project",
            })
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
        assert!(approval.user_input_questions.is_empty());
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
    fn approval_decision_result_uses_app_server_payload_shape() {
        let payload = json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "item-1",
        });
        let approval = parse_approval_request(
            Some("item/commandExecution/requestApproval"),
            Some(JsonRpcId::Number(99)),
            Some(&payload),
        )
        .unwrap();

        assert_eq!(
            approval.approval_result(CodexApprovalDecision::AcceptForSession),
            json!({ "decision": "acceptForSession" })
        );
    }

    #[test]
    fn approval_decision_result_preserves_new_codex_wire_values() {
        let payload = json!({
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "approval_id": "approval-1",
            "execve": {
                "program": "cargo",
                "argv": ["test", "-p", "warp"]
            },
            "available_decisions": [
                "approve",
                "approve_for_session",
                "approve_for_prefix",
                "decline",
                "cancel"
            ]
        });
        let approval = parse_approval_request(
            Some("exec_approval_request"),
            Some(JsonRpcId::Number(99)),
            Some(&payload),
        )
        .unwrap();

        assert_eq!(approval.item_id.as_deref(), Some("approval-1"));
        assert_eq!(approval.command.as_deref(), Some("cargo test -p warp"));
        assert_eq!(
            approval.available_decisions,
            vec![
                CodexApprovalDecision::Accept,
                CodexApprovalDecision::AcceptForSession,
                CodexApprovalDecision::AcceptForPrefix,
                CodexApprovalDecision::Decline,
                CodexApprovalDecision::Cancel,
            ]
        );
        assert_eq!(
            approval.approval_result(CodexApprovalDecision::AcceptForSession),
            json!({ "decision": "approve_for_session" })
        );
    }

    #[test]
    fn parses_request_user_input_questions() {
        let payload = json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "call-1",
            "questions": [{
                "id": "approval",
                "header": "Approve plan?",
                "question": "Should Codex continue with this plan?",
                "options": [
                    { "label": "Approve", "description": "Continue" },
                    { "label": "Decline", "description": "Stop" }
                ]
            }]
        });
        let approval = parse_approval_request(
            Some("item/tool/requestUserInput"),
            Some(JsonRpcId::Number(42)),
            Some(&payload),
        )
        .unwrap();

        assert!(approval.available_decisions.is_empty());
        assert_eq!(approval.title(), "Approve plan?");
        assert_eq!(approval.message(), "Should Codex continue with this plan?");
        assert_eq!(approval.user_input_questions[0].options[0].label, "Approve");
        assert_eq!(
            approval.user_input_result("approval", "Approve"),
            Some(json!({
                "answers": {
                    "approval": {
                        "answers": ["Approve"],
                    },
                },
            }))
        );
    }

    #[test]
    fn parses_idless_request_user_input_questions_from_latest_codex_shape() {
        let payload = json!({
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "questions": [{
                "header": "Prompt Test",
                "question": "What kind of prompt interaction would you like to test?",
                "isOther": true,
                "options": [
                    { "label": "Single choice" },
                    { "label": "Multiple choice" },
                    { "label": "Custom answer" }
                ]
            }]
        });
        let approval = parse_approval_request(
            Some("request_user_input"),
            Some(JsonRpcId::Number(42)),
            Some(&payload),
        )
        .unwrap();

        assert!(approval.available_decisions.is_empty());
        assert_eq!(approval.user_input_questions[0].id, "question-1");
        assert_eq!(approval.title(), "Prompt Test");
        assert_eq!(
            approval.message(),
            "What kind of prompt interaction would you like to test?"
        );
        assert_eq!(
            approval.user_input_result("question-1", "Single choice"),
            Some(json!({
                "answers": [{
                    "answers": ["Single choice"],
                }],
            }))
        );
    }

    #[test]
    fn request_user_input_without_questions_falls_back_to_approval_buttons() {
        let payload = json!({
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "reason": "Codex is requesting approval."
        });
        let approval = parse_approval_request(
            Some("request_user_input"),
            Some(JsonRpcId::Number(42)),
            Some(&payload),
        )
        .unwrap();

        assert!(approval.user_input_questions.is_empty());
        assert_eq!(approval.title(), "Codex needs approval");
        assert_eq!(
            approval.message(),
            "Approve Codex to continue with the proposed plan?"
        );
        assert_eq!(
            approval.effective_available_decisions(),
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
    fn command_execution_items_render_as_command_markers_with_output() {
        let text = codex_items_to_agent_text(&[
            CodexConversationItem {
                role: "item/started".to_string(),
                text: "/bin/zsh -lc 'git status --short'".to_string(),
            },
            CodexConversationItem {
                role: "item/commandExecution/outputDelta".to_string(),
                text: json!({
                    "body": " M app/src/codex_app_server/mod.rs\n",
                    "status": "running"
                })
                .to_string(),
            },
            CodexConversationItem {
                role: "item/completed".to_string(),
                text: "/bin/zsh -lc 'git status --short'".to_string(),
            },
            CodexConversationItem {
                role: "item/agentMessage".to_string(),
                text: "Done.".to_string(),
            },
        ]);

        assert_eq!(
            text,
            "commandExecution: /bin/zsh -lc 'git status --short'\n\ncommandExecution/output: \" M app/src/codex_app_server/mod.rs\\n\"\n\nDone."
        );
    }

    #[test]
    fn late_completed_command_execution_update_never_renders_raw_json() {
        let completed_command = json!({
            "aggregatedOutput": " 47M\thomeassistant-config\n976M\t.esphome\n",
            "command": "/bin/zsh -lc 'du -sh homeassistant-config .esphome 2>/dev/null || true'",
            "commandActions": [{
                "command": "du -sh homeassistant-config .esphome 2>/dev/null || true",
                "type": "unknown"
            }],
            "cwd": "/Users/te/dev/esphome",
            "durationMs": 389,
            "exitCode": 0,
            "id": "call_brnPF99vxX7WcYLCQb9J6uju",
            "processId": "45778",
            "source": "unifiedExecStartup",
            "status": "completed",
            "type": "commandExecution"
        });
        let text = codex_items_to_agent_text(&[
            CodexConversationItem {
                role: "item/started".to_string(),
                text: "/bin/zsh -lc 'du -sh homeassistant-config .esphome 2>/dev/null || true'"
                    .to_string(),
            },
            CodexConversationItem {
                role: "item/started".to_string(),
                text: "/bin/zsh -lc 'git status --short'".to_string(),
            },
            CodexConversationItem {
                role: "item/completed".to_string(),
                text: completed_command.to_string(),
            },
        ]);

        assert!(
            !text.contains("aggregatedOutput"),
            "raw commandExecution JSON leaked into agent text: {text}"
        );
        assert_eq!(
            text,
            "commandExecution: /bin/zsh -lc 'du -sh homeassistant-config .esphome 2>/dev/null || true'\n\ncommandExecution: /bin/zsh -lc 'git status --short'\n\ncommandExecution/output: {\"command\":\"/bin/zsh -lc 'du -sh homeassistant-config .esphome 2>/dev/null || true'\",\"output\":\" 47M\\thomeassistant-config\\n976M\\t.esphome\\n\"}"
        );
    }

    #[test]
    fn parses_nested_request_user_input_questions() {
        let payload = json!({
            "request": {
                "questions": [{
                    "id": "approval",
                    "header": "Approve plan?",
                    "question": "Should Codex continue with this plan?",
                    "options": [
                        { "label": "Approve", "description": "Continue" },
                        { "label": "Decline", "description": "Stop" }
                    ]
                }]
            }
        });
        let approval = parse_approval_request(
            Some("item/tool/requestUserInput"),
            Some(JsonRpcId::Number(42)),
            Some(&payload),
        )
        .unwrap();

        assert_eq!(approval.title(), "Approve plan?");
        assert_eq!(approval.user_input_questions[0].options[0].label, "Approve");
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
