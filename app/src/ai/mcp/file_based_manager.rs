use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use itertools::Itertools as _;
use repo_metadata::repositories::DetectedRepositories;
use serde_json::{Map, Value};
use uuid::Uuid;
use warp_core::features::FeatureFlag;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::{FileMCPWatcher, FileMCPWatcherEvent, MCPProvider};
use crate::ai::mcp::parsing::resolve_json;
use crate::ai::mcp::templatable_installation::TemplatableMCPServerInstallation;
use crate::ai::mcp::ParsedTemplatableMCPServerResult;
use crate::settings::ai::AISettings;
use crate::settings::AISettingsChangedEvent;
use crate::warp_managed_paths_watcher::warp_managed_mcp_config_path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileBasedMCPActivationMode {
    ReferenceOnly,
    CopyToWarpConfig,
}

/// Singleton model to manage file-based MCP servers.
#[derive(Default)]
pub struct FileBasedMCPManager {
    /// File-based MCP server installations detected from config files.
    /// Keyed by a consistent hash of the server's name, JSON template, and variable values.
    file_based_servers: HashMap<u64, TemplatableMCPServerInstallation>,
    /// Reverse mapping: logical root path → provider → set of server hashes.
    file_based_servers_by_root: HashMap<PathBuf, HashMap<MCPProvider, HashSet<u64>>>,
    /// Concrete config file path for each `(logical root path, provider)` source.
    config_paths_by_root_provider: HashMap<PathBuf, HashMap<MCPProvider, PathBuf>>,
    /// UUIDs that were actually auto-start requested while parsing each `(root, provider)`.
    /// They are temporarily stored here and removed to emit FileBasedMCPManagerEvent::CloudEnvMcpScanComplete
    pending_scan_auto_started_servers_by_root:
        HashMap<PathBuf, HashMap<MCPProvider, HashSet<Uuid>>>,
    /// File-based servers explicitly activated by the user. Keyed by stable installation UUIDs.
    activated_file_based_server_uuids: HashSet<Uuid>,
}

impl FileBasedMCPManager {
    #[allow(dead_code)]
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        Self::new_with_activated_servers(Vec::new(), ctx)
    }

    pub fn new_with_activated_servers(
        activated_file_based_server_uuids: Vec<Uuid>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        if FeatureFlag::FileBasedMcp.is_enabled() {
            ctx.subscribe_to_model(&FileMCPWatcher::handle(ctx), |me, event, ctx| {
                me.handle_watcher_event(event, ctx);
            });

            ctx.subscribe_to_model(&AISettings::handle(ctx), |me, event, ctx| {
                if matches!(event, AISettingsChangedEvent::FileBasedMcpEnabled { .. }) {
                    me.handle_file_based_mcp_enabled_change(ctx);
                }
            });
        }

        Self {
            file_based_servers: Default::default(),
            file_based_servers_by_root: Default::default(),
            config_paths_by_root_provider: Default::default(),
            pending_scan_auto_started_servers_by_root: Default::default(),
            activated_file_based_server_uuids: activated_file_based_server_uuids
                .into_iter()
                .collect(),
        }
    }

    /// Handle an event from [`FileMCPWatcher`].
    fn handle_watcher_event(&mut self, event: &FileMCPWatcherEvent, ctx: &mut ModelContext<Self>) {
        match event {
            FileMCPWatcherEvent::ConfigParsed {
                root_path,
                config_path,
                provider,
                servers,
            } => {
                self.apply_parsed_servers_from_config(
                    root_path.clone(),
                    *provider,
                    config_path.clone(),
                    servers.clone(),
                    ctx,
                );
            }
            FileMCPWatcherEvent::ConfigRemoved {
                root_path,
                config_path,
                provider,
            } => {
                self.remove_servers_for_root_provider(root_path, *provider, config_path, ctx);
            }
            FileMCPWatcherEvent::CloudEnvMcpScanComplete { repo_path } => {
                self.handle_cloud_environment_scan_complete(repo_path, ctx);
            }
        }
    }

    /// Get file-based MCP servers in scope for the given current working directory.
    pub fn get_servers_for_working_directory(
        &self,
        cwd: &Path,
        app: &AppContext,
    ) -> Vec<&TemplatableMCPServerInstallation> {
        let repo_root = DetectedRepositories::as_ref(app)
            .get_root_for_path(&LocalOrRemotePath::Local(cwd.to_path_buf()))
            .and_then(|r| PathBuf::try_from(r).ok());
        let candidate_roots = [dirs::home_dir(), repo_root];

        let mut servers = Vec::new();
        for root in candidate_roots.into_iter().flatten() {
            // Get user and project-scoped MCP servers from all providers for the given cwd.
            if let Some(provider_map) = self.file_based_servers_by_root.get(&root) {
                for hash_set in provider_map.values() {
                    servers.extend(
                        hash_set
                            .iter()
                            .filter_map(|h| self.file_based_servers.get(h)),
                    );
                }
            }
        }
        servers
    }

    /// Removes all tracked servers for the given `(root_path, provider)` pair,
    /// then removes any that are no longer referenced elsewhere.
    fn remove_servers_for_root_provider(
        &mut self,
        root_path: &PathBuf,
        provider: MCPProvider,
        config_path: &Path,
        ctx: &mut ModelContext<Self>,
    ) {
        let hashes = self
            .file_based_servers_by_root
            .get_mut(root_path)
            .and_then(|m| m.remove(&provider));

        let should_remove_config_root =
            if let Some(provider_map) = self.config_paths_by_root_provider.get_mut(root_path) {
                if provider_map
                    .get(&provider)
                    .is_some_and(|stored_config_path| stored_config_path == config_path)
                {
                    provider_map.remove(&provider);
                }
                provider_map.is_empty()
            } else {
                false
            };
        if should_remove_config_root {
            self.config_paths_by_root_provider.remove(root_path);
        }

        if let Some(hashes) = hashes {
            self.remove_if_orphaned(hashes, ctx);
        }
    }

    /// Removes servers if they are no longer referenced by any (root_path, provider) pair.
    /// Orphaned servers are removed from `file_based_servers` and the templatable manager is
    /// notified to despawn them and purge their credentials.
    fn remove_if_orphaned(
        &mut self,
        hashes: impl IntoIterator<Item = u64>,
        ctx: &mut ModelContext<Self>,
    ) {
        let referenced_hashes: HashSet<u64> = self
            .file_based_servers_by_root
            .values()
            .flat_map(|provider_map| provider_map.values())
            .flat_map(|hash_set| hash_set.iter().copied())
            .collect();

        let removed_servers: Vec<_> = hashes
            .into_iter()
            .filter(|hash| !referenced_hashes.contains(hash))
            .filter_map(|hash| self.file_based_servers.remove(&hash))
            .collect();

        // Notify the templatable manager to remove orphaned servers and purge their credentials.
        if !removed_servers.is_empty() {
            for server in &removed_servers {
                self.activated_file_based_server_uuids
                    .remove(&server.uuid());
                Self::persist_file_based_activation(server.uuid(), false, ctx);
            }

            let removed_uuids = removed_servers
                .iter()
                .map(|server| server.uuid())
                .collect_vec();
            ctx.emit(FileBasedMCPManagerEvent::DespawnServers {
                installation_uuids: removed_uuids,
            });

            let removed_hashes = removed_servers
                .iter()
                .filter_map(|server| server.hash())
                .collect_vec();
            ctx.emit(FileBasedMCPManagerEvent::PurgeCredentials {
                installation_hashes: removed_hashes,
            });
        }
    }

    /// Applies a parsed list of MCP servers
    /// spawning new servers and removing servers that are no longer present.
    #[cfg(test)]
    fn apply_parsed_servers(
        &mut self,
        root_path: PathBuf,
        provider: MCPProvider,
        parsed_servers: Vec<ParsedTemplatableMCPServerResult>,
        ctx: &mut ModelContext<Self>,
    ) {
        let config_path = Self::default_config_path_for_root_provider(&root_path, provider);
        self.apply_parsed_servers_from_config(
            root_path,
            provider,
            config_path,
            parsed_servers,
            ctx,
        );
    }

    fn apply_parsed_servers_from_config(
        &mut self,
        root_path: PathBuf,
        provider: MCPProvider,
        config_path: PathBuf,
        parsed_servers: Vec<ParsedTemplatableMCPServerResult>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.config_paths_by_root_provider
            .entry(root_path.clone())
            .or_default()
            .insert(provider, config_path);

        let previous_scanned_servers: HashSet<u64> = self
            .file_based_servers_by_root
            .get(&root_path)
            .and_then(|m| m.get(&provider))
            .cloned()
            .unwrap_or_default();

        let mut servers_to_spawn = Vec::new();
        let mut scanned_servers = HashSet::new();
        for server in parsed_servers {
            let Some(installation) = server.templatable_mcp_server_installation else {
                continue;
            };
            let Some(hash) = installation.hash() else {
                continue;
            };
            // TODO(APP-3429): Deduplicate file-based servers across provider directories.
            if let Entry::Vacant(e) = self.file_based_servers.entry(hash) {
                // Detected a server that hasn't previously been spawned.
                // Initialize metadata and mark it for spawning.
                e.insert(installation.clone());
                servers_to_spawn.push(installation);
            }

            // In all cases, add a reference to the server in the (root_path, provider) entry.
            self.file_based_servers_by_root
                .entry(root_path.clone())
                .or_default()
                .entry(provider)
                .or_default()
                .insert(hash);
            scanned_servers.insert(hash);
        }

        let auto_started_uuids = self.maybe_autostart_file_based_servers(servers_to_spawn, ctx);
        self.pending_scan_auto_started_servers_by_root
            .entry(root_path.clone())
            .or_default()
            .insert(provider, auto_started_uuids.into_iter().collect());

        // Determine which servers have been removed.
        let servers_to_remove = previous_scanned_servers
            .difference(&scanned_servers)
            .copied()
            .collect_vec();

        // Remove any servers that are no longer present in the config file.
        if let Some(provider_map) = self.file_based_servers_by_root.get_mut(&root_path) {
            if let Some(hash_set) = provider_map.get_mut(&provider) {
                for hash in &servers_to_remove {
                    hash_set.remove(hash);
                }
            }

            // If the set of servers for the provider is empty, remove the provider from the map.
            if provider_map.get(&provider).is_some_and(|s| s.is_empty()) {
                provider_map.remove(&provider);
            }
        }

        // If the set of servers for the root path is empty, remove the root path from the map.
        if self
            .file_based_servers_by_root
            .get(&root_path)
            .is_some_and(|m| m.is_empty())
        {
            self.file_based_servers_by_root.remove(&root_path);
        }

        let provider_removed = self
            .file_based_servers_by_root
            .get(&root_path)
            .is_none_or(|provider_map| !provider_map.contains_key(&provider));
        if provider_removed {
            let should_remove_config_root = if let Some(provider_map) =
                self.config_paths_by_root_provider.get_mut(&root_path)
            {
                provider_map.remove(&provider);
                provider_map.is_empty()
            } else {
                false
            };
            if should_remove_config_root {
                self.config_paths_by_root_provider.remove(&root_path);
            }
        }

        // If orphaned servers are found, remove them and purge their credentials.
        self.remove_if_orphaned(servers_to_remove, ctx);
    }

    /// Returns `true` if the server identified by `hash` is referenced from any global
    /// config location.
    ///
    /// "Global" means the installation was detected outside of a user repository:
    /// - For `MCPProvider::Warp`: the logical root for `~/.warp*/.mcp.json`.
    /// - For any other provider: the user's home directory (e.g. `~/.claude.json`).
    ///
    /// Project-scoped installations (those detected inside a repo) are not considered
    /// global, even if they also happen to be referenced from a global location (in which
    /// case this returns `true` due to the global reference).
    fn is_global_server(&self, hash: u64) -> bool {
        let home_dir = dirs::home_dir();
        self.file_based_servers_by_root
            .iter()
            .any(|(root_path, provider_map)| {
                provider_map.iter().any(|(provider, hashes)| {
                    if !hashes.contains(&hash) {
                        return false;
                    }
                    match provider {
                        MCPProvider::Warp => Self::is_global_warp_root(root_path),
                        MCPProvider::Claude | MCPProvider::Codex | MCPProvider::Agents => {
                            home_dir.as_ref().is_some_and(|home| root_path == home)
                        }
                    }
                })
            })
    }

    /// Returns `true` if the server identified by `hash` is referenced from the global
    /// Warp config (`~/.warp/.mcp.json`). Global Warp servers always auto-spawn.
    fn is_global_warp_server(&self, hash: u64) -> bool {
        self.file_based_servers_by_root
            .iter()
            .any(|(root_path, provider_map)| {
                Self::is_global_warp_root(root_path)
                    && provider_map
                        .get(&MCPProvider::Warp)
                        .is_some_and(|hashes| hashes.contains(&hash))
            })
    }

    fn is_global_warp_root(root_path: &Path) -> bool {
        warp_managed_mcp_config_path().is_some_and(|path| root_path == path.root_path.as_path())
    }

    fn default_config_path_for_root_provider(root_path: &Path, provider: MCPProvider) -> PathBuf {
        if provider == MCPProvider::Warp && Self::is_global_warp_root(root_path) {
            return warp_managed_mcp_config_path()
                .map(|path| path.config_path)
                .unwrap_or_else(|| root_path.join(provider.home_config_path()));
        }

        if dirs::home_dir()
            .as_ref()
            .is_some_and(|home_dir| root_path == home_dir)
        {
            return root_path.join(provider.home_config_path());
        }

        root_path.join(provider.project_config_path())
    }

    fn external_root_paths_for_installation(&self, installation_uuid: Uuid) -> Vec<PathBuf> {
        let Some(hash) = self.get_hash_by_uuid(installation_uuid) else {
            return vec![];
        };

        self.file_based_servers_by_root
            .iter()
            .filter(|(_, provider_map)| {
                provider_map.iter().any(|(provider, hashes)| {
                    *provider != MCPProvider::Warp && hashes.contains(&hash)
                })
            })
            .map(|(root_path, _)| root_path.clone())
            .sorted()
            .collect()
    }

    pub fn has_external_source(&self, installation_uuid: Uuid) -> bool {
        !self
            .external_root_paths_for_installation(installation_uuid)
            .is_empty()
    }

    fn add_warp_config_reference_for_installation(
        &mut self,
        root_path: PathBuf,
        config_path: PathBuf,
        installation: &TemplatableMCPServerInstallation,
    ) {
        let Some(hash) = installation.hash() else {
            return;
        };

        self.file_based_servers
            .entry(hash)
            .or_insert_with(|| installation.clone());
        self.file_based_servers_by_root
            .entry(root_path.clone())
            .or_default()
            .entry(MCPProvider::Warp)
            .or_default()
            .insert(hash);
        self.config_paths_by_root_provider
            .entry(root_path)
            .or_default()
            .insert(MCPProvider::Warp, config_path);
    }

    fn copy_external_installation_to_warp_configs(
        &mut self,
        installation: &TemplatableMCPServerInstallation,
    ) {
        for root_path in self.external_root_paths_for_installation(installation.uuid()) {
            let config_path =
                Self::default_config_path_for_root_provider(&root_path, MCPProvider::Warp);
            match Self::merge_installation_into_warp_config(installation, &config_path) {
                Ok(()) => self.add_warp_config_reference_for_installation(
                    root_path,
                    config_path,
                    installation,
                ),
                Err(err) => {
                    log::error!(
                        "Failed to copy externally detected MCP server '{}' into Warp config {}: {err:#}",
                        installation.templatable_mcp_server().name,
                        config_path.display()
                    );
                }
            }
        }
    }

    fn merge_installation_into_warp_config(
        installation: &TemplatableMCPServerInstallation,
        config_path: &Path,
    ) -> anyhow::Result<()> {
        let server_map = Self::resolved_server_map(installation)?;
        let mut config = Self::read_warp_mcp_config(config_path)?;
        let servers = Self::servers_object_for_warp_config(&mut config)?;
        servers.extend(server_map);

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut contents = serde_json::to_string_pretty(&config)?;
        contents.push('\n');
        fs::write(config_path, contents)?;
        Ok(())
    }

    fn resolved_server_map(
        installation: &TemplatableMCPServerInstallation,
    ) -> anyhow::Result<Map<String, Value>> {
        let resolved_json = resolve_json(installation);
        let value: Value = serde_json::from_str(&resolved_json)?;
        let object = value.as_object().ok_or_else(|| {
            anyhow::anyhow!(
                "resolved MCP server config for '{}' is not a JSON object",
                installation.templatable_mcp_server().name
            )
        })?;

        if object.len() != 1 {
            anyhow::bail!(
                "resolved MCP server config for '{}' must contain exactly one server, found {}",
                installation.templatable_mcp_server().name,
                object.len()
            );
        }

        Ok(object.clone())
    }

    fn read_warp_mcp_config(config_path: &Path) -> anyhow::Result<Value> {
        match fs::read_to_string(config_path) {
            Ok(contents) if contents.trim().is_empty() => Ok(Value::Object(Map::new())),
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Value::Object(Map::new())),
            Err(err) => Err(err.into()),
        }
    }

    fn servers_object_for_warp_config(
        config: &mut Value,
    ) -> anyhow::Result<&mut Map<String, Value>> {
        #[derive(Clone, Copy)]
        enum WrapperKey {
            NestedMcpServers,
            Servers,
            McpServers,
            McpServersSnake,
        }

        let wrapper_key = if config
            .pointer("/mcp/servers")
            .and_then(Value::as_object)
            .is_some()
        {
            Some(WrapperKey::NestedMcpServers)
        } else if config.get("servers").and_then(Value::as_object).is_some() {
            Some(WrapperKey::Servers)
        } else if config
            .get("mcpServers")
            .and_then(Value::as_object)
            .is_some()
        {
            Some(WrapperKey::McpServers)
        } else if config
            .get("mcp_servers")
            .and_then(Value::as_object)
            .is_some()
        {
            Some(WrapperKey::McpServersSnake)
        } else {
            None
        };

        match wrapper_key {
            Some(WrapperKey::NestedMcpServers) => config
                .pointer_mut("/mcp/servers")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("mcp.servers is not a JSON object")),
            Some(WrapperKey::Servers) => config
                .get_mut("servers")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("servers is not a JSON object")),
            Some(WrapperKey::McpServers) => config
                .get_mut("mcpServers")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("mcpServers is not a JSON object")),
            Some(WrapperKey::McpServersSnake) => config
                .get_mut("mcp_servers")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("mcp_servers is not a JSON object")),
            None => {
                let object = config
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("Warp MCP config is not a JSON object"))?;
                object
                    .entry("mcpServers")
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("mcpServers is not a JSON object"))
            }
        }
    }

    fn auto_start_decision(&self, hash: u64, file_based_mcp_enabled: bool) -> AutoStartDecision {
        let server_type = if self.is_global_warp_server(hash) {
            FileBasedMCPServerType::GlobalWarp
        } else if self.is_global_server(hash) {
            FileBasedMCPServerType::GlobalThirdParty
        } else {
            FileBasedMCPServerType::ProjectScoped
        };
        let should_autostart = match server_type {
            FileBasedMCPServerType::GlobalWarp => true,
            FileBasedMCPServerType::GlobalThirdParty => file_based_mcp_enabled,
            FileBasedMCPServerType::ProjectScoped => false,
        };

        AutoStartDecision {
            should_autostart,
            server_type,
        }
    }

    /// Returns the UUIDs of servers that were actually auto-started.
    fn maybe_autostart_file_based_servers(
        &mut self,
        servers_to_consider: Vec<TemplatableMCPServerInstallation>,
        ctx: &mut ModelContext<Self>,
    ) -> Vec<Uuid> {
        if servers_to_consider.is_empty() {
            return Vec::new();
        }
        let mcp_enabled = AISettings::as_ref(ctx).is_file_based_mcp_enabled(ctx);

        // Partition servers into three buckets based on scope:
        // - Global Warp: always auto-spawn.
        // - Global non-Warp: auto-spawn iff the toggle is on.
        // - Project-scoped (any provider): never auto-spawn; require explicit opt-in
        //   via the "Detected from {provider}" section of the MCP settings.
        let mut to_spawn = Vec::new();
        let mut auto_started_uuids = Vec::new();
        for installation in servers_to_consider {
            let Some(hash) = installation.hash() else {
                continue;
            };
            let installation_uuid = installation.uuid();
            let server_name = installation.templatable_mcp_server().name.clone();
            let AutoStartDecision {
                should_autostart, ..
            } = self.auto_start_decision(hash, mcp_enabled);
            let is_activated = self
                .activated_file_based_server_uuids
                .contains(&installation_uuid);
            if should_autostart || is_activated {
                log::info!("Spawning file-based MCP server '{server_name}' ({installation_uuid})");
                auto_started_uuids.push(installation_uuid);
                to_spawn.push(installation);
            }
        }

        if !to_spawn.is_empty() {
            ctx.emit(FileBasedMCPManagerEvent::SpawnServers {
                installations: to_spawn,
            });
        }
        auto_started_uuids
    }

    fn handle_cloud_environment_scan_complete(
        &mut self,
        repo_path: &PathBuf,
        ctx: &mut ModelContext<Self>,
    ) {
        let mcp_enabled = AISettings::as_ref(ctx).is_file_based_mcp_enabled(ctx);
        // FileMCPWatcher emits CloudEnvMcpScanComplete only after emitting ConfigParsed
        // for every provider config in this repo scan. Each ConfigParsed call records
        // the UUIDs actually emitted through SpawnServers in
        // pending_scan_auto_started_servers_by_root, so this remove() returns the wait set
        // for this completed scan.
        let wait_server_uuids: Vec<Uuid> = self
            .pending_scan_auto_started_servers_by_root
            .remove(repo_path)
            .into_iter()
            .flat_map(|provider_map| provider_map.into_values())
            .flatten()
            .sorted_by_key(|uuid| uuid.to_string())
            .collect();

        let mut detected_servers: Vec<CloudEnvMcpScanServer> = Vec::new();
        if let Some(provider_map) = self.file_based_servers_by_root.get(repo_path) {
            for (provider, hash_set) in provider_map {
                for hash in hash_set {
                    let Some(installation) = self.file_based_servers.get(hash) else {
                        continue;
                    };
                    let uuid = installation.uuid();
                    let auto_start_eligible = self
                        .auto_start_decision(*hash, mcp_enabled)
                        .should_autostart;
                    detected_servers.push(CloudEnvMcpScanServer {
                        uuid,
                        name: installation.templatable_mcp_server().name.clone(),
                        provider: *provider,
                        hash: *hash,
                        auto_start_eligible,
                    });
                }
            }
        }
        log::info!(
            "Cloud environment file-based MCP scan complete for {}: {} detected server(s), {} auto-started server(s)",
            repo_path.display(),
            detected_servers.len(),
            wait_server_uuids.len()
        );

        // Pass the UUIDs of auto-start-requested file-based MCP servers to the AgentDriver.
        ctx.emit(FileBasedMCPManagerEvent::CloudEnvMcpScanComplete {
            repo_path: repo_path.clone(),
            detected_servers,
            wait_server_uuids,
        });
    }

    fn handle_file_based_mcp_enabled_change(&mut self, ctx: &mut ModelContext<Self>) {
        // Only global third-party servers are affected by the toggle:
        // - Global Warp servers always spawn regardless of the toggle.
        // - Project-scoped servers (any provider) are never auto-spawned and their
        //   running state is managed per-card via the MCP settings UI; toggling the
        //   setting must not spawn or despawn them.
        let global_third_party_servers: Vec<_> = self
            .file_based_servers
            .iter()
            .filter(|(hash, _)| {
                self.auto_start_decision(**hash, true).server_type
                    == FileBasedMCPServerType::GlobalThirdParty
            })
            .map(|(_, server)| server.clone())
            .collect();
        if !AISettings::as_ref(ctx).is_file_based_mcp_enabled(ctx) {
            // Toggle off: despawn global third-party servers only.
            ctx.emit(FileBasedMCPManagerEvent::DespawnServers {
                installation_uuids: global_third_party_servers
                    .iter()
                    .map(|s| s.uuid())
                    .collect_vec(),
            });
        } else {
            // Toggle on: spawn global third-party servers (global Warp servers are
            // already running; project-scoped servers are unaffected).
            ctx.emit(FileBasedMCPManagerEvent::SpawnServers {
                installations: global_third_party_servers,
            });
        }
    }

    pub fn get_hash_by_uuid(&self, installation_uuid: Uuid) -> Option<u64> {
        self.file_based_servers
            .iter()
            .find(|(_, server)| server.uuid() == installation_uuid)
            .map(|(hash, _)| *hash)
    }

    pub fn set_server_activation(
        &mut self,
        installation_uuid: Uuid,
        active: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        self.set_server_activation_with_mode(
            installation_uuid,
            active,
            FileBasedMCPActivationMode::ReferenceOnly,
            ctx,
        );
    }

    pub fn set_server_activation_with_mode(
        &mut self,
        installation_uuid: Uuid,
        active: bool,
        mode: FileBasedMCPActivationMode,
        ctx: &mut ModelContext<Self>,
    ) {
        if active {
            let Some(installation) = self.get_installation_by_uuid(installation_uuid).cloned()
            else {
                log::warn!(
                    "Cannot activate file-based server {installation_uuid}: installation not found"
                );
                return;
            };

            if mode == FileBasedMCPActivationMode::CopyToWarpConfig {
                self.copy_external_installation_to_warp_configs(&installation);
            }
            self.activated_file_based_server_uuids
                .insert(installation_uuid);
            Self::persist_file_based_activation(installation_uuid, true, ctx);
            ctx.emit(FileBasedMCPManagerEvent::SpawnServers {
                installations: vec![installation],
            });
        } else {
            self.activated_file_based_server_uuids
                .remove(&installation_uuid);
            Self::persist_file_based_activation(installation_uuid, false, ctx);
            ctx.emit(FileBasedMCPManagerEvent::DespawnServers {
                installation_uuids: vec![installation_uuid],
            });
        }
    }

    fn persist_file_based_activation(
        installation_uuid: Uuid,
        active: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let global_resource_handles = crate::GlobalResourceHandlesProvider::as_ref(ctx).get();

        let Some(sender) = &global_resource_handles.model_event_sender else {
            return;
        };

        let event = if active {
            crate::persistence::ModelEvent::UpsertFileBasedMCPServerActivation { installation_uuid }
        } else {
            crate::persistence::ModelEvent::DeleteFileBasedMCPServerActivations {
                installation_uuids: vec![installation_uuid],
            }
        };

        if let Err(err) = sender.send(event) {
            log::error!("Failed to save file-based MCP server activation: {err}");
        }
    }

    /// Returns all detected file-based MCP server installations.
    pub fn file_based_servers(&self) -> Vec<&TemplatableMCPServerInstallation> {
        self.file_based_servers.values().collect()
    }

    /// Returns the installation with the given UUID, if any.
    pub fn get_installation_by_uuid(
        &self,
        uuid: Uuid,
    ) -> Option<&TemplatableMCPServerInstallation> {
        self.file_based_servers
            .values()
            .find(|server| server.uuid() == uuid)
    }

    /// Returns all root paths for the given installation scoped to a specific provider.
    pub fn directory_paths_for_installation_and_provider(
        &self,
        uuid: Uuid,
        provider: MCPProvider,
    ) -> Vec<PathBuf> {
        let Some(hash) = self.get_hash_by_uuid(uuid) else {
            return vec![];
        };
        self.file_based_servers_by_root
            .iter()
            .filter(|(_, provider_map)| {
                provider_map
                    .get(&provider)
                    .is_some_and(|hashes| hashes.contains(&hash))
            })
            .map(|(root, _)| root.clone())
            .sorted()
            .collect()
    }

    /// Returns the concrete config file paths where a file-based MCP installation
    /// was detected for a specific provider.
    pub fn config_file_paths_for_installation_and_provider(
        &self,
        uuid: Uuid,
        provider: MCPProvider,
    ) -> Vec<PathBuf> {
        let Some(hash) = self.get_hash_by_uuid(uuid) else {
            return vec![];
        };
        let mut config_paths = self
            .file_based_servers_by_root
            .iter()
            .filter(|(_, provider_map)| {
                provider_map
                    .get(&provider)
                    .is_some_and(|hashes| hashes.contains(&hash))
            })
            .map(|(root, _)| {
                self.config_paths_by_root_provider
                    .get(root)
                    .and_then(|provider_map| provider_map.get(&provider))
                    .cloned()
                    .unwrap_or_else(|| Self::default_config_path_for_root_provider(root, provider))
            })
            .collect_vec();
        config_paths.sort();
        config_paths.dedup();
        config_paths
    }

    /// Returns the directory a file-based MCP installation should be spawned from
    /// when its config does not specify `working_directory`.
    ///
    /// The spawn root is the directory the config was discovered in, with one
    /// exception: global Warp installs are discovered in `~/.warp*/`, which
    /// isn't a useful cwd for spawned processes, so they are remapped to the
    /// home directory instead.
    /// - Project-scoped installations: the repo root.
    /// - Global installations (`~/.warp/.mcp.json`, `~/.claude.json`, etc.): the
    ///   home directory.
    ///
    /// If the installation is referenced from multiple roots, the lexicographically
    /// smallest is returned for determinism. Returns `None` for installations that
    /// are not tracked by `FileBasedMCPManager` (e.g. cloud-templated installs).
    pub fn spawn_root_for_installation(&self, uuid: Uuid) -> Option<PathBuf> {
        let hash = self.get_hash_by_uuid(uuid)?;
        let discovery_root = self
            .file_based_servers_by_root
            .iter()
            .filter(|(_, provider_map)| provider_map.values().any(|hashes| hashes.contains(&hash)))
            .map(|(root, _)| root.clone())
            .sorted()
            .next()?;

        // Global Warp installs live under `~/.warp*/`, which is internal Warp
        // state rather than a meaningful working directory. Map them to the
        // home dir so all global installs (Warp and third-party) share a
        // consistent cwd.
        if self.is_global_warp_server(hash) {
            return dirs::home_dir().or(Some(discovery_root));
        }
        Some(discovery_root)
    }
}

struct AutoStartDecision {
    should_autostart: bool,
    server_type: FileBasedMCPServerType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileBasedMCPServerType {
    /// A file-based MCP server detected from Warp's global managed config.
    GlobalWarp,
    /// A file-based MCP server detected from a non-Warp provider's global user config.
    GlobalThirdParty,
    /// A file-based MCP server detected from a project/repository-scoped config.
    ProjectScoped,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CloudEnvMcpScanServer {
    pub uuid: Uuid,
    pub name: String,
    pub provider: MCPProvider,
    pub hash: u64,
    pub auto_start_eligible: bool,
}
pub enum FileBasedMCPManagerEvent {
    SpawnServers {
        installations: Vec<TemplatableMCPServerInstallation>,
    },
    DespawnServers {
        installation_uuids: Vec<Uuid>,
    },
    PurgeCredentials {
        installation_hashes: Vec<u64>,
    },
    CloudEnvMcpScanComplete {
        repo_path: PathBuf,
        #[allow(dead_code)]
        detected_servers: Vec<CloudEnvMcpScanServer>,
        wait_server_uuids: Vec<Uuid>,
    },
}

impl Entity for FileBasedMCPManager {
    type Event = FileBasedMCPManagerEvent;
}

impl SingletonEntity for FileBasedMCPManager {}

#[cfg(test)]
#[path = "file_based_manager_tests.rs"]
mod tests;
