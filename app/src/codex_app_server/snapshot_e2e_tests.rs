use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use chrono::Local;
use serde_json::Value;

use super::*;
use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::agent::{
    AIAgentActionResultType, AIAgentActionType, AIAgentInput, AIAgentOutputMessageType,
    RequestCommandOutputResult,
};

fn assert_fixture_snapshot(actual: String, relative_path: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("Could not read snapshot {}: {error}", path.display()));
    assert_eq!(actual.trim_end(), expected.trim_end(), "{}", path.display());
}

fn approval_from_wire_message(message: Value) -> CodexPendingApproval {
    let message = message.get("message").cloned().unwrap_or(message);
    let incoming: JsonRpcIncoming = serde_json::from_value(message).unwrap();
    parse_approval_request(
        incoming.method.as_deref(),
        incoming.id,
        incoming.params.as_ref(),
    )
    .unwrap()
}

fn approval_snapshot(approval: &CodexPendingApproval) -> String {
    let mut snapshot = String::new();
    writeln!(snapshot, "title: {}", approval.title()).unwrap();
    writeln!(snapshot, "message: {}", approval.message()).unwrap();
    if let Some(command) = &approval.command {
        writeln!(snapshot, "command: {command}").unwrap();
    }
    writeln!(snapshot, "controls:").unwrap();
    for control in approval.controls() {
        match control {
            CodexPendingApprovalControl::ApprovalDecision(decision) => {
                writeln!(snapshot, "- approval: {}", decision.label()).unwrap();
            }
            CodexPendingApprovalControl::UserInputOption {
                question_id,
                label,
                description,
            } => {
                writeln!(
                    snapshot,
                    "- option: {question_id} | {label} | {description}"
                )
                .unwrap();
            }
        }
    }
    snapshot
}

fn codex_agent_output_snapshot(items: &[CodexConversationItem]) -> String {
    let output_text = codex_items_to_agent_text(items);
    let mut snapshot = String::new();
    writeln!(snapshot, "agent_text:").unwrap();
    write_indented_block(&mut snapshot, &output_text);
    writeln!(snapshot, "render_model:").unwrap();
    snapshot.push_str(&agent_render_model_snapshot(output_text));
    snapshot
}

fn agent_render_model_snapshot(output_text: String) -> String {
    agent_render_model_snapshot_with_streaming(output_text, false)
}

fn agent_streaming_render_model_snapshot(output_text: String) -> String {
    agent_render_model_snapshot_with_streaming(output_text, true)
}

fn agent_render_model_snapshot_with_streaming(output_text: String, is_streaming: bool) -> String {
    let mut conversation = AIConversation::new_with_id(AIConversationId::new(), false);
    let exchange_id = conversation
        .append_codex_exchange(
            Some("fixture prompt".to_string()),
            (!is_streaming).then_some(output_text.clone()),
            Some("/Users/te/dev/warp".to_string()),
            is_streaming,
            Local::now(),
        )
        .unwrap();
    if is_streaming {
        conversation
            .update_codex_exchange_output(exchange_id, output_text, false, false)
            .unwrap();
    }
    let exchange = conversation.root_task_exchanges().next().unwrap();
    let action_results = exchange
        .input
        .iter()
        .filter_map(|input| match input {
            AIAgentInput::ActionResult { result, .. } => Some((result.id.clone(), result)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let output = exchange.output_status.output().unwrap();
    let output = output.get();

    let mut snapshot = String::new();
    for message in &output.messages {
        match &message.message {
            AIAgentOutputMessageType::Text(_) => {
                writeln!(snapshot, "- text:").unwrap();
                write_indented_block(&mut snapshot, &message.to_string());
            }
            AIAgentOutputMessageType::Action(action) => match &action.action {
                AIAgentActionType::RequestCommandOutput { command, .. } => {
                    writeln!(snapshot, "- action: request_command_output").unwrap();
                    writeln!(snapshot, "  command: {command}").unwrap();
                    match action_results.get(&action.id).map(|result| &result.result) {
                        Some(AIAgentActionResultType::RequestCommandOutput(
                            RequestCommandOutputResult::Completed { output, .. },
                        )) => {
                            writeln!(snapshot, "  result_output: {output:?}").unwrap();
                        }
                        Some(result) => {
                            writeln!(snapshot, "  result: {result:?}").unwrap();
                        }
                        None => {
                            writeln!(snapshot, "  result: <missing>").unwrap();
                        }
                    }
                }
                other => {
                    writeln!(snapshot, "- action: {other:?}").unwrap();
                }
            },
            other => {
                writeln!(snapshot, "- message: {other:?}").unwrap();
            }
        }
    }
    snapshot
}

fn live_stream_fixture_items_and_approval(
    fixture: &str,
) -> (Vec<CodexConversationItem>, CodexPendingApproval) {
    let mut items = Vec::new();
    for (line_index, line) in fixture.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line).unwrap_or_else(|error| {
            panic!("invalid live stream record {}: {error}", line_index + 1)
        });
        if record.get("direction").and_then(Value::as_str) != Some("in") {
            continue;
        }
        let Some(message) = record.get("message") else {
            continue;
        };
        let incoming: JsonRpcIncoming = serde_json::from_value(message.clone())
            .unwrap_or_else(|error| panic!("invalid JSON-RPC message {}: {error}", line_index + 1));
        if let Some(approval) = parse_approval_request(
            incoming.method.as_deref(),
            incoming.id.clone(),
            incoming.params.as_ref(),
        ) {
            return (items, approval);
        }
        if let Some(params) = incoming.params {
            if let Some(item) = parse_notification_item(incoming.method.as_deref(), &params) {
                items.push(item);
            }
        }
    }

    panic!("live stream fixture did not contain a Codex approval request");
}

fn write_indented_block(snapshot: &mut String, text: &str) {
    for line in text.lines() {
        if line.is_empty() {
            writeln!(snapshot).unwrap();
        } else {
            writeln!(snapshot, "  {line}").unwrap();
        }
    }
    if text.ends_with('\n') {
        writeln!(snapshot).unwrap();
    }
}

#[test]
fn codex_plan_confirmation_prompt_snapshot() {
    let fixture = include_str!("fixtures/approval_requests/request_user_input_plan_prompt.json");
    let message: Value = serde_json::from_str(fixture).unwrap();
    let approval = approval_from_wire_message(message);

    assert_fixture_snapshot(
        approval_snapshot(&approval),
        "src/codex_app_server/fixtures/snapshots/plan_confirmation_prompt.snap",
    );
}

#[test]
fn codex_real_plan_user_input_snapshot() {
    let fixture =
        include_str!("fixtures/approval_requests/plan_user_input_real_codex_0_130_0.json");
    let message: Value = serde_json::from_str(fixture).unwrap();
    let approval = approval_from_wire_message(message);

    assert_fixture_snapshot(
        approval_snapshot(&approval),
        "src/codex_app_server/fixtures/snapshots/real_plan_user_input.snap",
    );
}

#[test]
fn codex_streamed_command_snapshot_keeps_output_in_command_action() {
    let items = vec![
        CodexConversationItem {
            role: "item/agentMessage/delta".to_string(),
            text: "I will inspect the repo.".to_string(),
        },
        CodexConversationItem {
            role: "item/started".to_string(),
            text: "/bin/zsh -lc 'git status --short'".to_string(),
        },
        CodexConversationItem {
            role: "item/commandExecution/outputDelta".to_string(),
            text: serde_json::json!({
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
    ];

    assert_fixture_snapshot(
        codex_agent_output_snapshot(&items),
        "src/codex_app_server/fixtures/snapshots/streamed_command_output.snap",
    );
}

#[test]
fn codex_real_command_then_approval_stream_snapshot() {
    let fixture =
        include_str!("fixtures/live_streams/command_then_approval_real_codex_0_130_0.ndjson");
    assert!(
        fixture.contains("\"aggregatedOutput\":\"slipstream-codex-readonly\""),
        "fixture must preserve the real Codex completed command output shape"
    );
    let (items, approval) = live_stream_fixture_items_and_approval(fixture);
    let output_text = codex_items_to_agent_text(&items);
    let mut snapshot = String::new();

    writeln!(snapshot, "approval:").unwrap();
    write_indented_block(&mut snapshot, &approval_snapshot(&approval));
    writeln!(snapshot, "agent_text:").unwrap();
    write_indented_block(&mut snapshot, &output_text);
    writeln!(snapshot, "streaming_render_model:").unwrap();
    snapshot.push_str(&agent_streaming_render_model_snapshot(output_text));

    assert_fixture_snapshot(
        snapshot,
        "src/codex_app_server/fixtures/snapshots/real_command_then_approval_stream.snap",
    );
}
