#![cfg(not(target_family = "wasm"))]

use std::{collections::BTreeMap, net::TcpListener, path::PathBuf, sync::LazyLock, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use async_process::{Child, Stdio};
use command::r#async::Command;
use futures::stream::AbortHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use warp_core::user_preferences::GetUserPreferences;
use warpui::{r#async::Timer, AppContext, Entity, ModelContext, SingletonEntity};

use crate::{
    server::server_api::{no_cloud_mode_enabled, LOCAL_NO_CLOUD_SERVER_ROOT_URL},
    ChannelState,
};

const PREF_KEY: &str = "LocalMultiAgentConfig";
const BUNDLED_SERVICE_DIR: &str = "bundled/local-multi-agent";
const SERVER_ENTRYPOINT: &str = "dist/server.js";
const CONFIG_SCHEMA_JSON: &str =
    include_str!("../../../tools/local-multi-agent/config-schema.json");
const RESTART_DEBOUNCE: Duration = Duration::from_millis(500);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

static LOCAL_ROOT_URL: LazyLock<parking_lot::RwLock<Option<Url>>> =
    LazyLock::new(|| parking_lot::RwLock::new(None));

#[derive(Debug, Clone, Deserialize)]
struct LocalMultiAgentConfigSchema {
    defaults: LocalMultiAgentConfigSchemaDefaults,
}

#[derive(Debug, Clone, Deserialize)]
struct LocalMultiAgentConfigSchemaDefaults {
    #[serde(rename = "HOST")]
    host: String,
    #[serde(rename = "PORT")]
    port: u16,
    #[serde(rename = "OPENAI_MODEL")]
    openai_model: String,
    #[serde(rename = "LOCAL_MODEL_ALIASES")]
    local_model_aliases: String,
    #[serde(rename = "LOCAL_MODEL_LIST")]
    local_model_list: String,
    #[serde(rename = "LOCAL_ENABLE_TOOLS")]
    local_enable_tools: bool,
    #[serde(rename = "LOCAL_MAX_HISTORY_MESSAGES")]
    local_max_history_messages: u16,
    #[serde(rename = "LOCAL_MODEL_CONTEXT_TOKENS")]
    local_model_context_tokens: String,
    #[serde(rename = "LOCAL_GRAPHQL_DB_PATH")]
    local_graphql_db_path: String,
    #[serde(rename = "LOG_LEVEL")]
    log_level: String,
    #[serde(rename = "LOCAL_SERVICE_LOG_PATH")]
    local_service_log_path: String,
    #[serde(rename = "LOCAL_MULTI_AGENT_SYSTEM_PROMPT")]
    local_multi_agent_system_prompt: String,
}

fn config_schema_defaults() -> LocalMultiAgentConfigSchemaDefaults {
    serde_json::from_str::<LocalMultiAgentConfigSchema>(CONFIG_SCHEMA_JSON)
        .expect("local multi-agent config schema must be valid JSON")
        .defaults
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMultiAgentConfig {
    pub host: String,
    pub port: u16,
    pub openai_model: Option<String>,
    pub local_model_aliases: String,
    pub local_model_list: String,
    pub local_enable_tools: bool,
    pub local_max_history_messages: u16,
    pub local_model_context_tokens: Option<String>,
    pub local_graphql_db_path: Option<String>,
    pub log_level: String,
    pub local_service_log_path: Option<String>,
    pub local_multi_agent_system_prompt: String,
}

impl Default for LocalMultiAgentConfig {
    fn default() -> Self {
        let defaults = config_schema_defaults();
        Self {
            host: defaults.host,
            port: defaults.port,
            openai_model: non_empty(defaults.openai_model),
            local_model_aliases: defaults.local_model_aliases,
            local_model_list: defaults.local_model_list,
            local_enable_tools: defaults.local_enable_tools,
            local_max_history_messages: defaults.local_max_history_messages,
            local_model_context_tokens: non_empty(defaults.local_model_context_tokens),
            local_graphql_db_path: non_empty(defaults.local_graphql_db_path),
            log_level: defaults.log_level,
            local_service_log_path: non_empty(defaults.local_service_log_path),
            local_multi_agent_system_prompt: defaults.local_multi_agent_system_prompt,
        }
    }
}

impl LocalMultiAgentConfig {
    pub fn root_url(&self) -> Result<Url, LocalMultiAgentConfigError> {
        Url::parse(&format!("http://{}:{}", self.host.trim(), self.port))
            .map_err(|source| LocalMultiAgentConfigError::InvalidRootUrl { source })
    }

    pub fn validate(&self) -> Result<(), LocalMultiAgentConfigError> {
        if self.host.trim().is_empty() {
            return Err(LocalMultiAgentConfigError::InvalidHost);
        }
        if self.port == 0 {
            return Err(LocalMultiAgentConfigError::InvalidPort);
        }
        if self.local_max_history_messages < 4 {
            return Err(LocalMultiAgentConfigError::InvalidMaxHistoryMessages);
        }
        if !matches!(
            self.log_level.trim().to_ascii_lowercase().as_str(),
            "error" | "warn" | "info" | "debug"
        ) {
            return Err(LocalMultiAgentConfigError::InvalidLogLevel);
        }
        if !self.local_model_aliases.trim().is_empty() {
            let value: serde_json::Value = serde_json::from_str(&self.local_model_aliases)
                .map_err(|source| LocalMultiAgentConfigError::InvalidModelAliases { source })?;
            if !value.is_object() {
                return Err(LocalMultiAgentConfigError::ModelAliasesMustBeObject);
            }
        }
        if let Some(context_tokens) = &self.local_model_context_tokens {
            validate_context_tokens(context_tokens)?;
        }
        self.root_url()?;
        Ok(())
    }

    pub fn config_hash(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json);
        hex::encode(&hasher.finalize()[..8])
    }

    pub fn env(
        &self,
        openai_api_key: Option<&str>,
        openai_base_url: Option<&str>,
    ) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("HOST".to_string(), self.host.trim().to_string());
        env.insert("PORT".to_string(), self.port.to_string());
        env.insert(
            "LOCAL_ENABLE_TOOLS".to_string(),
            self.local_enable_tools.to_string(),
        );
        env.insert(
            "LOCAL_MAX_HISTORY_MESSAGES".to_string(),
            self.local_max_history_messages.to_string(),
        );
        env.insert("LOG_LEVEL".to_string(), self.log_level.trim().to_string());
        env.insert(
            "LOCAL_MODEL_ALIASES".to_string(),
            self.local_model_aliases.trim().to_string(),
        );
        env.insert(
            "LOCAL_MODEL_LIST".to_string(),
            self.local_model_list.trim().to_string(),
        );
        env.insert(
            "LOCAL_GRAPHQL_DB_PATH".to_string(),
            self.local_graphql_db_path
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(default_graphql_db_path),
        );
        env.insert(
            "LOCAL_SERVICE_LOG_PATH".to_string(),
            self.local_service_log_path
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(default_log_path),
        );
        env.insert(
            "LOCAL_MULTI_AGENT_SYSTEM_PROMPT".to_string(),
            self.local_multi_agent_system_prompt.clone(),
        );
        if let Some(value) = self.openai_model.as_deref().and_then(non_empty_str) {
            env.insert("OPENAI_MODEL".to_string(), value.to_string());
        }
        if let Some(value) = self
            .local_model_context_tokens
            .as_deref()
            .and_then(non_empty_str)
        {
            env.insert("LOCAL_MODEL_CONTEXT_TOKENS".to_string(), value.to_string());
        }
        if let Some(value) = openai_api_key.and_then(non_empty_str) {
            env.insert("OPENAI_API_KEY".to_string(), value.to_string());
        }
        if let Some(value) = openai_base_url.and_then(non_empty_str) {
            env.insert("OPENAI_BASE_URL".to_string(), value.to_string());
        }
        env
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalMultiAgentConfigError {
    #[error("Host is required.")]
    InvalidHost,
    #[error("Port must be between 1 and 65535.")]
    InvalidPort,
    #[error("Maximum history messages must be at least 4.")]
    InvalidMaxHistoryMessages,
    #[error("Log level must be one of error, warn, info, or debug.")]
    InvalidLogLevel,
    #[error("Model aliases must be valid JSON: {source}")]
    InvalidModelAliases { source: serde_json::Error },
    #[error("Model aliases must be a JSON object.")]
    ModelAliasesMustBeObject,
    #[error("Context tokens must be a positive integer or a JSON object.")]
    InvalidContextTokens,
    #[error("Context tokens JSON is invalid: {source}")]
    InvalidContextTokensJson { source: serde_json::Error },
    #[error("Could not build local root URL: {source}")]
    InvalidRootUrl { source: url::ParseError },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMultiAgentStatus {
    Disabled,
    Starting,
    Running {
        root_url: Url,
        pid: Option<u32>,
        config_hash: String,
    },
    Restarting,
    Failed {
        message: String,
    },
}

impl LocalMultiAgentStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Disabled => "Disabled".to_string(),
            Self::Starting => "Starting".to_string(),
            Self::Running { root_url, .. } => format!("Running at {}", root_url.as_str()),
            Self::Restarting => "Restarting".to_string(),
            Self::Failed { message } => format!("Failed: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMultiAgentManagerEvent {
    ConfigChanged,
    StatusChanged,
}

#[derive(Debug, Deserialize)]
struct LocalMultiAgentHealthResponse {
    ok: bool,
    version: Option<String>,
}

pub struct LocalMultiAgentManager {
    config: LocalMultiAgentConfig,
    status: LocalMultiAgentStatus,
    child: Option<Child>,
    restart_debounce: Option<AbortHandle>,
    openai_api_key: Option<String>,
    openai_base_url: Option<String>,
}

impl LocalMultiAgentManager {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let config = load_config(ctx);
        let status = if no_cloud_mode_enabled() {
            LocalMultiAgentStatus::Starting
        } else {
            LocalMultiAgentStatus::Disabled
        };
        if let Ok(root_url) = config.root_url() {
            set_global_root_url(Some(root_url.clone()));
            if no_cloud_mode_enabled() {
                let _ = ChannelState::override_server_root_url(root_url.to_string());
            }
        }

        let mut manager = Self {
            config,
            status,
            child: None,
            restart_debounce: None,
            openai_api_key: None,
            openai_base_url: None,
        };
        if no_cloud_mode_enabled() {
            manager.schedule_restart(ctx);
        }
        manager
    }

    pub fn config(&self) -> &LocalMultiAgentConfig {
        &self.config
    }

    pub fn status(&self) -> &LocalMultiAgentStatus {
        &self.status
    }

    pub fn root_url(&self) -> Option<Url> {
        self.config.root_url().ok()
    }

    pub fn global_root_url() -> Option<Url> {
        LOCAL_ROOT_URL.read().clone()
    }

    pub fn update_provider_config(
        &mut self,
        openai_api_key: Option<String>,
        openai_base_url: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.openai_api_key == openai_api_key && self.openai_base_url == openai_base_url {
            return;
        }
        self.openai_api_key = openai_api_key;
        self.openai_base_url = openai_base_url;
        self.schedule_restart(ctx);
    }

    pub fn set_config(
        &mut self,
        config: LocalMultiAgentConfig,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), LocalMultiAgentConfigError> {
        config.validate()?;
        if self.config == config {
            return Ok(());
        }
        self.config = config;
        save_config(&self.config, ctx);
        if let Ok(root_url) = self.config.root_url() {
            set_global_root_url(Some(root_url.clone()));
            if no_cloud_mode_enabled() {
                let _ = ChannelState::override_server_root_url(root_url.to_string());
            }
        }
        ctx.emit(LocalMultiAgentManagerEvent::ConfigChanged);
        self.schedule_restart(ctx);
        Ok(())
    }

    pub fn restart_with_config(&mut self, ctx: &mut ModelContext<Self>) {
        self.schedule_restart(ctx);
    }

    pub fn health_check(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(root_url) = self.root_url() else {
            self.set_failed("Invalid local service URL.".to_string(), ctx);
            return;
        };
        let current_status = self.status.clone();
        let _ = ctx.spawn(
            async move { health_check(root_url.clone()).await.map(|_| root_url) },
            move |manager, result, ctx| {
                match result {
                    Ok(root_url) => {
                        if !matches!(manager.status, LocalMultiAgentStatus::Running { .. }) {
                            manager.status = current_status;
                        }
                        log::info!("Local multi-agent health check succeeded at {root_url}");
                    }
                    Err(error) => {
                        manager.set_failed(format!("Health check failed: {error:#}"), ctx);
                    }
                }
                ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
            },
        );
    }

    pub fn record_config_error(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        self.status = LocalMultiAgentStatus::Failed { message };
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
    }

    fn schedule_restart(&mut self, ctx: &mut ModelContext<Self>) {
        if !no_cloud_mode_enabled() {
            self.stop_child();
            self.status = LocalMultiAgentStatus::Disabled;
            set_global_root_url(None);
            ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
            return;
        }

        if let Some(handle) = self.restart_debounce.take() {
            handle.abort();
        }

        self.status = LocalMultiAgentStatus::Restarting;
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
        self.restart_debounce = Some(
            ctx.spawn(
                async {
                    Timer::after(RESTART_DEBOUNCE).await;
                },
                |manager, _, ctx| {
                    manager.restart_debounce = None;
                    manager.start_now(ctx);
                },
            )
            .abort_handle(),
        );
    }

    fn start_now(&mut self, ctx: &mut ModelContext<Self>) {
        self.stop_child();
        let config = self.config.clone();
        let openai_api_key = self.openai_api_key.clone();
        let openai_base_url = self.openai_base_url.clone();
        self.status = LocalMultiAgentStatus::Starting;
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);

        let _ = ctx.spawn(
            async move { start_service(config, openai_api_key, openai_base_url).await },
            |manager, result, ctx| {
                match result {
                    Ok(StartServiceResult {
                        child,
                        root_url,
                        pid,
                        config_hash,
                    }) => {
                        manager.child = child;
                        set_global_root_url(Some(root_url.clone()));
                        let _ = ChannelState::override_server_root_url(root_url.to_string());
                        manager.status = LocalMultiAgentStatus::Running {
                            root_url,
                            pid,
                            config_hash,
                        };
                    }
                    Err(error) => manager.set_failed(error.to_string(), ctx),
                }
                ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
            },
        );
    }

    fn stop_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
        }
    }

    fn set_failed(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        self.stop_child();
        self.status = LocalMultiAgentStatus::Failed { message };
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
    }
}

impl Drop for LocalMultiAgentManager {
    fn drop(&mut self) {
        self.stop_child();
    }
}

impl Entity for LocalMultiAgentManager {
    type Event = LocalMultiAgentManagerEvent;
}

impl SingletonEntity for LocalMultiAgentManager {}

struct StartServiceResult {
    child: Option<Child>,
    root_url: Url,
    pid: Option<u32>,
    config_hash: String,
}

async fn start_service(
    config: LocalMultiAgentConfig,
    openai_api_key: Option<String>,
    openai_base_url: Option<String>,
) -> Result<StartServiceResult> {
    config.validate()?;
    let root_url = config.root_url()?;
    let config_hash = config.config_hash();

    match health_check(root_url.clone()).await {
        Ok(()) => {
            log::info!("Reusing existing healthy local multi-agent service at {root_url}");
            return Ok(StartServiceResult {
                child: None,
                root_url,
                pid: None,
                config_hash,
            });
        }
        Err(_) => ensure_port_available(&config)?,
    }

    let service_dir = service_dir().context("Local multi-agent service is not bundled")?;
    let server_js = service_dir.join(SERVER_ENTRYPOINT);
    if !server_js.is_file() {
        bail!(
            "Local multi-agent entrypoint not found at {}. Build or bundle tools/local-multi-agent first.",
            server_js.display()
        );
    }

    let path_env = std::env::var_os("PATH").and_then(|path| path.into_string().ok());
    let node_binary = match node_runtime::find_working_node_binary(path_env.as_deref()).await {
        Some(path) => path,
        None => {
            let client = http_client::Client::new();
            node_runtime::install_npm(&client)
                .await
                .context("Failed to install bundled Node.js runtime")?;
            node_runtime::node_binary_path().context("Failed to locate bundled Node.js runtime")?
        }
    };

    let mut command = Command::new(&node_binary);
    command
        .arg(&server_js)
        .current_dir(&service_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    for (key, value) in config.env(openai_api_key.as_deref(), openai_base_url.as_deref()) {
        command.env(key, value);
    }
    command.env("LOCAL_CONFIG_HASH", &config_hash);

    let child = command
        .spawn()
        .with_context(|| format!("Failed to spawn {}", server_js.display()))?;
    let pid = Some(child.id());

    wait_until_healthy(root_url.clone()).await?;

    Ok(StartServiceResult {
        child: Some(child),
        root_url,
        pid,
        config_hash,
    })
}

async fn wait_until_healthy(root_url: Url) -> Result<()> {
    let started = instant::Instant::now();
    let mut last_error = None;
    while started.elapsed() < Duration::from_secs(10) {
        match health_check(root_url.clone()).await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        Timer::after(Duration::from_millis(150)).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Timed out waiting for local service health check")))
}

async fn health_check(root_url: Url) -> Result<()> {
    let url = root_url
        .join("/health")
        .context("Failed to build local health URL")?;
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .context("request failed")?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status())
    }
    let health = response
        .json::<LocalMultiAgentHealthResponse>()
        .await
        .context("health response was not Warp local service JSON")?;
    if health.ok
        && health
            .version
            .as_deref()
            .is_some_and(|version| !version.is_empty())
    {
        Ok(())
    } else {
        bail!("health response did not identify a Warp local service")
    }
}

fn ensure_port_available(config: &LocalMultiAgentConfig) -> Result<()> {
    match TcpListener::bind((config.host.trim(), config.port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) => bail!(
            "Port {} on {} is already in use and does not look like a healthy Warp local service: {}",
            config.port,
            config.host,
            error
        ),
    }
}

fn service_dir() -> Option<PathBuf> {
    if let Some(resources) = warp_core::paths::bundled_resources_dir() {
        let bundled = resources.join(BUNDLED_SERVICE_DIR);
        if bundled.join(SERVER_ENTRYPOINT).is_file() {
            return Some(bundled);
        }
    }

    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("tools/local-multi-agent");
    dev.join(SERVER_ENTRYPOINT).is_file().then_some(dev)
}

fn default_graphql_db_path() -> String {
    warp_core::paths::data_dir()
        .join("local-multi-agent")
        .join("local-graphql.sqlite")
        .to_string_lossy()
        .into_owned()
}

fn default_log_path() -> String {
    let base = warp_logging::log_directory().unwrap_or_else(|_| warp_core::paths::data_dir());
    base.join("local-multi-agent.log")
        .to_string_lossy()
        .into_owned()
}

fn load_config(ctx: &AppContext) -> LocalMultiAgentConfig {
    match ctx.private_user_preferences().read_value(PREF_KEY) {
        Ok(Some(json)) => match serde_json::from_str(&json) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("Failed to parse local multi-agent config: {error}");
                LocalMultiAgentConfig::default()
            }
        },
        Ok(None) => LocalMultiAgentConfig::default(),
        Err(error) => {
            log::warn!("Failed to read local multi-agent config: {error}");
            LocalMultiAgentConfig::default()
        }
    }
}

fn save_config(config: &LocalMultiAgentConfig, ctx: &AppContext) {
    let Ok(json) = serde_json::to_string(config) else {
        return;
    };
    if let Err(error) = ctx.private_user_preferences().write_value(PREF_KEY, json) {
        log::warn!("Failed to persist local multi-agent config: {error}");
    }
}

fn set_global_root_url(root_url: Option<Url>) {
    *LOCAL_ROOT_URL.write() = root_url;
}

fn non_empty(value: String) -> Option<String> {
    non_empty_str(&value).map(str::to_string)
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn validate_context_tokens(value: &str) -> Result<(), LocalMultiAgentConfigError> {
    let value = value.trim();
    if value.is_empty() || value.parse::<u32>().is_ok_and(|v| v > 0) {
        return Ok(());
    }
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|source| LocalMultiAgentConfigError::InvalidContextTokensJson { source })?;
    if !parsed.is_object() {
        return Err(LocalMultiAgentConfigError::InvalidContextTokens);
    }
    Ok(())
}

pub fn local_no_cloud_root_url() -> Option<Url> {
    LocalMultiAgentManager::global_root_url()
        .or_else(|| Url::parse(LOCAL_NO_CLOUD_SERVER_ROOT_URL).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_schema() {
        let schema = config_schema_defaults();
        let config = LocalMultiAgentConfig::default();
        assert_eq!(config.host, schema.host);
        assert_eq!(config.port, schema.port);
        assert_eq!(config.local_model_aliases, schema.local_model_aliases);
        assert_eq!(config.local_model_list, schema.local_model_list);
        assert_eq!(config.local_enable_tools, schema.local_enable_tools);
        assert_eq!(
            config.local_max_history_messages,
            schema.local_max_history_messages
        );
        assert_eq!(config.log_level, schema.log_level);
    }

    #[test]
    fn config_to_env_uses_defaults_for_paths_and_provider_settings() {
        let config = LocalMultiAgentConfig::default();
        let env = config.env(Some("sk-test"), Some("http://127.0.0.1:11434/v1"));
        assert_eq!(env.get("HOST").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(env.get("PORT").map(String::as_str), Some("8787"));
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-test")
        );
        assert_eq!(
            env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:11434/v1")
        );
        assert!(env
            .get("LOCAL_GRAPHQL_DB_PATH")
            .is_some_and(|v| v.ends_with(".sqlite")));
        assert!(env
            .get("LOCAL_SERVICE_LOG_PATH")
            .is_some_and(|v| v.ends_with(".log")));
    }

    #[test]
    fn validation_rejects_bad_json_settings() {
        let config = LocalMultiAgentConfig {
            local_model_aliases: "[]".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LocalMultiAgentConfigError::ModelAliasesMustBeObject)
        ));

        let config = LocalMultiAgentConfig {
            local_model_context_tokens: Some("not-json".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LocalMultiAgentConfigError::InvalidContextTokensJson { .. })
        ));
    }
}
