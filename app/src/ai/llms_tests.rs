use super::*;

fn restore_env_var(name: &str, previous: Option<std::ffi::OsString>) {
    match previous {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

#[test]
fn llm_info_deserializes_without_base_model_name() {
    let raw = r#"{
            "display_name": "gpt-4o",
            "id": "gpt-4o",
            "usage_metadata": {
                "request_multiplier": 1,
                "credit_multiplier": null
            },
            "description": null,
            "disable_reason": null,
            "vision_supported": false,
            "spec": null,
            "provider": "Unknown"
        }"#;

    let info: LLMInfo = serde_json::from_str(raw).expect("should deserialize");
    assert_eq!(info.display_name, "gpt-4o");
    assert_eq!(info.base_model_name, "gpt-4o");
}

#[test]
fn llm_info_deserializes_host_configs_as_vec() {
    // Wire format from server: host_configs is a Vec
    let raw = r#"{
            "display_name": "gpt-4o",
            "id": "gpt-4o",
            "usage_metadata": { "request_multiplier": 1, "credit_multiplier": null },
            "provider": "OpenAI",
            "host_configs": [
                { "enabled": true, "model_routing_host": "DirectApi" },
                { "enabled": false, "model_routing_host": "AwsBedrock" }
            ]
        }"#;

    let info: LLMInfo = serde_json::from_str(raw).expect("should deserialize vec format");
    assert_eq!(info.display_name, "gpt-4o");
    assert_eq!(info.host_configs.len(), 2);
    assert!(
        info.host_configs
            .get(&LLMModelHost::DirectApi)
            .unwrap()
            .enabled
    );
    assert!(
        !info
            .host_configs
            .get(&LLMModelHost::AwsBedrock)
            .unwrap()
            .enabled
    );
}

#[test]
fn llm_info_round_trip_serializes_and_deserializes() {
    // Start with wire format (Vec)
    let wire_json = r#"{
            "display_name": "claude-3",
            "base_model_name": "claude-3",
            "id": "claude-3",
            "usage_metadata": { "request_multiplier": 2, "credit_multiplier": 1.5 },
            "description": "A powerful model",
            "vision_supported": true,
            "provider": "Anthropic",
            "host_configs": [
                { "enabled": true, "model_routing_host": "DirectApi" }
            ]
        }"#;

    // Deserialize from wire format
    let info: LLMInfo = serde_json::from_str(wire_json).expect("should deserialize");

    // Serialize (produces HashMap format)
    let serialized = serde_json::to_string(&info).expect("should serialize");

    // Deserialize again (from HashMap format)
    let round_tripped: LLMInfo =
        serde_json::from_str(&serialized).expect("should deserialize after round trip");

    assert_eq!(info, round_tripped);
}

#[test]
#[serial_test::serial]
fn llm_preferences_refreshes_on_init_in_no_cloud_mode() {
    let previous = std::env::var_os("WARP_NO_CLOUD");

    std::env::remove_var("WARP_NO_CLOUD");
    assert!(LLMPreferences::should_refresh_models_on_init());

    std::env::set_var("WARP_NO_CLOUD", "0");
    assert!(!LLMPreferences::should_refresh_models_on_init());

    std::env::set_var("WARP_NO_CLOUD", "1");
    assert!(LLMPreferences::should_refresh_models_on_init());

    restore_env_var("WARP_NO_CLOUD", previous);
}

#[test]
fn local_multi_agent_models_populate_agent_model_choices() {
    let mut config = LocalMultiAgentConfig::default();
    config.openai_model = Some("model-b".to_string());
    config.local_model_list = String::new();
    config.local_model_aliases = "{}".to_string();

    let models = models_by_feature_for_local_multi_agent(
        &config,
        &["model-a".to_string(), "model-b".to_string()],
    )
    .expect("local models should build LLM choices");

    assert_eq!(models.agent_mode.default_id, LLMId::from("model-b"));
    assert_eq!(
        models
            .agent_mode
            .choices
            .iter()
            .map(|model| model.id.to_string())
            .collect::<Vec<_>>(),
        vec!["model-a".to_string(), "model-b".to_string()]
    );
    assert_eq!(models.coding.choices, models.agent_mode.choices);
    assert_eq!(
        models.cli_agent.as_ref().map(|models| &models.choices),
        Some(&models.agent_mode.choices)
    );
}
