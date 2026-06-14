use super::*;

#[test]
fn command_suggestion_requires_current_prefix() {
    let response = LocalCommandAutocompleteResponse {
        most_likely_action: "git checkout feature/local".to_string(),
        ..Default::default()
    };

    assert_eq!(
        response.command_suggestion("git checkout "),
        Some("git checkout feature/local")
    );
    assert_eq!(response.command_suggestion("cargo "), None);
    assert_eq!(
        LocalCommandAutocompleteResponse {
            most_likely_action: "git checkout ".to_string(),
            ..Default::default()
        }
        .command_suggestion("git checkout "),
        None
    );
}

#[test]
fn command_suggestion_rejects_multiline_output() {
    let response = LocalCommandAutocompleteResponse {
        most_likely_action: "git status\ngit diff".to_string(),
        ..Default::default()
    };

    assert_eq!(response.command_suggestion("git "), None);
}

#[test]
fn context_sourced_responses_skip_completion_spec_validation() {
    for source in ["deterministic", "fallback"] {
        let response = LocalCommandAutocompleteResponse {
            source: source.to_string(),
            ..Default::default()
        };
        assert!(response.should_skip_completion_spec_validation());
    }

    for source in ["model", "none", ""] {
        let response = LocalCommandAutocompleteResponse {
            source: source.to_string(),
            ..Default::default()
        };
        assert!(!response.should_skip_completion_spec_validation());
    }
}
