use crate::channel::ChannelState;

const SLIPSTREAM_REPO_URL: &str = "https://github.com/pixelkaiser/slipstream";
const SLIPSTREAM_ISSUES_URL: &str = "https://github.com/pixelkaiser/slipstream/issues";
const WARP_DOCS_URL: &str = "https://docs.warp.dev/";
const WARP_ISSUES_URL: &str = "https://github.com/warpdotdev/Warp/issues";
const WARP_SLACK_URL: &str = "http://go.warp.dev/join-preview";
const WARP_PRIVACY_POLICY_URL: &str = "https://www.warp.dev/privacy";

pub const GITHUB_RELEASES_URL: &str = "https://github.com/pixelkaiser/slipstream/releases";

pub fn user_docs_url() -> &'static str {
    if ChannelState::is_slipstream() {
        SLIPSTREAM_REPO_URL
    } else {
        WARP_DOCS_URL
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn github_issues_url() -> &'static str {
    if ChannelState::is_slipstream() {
        SLIPSTREAM_ISSUES_URL
    } else {
        WARP_ISSUES_URL
    }
}

pub fn slack_url() -> &'static str {
    if ChannelState::is_slipstream() {
        SLIPSTREAM_REPO_URL
    } else {
        WARP_SLACK_URL
    }
}

pub fn privacy_policy_url() -> &'static str {
    if ChannelState::is_slipstream() {
        SLIPSTREAM_REPO_URL
    } else {
        WARP_PRIVACY_POLICY_URL
    }
}

pub fn feedback_form_url() -> String {
    let mut url = url::Url::parse(if ChannelState::is_slipstream() {
        "https://github.com/pixelkaiser/slipstream/issues/new/choose"
    } else {
        "https://github.com/warpdotdev/Warp/issues/new/choose"
    })
    .expect("Should not fail to parse");
    if let Some(version) = ChannelState::app_version() {
        let version_param = if ChannelState::is_slipstream() {
            "slipstream-version"
        } else {
            "warp-version"
        };
        url.query_pairs_mut().append_pair(version_param, version);
    }
    url.query_pairs_mut()
        .append_pair("os-version", &os_info::get().version().to_string());
    url.to_string()
}
