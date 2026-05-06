use std::path::PathBuf;

use settings::{macros::define_settings_group, SupportedPlatforms, SyncToCloud};

pub const DEFAULT_CODEX_APP_SERVER_URL: &str = "ws://127.0.0.1:4500";

define_settings_group!(CodexAppServerSettings, settings: [
    enabled: CodexAppServerEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.codex.enabled",
        description: "Whether Codex app-server integration is enabled.",
    },
    server_url: CodexAppServerUrl {
        type: String,
        default: DEFAULT_CODEX_APP_SERVER_URL.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.codex.app_server_url",
        description: "The Codex app-server WebSocket URL.",
    },
    imported_project_paths: CodexImportedProjectPaths {
        type: Vec<PathBuf>,
        default: vec![],
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.codex.imported_project_paths",
        description: "Project paths whose Codex conversations should be shown.",
    },
    imported_thread_ids: CodexImportedThreadIds {
        type: Vec<String>,
        default: vec![],
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.codex.imported_thread_ids",
        description: "Specific Codex thread IDs to show.",
    },
    bearer_token: CodexAppServerBearerToken {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
        description: "Bearer token for connecting to a non-loopback Codex app-server.",
    },
]);
