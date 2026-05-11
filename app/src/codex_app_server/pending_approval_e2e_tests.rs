use super::*;

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

#[test]
fn request_user_input_message_only_wire_request_stays_actionable() {
    let approval = approval_from_wire_message(json!({
        "id": 7,
        "method": "request_user_input",
        "params": {
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "message": "Codex is requesting approval."
        }
    }));

    assert!(approval.user_input_questions.is_empty());
    assert_eq!(approval.message(), "Codex is requesting approval.");
    assert_eq!(
        approval.controls(),
        vec![
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Accept),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Decline),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Cancel),
        ]
    );
    assert_eq!(
        approval.approval_result(CodexApprovalDecision::Accept),
        json!({ "decision": "accept" })
    );
}

#[test]
fn request_user_input_prompt_only_wire_request_stays_actionable() {
    let approval = approval_from_wire_message(json!({
        "id": 9,
        "method": "request_user_input",
        "params": {
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "prompt": "Codex is requesting approval."
        }
    }));

    assert_eq!(
        approval.controls(),
        vec![
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Accept),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Decline),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Cancel),
        ]
    );
}

#[test]
fn replayed_raw_codex_approval_fixtures_are_actionable() {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/codex_app_server/fixtures/approval_requests");
    let entries = std::fs::read_dir(&fixture_dir).unwrap_or_else(|error| {
        panic!(
            "Could not read Codex approval fixture directory {}: {error}",
            fixture_dir.display()
        )
    });
    let mut fixture_count = 0;

    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        fixture_count += 1;
        let fixture = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("Could not read fixture {}: {error}", path.display()));
        let message: Value = serde_json::from_str(&fixture)
            .unwrap_or_else(|error| panic!("Invalid JSON fixture {}: {error}", path.display()));
        let approval = approval_from_wire_message(message);
        assert!(
            !approval.controls().is_empty(),
            "fixture {} produced a dead approval card",
            path.display()
        );
        assert!(
            !approval.message().trim().is_empty(),
            "fixture {} produced an empty approval message",
            path.display()
        );
    }

    assert!(
        fixture_count > 0,
        "Codex approval replay tests need at least one raw app-server fixture in {}",
        fixture_dir.display()
    );
}

#[test]
fn real_codex_command_approval_fixture_preserves_object_decision() {
    let fixture =
        include_str!("fixtures/approval_requests/command_execution_real_codex_0_130_0.json");
    let message: Value = serde_json::from_str(fixture).unwrap();
    let approval = approval_from_wire_message(message);

    assert_eq!(
        approval.controls(),
        vec![
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Accept),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::AcceptForPrefix),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Cancel),
        ]
    );
    assert_eq!(
        approval.approval_result(CodexApprovalDecision::AcceptForPrefix),
        json!({
            "decision": {
                "acceptWithExecpolicyAmendment": {
                    "execpolicy_amendment": [
                        "/bin/zsh",
                        "-lc",
                        "printf slipstream-codex-approval-capture\\n"
                    ]
                }
            }
        })
    );
}

#[test]
fn request_user_input_question_wire_request_stays_a_prompt() {
    let approval = approval_from_wire_message(json!({
        "id": 8,
        "method": "request_user_input",
        "params": {
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "questions": [{
                "header": "Prompt Test",
                "question": "What kind of prompt interaction would you like to test?",
                "options": [
                    { "label": "Single choice" },
                    { "label": "Multiple choice" }
                ]
            }]
        }
    }));

    assert_eq!(
        approval.controls(),
        vec![
            CodexPendingApprovalControl::UserInputOption {
                question_id: "question-1".to_string(),
                label: "Single choice".to_string(),
                description: String::new(),
            },
            CodexPendingApprovalControl::UserInputOption {
                question_id: "question-1".to_string(),
                label: "Multiple choice".to_string(),
                description: String::new(),
            },
        ]
    );
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
fn request_user_input_with_multiple_option_questions_never_renders_dead_card() {
    let approval = approval_from_wire_message(json!({
        "id": 10,
        "method": "request_user_input",
        "params": {
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "reason": "Codex is requesting approval.",
            "questions": [
                {
                    "question": "Approve?",
                    "options": [{ "label": "Approve" }]
                },
                {
                    "question": "Scope?",
                    "options": [{ "label": "This command" }]
                }
            ]
        }
    }));

    assert_eq!(approval.title(), "Codex needs input");
    assert_eq!(
        approval.controls(),
        vec![
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Accept),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Decline),
            CodexPendingApprovalControl::ApprovalDecision(CodexApprovalDecision::Cancel),
        ]
    );
}
