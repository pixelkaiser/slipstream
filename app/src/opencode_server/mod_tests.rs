use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;

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
    assert!(items[1].text.contains("Tool bash: ls"));
    assert!(items[1].text.contains("Patch applied: app/src/lib.rs"));
    assert!(items[1].text.contains("Subtask (build): Run checks"));
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
