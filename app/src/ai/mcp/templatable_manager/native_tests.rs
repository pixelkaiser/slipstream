use std::collections::{HashMap, HashSet};

use uuid::Uuid;
use warpui::App;

use crate::ai::mcp::{
    JsonTemplate, TemplatableMCPServer, TemplatableMCPServerInstallation,
    TemplatableMCPServerManager,
};
use crate::auth::AuthStateProvider;
use crate::cloud_object::model::persistence::CloudModel;
use crate::test_util::settings::initialize_settings_for_tests;
use crate::workspaces::user_workspaces::UserWorkspaces;

fn local_installation(name: &str) -> TemplatableMCPServerInstallation {
    let templatable_mcp_server = TemplatableMCPServer {
        uuid: Uuid::new_v4(),
        name: name.to_string(),
        description: None,
        template: JsonTemplate {
            json: format!(r#"{{"{name}":{{"command":"echo","args":["ok"]}}}}"#),
            variables: Vec::new(),
        },
        version: 1,
        gallery_data: None,
    };

    TemplatableMCPServerInstallation::new(Uuid::new_v4(), templatable_mcp_server, HashMap::new())
}

#[test]
fn logged_out_startup_preserves_local_mcp_installations_without_cloud_template() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.add_singleton_model(|_| AuthStateProvider::new_logged_out_for_test());
        app.add_singleton_model(CloudModel::mock);
        app.add_singleton_model(UserWorkspaces::default_mock);

        let installation = local_installation("local-server");
        let installation_uuid = installation.uuid();
        let manager = TemplatableMCPServerManager {
            cloud_templatable_mcp_servers: HashMap::new(),
            locally_installed_servers: HashMap::from([(installation_uuid, installation)]),
            server_states: HashMap::new(),
            active_servers: HashMap::new(),
            spawned_servers: HashMap::new(),
            server_credentials: HashMap::new(),
            file_based_server_credentials: HashMap::new(),
            database_connection: None,
            server_error_messages: HashMap::new(),
            spawner: None,
            pending_reconnections: HashMap::new(),
            pending_oauth_csrf: HashMap::new(),
            cli_spawned_server_uuids: HashSet::new(),
        };
        let manager = app.add_singleton_model(|_| manager);

        manager.update(&mut app, |manager, ctx| {
            manager.delete_orphaned_installations(ctx);
        });

        manager.read(&app, |manager, _| {
            assert!(
                manager.get_installed_server(&installation_uuid).is_some(),
                "logged-out local MCP installation should survive startup without a cloud template"
            );
        });
    })
}
