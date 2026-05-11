use std::collections::HashMap;

use super::{
    artifact_from_fork_proto, codex_output_status, AIConversation, AIConversationAutoexecuteMode,
    AIConversationId,
};
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentActionResultType, AIAgentActionType, AIAgentInput, AIAgentOutputMessageType,
    AIAgentOutputStatus, FinishedAIAgentOutput, RequestCommandOutputResult,
};
use crate::ai::artifacts::Artifact;
use crate::persistence::model::AgentConversationData;
use chrono::Local;
use warp_core::features::FeatureFlag;
use warp_multi_agent_api as api;

fn restored_conversation(conversation_data: Option<AgentConversationData>) -> AIConversation {
    AIConversation::new_restored(
        AIConversationId::new(),
        vec![api::Task {
            id: "root-task".to_string(),
            messages: vec![],
            dependencies: None,
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        }],
        conversation_data,
    )
    .unwrap()
}

fn user_query_message(id: &str, request_id: &str, query: &str) -> api::Message {
    api::Message {
        id: id.to_string(),
        task_id: "root-task".to_string(),
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::UserQuery(api::message::UserQuery {
            query: query.to_string(),
            context: None,
            referenced_attachments: HashMap::new(),
            mode: None,
            intended_agent: Default::default(),
        })),
        request_id: request_id.to_string(),
        timestamp: None,
    }
}

fn agent_output_message(id: &str, request_id: &str) -> api::Message {
    api::Message {
        id: id.to_string(),
        task_id: "root-task".to_string(),
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: "Done".to_string(),
            },
        )),
        request_id: request_id.to_string(),
        timestamp: None,
    }
}

fn restored_conversation_with_queries(queries: &[&str]) -> AIConversation {
    let messages = queries
        .iter()
        .enumerate()
        .flat_map(|(index, query)| {
            let request_id = format!("request-{index}");
            [
                user_query_message(&format!("user-{index}"), &request_id, query),
                agent_output_message(&format!("agent-{index}"), &request_id),
            ]
        })
        .collect();

    AIConversation::new_restored(
        AIConversationId::new(),
        vec![api::Task {
            id: "root-task".to_string(),
            messages,
            dependencies: None,
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        }],
        None,
    )
    .unwrap()
}

#[test]
fn latest_user_query_returns_latest_non_empty_user_query() {
    let conversation =
        restored_conversation_with_queries(&["write unit tests", "fix the failing test"]);

    assert_eq!(
        conversation.latest_user_query(),
        Some("fix the failing test".to_string())
    );
}

#[test]
fn latest_user_query_trims_and_skips_empty_queries() {
    let conversation = restored_conversation_with_queries(&["  write unit tests  ", "  "]);

    assert_eq!(
        conversation.latest_user_query(),
        Some("write unit tests".to_string())
    );
}

#[test]
fn restored_conversation_defaults_autoexecute_override_when_not_persisted() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(true);
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null}"#).unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RespectUserSettings
    );
}

#[test]
fn restored_conversation_uses_persisted_last_event_sequence() {
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null,"last_event_sequence":42}"#)
            .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(conversation.last_event_sequence(), Some(42));
}

#[test]
fn restored_conversation_uses_persisted_remote_child_marker() {
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null,"is_remote_child":true}"#)
            .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert!(conversation.is_remote_child());
}

#[test]
fn child_conversation_detection_uses_parent_agent_id() {
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"parent_agent_id":"parent-run-id"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert!(conversation.is_child_agent_conversation());
    assert_eq!(conversation.parent_conversation_id(), None);
}

#[test]
fn cli_agent_transcript_vehicle_is_excluded_from_navigation() {
    let conversation = AIConversation::new(false, true);

    assert!(conversation.should_exclude_from_navigation());
}

#[test]
fn restored_conversation_defaults_unknown_persisted_autoexecute_override() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(true);
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"autoexecute_override":"UnexpectedValue"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RespectUserSettings
    );
}

#[test]
fn restored_conversation_uses_persisted_autoexecute_override_when_enabled() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(true);
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"autoexecute_override":"RunToCompletion"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RunToCompletion
    );
}

#[test]
fn restored_conversation_ignores_persisted_autoexecute_override_when_disabled() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(false);
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"autoexecute_override":"RunToCompletion"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RespectUserSettings
    );
}

#[test]
fn fork_artifacts_adds_file_artifacts_to_conversation() {
    let proto_artifact = api::message::artifact_event::ConversationArtifact {
        artifact: Some(
            api::message::artifact_event::conversation_artifact::Artifact::File(
                api::message::artifact_event::FileArtifact {
                    artifact_uid: "artifact-file-1".to_string(),
                    filepath: "outputs/report.txt".to_string(),
                    mime_type: "text/plain".to_string(),
                    size_bytes: 42,
                    description: "Daily summary".to_string(),
                },
            ),
        ),
    };

    assert_eq!(
        artifact_from_fork_proto(&proto_artifact),
        Some(Artifact::File {
            artifact_uid: "artifact-file-1".to_string(),
            filepath: "outputs/report.txt".to_string(),
            filename: "report.txt".to_string(),
            mime_type: "text/plain".to_string(),
            description: Some("Daily summary".to_string()),
            size_bytes: Some(42),
        })
    );
}

#[test]
fn codex_output_converts_command_execution_to_command_action() {
    let task_id = TaskId::new("root-task".to_string());
    let (status, action_results) = codex_output_status(
        Some("Before\n\ncommandExecution: /bin/zsh -lc 'rg foo'\n\nAfter".to_string()),
        false,
        &task_id,
    );

    let output = match status {
        AIAgentOutputStatus::Finished {
            finished_output: FinishedAIAgentOutput::Success { output },
        } => output,
        _ => panic!("expected finished successful output"),
    };
    let messages = &output.get().messages;

    assert_eq!(messages.len(), 3);
    assert!(matches!(
        messages[0].message,
        AIAgentOutputMessageType::Text(_)
    ));
    let AIAgentOutputMessageType::Action(action) = &messages[1].message else {
        panic!("expected command action");
    };
    let AIAgentActionType::RequestCommandOutput { command, .. } = &action.action else {
        panic!("expected request command output action");
    };
    assert_eq!(command, "/bin/zsh -lc 'rg foo'");
    assert!(!action.requires_result);
    assert!(matches!(
        messages[2].message,
        AIAgentOutputMessageType::Text(_)
    ));

    assert_eq!(action_results.len(), 1);
    assert_eq!(action_results[0].id, action.id);
    let AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
        command,
        output,
        exit_code,
        ..
    }) = &action_results[0].result
    else {
        panic!("expected completed command result");
    };
    assert_eq!(command, "/bin/zsh -lc 'rg foo'");
    assert_eq!(output, "");
    assert!(exit_code.was_successful());
}

#[test]
fn codex_output_attaches_command_output_to_command_action() {
    let task_id = TaskId::new("root-task".to_string());
    let (status, action_results) = codex_output_status(
        Some(
            "Before\n\ncommandExecution: /bin/zsh -lc 'git status --short'\n\ncommandExecution/output: \" M app/src/codex_app_server/mod.rs\\n\"\n\nAfter".to_string(),
        ),
        false,
        &task_id,
    );

    let output = match status {
        AIAgentOutputStatus::Finished {
            finished_output: FinishedAIAgentOutput::Success { output },
        } => output,
        _ => panic!("expected finished successful output"),
    };
    let messages = &output.get().messages;

    assert_eq!(messages.len(), 3);
    assert!(matches!(
        messages[1].message,
        AIAgentOutputMessageType::Action(_)
    ));
    assert_eq!(action_results.len(), 1);
    let AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
        command,
        output,
        ..
    }) = &action_results[0].result
    else {
        panic!("expected completed command result");
    };
    assert_eq!(command, "/bin/zsh -lc 'git status --short'");
    assert_eq!(output, " M app/src/codex_app_server/mod.rs\n");
}

#[test]
fn codex_output_attaches_delayed_command_output_to_latest_action() {
    let task_id = TaskId::new("root-task".to_string());
    let (status, action_results) = codex_output_status(
        Some(
            "commandExecution: /bin/zsh -lc 'git status --short'\n\nAssistant text before the output marker.\n\ncommandExecution/output: \" M app/src/codex_app_server/mod.rs\\n\"".to_string(),
        ),
        false,
        &task_id,
    );

    let output = match status {
        AIAgentOutputStatus::Finished {
            finished_output: FinishedAIAgentOutput::Success { output },
        } => output,
        _ => panic!("expected finished successful output"),
    };
    let rendered_text = output
        .get()
        .messages
        .iter()
        .map(|message| message.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered_text.contains("commandExecution/output"),
        "command output marker leaked into rendered text: {rendered_text}"
    );
    assert_eq!(action_results.len(), 1);
    let AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
        output,
        ..
    }) = &action_results[0].result
    else {
        panic!("expected completed command result");
    };
    assert_eq!(output, " M app/src/codex_app_server/mod.rs\n");
}

#[test]
fn codex_output_attaches_targeted_delayed_command_output_to_matching_action() {
    let task_id = TaskId::new("root-task".to_string());
    let du_command = "/bin/zsh -lc 'du -sh homeassistant-config .esphome 2>/dev/null || true'";
    let git_command = "/bin/zsh -lc 'git status --short'";
    let output_marker = serde_json::json!({
        "command": du_command,
        "output": " 47M\thomeassistant-config\n976M\t.esphome\n",
    });
    let (status, action_results) = codex_output_status(
        Some(format!(
            "commandExecution: {du_command}\n\ncommandExecution: {git_command}\n\ncommandExecution/output: {output_marker}"
        )),
        false,
        &task_id,
    );

    let output = match status {
        AIAgentOutputStatus::Finished {
            finished_output: FinishedAIAgentOutput::Success { output },
        } => output,
        _ => panic!("expected finished successful output"),
    };
    let rendered_text = output
        .get()
        .messages
        .iter()
        .map(|message| message.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered_text.contains("commandExecution/output"),
        "command output marker leaked into rendered text: {rendered_text}"
    );
    assert_eq!(action_results.len(), 2);
    let outputs_by_command = action_results
        .iter()
        .map(|result| {
            let AIAgentActionResultType::RequestCommandOutput(
                RequestCommandOutputResult::Completed {
                    command, output, ..
                },
            ) = &result.result
            else {
                panic!("expected completed command result");
            };
            (command.as_str(), output.as_str())
        })
        .collect::<HashMap<_, _>>();

    assert_eq!(
        outputs_by_command.get(du_command),
        Some(&" 47M\thomeassistant-config\n976M\t.esphome\n")
    );
    assert_eq!(outputs_by_command.get(git_command), Some(&""));
}

#[test]
fn append_finished_codex_exchange_stores_command_result_for_restore() {
    let mut conversation = restored_conversation(None);

    conversation
        .append_codex_exchange(
            Some("inspect the repo".to_string()),
            Some("commandExecution: git status --short".to_string()),
            None,
            false,
            Local::now(),
        )
        .unwrap();

    let exchange = conversation.root_task_exchanges().next().unwrap();
    let action = exchange
        .output_status
        .output()
        .unwrap()
        .get()
        .actions()
        .next()
        .unwrap()
        .clone();
    let result = exchange
        .input
        .iter()
        .find_map(|input| match input {
            AIAgentInput::ActionResult { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();

    assert_eq!(result.id, action.id);
}

#[test]
fn streaming_codex_exchange_stores_command_result_while_waiting_for_approval() {
    let mut conversation = restored_conversation(None);
    let exchange_id = conversation
        .append_codex_exchange(
            Some("inspect the repo".to_string()),
            Some("commandExecution: git status --short".to_string()),
            None,
            true,
            Local::now(),
        )
        .unwrap();

    conversation
        .update_codex_exchange_output(
            exchange_id,
            "commandExecution: git status --short\n\ncommandExecution/output: \" M app/src/codex_app_server/mod.rs\\n\"".to_string(),
            false,
            false,
        )
        .unwrap();

    let exchange = conversation.root_task_exchanges().next().unwrap();
    let action = exchange
        .output_status
        .output()
        .unwrap()
        .get()
        .actions()
        .next()
        .unwrap()
        .clone();
    let result = exchange
        .input
        .iter()
        .find_map(|input| match input {
            AIAgentInput::ActionResult { result, .. } => Some(result),
            _ => None,
        })
        .unwrap_or_else(|| panic!("streaming Codex command action result is missing"));

    assert_eq!(result.id, action.id);
    let AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
        output,
        ..
    }) = &result.result
    else {
        panic!("expected completed command result");
    };
    assert_eq!(output, " M app/src/codex_app_server/mod.rs\n");
}

#[test]
fn codex_conversation_initial_working_directory_falls_back_to_exchange_cwd() {
    let mut conversation = AIConversation::new_with_id(AIConversationId::new(), false);

    conversation
        .append_codex_exchange(
            Some("what changed?".to_string()),
            Some("Done".to_string()),
            Some("/Users/te/dev/warp".to_string()),
            false,
            Local::now(),
        )
        .unwrap();

    assert_eq!(
        conversation.initial_working_directory().as_deref(),
        Some("/Users/te/dev/warp")
    );
}
