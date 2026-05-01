use super::{derive_http_origin_from_ws_url, ChannelState};

#[test]
fn wss_becomes_https_and_strips_path() {
    let got = derive_http_origin_from_ws_url("wss://rtc.app.warp.dev/graphql/v2");
    assert_eq!(got.as_deref(), Some("https://rtc.app.warp.dev"));
}

#[test]
fn ws_becomes_http_and_preserves_port() {
    let got = derive_http_origin_from_ws_url("ws://localhost:8080/graphql/v2");
    assert_eq!(got.as_deref(), Some("http://localhost:8080"));
}

#[test]
fn unparseable_input_returns_none() {
    assert!(derive_http_origin_from_ws_url("not a url").is_none());
    assert!(derive_http_origin_from_ws_url("https://app.warp.dev").is_none());
}

#[test]
fn oss_default_identity_is_slipstream_distribution() {
    assert_eq!(ChannelState::app_id().to_string(), "com.slipstream.app");
    assert_eq!(ChannelState::url_scheme(), "slipstream");
    assert_eq!(ChannelState::channel().cli_command_name(), "slipstream");
    assert_eq!(ChannelState::app_display_name(), "Slipstream");
    assert_eq!(ChannelState::product_name(), "Slipstream");
}
