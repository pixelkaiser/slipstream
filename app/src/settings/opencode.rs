use std::path::PathBuf;

use settings::{macros::define_settings_group, SupportedPlatforms, SyncToCloud};

pub const DEFAULT_OPENCODE_SERVER_URL: &str = "http://127.0.0.1:4096";

define_settings_group!(OpenCodeServerSettings, settings: [
    enabled: OpenCodeServerEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.opencode.enabled",
        description: "Whether OpenCode server conversations are enabled.",
    },
    server_url: OpenCodeServerUrl {
        type: String,
        default: DEFAULT_OPENCODE_SERVER_URL.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.opencode.server_url",
        description: "The OpenCode server HTTP URL.",
    },
    username: OpenCodeServerUsername {
        type: String,
        default: "opencode".to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.opencode.username",
        description: "Username for OpenCode server HTTP basic auth.",
    },
    password: OpenCodeServerPassword {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
        toml_path: "features.opencode.password",
        description: "Password for OpenCode server HTTP basic auth.",
    },
    imported_project_paths: OpenCodeImportedProjectPaths {
        type: Vec<PathBuf>,
        default: vec![],
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "features.opencode.imported_project_paths",
        description: "Project paths whose OpenCode conversations should be shown.",
    },
]);
