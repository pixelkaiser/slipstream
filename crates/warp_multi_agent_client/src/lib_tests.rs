use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE;
use prost::Message as _;
use warp_server_client::base_client::AmbientHeaderPolicy;

use super::{
    Error, ambient_policy, ambient_policy_for_request, decode_response_event, endpoint_url,
    is_passive_suggestion_request, should_send_authenticated_request,
};

#[test]
fn detects_passive_suggestion_requests() {
    let regular = warp_multi_agent_api::Request::default();
    let passive = warp_multi_agent_api::Request {
        input: Some(warp_multi_agent_api::request::Input {
            r#type: Some(
                warp_multi_agent_api::request::input::Type::GeneratePassiveSuggestions(
                    Default::default(),
                ),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(!is_passive_suggestion_request(&regular));
    assert!(is_passive_suggestion_request(&passive));
}

#[test]
fn routes_regular_and_passive_requests_to_distinct_endpoints() {
    let prefix = if cfg!(feature = "agent_mode_evals") {
        "agent-mode-evals"
    } else {
        "ai"
    };

    assert!(endpoint_url(false, None).ends_with(&format!("/{prefix}/multi-agent")));
    assert!(endpoint_url(true, None).ends_with(&format!("/{prefix}/passive-suggestions")));
}

#[test]
fn endpoint_url_uses_override_root() {
    assert_eq!(
        endpoint_url(false, Some("http://127.0.0.1:8787")),
        "http://127.0.0.1:8787/ai/multi-agent"
    );
}

#[test]
fn endpoint_url_trims_override_trailing_slash() {
    assert_eq!(
        endpoint_url(false, Some("http://127.0.0.1:8787/")),
        "http://127.0.0.1:8787/ai/multi-agent"
    );
}

#[test]
fn selects_endpoint_specific_ambient_header_policies() {
    assert_eq!(ambient_policy(false), AmbientHeaderPolicy::workload_only());
    assert_eq!(ambient_policy(true), AmbientHeaderPolicy::omit_all());
}

#[test]
fn omits_ambient_headers_for_unauthenticated_local_requests() {
    assert_eq!(
        ambient_policy_for_request(false, false),
        AmbientHeaderPolicy::omit_all()
    );
}

#[test]
fn local_override_requests_do_not_require_user_auth() {
    assert!(!should_send_authenticated_request(Some(
        "http://127.0.0.1:8787"
    )));
}

#[test]
fn decodes_quoted_base64_protobuf_response_event() {
    let expected = warp_multi_agent_api::ResponseEvent::default();
    let encoded = BASE64_URL_SAFE.encode(expected.encode_to_vec());

    let decoded = decode_response_event(&format!("\"{encoded}\"")).unwrap();

    assert_eq!(decoded, expected);
}

#[test]
fn distinguishes_base64_and_protobuf_decode_errors() {
    assert!(matches!(
        decode_response_event("%"),
        Err(Error::Base64Decode(_))
    ));

    let invalid_protobuf = BASE64_URL_SAFE.encode([0xff]);
    assert!(matches!(
        decode_response_event(&invalid_protobuf),
        Err(Error::ProtobufDecode(_))
    ));
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn native_output_stream_is_send() {
    fn assert_send<T: Send>() {}

    assert_send::<super::OutputStream>();
}
