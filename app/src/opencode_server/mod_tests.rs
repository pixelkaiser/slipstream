use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::json;

use super::*;
use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::agent::{
    AIAgentActionResultType, AIAgentActionType, AIAgentInput, AIAgentOutputMessageType,
    ReadFilesResult, RequestCommandOutputResult,
};

#[test]
fn normalize_opencode_server_url_defaults_to_loopback_server() {
    let url = normalize_opencode_server_url("").unwrap();
    assert_eq!(url.as_str(), "http://127.0.0.1:4096/");
}

#[test]
fn normalize_opencode_server_url_adds_http_scheme_and_default_port() {
    let url = normalize_opencode_server_url("localhost").unwrap();
    assert_eq!(url.as_str(), "http://localhost:4096/");
}

#[test]
fn normalize_opencode_server_url_rejects_non_http_scheme() {
    let error = normalize_opencode_server_url("ws://localhost:4096").unwrap_err();
    assert!(error.to_string().contains("http or https"));
}

#[test]
fn basic_auth_header_construction_uses_http_basic_format() {
    assert_eq!(
        basic_auth_header("opencode", "secret"),
        "Basic b3BlbmNvZGU6c2VjcmV0"
    );
}

#[test]
fn loopback_server_does_not_require_credentials() {
    let config = OpenCodeServerConfig::from_values("http://127.0.0.1:4096", "", "").unwrap();
    assert!(config.basic_auth_header.is_none());
}

#[test]
fn non_loopback_server_requires_credentials() {
    let error = OpenCodeServerConfig::from_values("http://example.com:4096", "", "").unwrap_err();
    assert!(error.to_string().contains("credentials are required"));
}

#[test]
fn non_loopback_server_accepts_basic_auth_credentials() {
    let config =
        OpenCodeServerConfig::from_values("http://example.com:4096", "opencode", "secret").unwrap();
    assert_eq!(
        config.basic_auth_header.unwrap().to_str().unwrap(),
        "Basic b3BlbmNvZGU6c2VjcmV0"
    );
}

#[test]
fn directory_url_encodes_directory_query_parameter() {
    let config = OpenCodeServerConfig::from_values("http://127.0.0.1:4096", "", "").unwrap();
    let url = directory_url(
        &config,
        "/session",
        Some(Path::new("/Users/te/dev/warp nested")),
    );
    assert_eq!(
        url.as_str(),
        "http://127.0.0.1:4096/session?directory=%2FUsers%2Fte%2Fdev%2Fwarp+nested"
    );
}

#[test]
fn project_matching_uses_exact_worktree_for_active_root() {
    let projects = projects(&["/Users/te/dev/warp"]);
    let matched =
        matched_project_directories(&projects, &[PathBuf::from("/Users/te/dev/warp")], &[]);
    assert_eq!(matched, vec![PathBuf::from("/Users/te/dev/warp")]);
}

#[test]
fn project_matching_uses_longest_prefix_for_nested_active_root() {
    let projects = projects(&["/Users/te/dev", "/Users/te/dev/warp"]);
    let matched =
        matched_project_directories(&projects, &[PathBuf::from("/Users/te/dev/warp/app")], &[]);
    assert_eq!(matched, vec![PathBuf::from("/Users/te/dev/warp")]);
}

#[test]
fn project_matching_deduplicates_multiple_active_roots() {
    let projects = projects(&["/Users/te/dev/warp"]);
    let matched = matched_project_directories(
        &projects,
        &[
            PathBuf::from("/Users/te/dev/warp"),
            PathBuf::from("/Users/te/dev/warp/app"),
        ],
        &[],
    );
    assert_eq!(matched, vec![PathBuf::from("/Users/te/dev/warp")]);
}

#[test]
fn project_matching_skips_missing_active_project() {
    let projects = projects(&["/Users/te/dev/warp"]);
    let matched =
        matched_project_directories(&projects, &[PathBuf::from("/Users/te/dev/other")], &[]);
    assert!(matched.is_empty());
}

#[test]
fn project_matching_keeps_imported_project_fallback() {
    let projects = projects(&["/Users/te/dev/warp"]);
    let matched =
        matched_project_directories(&projects, &[], &[PathBuf::from("/Users/te/dev/imported")]);
    assert_eq!(matched, vec![PathBuf::from("/Users/te/dev/imported")]);
}

#[test]
fn converts_session_detail_to_external_agent_conversation() {
    let detail = session_detail(vec![OpenCodeConversationItem {
        role: "user".to_string(),
        text: "fix the bridge".to_string(),
    }]);
    let conversation_id = AIConversationId::new();

    let conversation = opencode_session_detail_to_ai_conversation(conversation_id, &detail);

    assert_eq!(conversation.id(), conversation_id);
    assert_eq!(
        conversation.external_agent_provider(),
        Some(ExternalAgentConversationProvider::OpenCode)
    );
    assert!(conversation.should_exclude_from_navigation());
}

#[test]
fn session_summary_filters_opencode_child_sessions() {
    let parent = serde_json::from_value::<OpenCodeSessionWire>(json!({
        "id": "ses_parent",
        "title": "Parent session",
        "projectID": "project-1",
        "directory": "/Users/te/dev/warp",
        "time": { "updated": 1777040510640i64 }
    }))
    .unwrap();
    let child = serde_json::from_value::<OpenCodeSessionWire>(json!({
        "id": "ses_child",
        "parentID": "ses_parent",
        "title": "Find files (@explore subagent)",
        "projectID": "project-1",
        "directory": "/Users/te/dev/warp",
        "time": { "updated": 1777040510641i64 }
    }))
    .unwrap();
    let statuses = HashMap::from([("ses_parent".to_string(), "idle".to_string())]);

    let parent_summary =
        session_summary_for_directory(parent, Path::new("/Users/te/dev/warp"), &statuses).unwrap();
    let child_summary =
        session_summary_for_directory(child, Path::new("/Users/te/dev/warp"), &statuses);

    assert_eq!(parent_summary.id, "ses_parent");
    assert_eq!(parent_summary.status.as_deref(), Some("idle"));
    assert!(child_summary.is_none());
}

#[test]
fn prompt_result_prefers_full_session_detail_for_live_response() {
    let prompt = "please say hello";
    let output_items = vec![OpenCodeConversationItem {
        role: "user".to_string(),
        text: prompt.to_string(),
    }];
    let detail = session_detail(vec![
        OpenCodeConversationItem {
            role: "user".to_string(),
            text: prompt.to_string(),
        },
        OpenCodeConversationItem {
            role: "assistant".to_string(),
            text: "Hello from the full session detail.".to_string(),
        },
    ]);

    assert_eq!(
        opencode_prompt_result_output_text(&output_items, Some(&detail), prompt),
        "Hello from the full session detail."
    );
}

#[test]
fn prompt_result_uses_latest_matching_detail_turn() {
    let prompt = "repeat this";
    let detail = session_detail(vec![
        OpenCodeConversationItem {
            role: "user".to_string(),
            text: prompt.to_string(),
        },
        OpenCodeConversationItem {
            role: "assistant".to_string(),
            text: "old response".to_string(),
        },
        OpenCodeConversationItem {
            role: "user".to_string(),
            text: prompt.to_string(),
        },
        OpenCodeConversationItem {
            role: "assistant".to_string(),
            text: "fresh response".to_string(),
        },
    ]);

    assert_eq!(
        opencode_prompt_result_output_text(&[], Some(&detail), prompt),
        "fresh response"
    );
}

#[test]
fn prompt_result_does_not_use_stale_detail_for_different_prompt() {
    let output_items = vec![OpenCodeConversationItem {
        role: "assistant".to_string(),
        text: "fresh immediate response".to_string(),
    }];
    let detail = session_detail(vec![
        OpenCodeConversationItem {
            role: "user".to_string(),
            text: "previous prompt".to_string(),
        },
        OpenCodeConversationItem {
            role: "assistant".to_string(),
            text: "stale response".to_string(),
        },
    ]);

    assert_eq!(
        opencode_prompt_result_output_text(&output_items, Some(&detail), "new prompt"),
        "fresh immediate response"
    );
}

#[test]
fn parses_message_parts_into_readable_items() {
    let messages = vec![
        message(
            "user",
            vec![
                json!({"type": "text", "text": "please inspect"}),
                json!({"type": "file", "path": "ignored"}),
            ],
            None,
            None,
        ),
        message(
            "assistant",
            vec![
                json!({"type": "text", "text": "Sure."}),
                json!({"type": "tool", "tool": "bash", "state": {"status": "completed", "title": "ls", "output": "app\ncrates"}}),
                json!({"type": "patch", "files": [{"path": "app/src/lib.rs"}]}),
                json!({"type": "subtask", "agent": "build", "description": "Run checks"}),
            ],
            None,
            None,
        ),
    ];

    let items = messages_to_conversation_items(&messages);
    assert_eq!(items[0].role, "user");
    assert_eq!(items[0].text, "please inspect");
    assert_eq!(items[1].role, "assistant");
    assert!(items[1].text.contains("Sure."));
    assert!(items[1].text.contains("commandExecution: ls"));
    assert!(items[1]
        .text
        .contains("commandExecution/output: \"app\\ncrates\""));
    assert!(items[1].text.contains("Patch applied: app/src/lib.rs"));
    assert!(items[1].text.contains("Subtask (build): Run checks"));
}

#[test]
fn parses_real_opencode_read_and_list_tools_into_boxed_actions() {
    let messages = vec![
        message(
            "user",
            vec![json!({"type": "text", "text": "inspect files"})],
            None,
            None,
        ),
        message(
            "assistant",
            vec![
                json!({"type": "text", "text": "I will inspect the fixture."}),
                json!({
                    "type": "tool",
                    "callID": "read-directory-1",
                    "tool": "read",
                    "state": {
                        "status": "completed",
                        "input": { "filePath": "/private/tmp/opencode-warp-e2e-fixture", "offset": 1, "limit": 2000 },
                        "output": "<path>/private/tmp/opencode-warp-e2e-fixture</path>\n<type>directory</type>\n<entries>\nnotes.txt\nREADME.md\n\n(2 entries)\n</entries>",
                        "metadata": {
                            "preview": "notes.txt\nREADME.md",
                            "truncated": false,
                            "loaded": []
                        },
                        "title": "private/tmp/opencode-warp-e2e-fixture"
                    }
                }),
                json!({
                    "type": "tool",
                    "callID": "read-1",
                    "tool": "read",
                    "state": {
                        "status": "completed",
                        "input": { "filePath": "/Users/te/dev/warp/README.md" },
                        "output": "<path>/Users/te/dev/warp/README.md</path>\n<type>file</type>\n<content>1: # Slipstream\n2: Local agent integrations live here.\n\n(End of file - total 2 lines)\n</content>",
                        "title": "README.md"
                    }
                }),
                json!({
                    "type": "tool",
                    "callID": "list-1",
                    "tool": "list",
                    "state": {
                        "status": "completed",
                        "input": { "path": "/Users/te/dev/warp" },
                        "output": "/Users/te/dev/warp/\n  README.md\n  app/\n  crates/\n",
                        "title": ""
                    }
                }),
                json!({"type": "text", "text": "Done."}),
            ],
            None,
            None,
        ),
    ];
    let items = messages_to_conversation_items(&messages);
    let assistant_items = items
        .into_iter()
        .filter(|item| item.role != "user")
        .collect::<Vec<_>>();

    assert_fixture_snapshot(
        opencode_agent_output_snapshot(&assistant_items),
        "src/opencode_server/fixtures/snapshots/read_and_list_tool_output.snap",
    );
}

#[test]
fn parses_error_empty_and_aborted_messages() {
    let empty = message("assistant", vec![], None, None);
    assert!(message_to_conversation_item(&empty).is_none());

    let error = message(
        "assistant",
        vec![
            json!({"type": "tool", "tool": "edit", "state": {"status": "error", "error": {"message": "failed"}}}),
        ],
        Some(json!({"message": "request failed"})),
        Some(json!({"reason": "aborted"})),
    );
    let item = message_to_conversation_item(&error).unwrap();
    assert!(item.text.contains("Tool edit error: failed"));
    assert!(item.text.contains("Error: request failed"));
    assert!(item.text.contains("Aborted."));
}

#[test]
fn parses_pending_permission_requests() {
    let request: OpenCodePermissionRequestWire = serde_json::from_value(json!({
        "id": "perm-1",
        "sessionID": "session-1",
        "permission": "bash",
        "patterns": ["cargo test -p warp"],
        "always": ["cargo test*"]
    }))
    .unwrap();

    let pending = request.into_pending();
    assert_eq!(pending.id, "perm-1");
    assert_eq!(pending.session_id, "session-1");
    assert_eq!(pending.permission, "bash");
    assert_eq!(pending.patterns, vec!["cargo test -p warp"]);
    assert_eq!(OpenCodePermissionReply::Once.wire_value(), "once");
}

#[test]
fn parses_pending_question_requests() {
    let request: OpenCodeQuestionRequestWire = serde_json::from_value(json!({
        "id": "question-1",
        "sessionID": "session-1",
        "questions": [{
            "header": "Approve plan?",
            "question": "Should OpenCode continue?",
            "custom": true,
            "options": [
                { "label": "Approve", "description": "Continue" },
                { "label": "Reject", "description": "Stop" }
            ]
        }]
    }))
    .unwrap();

    let pending = request.into_pending();
    assert_eq!(pending.title(), "Approve plan?");
    assert_eq!(pending.message(), "Should OpenCode continue?");
    assert_eq!(
        pending.single_question().unwrap().options[0].label,
        "Approve"
    );
    assert!(pending.single_question().unwrap().allows_custom_answer());
}

#[test]
fn parses_optionless_questions_as_custom_answer_requests() {
    let request: OpenCodeQuestionRequestWire = serde_json::from_value(json!({
        "id": "question-1",
        "sessionID": "session-1",
        "questions": [{
            "header": "Custom answer",
            "question": "What should OpenCode use?"
        }]
    }))
    .unwrap();

    let pending = request.into_pending();
    let question = pending.single_question().unwrap();
    assert!(question.options.is_empty());
    assert!(question.allows_custom_answer());
}

#[test]
fn parses_question_options_with_missing_optional_fields() {
    let request: OpenCodeQuestionRequestWire = serde_json::from_value(json!({
        "id": "question-1",
        "sessionID": "session-1",
        "questions": [{
            "question": "Pick one",
            "options": [{ "label": "Choice A" }]
        }]
    }))
    .unwrap();

    let pending = request.into_pending();
    let question = pending.single_question().unwrap();
    assert_eq!(question.header, "");
    assert_eq!(question.question, "Pick one");
    assert_eq!(question.options[0].description, "");
}

#[test]
fn parses_multi_step_question_requests() {
    let request: OpenCodeQuestionRequestWire = serde_json::from_value(json!({
        "id": "question-1",
        "sessionID": "session-1",
        "questions": [
            {
                "header": "Pick a path",
                "question": "Which implementation should OpenCode use?",
                "options": [{ "label": "Minimal" }]
            },
            {
                "header": "Add note",
                "question": "Any extra instructions?",
                "custom": true
            }
        ]
    }))
    .unwrap();

    let pending = request.into_pending();
    assert!(pending.single_question().is_none());
    assert_eq!(pending.questions.len(), 2);
    assert_eq!(pending.title(), "OpenCode needs input");
    assert_eq!(pending.questions[1].header, "Add note");
    assert!(pending.questions[1].allows_custom_answer());
}

#[test]
fn opencode_multi_step_custom_prompt_snapshot() {
    let request: OpenCodeQuestionRequestWire = serde_json::from_value(json!({
        "id": "question-plan-1",
        "sessionID": "session-1",
        "questions": [
            {
                "header": "Pick implementation",
                "question": "Which implementation should OpenCode use?",
                "options": [{ "label": "Minimal" }, { "label": "Complete" }]
            },
            {
                "header": "Add instruction",
                "question": "Any extra instructions?",
                "custom": true
            }
        ]
    }))
    .unwrap();
    let pending = request.into_pending();

    assert_fixture_snapshot(
        opencode_question_snapshot(&pending),
        "src/opencode_server/fixtures/snapshots/multi_step_custom_prompt.snap",
    );
}

#[test]
fn opencode_shell_tool_snapshot_keeps_output_in_command_action() {
    let messages = vec![
        message(
            "user",
            vec![json!({"type": "text", "text": "list files"})],
            None,
            None,
        ),
        message(
            "assistant",
            vec![
                json!({"type": "text", "text": "I will list the files."}),
                json!({
                    "type": "tool",
                    "tool": "bash",
                    "state": {
                        "status": "completed",
                        "input": {
                            "command": "ls",
                            "timeout": 120000,
                            "workdir": "/private/tmp/opencode-warp-command-detail-repro",
                            "description": "Lists files in current directory"
                        },
                        "output": "a.txt\nb.txt\n",
                        "metadata": {
                            "output": "a.txt\nb.txt\n",
                            "exit": 0,
                            "description": "Lists files in current directory",
                            "truncated": false
                        },
                        "title": "Lists files in current directory"
                    }
                }),
                json!({"type": "text", "text": "Files listed."}),
            ],
            None,
            None,
        ),
    ];
    let items = messages_to_conversation_items(&messages);
    let assistant_items = items
        .into_iter()
        .filter(|item| item.role != "user")
        .collect::<Vec<_>>();

    assert_fixture_snapshot(
        opencode_agent_output_snapshot(&assistant_items),
        "src/opencode_server/fixtures/snapshots/shell_tool_output.snap",
    );
}

fn projects(paths: &[&str]) -> Vec<OpenCodeProjectWire> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| OpenCodeProjectWire {
            id: format!("project-{index}"),
            worktree: PathBuf::from(path),
        })
        .collect()
}

fn message(
    role: &str,
    parts: Vec<serde_json::Value>,
    error: Option<serde_json::Value>,
    finish: Option<serde_json::Value>,
) -> OpenCodeMessageWire {
    OpenCodeMessageWire {
        info: OpenCodeMessageInfoWire {
            role: role.to_string(),
            error,
            finish,
        },
        parts,
    }
}

fn session_detail(items: Vec<OpenCodeConversationItem>) -> OpenCodeSessionDetail {
    OpenCodeSessionDetail {
        summary: OpenCodeSessionSummary {
            id: "ses_test".to_string(),
            title: "Test session".to_string(),
            directory: Some(PathBuf::from("/Users/te/dev/warp")),
            project_id: Some("project-test".to_string()),
            updated_at: Some(1777040510640),
            status: None,
        },
        items,
    }
}

fn assert_fixture_snapshot(actual: String, relative_path: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| {
            panic!(
                "Could not read snapshot {}: {error}\n\nactual snapshot:\n{actual}",
                path.display()
            )
        });
    assert_eq!(actual.trim_end(), expected.trim_end(), "{}", path.display());
}

fn opencode_question_snapshot(question: &OpenCodePendingQuestion) -> String {
    let mut snapshot = String::new();
    writeln!(snapshot, "title: {}", question.title()).unwrap();
    writeln!(snapshot, "message: {}", question.message()).unwrap();
    writeln!(snapshot, "questions:").unwrap();
    for (index, question_info) in question.questions.iter().enumerate() {
        writeln!(snapshot, "- index: {index}").unwrap();
        writeln!(snapshot, "  header: {}", question_info.header).unwrap();
        writeln!(snapshot, "  question: {}", question_info.question).unwrap();
        writeln!(snapshot, "  multiple: {}", question_info.multiple).unwrap();
        writeln!(
            snapshot,
            "  allows_custom_answer: {}",
            question_info.allows_custom_answer()
        )
        .unwrap();
        writeln!(snapshot, "  options:").unwrap();
        for option in &question_info.options {
            if option.description.is_empty() {
                writeln!(snapshot, "  - {} |", option.label).unwrap();
            } else {
                writeln!(snapshot, "  - {} | {}", option.label, option.description).unwrap();
            }
        }
    }
    writeln!(snapshot, "actions:").unwrap();
    let needs_submit_button = question.questions.len() > 1
        || question
            .questions
            .iter()
            .any(|question_info| question_info.multiple);
    if needs_submit_button {
        writeln!(snapshot, "- submit").unwrap();
    }
    writeln!(snapshot, "- reject").unwrap();
    snapshot
}

fn opencode_agent_output_snapshot(items: &[OpenCodeConversationItem]) -> String {
    let output_text = items
        .iter()
        .map(|item| item.text.as_str())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut snapshot = String::new();
    writeln!(snapshot, "agent_text:").unwrap();
    write_indented_block(&mut snapshot, &output_text);
    writeln!(snapshot, "render_model:").unwrap();
    snapshot.push_str(&agent_render_model_snapshot(output_text));
    snapshot
}

fn agent_render_model_snapshot(output_text: String) -> String {
    let mut conversation = AIConversation::new_with_id(AIConversationId::new(), false);
    conversation
        .append_codex_exchange(
            Some("fixture prompt".to_string()),
            Some(output_text),
            Some("/Users/te/dev/warp".to_string()),
            false,
            Local::now(),
        )
        .unwrap();
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
                AIAgentActionType::ReadFiles(request) => {
                    writeln!(snapshot, "- action: read_files").unwrap();
                    writeln!(
                        snapshot,
                        "  files: {}",
                        request
                            .locations
                            .iter()
                            .map(|location| location.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                    .unwrap();
                    match action_results.get(&action.id).map(|result| &result.result) {
                        Some(AIAgentActionResultType::ReadFiles(ReadFilesResult::Success {
                            files,
                        })) => {
                            for file in files {
                                writeln!(snapshot, "  result_file: {}", file.file_name).unwrap();
                                writeln!(snapshot, "  result_line_count: {}", file.line_count)
                                    .unwrap();
                            }
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
