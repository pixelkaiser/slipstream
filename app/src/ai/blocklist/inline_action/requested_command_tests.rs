//! Unit tests for requested command helpers.

use super::{command_detail_text_from_result, format_command_text};
use crate::ai::agent::{AIAgentActionResultType, RequestCommandOutputResult};
use crate::terminal::model::block::BlockId;
use warp_core::command::ExitCode;

#[test]
fn single_line_without_newline_is_unchanged_ascii() {
    let input = "echo hello world";
    let output = format_command_text(input);
    assert_eq!(output, input);
}

#[test]
fn single_line_without_newline_preserves_multibyte_characters() {
    let input = "echo 🚀✨";
    let output = format_command_text(input);
    assert_eq!(output, input);

    // Additional sanity check: string is valid UTF-8 and can be iterated by chars without panic
    let collected: String = output.chars().collect();
    assert_eq!(collected, output);
}

#[test]
fn truncates_at_first_newline_and_appends_ellipsis_when_more_content_exists() {
    let input = "cargo build\n--release";
    let output = format_command_text(input);
    assert_eq!(output, "cargo build…");
}

#[test]
fn truncates_at_first_newline_without_ellipsis_when_rest_is_whitespace() {
    let input = "git status\n   \t  ";
    let output = format_command_text(input);
    assert_eq!(output, "git status");
}

#[test]
fn does_not_split_multibyte_char_across_utf8_boundaries_when_newline_follows() {
    // The emoji is a multi-byte sequence; ensure truncation at the newline does not split it.
    let input = "echo 🧪\nthen do something";
    let output = format_command_text(input);
    assert_eq!(output, "echo 🧪…");

    // Validate resulting string is valid UTF-8 by iterating graphemes via chars
    let reconstructed: String = output.chars().collect();
    assert_eq!(reconstructed, output);
}

#[test]
fn preserves_combining_characters_when_newline_is_after_cluster() {
    // "e" + combining acute accent
    // Sanity checks that the formatter doesn't split this unicode sequence
    let composed = format!("{}{}", 'e', '\u{0301}');
    let input = format!("echo {composed}\nnext");
    let output = format_command_text(&input);
    assert_eq!(output, format!("echo {composed}…"));

    // Still valid UTF-8 and same when re-collected from chars
    let reconstructed: String = output.chars().collect();
    assert_eq!(reconstructed, output);
}

#[test]
fn newline_then_multibyte_results_in_ellipsis_only() {
    let input = "\n🚀";
    let output = format_command_text(input);
    assert_eq!(output, "…");

    // Sanity: output remains valid UTF-8
    let reconstructed: String = output.chars().collect();
    assert_eq!(reconstructed, output);
}

#[test]
fn transcript_command_detail_includes_completed_output() {
    let result = AIAgentActionResultType::RequestCommandOutput(
        RequestCommandOutputResult::Completed {
            block_id: BlockId::new(),
            command: "ls".to_string(),
            output: "a.txt\nb.txt\n".to_string(),
            exit_code: ExitCode::from(0),
            start_ts: None,
            completed_ts: None,
        },
    );

    assert_eq!(
        command_detail_text_from_result("Lists files", &result),
        Some("ls\n\na.txt\nb.txt\n".to_string())
    );
}

#[test]
fn transcript_command_detail_falls_back_to_action_command_text() {
    let result = AIAgentActionResultType::RequestCommandOutput(
        RequestCommandOutputResult::Completed {
            block_id: BlockId::new(),
            command: String::new(),
            output: "a.txt\n".to_string(),
            exit_code: ExitCode::from(0),
            start_ts: None,
            completed_ts: None,
        },
    );

    assert_eq!(
        command_detail_text_from_result("ls", &result),
        Some("ls\n\na.txt\n".to_string())
    );
}

#[test]
fn transcript_command_detail_ignores_cancelled_before_execution() {
    let result = AIAgentActionResultType::RequestCommandOutput(
        RequestCommandOutputResult::CancelledBeforeExecution,
    );

    assert_eq!(command_detail_text_from_result("ls", &result), None);
}
